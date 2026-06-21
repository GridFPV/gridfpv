//! Dockerized-RotorHazard **Director connect** e2e (#65, #73 — Slice 2b backend).
//!
//! The end-to-end proof that the Director's per-event source bridge *actually connects* a real
//! RotorHazard and feeds its passes into the event's log — the live counterpart to the in-process
//! Mock-bridge tests in `src/source.rs`. It:
//!
//! 1. Spins up a disposable dockerized RotorHazard (mock node) via the shared
//!    [`RhContainer`](gridfpv_testkit::RhContainer) harness.
//! 2. Builds an [`EventRegistry`], creates a [`Rotorhazard { url }`] timer pointed at the
//!    container, and **selects it for Practice** (the per-event selection the bridge reads).
//! 3. Spawns the **real** registry bridge ([`spawn_registry_bridge`]) — the same wiring `main`
//!    runs — then drives Practice's heat to `Running` through its log (what the control path
//!    appends).
//! 4. Asserts the Director **connects**: the timer's [`TimerStatus`] advances to `Connected`, and
//!    **passes flow** into Practice's log (real RH crossings, attributed to the heat's lineup).
//! 5. Finishes the heat and asserts the bridge tears the connection down (status leaves
//!    `Connected`).
//!
//! Like every `*_live` test this is **structural / tolerant** — RH's mock interface reads its CSV
//! continuously so lap *timing* is not controllable; we assert the connection state reached and
//! the presence of passes, never exact µs or an exact lap count.
//!
//! Local-only class (needs Docker), gated behind `--features live` + `#[ignore]`. DISTINCT RH port
//! 5042 (server full-event uses 5041, engine full-event 5040). Run via `cargo xtask live`, or:
//!
//! ```sh
//! cargo test -p gridfpv-app --features live --test rh_connect_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use gridfpv_app::source::{SIM_ADAPTER, SourceConfig, spawn_registry_bridge};
use gridfpv_events::{AdapterId, CompetitorRef, Event, HeatId, HeatTransition};
use gridfpv_server::app::AppState;
use gridfpv_server::events::{EventRegistry, PRACTICE_EVENT_ID};
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{CreateTimerRequest, TimerKind, TimerStatus};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};

/// DISTINCT RH host port for the app's RH-connect e2e (server full-event 5041, engine 5040).
const RH_PORT: u16 = 5042;
/// CSV tick interval (seconds) — a brisk pace so passes land within the live window.
const TICK: &str = "0.1";

/// Practice's event id (its in-memory log + default selection the bridge drives).
fn practice() -> EventId {
    EventId(PRACTICE_EVENT_ID.to_string())
}

fn count_passes(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::Pass(p) if p.gate.is_lap_gate()))
        .count()
}

fn read_all(state: &AppState) -> Vec<Event> {
    state
        .log()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .map(|s| s.event)
        .collect()
}

/// Poll until `cond` holds or `deadline` elapses; returns whether it held.
async fn wait_for(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (spins up dockerized RotorHazard and drives a live heat through the Director bridge)"]
async fn director_connects_rotorhazard_and_passes_flow_into_the_event_log() {
    // One busy node so several real passes land while the heat runs. `node-0` is the seat the
    // adapter reports; the bridge remaps it onto the heat's first lineup slot.
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 180,
            baseline_rssi: 70,
        }),
    )];
    // RAII: the container is removed when `rh` drops at the end of the test.
    let rh = RhContainer::start(RH_PORT, TICK, &scenario);

    // === Build the registry, configure an RH timer, and select it for Practice. ===
    let registry = EventRegistry::new(None).expect("event registry");
    let rh_timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Field RH".into(),
            kind: TimerKind::Rotorhazard {
                url: rh.url().to_string(),
            },
        })
        .expect("create RH timer");
    // A freshly-configured RH timer rests at `Configured` until the bridge connects it.
    assert_eq!(
        registry.timers().get(&rh_timer.id).unwrap().status,
        TimerStatus::Configured
    );
    registry
        .set_timers(&practice(), vec![rh_timer.id.clone()])
        .expect("select the RH timer for Practice");

    let state = registry.resolve(&practice()).expect("Practice state");

    // === Spawn the REAL registry bridge — the same wiring `main` runs. ===
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    // === Drive Practice's heat to Running (what the control path appends). ===
    let heat = HeatId("q-rh-1".into());
    let pilot = CompetitorRef("Ace".into());
    state
        .append(
            Event::HeatScheduled {
                heat: heat.clone(),
                lineup: vec![pilot.clone()],
            },
            None,
        )
        .unwrap();
    state
        .append(
            Event::HeatStateChanged {
                heat: heat.clone(),
                transition: HeatTransition::Running,
            },
            None,
        )
        .unwrap();

    // === The Director connects: status advances to Connected. ===
    let timers = registry.timers();
    let id = rh_timer.id.clone();
    let connected = wait_for(Duration::from_secs(30), || {
        timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        connected,
        "the Director should report the RH timer Connected once the socket is up; status = {:?}",
        timers.get(&rh_timer.id).map(|t| t.status)
    );

    // === Passes flow into Practice's log (real RH crossings, attributed to the lineup). ===
    let got_passes = wait_for(Duration::from_secs(40), || {
        count_passes(&read_all(&state)) >= 1
    })
    .await;
    assert!(
        got_passes,
        "real RotorHazard passes should land in the event log while the heat is Running"
    );
    // The passes are attributed to the heat's actual competitor (node-0 remapped onto lineup[0]),
    // not the raw `node-0` seat handle.
    let events = read_all(&state);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Pass(p) if p.competitor == pilot)),
        "passes should be remapped onto the heat's lineup competitor"
    );
    let pass_count = count_passes(&events);

    // === Finish the heat: the bridge tears the connection down. ===
    state
        .append(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Finished,
            },
            None,
        )
        .unwrap();
    let torn_down = wait_for(Duration::from_secs(15), || {
        timers.get(&rh_timer.id).map(|t| t.status) != Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        torn_down,
        "the Director should leave Connected once the heat finishes (teardown); status = {:?}",
        timers.get(&rh_timer.id).map(|t| t.status)
    );

    eprintln!(
        "app RH-connect e2e: connected, {pass_count} real pass(es) into the event log, then torn down"
    );
}
