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

/// How long to let RotorHazard settle back to **READY** after the reset (`stop_race` +
/// `discard_laps`) before emitting `stage_race`. RotorHazard runs its socket handlers + the staging
/// countdown on one gevent loop, and emitting the reset and the stage in the same tick makes RH's
/// `on_discard_laps`/`on_stop_race` see the just-set `STAGING` status and abort it ("Stopping race
/// during staging") *before* it reaches RACING — so the timer never replays passes and the heat
/// records zero laps. Empirically the abort is 100% reproducible at 0ms between emits and gone by
/// ~50ms; 300ms is a comfortable margin that is still imperceptible at the start line.
const STAGE_RESET_SETTLE: Duration = Duration::from_millis(300);

/// The minimum backoff between reconnect attempts after a dropped/failed connection.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(500);

/// The maximum backoff between reconnect attempts (the backoff doubles up to this ceiling).
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(10);

/// How long the connection can drain no events before it probes liveness with a fresh `load_data`.
/// RH pushes asynchronously, so a healthy idle link is silent; the probe distinguishes "idle" from
/// "dropped" without depending on transport-level disconnect callbacks.
const IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// A **pending tune** the driver applies on its next loop (race redesign Slice 4a): the per-node
/// `(node_index, frequency_mhz)` assignment the engine allocated for the staging heat, shared from
/// the async [`RhConnection::tune`] caller to the blocking driver thread. `None` ⇒ nothing pending.
type TuneSlot = Arc<Mutex<Option<Vec<(u64, u16)>>>>;

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

/// Render an error together with its full `source()` chain as `"top: cause: root-cause"`.
///
/// `rust_socketio::Error`'s Display is lossy for the connect path: its
/// `IncompleteResponseFromEngineIo(rust_engineio::Error)` variant carries no `{0}`, so it prints the
/// bare string "EngineIO Error" and drops the wrapped engine.io/reqwest cause — a refused TCP
/// connect, a handshake reject, a TLS failure, and a timeout all collapse to that one opaque line.
/// Walking `std::error::Error::source()` recovers the real reason so a connect-failure log is
/// actionable (e.g. distinguishes "RH not running on :5000 (connection refused)" from a genuine
/// handshake regression).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
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
    /// A **pending tune** the driver applies on its next loop (race redesign Slice 4a): the per-node
    /// `(node_index, frequency_mhz)` assignment the engine allocated for the staging heat. Set by
    /// [`tune`](Self::tune) (called when a heat is Staged), drained + emitted on the driver thread
    /// (`set_frequency` per node) so the device tunes its nodes to the assigned channels.
    tune: TuneSlot,
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
        let tune: TuneSlot = Arc::new(Mutex::new(None));
        let driver = {
            let cancel = cancel.clone();
            let armed = armed.clone();
            let tune = tune.clone();
            tokio::task::spawn_blocking(move || {
                drive(url, timer_id, timers, cancel, armed, tune);
            })
        };
        Self {
            cancel,
            armed,
            tune,
            _driver: driver,
        }
    }

    /// **Tune** this connection's nodes to an assigned channel plan (race redesign Slice 4a): the
    /// engine allocates the channels, the adapter applies them (RE §7.3). `assignment` is the
    /// per-node `(node_index, frequency_mhz)` set for the staging heat; the driver thread emits a
    /// `set_frequency` per node on its next loop (best-effort — a failed emit on a dropped link is
    /// logged, not fatal). The bridge calls this when a heat is **Staged**, before it arms/runs.
    pub fn tune(&self, assignment: Vec<(u64, u16)>) {
        let mut slot = self.tune.lock().expect("tune lock poisoned");
        *slot = Some(assignment);
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
/// drop it. [`Event::Pass`]es feed the lap projection and the signal facts
/// ([`Event::SignalChunk`]/[`Event::SignalThresholds`], marshaling Slice 1) feed the signal-trace
/// projection; each is keyed on a node seat (`node-{n}`), attributed to `lineup[n]` and re-stamped
/// with `adapter`. Facts for a node outside the lineup (an idle seat) are dropped, as are the
/// adapter's lifecycle / `CompetitorSeen` events (the heat lineup is already established by the
/// control path).
fn remap(event: Event, lineup: &[CompetitorRef], adapter: &AdapterId) -> Option<Event> {
    match event {
        Event::Pass(mut pass) => {
            let index = node_index(&pass.competitor)?;
            let competitor = lineup.get(index)?.clone();
            pass.adapter = adapter.clone();
            pass.competitor = competitor;
            Some(Event::Pass(pass))
        }
        Event::SignalChunk(mut chunk) => {
            let index = node_index(&chunk.competitor)?;
            chunk.competitor = lineup.get(index)?.clone();
            chunk.adapter = adapter.clone();
            Some(Event::SignalChunk(chunk))
        }
        Event::SignalThresholds(mut t) => {
            let index = node_index(&t.competitor)?;
            t.competitor = lineup.get(index)?.clone();
            t.adapter = adapter.clone();
            Some(Event::SignalThresholds(t))
        }
        Event::SignalHistory(mut h) => {
            // The dense post-race history (RH `current_marshal_data`) is keyed on a node seat exactly
            // like a chunk; remap it onto the heat's lineup pilot so the signal-trace projection's
            // prefer-dense rule supersedes the coarse streamed chunks for the right competitor.
            let index = node_index(&h.competitor)?;
            h.competitor = lineup.get(index)?.clone();
            h.adapter = adapter.clone();
            Some(Event::SignalHistory(h))
        }
        _ => None,
    }
}

/// The persistent driver: connect → `Connected` → maintain/monitor → reconnect on drop, until
/// cancelled, then disconnect and leave `Disconnected` (#105). Runs on a dedicated blocking thread.
#[allow(clippy::too_many_arguments)]
fn drive(
    url: String,
    timer_id: TimerId,
    timers: TimerRegistry,
    cancel: Arc<AtomicBool>,
    armed: Arc<Mutex<Option<ArmedHeat>>>,
    tune: TuneSlot,
) {
    let mut backoff = RECONNECT_BACKOFF_MIN;
    // The adapter is created **once** and reused across every (re)connection (#105). Its dedup +
    // `last_race_status` must be continuous across a reconnect: on a mid-race drop the running heat
    // stays `staged` (it lives in the shared `armed` Mutex, not the adapter), so the staging block
    // below does NOT reset RH, and RotorHazard re-sends the in-progress `current_laps` snapshot on
    // the new socket. A *fresh* adapter (empty dedup) would re-emit every replayed lap as a Pass,
    // and the lap projection — which does not dedup by sequence — would turn those into duplicate
    // laps (double-count). Reusing the adapter keeps that snapshot deduped.
    //
    // Combined invariant with #156 (the RACING-transition dedup reset):
    //   * Mid-race reconnect: the adapter persists, so `last_race_status == RACING`. RH's re-sent
    //     `race_status=RACING` is NOT a transition (`previous == Some(RACING)`) → no SessionStarted
    //     re-emit, no #156 reset → the re-sent `current_laps` are deduped (no double-count). ✓
    //   * New race / cross-heat: a real READY/DONE→RACING transition DOES fire #156, resetting dedup
    //     so the next heat (whose lap_number restarts at 0) ingests its own fresh laps. ✓
    let mut carry_adapter = Some(RotorHazardAdapter::new());
    while !cancel.load(Ordering::Relaxed) {
        timers.set_status(&timer_id, TimerStatus::Connecting);
        // Reuse the carried adapter (preserving dedup/last_race_status across reconnects); only on
        // the first attempt is it `Some` from above — every later iteration re-seeds it from the
        // adapter recovered out of the previous connection's `disconnect`.
        let adapter = carry_adapter.take().unwrap_or_else(RotorHazardAdapter::new);
        let conn = match RotorHazardConnection::connect(&url, adapter) {
            Ok(conn) => conn,
            Err(e) => {
                // The connect attempt failed: surface Error, back off, and retry (unless cancelled).
                // The adapter was consumed by the failed `connect`; start the next attempt fresh.
                // (A connect failure means no socket and no replayed snapshot, so there is nothing
                // to dedup against — a fresh adapter is correct and #156 re-seeds on the next race.)
                //
                // Log the full error *chain*, not just `rust_socketio`'s top-level Display: its
                // `IncompleteResponseFromEngineIo` variant renders as the bare, useless string
                // "EngineIO Error" (no `{0}`), which hides the actual cause — a refused TCP connect
                // (RH not running / wrong port), an engine.io handshake reject, a TLS fault, or a
                // timeout all collapse to the same opaque line. `error_chain` walks `source()` so the
                // log tells a dead `:5000` apart from a genuine handshake failure at a glance.
                eprintln!(
                    "gridfpv: RotorHazard connect failed for {:?}: {}",
                    timer_id.0,
                    error_chain(&e)
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
        let dropped = maintain(&conn, &cancel, &armed, &tune);

        // Stop any in-flight race and disconnect on the way out of this connection. `disconnect`
        // returns the adapter so the next reconnect reuses its dedup state (the #105 fix).
        conn.stop_race().ok();
        carry_adapter = Some(conn.disconnect());

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
    tune: &Mutex<Option<Vec<(u64, u16)>>>,
) -> bool {
    let mut last_activity = Instant::now();
    let mut probed_since_activity = false;
    let mut stage_deadline: Option<Instant> = None;

    while !cancel.load(Ordering::Relaxed) {
        // The source of truth for a drop (#105): `rust_socketio` runs with `.reconnect(false)`, so a
        // dropped socket fires the transport's `close`/`error` handlers, which flip `is_alive` to
        // false. (An emit alone can't be trusted — a buffering client returns `Ok` on a dead link.)
        if !conn.is_alive() {
            return true;
        }

        // Apply a pending tune (race redesign Slice 4a): the bridge requested the device tune its
        // nodes to the staging heat's assigned channels. Emit a `set_frequency` per node; this is
        // best-effort (the engine has already allocated — applying is the adapter's half), so a
        // failed emit on a supposedly-live socket signals a drop the caller reconnects from.
        let pending_tune = tune.lock().expect("tune lock poisoned").take();
        if let Some(assignment) = pending_tune {
            for (node, mhz) in assignment {
                if conn.set_frequency(node, mhz).is_err() {
                    return true;
                }
            }
        }

        // Stage a freshly-armed heat once (reset RH to a clean READY state, then stage; RH
        // auto-starts). Done lazily here so staging happens on the driver thread, not the caller.
        //
        // The reset (`stop_race` + `discard_laps`) and the `stage_race` MUST NOT be emitted
        // back-to-back: RotorHazard processes socket emits on a gevent loop, and `stage_race`'s
        // STAGING→RACING transition runs as a non-blocking countdown greenlet. When the reset
        // emits land in the same gevent tick as `stage_race`, RH's `on_discard_laps`/`on_stop_race`
        // observes the just-set `STAGING` status and calls `on_stop_race()` — logging "Stopping
        // race during staging" and dropping RH back to READY *before* it ever reaches RACING. A
        // timer that never reaches RACING never replays/records any passes, so the heat produces
        // **zero laps** (the no-laps symptom). Empirically the hazard is 100% reproducible with 0ms
        // between emits and 0% with a ≥50ms gap; we settle generously between the reset and the
        // stage so RH has fully returned to READY first.
        let mut just_staged = false;
        let do_stage = {
            let slot = armed.lock().expect("armed-heat lock poisoned");
            matches!(slot.as_ref(), Some(heat) if !heat.staged)
        };
        if do_stage {
            // Reset RH to a clean READY state. (`stop_race` first in case a prior heat is still
            // STAGING/RACING; `discard_laps` clears any stale laps and forces READY.)
            conn.stop_race().ok();
            conn.discard_laps().ok();
            // Let RotorHazard settle back to READY before staging — see the hazard note above. The
            // sleep wakes early on cancel so teardown stays prompt.
            if sleep_unless_cancelled(STAGE_RESET_SETTLE, cancel) {
                return false;
            }
            // Drop the reset-era event churn so it isn't remapped as race passes.
            let _ = conn.events();
            if conn.stage_race().is_err() {
                // A failed emit on a supposedly-live socket signals a drop.
                return true;
            }
            // Mark the (still-armed) heat staged. A concurrent disarm/re-arm between the check above
            // and here is benign: a re-arm reset `staged` to false (we re-stage next loop), a disarm
            // cleared the slot (nothing to mark).
            let mut slot = armed.lock().expect("armed-heat lock poisoned");
            if let Some(heat) = slot.as_mut() {
                heat.staged = true;
            }
            just_staged = true;
            stage_deadline = Some(Instant::now() + STAGE_SETTLE);
        } else if armed.lock().expect("armed-heat lock poisoned").is_none() {
            // Nothing armed: clear any stale stage wait.
            stage_deadline = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{SignalChunk, SignalThresholds, SourceTime};

    fn lineup() -> Vec<CompetitorRef> {
        vec![CompetitorRef("Ace".into()), CompetitorRef("Bee".into())]
    }

    #[test]
    fn remap_attributes_signal_chunk_to_the_lineup_pilot() {
        // A trace chunk on node-1 is re-attributed to lineup[1] and re-stamped with the adapter id,
        // exactly like a pass — so the signal-trace projection groups it under the right pilot.
        let adapter = AdapterId("timer-7".into());
        let chunk = Event::SignalChunk(SignalChunk {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("node-1".into()),
            from: SourceTime::from_micros(0),
            period_micros: 100_000,
            rssi: vec![70, 150],
        });
        match remap(chunk, &lineup(), &adapter) {
            Some(Event::SignalChunk(c)) => {
                assert_eq!(c.competitor, CompetitorRef("Bee".into()));
                assert_eq!(c.adapter, adapter);
                assert_eq!(c.rssi, vec![70, 150]);
            }
            other => panic!("expected a remapped SignalChunk, got {other:?}"),
        }
    }

    #[test]
    fn remap_attributes_thresholds_and_drops_off_lineup_nodes() {
        let adapter = AdapterId("timer-7".into());
        let t = Event::SignalThresholds(SignalThresholds {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("node-0".into()),
            enter: 90,
            exit: 80,
        });
        match remap(t, &lineup(), &adapter) {
            Some(Event::SignalThresholds(t)) => {
                assert_eq!(t.competitor, CompetitorRef("Ace".into()));
                assert_eq!(t.adapter, adapter);
            }
            other => panic!("expected remapped SignalThresholds, got {other:?}"),
        }
        // A node beyond the (2-seat) lineup is dropped, like an idle-seat pass.
        let off = Event::SignalChunk(SignalChunk {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("node-5".into()),
            from: SourceTime::from_micros(0),
            period_micros: 100_000,
            rssi: vec![0],
        });
        assert!(remap(off, &lineup(), &adapter).is_none());
    }

    /// A failed connect to a dead port must produce an *actionable* log: the bare top-level
    /// `rust_socketio` Display is "EngineIO Error" (hides the cause), but `error_chain` walks the
    /// `source()` chain down to the real reason (here: the refused TCP connect). This is the
    /// regression-diagnosis fix — a dead `:5000` no longer looks identical to a handshake failure.
    #[test]
    fn error_chain_surfaces_the_real_connect_cause() {
        // Port 1 is reserved/unused on the loopback, so the connect is refused immediately.
        // (`RotorHazardConnection` isn't `Debug`, so match rather than `expect_err`.)
        let err =
            match RotorHazardConnection::connect("http://127.0.0.1:1", RotorHazardAdapter::new()) {
                Ok(_) => panic!("connecting to a dead port must fail"),
                Err(e) => e,
            };
        let chained = error_chain(&err);
        // The top-level Display alone is the useless opaque string...
        assert_eq!(err.to_string(), "EngineIO Error");
        // ...but the chain recovers the underlying cause (refused / no connection).
        assert!(
            chained.len() > "EngineIO Error".len(),
            "error_chain must add the underlying cause, got {chained:?}"
        );
        let lower = chained.to_lowercase();
        assert!(
            lower.contains("refused") || lower.contains("connect"),
            "error_chain should name the refused connect, got {chained:?}"
        );
    }
}
