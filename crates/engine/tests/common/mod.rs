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
pub fn run_mock_heat(port: u16, heat: &str, scenario: &[(usize, String)]) -> Vec<Event> {
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

    // While Running, poll the timer crossings; keep at least one pass.
    let got_pass = wait_until(&conn, &mut live, Duration::from_secs(25), |evs| {
        evs.iter().any(|e| matches!(e, Event::Pass(_)))
    });

    // Close the race on RH, then drain any final crossings before finishing.
    conn.stop_race().ok();
    std::thread::sleep(Duration::from_millis(800));
    live.extend(conn.events());
    conn.disconnect();

    assert!(
        got_pass,
        "no timer crossings were produced while the heat was running"
    );

    // Interleave the live passes into the log between Running and Finished, in the
    // order RH reported them. Only `Pass`es are folded in — lifecycle/`CompetitorSeen`
    // are adapter bookkeeping, not part of the heat's canonical race-engine log.
    log.extend(live.into_iter().filter(|e| matches!(e, Event::Pass(_))));

    // Close the heat loop: ForceEnd (Running -> Unofficial) then Finalize. ForceEnd stands in for
    // the runtime auto-complete here (the harness has already stopped the RH race).
    drive(&mut log, &heat, &mut state, HeatCommand::ForceEnd);
    drive(&mut log, &heat, &mut state, HeatCommand::Finalize);
    debug_assert_eq!(state, HeatState::Final);

    log
}
