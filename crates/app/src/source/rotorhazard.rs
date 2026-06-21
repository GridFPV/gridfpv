//! The live **RotorHazard lap source** (#65, Slice 2b) — only compiled under `--features live`.
//!
//! This is the real counterpart to the [`SimSource`](super::SimSource): when an event selects a
//! [`Rotorhazard { url }`](gridfpv_server::timers::TimerKind::Rotorhazard) timer and a heat goes
//! `Running`, the per-event bridge ([`run_bridge`](super::run_bridge)) drives an [`RhSource`] for
//! it, exactly as it drives a `SimSource` for a Mock timer. The `RhSource`:
//!
//! 1. **Connects** to the RH server at `url` through the adapters crate's feature-gated
//!    [`RotorHazardConnection`] (a Socket.IO transport behind the pure
//!    [`RotorHazardAdapter`](gridfpv_adapters::rotorhazard::RotorHazardAdapter) translator).
//! 2. **Drives the race**: resets RH to a clean state, stages it, then polls the connection's
//!    translated [`Event`] stream and **feeds the passes into the event's log** (the same log the
//!    Mock bridge appends to, so `/stream` wakes and the console animates). RH node seats
//!    (`node-0`, `node-1`, …) are remapped onto the running heat's lineup by index, so passes
//!    attribute to the heat's actual competitors.
//! 3. **Tears down** on cancellation — when the heat leaves `Running` the bridge drops this
//!    future; a cancel flag tells the driver thread to stop the RH race and disconnect.
//!
//! Throughout, it updates the timer's **live connection status** in the
//! [`TimerRegistry`](gridfpv_server::timers::TimerRegistry):
//! [`Connecting`](gridfpv_server::timers::TimerStatus::Connecting) →
//! [`Connected`](gridfpv_server::timers::TimerStatus::Connected) →
//! [`Disconnected`](gridfpv_server::timers::TimerStatus::Disconnected) (clean teardown) or
//! [`Error`](gridfpv_server::timers::TimerStatus::Error) (the connect failed). A fresh `GET /timers`
//! reflects the current value, so the console can poll a drop-off.
//!
//! # Why a dedicated driver thread
//!
//! The RotorHazard transport's emit/poll are **blocking** — they `block_on` an internal runtime —
//! so they must never run on a tokio worker thread (that panics). The entire connection lifecycle
//! (connect → reset → stage → drain → stop → disconnect) therefore runs on one dedicated
//! [`spawn_blocking`](tokio::task::spawn_blocking) thread; the async `run_heat` future only awaits
//! it and flips a shared **cancel flag** when the bridge drops it. The driver checks that flag each
//! loop and tears down cleanly on its own thread, so an aborted future never emits on the runtime.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::RotorHazardConnection;
use gridfpv_events::{AdapterId, CompetitorRef, Event};
use gridfpv_server::timers::{TimerId, TimerRegistry, TimerStatus};

use super::{HeatRun, LapSource, PassSink, SourceError};

/// How often the driver thread drains the RotorHazard connection's translated-event queue and
/// appends any new passes to the log.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait, after staging, for the RH race to reach RACING before giving up on the wait
/// (the drain loop still runs regardless — this only bounds the staging settle).
const STAGE_SETTLE: Duration = Duration::from_secs(15);

/// A live RotorHazard lap source (feature `live`): connects to one RH server and feeds its passes
/// into the running heat's log, maintaining the timer's connection status. See the [module
/// docs](self).
pub struct RhSource {
    /// The selected timer's id — the handle whose [`TimerStatus`] this source drives.
    timer_id: TimerId,
    /// The RH server base URL to dial (e.g. `http://rotorhazard.local:5000`).
    url: String,
    /// The app-level timer registry, so the source can publish its live connection status.
    timers: TimerRegistry,
}

impl RhSource {
    /// A live RH source for `timer_id` pointed at `url`, publishing status through `timers`.
    pub fn new(timer_id: TimerId, url: String, timers: TimerRegistry) -> Self {
        Self {
            timer_id,
            url,
            timers,
        }
    }

    /// The RH node index `node-{n}` encodes, if any. Passes from the adapter carry the stable node
    /// seat handle; we remap it onto the running heat's lineup by this index.
    fn node_index(competitor: &CompetitorRef) -> Option<usize> {
        competitor.0.strip_prefix("node-")?.parse().ok()
    }

    /// Remap one canonical RH [`Event`] onto the heat's lineup and this source's adapter id, or
    /// `None` to drop it. Only [`Event::Pass`]es feed the lap projection here; a pass on node `n`
    /// is attributed to `lineup[n]` (its actual competitor) and re-stamped with `adapter`. Passes
    /// for a node outside the lineup (an idle seat) are dropped. The adapter's lifecycle /
    /// `CompetitorSeen` events are not appended — the heat's lineup is already established by the
    /// control path, and the bridge owns the heat lifecycle.
    fn remap(event: Event, lineup: &[CompetitorRef], adapter: &AdapterId) -> Option<Event> {
        match event {
            Event::Pass(mut pass) => {
                let index = Self::node_index(&pass.competitor)?;
                let competitor = lineup.get(index)?.clone();
                pass.adapter = adapter.clone();
                pass.competitor = competitor;
                Some(Event::Pass(pass))
            }
            _ => None,
        }
    }

    /// The blocking driver: the whole connection lifecycle on one dedicated thread (see the module
    /// docs on why this must not run on a tokio worker). Runs until `cancel` is set, then tears
    /// the connection down and marks the timer `Disconnected`. Returns the last error, if any.
    fn drive(
        url: String,
        timer_id: TimerId,
        timers: TimerRegistry,
        sink: PassSink,
        lineup: Vec<CompetitorRef>,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), SourceError> {
        timers.set_status(&timer_id, TimerStatus::Connecting);

        let conn = match RotorHazardConnection::connect(&url, RotorHazardAdapter::new()) {
            Ok(conn) => conn,
            Err(e) => {
                timers.set_status(&timer_id, TimerStatus::Error);
                return Err(SourceError(format!("RotorHazard connect failed: {e}")));
            }
        };
        timers.set_status(&timer_id, TimerStatus::Connected);

        // From here the connection is up: whatever happens, leave the timer Disconnected and stop
        // the RH race on the way out.
        let result = Self::drive_connected(&conn, &sink, &lineup, &cancel);

        conn.stop_race().ok();
        conn.disconnect().ok();
        timers.set_status(&timer_id, TimerStatus::Disconnected);
        result
    }

    /// The connected phase: reset → stage → drain passes into the log until cancelled.
    fn drive_connected(
        conn: &RotorHazardConnection,
        sink: &PassSink,
        lineup: &[CompetitorRef],
        cancel: &AtomicBool,
    ) -> Result<(), SourceError> {
        // Reset RH to a clean READY state (a prior DONE race blocks staging), drop the reset-era
        // event churn, then stage (RH auto-starts).
        conn.stop_race().ok();
        conn.discard_laps().ok();
        Self::sleep_unless_cancelled(Duration::from_millis(400), cancel);
        let _ = conn.events();
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _ = conn.stage_race();

        // Wait (bounded) for RACING so the first passes are real race passes, but keep draining
        // throughout so nothing is dropped while we wait.
        let adapter = sink.adapter().clone();
        let deadline = Instant::now() + STAGE_SETTLE;
        let mut started = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            for event in conn.events() {
                if matches!(event, Event::SessionStarted { .. }) {
                    started = true;
                }
                if let Some(remapped) = Self::remap(event, lineup, &adapter) {
                    sink.append_event(remapped)?;
                }
            }
            if started || Instant::now() >= deadline {
                break;
            }
            Self::sleep_unless_cancelled(DRAIN_INTERVAL, cancel);
        }

        // Steady-state drain until the bridge cancels us (the heat left Running).
        while !cancel.load(Ordering::Relaxed) {
            for event in conn.events() {
                if let Some(remapped) = Self::remap(event, lineup, &adapter) {
                    sink.append_event(remapped)?;
                }
            }
            Self::sleep_unless_cancelled(DRAIN_INTERVAL, cancel);
        }
        Ok(())
    }

    /// Sleep `dur`, but wake early (in short slices) if `cancel` flips, so teardown is prompt.
    fn sleep_unless_cancelled(dur: Duration, cancel: &AtomicBool) {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl LapSource for RhSource {
    fn run_heat(
        &self,
        run: HeatRun,
        sink: PassSink,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), SourceError>> + Send>> {
        let timer_id = self.timer_id.clone();
        let url = self.url.clone();
        let timers = self.timers.clone();
        Box::pin(async move {
            // A shared cancel flag: the driver runs on a blocking thread; this future flips the
            // flag when it is dropped (the bridge aborts it on the heat leaving Running) so the
            // driver tears down on its own thread — never emitting on a tokio worker.
            let cancel = Arc::new(AtomicBool::new(false));
            let _guard = CancelOnDrop(cancel.clone());

            let lineup = run.lineup.clone();
            let driver = tokio::task::spawn_blocking(move || {
                Self::drive(url, timer_id, timers, sink, lineup, cancel)
            });

            // Await the driver. If THIS future is dropped (cancelled), `_guard` flips the flag and
            // the detached driver thread tears down cleanly; we simply never observe its result.
            match driver.await {
                Ok(result) => result,
                Err(join) => Err(SourceError(format!(
                    "RotorHazard driver task failed: {join}"
                ))),
            }
        })
    }
}

/// Flips a cancel flag when dropped — the async future's hook to stop the blocking driver thread
/// on cancellation (the bridge aborting the heat task) without emitting on the runtime.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
