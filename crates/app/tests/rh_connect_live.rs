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
//! 5. Then drives Practice's heat through the real lifecycle to `Running`, and asserts **passes
//!    flow** into Practice's log over that already-live connection (real RH crossings, attributed to
//!    the heat's lineup). This also exercises the **Grid-owns-all-timing** flow: at **Staged** the
//!    bridge prepares the RH connection (zero RH's staging hold/tones + reset to READY) **and seats
//!    the heat's bound pilots onto their RH nodes** (the laps-attribute fix), and at **Running**
//!    (Grid's go) the driver emits a single `stage_race` so RH starts recording immediately — no
//!    RH-side staging sequence competing with Grid's start procedure. Because the reset now happens
//!    at Staged (seconds before the start emit, never the same gevent tick), the old reset-vs-staging
//!    race is gone and the `STAGE_RESET_SETTLE` band-aid is retired; this assertion (passes land, the
//!    heat is not zero-laps) is the guard. The seating is asserted directly: RH's own
//!    "Racing heat … pilots: Ace" log lists the bound callsign (vs the empty pilots list of the
//!    unseated bug), and RH dismisses **no** crossing for an unseated node.
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
use gridfpv_server::events::{CreateEventRequest, EventRegistry};
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{
    CreateTimerRequest, MOCK_TIMER_ID, PluginPresence, TimerId, TimerKind, TimerStatus,
};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};

/// Count how many lines of `rh`'s container log contain `needle` — used to assert the heat-end dense
/// save fires **exactly once** (no #250 loop: `Heat added` / `Current laps saved` must not repeat).
fn count_log_lines(rh: &RhContainer, needle: &str) -> usize {
    let out = std::process::Command::new("docker")
        .args(["logs", rh.name()])
        .output()
        .expect("docker logs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    text.lines().filter(|l| l.contains(needle)).count()
}

/// DISTINCT RH host port for the app's RH-connect e2e (server full-event 5041, engine 5040).
const RH_PORT: u16 = 5042;
/// CSV tick interval (seconds) — a brisk pace so passes land within the live window.
const TICK: &str = "0.1";

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
            seed: 0,
        }),
    )];
    // RAII: the container is removed when `rh` drops at the end of the test.
    let rh = RhContainer::start(RH_PORT, TICK, &scenario);

    // === Build the registry, configure an RH timer, and select it for Practice. ===
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
    // A freshly-configured RH timer rests at `Configured` until the Director connects it.
    assert_eq!(
        registry.timers().get(&rh_timer.id).unwrap().status,
        TimerStatus::Configured
    );
    // Make Practice the active event and select the RH timer for it — the reconciler connects the
    // active event's selected RH timers (#105).
    registry
        .set_active(&event_of(&registry))
        .expect("make the event active");
    registry
        .set_timers(&event_of(&registry), vec![rh_timer.id.clone()])
        .expect("select the RH timer for the event");

    let state = registry
        .resolve(&event_of(&registry))
        .expect("the event's state");

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
    // The GridFPV-plugin handshake probe runs right after connect (D16, S1): the timer's plugin
    // presence becomes *known* (Some). Under `cargo xtask live` the plugin folder is mounted
    // (GRIDFPV_RH_PLUGIN), so it resolves to `Present`; a plain `cargo test` (no plugin mounted)
    // resolves to `Missing`. Assert it was probed, and — when mounted — that it's recognized.
    let probed = wait_for(Duration::from_secs(10), || {
        timers.get(&id).map(|t| t.plugin.is_some()).unwrap_or(false)
    })
    .await;
    assert!(
        probed,
        "the Director should probe the RH timer for the GridFPV plugin after connect; plugin = {:?}",
        timers.get(&id).map(|t| t.plugin.clone())
    );
    if std::env::var_os("GRIDFPV_RH_PLUGIN").is_some() {
        let plugin = timers.get(&id).and_then(|t| t.plugin.clone());
        assert!(
            matches!(plugin, Some(PluginPresence::Present { .. })),
            "with the GridFPV plugin mounted, the handshake should report it Present; got {plugin:?}"
        );
    }

    // No heat has run yet, yet the timer is live — there are no passes in the log at this point.
    assert_eq!(
        count_passes(&read_all(&state)),
        0,
        "no passes should exist before a heat — the connection is idle but live"
    );

    // === Now drive Practice's heat through the real lifecycle to Running (what the control path
    // appends: Scheduled → Staged → Armed → Running). It uses the ALREADY-LIVE connection rather than
    // dialing a fresh socket. The **Staged** step is load-bearing for the Grid-owns-timing flow: it
    // is where the bridge *prepares* the RH connection (zero its staging hold/tones + reset to READY)
    // so the eventual arm at Running starts RH recording instantly with no RH-side staging. ===
    let heat = HeatId("q-rh-1".into());
    let pilot = CompetitorRef("Ace".into());
    state
        .append(
            Event::HeatScheduled {
                heat: heat.clone(),
                lineup: vec![pilot.clone()],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
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
        // Give the bridge a poll to act on Staged (prepare the RH connection) before arming at
        // Running — mirrors the real control path's spacing (the Armed hold sits between them).
        tokio::time::sleep(Duration::from_millis(700)).await;
    }

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

    // === The laps-attribute fix: the heat's bound pilot is SEATED on its RH node at Stage, so RH
    // records AND attributes passes (its pass gate dismisses a crossing on a node with no seated
    // pilot — the zero-laps bug). Prove the seat took on the RH side two ways:
    //
    //  1. RH's own staging log names the seated callsign — "Racing heat '…' round N, pilots: Ace" —
    //     rather than the empty "pilots:" that the unseated (buggy) path logged. This is the direct
    //     before/after: empty pilots list → the bound callsign listed.
    let pilots_line_lists_the_seated_callsign = {
        let out = std::process::Command::new("docker")
            .args(["logs", rh.name()])
            .output()
            .expect("docker logs");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        text.lines()
            .filter(|l| l.contains("Racing heat") && l.contains("pilots:"))
            .any(|l| l.contains("Ace"))
    };
    assert!(
        pilots_line_lists_the_seated_callsign,
        "RotorHazard's 'Racing heat … pilots:' log must list the heat's bound pilot ('Ace') — the \
         laps-attribute fix seats it on the RH node at Stage; an empty pilots list is the unseated \
         bug that records zero laps"
    );
    //  2. RH actually RECORDED passes (logged a `Pass record`, not the `Pass record dismissed: …
    //     Pilot not defined` the unseated gate emits). The `got_passes` assertion above already
    //     proved the laps flowed into the event log (attributed to node-0 → lineup[0]); this
    //     re-affirms RH itself recorded them rather than dismissing every crossing.
    assert_eq!(
        count_log_lines(&rh, "Pilot not defined"),
        0,
        "RotorHazard must not DISMISS passes for an unseated node ('Pilot not defined') — the bound \
         pilot is seated, so every crossing on its node records"
    );

    // Let the heat run on a little so the COARSE streamed trace accumulates a representative run of
    // samples (one `SignalChunk` per `node_data` heartbeat) — a longer, more realistic baseline for
    // the dense path to beat than a single sample.
    let pilot_key = gridfpv_projection::CompetitorKey {
        adapter: AdapterId(SIM_ADAPTER.to_string()),
        competitor: pilot.clone(),
    };
    let _ = wait_for(Duration::from_secs(12), || {
        gridfpv_projection::signal_trace(&read_all(&state))
            .competitor(&pilot_key)
            .map(|t| t.samples.len())
            .unwrap_or(0)
            >= 30
    })
    .await;
    let events = read_all(&state);
    // The COARSE streamed sample count for the lineup pilot, captured live before the heat finishes.
    let coarse_samples = gridfpv_projection::signal_trace(&events)
        .competitor(&pilot_key)
        .map(|t| t.samples.len())
        .unwrap_or(0);
    assert!(
        coarse_samples >= 1,
        "the live heat should have streamed at least one coarse signal sample"
    );

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

    // === Marshaling path-2 through the PRODUCTION flow: finishing the heat disarms it, which makes
    // the driver stop the RH race (-> DONE) and pull RotorHazard's DENSE per-tick history into the
    // finishing heat's log. Assert a `SignalHistory` lands and the folded trace now carries strictly
    // MORE samples than the coarse stream — the full-fidelity upgrade, activated by the normal
    // staging/finish loop. The savable heat is the one **seated at Stage** (already current), which
    // the finish reuses rather than adding a separate empty heat — not a bespoke marshal-data poke. ===
    let got_dense = wait_for(Duration::from_secs(20), || {
        read_all(&state)
            .iter()
            .any(|e| matches!(e, Event::SignalHistory(_)))
    })
    .await;
    assert!(
        got_dense,
        "the production heat-finish flow must pull RotorHazard's dense SignalHistory into the log \
         (coarse stream was {coarse_samples} samples)"
    );
    let dense_events = read_all(&state);
    let dense_samples = gridfpv_projection::signal_trace(&dense_events)
        .competitor(&pilot_key)
        .map(|t| t.samples.len())
        .expect("dense trace for the lineup pilot");
    // S2 split: with the GridFPV plugin (the path `cargo xtask live` mounts) the dense history is
    // pushed LIVE over `gridfpv_signal` and supersedes the coarse stream *during* the race — the
    // post-race pull is suppressed, so the mid-race "coarse" baseline is already the dense trace and
    // `dense == coarse` is expected. Without the plugin (stock RH fallback, deleted in S3) the
    // DONE-edge save-then-pull yields strictly more samples than the coarse stream.
    if std::env::var_os("GRIDFPV_RH_PLUGIN").is_some() {
        assert!(
            dense_samples >= 1,
            "the live plugin must deliver a dense SignalHistory trace; got {dense_samples}"
        );
    } else {
        assert!(
            dense_samples > coarse_samples,
            "the dense history must supersede the coarse stream with MORE samples: \
             dense={dense_samples} coarse={coarse_samples}"
        );
    }
    eprintln!(
        "app marshaling path-2 (production flow): coarse stream = {coarse_samples} samples, dense \
         history = {dense_samples} samples (full-fidelity upgrade activated by the normal heat loop)"
    );

    // === #250 regression guard: the heat-end dense save must fire EXACTLY ONCE. ===
    //
    // The #250 dense activation re-fired the heat-end `add_heat → set_current_heat → stop_race`
    // dance on every `maintain` re-entry: the burst of emits could drop the socket, and the driver
    // reconnected into a fresh `maintain` whose local guard was reset while the *shared* armed slot
    // was still `finishing` — so it re-ran the dance, looping heat after heat, re-flooding+resetting
    // the link and stopping the live race so NO laps landed. The fix moves the once-only guard into
    // the shared slot (`done`, set before any emit), so a reconnect/re-sent DONE/maintain re-entry
    // never re-runs it. We prove that here three ways:
    //
    //  1. The RH container log shows the heat-save dance ran ONCE, not in a loop. RotorHazard logs
    //     `Current laps saved: ...` per `save_laps`; a loop logs it many times. We allow up to a
    //     small constant for the legitimate single save (and the per-pilotrace pull on older RH),
    //     but a loop would show double-digits. `Heat added` likewise must not repeat per finish.
    let laps_saved = count_log_lines(&rh, "Current laps saved");
    let heats_added = count_log_lines(&rh, "Heat added");
    eprintln!(
        "app #250 guard: RH log shows {laps_saved}x 'Current laps saved', {heats_added}x 'Heat added' \
         (a loop would show these climbing without bound)"
    );
    assert!(
        laps_saved <= 3,
        "the heat-end dense save must fire ONCE, not loop: RH logged 'Current laps saved' \
         {laps_saved} times (the #250 regression flooded the socket with a save-per-reconnect loop)"
    );
    // One finish should add at most one savable heat (plus the heats present from staging). A loop
    // creates a fresh heat every reconnect — climbing without bound.
    assert!(
        heats_added <= 4,
        "the heat-end dense save must add a savable heat ONCE, not loop: RH logged 'Heat added' \
         {heats_added} times (the #250 regression created heat after heat)"
    );

    //  2. Laps actually landed — the live race was NOT interrupted by the finish. The earlier passes
    //     assertion proves laps flowed; re-affirm the run still holds them after the finish (the loop
    //     repeatedly stop_race'd the live heat, so under the regression the count would collapse).
    assert!(
        count_passes(&read_all(&state)) >= 1,
        "laps must still be present after the heat-end save (the #250 loop repeatedly stopped the \
         live race, leaving zero laps)"
    );

    //  3. The connection never flapped during the finish (asserted below: it stays Connected).
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
            seed: 0,
        }),
    )];
    let rh = RhContainer::start(RH_FAILOVER_PORT, TICK, &scenario);

    let registry = test_registry();
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
                ..Default::default()
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
            channel_capability: None,
            node_count: None,
            available_channels: None,
        })
        .expect("create RH timer");

    registry
        .set_active(&event_of(&registry))
        .expect("make the event active");
    // Select [RH, Mock] with the RH as the explicit PRIMARY and the Mock as the alternate.
    registry
        .set_timers(
            &event_of(&registry),
            vec![rh_timer.id.clone(), TimerId(MOCK_TIMER_ID.to_string())],
        )
        .expect("select RH primary + Mock alternate");
    registry
        .set_primary_timer(&event_of(&registry), Some(rh_timer.id.clone()))
        .expect("designate the RH timer primary");

    let state = registry
        .resolve(&event_of(&registry))
        .expect("the event's state");
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

    // Run the heat through the real lifecycle (Scheduled → Staged → Armed → Running); while the RH
    // primary is healthy, its passes feed (the Mock alternate is gated). Staged prepares the RH
    // connection for an instant start (Grid owns all timing).
    let heat = HeatId("q-fo-1".into());
    let pilot = CompetitorRef("Ace".into());
    state
        .append(
            Event::HeatScheduled {
                heat: heat.clone(),
                lineup: vec![pilot.clone()],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
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

/// DISTINCT RH host port for the in-place URL-edit e2e (5042/5043 are taken above).
const RH_URL_EDIT_PORT: u16 = 5044;

/// The wrong address the RD actually types at a venue — right host, nothing listening on that port.
/// Port 1 is privileged and unbound, so the dial is **refused immediately**: a fast, deterministic
/// `Error` rather than a long connect timeout.
const WRONG_URL: &str = "http://127.0.0.1:1";

/// Repointing a **live** RotorHazard timer's URL in place recovers without a restart (issue #382).
///
/// The field failure this guards, verbatim: an RD configures a timer, fat-fingers the address, sees
/// the timer sit in `Error`, and **edits the URL on the existing timer** to the right one — and the
/// Director has to notice. It did not, and the timer stayed dead until the whole app was restarted,
/// which cost a user a debugging session because nothing on screen said "restart me".
///
/// The reconciler's decision for this ([`Step::Supersede`] + re-`Open` when the wanted URL differs
/// from the dialled one) already has in-crate unit coverage in `src/source/rh_connections.rs`; what
/// had **no** coverage is the socket-level end of it — that superseding a dialer stuck in its
/// connect-retry backoff actually cancels it, that the re-open lands on a real RotorHazard, and
/// that the plugin is re-probed on the new address. That is what this asserts, against the live
/// dockerized RH.
///
/// "No restart" is structural here: the `EventRegistry`, the `spawn_registry_bridge` handle, and the
/// timer's own [`TimerId`] are all created **once**, before the bad URL is ever dialled, and are
/// never rebuilt — the only thing that changes between `Error` and `Connected` is the URL on the
/// existing timer record.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (dials a wrong URL, asserts Error, then edits the URL in place and asserts it reaches Connected with no restart)"]
async fn editing_a_live_timers_url_in_place_re_dials_without_a_restart() {
    let scenario = vec![(
        0usize,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 180,
            baseline_rssi: 70,
            seed: 0,
        }),
    )];
    let rh = RhContainer::start(RH_URL_EDIT_PORT, TICK, &scenario);

    // === Everything below is built ONCE. Nothing here is rebuilt after the URL edit. ===
    let registry = test_registry();
    // The timer is created pointing at the WRONG address — the typo, as the RD would save it.
    let rh_timer = registry
        .timers()
        .create(&CreateTimerRequest {
            name: "Field RH".into(),
            kind: TimerKind::Rotorhazard {
                url: WRONG_URL.to_string(),
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        })
        .expect("create RH timer at the wrong URL");
    registry
        .set_active(&event_of(&registry))
        .expect("make the event active");
    registry
        .set_timers(&event_of(&registry), vec![rh_timer.id.clone()])
        .expect("select the RH timer for the event");

    let _bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    // === 1. The wrong URL surfaces as `Error` — the state the RD is staring at. ===
    // The dialer retries with backoff, so it oscillates Connecting → Error; we only need to observe
    // that it *reaches* Error (it never reaches Connected — there is nothing on that port).
    let timers = registry.timers();
    let id = rh_timer.id.clone();
    assert!(
        wait_for(Duration::from_secs(30), || {
            timers.get(&id).map(|t| t.status) == Some(TimerStatus::Error)
        })
        .await,
        "a timer pointed at an unreachable URL must surface TimerStatus::Error; status = {:?}",
        timers.get(&id).map(|t| t.status)
    );
    eprintln!("app url-edit e2e: wrong URL surfaced Error, as the RD would see it");

    // === 2. The RD EDITS THE URL IN PLACE — same timer, same id, no restart. ===
    registry
        .timers()
        .update(
            &rh_timer.id,
            &gridfpv_server::timers::UpdateTimerRequest {
                kind: Some(TimerKind::Rotorhazard {
                    url: rh.url().to_string(),
                }),
                ..Default::default()
            },
        )
        .expect("repoint the existing timer at the real RotorHazard");
    eprintln!(
        "app url-edit e2e: repointed {:?} → {} (in place, no restart)",
        rh_timer.name,
        rh.url()
    );

    // === 3. It re-dials on its own and reaches `Connected` at the new address. ===
    // The reconciler supersedes the failing dialer (cancelling it out of its retry backoff, which
    // caps at 10s) and opens a fresh connection on the edited URL; the container also needs its
    // socket handshake. 60s is generous headroom over that worst case.
    assert!(
        wait_for(Duration::from_secs(60), || {
            timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
        })
        .await,
        "the edited URL must re-dial and reach Connected with no restart; status = {:?}",
        timers.get(&id).map(|t| t.status)
    );

    // The connection is genuinely live at the NEW address, not just a status flag: `Connected` is
    // only published after a real socket connect, and the plugin handshake is re-probed over it.
    assert!(
        wait_for(Duration::from_secs(15), || {
            timers.get(&id).and_then(|t| t.plugin).is_some()
        })
        .await,
        "the plugin must be re-probed over the re-dialled connection; plugin = {:?}",
        timers.get(&id).and_then(|t| t.plugin)
    );

    // The timer is the same record throughout — the recovery was an edit, not a re-create.
    let settled = timers.get(&id).expect("the timer still exists");
    assert_eq!(settled.id, rh_timer.id, "the timer id must be unchanged");
    assert_eq!(
        settled.kind,
        TimerKind::Rotorhazard {
            url: rh.url().to_string()
        },
        "the timer must be dialled at the edited URL"
    );
    eprintln!(
        "app url-edit e2e: {:?} recovered to Connected at {} after an in-place URL edit \
         (no restart) — plugin {:?}",
        settled.name,
        rh.url(),
        settled.plugin
    );
}
