//! Copying a log: reading entries as stored, writing them as text, and the chain that says
//! whether two copies are the same log.

use gridfpv_events::{AdapterId, Event, SeatRef, SessionId};
use gridfpv_storage::{Chain, EventLog, InMemoryLog, RawEntry, SqliteLog};

fn events() -> Vec<(Event, Option<i64>)> {
    let adapter = AdapterId("timer-1".into());
    vec![
        (
            Event::AdapterConnected {
                adapter: adapter.clone(),
            },
            Some(1_000),
        ),
        (
            Event::SessionStarted {
                adapter: adapter.clone(),
                session: SessionId("heat-1".into()),
            },
            None,
        ),
        (
            Event::SeatSeen {
                adapter: adapter.clone(),
                seat: SeatRef("A".into()),
            },
            Some(-7),
        ),
        (Event::AdapterDisconnected { adapter }, Some(i64::MAX)),
    ]
}

fn filled<L: EventLog>(mut log: L) -> L {
    log.append_batch(events()).unwrap();
    log
}

fn raw_matches_parsed<L: EventLog>(log: &L) {
    let raw = log.read_raw_from(0).unwrap();
    let parsed = log.read_all().unwrap();
    assert_eq!(raw.len(), parsed.len());
    for (raw, parsed) in raw.iter().zip(&parsed) {
        assert_eq!(raw.offset, parsed.offset);
        assert_eq!(raw.recorded_at, parsed.recorded_at);
        let event: Event = serde_json::from_str(&raw.json).unwrap();
        assert_eq!(event, parsed.event);
    }
    // From partway, at the end and past it.
    let tail = log.read_raw_from(2).unwrap();
    assert_eq!(tail.len(), parsed.len().saturating_sub(2));
    if let Some(first) = tail.first() {
        assert_eq!(first.offset, 2);
    }
    assert!(log.read_raw_from(parsed.len() as u64).unwrap().is_empty());
    assert!(log.read_raw_from(9_999).unwrap().is_empty());
}

#[test]
fn raw_entries_are_the_parsed_entries_in_both_backends() {
    raw_matches_parsed(&filled(InMemoryLog::new()));
    raw_matches_parsed(&filled(SqliteLog::open_in_memory().unwrap()));
    raw_matches_parsed(&InMemoryLog::new());
    raw_matches_parsed(&SqliteLog::open_in_memory().unwrap());
}

#[test]
fn both_backends_give_the_same_chain_for_the_same_events() {
    let memory = filled(InMemoryLog::new()).read_raw_from(0).unwrap();
    let sqlite = filled(SqliteLog::open_in_memory().unwrap())
        .read_raw_from(0)
        .unwrap();
    assert_eq!(
        Chain::start().then_all(&memory),
        Chain::start().then_all(&sqlite)
    );
}

#[test]
fn a_copy_made_from_raw_entries_is_the_same_log() {
    let from = filled(SqliteLog::open_in_memory().unwrap());
    let raw = from.read_raw_from(0).unwrap();

    // Sent in two parts, as a Director that lost its connection halfway would.
    let mut to = SqliteLog::open_in_memory().unwrap();
    let first = to
        .append_raw_batch(raw[..1].iter().map(|e| (e.json.as_str(), e.recorded_at)))
        .unwrap();
    let second = to
        .append_raw_batch(raw[1..].iter().map(|e| (e.json.as_str(), e.recorded_at)))
        .unwrap();
    assert_eq!((first, second), (0, 1));

    assert_eq!(to.read_raw_from(0).unwrap(), raw);
    assert_eq!(to.read_all().unwrap(), from.read_all().unwrap());
    assert_eq!(to.len().unwrap(), from.len().unwrap());
}

#[test]
fn stored_text_is_kept_exactly_even_when_this_build_would_write_it_otherwise() {
    // Odd spacing, another key order and a key this build does not know. A copy has to keep
    // all of it, or the chain at the next entry differs between the two ends.
    let odd =
        r#"{ "AdapterConnected" : { "extra_from_a_newer_build": [1, 2], "adapter": "timer-1" } }"#;
    let mut log = SqliteLog::open_in_memory().unwrap();
    log.append_raw_batch([(odd, Some(5))]).unwrap();

    let raw = log.read_raw_from(0).unwrap();
    assert_eq!(
        raw,
        vec![RawEntry {
            offset: 0,
            recorded_at: Some(5),
            json: odd.to_owned()
        }]
    );
    // It still reads as the event it is.
    assert_eq!(
        log.read_all().unwrap()[0].event,
        Event::AdapterConnected {
            adapter: AdapterId("timer-1".into())
        }
    );
    // And the tidy spelling of the same event is a different chain.
    let tidy = serde_json::to_string(&log.read_all().unwrap()[0].event).unwrap();
    assert_ne!(tidy, odd);
    assert_ne!(
        Chain::start().then(&tidy, Some(5)),
        Chain::start().then(odd, Some(5))
    );
}

#[test]
fn raw_range_is_half_open_and_clamped() {
    let log = filled(SqliteLog::open_in_memory().unwrap());
    let offsets = |start, end| -> Vec<u64> {
        log.read_raw_range(start, end)
            .unwrap()
            .iter()
            .map(|e| e.offset)
            .collect()
    };
    assert_eq!(offsets(1, 3), vec![1, 2]);
    assert_eq!(offsets(0, 0), Vec::<u64>::new());
    assert_eq!(offsets(3, 1), Vec::<u64>::new());
    assert_eq!(offsets(2, 1_000), vec![2, 3]);
    assert_eq!(offsets(4, 1_000), Vec::<u64>::new());
}

#[test]
fn an_empty_raw_batch_changes_nothing() {
    let mut log = filled(SqliteLog::open_in_memory().unwrap());
    let next = log.append_raw_batch(std::iter::empty()).unwrap();
    assert_eq!(next, 4);
    assert_eq!(log.len().unwrap(), 4);
}

#[test]
fn a_raw_copy_survives_closing_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("copy.sqlite");
    let raw = filled(InMemoryLog::new()).read_raw_from(0).unwrap();
    {
        let mut log = SqliteLog::open(&path).unwrap();
        log.append_raw_batch(raw.iter().map(|e| (e.json.as_str(), e.recorded_at)))
            .unwrap();
    }
    let log = SqliteLog::open(&path).unwrap();
    assert_eq!(log.read_raw_from(0).unwrap(), raw);
}
