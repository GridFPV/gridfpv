//! Pushing events to a remote server (docs/primitives.md §13.4).
//!
//! One task, for as long as the Director runs. While a server is set and switched on
//! (`gridfpv_server::link`), it keeps the server's copy of **every event** up with the
//! Director's: the log, entry for entry, and the header that names what the log refers to.
//!
//! - **The Director connects outward.** Nothing here listens.
//! - **It is always safe to stop and start.** How far the server has got is asked of the
//!   server, never remembered across runs. A batch sent twice is kept once.
//! - **A log goes as it is stored.** Entries are sent as their stored text and never
//!   written out again on the way (`gridfpv_storage::RawEntry`).
//! - **A different event under the same id is never overwritten.** The server refuses it
//!   and the event is marked [`LinkEventState::Different`] until the director says to
//!   replace the server's copy.
//! - **Race day does not wait on it.** Every failure is reported on the desk and tried
//!   again later. Nothing else in the Director reads what this task does.

use std::collections::HashMap;
use std::time::Duration;

use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::link::{LinkDesk, LinkEvent, LinkEventState, LinkState};
pub use gridfpv_server::link_client::{Client, Held, PushError, log_body};
use gridfpv_server::remote::PushRefusal;
use gridfpv_storage::{Chain, RawEntry};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

/// The most entries in one batch. The server takes ten times this.
const BATCH_ENTRIES: usize = 500;

/// About the most text in one batch, so one request stays small beside the server's limit.
const BATCH_BYTES: usize = 4 * 1024 * 1024;

/// How often every event is looked at, when nothing says to look sooner.
const TICK: Duration = Duration::from_secs(1);

/// How often the server is asked who holds which pilot.
const ACCOUNTS_EVERY: Duration = Duration::from_secs(15);

/// How often the server is asked what it holds of every event.
const HELD_EVERY: Duration = Duration::from_secs(15);

/// How long to wait before reaching for a server that did not answer.
const RETRY: Duration = Duration::from_secs(5);

/// How many of `entries` go in the next batch: at least one, and no more than fit.
fn batch_len(entries: &[RawEntry]) -> usize {
    let mut bytes = 0;
    for (i, entry) in entries.iter().enumerate().take(BATCH_ENTRIES) {
        bytes += entry.json.len();
        if bytes > BATCH_BYTES && i > 0 {
            return i;
        }
    }
    entries.len().min(BATCH_ENTRIES)
}

/// What is remembered about one event while connected to one server.
#[derive(Default)]
struct Known {
    /// What the server holds, once it has said.
    server: Option<(u64, Chain)>,
    /// A mark of the header last sent, so it is sent again only when it changes.
    header: Option<[u8; 32]>,
    /// The server holds a different log under this id.
    different: bool,
    /// The event was removed on the server. Nothing is sent until a replace is asked for.
    removed: bool,
}

/// Everything remembered while connected to one server. Dropped when the link changes.
#[derive(Default)]
pub struct Memory {
    events: HashMap<EventId, Known>,
    /// A mark of the pilot list last sent, so it is sent again only when it changes.
    pilots: Option<[u8; 32]>,
    /// When the server was last asked who holds which pilot.
    accounts_asked: Option<std::time::Instant>,
    /// When the server was last asked what it holds of every event.
    held_asked: Option<std::time::Instant>,
}

impl Memory {
    /// Ask the server what it holds of every event at the next look, whether or not it is
    /// time to.
    pub fn check_now(&mut self) {
        self.held_asked = None;
    }
}

/// An event's log from `start` on, as stored.
fn read_raw(
    registry: &EventRegistry,
    event: &EventId,
    start: u64,
) -> Result<Vec<RawEntry>, PushError> {
    let log = registry
        .log(event)
        .ok_or_else(|| PushError::Server("The event is gone.".into()))?;
    let log = log
        .lock()
        .map_err(|_| PushError::Server("The event's log cannot be read.".into()))?;
    log.read_raw_from(start)
        .map_err(|e| PushError::Server(format!("The event's log cannot be read: {e}")))
}

/// Bring the server's copy of one event up with the Director's. Returns how many entries
/// the event holds here and how many the server now holds.
async fn sync_event(
    registry: &EventRegistry,
    desk: &LinkDesk,
    client: &Client,
    known: &mut Known,
    event: &EventId,
) -> Result<(u64, u64, LinkEventState), PushError> {
    // The header first: it is what gives the event its name on the server. Building it
    // also brings the event up to the clock, so a start or a stop that fell due is in the
    // log before the log is read.
    let header = gridfpv_server::link::header(registry, event)
        .await
        .map_err(|e| PushError::Server(e.message))?;

    if desk.take_replace(event) {
        client.delete(event).await?;
        *known = Known::default();
    }

    let all = read_raw(registry, event, 0)?;
    let len = all.len() as u64;

    if known.removed {
        return Ok((len, 0, LinkEventState::Removed));
    }
    let (mut sent, mut chain) = match known.server {
        Some(server) => server,
        None => match client.held(event).await? {
            Held::Some(status) => {
                let chain = Chain::from_hex(&status.chain).ok_or_else(|| {
                    PushError::Server("The server's answer could not be read.".into())
                })?;
                (status.len, chain)
            }
            Held::Nothing => (0, Chain::start()),
            Held::Removed => {
                known.removed = true;
                return Ok((len, 0, LinkEventState::Removed));
            }
        },
    };
    // Is what the server holds the start of this log?
    let same = sent <= len && Chain::start().then_all(&all[..sent.min(len) as usize]) == chain;
    if !same {
        known.server = Some((sent, chain));
        known.different = true;
        return Ok((len, sent, LinkEventState::Different));
    }
    known.different = false;

    while sent < len {
        let rest = &all[sent as usize..];
        let batch = &rest[..batch_len(rest)];
        match client.post_log(event, sent, chain, batch).await? {
            Ok(status) => {
                let expected = chain.then_all(batch);
                if status.len != sent + batch.len() as u64 || status.chain != expected.to_hex() {
                    // Somebody else is writing this copy. Ask again from the top next time.
                    known.server = None;
                    return Err(PushError::Server(
                        "The server's copy changed while it was being sent. It will be checked again.".into(),
                    ));
                }
                sent = status.len;
                chain = expected;
                known.server = Some((sent, chain));
            }
            Err(refused) => {
                known.server = None;
                return match refused.reason {
                    PushRefusal::Different => {
                        known.different = true;
                        Ok((len, refused.held.len, LinkEventState::Different))
                    }
                    PushRefusal::Removed => {
                        known.removed = true;
                        Ok((len, 0, LinkEventState::Removed))
                    }
                    // The server lost some of it (its copy was removed). Start over next time.
                    PushRefusal::Gap => Err(PushError::Server(
                        "The server holds less than was sent. It will be sent again.".into(),
                    )),
                };
            }
        }
    }
    known.server = Some((sent, chain));

    let mark: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&header).map_err(|e| PushError::Server(e.to_string()))?)
            .into();
    if known.header != Some(mark) {
        if !client.put_header(event, &header).await? {
            *known = Known {
                removed: true,
                ..Known::default()
            };
            return Ok((len, 0, LinkEventState::Removed));
        }
        known.header = Some(mark);
    }
    Ok((len, sent, LinkEventState::Sent))
}

/// Now and then, ask the server what it holds of every event, and forget what was
/// remembered about any that changed there. An event can change under a Director that
/// sends nothing new: someone removed it on the server's page, the server was put back
/// from a backup, or another Director sent to it.
async fn check_held(client: &Client, memory: &mut Memory) -> Result<(), PushError> {
    let due = memory
        .held_asked
        .is_none_or(|asked| asked.elapsed() >= HELD_EVERY);
    if !due {
        return Ok(());
    }
    memory.held_asked = Some(std::time::Instant::now());
    // An older server cannot say. Then each event is found out as it is sent, as before.
    let Some(all) = client.events().await? else {
        return Ok(());
    };
    for (id, known) in memory.events.iter_mut() {
        let theirs = all.iter().find(|held| &held.id == id);
        let removed = theirs.is_some_and(|held| held.removed);
        if removed != known.removed {
            *known = Known {
                removed,
                ..Known::default()
            };
            continue;
        }
        let Some((len, chain)) = known.server else {
            continue;
        };
        let same = match theirs {
            Some(held) => held.len == len && held.chain == chain.to_hex(),
            // Nothing there is the same as an empty copy, but the header is gone too.
            None => false,
        };
        if !same {
            *known = Known::default();
        }
    }
    Ok(())
}

/// Look at every event once, and bring the server's copies up. `Err` only for what stops
/// all of them: the server is unreachable, or the token is not taken.
pub async fn sync_all(
    registry: &EventRegistry,
    client: &Client,
    memory: &mut Memory,
) -> Result<(), PushError> {
    let desk = registry.link();
    let events = registry.list();
    let ids: Vec<EventId> = events.iter().map(|meta| meta.id.clone()).collect();
    memory.events.retain(|id, _| ids.contains(id));
    desk.keep_events(&ids);
    check_held(client, memory).await?;

    for meta in events {
        let known = memory.events.entry(meta.id.clone()).or_default();
        let (len, sent, state) = match sync_event(registry, &desk, client, known, &meta.id).await {
            Ok(done) => done,
            Err(PushError::Token) => return Err(PushError::Token),
            Err(PushError::Server(why)) => {
                let sent = known.server.map_or(0, |(sent, _)| sent);
                desk.report_event(LinkEvent {
                    id: meta.id.clone(),
                    name: meta.name.clone(),
                    len: registry
                        .log(&meta.id)
                        .and_then(|log| log.lock().ok()?.len().ok())
                        .unwrap_or(sent),
                    sent,
                    state: LinkEventState::Failed {
                        message: why.clone(),
                    },
                });
                // One event that cannot be sent does not hold the others up. A server that
                // cannot be reached at all does.
                if why.starts_with("The server could not be reached") {
                    return Err(PushError::Server(why));
                }
                continue;
            }
        };
        desk.report_event(LinkEvent {
            id: meta.id,
            name: meta.name,
            len,
            sent,
            state,
        });
    }
    Ok(())
}

/// Keep the server and the Director agreed about pilots (§13.7):
///
/// - **the pilots this Director has** go up when they change, so the club knows a pilot
///   before any event names them;
/// - **who holds each pilot** comes down now and then, so a pilot who claimed their results
///   on the server shows as tied on the Pilots page.
///
/// With `now` true the server is asked who holds what whether or not it is time.
///
/// Nothing here holds racing or the events up. A server that does not answer these (an
/// older one has no such routes) is asked again later.
pub async fn sync_pilots(
    registry: &EventRegistry,
    client: &Client,
    memory: &mut Memory,
    now: bool,
) -> Result<(), PushError> {
    let pilots = registry.pilots();
    let ours: Vec<_> = pilots
        .list()
        .iter()
        .map(gridfpv_server::link_pilots::push_pilot)
        .collect();
    let mark: [u8; 32] =
        Sha256::digest(serde_json::to_vec(&ours).map_err(|e| PushError::Server(e.to_string()))?)
            .into();
    if memory.pilots != Some(mark) {
        client.put_pilots(&ours).await?;
        memory.pilots = Some(mark);
    }

    let due = memory
        .accounts_asked
        .is_none_or(|asked| asked.elapsed() >= ACCOUNTS_EVERY);
    if now || due {
        // Whether or not it answers, it is not asked again at once.
        memory.accounts_asked = Some(std::time::Instant::now());
        let before = pilots.account_mark();
        let held = client
            .pilots()
            .await?
            .into_iter()
            .filter_map(|pilot| {
                let account = pilot.account?;
                Some((
                    pilot.id,
                    gridfpv_server::pilots::PilotAccount {
                        id: account.id,
                        name: account.name,
                    },
                ))
            })
            .collect();
        pilots
            .take_server_accounts(before, &held)
            .map_err(|e| PushError::Server(e.to_string()))?;
    }
    Ok(())
}

/// The task that listens to the server, stopped when this is dropped.
struct Listening(JoinHandle<()>);

impl Drop for Listening {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start pushing. Runs until aborted.
pub fn spawn(registry: EventRegistry) -> JoinHandle<()> {
    tokio::spawn(async move {
        let desk = registry.link();
        let mut changed = desk.watch();
        loop {
            changed.borrow_and_update();
            let stored = desk.stored();
            if !stored.ready() {
                // The desk shows "not set" or "off" from the settings themselves.
                if changed.changed().await.is_err() {
                    return;
                }
                continue;
            }
            desk.report(LinkState::Connecting);
            let connected = async {
                let client = Client::new(&stored)?;
                let hello = client.hello().await?;
                Ok::<_, PushError>((client, hello))
            };
            let (client, hello) = match connected.await {
                Ok(connected) => connected,
                Err(why) => {
                    desk.report(LinkState::Failed {
                        message: why.to_string(),
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(RETRY) => {}
                        gone = changed.changed() => if gone.is_err() { return },
                    }
                    continue;
                }
            };
            desk.report_server(hello.club, hello.version);
            desk.report(LinkState::Connected);
            // While connected, hear what pilots ask for on the server (§13.4). It stops
            // when this connection is given up, below.
            let _listening = Client::new(&stored)
                .ok()
                .map(|client| Listening(crate::listen::spawn(registry.clone(), client)));

            // Connected. Push until the settings change or the server stops answering.
            let mut memory = Memory::default();
            // The open event says when its log grows, so a lap goes up at once. It is
            // listened to from BEFORE each look, not after: a lap that lands while a look
            // is under way must not wait for the next tick.
            let mut open: Option<(EventId, tokio::sync::watch::Receiver<u64>)> = None;
            loop {
                let active = registry.active().map(|event| event.id);
                if open.as_ref().map(|(id, _)| id) != active.as_ref() {
                    open = active.and_then(|id| {
                        let live = registry.live(&id)?;
                        Some((id, live.changed.subscribe()))
                    });
                }
                if let Some((_, grew)) = &mut open {
                    grew.borrow_and_update();
                }
                if let Err(why) = sync_all(&registry, &client, &mut memory).await {
                    desk.report(LinkState::Failed {
                        message: why.to_string(),
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(RETRY) => {}
                        gone = changed.changed() => if gone.is_err() { return },
                    }
                    break;
                }
                // The events are up. Pilots come after, and never hold them back.
                if let Err(PushError::Token) =
                    sync_pilots(&registry, &client, &mut memory, false).await
                {
                    // The token was taken back between two requests. The next look says so.
                    continue;
                }
                // Every other reason to look (another event, a pilot renamed) waits for
                // the tick.
                let grew = async {
                    match &mut open {
                        Some((_, grew)) => {
                            if grew.changed().await.is_err() {
                                // The event is gone. The tick finds what is open now.
                                std::future::pending::<()>().await;
                            }
                        }
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    _ = tokio::time::sleep(TICK) => {}
                    _ = grew => {}
                    gone = changed.changed() => {
                        if gone.is_err() { return }
                        // A replace was asked for, or the settings changed.
                        if desk.stored() != stored { break }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(offset: u64, json: &str, recorded_at: Option<i64>) -> RawEntry {
        RawEntry {
            offset,
            recorded_at,
            json: json.to_owned(),
        }
    }

    #[test]
    fn a_body_keeps_each_entry_as_it_is_stored() {
        let odd = r#"{ "B" : 1, "A":[2,  3] }"#;
        let entries = [entry(4, odd, Some(-7)), entry(5, r#""x""#, None)];
        let body = log_body(4, Chain::start(), &entries, 99);
        assert!(body.contains(odd), "{body}");

        // And the server's own reader gets the same text back out.
        let read: gridfpv_server::remote::PushLog = serde_json::from_str(&body).unwrap();
        assert_eq!(read.from, 4);
        assert_eq!(read.before, Chain::start().to_hex());
        assert_eq!(read.sent_at, Some(99));
        assert_eq!(read.entries.len(), 2);
        assert_eq!(read.entries[0].event.get(), odd);
        assert_eq!(read.entries[0].recorded_at, Some(-7));
        assert_eq!(read.entries[1].event.get(), r#""x""#);
        assert_eq!(read.entries[1].recorded_at, None);
    }

    #[test]
    fn an_empty_body_is_still_a_body() {
        let read: gridfpv_server::remote::PushLog =
            serde_json::from_str(&log_body(0, Chain::start(), &[], 1)).unwrap();
        assert!(read.entries.is_empty());
    }

    #[test]
    fn a_batch_is_never_empty_and_never_over_the_limits() {
        let small: Vec<RawEntry> = (0..1200).map(|i| entry(i, "{}", None)).collect();
        assert_eq!(batch_len(&small), BATCH_ENTRIES);
        assert_eq!(batch_len(&small[..3]), 3);
        assert_eq!(batch_len(&[]), 0);

        // Big entries: as many as fit under the size.
        let big = "x".repeat(BATCH_BYTES / 4 + 1);
        let bigs: Vec<RawEntry> = (0..10).map(|i| entry(i, &big, None)).collect();
        assert_eq!(batch_len(&bigs), 3);
        // One entry over the size on its own still goes, alone.
        let huge = "x".repeat(BATCH_BYTES + 1);
        let huges = [entry(0, &huge, None), entry(1, "{}", None)];
        assert_eq!(batch_len(&huges), 1);
    }
}
