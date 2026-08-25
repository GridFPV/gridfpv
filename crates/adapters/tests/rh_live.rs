//! Live integration test against dockerized RotorHazard (#25).
//!
//! Drives a real race on a running RotorHazard (the mock-node container in
//! `docker/rotorhazard/`) over Socket.IO and asserts the adapter translates the
//! live stream into correct laps — the "real timing in" half of v0.2's done-when.
//!
//! Gated behind the `live` feature AND `#[ignore]`, so it never runs in the
//! default `cargo test` or the shared CI pipeline. It's a **local** class that
//! manages its own disposable RotorHazard container (Docker required). Run via:
//!
//! ```sh
//! cargo xtask live
//! # or just this test:
//! cargo test -p gridfpv-adapters --features live --test rh_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::Event;
use gridfpv_projection::lap_list;
use gridfpv_testkit::RhContainer;

/// Port for this test's disposable RotorHazard (distinct from rh_signal's).
const PORT: u16 = 5030;

/// The in-repo GridFPV plugin directory, mounted explicitly by the owned-format test.
///
/// That test is about what the **plugin** guarantees, so it must not depend on which leg of the
/// version × plugin matrix is running (`RhContainer::start` mounts a plugin only when the
/// harness's env var points at one). Every other test here uses `start`, unchanged.
fn plugin_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/gridfpv")
        .canonicalize()
        .expect("the in-repo plugins/gridfpv directory exists")
}

fn event_kind(e: &Event) -> &'static str {
    match e {
        Event::AdapterConnected { .. } => "AdapterConnected",
        Event::AdapterDisconnected { .. } => "AdapterDisconnected",
        Event::SessionStarted { .. } => "SessionStarted",
        Event::SessionEnded { .. } => "SessionEnded",
        Event::CompetitorSeen { .. } => "CompetitorSeen",
        Event::CompetitorRegistered { .. } => "CompetitorRegistered",
        Event::Pass(_) => "Pass",
        Event::SignalChunk(_) => "SignalChunk",
        Event::SignalThresholds(_) => "SignalThresholds",
        Event::SignalHistory(_) => "SignalHistory",
        Event::HeatScheduled { .. } => "HeatScheduled",
        Event::HeatStateChanged { .. } => "HeatStateChanged",
        Event::CurrentHeatSelected { .. } => "CurrentHeatSelected",
        Event::HeatStarting { .. } => "HeatStarting",
        Event::HeatFinalizing { .. } => "HeatFinalizing",
        Event::DetectionVoided { .. } => "DetectionVoided",
        Event::LapInserted { .. } => "LapInserted",
        Event::LapAdjusted { .. } => "LapAdjusted",
        Event::LapSplit { .. } => "LapSplit",
        Event::LapThrownOut { .. } => "LapThrownOut",
        Event::HeatVoided { .. } => "HeatVoided",
        Event::PenaltyApplied { .. } => "PenaltyApplied",
        Event::ProtestFiled { .. } => "ProtestFiled",
        Event::ProtestResolved { .. } => "ProtestResolved",
        Event::RulingReversed { .. } => "RulingReversed",
        Event::RoundFieldDrawn { .. } => "RoundFieldDrawn",
    }
}

/// Drain events until `pred` is satisfied or `timeout` elapses; returns whether it
/// was satisfied. Accumulates everything drained into `sink`.
fn wait_until(
    conn: &RotorHazardConnection,
    sink: &mut Vec<Event>,
    timeout: Duration,
    pred: impl Fn(&[Event]) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        sink.extend(conn.events());
        if pred(sink) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "requires a running dockerized RotorHazard (docker/rotorhazard/)"]
fn live_rotorhazard_race_translates_to_laps() {
    // Disposable RotorHazard with mock nodes (no CSVs); driven via `simulate_lap`.
    let rh = RhContainer::start(PORT, "0.5", &[]);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();

    // Let the connection settle and the server register us before driving it.
    std::thread::sleep(Duration::from_secs(2));

    // Disable RH's lap minimum for the sim: our rapid `simulate_lap` injections are far closer
    // together than RH's 10s default `MIN_LAP_TIME`, which otherwise logs "Pass record under lap
    // minimum (10)" for every short lap. `0` lets every sim lap record cleanly.
    conn.set_min_lap_time(0).ok();

    // Reset to a clean READY state (a prior DONE race blocks staging), then ignore
    // any events that reset produced — we only assert on the fresh race below.
    conn.stop_race().ok();
    conn.discard_laps().expect("emit discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    // Stage the race and wait for it to actually start (RACING -> SessionStarted).
    conn.stage_race().expect("emit stage_race");
    let started = wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
        evs.iter()
            .any(|e| matches!(e, Event::SessionStarted { .. }))
    });
    if !started {
        eprintln!(
            "debug: {} events so far: {:?}",
            events.len(),
            events.iter().map(event_kind).collect::<Vec<_>>()
        );
    }
    assert!(started, "race never reached RACING (no SessionStarted)");

    // Three crossings on node 0 (=> 2 laps) and one on node 1 (=> 0 laps).
    for node in [0u64, 0, 1, 0] {
        conn.simulate_lap(node).expect("emit simulate_lap");
        std::thread::sleep(Duration::from_millis(1200));
        events.extend(conn.events());
    }

    // Wait until node-0 has accumulated at least two passes (one full lap).
    let got_laps = wait_until(&conn, &mut events, Duration::from_secs(10), |evs| {
        lap_list(evs)
            .competitors
            .iter()
            .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= 1)
    });

    conn.stop_race().expect("emit stop_race");
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());

    // Second heat on the SAME persistent connection/adapter (#105 cross-heat regression).
    // RotorHazard resets lap_number to 0 at each race start; without resetting the per-lap
    // dedup on the RACING transition, heat 2's laps would collide with heat 1's and be
    // suppressed (zero laps ingested past the first heat). Reset, re-stage, and assert
    // heat 2 produces its own fresh laps.
    conn.discard_laps().expect("emit discard_laps (heat 2)");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();
    let mut heat2: Vec<Event> = Vec::new();

    conn.stage_race().expect("emit stage_race (heat 2)");
    let started2 = wait_until(&conn, &mut heat2, Duration::from_secs(20), |evs| {
        evs.iter()
            .any(|e| matches!(e, Event::SessionStarted { .. }))
    });
    assert!(started2, "heat 2 never reached RACING (no SessionStarted)");

    for node in [0u64, 0, 1, 0] {
        conn.simulate_lap(node).expect("emit simulate_lap (heat 2)");
        std::thread::sleep(Duration::from_millis(1200));
        heat2.extend(conn.events());
    }
    let got_laps2 = wait_until(&conn, &mut heat2, Duration::from_secs(10), |evs| {
        lap_list(evs)
            .competitors
            .iter()
            .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= 1)
    });

    conn.stop_race().expect("emit stop_race (heat 2)");
    std::thread::sleep(Duration::from_millis(800));
    heat2.extend(conn.events());
    conn.disconnect();

    assert!(
        got_laps2,
        "heat 2 ingested no completed lap for node-0 \
         (cross-heat dedup regression: lap_number reset collided with heat 1)"
    );
    let laps2 = lap_list(&heat2);
    let node0_h2 = laps2
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .expect("node-0 present in heat 2 lap list");
    assert!(
        node0_h2.lap_count() >= 1,
        "expected >= 1 lap for node-0 in heat 2, got {}",
        node0_h2.lap_count()
    );

    // The live stream produced lifecycle + competitor sightings + real laps.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::SessionStarted { .. })),
        "expected a SessionStarted"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CompetitorSeen { .. })),
        "expected at least one CompetitorSeen"
    );
    assert!(got_laps, "node-0 never produced a completed lap");

    let laps = lap_list(&events);
    let node0 = laps
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .expect("node-0 present in lap list");
    assert!(
        node0.lap_count() >= 1,
        "expected >= 1 lap for node-0, got {}",
        node0.lap_count()
    );
    assert!(
        node0.laps.iter().all(|l| l.duration_micros > 0),
        "lap durations must be positive (source-clock derived)"
    );
    println!(
        "live RH: node-0 completed {} lap(s): {:?}",
        node0.lap_count(),
        node0.laps
    );
}

/// Mid-race reconnect must not double-count (#105). Drives a real race, accumulates laps, then
/// **disconnects mid-race and reconnects reusing the same adapter** (the persisted-adapter fix:
/// `disconnect` returns the adapter, the next `connect` takes it). RotorHazard re-sends the full
/// in-progress `current_laps` snapshot on the new socket; because the carried adapter's dedup
/// already holds those laps, the lap count after the reconnect must equal the count before it (no
/// duplicates). Pre-fix, a fresh adapter on reconnect re-emitted every in-progress lap and the lap
/// projection (no sequence dedup) turned them into duplicate laps.
#[test]
#[ignore = "requires a running dockerized RotorHazard (docker/rotorhazard/)"]
fn mid_race_reconnect_does_not_double_count_laps() {
    let rh = RhContainer::start(PORT + 1, "0.5", &[]);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    let mut events: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(2));

    // Disable RH's lap minimum for the sim (see the first test) so the short `simulate_lap`
    // crossings below don't trip "Pass record under lap minimum (10)".
    conn.set_min_lap_time(0).ok();

    conn.stop_race().ok();
    conn.discard_laps().expect("emit discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    conn.stage_race().expect("emit stage_race");
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "race never reached RACING (no SessionStarted)"
    );

    // Drive several crossings on node 0 so there are in-progress laps to replay.
    for node in [0u64, 0, 0, 0] {
        conn.simulate_lap(node).expect("emit simulate_lap");
        std::thread::sleep(Duration::from_millis(1200));
        events.extend(conn.events());
    }
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(10), |evs| {
            lap_list(evs)
                .competitors
                .iter()
                .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= 1)
        }),
        "node-0 never produced a completed lap before the reconnect"
    );
    let laps_before = lap_list(&events)
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .map(|c| c.lap_count())
        .unwrap_or(0);
    assert!(laps_before >= 1, "need at least one lap before reconnect");

    // Simulate a mid-race drop+reconnect: recover the adapter from `disconnect` and feed it into a
    // fresh socket. The race keeps RUNNING server-side, so RH replays the in-progress snapshot.
    let adapter = conn.disconnect();
    std::thread::sleep(Duration::from_millis(500));
    let conn = RotorHazardConnection::connect(rh.url(), adapter)
        .expect("reconnect to RotorHazard reusing the persisted adapter");

    // Drain the replayed snapshot the reconnect triggers; give it time to arrive.
    let mut after: Vec<Event> = Vec::new();
    wait_until(&conn, &mut after, Duration::from_secs(5), |_| false);
    events.extend(after);

    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());
    conn.disconnect();

    let laps_after = lap_list(&events)
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .map(|c| c.lap_count())
        .unwrap_or(0);
    assert_eq!(
        laps_after, laps_before,
        "mid-race reconnect double-counted laps: {laps_before} before, {laps_after} after \
         (the replayed in-progress snapshot was not deduped — the persisted-adapter fix regressed)"
    );
    println!("live RH reconnect: node-0 stable at {laps_after} lap(s) across the reconnect");
}

/// **A heat run past RotorHazard's configured lap limit must still deliver every crossing**
/// (#403, #404, #405).
///
/// The field case, 2026-08-25: a pilot flew 8 gate crossings in an open-practice heat and Grid
/// showed 4 laps. RotorHazard had detected all 8 — but it declared a winner at lap 3, numbered
/// every later crossing `-1`, and marked them deleted at source (`RHRace.py`:
/// `lap_data.deleted = lap_late_flag`). Grid correctly skips deleted laps, so four crossings the
/// timer read perfectly were gone before Grid could see them. The cause was that Grid neutralised
/// RotorHazard's *staging* fields and nothing about its stopping or counting.
///
/// The rest of the live matrix cannot catch this: its heats are a handful of laps long, far too
/// short to reach any default win condition. So this test **arms the trap first** — it gives the
/// timer's own race format `FIRST_TO_LAP_X` at 3 laps, exactly the configuration that bit — and
/// then runs a heat well past it. What must happen:
///
///   * Grid races on the plugin's **Grid-owned** `GridFPV` format, not the sabotaged one, and
///     that is *confirmed* rather than assumed;
///   * the timer's own format is a different row, so it was never mutated to get there;
///   * all [`CROSSINGS`] crossings arrive, i.e. `CROSSINGS - 1` completed laps (holeshot first).
///
/// Run it against both supported RotorHazard versions — the conduct columns are the same on 4.3.0
/// and 4.4.0, but `unlimited_time` (DB column `race_mode`) is itself a rename, so "the field names
/// carry" is a thing to check, not assume:
///
/// ```sh
/// cargo xtask live --rh 4.3.0 --plugin --full
/// cargo xtask live --rh 4.4.0 --plugin --full
/// ```
#[test]
#[ignore = "requires a running dockerized RotorHazard (docker/rotorhazard/)"]
fn heat_past_rh_lap_limit_still_delivers_every_crossing() {
    /// Crossings to fly — comfortably past the 3-lap cap armed below, and the field's own count.
    const CROSSINGS: usize = 8;
    /// The lap cap given to the timer's own format: RotorHazard's `WinCondition.FIRST_TO_LAP_X`.
    const RH_WIN_CONDITION_FIRST_TO_LAP_X: i64 = 2;
    const RH_LAP_CAP: i64 = 3;

    let rh = RhContainer::start_with_plugin(PORT + 2, "0.5", &[], Some(plugin_dir()));
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    // The plugin must be there: this test is about the guarantee it provides. A missing handshake
    // means the mount failed, not that the guarantee is optional.
    let hello = conn.wait_for_plugin(Duration::from_secs(20)).expect(
        "the GridFPV plugin answered gridfpv_hello (it is mounted read-only from the repo)",
    );
    assert!(
        hello.capabilities.iter().any(|c| c == "owned_format"),
        "the plugin must advertise `owned_format`; it advertised {:?}",
        hello.capabilities
    );

    let mut events: Vec<Event> = Vec::new();
    std::thread::sleep(Duration::from_secs(2));

    // Short sim laps: RH's 10s default MIN_LAP_TIME would otherwise reject them (see the tests
    // above). Orthogonal to the win condition under test.
    conn.set_min_lap_time(0).ok();

    // Clean READY state, then forget the churn.
    conn.stop_race().ok();
    conn.discard_laps().expect("emit discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events();

    // ---- arm the trap on the TIMER'S OWN format --------------------------------------------
    let rd_format = conn
        .current_format_id()
        .expect("RotorHazard reported its current race format id in race_status");
    conn.set_race_format_win_condition(rd_format, RH_WIN_CONDITION_FIRST_TO_LAP_X, RH_LAP_CAP)
        .expect("give the timer's own format a 3-lap win condition");
    std::thread::sleep(Duration::from_secs(1));
    let _ = conn.events();

    // ---- prepare exactly as the Director's staging loop does --------------------------------
    conn.prepare_instant_start().expect("prepare_instant_start");
    assert!(
        conn.owned_format_selected(),
        "the Grid-owned race format was not confirmed selected — Grid would have raced the \
         timer's own format, whose {RH_LAP_CAP}-lap win condition truncates the heat (#403)"
    );
    let grid_format = conn
        .owned_format_id()
        .expect("the plugin named its GridFPV race format id");
    assert_ne!(
        grid_format, rd_format,
        "the Grid-owned format must be its own row — racing (and neutralising) the timer's own \
         format row is what #404 set out to stop"
    );

    // Seat a pilot on node 0 and make that heat current, exactly as the Director does at Stage.
    // Selecting a heat can switch RotorHazard's effective race format, so this is where a
    // selection that only *looked* durable would come undone.
    let seated = conn
        .seat_heat(&[(0, "LAPCAP".to_string())])
        .expect("seat node 0");
    assert!(seated.is_some(), "seating did not produce a heat to race");
    let _ = conn.events();

    // The driver re-prepares immediately before staging, for exactly the reason above.
    conn.prepare_instant_start()
        .expect("prepare_instant_start (pre-stage)");
    assert!(
        conn.owned_format_selected(),
        "selecting a heat dislodged the Grid-owned race format"
    );

    // ---- fly the heat, well past the cap ----------------------------------------------------
    conn.stage_race().expect("emit stage_race");
    assert!(
        wait_until(&conn, &mut events, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "race never reached RACING (no SessionStarted)"
    );

    for _ in 0..CROSSINGS {
        conn.simulate_lap(0).expect("emit simulate_lap");
        std::thread::sleep(Duration::from_millis(1200));
        events.extend(conn.events());
    }
    // Holeshot-first numbering: N crossings are N-1 completed laps.
    let wanted = CROSSINGS - 1;
    let arrived = wait_until(&conn, &mut events, Duration::from_secs(15), |evs| {
        lap_list(evs)
            .competitors
            .iter()
            .any(|c| c.competitor.competitor.0 == "node-0" && c.lap_count() >= wanted)
    });

    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    events.extend(conn.events());
    conn.disconnect();

    let laps = lap_list(&events);
    let got = laps
        .competitors
        .iter()
        .find(|c| c.competitor.competitor.0 == "node-0")
        .map(|c| c.lap_count())
        .unwrap_or(0);
    assert!(
        arrived && got >= wanted,
        "RotorHazard's own {RH_LAP_CAP}-lap win condition truncated the heat: flew {CROSSINGS} \
         crossings, Grid recorded {got} lap(s), expected {wanted}. RotorHazard declared a winner \
         and deleted the later crossings at source — Grid is not racing on a neutralised race \
         format (#403)"
    );
    println!(
        "live RH: {CROSSINGS} crossings past a {RH_LAP_CAP}-lap win condition -> {got} laps, on \
         the Grid-owned race format ({grid_format}); the timer's own format ({rd_format}) was \
         never touched"
    );
}
