//! Shared mock-RH end-to-end harness for the engine's `live` tests (#29, #38).
//!
//! [`run_mock_heat`] drives a **real dockerized RotorHazard** through one full heat
//! and returns the merged canonical event log — the heat's [`Event::HeatScheduled`],
//! the [`Event::HeatStateChanged`] transitions the FSM records on the forward path,
//! and the [`Event::Pass`]es the timer produced while the heat was `Running`,
//! interleaved in append order.
//!
//! This is the §5.1 "mock-RH e2e" harness from `docs/testing-strategy.html`, promoted
//! to a reusable helper so every engine feature gets a real end-to-end test: a
//! downstream test calls [`run_mock_heat`], then folds the returned log with its own
//! feature (scoring #30, marshaling #31, …). The helper owns the *plumbing* — spin up
//! RH, connect, reset, drive the heat loop, collect passes, tear down — and stays
//! deliberately structural: the mock interface reads its CSV continuously (decoupled
//! from race start), so lap *timing* is not controllable and callers assert structure
//! (states reached, transition order, passes only while live), never exact µs.
//!
//! RAII: the [`RhContainer`] is dropped at the end of [`run_mock_heat`], removing the
//! container before the function returns.
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_engine::heat::{HeatCommand, HeatState, apply, next_state};
use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};
use gridfpv_testkit::RhContainer;

/// Default port for the engine's mock-RH e2e harness.
///
/// Distinct from the adapters' own live tests (`rh_live` 5030, `rh_signal` 5031) so
/// the engine's e2e can in principle run alongside them without a port clash; the
/// targets are still run sequentially by `cargo xtask live` so at most one container
/// exists at a time.
///
/// `allow(dead_code)`: this shared `common` module is compiled into every engine
/// `live` test binary, but each test uses only the port it needs (the scoring e2e
/// runs on its own distinct port), so an individual binary may not reference this.
#[allow(dead_code)]
pub const DEFAULT_PORT: u16 = 5032;

/// The container's CSV tick interval (seconds), matching the adapters' signal test.
const TICK: &str = "0.1";

/// Record one FSM command as an `Event::HeatStateChanged`, advancing `state` and
/// appending the transition to `log`. Panics if the command is illegal in `state`
/// (the harness only drives the legal forward path, so a rejection is a bug).
fn drive(
    log: &mut Vec<Event>,
    heat: &HeatId,
    state: &mut HeatState,
    command: HeatCommand,
) -> HeatTransition {
    let transition =
        apply(*state, command).unwrap_or_else(|e| panic!("FSM rejected {command:?}: {e}"));
    *state = next_state(*state, transition);
    log.push(Event::HeatStateChanged {
        heat: heat.clone(),
        transition,
    });
    transition
}

/// Poll `conn` until `pred` holds over the accumulated `sink`, or `timeout` elapses.
/// Returns whether the predicate was satisfied. Drained events are appended to `sink`
/// so nothing is lost between polls.
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

/// Drive a full mock heat end to end and return the merged canonical event log.
///
/// `port` is the host port for the disposable RotorHazard (use [`DEFAULT_PORT`] unless
/// running several harnesses at once). `heat` names the heat in the log. `scenario` is
/// the per-node CSV set as `(node_index, csv)` pairs — typically built with
/// [`gridfpv_testkit::node_csv`]; each `node_index` becomes a lineup entry
/// `CompetitorRef("node-{node_index}")` (the adapter's seat ref).
///
/// The returned [`Vec<Event>`] is, in append order:
/// 1. one [`Event::HeatScheduled { heat, lineup }`] (lineup = one
///    `CompetitorRef("node-{i}")` per scenario node index, in scenario order),
/// 2. the forward-path [`Event::HeatStateChanged`] transitions the FSM records —
///    `Staged`, `Armed`, then (after RH actually starts) `Running`, then `Finished`
///    and `Finalized`,
/// 3. interleaved between the `Running` and `Finished` transitions, the
///    [`Event::Pass`]es the timer produced while the heat was live (other adapter
///    events such as lifecycle/`CompetitorSeen` are dropped — only `Pass`es are kept).
///
/// Folding the result with [`gridfpv_engine::heat::heat_state`] yields
/// [`HeatState::Final`]; the passes all fall in the live window, so cross-checking
/// each pass's surrounding state with [`gridfpv_engine::heat::consumes_pass`] holds.
///
/// Timing is tolerant: it waits for RH to reach `RACING` and for at least one pass,
/// but never asserts exact lap times. The [`RhContainer`] is torn down on return.
///
/// `allow(dead_code)`: `common` is compiled into every engine `live` test binary, and the
/// marshaling-evidence one uses [`run_mock_heat_with_signal`] instead.
#[allow(dead_code)]
pub fn run_mock_heat(port: u16, heat: &str, scenario: &[(usize, String)]) -> Vec<Event> {
    run_mock_heat_keeping(port, heat, scenario, |e| matches!(e, Event::Pass(_)), 1)
}

/// [`run_mock_heat`], but the race is held open until the timer has produced at least
/// `min_passes` crossings (rather than stopping on the very first one).
///
/// Why this exists: stopping on the first pass makes the heat's pass count a **race** between
/// the CSV cadence and the harness's poll/drain windows — the same scenario yields 1 pass on one
/// run and 5 on the next. Any assertion whose shape depends on "did we get 1 or ≥2 detections?"
/// then passes or fails by luck. Waiting for a stated `min_passes` at a lap cadence slower than
/// the ~1s stop-and-drain window makes the count reproducible: the race is closed within one
/// poll tick (250ms) of the `min_passes`-th crossing, and the next crossing is a full lap away,
/// so the drain adds none.
///
/// `allow(dead_code)`: `common` is compiled into every engine `live` test binary and most use
/// the plain [`run_mock_heat`].
#[allow(dead_code)]
pub fn run_mock_heat_until(
    port: u16,
    heat: &str,
    scenario: &[(usize, String)],
    min_passes: usize,
) -> Vec<Event> {
    run_mock_heat_keeping(
        port,
        heat,
        scenario,
        |e| matches!(e, Event::Pass(_)),
        min_passes,
    )
}

/// [`run_mock_heat`], but the heat's **signal facts ride along** with its passes.
///
/// The default harness keeps only `Pass`es — adapter bookkeeping is not part of the heat's
/// canonical race-engine log. The marshaling surfaces need more than that: the RSSI trace is the
/// *evidence* an RD reconstructs a mis-detected race from (#388), and it flows per seated node
/// whether or not that node ever produced a crossing. This variant keeps
/// [`Event::SignalChunk`] / [`Event::SignalThresholds`] / [`Event::SignalHistory`] and
/// [`Event::CompetitorSeen`] alongside the passes, so a test can fold the same window the
/// server's marshaling projections do.
///
/// `allow(dead_code)`: `common` is compiled into every engine `live` test binary and most use
/// only [`run_mock_heat`].
#[allow(dead_code)]
pub fn run_mock_heat_with_signal(
    port: u16,
    heat: &str,
    scenario: &[(usize, String)],
) -> Vec<Event> {
    run_mock_heat_keeping(
        port,
        heat,
        scenario,
        |e| {
            matches!(
                e,
                Event::Pass(_)
                    | Event::SignalChunk(_)
                    | Event::SignalThresholds(_)
                    | Event::SignalHistory(_)
                    | Event::CompetitorSeen { .. }
            )
        },
        1,
    )
}

/// The shared harness behind [`run_mock_heat`] and [`run_mock_heat_with_signal`]: `keep` decides
/// which of the adapter's live events are folded into the returned canonical log, and
/// `min_passes` is how many crossings the race is held open for (see [`run_mock_heat_until`]).
fn run_mock_heat_keeping(
    port: u16,
    heat: &str,
    scenario: &[(usize, String)],
    keep: impl Fn(&Event) -> bool,
    min_passes: usize,
) -> Vec<Event> {
    let heat = HeatId(heat.to_string());
    let lineup: Vec<CompetitorRef> = scenario
        .iter()
        .map(|(node_index, _)| CompetitorRef(format!("node-{node_index}")))
        .collect();

    // RAII: dropped at the end of this function, removing the container.
    let rh = RhContainer::start(port, TICK, scenario);
    let conn = RotorHazardConnection::connect(rh.url(), RotorHazardAdapter::new())
        .expect("connect to RotorHazard");

    // The canonical log we build up and return. Seed it with the heat's creation.
    let mut log: Vec<Event> = vec![Event::HeatScheduled {
        heat: heat.clone(),
        lineup,
        class: None,
        round: None,
        frequencies: vec![],
        label: None,
    }];
    // The heat's current FSM state, validated by `apply` and advanced by `next_state`.
    let mut state = HeatState::Scheduled;

    // Settle, then reset RH to a clean READY state so staging starts from a known
    // place regardless of any prior container state (mirrors the adapters' pattern).
    std::thread::sleep(Duration::from_secs(2));
    conn.stop_race().ok();
    conn.discard_laps().expect("discard_laps");
    std::thread::sleep(Duration::from_secs(2));
    let _ = conn.events(); // drop the reset's snapshot churn

    // Drive the heat loop forward: Scheduled -> Staged -> Armed (Start arms the heat).
    drive(&mut log, &heat, &mut state, HeatCommand::Stage);
    drive(&mut log, &heat, &mut state, HeatCommand::Start);

    // Actually start the race on RH (it stages + auto-starts), then record Running.
    conn.stage_race().expect("stage_race");
    let mut live: Vec<Event> = Vec::new();
    assert!(
        wait_until(&conn, &mut live, Duration::from_secs(20), |evs| {
            evs.iter()
                .any(|e| matches!(e, Event::SessionStarted { .. }))
        }),
        "RotorHazard never reached RACING"
    );
    // The override stands in for the runtime auto-start here (this harness drives the FSM by
    // hand rather than running the Director clock): force Armed -> Running.
    drive(&mut log, &heat, &mut state, HeatCommand::SkipCountdown);
    debug_assert_eq!(state, HeatState::Running);

    // While Running, poll the timer crossings; hold the race open until `min_passes` have
    // landed, so the heat's pass count is the scenario's choice and not a timing race.
    let min_passes = min_passes.max(1);
    let got_pass = wait_until(&conn, &mut live, Duration::from_secs(60), |evs| {
        evs.iter().filter(|e| matches!(e, Event::Pass(_))).count() >= min_passes
    });

    // Close the race on RH, then drain any final crossings before finishing.
    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    live.extend(conn.events());
    conn.disconnect();

    assert!(
        got_pass,
        "the heat was held open for {min_passes} timer crossing(s) and only {} arrived",
        live.iter().filter(|e| matches!(e, Event::Pass(_))).count()
    );

    // Interleave the live events `keep` selects into the log between Running and Finished, in
    // the order RH reported them. By default that is `Pass`es only — lifecycle/`CompetitorSeen`
    // are adapter bookkeeping, not part of the heat's canonical race-engine log — but the
    // marshaling harness also keeps the signal facts (see `run_mock_heat_with_signal`).
    log.extend(live.into_iter().filter(|e| keep(e)));

    // Close the heat loop: ForceEnd (Running -> Unofficial) then Finalize. ForceEnd stands in for
    // the runtime auto-complete here (the harness has already stopped the RH race).
    drive(&mut log, &heat, &mut state, HeatCommand::ForceEnd);
    drive(&mut log, &heat, &mut state, HeatCommand::Finalize);
    debug_assert_eq!(state, HeatState::Final);

    log
}
