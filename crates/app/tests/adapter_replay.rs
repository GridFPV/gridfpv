//! Recorded-session replay golden cases (#24).
//!
//! The end-to-end correctness guardrail for v0.2: take a **recorded source session**
//! (the adapter's own raw wire frames, as captured fixtures), run it through the
//! adapter's `translate` to canonical events, derive the lap list with the
//! projection engine, and assert it matches a checked-in **golden** result.
//!
//! This closes the loop the v1 harness (`tests/replay.rs`) opened: that one starts
//! from canonical events; this one starts one step earlier, from each source's raw
//! frames, so an adapter regression (a changed field mapping, a unit slip) is caught
//! here. Adding a source is one fixture pair plus one row in the table below.
//!
//! The fixtures are shared with the adapter crate (single source of truth). The
//! **Velocidrone** one is transcribed from real 1.17.13 wire shapes — every scalar a quoted
//! string, `"True"`/`"False"` booleans, `uid` a bare number, and whole-field snapshots
//! rather than per-crossing deltas (#494) — so this golden now pins the translation of
//! frames the game actually sends. The RotorHazard one is still synthesized from its
//! documented format; real-capture validation against dockerized RotorHazard is #25.
//!
//! Note the Velocidrone golden's shape: **Ace and Bee each complete three laps**, the third
//! ending on the `finished:"True"` crossing. Under the pre-#494 gate mapping that final
//! crossing was filed as a *split* and both pilots' last laps were missing from this list
//! entirely — which is what makes this golden worth reading rather than just regenerating.

use gridfpv_adapters::Adapter;
use gridfpv_adapters::rotorhazard::{Raw as RotorHazardRaw, RotorHazardAdapter};
use gridfpv_adapters::velocidrone::{Raw as VelocidroneRaw, VelocidroneAdapter};
use gridfpv_events::Event;
use gridfpv_projection::{LapList, lap_list};

/// Replay a recorded session through an adapter and project it to a lap list.
fn replay<A, R>(mut adapter: A, frames: Vec<R>) -> LapList
where
    A: Adapter<Raw = R>,
{
    let mut events: Vec<Event> = Vec::new();
    for frame in frames {
        events.extend(adapter.translate(frame));
    }
    lap_list(&events)
}

/// Assert a derived lap list equals its checked-in golden.
fn assert_matches_golden(got: &LapList, golden_json: &str) {
    let want: LapList = serde_json::from_str(golden_json).expect("golden fixture is valid JSON");
    assert_eq!(*got, want, "replayed lap list did not match golden");
}

#[test]
fn velocidrone_recorded_session_projects_to_golden() {
    // The fixture interleaves bare strings as commentary (what each frame group demonstrates);
    // only the objects are frames.
    let entries: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../adapters/src/velocidrone/fixtures/sprint.frames.json"
    ))
    .expect("velocidrone frames fixture is valid JSON");
    let frames: Vec<VelocidroneRaw> = entries
        .into_iter()
        .filter(|v| v.is_object())
        .map(|v| serde_json::from_value(v).expect("velocidrone fixture frame parses"))
        .collect();

    let got = replay(VelocidroneAdapter::with_default_id(), frames);
    assert_matches_golden(&got, include_str!("fixtures/velocidrone.laps.json"));
}

#[test]
fn rotorhazard_recorded_session_projects_to_golden() {
    let frames: Vec<RotorHazardRaw> = serde_json::from_str(include_str!(
        "../../adapters/src/rotorhazard/fixtures/recorded-session.json"
    ))
    .expect("rotorhazard frames fixture is valid JSON");

    let got = replay(RotorHazardAdapter::new(), frames);
    assert_matches_golden(&got, include_str!("fixtures/rotorhazard.laps.json"));
}
