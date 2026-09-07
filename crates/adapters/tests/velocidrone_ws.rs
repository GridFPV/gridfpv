//! Velocidrone WebSocket transport integration test (#26, #484).
//!
//! Proves the live WS path end to end against [`gridfpv_testkit::vd_mock`] — the
//! **wire-faithful** stand-in for the game's own server, built from the 1.17.13 decompile:
//! binary frames, the game's strict handshake, masked-only client data, the `{0x8A,0x00}`
//! ping answer, no close frames, the two-level authorization matrix, and per-uid
//! `ActivateError`.
//!
//! **This file used to prove almost nothing.** Its previous in-process mock sent *text*
//! frames and the transport decoded only text frames, so the two agreed with each other
//! and disagreed with the game, which sends every message as **binary** — the exact
//! "check that could not see the thing it was supposed to check" shape CLAUDE.md names.
//! Pointing it at `vd_mock` is what makes it a real gate: run this file against the old
//! transport and [`the_real_2025_capture_replays_into_laps`] fails with zero events.
//!
//! Gated behind the `live` feature (it needs the transport), but **not** `#[ignore]` — the
//! mock is in-process, so it needs no external service and `cargo xtask test` runs it:
//!
//! ```sh
//! cargo test -p gridfpv-adapters --features live --test velocidrone_ws
//! ```
#![cfg(feature = "live")]

use std::thread;
use std::time::{Duration, Instant};

use gridfpv_adapters::velocidrone::transport::{VdCommand, VelocidroneConnection};
use gridfpv_events::{Event, GateIndex};
use gridfpv_testkit::vd_mock::{
    Feed, MockPilot, RacePlan, VdMock, VdMockConfig, bench_roster, capture_race4,
};

/// Scripted/replayed delays are scaled hard so a three-lap race runs in well under a
/// second of wall clock. The mock applies this to every sleep in its feed.
const FAST: f64 = 0.02;

/// Drain `conn` until `done` is satisfied by everything drained so far, or `timeout`
/// elapses. Returns everything drained either way, so a failing assertion can show what
/// actually arrived.
fn drain_until(
    conn: &VelocidroneConnection,
    timeout: Duration,
    done: impl Fn(&[Event]) -> bool,
) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut got: Vec<Event> = Vec::new();
    while Instant::now() < deadline {
        got.extend(conn.events());
        if done(&got) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    got
}

/// Every [`Event::Pass`] for one competitor, in arrival order.
fn passes_for<'a>(events: &'a [Event], competitor: &str) -> Vec<&'a gridfpv_events::Pass> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Pass(p) if p.competitor.0 == competitor => Some(p),
            _ => None,
        })
        .collect()
}

fn ended(events: &[Event]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, Event::SessionEnded { .. }))
}

/// **Ground truth.** The real 2025-01-23 capture off a live build — 134 verbatim frames,
/// single pilot, three laps over 42 gates, ending in the `finished:"True"` tail — replayed
/// as the game sends them (binary frames, legacy shapes: no `uid`, countdown from 3).
///
/// This is the test that would have caught #494 twice over: the text-only reader saw none
/// of it, and the numeric `PilotData` could not have parsed a frame of it.
#[test]
fn the_real_2025_capture_replays_into_laps() {
    let mock = VdMock::start(VdMockConfig {
        feed: Feed::Replay(capture_race4()),
        autostart: true,
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let events = drain_until(&conn, Duration::from_secs(20), ended);
    let readback = conn.readback();
    conn.disconnect();

    assert!(
        matches!(events.first(), Some(Event::SessionStarted { .. })),
        "the capture opens with racestatus:start, got {:?}",
        events.first()
    );
    assert!(ended(&events), "the capture ends with `race finished`");

    // The capture is single-pilot: "Dacus", 43 crossings on the final lap.
    let dacus = passes_for(&events, "Dacus");
    assert!(
        dacus.len() > 100,
        "the capture carries 3 laps of ~42 gates; got {} passes",
        dacus.len()
    );

    // Three lap-gate crossings open laps 1-3, and the `finished:\"True\"` tail is the
    // fourth — the crossing the old gate mapping filed as split 42 and lost.
    let lap_gates = dacus.iter().filter(|p| p.gate == GateIndex::LAP).count();
    assert_eq!(
        lap_gates, 4,
        "3 lap starts + the finishing crossing must all be LAP gates"
    );

    assert_eq!(
        readback.malformed_frames, 0,
        "every frame in a real capture must decode"
    );
    assert_eq!(readback.last_race_action.as_deref(), Some("race finished"));
}

/// The current-build path: seat a heat by uid, start it, ingest laps, finish.
#[test]
fn a_1_17_13_heat_seats_starts_and_finishes() {
    let roster = bench_roster();
    let mock = VdMock::start(VdMockConfig {
        pilots: roster.clone(),
        feed: Feed::Scripted(RacePlan::default()),
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");

    // Seat the first two pilots. `activate` has no positive ack, so the outcome is built
    // from the absence of `ActivateError` plus the `getpilots` roster.
    let uids: Vec<String> = roster.iter().take(2).map(|p| p.uid.to_string()).collect();
    let outcome = conn.seat(&uids);

    assert!(
        outcome.is_clean(),
        "seating two present pilots must raise no complaint: {outcome:?}"
    );
    assert_eq!(outcome.seated, uids);
    let names: Vec<String> = outcome
        .roster
        .as_ref()
        .expect("getpilots must answer with the roster")
        .iter()
        .map(|p| p.name.clone())
        .collect();
    assert_eq!(names, vec!["Ace", "Bee", "Cyn", "Dex"]);

    // The sim's own state is the real readback: exactly the seated pair is flying.
    assert_eq!(
        mock.flying(),
        vec![true, true, false, false],
        "allspectate then activate must leave exactly the heat in the air"
    );
    let sent: Vec<String> = mock.commands().iter().map(|c| c.command.clone()).collect();
    assert!(sent.contains(&"allspectate".to_string()));
    assert!(sent.contains(&"activate".to_string()));
    // #524: seating also closes the room, so nobody wanders in and changes the field under a heat
    // that is already seated.
    assert!(
        sent.contains(&"lock".to_string()),
        "seating must lock the lobby: {sent:?}"
    );
    // …and in that order — locking before the field is set would be pointless.
    let lock_at = sent.iter().position(|c| c == "lock").expect("a lock");
    let activate_at = sent
        .iter()
        .position(|c| c == "activate")
        .expect("an activate");
    assert!(activate_at < lock_at, "activate then lock, not the reverse");

    conn.send(VdCommand::StartRace);
    let events = drain_until(&conn, Duration::from_secs(20), ended);
    let readback = conn.readback();
    conn.disconnect();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::SessionStarted { .. })),
        "startrace must produce racestatus:start"
    );
    assert!(ended(&events), "the scripted heat runs to `race finished`");
    assert_eq!(
        readback.race_laps,
        Some(3),
        "racetype carries the lap count"
    );
    assert!(
        !passes_for(&events, "Ace").is_empty(),
        "the seated pilots must produce laps"
    );
    assert_eq!(readback.malformed_frames, 0);
}

/// The seating readback that matters: a uid that is not in the room draws one
/// `ActivateError` per missing uid, and that is the only signal the write went wrong.
#[test]
fn a_uid_that_is_not_in_the_room_is_reported_by_the_readback() {
    let roster = bench_roster();
    let mock = VdMock::start(VdMockConfig {
        pilots: roster.clone(),
        feed: Feed::Scripted(RacePlan::default()),
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let present = roster[0].uid.to_string();
    let outcome = conn.seat(&[present.clone(), "99999".into(), "not-a-uid".into()]);
    conn.disconnect();

    assert_eq!(outcome.seated, vec![present]);
    assert_eq!(
        outcome.not_found,
        vec!["99999"],
        "the sim complains once per uid it cannot seat"
    );
    // A uid that is not an integer is omitted from the write rather than guessed — the
    // game would silently skip it, and a guess seats the wrong pilot.
    assert_eq!(outcome.unusable, vec!["not-a-uid"]);
    assert!(!outcome.is_clean());
}

/// **The host-gating trap.** In a non-host room every control and roster command is
/// dropped *silently* — no error frame, nothing. So the seating "succeeds" by the absence
/// of complaints, and the only thing that betrays it is the roster never arriving. The
/// Director must treat an unconfirmed seating as a failure, not a success.
#[test]
fn a_non_host_room_drops_the_seating_write_and_the_roster_never_confirms() {
    let roster = bench_roster();
    let mock = VdMock::start(VdMockConfig {
        pilots: roster.clone(),
        feed: Feed::Scripted(RacePlan::default()),
        host: false,
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let outcome = conn.seat(&[roster[0].uid.to_string()]);
    conn.disconnect();

    assert!(
        outcome.not_found.is_empty(),
        "a dropped command draws no complaint — that is the trap"
    );
    assert!(
        outcome.roster.is_none(),
        "getpilots is host-gated, so no roster comes back: the seating is UNCONFIRMED"
    );
    assert!(
        mock.flying().iter().all(|f| *f),
        "nothing was actually seated"
    );
    assert!(
        mock.commands().iter().all(|c| !c.authorized),
        "every command must have been dropped by the gate"
    );
}

/// Unknown frames, a genuinely malformed frame, and a player name carrying an unescaped
/// quote (the game's serializer never escapes strings) must all be survivable: the
/// malformed one is *counted* — never silent — and the race still lands.
#[test]
fn junk_frames_are_counted_but_do_not_stop_the_feed() {
    let mock = VdMock::start(VdMockConfig {
        pilots: bench_roster(),
        feed: Feed::Scripted(RacePlan {
            junk: true,
            ..RacePlan::default()
        }),
        autostart: true,
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let events = drain_until(&conn, Duration::from_secs(20), ended);
    let readback = conn.readback();
    conn.disconnect();

    assert!(ended(&events), "the race still finishes through the junk");
    assert!(
        !passes_for(&events, "Ace").is_empty(),
        "laps still arrive through the junk"
    );
    assert!(
        readback.malformed_frames > 0,
        "the malformed frame must be COUNTED — a silent drop is the #494 bug"
    );
}

/// The 60 Hz `imu` feed must be tolerated without being charged as drift and without
/// disturbing the lap stream.
#[test]
fn the_imu_feed_is_tolerated_and_costs_nothing() {
    let mock = VdMock::start(VdMockConfig {
        pilots: bench_roster().into_iter().take(1).collect(),
        feed: Feed::Scripted(RacePlan {
            imu: true,
            ..RacePlan::default()
        }),
        autostart: true,
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let events = drain_until(&conn, Duration::from_secs(20), ended);
    let readback = conn.readback();
    conn.disconnect();

    assert!(ended(&events));
    assert_eq!(
        readback.malformed_frames, 0,
        "imu frames are known, not drift"
    );
    assert!(!passes_for(&events, "Ace").is_empty());
}

/// A pre-tournament build sends no `uid` in `racedata` at all — the player is identified
/// only by the map key. Attribution is by name, so that build still times a race.
#[test]
fn a_legacy_build_without_uids_still_times_a_race() {
    let mock = VdMock::start(VdMockConfig {
        pilots: bench_roster(),
        feed: Feed::Scripted(RacePlan {
            uids_on_wire: false,
            countdown_from: 3,
            ..RacePlan::default()
        }),
        autostart: true,
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let events = drain_until(&conn, Duration::from_secs(20), ended);
    let readback = conn.readback();
    conn.disconnect();

    assert!(ended(&events));
    assert!(!passes_for(&events, "Ace").is_empty());
    assert_eq!(readback.malformed_frames, 0);
}

/// An aborted race must close the session. `"abort"` is the literal in the binary;
/// matching only the community spellings (`"aborted"`, `"race aborted"`) would leave the
/// session open forever.
#[test]
fn an_aborted_race_closes_the_session() {
    let mock = VdMock::start(VdMockConfig {
        pilots: bench_roster(),
        feed: Feed::Scripted(RacePlan {
            abort_after_go_ms: Some(4_000),
            ..RacePlan::default()
        }),
        autostart: true,
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    let events = drain_until(&conn, Duration::from_secs(20), ended);
    let readback = conn.readback();
    conn.disconnect();

    assert!(ended(&events), "an abort must end the session");
    assert_eq!(readback.last_race_action.as_deref(), Some("abort"));
}

/// The keep-alive is the vendor's `{"command":"ping"}`, on the ~5 s cadence the sliding
/// 40 s idle timeout wants. The empty frame we used to send is tolerated but undocumented;
/// any *other* non-empty frame is what historically spammed the game's log and stuttered
/// the sim.
#[test]
fn the_keep_alive_reaches_the_sim_as_the_ping_command() {
    let mock = VdMock::start(VdMockConfig {
        pilots: vec![MockPilot {
            name: "Ace".into(),
            uid: 41231,
            colour: "FF0000".into(),
            flying: true,
            gate_ms: 900,
        }],
        feed: Feed::Scripted(RacePlan::default()),
        time_scale: FAST,
        ..VdMockConfig::default()
    });

    let conn = VelocidroneConnection::connect(&mock.url()).expect("connect to vd-mock");
    // The cadence is 5 s; wait past one tick.
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if mock.commands().iter().any(|c| c.command == "ping") {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    conn.disconnect();

    let pings: Vec<_> = mock
        .commands()
        .into_iter()
        .filter(|c| c.command == "ping")
        .collect();
    assert!(
        !pings.is_empty(),
        "the transport must keep the connection alive with the ping command"
    );
    assert_eq!(pings[0].raw, r#"{"command":"ping"}"#);
}
