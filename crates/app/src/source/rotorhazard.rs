//! The live **RotorHazard connection layer** (#65, #105) — only compiled under `--features live`.
//!
//! A RotorHazard timer **connects when it is selected for the active event and stays connected**,
//! so the Director monitors its live link continuously and a drop-off is visible *before and
//! between* races — not only while a heat runs (#105). The earlier slice (#65) connected an RH
//! timer only for the duration of a `Running` heat and tore the socket down when the heat left
//! `Running`; that meant a selected-but-idle timer read `Configured` and a drop could not surface
//! until a race was underway.
//!
//! This module splits that responsibility into two pieces:
//!
//! 1. **The persistent connection** ([`RhConnection`]). One per *(active event, RH timer)* pair.
//!    A dedicated [`spawn_blocking`](tokio::task::spawn_blocking) driver thread connects to the RH
//!    server, drives the timer's [`TimerStatus`](gridfpv_server::timers::TimerStatus) through its
//!    lifecycle (`Connecting → Connected`), then **loops maintaining and monitoring** the link:
//!    on a drop it sets `Disconnected`/`Error` and **reconnects with backoff**, updating the status
//!    across attempts. While connected it continuously drains the translated event stream — when a
//!    heat is "armed" on the connection (see below) it appends the translated passes (remapped onto
//!    the heat lineup) into the event log; otherwise it discards them (idle monitoring). On cancel
//!    (the timer is deselected, the active event changes, or the Director shuts down) it stops any
//!    running race, disconnects, and leaves the timer [`Disconnected`].
//!
//! 2. **Race driving, decoupled from the connection.** A running heat does **not** open a socket
//!    of its own — it *uses the already-live connection*. When a heat enters `Running` the bridge
//!    [arms](RhConnection::arm_heat) the heat on each selected RH connection (stage the RH race +
//!    remap its node seats onto the heat lineup); the driver thread then routes drained passes into
//!    the event log. When the heat leaves `Running` the bridge [disarms](RhConnection::disarm) it —
//!    the race is stopped/cleared but the **connection stays alive** (and keeps reporting status).
//!
//! # Why a dedicated driver thread
//!
//! The RotorHazard transport's emit/poll are **blocking** — they `block_on` an internal runtime —
//! so they must never run on a tokio worker thread (that panics). The entire connection lifecycle
//! (connect → monitor → reconnect → stage → drain → stop → disconnect) therefore runs on one
//! dedicated `spawn_blocking` thread; the async side only holds a handle that flips shared atomics
//! (a cancel flag, and an "armed heat" slot). The driver checks those each loop and reacts on its
//! own thread, so nothing ever emits on a tokio worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::{AdapterId, CompetitorRef, Event};
use gridfpv_server::timers::{TimerId, TimerRegistry, TimerStatus};
use tokio::task::JoinHandle;

use super::PassSink;

/// How often the driver thread drains the RotorHazard connection's translated-event queue.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait, after staging an armed heat, for the RH race to reach RACING before giving up
/// on the wait (the drain loop still runs regardless — this only bounds the staging settle).
const STAGE_SETTLE: Duration = Duration::from_secs(15);

/// The minimum backoff between reconnect attempts after a dropped/failed connection.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(500);

/// The maximum backoff between reconnect attempts (the backoff doubles up to this ceiling).
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(10);

/// How long the connection can drain no events before it probes liveness with a fresh `load_data`.
/// RH pushes asynchronously, so a healthy idle link is silent; the probe distinguishes "idle" from
/// "dropped" without depending on transport-level disconnect callbacks.
const IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// A heat armed onto a live RH connection: the lineup its node seats remap onto, and a flag the
/// driver flips once it has staged the RH race for this arming (so a re-drain doesn't re-stage).
struct ArmedHeat {
    /// The running heat's lineup, in seeding order; node `n`'s passes attribute to `lineup[n]`.
    lineup: Vec<CompetitorRef>,
    /// The sink (the event's log) translated passes are appended through while armed.
    sink: PassSink,
    /// Set by the driver once it has staged the RH race for this arming.
    staged: bool,
}

/// One persistent live RotorHazard connection for a *(active event, RH timer)* pair (#105).
///
/// Owns a dedicated `spawn_blocking` driver thread that connects on construction, maintains and
/// monitors the link (reconnecting with backoff on a drop), and drives a race onto the *existing*
/// connection when a heat is armed. The async handle here only flips shared atomics the driver
/// observes; dropping/[`cancel`](Self::cancel)ling it tears the connection down on the driver
/// thread.
pub struct RhConnection {
    /// The cancel flag the driver polls; flipped on [`cancel`](Self::cancel) / drop.
    cancel: Arc<AtomicBool>,
    /// The armed-heat slot: `Some` while a heat is racing on this connection, else `None`.
    armed: Arc<Mutex<Option<ArmedHeat>>>,
    /// The driver thread's join handle, held so the spawned task is owned by this connection;
    /// teardown is cooperative via the `cancel` flag (the thread is blocking, so it cannot be
    /// aborted) — dropping the connection flips `cancel` and lets the thread exit on its own.
    _driver: JoinHandle<()>,
}

impl RhConnection {
    /// Open a persistent connection for `timer_id` at `url`, publishing status through `timers`.
    ///
    /// Spawns the driver thread immediately: it sets the timer `Connecting`, connects, then
    /// `Connected`, and loops maintaining the link. The connection is idle (monitoring only) until
    /// a heat is [armed](Self::arm_heat) onto it.
    pub fn open(timer_id: TimerId, url: String, timers: TimerRegistry) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let armed: Arc<Mutex<Option<ArmedHeat>>> = Arc::new(Mutex::new(None));
        let driver = {
            let cancel = cancel.clone();
            let armed = armed.clone();
            tokio::task::spawn_blocking(move || {
                drive(url, timer_id, timers, cancel, armed);
            })
        };
        Self {
            cancel,
            armed,
            _driver: driver,
        }
    }

    /// Arm a running heat onto this live connection: the driver stages the RH race and routes its
    /// translated passes (remapped onto `lineup`) into `sink`'s log. Replaces any previously armed
    /// heat (a newer running heat supersedes the prior one).
    pub fn arm_heat(&self, lineup: Vec<CompetitorRef>, sink: PassSink) {
        let mut slot = self.armed.lock().expect("armed-heat lock poisoned");
        *slot = Some(ArmedHeat {
            lineup,
            sink,
            staged: false,
        });
    }

    /// Disarm the current heat (it left `Running`): the driver stops/clears the RH race but the
    /// **connection stays alive** (and keeps reporting status). A no-op if nothing is armed.
    pub fn disarm(&self) {
        let mut slot = self.armed.lock().expect("armed-heat lock poisoned");
        *slot = None;
    }

    /// Tear the connection down: stop any race, disconnect, leave the timer `Disconnected`. Called
    /// when the timer is deselected, the active event changes, or the Director shuts down.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for RhConnection {
    fn drop(&mut self) {
        // A dropped connection (the reconcile map removed it) must still tear down on its thread.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The RH node index `node-{n}` encodes, if any. Passes from the adapter carry the stable node seat
/// handle; we remap it onto the running heat's lineup by this index.
fn node_index(competitor: &CompetitorRef) -> Option<usize> {
    competitor.0.strip_prefix("node-")?.parse().ok()
}

/// Remap one canonical RH [`Event`] onto the heat's lineup and the source adapter id, or `None` to
/// drop it. Only [`Event::Pass`]es feed the lap projection; a pass on node `n` is attributed to
/// `lineup[n]` and re-stamped with `adapter`. Passes for a node outside the lineup (an idle seat)
/// are dropped, as are the adapter's lifecycle / `CompetitorSeen` events (the heat lineup is already
/// established by the control path).
fn remap(event: Event, lineup: &[CompetitorRef], adapter: &AdapterId) -> Option<Event> {
    match event {
        Event::Pass(mut pass) => {
            let index = node_index(&pass.competitor)?;
            let competitor = lineup.get(index)?.clone();
            pass.adapter = adapter.clone();
            pass.competitor = competitor;
            Some(Event::Pass(pass))
        }
        _ => None,
    }
}

/// The persistent driver: connect → `Connected` → maintain/monitor → reconnect on drop, until
/// cancelled, then disconnect and leave `Disconnected` (#105). Runs on a dedicated blocking thread.
fn drive(
    url: String,
    timer_id: TimerId,
    timers: TimerRegistry,
    cancel: Arc<AtomicBool>,
    armed: Arc<Mutex<Option<ArmedHeat>>>,
) {
    let mut backoff = RECONNECT_BACKOFF_MIN;
    while !cancel.load(Ordering::Relaxed) {
        timers.set_status(&timer_id, TimerStatus::Connecting);
        let conn = match RotorHazardConnection::connect(&url, RotorHazardAdapter::new()) {
            Ok(conn) => conn,
            Err(e) => {
                // The connect attempt failed: surface Error, back off, and retry (unless cancelled).
                eprintln!(
                    "gridfpv: RotorHazard connect failed for {:?}: {e}",
                    timer_id.0
                );
                timers.set_status(&timer_id, TimerStatus::Error);
                if sleep_unless_cancelled(backoff, &cancel) {
                    break;
                }
                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                continue;
            }
        };
        timers.set_status(&timer_id, TimerStatus::Connected);
        backoff = RECONNECT_BACKOFF_MIN;

        // Maintain the live link until it drops or we are cancelled.
        let dropped = maintain(&conn, &cancel, &armed);

        // Stop any in-flight race and disconnect cleanly on the way out of this connection.
        conn.stop_race().ok();
        conn.disconnect().ok();

        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // The link dropped (not a cancel): mark Disconnected and reconnect after a short backoff.
        if dropped {
            eprintln!(
                "gridfpv: RotorHazard connection lost for {:?}; reconnecting",
                timer_id.0
            );
            timers.set_status(&timer_id, TimerStatus::Disconnected);
            if sleep_unless_cancelled(backoff, &cancel) {
                break;
            }
            backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
        }
    }
    // Cancelled: leave the timer Disconnected (deselected / event changed / shutdown).
    timers.set_status(&timer_id, TimerStatus::Disconnected);
}

/// Maintain one established connection: drain translated events each tick (routing passes into the
/// armed heat's log, or discarding them while idle), stage a freshly-armed heat, and probe liveness
/// when idle. Returns `true` if the link appears to have **dropped** (so the caller reconnects),
/// `false` if it exited because of cancellation.
fn maintain(
    conn: &RotorHazardConnection,
    cancel: &AtomicBool,
    armed: &Mutex<Option<ArmedHeat>>,
) -> bool {
    let mut last_activity = Instant::now();
    let mut probed_since_activity = false;
    let mut stage_deadline: Option<Instant> = None;

    while !cancel.load(Ordering::Relaxed) {
        // Stage a freshly-armed heat once (reset RH to a clean READY state, then stage; RH
        // auto-starts). Done lazily here so staging happens on the driver thread, not the caller.
        let mut just_staged = false;
        {
            let mut slot = armed.lock().expect("armed-heat lock poisoned");
            if let Some(heat) = slot.as_mut() {
                if !heat.staged {
                    conn.stop_race().ok();
                    conn.discard_laps().ok();
                    // Drop the reset-era event churn so it isn't remapped as race passes.
                    let _ = conn.events();
                    if conn.stage_race().is_err() {
                        // A failed emit on a supposedly-live socket signals a drop.
                        return true;
                    }
                    heat.staged = true;
                    just_staged = true;
                    stage_deadline = Some(Instant::now() + STAGE_SETTLE);
                }
            } else {
                // Nothing armed: clear any stale stage wait.
                stage_deadline = None;
            }
        }
        if just_staged {
            last_activity = Instant::now();
            probed_since_activity = false;
        }

        // Drain whatever the transport has translated since the last tick.
        let drained = conn.events();
        if !drained.is_empty() {
            last_activity = Instant::now();
            probed_since_activity = false;
            let mut slot = armed.lock().expect("armed-heat lock poisoned");
            if let Some(heat) = slot.as_mut() {
                let adapter = heat.sink.adapter().clone();
                let mut saw_start = false;
                for event in drained {
                    if matches!(event, Event::SessionStarted { .. }) {
                        saw_start = true;
                    }
                    if let Some(remapped) = remap(event, &heat.lineup, &adapter) {
                        if heat.sink.append_event(remapped).is_err() {
                            // The log went away (event dropped at shutdown): stop draining.
                            return false;
                        }
                    }
                }
                if saw_start {
                    stage_deadline = None;
                }
            }
            // While idle (nothing armed) the drained events are monitoring-only and discarded.
        } else if stage_deadline.map(|d| Instant::now() >= d).unwrap_or(false) {
            // Gave up waiting for RACING after staging; keep draining steady-state.
            stage_deadline = None;
        } else if last_activity.elapsed() >= IDLE_PROBE_INTERVAL && !probed_since_activity {
            // A quiet link: probe liveness once. A failed emit means the socket has dropped.
            if conn.probe_liveness().is_err() {
                return true;
            }
            probed_since_activity = true;
        }

        if sleep_unless_cancelled(DRAIN_INTERVAL, cancel) {
            return false;
        }
    }
    false
}

/// Sleep `dur`, but wake early (in short slices) if `cancel` flips. Returns `true` if it woke
/// because of cancellation (so callers can stop promptly), `false` if it slept the full duration.
fn sleep_unless_cancelled(dur: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cancel.load(Ordering::Relaxed)
}
