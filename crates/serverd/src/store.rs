//! The server's copy of each event (§13.3).
//!
//! One event is one SQLite file, `clubs/<club>/<event>.sqlite` under the data folder. It is
//! the same kind of file a Director keeps: the log, and a small table beside it that holds
//! the event's header.
//!
//! The copy only grows, and only by what a Director sends. [`Copy::append`] is the one way
//! in, and it takes a batch only when the batch continues the log that is held:
//!
//! - it starts at the end of the copy, and everything before it is the same log; or
//! - it starts earlier and repeats what is held, entry for entry, then goes on. This is a
//!   Director sending again after it lost the answer to a batch.
//!
//! Anything else is refused and changes nothing. "The same log" is judged by the chain
//! (`gridfpv_storage::Chain`), never by the event's id: an event made again from nothing,
//! or brought back from an old backup, keeps its id and is a different log.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gridfpv_engine::state::EventState;
use gridfpv_server::events::EventId;
use gridfpv_storage::{Chain, EventLog, Offset, RawEntry, SqliteLog, StorageError};
use tokio::sync::watch;

use crate::book::ClubId;

/// The key, in an event file's `meta` table, of the header a Director sent.
const HEADER_KEY: &str = "push_header";

/// The key of how far the Director's clock is ahead of this server's, in microseconds.
const CLOCK_KEY: &str = "push_clock_ahead";

/// Where clubs' events are kept, under the data folder.
const CLUBS_DIR: &str = "clubs";

const EVENT_SUFFIX: &str = ".sqlite";

/// Whether text will do as an event's id here. An id becomes a file name, so it may hold
/// only what a Director's own ids hold: lower-case letters, digits and hyphens.
pub fn valid_event_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.starts_with('-')
}

/// How much of a log is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Held {
    pub len: Offset,
    pub chain: Chain,
}

/// Why a batch was not taken. Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// It starts after the end of the copy.
    Gap,
    /// It belongs to a different log than the one held.
    Different,
}

/// What came of a batch: what is held now, or why it was not taken and what is held.
pub type Taken = Result<Held, (Refusal, Held)>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Storage(#[from] StorageError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

struct Inner {
    log: SqliteLog,
    held: Held,
    /// The event as its log makes it, worked out as far as `state.applied`.
    state: EventState,
    /// How far the Director's clock is ahead of this server's, in microseconds. Every time
    /// in the log is on the Director's clock, so a page that counts a race down needs to
    /// know what that clock says now.
    clock_ahead: i64,
}

/// The server's copy of one event.
pub struct Copy {
    inner: Mutex<Inner>,
    /// The length of the copy, sent each time it grows, for pages that follow the event.
    grew: watch::Sender<u64>,
}

impl Copy {
    fn open(path: &Path) -> Result<Self, StoreError> {
        let log = SqliteLog::open(path)?;
        // The chain is worked out from the file each time it is opened. It is not kept
        // beside the log, so it can never disagree with it.
        let raw = log.read_raw_from(0)?;
        let held = Held {
            len: raw.len() as Offset,
            chain: Chain::start().then_all(&raw),
        };
        let clock_ahead = log
            .get_meta(CLOCK_KEY)?
            .and_then(|text| text.parse().ok())
            .unwrap_or(0);
        Ok(Self {
            grew: watch::Sender::new(held.len),
            inner: Mutex::new(Inner {
                log,
                held,
                state: EventState::new(),
                clock_ahead,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves the file as SQLite left it, which is whole.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// How much is held.
    pub fn held(&self) -> Held {
        self.lock().held
    }

    /// Told the copy's length each time it grows.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.grew.subscribe()
    }

    /// The header a Director last sent, as its JSON. `None` before the first.
    pub fn header(&self) -> Result<Option<String>, StoreError> {
        Ok(self.lock().log.get_meta(HEADER_KEY)?)
    }

    /// Keep a header, in place of the one before.
    pub fn set_header(&self, json: &str) -> Result<(), StoreError> {
        self.lock().log.set_meta(HEADER_KEY, json)?;
        // A page following the event reads the header again too.
        self.grew.send_modify(|_| {});
        Ok(())
    }

    /// Note what the Director's clock said (`director_now`) when this server's said
    /// `server_now`, both in microseconds since 1970.
    pub fn set_clock(&self, director_now: i64, server_now: i64) -> Result<(), StoreError> {
        let ahead = director_now.saturating_sub(server_now);
        let mut inner = self.lock();
        inner.clock_ahead = ahead;
        inner.log.set_meta(CLOCK_KEY, &ahead.to_string())?;
        Ok(())
    }

    /// What the Director's clock says when this server's says `server_now`, as far as the
    /// last push told. The server's own clock until a Director has said.
    pub fn director_now(&self, server_now: i64) -> i64 {
        server_now.saturating_add(self.lock().clock_ahead)
    }

    /// Look at the event as its log makes it. The state is brought up to the end of the
    /// copy first.
    ///
    /// Nothing is ever added here for time having passed (a countdown reaching its tone):
    /// that is a fact only the Director writes, and it arrives with the next push.
    pub fn with_state<R>(&self, f: impl FnOnce(&EventState) -> R) -> Result<R, StoreError> {
        let mut inner = self.lock();
        let inner = &mut *inner;
        for stored in inner.log.read_from(inner.state.applied)? {
            inner
                .state
                .apply(stored.offset, stored.recorded_at, &stored.event);
        }
        Ok(f(&inner.state))
    }

    /// Every entry from `start` on, as stored.
    pub fn read_raw_from(&self, start: Offset) -> Result<Vec<RawEntry>, StoreError> {
        Ok(self.lock().log.read_raw_from(start)?)
    }

    /// Every entry from `start` on, parsed.
    pub fn read_from(
        &self,
        start: Offset,
    ) -> Result<Vec<gridfpv_storage::StoredEvent>, StoreError> {
        Ok(self.lock().log.read_from(start)?)
    }

    /// Add a batch a Director sent: entries starting at `from`, with `before` the chain of
    /// everything ahead of them. See the module docs for when it is taken.
    ///
    /// The caller has already made sure each entry reads as an event.
    pub fn append(
        &self,
        from: Offset,
        before: Chain,
        entries: &[(String, Option<i64>)],
    ) -> Result<Taken, StoreError> {
        let mut inner = self.lock();
        let held = inner.held;
        if from > held.len {
            return Ok(Err((Refusal::Gap, held)));
        }
        // How many of the entries repeat what is held, and the chain where they start.
        let repeated = ((held.len - from) as usize).min(entries.len());
        let chain_at_from = if from == held.len {
            held.chain
        } else {
            Chain::start().then_all(&inner.log.read_raw_range(0, from)?)
        };
        if chain_at_from != before {
            return Ok(Err((Refusal::Different, held)));
        }
        if repeated > 0 {
            let stored = inner.log.read_raw_range(from, from + repeated as Offset)?;
            let same = stored
                .iter()
                .zip(entries)
                .all(|(stored, (json, at))| &stored.json == json && &stored.recorded_at == at);
            if !same {
                return Ok(Err((Refusal::Different, held)));
            }
        }
        let new = &entries[repeated..];
        if new.is_empty() {
            return Ok(Ok(held));
        }
        inner
            .log
            .append_raw_batch(new.iter().map(|(json, at)| (json.as_str(), *at)))?;
        let chain = new
            .iter()
            .fold(held.chain, |chain, (json, at)| chain.then(json, *at));
        inner.held = Held {
            len: held.len + new.len() as Offset,
            chain,
        };
        let held = inner.held;
        drop(inner);
        self.grew.send_replace(held.len);
        Ok(Ok(held))
    }
}

/// The copies that are open, by club and event.
type Open = HashMap<(ClubId, EventId), Arc<Copy>>;

/// Every event this server holds. Cloning shares the one store.
#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
    open: Arc<Mutex<Open>>,
}

impl Store {
    /// The store in a data folder.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
            open: Arc::default(),
        }
    }

    fn club_dir(&self, club: &ClubId) -> PathBuf {
        self.dir.join(CLUBS_DIR).join(&club.0)
    }

    fn path(&self, club: &ClubId, event: &EventId) -> PathBuf {
        self.club_dir(club)
            .join(format!("{}{EVENT_SUFFIX}", event.0))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn open_at(
        &self,
        club: &ClubId,
        event: &EventId,
        create: bool,
    ) -> Result<Option<Arc<Copy>>, StoreError> {
        if !valid_event_id(&event.0) {
            return Ok(None);
        }
        // The map is held across the open, so two requests for a new event make one file
        // and share one copy.
        let mut open = self.lock();
        let key = (club.clone(), event.clone());
        if let Some(copy) = open.get(&key) {
            return Ok(Some(copy.clone()));
        }
        let path = self.path(club, event);
        if !path.is_file() {
            if !create {
                return Ok(None);
            }
            std::fs::create_dir_all(self.club_dir(club))?;
        }
        let copy = Arc::new(Copy::open(&path)?);
        open.insert(key, copy.clone());
        Ok(Some(copy))
    }

    /// A club's copy of an event. `None` when the club holds no such event.
    pub fn get(&self, club: &ClubId, event: &EventId) -> Result<Option<Arc<Copy>>, StoreError> {
        self.open_at(club, event, false)
    }

    /// A club's copy of an event, made empty if the club holds none. `None` only when the id
    /// will not do (see [`valid_event_id`]).
    pub fn get_or_create(
        &self,
        club: &ClubId,
        event: &EventId,
    ) -> Result<Option<Arc<Copy>>, StoreError> {
        self.open_at(club, event, true)
    }

    /// Remove a club's copy of an event. `false` when it held none.
    pub fn delete(&self, club: &ClubId, event: &EventId) -> Result<bool, StoreError> {
        if !valid_event_id(&event.0) {
            return Ok(false);
        }
        let mut open = self.lock();
        open.remove(&(club.clone(), event.clone()));
        let path = self.path(club, event);
        if !path.is_file() {
            return Ok(false);
        }
        std::fs::remove_file(&path)?;
        // What SQLite keeps beside a file that is open for writing.
        for beside in ["-wal", "-shm"] {
            let mut name = path.clone().into_os_string();
            name.push(beside);
            match std::fs::remove_file(PathBuf::from(name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(true)
    }

    /// The ids of every event a club holds, in order.
    pub fn list(&self, club: &ClubId) -> Result<Vec<EventId>, StoreError> {
        let dir = match std::fs::read_dir(self.club_dir(club)) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut ids = Vec::new();
        for entry in dir {
            let name = entry?.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(EVENT_SUFFIX)) else {
                continue;
            };
            if valid_event_id(id) {
                ids.push(EventId(id.to_owned()));
            }
        }
        ids.sort();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn club() -> ClubId {
        ClubId("club".into())
    }
    fn event(id: &str) -> EventId {
        EventId(id.into())
    }
    fn entries(texts: &[&str]) -> Vec<(String, Option<i64>)> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    format!("{{\"SessionEnded\":{{\"adapter\":\"a\",\"session\":\"{t}\"}}}}"),
                    Some(i as i64),
                )
            })
            .collect()
    }
    fn chain_of(entries: &[(String, Option<i64>)]) -> Chain {
        entries
            .iter()
            .fold(Chain::start(), |c, (json, at)| c.then(json, *at))
    }
    fn fresh() -> (tempfile::TempDir, Store, Arc<Copy>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let copy = store
            .get_or_create(&club(), &event("night-1"))
            .unwrap()
            .unwrap();
        (dir, store, copy)
    }

    #[test]
    fn ids_that_would_leave_the_folder_are_not_ids() {
        for good in ["a", "thursday-club-night-764c5x", "0", &"a".repeat(120)] {
            assert!(valid_event_id(good), "{good:?}");
        }
        for bad in [
            "",
            "..",
            ".",
            "a/b",
            "a\\b",
            "../x",
            "a.sqlite",
            "A",
            "a b",
            "a\0",
            "é",
            "-a",
            "a_b",
            &"a".repeat(121),
        ] {
            assert!(!valid_event_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_bad_id_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        for bad in ["../escape", "a/b", ""] {
            assert!(store.get_or_create(&club(), &event(bad)).unwrap().is_none());
            assert!(store.get(&club(), &event(bad)).unwrap().is_none());
            assert!(!store.delete(&club(), &event(bad)).unwrap());
        }
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_new_copy_is_empty_and_starts_the_chain() {
        let (_dir, _store, copy) = fresh();
        assert_eq!(
            copy.held(),
            Held {
                len: 0,
                chain: Chain::start()
            }
        );
        assert_eq!(copy.header().unwrap(), None);
    }

    #[test]
    fn batches_that_follow_on_are_taken() {
        let (_dir, _store, copy) = fresh();
        let all = entries(&["a", "b", "c", "d"]);
        let first = copy.append(0, Chain::start(), &all[..2]).unwrap().unwrap();
        assert_eq!(first.len, 2);
        assert_eq!(first.chain, chain_of(&all[..2]));
        let second = copy.append(2, first.chain, &all[2..]).unwrap().unwrap();
        assert_eq!(
            second,
            Held {
                len: 4,
                chain: chain_of(&all)
            }
        );
        assert_eq!(copy.held(), second);
        let stored: Vec<_> = copy
            .read_raw_from(0)
            .unwrap()
            .into_iter()
            .map(|e| (e.json, e.recorded_at))
            .collect();
        assert_eq!(stored, all);
    }

    #[test]
    fn an_empty_batch_at_the_end_is_a_question_and_changes_nothing() {
        let (_dir, _store, copy) = fresh();
        let all = entries(&["a"]);
        copy.append(0, Chain::start(), &all).unwrap().unwrap();
        let held = copy.append(1, chain_of(&all), &[]).unwrap().unwrap();
        assert_eq!(held.len, 1);
        // With the wrong chain, even an empty batch is told the logs differ.
        assert_eq!(
            copy.append(1, Chain::start(), &[]).unwrap(),
            Err((Refusal::Different, held))
        );
    }

    #[test]
    fn a_batch_past_the_end_is_a_gap() {
        let (_dir, _store, copy) = fresh();
        let all = entries(&["a", "b", "c"]);
        copy.append(0, Chain::start(), &all[..1]).unwrap().unwrap();
        let held = copy.held();
        assert_eq!(
            copy.append(2, chain_of(&all[..2]), &all[2..]).unwrap(),
            Err((Refusal::Gap, held))
        );
        assert_eq!(copy.held(), held);
    }

    #[test]
    fn a_batch_sent_again_is_taken_once() {
        // The Director sent a batch, the answer was lost, and it sends the same again.
        let (_dir, _store, copy) = fresh();
        let all = entries(&["a", "b", "c"]);
        copy.append(0, Chain::start(), &all).unwrap().unwrap();
        let again = copy.append(0, Chain::start(), &all).unwrap().unwrap();
        assert_eq!(
            again,
            Held {
                len: 3,
                chain: chain_of(&all)
            }
        );
        // From the middle, too.
        let again = copy
            .append(1, chain_of(&all[..1]), &all[1..])
            .unwrap()
            .unwrap();
        assert_eq!(again.len, 3);
        assert_eq!(copy.read_raw_from(0).unwrap().len(), 3);
    }

    #[test]
    fn a_batch_that_repeats_some_and_adds_more_adds_only_the_more() {
        let (_dir, _store, copy) = fresh();
        let all = entries(&["a", "b", "c", "d", "e"]);
        copy.append(0, Chain::start(), &all[..3]).unwrap().unwrap();
        let held = copy
            .append(1, chain_of(&all[..1]), &all[1..])
            .unwrap()
            .unwrap();
        assert_eq!(
            held,
            Held {
                len: 5,
                chain: chain_of(&all)
            }
        );
        let stored: Vec<_> = copy
            .read_raw_from(0)
            .unwrap()
            .into_iter()
            .map(|e| e.json)
            .collect();
        assert_eq!(
            stored,
            all.iter().map(|(j, _)| j.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn another_log_under_the_same_id_is_refused_and_changes_nothing() {
        let (_dir, _store, copy) = fresh();
        let ours = entries(&["a", "b", "c"]);
        copy.append(0, Chain::start(), &ours).unwrap().unwrap();
        let held = copy.held();
        let refused = Err((Refusal::Different, held));

        // An event made again from nothing: it starts at 0 with other entries.
        let other = entries(&["x", "y", "z", "w"]);
        assert_eq!(copy.append(0, Chain::start(), &other).unwrap(), refused);
        // A log that was the same for a while and then was not, sent from the end.
        let forked = entries(&["a", "b", "DIFFERENT"]);
        assert_eq!(
            copy.append(3, chain_of(&forked), &entries(&["d"])).unwrap(),
            refused
        );
        // The same, sent from the middle: the chain before is right, the repeat is not.
        assert_eq!(
            copy.append(2, chain_of(&ours[..2]), &forked[2..]).unwrap(),
            refused
        );
        // The same text at another time is another entry.
        let mut retimed = ours.clone();
        retimed[2].1 = Some(99);
        assert_eq!(
            copy.append(2, chain_of(&ours[..2]), &retimed[2..]).unwrap(),
            refused
        );
        // A shorter log (brought back from an old backup) that then grew another way.
        assert_eq!(
            copy.append(2, chain_of(&ours[..2]), &entries(&["q", "r", "s"])[..])
                .unwrap(),
            refused
        );

        assert_eq!(copy.held(), held);
        assert_eq!(copy.read_raw_from(0).unwrap().len(), 3);
    }

    #[test]
    fn a_copy_is_the_same_when_the_file_is_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        let all = entries(&["a", "b"]);
        {
            let store = Store::new(dir.path());
            let copy = store.get_or_create(&club(), &event("e")).unwrap().unwrap();
            copy.append(0, Chain::start(), &all).unwrap().unwrap();
            copy.set_header("{\"h\":1}").unwrap();
        }
        let store = Store::new(dir.path());
        let copy = store.get(&club(), &event("e")).unwrap().unwrap();
        assert_eq!(
            copy.held(),
            Held {
                len: 2,
                chain: chain_of(&all)
            }
        );
        assert_eq!(copy.header().unwrap().as_deref(), Some("{\"h\":1}"));
        // And it goes on from there.
        let more = entries(&["a", "b", "c"]);
        assert_eq!(
            copy.append(2, chain_of(&all), &more[2..])
                .unwrap()
                .unwrap()
                .len,
            3
        );
    }

    #[test]
    fn asking_twice_gives_the_one_copy() {
        let (_dir, store, copy) = fresh();
        let again = store.get(&club(), &event("night-1")).unwrap().unwrap();
        assert!(Arc::ptr_eq(&copy, &again));
        assert!(store.get(&club(), &event("other")).unwrap().is_none());
    }

    #[test]
    fn a_club_does_not_see_another_clubs_event() {
        let (_dir, store, copy) = fresh();
        copy.append(0, Chain::start(), &entries(&["a"]))
            .unwrap()
            .unwrap();
        let other = ClubId("other".into());
        assert!(store.get(&other, &event("night-1")).unwrap().is_none());
        assert!(store.list(&other).unwrap().is_empty());
        // The other club can hold its own event under the same id.
        let theirs = store
            .get_or_create(&other, &event("night-1"))
            .unwrap()
            .unwrap();
        assert_eq!(theirs.held().len, 0);
        assert_eq!(copy.held().len, 1);
        assert!(
            !store
                .delete(&ClubId("third".into()), &event("night-1"))
                .unwrap()
        );
    }

    #[test]
    fn listing_gives_the_events_and_nothing_else_in_the_folder() {
        let (dir, store, _copy) = fresh();
        store
            .get_or_create(&club(), &event("a-first"))
            .unwrap()
            .unwrap();
        let folder = dir.path().join(CLUBS_DIR).join("club");
        std::fs::write(folder.join("notes.txt"), "x").unwrap();
        std::fs::write(folder.join("Bad Name.sqlite"), "x").unwrap();
        assert_eq!(
            store.list(&club()).unwrap(),
            vec![event("a-first"), event("night-1")]
        );
    }

    #[test]
    fn deleting_removes_the_file_and_a_new_copy_starts_empty() {
        let (dir, store, copy) = fresh();
        copy.append(0, Chain::start(), &entries(&["a", "b"]))
            .unwrap()
            .unwrap();
        assert!(store.delete(&club(), &event("night-1")).unwrap());
        assert!(!store.delete(&club(), &event("night-1")).unwrap());
        assert!(store.get(&club(), &event("night-1")).unwrap().is_none());
        assert!(store.list(&club()).unwrap().is_empty());
        let left: Vec<_> = std::fs::read_dir(dir.path().join(CLUBS_DIR).join("club"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");

        let again = store
            .get_or_create(&club(), &event("night-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            again.held(),
            Held {
                len: 0,
                chain: Chain::start()
            }
        );
    }

    #[test]
    fn a_watcher_hears_the_copy_grow() {
        let (_dir, _store, copy) = fresh();
        let mut watch = copy.watch();
        assert_eq!(*watch.borrow_and_update(), 0);
        copy.append(0, Chain::start(), &entries(&["a", "b"]))
            .unwrap()
            .unwrap();
        assert!(watch.has_changed().unwrap());
        assert_eq!(*watch.borrow_and_update(), 2);
        // A refused batch is not heard.
        let _ = copy.append(9, Chain::start(), &entries(&["x"])).unwrap();
        assert!(!watch.has_changed().unwrap());
        // A new header is.
        copy.set_header("{}").unwrap();
        assert!(watch.has_changed().unwrap());
    }

    #[test]
    fn many_senders_at_once_leave_one_whole_log() {
        // Eight threads each send the same log in the same batches. Whatever the order, the
        // copy ends as that log, once.
        let (_dir, _store, copy) = fresh();
        let all = entries(&["a", "b", "c", "d", "e", "f"]);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for start in [0usize, 2, 4] {
                        let before = chain_of(&all[..start]);
                        // A thread ahead of the copy is told Gap and tries again.
                        while let Err((reason, _)) = copy
                            .append(start as u64, before, &all[start..start + 2])
                            .unwrap()
                        {
                            assert_eq!(reason, Refusal::Gap);
                            std::thread::yield_now();
                        }
                    }
                });
            }
        });
        assert_eq!(
            copy.held(),
            Held {
                len: 6,
                chain: chain_of(&all)
            }
        );
    }
}
