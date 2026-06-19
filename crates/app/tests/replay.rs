//! Replay-harness scaffold (#10).
//!
//! The correctness discipline that grows from v0.2: feed a **recorded session**
//! (a checked-in fixture of canonical events) through the projection engine and
//! assert the derived lap list matches a checked-in **golden** result. Because
//! projections are a pure fold over the log, a recorded session always replays to
//! the same lap list — so any change that alters derivation is caught here.
//!
//! `replay()` is the reusable harness; each `#[test]` is one golden case. Adding a
//! case is two fixture files plus a three-line test.

use gridfpv_events::Event;
use gridfpv_projection::{LapList, lap_list};

/// Replay a recorded session (JSON array of `Event`) into a lap list.
fn replay(recorded_session_json: &str) -> LapList {
    let events: Vec<Event> = serde_json::from_str(recorded_session_json)
        .expect("recorded session fixture is valid JSON");
    lap_list(&events)
}

/// Assert that a recorded session replays to its golden lap list.
fn assert_replays_to_golden(session_json: &str, golden_json: &str) {
    let got = replay(session_json);
    let want: LapList = serde_json::from_str(golden_json).expect("golden fixture is valid JSON");
    assert_eq!(got, want, "replayed lap list did not match golden");
}

#[test]
fn sprint_session_matches_golden() {
    assert_replays_to_golden(
        include_str!("fixtures/sprint.events.json"),
        include_str!("fixtures/sprint.laps.json"),
    );
}

/// The fold is deterministic and order-independent: shuffling the recorded events
/// yields the identical lap list (passes are grouped and ordered by the engine).
#[test]
fn replay_is_recomputable_regardless_of_event_order() {
    let session = include_str!("fixtures/sprint.events.json");
    let mut events: Vec<Event> = serde_json::from_str(session).unwrap();
    let forward = lap_list(&events);
    events.reverse();
    let reversed = lap_list(&events);
    assert_eq!(forward, reversed);
}
