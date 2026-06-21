//! Dockerized-RotorHazard **Director persistent-connect** e2e (#65, #73, #105).
//!
//! The end-to-end proof that the Director **connects a real RotorHazard on selection and keeps it
//! connected** (#105) — the live counterpart to the in-process Mock-bridge tests in `src/source.rs`.
//! It:
//!
//! 1. Spins up a disposable dockerized RotorHazard (mock node) via the shared
//!    [`RhContainer`](gridfpv_testkit::RhContainer) harness.
//! 2. Builds an [`EventRegistry`], creates a [`Rotorhazard { url }`] timer pointed at the
//!    container, **makes Practice the active event**, and **selects the RH timer for Practice** (the
//!    selection the persistent-connection reconciler reads).
//! 3. Spawns the **real** registry bridge ([`spawn_registry_bridge`]) — the same wiring `main`
//!    runs (it also spawns the persistent-connection reconciler).
//! 4. Asserts the Director **connects on selection, before any heat**: the timer's [`TimerStatus`]
//!    advances to `Connected` *without* a heat being run (#105 — the whole point: a drop-off is
//!    visible before/between races).
//! 5. Then drives Practice's heat to `Running`, and asserts **passes flow** into Practice's log over
//!    that already-live connection (real RH crossings, attributed to the heat's lineup).
//! 6. Finishes the heat and asserts the connection **stays `Connected`** — the heat is disarmed but
//!    the persistent connection is NOT torn down (#105), so status keeps reflecting the live link.
//! 7. **Stops the RH container** out from under the live connection and asserts the Director
//!    **detects the drop** — the timer leaves `Connected` (→ `Disconnected`/`Error`/`Connecting`)
//!    within ~10s (#105). This is the regression guard: a buffering auto-reconnect used to hide a
//!    real drop, so the timer read `Connected` indefinitely.
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
use gridfpv_server::timers::{CreateTimerRequest, MOCK_TIMER_ID, TimerId, TimerKind, TimerStatus};
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
#[ignore = "requires Docker (spins up dockerized RotorHazard, connects it on selection, then drives a live heat over the persistent connection)"]
async fn director_connects_rotorhazard_on_selection_and_keeps_it_connected_through_a_heat() {
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
    // A freshly-configured RH timer rests at `Configured` until the Director connects it.
    assert_eq!(
        registry.timers().get(&rh_timer.id).unwrap().status,
        TimerStatus::Configured
    );
    // Make Practice the active event and select the RH timer for it — the reconciler connects the
    // active event's selected RH timers (#105).
    registry
        .set_active(&practice())
        .expect("make Practice active");
    registry
        .set_timers(&practice(), vec![rh_timer.id.clone()])
        .expect("select the RH timer for Practice");

    let state = registry.resolve(&practice()).expect("Practice state");

    // === Spawn the REAL registry bridge — the same wiring `main` runs (it also spawns the
    // persistent-connection reconciler). ===
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    // === The Director connects ON SELECTION — BEFORE any heat is run (#105). This is the crux:
    // a selected-but-idle RH timer must reach Connected so a drop-off is visible before/between
    // races, not only while a race is underway. ===
    let timers = registry.timers();
    let id = rh_timer.id.clone();
    let connected = wait_for(Duration::from_secs(30), || {
        timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        connected,
        "the Director should report the RH timer Connected on selection, before any heat; status = {:?}",
        timers.get(&rh_timer.id).map(|t| t.status)
    );
    // No heat has run yet, yet the timer is live — there are no passes in the log at this point.
    assert_eq!(
        count_passes(&read_all(&state)),
        0,
        "no passes should exist before a heat — the connection is idle but live"
    );

    // === Now drive Practice's heat to Running (what the control path appends). It uses the
    // ALREADY-LIVE connection rather than dialing a fresh socket. ===
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

    // === Finish the heat: the heat is disarmed but the persistent connection STAYS UP (#105). ===
    state
        .append(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Finished,
            },
            None,
        )
        .unwrap();
    // Give the bridge several poll cycles to observe the Finished transition and disarm the heat,
    // then assert the connection is *still* Connected — not torn down. We can't prove a negative by
    // waiting forever, so we wait a generous window and assert it never left Connected.
    let stayed_connected = wait_for(Duration::from_secs(8), || {
        // Invert the usual helper: succeed only if it ever LEFT Connected (a regression). If it
        // never does, `wait_for` returns false after the window — which is what we want.
        timers.get(&rh_timer.id).map(|t| t.status) != Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        !stayed_connected,
        "the persistent connection must STAY Connected after a heat finishes (the heat is disarmed, \
         the socket is not torn down) — that is the whole point of #105; status = {:?}",
        timers.get(&rh_timer.id).map(|t| t.status)
    );

    eprintln!(
        "app RH-persistent-connect e2e: connected ON SELECTION (before any heat), {pass_count} real \
         pass(es) into the event log while the heat ran, and STILL Connected after it finished"
    );

    // === Drop detection (#105): stop the RH container out from under the live connection and assert
    // the Director observes the drop within ~10s. This is the regression the fix targets: with
    // `rust_socketio`'s auto-reconnect the emit-buffering hid a real drop and the timer stayed
    // Connected indefinitely. With `.reconnect(false)` + the `close`/`error` handlers flipping the
    // alive flag, the driver's monitor now catches it and moves the timer off Connected. ===
    eprintln!("app RH-drop-detection: stopping the RH container to simulate a timer drop-off…");
    rh.stop();
    let drop_start = Instant::now();
    let dropped = wait_for(Duration::from_secs(10), || {
        matches!(
            timers.get(&rh_timer.id).map(|t| t.status),
            Some(TimerStatus::Disconnected)
                | Some(TimerStatus::Error)
                | Some(TimerStatus::Connecting)
        )
    })
    .await;
    let dropped_status = timers.get(&rh_timer.id).map(|t| t.status);
    assert!(
        dropped,
        "the Director must detect a stopped RotorHazard and move the timer off Connected within \
         10s (the whole point of #105's drop detection); status = {dropped_status:?}"
    );
    eprintln!(
        "app RH-drop-detection: timer left Connected after RH was stopped — status {dropped_status:?} \
         in {:?} (drop DETECTED)",
        drop_start.elapsed()
    );
}

/// DISTINCT RH host port for the primary/alternate failover e2e (avoids the 5042 above).
const RH_FAILOVER_PORT: u16 = 5043;

/// Primary RH + alternate Mock **failover** over a live connection (issue #112).
///
/// The end-to-end proof of the single-active-source feed + failover: with a **primary RH** timer
/// (live, dockerized) and an **alternate Mock** both selected for the running heat, only the RH
/// primary's passes feed the log while it is healthy — the Mock alternate is hot standby (gated
/// off). When the RH container is **stopped mid-heat** the Director fails over: the primary leaves
/// `Connected`, and the Mock alternate's synthetic passes take over so laps keep landing. This is
/// the redundancy guarantee — a dropped primary does not stop the race log.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (primary RH + alternate Mock; stops the RH container mid-heat and asserts the Mock alternate takes over)"]
async fn director_fails_over_from_a_dropped_rh_primary_to_a_mock_alternate() {
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 180,
            baseline_rssi: 70,
        }),
    )];
    let rh = RhContainer::start(RH_FAILOVER_PORT, TICK, &scenario);

    let registry = EventRegistry::new(None).expect("event registry");
    // A brisk, long-running Mock alternate so it still has passes to emit after the failover (a
    // hot-standby Mock runs its emission in real time; failover catches the not-yet-emitted passes).
    registry
        .timers()
        .update(
            &TimerId(MOCK_TIMER_ID.to_string()),
            &gridfpv_server::timers::UpdateTimerRequest {
                name: None,
                kind: Some(TimerKind::Mock {
                    laps: 600,
                    lap_ms: 100,
                }),
            },
        )
        .expect("retune the Mock alternate to a long, brisk run");
    let rh_timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Field RH".into(),
            kind: TimerKind::Rotorhazard {
                url: rh.url().to_string(),
            },
        })
        .expect("create RH timer");

    registry
        .set_active(&practice())
        .expect("make Practice active");
    // Select [RH, Mock] with the RH as the explicit PRIMARY and the Mock as the alternate.
    registry
        .set_timers(
            &practice(),
            vec![rh_timer.id.clone(), TimerId(MOCK_TIMER_ID.to_string())],
        )
        .expect("select RH primary + Mock alternate");
    registry
        .set_primary_timer(&practice(), Some(rh_timer.id.clone()))
        .expect("designate the RH timer primary");

    let state = registry.resolve(&practice()).expect("Practice state");
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    // The RH primary connects on selection.
    let timers = registry.timers();
    let id = rh_timer.id.clone();
    assert!(
        wait_for(Duration::from_secs(30), || {
            timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
        })
        .await,
        "the RH primary should reach Connected on selection"
    );

    // Run the heat: while the RH primary is healthy, its passes feed (the Mock alternate is gated).
    let heat = HeatId("q-fo-1".into());
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

    assert!(
        wait_for(Duration::from_secs(40), || count_passes(&read_all(&state))
            >= 1)
        .await,
        "the RH primary's passes should feed while it is healthy"
    );
    let before_drop = count_passes(&read_all(&state));
    eprintln!("app failover e2e: {before_drop} pass(es) from the RH primary before the drop");

    // === Stop the RH container mid-heat: the primary drops and the Mock alternate takes over. ===
    eprintln!("app failover e2e: stopping the RH primary container mid-heat…");
    rh.stop();
    assert!(
        wait_for(Duration::from_secs(15), || {
            matches!(
                timers.get(&rh_timer.id).map(|t| t.status),
                Some(TimerStatus::Disconnected)
                    | Some(TimerStatus::Error)
                    | Some(TimerStatus::Connecting)
            )
        })
        .await,
        "the Director must detect the dropped RH primary"
    );

    // The Mock alternate now feeds: the pass count keeps growing past the pre-drop total even though
    // the RH primary is dead. This is the failover — a dropped primary does not stop the log.
    let target = before_drop + 2;
    assert!(
        wait_for(Duration::from_secs(20), || count_passes(&read_all(&state))
            >= target)
        .await,
        "the Mock alternate should take over and keep laps landing after the RH primary dropped; \
         passes = {} (wanted ≥ {target})",
        count_passes(&read_all(&state))
    );
    eprintln!(
        "app failover e2e: Mock alternate took over after the RH primary dropped — {} total pass(es) \
         (failover CONFIRMED)",
        count_passes(&read_all(&state))
    );
}
