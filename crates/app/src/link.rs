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
use gridfpv_server::link::{LinkDesk, LinkEvent, LinkEventState, LinkState, LinkStored, LinkTrust};
use gridfpv_server::remote::{PushHeader, PushHello, PushRefusal, PushRefused, PushStatus};
use gridfpv_storage::{Chain, RawEntry};
use sha2::{Digest, Sha256};
use tokio::task::JoinHandle;

/// The most entries in one batch. The server takes ten times this.
const BATCH_ENTRIES: usize = 500;

/// About the most text in one batch, so one request stays small beside the server's limit.
const BATCH_BYTES: usize = 4 * 1024 * 1024;

/// How often every event is looked at, when nothing says to look sooner.
const TICK: Duration = Duration::from_secs(1);

/// How long to wait before reaching for a server that did not answer.
const RETRY: Duration = Duration::from_secs(5);

/// What went wrong talking to the server, in words for the director.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    /// The server could not be reached, or what it said made no sense. Worth trying again.
    Server(String),
    /// The server did not take the token.
    Token,
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(why) => f.write_str(why),
            Self::Token => f.write_str(
                "The server did not take the link token. It may have been taken back, or it is for another server.",
            ),
        }
    }
}

/// A connection to one server, with one token.
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// A client for the server in the settings. Fails only when the settings cannot make
    /// one (extra authorities that are not certificates).
    pub fn new(stored: &LinkStored) -> Result<Self, PushError> {
        let mut builder = gridfpv_server::http::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            // The desk let through plain HTTP only for this same computer (§13.5).
            .https_only(stored.url.starts_with("https://"));
        match &stored.trust {
            LinkTrust::System => {}
            LinkTrust::ExtraAuthorities { pem } => {
                let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
                    .ok()
                    .filter(|certs| !certs.is_empty())
                    .ok_or_else(|| {
                        PushError::Server(
                            "The extra certificate authorities could not be read.".into(),
                        )
                    })?;
                builder = builder.tls_certs_merge(certs);
            }
        }
        let http = builder
            .build()
            .map_err(|e| PushError::Server(format!("The connection could not be set up: {e}")))?;
        Ok(Self {
            http,
            base: stored.url.clone(),
            token: stored.token.clone(),
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, PushError> {
        let response = request.send().await.map_err(|e| {
            PushError::Server(format!("The server could not be reached: {}", plain(e)))
        })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(PushError::Token);
        }
        Ok(response)
    }

    /// Whatever else the server answered, as a reason.
    async fn unexpected(response: reqwest::Response) -> PushError {
        let status = response.status();
        let said = response
            .json::<gridfpv_server::error::ProtocolError>()
            .await
            .map(|e| e.message)
            .unwrap_or_default();
        PushError::Server(if said.is_empty() {
            format!("The server answered {status}.")
        } else {
            format!("The server said: {said}")
        })
    }

    async fn read<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, PushError> {
        response.json::<T>().await.map_err(|e| {
            PushError::Server(format!(
                "The server's answer could not be read. Is this a Grid server? ({})",
                plain(e)
            ))
        })
    }

    /// Which club the token pushes into.
    pub async fn hello(&self) -> Result<PushHello, PushError> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/push/hello"))
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await
    }

    /// How much of an event the server holds. `None` when it holds none of it.
    pub async fn held(&self, event: &EventId) -> Result<Option<PushStatus>, PushError> {
        let path = format!("/push/events/{}", event.0);
        let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await.map(Some)
    }

    /// Send an event's header.
    pub async fn put_header(&self, event: &EventId, header: &PushHeader) -> Result<(), PushError> {
        let path = format!("/push/events/{}", event.0);
        let response = self
            .send(self.request(reqwest::Method::PUT, &path).json(header))
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Ok(())
    }

    /// Send a run of entries that starts at `from`, where `before` is the chain of what is
    /// ahead of them. `Ok(Err(..))` is the server saying it did not take them, and why.
    pub async fn post_log(
        &self,
        event: &EventId,
        from: u64,
        before: Chain,
        entries: &[RawEntry],
    ) -> Result<Result<PushStatus, PushRefused>, PushError> {
        let path = format!("/push/events/{}/log", event.0);
        let response = self
            .send(
                self.request(reqwest::Method::POST, &path)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(log_body(from, before, entries, now_micros())),
            )
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Self::read(response).await.map(Err);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await.map(Ok)
    }

    /// Remove the server's copy of an event. Holding none is as good.
    pub async fn delete(&self, event: &EventId) -> Result<(), PushError> {
        let path = format!("/push/events/{}", event.0);
        let response = self
            .send(self.request(reqwest::Method::DELETE, &path))
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(Self::unexpected(response).await)
    }
}

/// A network error without the address in it twice, and with its cause.
fn plain(error: reqwest::Error) -> String {
    use std::error::Error;
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// The body of a log push. It is built as text so each entry goes as it is stored:
/// passing an entry through a JSON value would write its keys in another order, and the
/// server would hold other text than the chain was worked out from.
pub fn log_body(from: u64, before: Chain, entries: &[RawEntry], sent_at: i64) -> String {
    let mut body = format!(
        "{{\"from\":{from},\"before\":\"{}\",\"entries\":[",
        before.to_hex()
    );
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        match entry.recorded_at {
            Some(at) => body.push_str(&format!("{{\"recorded_at\":{at},\"event\":")),
            None => body.push_str("{\"event\":"),
        }
        body.push_str(&entry.json);
        body.push('}');
    }
    body.push_str(&format!("],\"sent_at\":{sent_at}}}"));
    body
}

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
}

/// Everything remembered while connected to one server. Dropped when the link changes.
#[derive(Default)]
pub struct Memory {
    events: HashMap<EventId, Known>,
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

    let (mut sent, mut chain) = match known.server {
        Some(server) => server,
        None => match client.held(event).await? {
            Some(status) => {
                let chain = Chain::from_hex(&status.chain).ok_or_else(|| {
                    PushError::Server("The server's answer could not be read.".into())
                })?;
                (status.len, chain)
            }
            None => (0, Chain::start()),
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
        client.put_header(event, &header).await?;
        known.header = Some(mark);
    }
    Ok((len, sent, LinkEventState::Sent))
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

            // Connected. Push until the settings change or the server stops answering.
            let mut memory = Memory::default();
            loop {
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
                // The open event says when its log grows, so a lap goes up at once. Every
                // other reason to look (another event, a pilot renamed) waits for the tick.
                let mut open = registry
                    .active()
                    .and_then(|event| registry.live(&event.id))
                    .map(|live| live.changed.subscribe());
                let grew = async {
                    match &mut open {
                        Some(open) => {
                            let _ = open.changed().await;
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
