//! Dockerized-RotorHazard **plugin-less refusal** e2e (#424, the decision in #405).
//!
//! The GridFPV plugin is **required** to race a RotorHazard timer (#405). This is the live proof
//! that GridFPV *degrades correctly* when it is not there — which is the one thing the matrix's
//! no-plugin legs used to leave uncovered.
//!
//! ## Why this test exists at all
//!
//! Those legs previously ran `INGEST_TARGETS`: they asserted the **stock `current_laps` path
//! works**. Under #405 that is no longer a behaviour GridFPV guarantees, so the assertion was
//! testing a promise the product had withdrawn. The obvious move — delete the legs — recreates
//! #389 one level up: #389 was a plugin-only lap-ingestion regression that reached real hardware
//! with **all 12 live targets green**, because only one configuration was ever exercised. Deleting
//! the no-plugin legs would leave nothing proving that a plugin-less timer produces a *clear
//! refusal* rather than a silent half-working race. #405 says so explicitly:
//!
//! > Do not simply delete them: *"we degrade correctly"* is exactly what regressed silently in
//! > #389.
//!
//! ## What it asserts, against a real plugin-less RotorHazard
//!
//! 1. **Connecting succeeds.** #405 puts the refusal at *racing*, not at connecting: a live socket
//!    is what lets Grid probe presence at all, drive #386's restart once the plugin is dropped in,
//!    and tell the RD what is wrong. So the timer reaches `Connected`, and its presence is probed
//!    to a definite [`PluginPresence::Missing`] — **not** left `None`, which is a different problem
//!    ("connect it first") with a different fix.
//! 2. **Selecting it for an event refuses** — `PUT /events/{id}/timers` answers a typed `400`.
//! 3. **Arming a heat on it refuses** — the arm-time backstop, for the case a selection was valid
//!    when it was made and the plugin went away afterwards (an RD restarts RH without it). This is
//!    reached by selecting through the registry, exactly as a pre-#405 persisted event carries it.
//! 4. **Both refusals are specific and RD-facing.** Each says the plugin is the problem and names
//!    the timer by its **friendly name** — never its raw id (the repo display rule). A generic
//!    connection error, or silence, fails this test.
//! 5. **Nothing raced.** No pass reaches the log: the refusal is a refusal, not a warning in front
//!    of a race that runs anyway.
//!
//! The complementary liveness case — a plugin that loads and then **stops delivering**, which must
//! fall back to `current_laps` *and say so* (#389) — is a different scenario and is covered on the
//! plugin legs by the adapter's own fallback tests; required-and-present is not
//! required-and-healthy.
//!
//! The container here is started with **no plugins mounted at all**, ignoring
//! [`PLUGIN_ENV`](gridfpv_testkit::PLUGIN_ENV), so the test means the same thing on whichever
//! matrix leg runs it. The RH **version** axis still varies, which is the part that matters: the
//! handshake probe has to reach `Missing` on 4.3.0 (the floor, and what the RD's field timer runs)
//! as well as on the current stable.
//!
//! Local-only class (needs Docker), gated behind `--features live` + `#[ignore]`. DISTINCT RH port
//! 5044 (app RH-connect 5042, app RH-restart 5043, server full-event 5041, engine 5040). Run via
//! `cargo xtask live --no-plugin`, or:
//!
//! ```sh
//! cargo test -p gridfpv-app --features live --test rh_no_plugin_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gridfpv_app::source::{SIM_ADAPTER, SourceConfig, spawn_registry_bridge};
use gridfpv_events::{AdapterId, CompetitorRef, Event, HeatId};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::control::{Command, CommandAck};
use gridfpv_server::events::{CreateEventRequest, EventRegistry};
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{CreateTimerRequest, PluginPresence, Timer, TimerKind, TimerStatus};
use gridfpv_testkit::RhContainer;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// DISTINCT RH host port for this e2e (app RH-connect 5042, app RH-restart 5043).
const RH_PORT: u16 = 5044;
/// CSV tick interval (seconds). Nothing here races, so the pace only has to be plausible.
const TICK: &str = "0.1";
/// The timer's **friendly name** — the string every refusal must put in front of the RD.
const TIMER_NAME: &str = "Bench RotorHazard";

/// The id of the registry's single created event.
fn event_of(registry: &EventRegistry) -> EventId {
    let mut list = registry.list();
    assert_eq!(list.len(), 1, "one created event per test registry");
    EventId(list.remove(0).id.0)
}

/// A fresh registry holding one created event.
fn test_registry() -> EventRegistry {
    let registry = EventRegistry::new(None).expect("event registry");
    registry
        .create(&CreateEventRequest::named("No-Plugin Refusal E2E"))
        .expect("create the test event");
    registry
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
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Drive one request through the real router with no socket (`tower::ServiceExt::oneshot`);
/// return the status and the body as text.
async fn call(registry: &EventRegistry, request: Request<Body>) -> (StatusCode, String) {
    let response = router(registry.clone())
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("collect the body")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn post(uri: String, json: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(json))
        .unwrap()
}

fn put(uri: String, json: String) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(json))
        .unwrap()
}

fn passes(state: &AppState) -> usize {
    state
        .log()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .filter(|s| matches!(&s.event, Event::Pass(p) if p.gate.is_lap_gate()))
        .count()
}

/// Assert `message` is the RD-facing refusal this test demands: it names the timer by its friendly
/// name, blames the **plugin** specifically, and leaks no raw id.
fn assert_names_the_timer_and_the_plugin(message: &str, timer: &Timer, what: &str) {
    assert!(
        message.contains(TIMER_NAME),
        "the {what} refusal must name the timer by its friendly name — the RD has to know WHICH \
         timer to go fix: {message:?}"
    );
    assert!(
        message.to_lowercase().contains("plugin"),
        "the {what} refusal must say the GridFPV plugin is the problem, not read as a generic \
         connection error — that ambiguity is the whole point of #405: {message:?}"
    );
    assert!(
        !message.contains(&timer.id.0),
        "the {what} refusal leaks the raw timer id {:?} (CLAUDE.md: friendly names, never raw \
         ids): {message:?}",
        timer.id.0
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (spins up a plugin-less dockerized RotorHazard and asserts Grid connects but refuses to race it)"]
async fn a_plugin_less_rotorhazard_connects_but_grid_refuses_to_race_it_and_says_why() {
    // A real RotorHazard with **no plugins mounted whatsoever** — not even when the matrix leg
    // exports GRIDFPV_RH_PLUGIN. `start_with_plugins(&[])` is the explicit, env-independent way to
    // say "stock RH", so this test asserts the same thing on every leg it is run on.
    let rh = RhContainer::start_with_plugins(RH_PORT, TICK, &[], &[]);

    let registry = test_registry();
    let event = event_of(&registry);
    let timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: TIMER_NAME.into(),
            kind: TimerKind::Rotorhazard {
                url: rh.url().to_string(),
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        })
        .expect("create the RH timer");

    // The real wiring `main` runs, including the persistent-connection reconciler.
    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    // ── 1. CONNECTING SUCCEEDS ────────────────────────────────────────────────────────────────
    // #383's event-less connect: a timer can be connected with no event selecting it, which is the
    // only way presence can be probed *before* selection. #405 deliberately puts the refusal at
    // racing rather than here — without a live socket Grid could not probe the plugin, could not
    // drive #386's restart after the RD drops it in, and could not tell them what is wrong.
    let (status, body) = call(
        &registry,
        Request::builder()
            .method("POST")
            .uri(format!("/timers/{}/connect", timer.id.0))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "connecting a plugin-less RotorHazard must SUCCEED — the refusal belongs at racing, not at \
         connecting: {body}"
    );

    let timers = registry.timers();
    let id = timer.id.clone();
    let connected = wait_for(Duration::from_secs(30), || {
        timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        connected,
        "a plugin-less RotorHazard must still reach Connected; status = {:?}",
        timers.get(&id).map(|t| t.status)
    );

    // Presence is probed to a DEFINITE `Missing`. `None` would be a different problem with a
    // different fix ("connect this timer first"), and reporting it here would send the RD looking
    // for a connection fault instead of an uninstalled plugin.
    let probed = wait_for(Duration::from_secs(15), || {
        matches!(
            timers.get(&id).and_then(|t| t.plugin.clone()),
            Some(PluginPresence::Missing)
        )
    })
    .await;
    assert!(
        probed,
        "the handshake probe must resolve a stock RotorHazard to PluginPresence::Missing, not \
         leave it unprobed; plugin = {:?}",
        timers.get(&id).and_then(|t| t.plugin.clone())
    );

    // ── 2. SELECTING IT FOR AN EVENT REFUSES ──────────────────────────────────────────────────
    // The gate lives in the API, not only in the console's picker: `PUT /events/{id}/timers` is
    // reachable directly, and a rule enforced only in the UI is not enforced.
    let (status, body) = call(
        &registry,
        put(
            format!("/events/{}/timers", event.0),
            serde_json::json!({ "ids": [timer.id.0] }).to_string(),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "selecting a plugin-less RotorHazard for an event must be refused: {body}"
    );
    assert_names_the_timer_and_the_plugin(&body, &timer, "selection");
    assert!(
        !registry
            .meta_of(&event)
            .expect("the event")
            .timers
            .contains(&timer.id),
        "a refused selection must not be recorded — a half-applied selection is how an event ends \
         up racing a timer the API said no to"
    );

    // ── 3. ARMING A HEAT ON IT REFUSES ────────────────────────────────────────────────────────
    // The arm-time backstop (#405): a selection that was *valid when it was made* and a plugin
    // that has since gone away — the RD restarts RotorHazard without it, or a pre-#405 event was
    // persisted already selecting it. Seed that shape through the registry, which is exactly how
    // such an event carries the selection, then arm.
    registry
        .set_timers(&event, vec![timer.id.clone()])
        .expect("seed the pre-existing selection the backstop exists for");
    registry.set_active(&event).expect("make the event active");
    let state = registry.resolve(&event).expect("the event's state");

    let heat = HeatId("q-no-plugin-1".into());
    state
        .append(
            Event::HeatScheduled {
                heat: heat.clone(),
                lineup: vec![CompetitorRef("Ace".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            None,
        )
        .expect("schedule the heat");

    let (status, body) = call(
        &registry,
        post(
            format!("/events/{}/control", event.0),
            serde_json::to_string(&Command::Start { heat: heat.clone() }).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the control route answers with an ack, not an HTTP error: {body}"
    );
    let ack: CommandAck = serde_json::from_str(&body).expect("the body is a CommandAck");
    assert!(
        !ack.ok,
        "arming a heat on a plugin-less RotorHazard must be REFUSED, not acked: {ack:?}"
    );
    let message = ack
        .error
        .as_ref()
        .map(|e| e.message.clone())
        .unwrap_or_default();
    assert!(
        !message.is_empty(),
        "a refusal with no message is the silence #405 rules out: {ack:?}"
    );
    assert_names_the_timer_and_the_plugin(&message, &timer, "arm");

    // ── 4. NOTHING RACED ──────────────────────────────────────────────────────────────────────
    // The refusal has to be a refusal. Give the bridge several polls to prove it did NOT arm the
    // connection behind the ack, then assert the heat never left `Scheduled` and no pass landed.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        passes(&state),
        0,
        "a refused arm must not race: no pass may reach the log"
    );
    let armed = state
        .log()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .any(|s| matches!(&s.event, Event::HeatStateChanged { heat: h, .. } if h == &heat));
    assert!(
        !armed,
        "a refused arm must append no heat transition — the heat stays Scheduled"
    );
}
