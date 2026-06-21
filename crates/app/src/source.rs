//! The Director's built-in **lap source + control→source bridge** (#13, v0.4).
//!
//! This is the piece that makes "click **Start** → see a race" work with *nothing*
//! installed — no Docker, no hardware, no second process. When the RD drives a heat to
//! `Running` through the control path, the bridge generates synthetic lap-gate passes for
//! that heat's lineup over **real time**, appends them to the one append-only log via
//! [`AppState::append`] (so `/stream` wakes and the console animates), and stops when the
//! heat `Finishes`/`Aborts`/`Scores` (or a newer heat takes the timer).
//!
//! # The seam — [`LapSource`]
//!
//! A lap source is anything that, told a heat went `Running` with a known lineup, *emits
//! lap-gate passes for that heat over time*. The bridge owns the timer/cancellation and
//! the log; a source only decides **what passes to emit and when**:
//!
//! ```ignore
//! trait LapSource {
//!     async fn run_heat(&self, heat: HeatRun, sink: &PassSink) -> Result<(), SourceError>;
//! }
//! ```
//!
//! The bridge calls [`LapSource::run_heat`] inside a cancellable task; the source sleeps
//! between passes and pushes each one through the [`PassSink`] (which appends to the log).
//! Cancellation is cooperative — the bridge drops the future on a `Finished`/`Aborted`
//! transition, so a source must only `.await` between passes (it does, on the sink push
//! and on `sleep`) for the cancel to land promptly.
//!
//! The only concrete source today is [`SimSource`] (pure Rust, deterministic-enough). A
//! **real RotorHazard source** slots in behind the very same trait later: it would connect
//! to an RH server, map the heat's lineup onto RH node seats, and translate RH lap
//! callbacks into [`PassSink::emit`] calls — feature-gated so the default Director stays
//! openssl-free (the sim pulls no network/TLS at all). Nothing in the bridge changes.
//!
//! # How the bridge observes transitions
//!
//! The `/stream` append-notify ([`Notify`](tokio::sync::Notify)) is `pub(crate)` to the
//! server crate, so from the app crate the clean cross-crate option is to **poll the log
//! tail**: the bridge advances an [`Offset`] cursor over
//! [`read_from`](gridfpv_server::app::EventSource::read_from) on a short interval
//! ([`POLL_INTERVAL`]) and reacts to the `HeatScheduled` / `HeatStateChanged` events it
//! sees. On a `Running` transition it looks back for that heat's lineup (its
//! `HeatScheduled`) and spawns the source task; on `Finished`/`Aborted`/`Scored`/`Restarted`
//! for the running heat — or a *different* heat going `Running` — it cancels the task. At
//! most one heat emits at a time (a single in-flight task), which is plenty for a Director
//! driving one timer.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, HeatId, HeatTransition, Pass, SourceTime,
};
use gridfpv_server::app::AppState;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{TimerKind, TimerRegistry};
use gridfpv_storage::Offset;
use tokio::task::JoinHandle;

#[cfg(feature = "live")]
mod rh_connections;
#[cfg(feature = "live")]
mod rotorhazard;
#[cfg(feature = "live")]
pub use rh_connections::{RhConnections, spawn_rh_reconciler};

/// How often the bridge polls the log tail for new heat-loop events. Short enough that a
/// `Start` click feels instant (the first pass lands within a poll), long enough to be a
/// negligible idle cost. A real source would use the RH event callback instead of polling.
pub const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// The adapter id every sim-generated pass carries, so the lap projection groups them
/// under one synthetic source. A real RH source would use its own adapter id.
pub const SIM_ADAPTER: &str = "sim";

/// Default number of laps each sim pilot flies (beyond the holeshot). Overridable via
/// `GRIDFPV_SIM_LAPS`.
pub const DEFAULT_SIM_LAPS: u32 = 5;

/// Default real-time pace of a sim lap, in milliseconds. Overridable via
/// `GRIDFPV_SIM_LAP_MS`. The console animates at this cadence; passes are spaced this far
/// apart in real time (with mild per-pilot variation).
pub const DEFAULT_SIM_LAP_MS: u64 = 2500;

// --- the seam -------------------------------------------------------------------------

/// One heat handed to a [`LapSource`]: which heat, and its lineup (in seeding order).
#[derive(Debug, Clone)]
pub struct HeatRun {
    /// The heat that just went `Running`.
    pub heat: HeatId,
    /// The competitors to emit passes for (the heat's `HeatScheduled` lineup).
    pub lineup: Vec<CompetitorRef>,
}

/// The append surface a [`LapSource`] pushes passes through. Wraps the shared
/// [`AppState`] so every emitted pass lands in the one log and wakes `/stream`.
#[derive(Clone)]
pub struct PassSink {
    state: AppState,
    adapter: AdapterId,
}

impl PassSink {
    /// A sink over `state` tagging passes with `adapter`.
    pub fn new(state: AppState, adapter: AdapterId) -> Self {
        Self { state, adapter }
    }

    /// Emit one lap-gate pass for `competitor` at race-relative time `at` (ms since the
    /// race start, like RH), with a per-pilot monotonic `sequence`. Appends through
    /// [`AppState::append`] so the live state updates and `/stream` wakes.
    pub fn emit(
        &self,
        competitor: &CompetitorRef,
        at: SourceTime,
        sequence: u64,
    ) -> Result<(), SourceError> {
        let pass = Event::Pass(Pass {
            adapter: self.adapter.clone(),
            competitor: competitor.clone(),
            at,
            sequence: Some(sequence),
            gate: GateIndex::LAP,
            signal: None,
        });
        self.state
            .append(pass, None)
            .map_err(|e| SourceError(format!("{e:?}")))?;
        Ok(())
    }

    /// Append an already-built canonical [`Event`] through this sink's [`AppState`], stamping
    /// passes with the sink's `adapter` id. Used by the live RotorHazard source to feed the
    /// adapter's translated passes (which carry their own real signal context, source-clock
    /// timestamps and per-node sequence) straight into the event log — rather than re-synthesizing
    /// them through [`emit`](Self::emit). Returns the resulting [`Offset`] on success.
    #[cfg(feature = "live")]
    pub(crate) fn append_event(&self, event: Event) -> Result<(), SourceError> {
        self.state
            .append(event, None)
            .map_err(|e| SourceError(format!("{e:?}")))?;
        Ok(())
    }

    /// This sink's adapter id (the configured RH timer's adapter), for re-stamping translated
    /// passes onto a single source in the lap projection.
    #[cfg(feature = "live")]
    pub(crate) fn adapter(&self) -> &AdapterId {
        &self.adapter
    }
}

/// A lap source: emits lap-gate passes for one running heat over time.
///
/// Implementors own *what* passes to emit and *when* (sleeping between them); the bridge
/// owns the log handle (via the [`PassSink`]) and the task lifecycle (spawn on `Running`,
/// cancel on `Finished`/`Aborted`). The future is dropped to cancel, so implementors must
/// only hold state across `.await` points that are safe to abandon mid-flight (a partly
/// emitted heat just stops — the log keeps whatever passes already landed).
pub trait LapSource: Send + Sync + 'static {
    /// Drive `run` to completion, pushing each pass through `sink`. Returns when the heat's
    /// synthetic passes are exhausted (or on error); may be cancelled early by the bridge
    /// dropping the returned future.
    fn run_heat(
        &self,
        run: HeatRun,
        sink: PassSink,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SourceError>> + Send>>;
}

/// An error from a lap source — today only "the log append failed" (a poisoned lock or a
/// storage error), surfaced so the bridge can log it and stop the heat.
#[derive(Debug, Clone)]
pub struct SourceError(pub String);

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lap source error: {}", self.0)
    }
}

impl std::error::Error for SourceError {}

// --- the sim source -------------------------------------------------------------------

/// The built-in synthetic lap source: a holeshot plus `laps` laps per pilot at `lap`
/// real-time pace, with mild deterministic per-pilot variation so the running order is not
/// a flat tie.
///
/// Per pilot it emits `laps + 1` lap-gate passes: the **holeshot** (the first pass, which
/// starts the pilot's clock at race-relative `0` plus a small stagger) then one pass per
/// lap. `Pass.at` is the race-relative [`SourceTime`] in microseconds — milliseconds since
/// the race start, mirroring how RotorHazard reports lap times on a per-race clock.
///
/// Pacing is **real time**: between passes the source sleeps the (varied) lap duration so
/// the console animates laps ticking in. Tests inject a tiny `lap` (e.g. 1ms) so the whole
/// heat runs in well under a second with no special clock plumbing.
#[derive(Debug, Clone)]
pub struct SimSource {
    /// Laps each pilot flies beyond the holeshot.
    pub laps: u32,
    /// The nominal real-time pace of one lap.
    pub lap: Duration,
}

impl SimSource {
    /// A sim source with explicit knobs.
    pub fn new(laps: u32, lap: Duration) -> Self {
        Self { laps, lap }
    }

    /// Build from the environment knobs (`GRIDFPV_SIM_LAPS`, `GRIDFPV_SIM_LAP_MS`),
    /// falling back to [`DEFAULT_SIM_LAPS`] / [`DEFAULT_SIM_LAP_MS`] when unset or
    /// unparseable.
    pub fn from_env() -> Self {
        let laps = parse_env_u32("GRIDFPV_SIM_LAPS").unwrap_or(DEFAULT_SIM_LAPS);
        let lap_ms = parse_env_u64("GRIDFPV_SIM_LAP_MS").unwrap_or(DEFAULT_SIM_LAP_MS);
        Self::new(laps, Duration::from_millis(lap_ms))
    }

    /// The per-pilot lap pace: the nominal pace scaled by a small deterministic factor
    /// derived from the pilot's index, so pilots don't all cross in lockstep and the
    /// running order is decided. Index 0 is the nominal pace; later pilots are a few
    /// percent slower, spreading the field.
    fn pilot_lap(&self, pilot_index: usize) -> Duration {
        // +4% per seed position, capped so the spread stays modest even for big fields.
        let pct = 4u32.saturating_mul(pilot_index.min(8) as u32);
        let scaled = self.lap.as_micros() as u64 * (100 + pct as u64) / 100;
        Duration::from_micros(scaled)
    }
}

impl LapSource for SimSource {
    fn run_heat(
        &self,
        run: HeatRun,
        sink: PassSink,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SourceError>> + Send>> {
        let this = self.clone();
        Box::pin(async move {
            // Per-pilot race-relative clock (µs) and monotonic sequence. Each pilot is
            // driven independently so their passes interleave by real wall-clock time —
            // the bridge appends whichever lands first.
            //
            // A simple serialized timer: walk lap-by-lap across all pilots, sleeping the
            // pace before each pilot's next pass. The holeshot (lap 0) opens each pilot's
            // clock with a small stagger so seeds don't tie at exactly 0.
            let mut clock_micros: Vec<i64> = vec![0; run.lineup.len()];
            let mut sequence: Vec<u64> = vec![0; run.lineup.len()];

            // Holeshots: stagger seeds by ~one tenth of a lap so the start order is the
            // seeding order (and times are distinct).
            for (i, competitor) in run.lineup.iter().enumerate() {
                let stagger = this.pilot_lap(i).as_micros() as i64 / 10 * i as i64;
                clock_micros[i] = stagger;
                sink.emit(competitor, SourceTime::from_micros(stagger), sequence[i])?;
            }

            // Then `laps` laps: each lap, every pilot crosses once, paced in real time.
            for _lap in 0..this.laps {
                for (i, competitor) in run.lineup.iter().enumerate() {
                    let lap = this.pilot_lap(i);
                    tokio::time::sleep(lap).await;
                    clock_micros[i] += lap.as_micros() as i64;
                    sequence[i] += 1;
                    sink.emit(
                        competitor,
                        SourceTime::from_micros(clock_micros[i]),
                        sequence[i],
                    )?;
                }
            }
            Ok(())
        })
    }
}

// --- the bridge -----------------------------------------------------------------------

/// The configured lap source for the Director, selected by `GRIDFPV_SOURCE`.
///
/// `sim` (the default) is the only source implemented here. `rh:<url>` is **reserved** for
/// the later feature-gated RotorHazard source; selecting it today logs an "unsupported,
/// using sim" line and falls back to the sim, so the env var is forward-compatible.
pub enum SourceConfig {
    /// The built-in synthetic source.
    Sim(SimSource),
}

impl SourceConfig {
    /// Resolve the source from `GRIDFPV_SOURCE` (+ the sim knobs). Returns the config and a
    /// human-readable description for the startup banner.
    pub fn from_env() -> Self {
        let raw = std::env::var("GRIDFPV_SOURCE").unwrap_or_default();
        let raw = raw.trim();
        if raw.is_empty() || raw.eq_ignore_ascii_case("sim") {
            return SourceConfig::Sim(SimSource::from_env());
        }
        // `rh:<url>` (and anything else) is not wired in the default Director yet.
        eprintln!(
            "gridfpv: GRIDFPV_SOURCE={raw:?} is not supported in this build (the RotorHazard \
             source is feature-gated and not compiled in) — using the built-in sim source"
        );
        SourceConfig::Sim(SimSource::from_env())
    }

    /// A one-line description of the active source, for the startup banner.
    pub fn describe(&self) -> String {
        match self {
            SourceConfig::Sim(sim) => format!(
                "sim (holeshot + {} laps @ ~{}ms/lap real-time, with per-pilot variation)",
                sim.laps,
                sim.lap.as_millis()
            ),
        }
    }
}

/// How often the registry-aware spawner polls for newly-created events to attach a bridge to.
pub const REGISTRY_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Spawn a **per-event** control→source bridge across the whole [`EventRegistry`] (issue #72/#73).
///
/// With events now first-class containers, each event has its **own** log, so the source is
/// per-event: the Director runs one [`run_bridge`] per event, feeding passes into THAT event's
/// log when a heat goes `Running` *there*. Which source(s) run is now the **event's selected
/// timers** (issue #73), not a single global env source: each bridge reads its event's
/// [`EventMeta::timers`](gridfpv_server::events::EventMeta::timers) selection live (resolving each
/// id through the app-level [`TimerRegistry`]) when a heat starts. A selected **Mock** timer runs
/// the synthetic emission with *that timer's* `laps`/`lap_ms`; a selected **RotorHazard** timer is
/// a no-op stub (2b / #65 connects it). The built-in Mock's config comes from the env
/// defaults seeded into the timer registry.
///
/// This spawner seeds a bridge for every event present at startup (Practice + any already-loaded
/// persistent events) and polls the registry on [`REGISTRY_POLL_INTERVAL`] to attach a bridge to
/// any event *created at runtime* (`POST /events`). So every event can run independently and
/// concurrently. The `SourceConfig` argument is retained for the startup banner only.
///
/// Returns the spawner's [`JoinHandle`]; the per-event bridge tasks it spawns run for the
/// process lifetime (each ends when its event's log handle is dropped at shutdown).
pub fn spawn_registry_bridge(
    registry: EventRegistry,
    _source: SourceConfig,
    adapter: AdapterId,
) -> JoinHandle<()> {
    // Under the `live` feature, also spawn the persistent RotorHazard connection reconciler (#105):
    // the active event's selected RH timers connect on selection and stay connected (status
    // monitored continuously), and a running heat arms onto that *already-live* connection rather
    // than dialing per heat. The per-event bridge shares the resulting connection set so it can
    // arm/disarm heats on the live connections. A non-`live` build keeps RH a no-op stub.
    #[cfg(feature = "live")]
    let connections = {
        let (connections, _reconciler) = spawn_rh_reconciler(registry.clone());
        connections
    };

    let timers = registry.timers();
    tokio::spawn(async move {
        // The set of events that already have a bridge, so each event is attached exactly once.
        let mut attached: HashSet<EventId> = HashSet::new();
        let mut ticker = tokio::time::interval(REGISTRY_POLL_INTERVAL);
        loop {
            for meta in registry.list() {
                if attached.contains(&meta.id) {
                    continue;
                }
                // Resolve the event's own AppState/log and spawn its selection-aware bridge.
                if let Some(state) = registry.resolve(&meta.id) {
                    let registry = registry.clone();
                    let timers = timers.clone();
                    let adapter = adapter.clone();
                    let event_id = meta.id.clone();
                    #[cfg(feature = "live")]
                    let connections = connections.clone();
                    tokio::spawn(async move {
                        run_bridge(
                            state,
                            registry,
                            timers,
                            event_id,
                            adapter,
                            #[cfg(feature = "live")]
                            connections,
                        )
                        .await;
                    });
                    attached.insert(meta.id);
                }
            }
            ticker.tick().await;
        }
    })
}

/// The bridge loop: poll the log tail, drive the event's **selected timers** on `Running`, cancel
/// on `Finished`/`Aborted`/`Scored`/`Restarted` (or a newer heat going `Running`).
///
/// On a `Running` transition the bridge reads the event's current
/// [`EventMeta::timers`](gridfpv_server::events::EventMeta::timers) selection from `registry`,
/// resolves each id through `timers`, and builds the [`LapSource`] for it (a Sim timer's
/// `laps`/`lap_ms`; a RotorHazard timer is skipped as a no-op stub). Exposed (crate-internal) so
/// the test harness can run it directly against an in-memory [`AppState`].
pub(crate) async fn run_bridge(
    state: AppState,
    registry: EventRegistry,
    timers: TimerRegistry,
    event_id: EventId,
    adapter: AdapterId,
    #[cfg(feature = "live")] connections: RhConnections,
) {
    let mut cursor: Offset = 0;
    // The in-flight heat task, if a heat is currently emitting. At most one at a time.
    let mut active: Option<ActiveHeat> = None;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);

    loop {
        ticker.tick().await;

        // Reap a finished/cancelled source task so a heat that ran to the end clears the
        // slot (without it, a re-Start of the same heat would be ignored). A heat with an armed
        // RotorHazard connection is NOT reaped on the Mock tasks finishing — the live connection
        // keeps draining real passes until the heat leaves `Running` (it is disarmed there).
        if let Some(running) = &active {
            let has_armed_rh = {
                #[cfg(feature = "live")]
                {
                    !running.armed_rh.is_empty()
                }
                #[cfg(not(feature = "live"))]
                {
                    false
                }
            };
            if running.is_finished() && !has_armed_rh {
                active = None;
            }
        }

        let new_events = match read_tail(&state, cursor) {
            Ok(batch) => batch,
            // A poisoned lock (or a dropped log at shutdown) ends the bridge cleanly.
            Err(_) => return,
        };
        if new_events.is_empty() {
            continue;
        }

        for (offset, event) in new_events {
            cursor = offset + 1;
            // Only heat-loop transitions drive the bridge. Other events (HeatScheduled,
            // passes, registrations, …) need no action — the lineup is looked up from the
            // log when the heat goes Running.
            if let Event::HeatStateChanged { heat, transition } = event {
                handle_transition(
                    &state,
                    &registry,
                    &timers,
                    &event_id,
                    &adapter,
                    &mut active,
                    #[cfg(feature = "live")]
                    &connections,
                    heat,
                    transition,
                );
            }
        }
    }
}

/// Resolve the event's selected **Mock** timers into the [`SimSource`]s to run for this heat
/// (issues #73, #105).
///
/// Reads the event's current `timers` selection from `registry`, looks each id up in the app-level
/// `timers` registry, and maps each **Mock** timer to a [`SimSource`] with that timer's
/// `laps`/`lap_ms`. A selected **RotorHazard** timer is *not* a per-heat source here — under the
/// `live` feature it is driven through its **persistent connection** (#105): the heat is armed onto
/// the already-live connection (see [`selected_rh_timers`] + [`handle_transition`]) rather than a
/// new socket dialed each heat. In a **non-`live`** build a RotorHazard timer is a no-op stub. An id
/// that no longer resolves (a since-deleted timer) is skipped. Returns the synthetic sources to
/// drive concurrently for the running heat — empty when the event selects no usable Mock timer.
fn selected_sources(
    registry: &EventRegistry,
    timers: &TimerRegistry,
    event_id: &EventId,
) -> Vec<Arc<dyn LapSource>> {
    let Some(selection) = registry.timers_of(event_id) else {
        return Vec::new();
    };
    let mut sources: Vec<Arc<dyn LapSource>> = Vec::new();
    for id in selection {
        let Some(timer) = timers.get(&id) else {
            continue;
        };
        match timer.kind {
            TimerKind::Mock { laps, lap_ms } => {
                sources.push(Arc::new(SimSource::new(
                    laps,
                    Duration::from_millis(lap_ms),
                )));
            }
            // RotorHazard is driven through its persistent connection (#105), not a per-heat source.
            TimerKind::Rotorhazard { .. } => {}
        }
    }
    sources
}

/// The event's selected **RotorHazard** timer ids (issue #105) — the timers a running heat arms onto
/// their already-live persistent connections (race driving decoupled from connecting). An id that no
/// longer resolves, or is not a RotorHazard timer, is skipped.
#[cfg(feature = "live")]
fn selected_rh_timers(
    registry: &EventRegistry,
    timers: &TimerRegistry,
    event_id: &EventId,
) -> Vec<gridfpv_server::timers::TimerId> {
    let Some(selection) = registry.timers_of(event_id) else {
        return Vec::new();
    };
    selection
        .into_iter()
        .filter(|id| {
            matches!(
                timers.get(id).map(|t| t.kind),
                Some(TimerKind::Rotorhazard { .. })
            )
        })
        .collect()
}

/// React to a heat-loop transition: start emitting from the event's selected timers on `Running`,
/// stop on a terminal / off-ramp transition for the heat that is currently emitting.
#[allow(clippy::too_many_arguments)]
fn handle_transition(
    state: &AppState,
    registry: &EventRegistry,
    timers: &TimerRegistry,
    event_id: &EventId,
    adapter: &AdapterId,
    active: &mut Option<ActiveHeat>,
    #[cfg(feature = "live")] connections: &RhConnections,
    heat: HeatId,
    transition: HeatTransition,
) {
    match transition {
        HeatTransition::Running => {
            // A different heat taking the timer cancels the previous one (only one heat
            // emits at a time). Re-running the *same* heat also restarts cleanly.
            if let Some(running) = active.take() {
                running.stop(
                    #[cfg(feature = "live")]
                    connections,
                    #[cfg(feature = "live")]
                    event_id,
                );
            }
            let Some(lineup) = lineup_of(state, &heat) else {
                // No HeatScheduled for this heat (or the log read failed): nothing to emit.
                return;
            };
            if lineup.is_empty() {
                return;
            }
            // Resolve the event's selected Mock timers to the synthetic source(s) to run.
            let sources = selected_sources(registry, timers, event_id);
            let mut handles = Vec::with_capacity(sources.len());
            for source in sources {
                let sink = PassSink::new(state.clone(), adapter.clone());
                let run = HeatRun {
                    heat: heat.clone(),
                    lineup: lineup.clone(),
                };
                handles.push(tokio::spawn(async move {
                    if let Err(e) = source.run_heat(run, sink).await {
                        eprintln!("gridfpv: timer source stopped: {e}");
                    }
                }));
            }
            // Arm the heat onto each selected RotorHazard timer's already-live persistent connection
            // (#105): the connection stages the race + drains its passes into THIS event's log. This
            // reuses the connection opened on selection rather than dialing per heat.
            #[cfg(feature = "live")]
            let armed_rh = {
                let mut armed = Vec::new();
                for timer_id in selected_rh_timers(registry, timers, event_id) {
                    let sink = PassSink::new(state.clone(), adapter.clone());
                    if connections.arm_heat(event_id, &timer_id, lineup.clone(), sink) {
                        armed.push(timer_id);
                    }
                }
                armed
            };
            #[cfg(feature = "live")]
            let nothing_armed = armed_rh.is_empty();
            #[cfg(not(feature = "live"))]
            let nothing_armed = true;
            if handles.is_empty() && nothing_armed {
                return;
            }
            *active = Some(ActiveHeat {
                heat,
                handles,
                #[cfg(feature = "live")]
                armed_rh,
            });
        }
        // Any transition that takes the heat off `Running` stops its emission. The bridge
        // only emits while `Running`, mirroring `consumes_pass` (race-engine §2).
        HeatTransition::Finished
        | HeatTransition::Aborted
        | HeatTransition::Scored
        | HeatTransition::Restarted
        | HeatTransition::Discarded
        | HeatTransition::Advanced => {
            if let Some(running) = active.as_ref() {
                if running.heat == heat {
                    if let Some(running) = active.take() {
                        running.stop(
                            #[cfg(feature = "live")]
                            connections,
                            #[cfg(feature = "live")]
                            event_id,
                        );
                    }
                }
            }
        }
        // Staged/Armed are pre-Running: nothing to cancel (the heat isn't emitting yet).
        HeatTransition::Staged | HeatTransition::Armed => {}
    }
}

/// A heat currently emitting passes: which heat, the synthetic-source task(s) driving its selected
/// **Mock** timers (issue #73 — an event may select several), and (under `live`) the selected
/// **RotorHazard** timers armed onto their persistent connections for this heat (#105).
struct ActiveHeat {
    heat: HeatId,
    handles: Vec<JoinHandle<()>>,
    /// The RotorHazard timers armed onto their live connections for this heat, disarmed when the
    /// heat leaves `Running` (the connection stays alive).
    #[cfg(feature = "live")]
    armed_rh: Vec<gridfpv_server::timers::TimerId>,
}

impl ActiveHeat {
    /// Whether every synthetic-source task has finished — the slot can be reaped. RotorHazard
    /// connections are persistent (not per-heat tasks), so they don't gate reaping; a finished
    /// Mock run still leaves any armed RH connection live until the heat leaves `Running`.
    fn is_finished(&self) -> bool {
        self.handles.iter().all(|h| h.is_finished())
    }

    /// Stop this heat's emission: abort every in-flight Mock source task, and **disarm** each armed
    /// RotorHazard timer (stop/clear its race but keep the connection alive). Called when the heat
    /// leaves `Running` or is superseded by a newer running heat.
    fn stop(
        &self,
        #[cfg(feature = "live")] connections: &RhConnections,
        #[cfg(feature = "live")] event_id: &EventId,
    ) {
        for h in &self.handles {
            h.abort();
        }
        #[cfg(feature = "live")]
        for timer_id in &self.armed_rh {
            connections.disarm(event_id, timer_id);
        }
    }
}

/// Read the log tail from `cursor`, returning `(offset, event)` pairs. A thin wrapper over
/// the shared log handle so the bridge can poll without reaching into the server internals.
fn read_tail(state: &AppState, cursor: Offset) -> Result<Vec<(Offset, Event)>, SourceError> {
    let log = state
        .log()
        .lock()
        .map_err(|_| SourceError("the event log lock was poisoned".into()))?
        .read_from(cursor)
        .map_err(|e| SourceError(e.to_string()))?;
    Ok(log.into_iter().map(|s| (s.offset, s.event)).collect())
}

/// The lineup of `heat` from its most recent `HeatScheduled` in the log, or `None` if the
/// heat was never scheduled (or the read failed).
fn lineup_of(state: &AppState, heat: &HeatId) -> Option<Vec<CompetitorRef>> {
    let stored = state.log().lock().ok()?.read_all().ok()?;
    let mut lineup = None;
    for s in stored {
        if let Event::HeatScheduled { heat: h, lineup: l } = s.event {
            if &h == heat {
                lineup = Some(l);
            }
        }
    }
    lineup
}

fn parse_env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.trim().parse().ok()
}

fn parse_env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use gridfpv_server::events::{EventRegistry, PRACTICE_EVENT_ID};
    use gridfpv_server::live_state::live_state;
    use gridfpv_server::timers::{MOCK_TIMER_ID, TimerId, TimerKind, UpdateTimerRequest};
    use tokio::time::{Instant, sleep, timeout};

    /// The Practice event id every bridge test drives (its in-memory log + default `["mock"]`
    /// selection).
    fn practice() -> EventId {
        EventId(PRACTICE_EVENT_ID.to_string())
    }

    /// Build a fresh registry and retune its built-in **Mock** to a fast pace (`lap_ms`) and
    /// the wanted `laps`, so the whole heat runs in a few ms. Practice defaults to selecting the
    /// Mock, so the bridge over Practice drives this retuned source. The bridge polls at
    /// [`POLL_INTERVAL`], which dominates start-up latency, so tests keep total laps small.
    fn fast_registry(laps: u32, lap_ms: u64) -> EventRegistry {
        let registry = EventRegistry::new(None).unwrap();
        registry
            .timers()
            .update(
                &TimerId(MOCK_TIMER_ID.to_string()),
                &UpdateTimerRequest {
                    name: None,
                    kind: Some(TimerKind::Mock { laps, lap_ms }),
                },
            )
            .unwrap();
        registry
    }

    /// Spawn the selection-aware bridge for `registry`'s Practice event, returning the bridge
    /// handle and Practice's `AppState` (the same log the bridge polls), so a test appends the
    /// schedule/transition events the bridge reacts to.
    fn spawn_bridge_for(registry: &EventRegistry) -> (JoinHandle<()>, AppState) {
        let state = registry.resolve(&practice()).unwrap();
        let timers = registry.timers();
        let adapter = AdapterId(SIM_ADAPTER.to_string());
        let reg = registry.clone();
        let bridge_state = state.clone();
        #[cfg(feature = "live")]
        let connections = RhConnections::new();
        let handle = tokio::spawn(async move {
            run_bridge(
                bridge_state,
                reg,
                timers,
                practice(),
                adapter,
                #[cfg(feature = "live")]
                connections,
            )
            .await
        });
        (handle, state)
    }

    fn read_all_events(state: &AppState) -> Vec<Event> {
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

    fn count_passes(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::Pass(p) if p.gate.is_lap_gate()))
            .count()
    }

    /// Poll until `cond` over the current log holds, or fail after `deadline`. Keeps the
    /// tests deterministic-by-condition rather than by fixed sleeps.
    async fn wait_until(
        state: &AppState,
        deadline: Duration,
        mut cond: impl FnMut(&[Event]) -> bool,
    ) {
        let start = Instant::now();
        loop {
            let events = read_all_events(state);
            if cond(&events) {
                return;
            }
            if start.elapsed() > deadline {
                panic!(
                    "condition not met within {deadline:?}; log has {} events",
                    events.len()
                );
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn running_heat_emits_laps_for_every_lineup_member() {
        let laps = 3u32;
        let registry = fast_registry(laps, 1);
        let (bridge, state) = spawn_bridge_for(&registry);

        // Schedule a heat and drive it to Running via the (shared) log — exactly what the
        // control path appends.
        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
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

        // Each pilot emits a holeshot + `laps` laps = laps+1 passes; two pilots => 2*(laps+1).
        let expected = 2 * (laps as usize + 1);
        timeout(
            Duration::from_secs(5),
            wait_until(&state, Duration::from_secs(5), move |events| {
                count_passes(events) >= expected
            }),
        )
        .await
        .expect("the sim should emit all passes well within the timeout");

        let events = read_all_events(&state);
        assert_eq!(
            count_passes(&events),
            expected,
            "exactly holeshot+laps per pilot"
        );

        // (b) live_state / PilotProgress shows laps for each pilot.
        let ls = live_state(&events);
        assert_eq!(ls.progress.len(), 2);
        for p in &ls.progress {
            assert!(
                p.laps_completed >= 1,
                "pilot {:?} should have completed laps, got {}",
                p.competitor,
                p.laps_completed
            );
        }

        bridge.abort();
    }

    #[tokio::test]
    async fn finished_transition_stops_emission() {
        // Many laps at a slightly slower pace so the heat is still mid-emission when we
        // Finish it — proving the Finish actually cancels in-flight emission.
        let registry = fast_registry(50, 3);
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
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

        // Let a few passes land, then Finish.
        wait_until(&state, Duration::from_secs(5), |events| {
            count_passes(events) >= 2
        })
        .await;
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat.clone(),
                    transition: HeatTransition::Finished,
                },
                None,
            )
            .unwrap();

        // After the bridge observes Finished (a poll interval) and the in-flight task is
        // aborted, the pass count must settle. Sample, wait past several poll/lap cycles,
        // sample again: no further passes.
        sleep(POLL_INTERVAL * 3).await;
        let settled = count_passes(&read_all_events(&state));
        sleep(POLL_INTERVAL * 4).await;
        let after = count_passes(&read_all_events(&state));
        assert_eq!(
            after, settled,
            "no passes should be appended after Finished"
        );
        assert!(
            after < 50,
            "emission stopped well before the full lap count"
        );

        bridge.abort();
    }

    #[tokio::test]
    async fn a_newer_running_heat_supersedes_the_previous_one() {
        let registry = fast_registry(40, 1);
        let (bridge, state) = spawn_bridge_for(&registry);

        // Heat 1 starts running.
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                },
                None,
            )
            .unwrap();
        state
            .append(
                Event::HeatStateChanged {
                    heat: HeatId("q-1".into()),
                    transition: HeatTransition::Running,
                },
                None,
            )
            .unwrap();
        wait_until(&state, Duration::from_secs(5), |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::Pass(p) if p.competitor == CompetitorRef("A".into())))
        })
        .await;

        // Heat 2 starts running: the bridge must cancel heat 1 and emit for B.
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-2".into()),
                    lineup: vec![CompetitorRef("B".into())],
                },
                None,
            )
            .unwrap();
        state
            .append(
                Event::HeatStateChanged {
                    heat: HeatId("q-2".into()),
                    transition: HeatTransition::Running,
                },
                None,
            )
            .unwrap();
        wait_until(&state, Duration::from_secs(5), |events| {
            events
                .iter()
                .any(|e| matches!(e, Event::Pass(p) if p.competitor == CompetitorRef("B".into())))
        })
        .await;

        bridge.abort();
    }

    #[tokio::test]
    async fn an_event_selecting_only_rotorhazard_emits_nothing() {
        // RotorHazard is a reserved no-op stub in this slice (#73): an event whose ONLY selected
        // timer is RotorHazard must emit no synthetic passes when its heat runs.
        use gridfpv_server::timers::CreateTimerRequest;
        let registry = EventRegistry::new(None).unwrap();
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
            })
            .unwrap();
        // Select only the RotorHazard timer for Practice.
        registry.set_timers(&practice(), vec![rh.id]).unwrap();
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                },
                None,
            )
            .unwrap();
        state
            .append(
                Event::HeatStateChanged {
                    heat,
                    transition: HeatTransition::Running,
                },
                None,
            )
            .unwrap();

        // Wait past several poll/lap cycles; no passes should ever land (RH is a stub).
        sleep(POLL_INTERVAL * 4).await;
        assert_eq!(count_passes(&read_all_events(&state)), 0);
        bridge.abort();
    }

    #[tokio::test]
    async fn an_event_drives_its_selected_sim_timers_config() {
        // An event selecting a CREATED fast Sim timer (not the built-in) runs the synthetic
        // emission with THAT timer's laps — proving the bridge reads the per-event selection and
        // the selected timer's own config (#73).
        use gridfpv_server::timers::CreateTimerRequest;
        let registry = EventRegistry::new(None).unwrap();
        let timer = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Fast Sim".into(),
                kind: TimerKind::Mock { laps: 2, lap_ms: 1 },
            })
            .unwrap();
        registry.set_timers(&practice(), vec![timer.id]).unwrap();
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                },
                None,
            )
            .unwrap();
        state
            .append(
                Event::HeatStateChanged {
                    heat,
                    transition: HeatTransition::Running,
                },
                None,
            )
            .unwrap();

        // 1 pilot, 2 laps + holeshot = 3 passes — the selected timer's laps, not the env default.
        wait_until(&state, Duration::from_secs(5), |events| {
            count_passes(events) >= 3
        })
        .await;
        sleep(POLL_INTERVAL * 3).await;
        assert_eq!(count_passes(&read_all_events(&state)), 3);
        bridge.abort();
    }

    #[test]
    fn source_config_defaults_to_sim_and_describes_itself() {
        // No env reliance: build a sim config directly and confirm the banner text.
        let cfg = SourceConfig::Sim(SimSource::new(5, Duration::from_millis(2500)));
        let desc = cfg.describe();
        assert!(desc.contains("sim"));
        assert!(desc.contains('5'));
    }

    #[test]
    fn per_pilot_pace_spreads_the_field() {
        let sim = SimSource::new(5, Duration::from_millis(2000));
        // Seed 0 is the nominal pace; later seeds are slower (so the order isn't a tie).
        assert!(sim.pilot_lap(1) > sim.pilot_lap(0));
        assert!(sim.pilot_lap(2) > sim.pilot_lap(1));
    }
}
