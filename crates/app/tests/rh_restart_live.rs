//! Dockerized-RotorHazard **restart-from-GridFPV** e2e (#386).
//!
//! The end-to-end proof that the RD can complete the guided plugin install **without ever opening
//! RotorHazard's web UI**: RotorHazard imports plugins once at startup, so the folder just dropped
//! into `plugins/` is inert until RH re-executes — and RH exposes that restart, unauthenticated, on
//! the socket the Director is already holding (`restart_server`, v4.4.0 `server.py:1881`).
//!
//! This drives the **whole real path**, not just the emit:
//!
//! 1. A disposable dockerized RotorHazard, connected by the **real** registry bridge (the same
//!    wiring `main` runs), reaching `Connected` with its plugin probed.
//! 2. The RD-gated **`POST /timers/{id}/restart` route on the Director's own router** parks the
//!    request; the connection reconciler drains it and the driver emits `restart_server`.
//! 3. RotorHazard **actually re-executes** — its container log gains a second `RotorHazard v…`
//!    startup banner, which is the only proof the plugin directory is genuinely re-imported.
//! 4. The Director rides the expected **drop → reconnect** on its own: the timer leaves `Connected`
//!    (this is normal, not a fault), reconnects with backoff, and the reconnect **re-probes the
//!    plugin** — the mechanism that flips a timer's `PluginPresence` `Missing → Present` with no
//!    extra plumbing, which is why the whole feature composes for free.
//! 5. The **refusal** is real, not a confirm dialog: with a heat `Running` on that timer the route
//!    answers `400` naming the heat, and RotorHazard does **not** restart (the banner count is
//!    unchanged). Restarting mid-race would take the RD's timing hardware down with the race on it.
//!
//! Only `restart_server` is wired anywhere in the product — its `shutdown_pi` / `reboot_pi`
//! neighbours stay out of reach — so there is nothing else here to exercise.
//!
//! Local-only class (needs Docker), gated behind `--features live` + `#[ignore]`. DISTINCT RH port
//! 5045 (the app's connect e2e uses 5042, its failover e2e 5043). Run via `cargo xtask live`, or:
//!
//! ```sh
//! cargo test -p gridfpv-app --features live --test rh_restart_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gridfpv_app::director::build_app;
use gridfpv_app::source::{SIM_ADAPTER, SourceConfig, spawn_registry_bridge};
use gridfpv_events::{AdapterId, CompetitorRef, Event, HeatId, HeatTransition};
use gridfpv_server::events::{CreateEventRequest, EventRegistry};
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{CreateTimerRequest, TimerId, TimerKind, TimerStatus};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// DISTINCT RH host port for the restart e2e (connect e2e 5042, failover 5043).
const RH_PORT: u16 = 5045;
/// CSV tick interval (seconds).
const TICK: &str = "0.1";

/// The line RotorHazard prints **once per process start** — so counting it counts restarts. A
/// `restart_server` re-execs the process, which prints the banner again.
const RH_STARTUP_BANNER: &str = "RotorHazard v";

/// The id of the registry's single created event (its log + the timer selection the bridge
/// drives). There is no built-in event any more (#414), so [`test_registry`] creates one.
fn event_of(registry: &EventRegistry) -> EventId {
    let mut list = registry.list();
    assert_eq!(list.len(), 1, "one created event per test registry");
    EventId(list.remove(0).id.0)
}

/// A fresh registry holding one created event — the fixture every live test drives against.
fn test_registry() -> EventRegistry {
    let registry = EventRegistry::new(None).expect("event registry");
    registry
        .create(&CreateEventRequest::named("Live Test Event"))
        .expect("create the test event");
    registry
}

/// How many times RotorHazard has started inside `rh` (its startup banner count).
fn start_count(rh: &RhContainer) -> usize {
    rh.logs()
        .lines()
        .filter(|l| l.contains(RH_STARTUP_BANNER))
        .count()
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

/// `POST /timers/{id}/restart` against the **Director's own router** — the real RD-gated route,
/// with no network. Returns its status and body text.
async fn post_restart(registry: &EventRegistry, timer: &TimerId) -> (StatusCode, String) {
    // The SPA fallback needs an assets dir; a non-existent one is fine (the API routes match first).
    let assets = std::env::temp_dir().join("gridfpv-rh-restart-live-no-assets");
    let response = build_app(registry.clone(), &assets)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/timers/{}/restart", timer.0))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (restarts a dockerized RotorHazard from the Director and asserts it reconnects and re-probes the plugin)"]
async fn restarting_a_timer_reexecutes_rotorhazard_and_the_director_reconnects_and_re_probes() {
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 180,
            baseline_rssi: 70,
            seed: 0,
        }),
    )];
    // RAII: removed when `rh` drops at the end of the test.
    let rh = RhContainer::start(RH_PORT, TICK, &scenario);

    // === Configure the RH timer, select it for the active Practice event, and run the REAL bridge
    // (which also spawns the persistent-connection reconciler — the thing that drains the restart
    // request queue). ===
    let registry = test_registry();
    let rh_timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Field RH".into(),
            kind: TimerKind::Rotorhazard {
                url: rh.url().to_string(),
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        })
        .expect("create RH timer");
    registry
        .set_active(&event_of(&registry))
        .expect("make the event active");
    registry
        .set_timers(&event_of(&registry), vec![rh_timer.id.clone()])
        .expect("select the RH timer for the event");
    let state = registry
        .resolve(&event_of(&registry))
        .expect("the event's state");

    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    let timers = registry.timers();
    let id = rh_timer.id.clone();
    assert!(
        wait_for(Duration::from_secs(30), || {
            timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
        })
        .await,
        "the Director should connect the selected RH timer; status = {:?}",
        timers.get(&id).map(|t| t.status)
    );
    // The plugin probe runs on connect — capture what it found so the post-restart probe can be
    // compared against it (the re-probe is the mechanism that flips Missing → Present for real).
    assert!(
        wait_for(Duration::from_secs(10), || {
            timers.get(&id).is_some_and(|t| t.plugin.is_some())
        })
        .await,
        "the Director should probe the RH timer for the GridFPV plugin after connect"
    );
    let plugin_before = timers.get(&id).and_then(|t| t.plugin.clone());
    let starts_before = start_count(&rh);
    assert_eq!(
        starts_before, 1,
        "RotorHazard should have started exactly once so far; log shows {starts_before}"
    );

    // === The RD-gated route: park the restart. The reconciler drains it and the driver emits
    // RotorHazard's `restart_server` on the socket already held. ===
    let (status, body) = post_restart(&registry, &rh_timer.id).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the restart route should accept a connected, idle RH timer; body = {body}"
    );

    // === RotorHazard actually RE-EXECUTES. This is the assertion that matters: only a genuine
    // process restart re-imports the `plugins/` directory, which is the whole point of #386. ===
    let restarted = wait_for(Duration::from_secs(60), || start_count(&rh) > starts_before).await;
    assert!(
        restarted,
        "RotorHazard must re-execute on `restart_server` — its log should gain a second \
         '{RH_STARTUP_BANNER}…' startup banner; still {} start(s)",
        start_count(&rh)
    );

    // === The expected drop → reconnect, which the Director rides on its own. The timer leaving
    // `Connected` here is NORMAL (RH is re-executing), not a fault — the console narrates it as a
    // restart in progress rather than an error. ===
    let dropped = wait_for(Duration::from_secs(30), || {
        !matches!(
            timers.get(&id).map(|t| t.status),
            Some(TimerStatus::Connected)
        )
    })
    .await;
    assert!(
        dropped,
        "the socket must drop when RotorHazard re-executes; status = {:?}",
        timers.get(&id).map(|t| t.status)
    );

    // === …and comes back by itself: the driver's backoff reconnect (10s cap) re-establishes the
    // link with no further plumbing. ===
    let reconnect_start = Instant::now();
    let back = wait_for(Duration::from_secs(120), || {
        timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        back,
        "the Director must reconnect to the restarted RotorHazard on its own (backoff-capped at \
         10s); status = {:?}",
        timers.get(&id).map(|t| t.status)
    );

    // === The reconnect **re-probes the plugin** — the free mechanism the whole feature leans on:
    // after installing a plugin and restarting, PluginPresence flips Missing → Present with no
    // extra plumbing. Here the plugin set is unchanged across the restart, so the re-probe must
    // land on the SAME presence it found the first time. ===
    let re_probed = wait_for(Duration::from_secs(30), || {
        timers.get(&id).is_some_and(|t| t.plugin.is_some())
    })
    .await;
    assert!(re_probed, "the reconnect must re-probe the GridFPV plugin");
    let plugin_after = timers.get(&id).and_then(|t| t.plugin.clone());
    assert_eq!(
        format!("{plugin_after:?}"),
        format!("{plugin_before:?}"),
        "the post-restart re-probe should report the same plugin presence (the plugin directory is \
         unchanged across the restart)"
    );
    eprintln!(
        "app RH-restart e2e: RotorHazard re-executed and the Director reconnected in {:?}, \
         re-probing the plugin as {plugin_after:?}",
        reconnect_start.elapsed()
    );

    // === The REFUSAL is real, gated on heat phase — not a confirm dialog. Drive Practice's heat to
    // `Running` on this timer and assert the route says no, names the heat, and that RotorHazard is
    // NOT restarted. ===
    let starts_before_refusal = start_count(&rh);
    let heat = HeatId("q-restart-1".into());
    state
        .append(
            Event::HeatScheduled {
                heat: heat.clone(),
                lineup: vec![CompetitorRef("Ace".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: Some("Qualifier Heat 1".into()),
            },
            None,
        )
        .unwrap();
    for transition in [
        HeatTransition::Staged,
        HeatTransition::Armed,
        HeatTransition::Running,
    ] {
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat.clone(),
                    transition,
                },
                None,
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
    }

    let (status, body) = post_restart(&registry, &rh_timer.id).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "restarting a timer with a RUNNING heat on it must be refused outright; body = {body}"
    );
    // The refusal names the heat and the timer by their friendly names, never a raw id (CLAUDE.md).
    assert!(
        body.contains("Qualifier Heat 1") && body.contains("Field RH"),
        "the refusal must name the heat and the timer: {body}"
    );
    assert!(
        !body.contains(&rh_timer.id.0),
        "the refusal must not leak the raw timer id: {body}"
    );

    // Give the reconciler several ticks to prove nothing was queued: RotorHazard must NOT restart.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        start_count(&rh),
        starts_before_refusal,
        "a refused restart must not reach RotorHazard — its startup banner count must be unchanged"
    );
    eprintln!(
        "app RH-restart e2e: a Running heat refused the restart (naming it) and RotorHazard was \
         never re-executed"
    );

    // Leave the heat finished so the connection tears down cleanly with the test.
    state
        .append(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Finished,
            },
            None,
        )
        .unwrap();
}
