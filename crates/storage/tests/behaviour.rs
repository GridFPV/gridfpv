//! Shared behavioural contract for any [`EventLog`] backend.
//!
//! Every assertion in `run_contract` must hold identically for the in-memory
//! and SQLite backends — that equivalence is the whole point of the trait. Each
//! `#[test]` builds a fresh backend and runs the same suite against it.

use gridfpv_events::{AdapterId, CompetitorRef, Event, GateIndex, Pass, SessionId, SourceTime};
use gridfpv_storage::{EventLog, InMemoryLog, SqliteLog, StoredEvent};

/// A representative spread of events covering every `Event` variant.
fn sample_events() -> Vec<Event> {
    let adapter = AdapterId("velocidrone".into());
    vec![
        Event::AdapterConnected {
            adapter: adapter.clone(),
        },
        Event::SessionStarted {
            adapter: adapter.clone(),
            session: SessionId("heat-1".into()),
        },
        Event::CompetitorSeen {
            adapter: adapter.clone(),
            competitor: CompetitorRef("AcroAce".into()),
        },
        Event::Pass(Pass {
            adapter: adapter.clone(),
            competitor: CompetitorRef("AcroAce".into()),
            at: SourceTime::from_micros(12_500_000),
            sequence: Some(1),
            gate: GateIndex::LAP,
            signal: None,
        }),
        Event::Pass(Pass {
            adapter: adapter.clone(),
            competitor: CompetitorRef("AcroAce".into()),
            at: SourceTime::from_micros(25_000_000),
            sequence: Some(2),
            gate: GateIndex(2),
            signal: None,
        }),
        Event::SessionEnded {
            adapter: adapter.clone(),
            session: SessionId("heat-1".into()),
        },
        Event::AdapterDisconnected { adapter },
    ]
}

/// Assert offsets are dense and contiguous from 0, in vector order.
fn assert_dense_from_zero(entries: &[StoredEvent]) {
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.offset, i as u64,
            "offset must equal append position (dense from 0)"
        );
    }
}

/// The full behavioural contract. Generic so the exact same code drives both
/// backends.
fn run_contract<L: EventLog>(log: &mut L) {
    // Empty to start.
    assert!(log.is_empty().unwrap());
    assert_eq!(log.len().unwrap(), 0);
    assert!(log.read_all().unwrap().is_empty());

    let events = sample_events();

    // --- single appends: offsets are monotonic & contiguous from 0 ---
    let o0 = log.append(events[0].clone(), Some(1_000)).unwrap();
    let o1 = log.append(events[1].clone(), None).unwrap();
    let o2 = log.append(events[2].clone(), Some(3_000)).unwrap();
    assert_eq!((o0, o1, o2), (0, 1, 2));
    assert_eq!(log.len().unwrap(), 3);
    assert!(!log.is_empty().unwrap());

    // --- batch append continues the offset sequence contiguously ---
    let rest: Vec<(Event, Option<i64>)> = events[3..].iter().cloned().map(|e| (e, None)).collect();
    let batch_first = log.append_batch(rest).unwrap();
    assert_eq!(batch_first, 3, "batch's first offset follows the singles");
    assert_eq!(log.len().unwrap() as usize, events.len());

    // --- read_all replays everything in append order, round-tripped intact ---
    let all = log.read_all().unwrap();
    assert_eq!(all.len(), events.len());
    assert_dense_from_zero(&all);
    for (entry, original) in all.iter().zip(events.iter()) {
        assert_eq!(
            &entry.event, original,
            "round-trip preserves event contents"
        );
    }
    // recorded_at survives as supplied (including None).
    assert_eq!(all[0].recorded_at, Some(1_000));
    assert_eq!(all[1].recorded_at, None);
    assert_eq!(all[2].recorded_at, Some(3_000));

    // --- read_from: replay resuming partway through ---
    let from2 = log.read_from(2).unwrap();
    assert_eq!(from2.len(), events.len() - 2);
    assert_eq!(from2[0].offset, 2);
    assert_eq!(&from2[0].event, &events[2]);
    // start at/after the end => empty.
    assert!(log.read_from(log.len().unwrap()).unwrap().is_empty());
    assert!(log.read_from(9_999).unwrap().is_empty());

    // --- read_range: half-open [start, end) ---
    let mid = log.read_range(1, 4).unwrap();
    let mid_offsets: Vec<u64> = mid.iter().map(|e| e.offset).collect();
    assert_eq!(mid_offsets, vec![1, 2, 3]);
    // empty / inverted ranges.
    assert!(log.read_range(2, 2).unwrap().is_empty());
    assert!(log.read_range(4, 1).unwrap().is_empty());
    // end past the log end is clamped.
    let tail = log.read_range(5, 1000).unwrap();
    assert_eq!(tail.len(), events.len() - 5);
    assert_eq!(tail[0].offset, 5);

    // --- empty batch is a no-op returning the next offset ---
    let next = log.append_batch(std::iter::empty()).unwrap();
    assert_eq!(next, events.len() as u64);
    assert_eq!(log.len().unwrap() as usize, events.len());
}

#[test]
fn in_memory_satisfies_contract() {
    let mut log = InMemoryLog::new();
    run_contract(&mut log);
}

#[test]
fn sqlite_in_memory_satisfies_contract() {
    let mut log = SqliteLog::open_in_memory().unwrap();
    run_contract(&mut log);
}

#[test]
fn sqlite_tempfile_satisfies_contract() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.sqlite3");
    let mut log = SqliteLog::open(&path).unwrap();
    run_contract(&mut log);
}

#[test]
fn sqlite_persists_across_reopen() {
    // Durability: what one connection appends, a fresh one replays in order.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.sqlite3");
    let events = sample_events();
    {
        let mut log = SqliteLog::open(&path).unwrap();
        let first = log
            .append_batch(events.iter().cloned().map(|e| (e, None)))
            .unwrap();
        assert_eq!(first, 0);
    }
    let reopened = SqliteLog::open(&path).unwrap();
    let replayed = reopened.read_all().unwrap();
    assert_eq!(replayed.len(), events.len());
    assert_dense_from_zero(&replayed);
    for (entry, original) in replayed.iter().zip(events.iter()) {
        assert_eq!(&entry.event, original);
    }
}

#[test]
fn backends_agree_byte_for_byte() {
    // The two backends must produce identical StoredEvent sequences from the
    // same input — the equivalence the trait exists to guarantee.
    let events = sample_events();

    let mut mem = InMemoryLog::new();
    mem.append_batch(events.iter().cloned().map(|e| (e, Some(7))))
        .unwrap();

    let mut sql = SqliteLog::open_in_memory().unwrap();
    sql.append_batch(events.iter().cloned().map(|e| (e, Some(7))))
        .unwrap();

    assert_eq!(mem.read_all().unwrap(), sql.read_all().unwrap());
}
