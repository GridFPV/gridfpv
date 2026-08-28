//! Dockerized-RotorHazard **Tune-page write path** e2e (#355, #413, #437, #479).
//!
//! The three write verbs the Tune page owns — `POST /timers/{id}/calibration`,
//! `POST /timers/{id}/channel`, `POST /timers/{id}/capture` — each run the same four-stage pipeline
//! before anything reaches a receiver:
//!
//! ```text
//!   registry queue  →  connection reconciler drain  →  driver dispatch  →  RotorHazard
//!                                                                              ↓
//!                                              GET /timers/{id}/signal  ←  readback
//! ```
//!
//! **Why this target exists (#479).** Every stage of that pipeline had unit coverage and none of it
//! had end-to-end coverage. The #437 dialling-gate is proven open and closed by unit tests, but a
//! unit test cannot see the thing this file exists to check: that a level an RD types actually
//! lands on a real detector. The failure mode is the one CLAUDE.md opens with — RotorHazard does
//! not ack, a handler that raises aborts silently, and every layer above happily reports success.
//! `on_set_frequency` running `int(data['channel'])` unguarded on the catalog's `"R8"` is exactly
//! that bug, and it shipped: the gate stayed on its old frequency while the console said `200`.
//!
//! So each test here **asserts on the readback, never on the dispatch**. A `CalibrationDispatch` /
//! `ChannelDispatch` is a record of what was sent; the assertion is that
//! `GET /timers/{id}/signal` — which is fed by RotorHazard's own heartbeat, not by GridFPV's
//! record — comes back holding what was written.
//!
//! Local-only class (needs Docker), gated behind `--features live` + `#[ignore]`. DISTINCT RH ports
//! 5047/5048/5049, one per test, because the tests in a file run in parallel (connect 5042,
//! failover 5043, no-plugin 5044, restart 5045, min-lap 5046). Run via `cargo xtask live`, or:
//!
//! ```sh
//! cargo test -p gridfpv-app --features live --test rh_tune_write_live -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::Duration;

use gridfpv_app::source::{SIM_ADAPTER, SourceConfig, spawn_registry_bridge};
use gridfpv_events::AdapterId;
use gridfpv_server::events::{CreateEventRequest, EventRegistry};
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{
    CalibrationRequest, CaptureRequest, CaptureResolution, CaptureThreshold, ChannelRequest,
    CreateTimerRequest, NodeSignal, TimerId, TimerKind, TimerStatus,
};
use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};
use tokio::task::JoinHandle;

/// RH host port for the calibration write (connect 5042 … min-lap 5046).
const CALIBRATION_PORT: u16 = 5047;
/// RH host port for the channel write.
const CHANNEL_PORT: u16 = 5048;
/// RH host port for the capture write.
const CAPTURE_PORT: u16 = 5049;

/// CSV tick interval (seconds).
const TICK: &str = "0.1";

/// The node every test writes to. `0` is always present on a stock RotorHazard.
const NODE: u32 = 0;

/// How long to wait for the Director to reach `Connected` on a fresh container.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for a write to traverse queue → drain → dispatch → RotorHazard → heartbeat →
/// readback. Generous: the reconciler ticks on its own schedule and RotorHazard applies the write
/// on its gevent loop, so this bounds a pipeline with three independent cadences in it.
const READBACK_TIMEOUT: Duration = Duration::from_secs(20);

/// Poll `cond` until it holds or `deadline` passes; returns whether it held.
async fn wait_for(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = std::time::Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A connected RotorHazard with the real Director wiring behind it — the fixture all three write
/// verbs are driven against.
struct Bench {
    rh: RhContainer,
    registry: EventRegistry,
    timer: TimerId,
    /// The registry bridge (and, under `live`, the connection reconciler it spawns). Held because
    /// dropping it takes the pipeline under test with it.
    _bridge: JoinHandle<()>,
}

impl Bench {
    /// The timer's current tune signal, **renewing the subscription lease** as a real console poll
    /// does. This is `GET /timers/{id}/signal`, and it is the readback every assertion reads.
    fn signal_node(&self, node: u32) -> Option<NodeSignal> {
        self.registry
            .timers()
            .signal(&self.timer)
            .nodes
            .into_iter()
            .find(|n| n.node == node)
    }
}

/// The id of the registry's single created event.
fn event_of(registry: &EventRegistry) -> EventId {
    let mut list = registry.list();
    assert_eq!(list.len(), 1, "one created event per test registry");
    list.remove(0).id
}

/// Bring up RotorHazard on `port`, configure it as the active event's timer, spawn the real
/// bridge + reconciler, and wait until the Director reports it `Connected` with a live tune
/// subscription open.
async fn bench(port: u16) -> Bench {
    // One node with a realistic pass profile, so the tune feed carries real RSSI rather than a
    // flat line — a capture has nothing to measure on a dead signal.
    let scenario: Vec<(usize, String)> = vec![(
        0,
        node_csv(&NodeCsv {
            ticks_per_lap: 2,
            peak_rssi: 180,
            baseline_rssi: 70,
            seed: 0,
        }),
    )];
    let rh = RhContainer::start(port, TICK, &scenario);

    let registry = EventRegistry::new(None).expect("event registry");
    registry
        .create(&CreateEventRequest::named("Tune writes"))
        .expect("create the event");
    let timer = registry
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
        .expect("create RH timer")
        .id;
    registry
        .set_active(&event_of(&registry))
        .expect("make the event active");
    registry
        .set_timers(&event_of(&registry), vec![timer.clone()])
        .expect("select the RH timer for the event");

    // The real wiring `main` runs — under `live` this also spawns the persistent-connection
    // reconciler, which is the stage that drains the write queue.
    let bridge = spawn_registry_bridge(
        registry.clone(),
        SourceConfig::from_env(),
        AdapterId(SIM_ADAPTER.to_string()),
    );

    let timers = registry.timers();
    let id = timer.clone();
    let connected = wait_for(CONNECT_TIMEOUT, || {
        timers.get(&id).map(|t| t.status) == Some(TimerStatus::Connected)
    })
    .await;
    assert!(
        connected,
        "the Director must connect the selected RH timer before any write can be dispatched; \
         status = {:?}",
        timers.get(&timer).map(|t| t.status)
    );

    let bench = Bench {
        rh,
        registry,
        timer,
        _bridge: bridge,
    };

    // Open the tune subscription and wait for RotorHazard to actually be feeding it. Without this
    // the readback has nothing to read, and a passing assertion would only mean "we never looked".
    let streaming = wait_for(READBACK_TIMEOUT, || {
        let signal = bench.registry.timers().signal(&bench.timer);
        signal.streaming && signal.nodes.iter().any(|n| n.reading.seen)
    })
    .await;
    assert!(
        streaming,
        "the tune subscription must be live before a write is asserted on — RotorHazard's log \
         was:\n{}",
        bench.rh.logs()
    );

    bench
}

/// **A calibration write reaches a real detector** (#355).
///
/// The whole point of D27: an RD's threshold is GridFPV's value, and writing it to the timer is
/// *applying* it. `alter_heat` shipped with no readback and silently dropped laps; a threshold
/// write with no readback loses a gate the same way, only quieter — the console shows the level it
/// sent back to itself while the detector triggers on something else entirely.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (writes both detection thresholds to a real RotorHazard and asserts they come back on the tune feed)"]
async fn a_calibration_write_lands_on_rotorhazard_and_reads_back() {
    let bench = bench(CALIBRATION_PORT).await;

    // Levels well away from RotorHazard's own defaults, so a readback that matches cannot be the
    // value that was already there.
    const ENTER_AT: u32 = 137;
    const EXIT_AT: u32 = 93;

    let dispatch = bench
        .registry
        .timers()
        .request_calibration(
            &bench.timer,
            &CalibrationRequest {
                node: NODE,
                enter_at: Some(ENTER_AT),
                exit_at: Some(EXIT_AT),
            },
            false,
        )
        .expect("a connected RotorHazard timer accepts a calibration write");
    assert_eq!(
        (dispatch.enter_at, dispatch.exit_at),
        (Some(ENTER_AT), Some(EXIT_AT)),
        "the dispatch records what was queued — the assertion that matters is the readback below"
    );

    let landed = wait_for(READBACK_TIMEOUT, || {
        bench.signal_node(NODE).is_some_and(|n| {
            n.reading.enter_at == Some(ENTER_AT as f32) && n.reading.exit_at == Some(EXIT_AT as f32)
        })
    })
    .await;
    assert!(
        landed,
        "both thresholds must come back from RotorHazard's own feed, not from GridFPV's record: a \
         socket emit has no failure signal, so this readback is the only proof the write landed. \
         node = {:?}; RotorHazard's log was:\n{}",
        bench.signal_node(NODE),
        bench.rh.logs()
    );

    // …and GridFPV recorded it as its own (D27), so it survives a reconnect and is re-applied.
    let held = bench.registry.timers().calibration(&bench.timer);
    let node = held
        .iter()
        .find(|c| c.node == NODE)
        .expect("GridFPV holds a calibration record for the node it wrote");
    assert_eq!(
        (node.enter_at, node.exit_at),
        (Some(ENTER_AT), Some(EXIT_AT))
    );

    bench.rh.stop();
}

/// **A channel write reaches a real receiver** (#413, #437).
///
/// This is the verb that already shipped the silent failure CLAUDE.md leads with: `on_set_frequency`
/// runs `int(data['channel'])` unguarded, so sending the catalog's code `"R8"` raised `ValueError`,
/// killed the handler, and left the node on its old frequency while every layer above reported
/// success. The readback is what makes that visible — and this target drives a real band/channel
/// pair, so the label translation is exercised rather than assumed.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (tunes a node to Raceband R7 on a real RotorHazard and asserts the frequency comes back on the tune feed)"]
async fn a_channel_write_lands_on_rotorhazard_and_reads_back() {
    let bench = bench(CHANNEL_PORT).await;

    // Raceband R7. Picked because it is a real catalog entry, so the band/channel labels travel
    // with the write — the exact shape that used to kill RotorHazard's handler.
    const MHZ: u16 = 5880;

    let before = bench
        .signal_node(NODE)
        .and_then(|n| n.reading.frequency_mhz);
    assert_ne!(
        before,
        Some(MHZ),
        "the node must not already be on {MHZ} MHz, or this test would pass without writing \
         anything"
    );

    let dispatch = bench
        .registry
        .timers()
        .request_channel(
            &bench.timer,
            &ChannelRequest {
                node: NODE,
                mhz: MHZ,
                band: Some("Raceband".into()),
                channel: Some("R7".into()),
            },
            false,
        )
        .expect("a connected RotorHazard timer accepts a channel write");
    assert_eq!(dispatch.mhz, MHZ);
    assert_eq!(
        (dispatch.band.as_deref(), dispatch.channel.as_deref()),
        (Some("Raceband"), Some("R7")),
        "the label GridFPV sends is resolved from ITS catalog, never trusted from the wire"
    );

    let landed = wait_for(READBACK_TIMEOUT, || {
        bench
            .signal_node(NODE)
            .is_some_and(|n| n.reading.frequency_mhz == Some(MHZ))
    })
    .await;
    assert!(
        landed,
        "the node must come back tuned to {MHZ} MHz on RotorHazard's own feed. This is the #437 / \
         `\"R8\"` failure mode: a handler that raises leaves the gate on its old frequency while \
         the write reports success. node = {:?}; RotorHazard's log was:\n{}",
        bench.signal_node(NODE),
        bench.rh.logs()
    );

    // GridFPV's own record carries the friendly name it resolved, not the raw MHz alone.
    let held = bench.registry.timers().node_channels(&bench.timer);
    let node = held
        .iter()
        .find(|c| c.node == NODE)
        .expect("GridFPV holds a channel record for the node it tuned");
    assert_eq!(node.mhz, MHZ);
    assert_eq!(
        (node.band.as_deref(), node.channel.as_deref()),
        (Some("Raceband"), Some("R7")),
        "the catalog label is resolved server-side and recorded, so the console shows a name"
    );

    bench.rh.stop();
}

/// **A capture runs end to end and GridFPV can say what it saw** (#355, #446).
///
/// A capture is the longest of the three paths: dispatch, then RotorHazard samples for
/// [`CAPTURE_WINDOW_MS`](gridfpv_server::timers) 3 s, then the captured level arrives unasked on
/// `node_enter_at_level`, and only then can GridFPV resolve what happened.
///
/// The assertion is deliberately **not** "the level changed". RotorHazard refuses a capture (a node
/// that is not answering, one already capturing) by returning `False` and emitting nothing, and a
/// stable gate can legitimately measure the same number twice — #446 is precisely the bug of
/// reporting those two as one. What this target proves is that the pipeline carried the capture and
/// GridFPV was *watching*: anything but [`CaptureResolution::Unobserved`], which is the outcome
/// that means the tune feed carried nothing at all.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires Docker (starts a real capture on RotorHazard and asserts GridFPV observed its outcome rather than timing out unwatched)"]
async fn a_capture_runs_end_to_end_and_is_observed() {
    let bench = bench(CAPTURE_PORT).await;

    // #465: one press arms BOTH halves — enter's window opens now, exit's a delay later.
    let dispatch = bench
        .registry
        .timers()
        .request_capture(&bench.timer, &CaptureRequest { node: NODE }, false)
        .expect("a connected RotorHazard timer accepts a capture");
    assert_eq!(dispatch.node, NODE);
    assert!(
        dispatch.exit_delay_ms > 0,
        "the exit window opens after the enter one — back-to-back over a single pass (#465)"
    );
    assert!(
        bench.registry.timers().capture_in_flight(&bench.timer),
        "the capture is in flight the moment it is accepted — the console shows it as running"
    );

    // Both halves resolve on their own clocks: enter within its window, exit a delay later.
    // `resolve_captures` returns an outcome only once that half is out of time, so poll and
    // collect until both halves have answered rather than sleeping a guessed total.
    let mut enter = None;
    let mut exit = None;
    let both_windows = Duration::from_millis((dispatch.exit_delay_ms + dispatch.window_ms) as u64);
    let resolved = wait_for(READBACK_TIMEOUT + both_windows, || {
        for o in bench.registry.timers().resolve_captures() {
            if o.timer == bench.timer && o.node == NODE {
                match o.threshold {
                    CaptureThreshold::Enter => enter = Some(o),
                    CaptureThreshold::Exit => exit = Some(o),
                }
            }
        }
        enter.is_some() && exit.is_some()
    })
    .await;
    assert!(
        resolved,
        "both capture halves must resolve within their windows plus grace — an unresolved half \
         leaves the console spinning forever. enter resolved = {}, exit resolved = {}; \
         RotorHazard's log was:\n{}",
        enter.is_some(),
        exit.is_some(),
        bench.rh.logs()
    );
    let outcomes = [
        (
            CaptureThreshold::Enter,
            enter.expect("resolved implies enter"),
        ),
        (CaptureThreshold::Exit, exit.expect("resolved implies exit")),
    ];

    // Each half settles independently through #446's three-way resolution — one may be
    // `Measured` while the other is `Unchanged`, and both must have been watched.
    for (half, outcome) in &outcomes {
        assert_ne!(
            outcome.resolution,
            CaptureResolution::Unobserved,
            "GridFPV must have been watching the tune feed across the {half:?} window. \
             `Unobserved` means the subscription was never open or the link dropped — it is the \
             one outcome that indicts this pipeline rather than the gate. outcome = {outcome:?}; \
             RotorHazard's log was:\n{}",
            bench.rh.logs()
        );
        assert!(
            outcome.reported.is_some(),
            "…and an observed capture always knows what the timer is now detecting against, \
             whether or not the level changed (#446): {outcome:?}"
        );
    }

    // `Measured` is the only outcome GridFPV may record as its own (D27) — and when it does, the
    // recorded level must be exactly what it reported seeing, filed under its own half.
    for (half, outcome) in &outcomes {
        if outcome.resolution == CaptureResolution::Measured {
            assert_eq!(
                outcome.level, outcome.reported,
                "a measured capture records the level it observed, never a different one"
            );
            let held = bench.registry.timers().calibration(&bench.timer);
            let recorded = held
                .iter()
                .find(|c| c.node == NODE)
                .and_then(|c| match half {
                    CaptureThreshold::Enter => c.enter_at,
                    CaptureThreshold::Exit => c.exit_at,
                });
            assert_eq!(
                recorded, outcome.level,
                "a measured {half:?} capture becomes GridFPV's own {half:?} threshold (D27)"
            );
        } else {
            assert_eq!(
                outcome.level, None,
                "only a measured capture records anything — an unchanged one cannot attribute \
                 the level to the capture, and adopting a readback as config is what D27 forbids"
            );
        }
    }

    bench.rh.stop();
}
