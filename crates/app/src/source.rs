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
//! `HeatScheduled`) and spawns the source task; on `Finished`/`Aborted`/`Finalized`/`Restarted`
//! for the running heat — or a *different* heat going `Running` — it cancels the task. At
//! most one heat emits at a time (a single in-flight task), which is plenty for a Director
//! driving one timer.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, HeatId, HeatTransition, Pass, SourceTime,
};
use gridfpv_projection::{CompetitorKey, registrations};
use gridfpv_server::app::AppState;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::pilots::PilotDirectory;
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{Timer, TimerId, TimerKind, TimerRegistry};
use gridfpv_storage::Offset;
use tokio::task::JoinHandle;

pub mod failover;
pub use failover::active_source;

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

/// The shared **active-source gate** (issue #112): the single selected timer whose passes are
/// currently fed into the log, the rest being hot-standby alternates whose passes are dropped.
///
/// The bridge re-evaluates the active source every poll (so a primary drop fails over to an
/// alternate live, mid-heat) and stores it here; each source's [`PassSink`] is bound to its own
/// owning timer id and only appends while it *is* the active source. Cloning shares the one cell
/// (`Arc<RwLock<…>>`) across every source feeding one heat.
#[derive(Clone, Default)]
pub struct ActiveSourceGate {
    inner: Arc<RwLock<Option<TimerId>>>,
}

impl ActiveSourceGate {
    /// A gate with no active source yet (nothing feeds until [`set`](Self::set)).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the currently-active source (the bridge calls this each poll). `None` ⇒ no selected
    /// timer is healthy, so nothing feeds.
    pub fn set(&self, active: Option<TimerId>) {
        *self.inner.write().expect("active-source gate poisoned") = active;
    }

    /// Whether `timer` is the active source right now — the sink's append gate.
    pub fn is_active(&self, timer: &TimerId) -> bool {
        self.inner
            .read()
            .expect("active-source gate poisoned")
            .as_ref()
            == Some(timer)
    }
}

/// The append surface a [`LapSource`] pushes passes through. Wraps the shared
/// [`AppState`] so every emitted pass lands in the one log and wakes `/stream`.
///
/// When bound to an [`ActiveSourceGate`] and an owning [`TimerId`] (issue #112), the sink only
/// appends while its timer is the **active source** — a hot-standby alternate's passes are dropped
/// so the same crossing is never double-counted. An unbound sink (`gate`/`timer` `None`) always
/// feeds, preserving the pre-#112 single-timer behaviour exactly.
#[derive(Clone)]
pub struct PassSink {
    state: AppState,
    adapter: AdapterId,
    /// The active-source gate this sink is bound to (issue #112), or `None` for an always-feeding
    /// sink (single-timer events / non-failover callers).
    gate: Option<ActiveSourceGate>,
    /// The timer this sink feeds for; appends pass the gate only while it is the active source.
    timer: Option<TimerId>,
    /// The heat this sink feeds — **stamped onto every appended pass** (`Pass::heat`), so pass
    /// attribution is by tag, not log position (a heat-span event landing mid-race can no longer
    /// steal the running heat's laps). The bridge sets it when it builds a Running heat's sinks;
    /// `None` (a bare test/demo sink) appends untagged, positional-legacy passes.
    heat: Option<HeatId>,
}

impl PassSink {
    /// A sink over `state` tagging passes with `adapter`, **always feeding** (no active-source
    /// gate). Used where a single source owns the heat or by callers that don't fail over.
    pub fn new(state: AppState, adapter: AdapterId) -> Self {
        Self {
            state,
            adapter,
            gate: None,
            timer: None,
            heat: None,
        }
    }

    /// A sink bound to an [`ActiveSourceGate`] and its owning `timer` (issue #112): it appends only
    /// while `timer` is the active source, so an alternate's passes are dropped (hot standby).
    pub fn gated(
        state: AppState,
        adapter: AdapterId,
        gate: ActiveSourceGate,
        timer: TimerId,
    ) -> Self {
        Self {
            state,
            adapter,
            gate: Some(gate),
            timer: Some(timer),
            heat: None,
        }
    }

    /// Bind this sink to the heat it feeds: every appended pass is stamped `Pass::heat` so the
    /// folds attribute it by TAG (robust against heat-span events landing mid-race), never by
    /// log position alone. Builder style, applied when the bridge builds a Running heat's sinks.
    pub fn for_heat(mut self, heat: HeatId) -> Self {
        self.heat = Some(heat);
        self
    }

    /// Whether this sink may append right now: an unbound sink always may; a gated sink may only
    /// while its owning timer is the active source (issue #112).
    fn feeds(&self) -> bool {
        match (&self.gate, &self.timer) {
            (Some(gate), Some(timer)) => gate.is_active(timer),
            _ => true,
        }
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
        // Issue #112: a hot-standby alternate's passes are dropped — only the active source feeds.
        // The source still runs (stays armed/draining) so a failover to it lands instantly.
        if !self.feeds() {
            return Ok(());
        }
        let pass = Pass {
            adapter: self.adapter.clone(),
            competitor: competitor.clone(),
            at,
            sequence: Some(sequence),
            gate: GateIndex::LAP,
            signal: None,
            heat: self.heat.clone(),
        };
        self.state
            .append(Event::Pass(pass), None)
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
        // Issue #112: drop an alternate RH connection's passes while it is not the active source.
        // The connection stays live (hot standby) — only its appends are gated here.
        if !self.feeds() {
            return Ok(());
        }
        // Stamp the sink's heat onto the pass (tag attribution — see `for_heat`): the adapter
        // built the pass without one; the sink is the component that knows which heat it feeds.
        let event = match event {
            Event::Pass(mut pass) if self.heat.is_some() => {
                pass.heat = self.heat.clone();
                Event::Pass(pass)
            }
            other => other,
        };
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
/// not driven by the sim bridge — the RH connection reconciler dials it (#65/#73). The built-in Mock's config comes from the env
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
            // Prune ids that left the registry: their per-event tasks exit on their own (they
            // watch for the event's disappearance), and dropping them here keeps this set from
            // growing forever across create/delete cycles.
            let live: HashSet<EventId> = registry.list().into_iter().map(|m| m.id).collect();
            attached.retain(|id| live.contains(id));
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
/// on `Finished`/`Aborted`/`Finalized`/`Restarted` (or a newer heat going `Running`).
///
/// On a `Running` transition the bridge reads the event's current
/// [`EventMeta::timers`](gridfpv_server::events::EventMeta::timers) selection from `registry`,
/// resolves each id through `timers`, and builds the [`LapSource`] for it (a Sim timer's
/// `laps`/`lap_ms`; a RotorHazard timer is skipped here — its passes come from the real adapter). Exposed (crate-internal) so
/// the test harness can run it directly against an in-memory [`AppState`].
pub(crate) async fn run_bridge(
    state: AppState,
    registry: EventRegistry,
    timers: TimerRegistry,
    event_id: EventId,
    adapter: AdapterId,
    #[cfg(feature = "live")] connections: RhConnections,
) {
    // The in-flight heat task, if a heat is currently emitting. At most one at a time.
    let mut active: Option<ActiveHeat> = None;
    // The runtime-clock drivers (heat-lifecycle Slice 2), keyed per heat (see `HeatClock`).
    let mut clock = HeatClock::default();
    // START FROM THE CURRENT STATE, THEN FOLLOW THE TAIL — never replay history. A persistent
    // event's log holds every past heat's transitions; replaying them re-fired start drivers
    // (spurious `HeatStarting` facts, stale `Running` races) and re-spawned sim sources whose
    // synchronous holeshots corrupted an already-scored heat's window on every Director
    // restart. Instead: one read snapshots the log — the cursor starts at its tail, and any
    // heat CURRENTLY in a runtime-driven state (Armed / Running / Unofficial) is handed to the
    // normal transition handler once, as if its state had just been observed. A mid-race
    // Director restart therefore re-arms the heat's sources and completion clock (fresh clocks
    // over current state — the old process's timers died with it) and the race still auto-ends;
    // finished history stays untouched.
    let mut cursor: Offset = match read_tail(&state, 0) {
        Ok(batch) => {
            let tail = batch.last().map(|(offset, _)| offset + 1).unwrap_or(0);
            let events: Vec<Event> = batch.into_iter().map(|(_, e)| e).collect();
            for (heat, synthetic) in in_flight_heats(&events) {
                handle_transition(
                    &state,
                    &registry,
                    &timers,
                    &event_id,
                    &adapter,
                    &mut active,
                    &mut clock,
                    #[cfg(feature = "live")]
                    &connections,
                    heat,
                    synthetic,
                );
            }
            tail
        }
        Err(_) => return,
    };
    let mut ticker = tokio::time::interval(POLL_INTERVAL);

    loop {
        ticker.tick().await;

        // A DELETED event ends its bridge: the AppState Arc outlives the registry entry, so
        // without this check the orphaned task would poll a dead log forever.
        if registry.resolve(&event_id).is_none() {
            return;
        }

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

        // Issue #112 — live failover: while a heat is running, re-evaluate the **active source**
        // each poll from the event's current selection + the live timer statuses, and update the
        // gate. A primary drop (its RH connection leaves `Connected`) hands the feed to the first
        // healthy alternate; a primary recovery takes it back — all without touching the running
        // sources (they keep emitting; the gate decides whose passes land).
        if let Some(running) = &active {
            if let Some(meta) = registry.meta_of(&event_id) {
                running.gate.set(active_source(&meta, &timers));
            }
        }

        let new_events = match read_tail(&state, cursor) {
            Ok(batch) => batch,
            // A transient read error (an I/O blip on the SQLite log) must NOT end the event's
            // runtime — that silently killed every start/completion/auto-official clock until
            // the next Director restart, sticking heats in Armed forever. Warn and retry on the
            // next tick; a genuinely dropped log at shutdown just keeps idling until the process
            // exits (the bridge holds a strong state handle either way).
            Err(e) => {
                eprintln!("gridfpv: bridge could not read the event log (will retry): {e:?}");
                continue;
            }
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
                    &mut clock,
                    #[cfg(feature = "live")]
                    &connections,
                    heat,
                    transition,
                );
            }
        }
    }
}

// --- the sim auto-presence reconciler (race redesign Slice 1a) -------------------------

/// Spawn the **sim auto-presence reconciler** across the whole [`EventRegistry`] (race redesign
/// Slice 1a).
///
/// "Presence = roster membership": a pilot on an [`EventMeta::roster`] *is* present at the event.
/// When the sim (Velocidrone) adapter reports a player via [`Event::CompetitorSeen`], the RD would
/// normally have to add that pilot to the roster and bind the timing-source competitor to a GridFPV
/// pilot by hand. This reconciler does it automatically for the sim: per active event it tails the
/// log for `CompetitorSeen`, and for each seen competitor **not yet bound** whose name matches a
/// **directory pilot's callsign**, it (a) adds that pilot to the event's roster (= present) if
/// absent, and (b) appends an [`Event::CompetitorRegistered`] binding (the same binding the RD's
/// [`Command::Register`](gridfpv_server::control::Command::Register) produces, folded by
/// [`registrations`]). Unmatched / unrostered-but-no-matching-pilot seen players are a no-op (the RD
/// handles them in Slice 1b).
///
/// The reconcile is **idempotent**: roster add is set-membership (no duplicate) and a binding is
/// only appended for a competitor with no existing registration, so re-seeing a player does nothing.
///
/// Mirrors [`spawn_registry_bridge`]: it seeds a reconciler task per event present at startup and
/// polls the registry on [`REGISTRY_POLL_INTERVAL`] to attach one to any event created at runtime.
/// Returns the spawner's [`JoinHandle`]; the per-event tasks run for the process lifetime.
pub fn spawn_presence_reconciler(registry: EventRegistry) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut attached: HashSet<EventId> = HashSet::new();
        let mut ticker = tokio::time::interval(REGISTRY_POLL_INTERVAL);
        loop {
            let live: HashSet<EventId> = registry.list().into_iter().map(|m| m.id).collect();
            attached.retain(|id| live.contains(id));
            for meta in registry.list() {
                if attached.contains(&meta.id) {
                    continue;
                }
                if let Some(state) = registry.resolve(&meta.id) {
                    let registry = registry.clone();
                    let event_id = meta.id.clone();
                    tokio::spawn(async move {
                        run_presence_reconciler(state, registry, event_id).await;
                    });
                    attached.insert(meta.id);
                }
            }
            ticker.tick().await;
        }
    })
}

/// The per-event auto-presence loop (race redesign Slice 1a): poll the log tail for
/// [`Event::CompetitorSeen`] and reconcile each one into presence + a binding.
///
/// Polls the event's log on [`POLL_INTERVAL`] from a cursor. On each batch it folds the *whole*
/// log's [`registrations`] (so "already bound" reflects every binding, RD- or auto-made) and, for
/// each newly-seen competitor in the batch, calls [`reconcile_seen`]. Exposed (crate-internal) so
/// the test harness can run it directly against an in-memory [`AppState`].
pub(crate) async fn run_presence_reconciler(
    state: AppState,
    registry: EventRegistry,
    event_id: EventId,
) {
    let pilots = registry.pilots();
    let mut cursor: Offset = 0;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    loop {
        ticker.tick().await;
        // A DELETED event ends its reconciler (the log Arc alone would keep it alive forever).
        if registry.resolve(&event_id).is_none() {
            return;
        }
        let new_events = match read_tail(&state, cursor) {
            Ok(batch) => batch,
            // A poisoned lock (or a dropped log at shutdown) ends the reconciler cleanly.
            Err(_) => return,
        };
        if new_events.is_empty() {
            continue;
        }
        // Fold the current bindings over the whole log so "already bound" is authoritative
        // (an RD bind or a prior auto-bind both count). Cheap: one read; the log is per-event.
        let bindings = match read_all(&state) {
            Ok(events) => registrations(events.iter()),
            Err(_) => continue,
        };
        // Dedup WITHIN the batch: `bindings` was folded once before the loop, so two
        // CompetitorSeen for the same key in one poll batch both read "unbound" and appended
        // twice (benign — same key/value, last-wins fold — but noise in the log).
        let mut seen_this_batch: HashSet<(AdapterId, CompetitorRef)> = HashSet::new();
        for (offset, event) in new_events {
            cursor = offset + 1;
            if let Event::CompetitorSeen {
                adapter,
                competitor,
            } = event
            {
                if !seen_this_batch.insert((adapter.clone(), competitor.clone())) {
                    continue;
                }
                reconcile_seen(
                    &state, &registry, &pilots, &event_id, &bindings, adapter, competitor,
                );
            }
        }
    }
}

/// Reconcile one seen competitor into presence + a binding (race redesign Slice 1a).
///
/// No-op when the competitor is **already bound** (its `(adapter, competitor)` is in `bindings`),
/// or when **no directory pilot's callsign matches** the competitor name. Otherwise: add the
/// matched pilot to the event's [`roster`](gridfpv_server::events::EventMeta::roster) (idempotent —
/// set membership = presence) and append an [`Event::CompetitorRegistered`] binding so the lap
/// projection attributes the sim player's laps to that pilot. Best-effort: a roster/append failure
/// is logged-shaped (eprintln) and skipped rather than crashing the reconciler.
fn reconcile_seen(
    state: &AppState,
    registry: &EventRegistry,
    pilots: &PilotDirectory,
    event_id: &EventId,
    bindings: &std::collections::BTreeMap<CompetitorKey, gridfpv_server::scope::PilotId>,
    adapter: AdapterId,
    competitor: CompetitorRef,
) {
    let key = CompetitorKey {
        adapter: adapter.clone(),
        competitor: competitor.clone(),
    };
    // Already bound (by the RD or a prior auto-bind) → nothing to do (idempotent).
    if bindings.contains_key(&key) {
        return;
    }
    // Match the seen competitor name against a directory pilot's callsign. Unmatched → no-op.
    let Some(pilot_id) = match_callsign(pilots, &competitor) else {
        return;
    };
    // (a) Presence: add the matched pilot to the event's roster (idempotent set-membership).
    if let Err(e) = registry.add_to_roster(event_id, pilot_id.clone()) {
        eprintln!("gridfpv: auto-presence could not add pilot to roster: {e}");
        return;
    }
    // (b) Binding: append the CompetitorRegistered the RD's `Register` command would (#60), so
    // the sim player's laps attribute to the matched pilot.
    if let Err(e) = state.append(
        Event::CompetitorRegistered {
            adapter,
            competitor,
            pilot: pilot_id,
        },
        None,
    ) {
        eprintln!("gridfpv: auto-presence could not append binding: {e:?}");
    }
}

/// Find the directory pilot whose **callsign matches** the seen competitor name (race redesign
/// Slice 1a), or `None`. The match is **case-insensitive and trimmed** so a sim player name and a
/// stored callsign that differ only in surrounding whitespace or case still bind. The directory is
/// listed in id order, so the first matching pilot wins deterministically.
fn match_callsign(
    pilots: &PilotDirectory,
    competitor: &CompetitorRef,
) -> Option<gridfpv_server::scope::PilotId> {
    let name = competitor.0.trim();
    pilots
        .list()
        .into_iter()
        .find(|p| p.callsign.trim().eq_ignore_ascii_case(name))
        .map(|p| p.id)
}

/// Read the whole event log, returning its [`Event`]s in append order. A thin wrapper over the
/// shared log handle used by the presence reconciler to fold the current registration bindings.
fn read_all(state: &AppState) -> Result<Vec<Event>, SourceError> {
    let stored = state
        .log()
        .lock()
        .map_err(|_| SourceError("the event log lock was poisoned".into()))?
        .read_all()
        .map_err(|e| SourceError(e.to_string()))?;
    Ok(stored.into_iter().map(|s| s.event).collect())
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
) -> Vec<(TimerId, Arc<dyn LapSource>)> {
    let Some(selection) = registry.timers_of(event_id) else {
        return Vec::new();
    };
    let mut sources: Vec<(TimerId, Arc<dyn LapSource>)> = Vec::new();
    for id in selection {
        let Some(timer) = timers.get(&id) else {
            continue;
        };
        match timer.kind {
            TimerKind::Mock { laps, lap_ms } => {
                sources.push((
                    id,
                    Arc::new(SimSource::new(laps, Duration::from_millis(lap_ms))),
                ));
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
    clock: &mut HeatClock,
    #[cfg(feature = "live")] connections: &RhConnections,
    heat: HeatId,
    transition: HeatTransition,
) {
    // Heat-lifecycle Slice 2 — the runtime clock. Any transition for THIS heat cancels its in-flight
    // clock drivers first (a stale countdown/completion must never append after the heat moved on);
    // the per-state arms below then (re)spawn the driver the new state wants. A transition for a
    // *different* heat leaves this heat's drivers alone.
    clock.cancel_for(&heat);
    match transition {
        HeatTransition::Running => {
            // A different heat taking the timer cancels the previous one (only one heat
            // emits at a time). Re-running the *same* heat also restarts cleanly. A superseded
            // open-practice heat's accumulator is cleared below by `begin`/the clear-on-stop.
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
            // Issue #112: the **active-source gate** — only the active source's passes feed the log
            // (the primary while healthy, else the first healthy alternate). All selected timers
            // still run (hot standby); the gate drops a non-active source's passes. It is seeded
            // immediately so the very first pass is gated correctly, then re-evaluated each poll in
            // `run_bridge` so a primary drop fails over mid-heat.
            let gate = ActiveSourceGate::new();
            if let Some(meta) = registry.meta_of(event_id) {
                gate.set(active_source(&meta, timers));
            }
            // Resolve the event's selected Mock timers to the synthetic source(s) to run — each
            // bound to a gated sink for its own timer id, so only the active one feeds.
            let sources = selected_sources(registry, timers, event_id);
            let mut handles = Vec::with_capacity(sources.len());
            for (timer_id, source) in sources {
                let sink = PassSink::gated(state.clone(), adapter.clone(), gate.clone(), timer_id)
                    .for_heat(heat.clone());
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
            // reuses the connection opened on selection rather than dialing per heat. Each arming's
            // sink is gated on its own timer id (issue #112) so an alternate RH connection stays live
            // but its passes are dropped while it is not the active source.
            #[cfg(feature = "live")]
            let armed_rh = {
                let mut armed = Vec::new();
                for timer_id in selected_rh_timers(registry, timers, event_id) {
                    let sink = PassSink::gated(
                        state.clone(),
                        adapter.clone(),
                        gate.clone(),
                        timer_id.clone(),
                    )
                    .for_heat(heat.clone());
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
            // Spawn the completion clock (heat-lifecycle Slice 2): it watches the running passes and
            // auto-appends `Finished` once the win condition + grace are met. Independent of whether
            // any source emits — a real RH heat with no Mock source still needs auto-completion.
            HeatClock::install(
                &mut clock.completion,
                heat.clone(),
                spawn_completion_driver(state, registry, event_id, heat.clone()),
            );

            if handles.is_empty() && nothing_armed {
                return;
            }
            *active = Some(ActiveHeat {
                heat,
                handles,
                gate,
                #[cfg(feature = "live")]
                armed_rh,
            });
        }
        // Any transition that takes the heat off `Running` stops its emission. The bridge
        // only emits while `Running`, mirroring `consumes_pass` (race-engine §2).
        HeatTransition::Finished
        | HeatTransition::Aborted
        | HeatTransition::Finalized
        | HeatTransition::Reverted
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
            // Auto-official timer (marshaling Slice 5): the two transitions that land the heat in
            // `Unofficial` — `Finished` (race-end) and `Reverted` (a finalized result re-opened) —
            // (re)arm the protest window. The `cancel_for(&heat)` at the top already dropped any prior
            // protest driver, so a `Revert` cleanly re-opens the window from the new `Unofficial`
            // instant. A round with no protest window spawns an inert driver that returns immediately.
            // The other transitions here (`Finalized`/`Aborted`/`Restarted`/`Discarded`/`Advanced`)
            // leave `Unofficial`, so they were cancelled above and are not re-armed.
            if matches!(
                transition,
                HeatTransition::Finished | HeatTransition::Reverted
            ) {
                HeatClock::install(
                    &mut clock.protest,
                    heat.clone(),
                    spawn_auto_official_driver(state, registry, event_id, heat.clone()),
                );
            }
        }
        // Staged is pre-Running: the heat isn't emitting yet, but it is the moment to **tune** the
        // device to the heat's assigned channels (race redesign Slice 4a — "the engine allocates,
        // the adapter applies") and to **prepare** each RotorHazard timer for an instant start (Grid
        // owns all timing). The Mock has no tune/prepare (the sim source ignores channels and has no
        // staging — no-op); each selected RotorHazard timer's live connection emits `set_frequency`
        // per node and zeroes RH's staging + resets it to READY. The tune plan is read from the
        // heat's `HeatScheduled.frequencies` (empty ⇒ nothing to tune); the prepare runs regardless
        // so RH is reset and its staging zeroed even for a heat with no explicit channel plan.
        HeatTransition::Staged => {
            #[cfg(feature = "live")]
            {
                // #412: the tune plan and the seating are both **per timer** now. A heat's
                // lineup position `n` maps to the timer's *n*-th ENABLED node, which is not `n`
                // once the RD has switched one off — and two selected timers may have different
                // nodes disabled, so one shared plan would seat a pilot on the wrong gate.
                for timer_id in selected_rh_timers(registry, timers, event_id) {
                    let Some(timer) = timers.get(&timer_id) else {
                        continue;
                    };
                    let plan = tune_plan_of(state, &timer, &heat);
                    let seats = seats_of(state, registry, &timer, &heat);
                    // Ready RH for an instant start at Grid's go: zero its staging hold/tones + reset
                    // to READY now, well before the Armed hold + tone fire. Retires the old at-go
                    // reset/stage race (the `STAGE_RESET_SETTLE` band-aid).
                    connections.prepare(event_id, &timer_id);
                    if !plan.is_empty() {
                        connections.tune(event_id, &timer_id, plan.clone());
                    }
                    // Seat the heat's bound pilots onto their RH nodes (the laps-attribute fix): RH
                    // dismisses a crossing on a node with no seated pilot, so without this RH races an
                    // empty-pilot heat and records zero laps. Empty (an open-practice / unbound heat)
                    // is a no-op — the connection then runs in practice mode (RH records via the
                    // no-current-heat gate branch).
                    if !seats.is_empty() {
                        connections.seat(event_id, &timer_id, seats.clone());
                    }
                }
            }
            // Mock timers are a no-op: the synthetic source flies on no real channels. In a
            // non-`live` build there is no RH connection at all, so `heat` is unused here.
            #[cfg(not(feature = "live"))]
            let _ = &heat;
        }
        // Armed: run the **start procedure** (heat-lifecycle Slice 2). The heat isn't emitting yet;
        // the start driver logs the chosen delay (`HeatStarting`) and, after it, the auto
        // `Armed → Running`. A manual `SkipCountdown` (or an abort) cancels this via `cancel_for`
        // before it fires — see the top of this fn.
        HeatTransition::Armed => {
            HeatClock::install(
                &mut clock.start,
                heat.clone(),
                spawn_start_driver(state, registry, event_id, heat),
            );
        }
    }
}

/// The runtime-clock drivers in flight for the bridge (heat-lifecycle Slice 2), **keyed per
/// heat**: the `start` countdown for each heat in `Armed`, the `completion` clock for each in
/// `Running`, and the `protest` auto-official timer for each in `Unofficial`. Per-heat maps —
/// NOT single slots — because the windows genuinely overlap in normal operation (heat 2
/// finishes while heat 1's protest window is still open): a single slot silently DETACHED the
/// older heat's task on overwrite, `cancel_for` could never find it again, and its orphaned
/// timer later force-finalized a heat the RD had already discarded or reverted.
#[derive(Default)]
struct HeatClock {
    /// Start countdowns, per heat in `Armed` (appends `HeatStarting` then auto `Running`).
    start: std::collections::HashMap<HeatId, JoinHandle<()>>,
    /// Completion clocks, per heat in `Running` (appends auto `Finished` on win + grace).
    completion: std::collections::HashMap<HeatId, JoinHandle<()>>,
    /// Auto-official timers, per heat in `Unofficial` (marshaling Slice 5): when the round armed
    /// a protest window, logs the deadline (`HeatFinalizing`) then appends the auto `Finalized`.
    protest: std::collections::HashMap<HeatId, JoinHandle<()>>,
}

impl HeatClock {
    /// Cancel any in-flight start/completion/protest driver belonging to `heat` (it just changed
    /// state, so a pending auto-transition for the *old* state must not land). Drivers for other
    /// heats are left running. Aborting a finished task is a harmless no-op.
    fn cancel_for(&mut self, heat: &HeatId) {
        if let Some(task) = self.start.remove(heat) {
            task.abort();
        }
        if let Some(task) = self.completion.remove(heat) {
            task.abort();
        }
        if let Some(task) = self.protest.remove(heat) {
            task.abort();
        }
    }

    /// Install `task` as `heat`'s driver in `slot`, aborting any previous task for the SAME heat
    /// (a re-arm replaces; other heats' drivers are untouched — the single-slot detach bug).
    fn install(
        slot: &mut std::collections::HashMap<HeatId, JoinHandle<()>>,
        heat: HeatId,
        task: JoinHandle<()>,
    ) {
        if let Some(previous) = slot.insert(heat, task) {
            previous.abort();
        }
    }
}

/// A heat currently emitting passes: which heat, the synthetic-source task(s) driving its selected
/// **Mock** timers (issue #73 — an event may select several), and (under `live`) the selected
/// **RotorHazard** timers armed onto their persistent connections for this heat (#105).
struct ActiveHeat {
    heat: HeatId,
    handles: Vec<JoinHandle<()>>,
    /// The active-source gate for this heat (issue #112): the bridge re-evaluates the active source
    /// each poll and stores it here, and every source's sink reads it to gate its appends — so only
    /// the active source feeds and a primary drop fails over live.
    gate: ActiveSourceGate,
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
/// The heats caught MID-FLIGHT by a Director restart, each paired with the SYNTHETIC
/// transition that re-enters the normal `handle_transition` path for its current state:
/// `Armed` → the start countdown re-arms; `Running` → sources + the completion clock re-arm
/// (the race still auto-ends); `Unofficial` → the protest auto-official timer re-arms.
/// Everything else (Scheduled / Staged / Final / …) needs no runtime driver.
fn in_flight_heats(events: &[Event]) -> Vec<(HeatId, HeatTransition)> {
    let mut seen = std::collections::BTreeSet::new();
    let mut in_flight = Vec::new();
    for event in events {
        let heat = match event {
            Event::HeatScheduled { heat, .. } | Event::HeatStateChanged { heat, .. } => heat,
            _ => continue,
        };
        if !seen.insert(heat.clone()) {
            continue;
        }
        use gridfpv_engine::heat::HeatState;
        let synthetic = match gridfpv_engine::heat::heat_state(events, heat) {
            Some(HeatState::Armed) => HeatTransition::Armed,
            Some(HeatState::Running) => HeatTransition::Running,
            Some(HeatState::Unofficial) => HeatTransition::Finished,
            _ => continue,
        };
        in_flight.push((heat.clone(), synthetic));
    }
    in_flight
}

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
        if let Event::HeatScheduled {
            heat: h, lineup: l, ..
        } = s.event
        {
            if &h == heat {
                lineup = Some(l);
            }
        }
    }
    lineup
}

/// The per-pilot frequency assignment of `heat` from its most recent `HeatScheduled` (race redesign
/// Slice 4a), mapped onto **real node indices** for the RH `set_frequency` tune.
///
/// The node a competitor's MHz is applied to is its seat on `timer` — [`Timer::seat_nodes`], which
/// walks the timer's **enabled** indices rather than `0..lineup.len()` (#412). Tuning by lineup
/// position would put the heat's third pilot's channel on node 2 while RotorHazard seats them on
/// node 3: the pilot flies a gate tuned to somebody else's video channel, which is a dead node with
/// extra steps. A **disabled node is never tuned** — it is not offered a channel at all.
///
/// A heat with no assigned frequencies (a sim/un-channelled heat) yields an empty plan (no tuning).
#[cfg(feature = "live")]
fn tune_plan_of(state: &AppState, timer: &Timer, heat: &HeatId) -> Vec<(u64, u16)> {
    let Some(stored) = state.log().lock().ok().and_then(|g| g.read_all().ok()) else {
        return Vec::new();
    };
    let mut plan = Vec::new();
    for s in stored {
        if let Event::HeatScheduled {
            heat: h,
            lineup,
            frequencies,
            label: None,
            ..
        } = s.event
        {
            if &h == heat {
                // Map each (competitor, mhz) onto the REAL node that competitor is seated on.
                let seats = timer.seat_nodes(&lineup);
                plan = frequencies
                    .into_iter()
                    .filter_map(|(competitor, mhz)| {
                        seats
                            .iter()
                            .find(|(_, seated)| *seated == competitor)
                            .map(|(node, _)| (*node as u64, mhz))
                    })
                    .collect();
            }
        }
    }
    plan
}

/// The heat's **node→pilot seating** for RotorHazard (the laps-attribute fix): one
/// `(node_index, callsign)` per **bound** node of `heat`, read from the heat's lineup (its durable
/// `HeatScheduled` bind).
///
/// `node_index` is the **real** node the pilot flies — [`Timer::seat_nodes`] walks `timer`'s enabled
/// indices, so the heat's third pilot sits on node 3 when node 2 is disabled, not on node 2 (#412).
/// RotorHazard's `alter_heat` is keyed on that real `seat_index`, so getting it wrong seats the
/// pilot on the dead node this whole feature exists to keep them off — and their laps would land on
/// somebody else's row.
///
/// Each bound seat resolves to its pilot's **callsign** via the directory (CLAUDE.md: resolve a ref
/// to its friendly name from a durable source, never print the raw id). An open-practice /
/// unchannelled heat seats per **channel** as `node-{i}` refs (no bound pilot) — those are skipped
/// here, leaving an empty plan (RH then races in practice mode). A pilot ref that does not resolve
/// falls back to the raw ref string as a last resort so the node is still seated (RH records there)
/// rather than dropped.
#[cfg(feature = "live")]
fn seats_of(
    state: &AppState,
    registry: &EventRegistry,
    timer: &Timer,
    heat: &HeatId,
) -> Vec<(u64, String)> {
    let Some(lineup) = lineup_of(state, heat) else {
        return Vec::new();
    };
    let pilots = registry.pilots();
    let mut seats = Vec::new();
    for (node, competitor) in timer.seat_nodes(&lineup) {
        // An open-practice seat (`node-{i}`) names a channel, not a bound pilot: leave it unseated.
        if gridfpv_server::timers::node_seat_index(&competitor).is_some() {
            continue;
        }
        let pilot_id = gridfpv_server::scope::PilotId(competitor.0.clone());
        let callsign = pilots
            .get(&pilot_id)
            .map(|p| p.callsign)
            .unwrap_or(competitor.0);
        seats.push((node as u64, callsign));
    }
    seats
}

// --- the runtime clock (heat-lifecycle redesign, Slice 2) -----------------------------
//
// The clock is a **runtime input**, exactly like an RD button press: it never computes a
// transition from wall-clock *inside the fold* — it appends **logged events** that the pure
// engine/projection folds like any other. Two auto-transitions are driven here:
//
//   * **start** — when a heat enters `Armed`, the runtime reads the round's `start_procedure`,
//     picks the randomized delay **once** (RNG only here, at emission time), writes it to the log
//     as `Event::HeatStarting { delay_ms }` (so the console can cue the tone and a replay reads the
//     same delay), then appends `HeatStateChanged { Running }` after that delay.
//   * **completion** — while a heat is `Running`, the runtime evaluates the round's win condition
//     over the running passes (a *pure* predicate, `race_end_reached`), and once the race-end
//     criterion is met it holds the configured **grace window** for trailing pilots, then appends
//     `HeatStateChanged { Finished }`.
//
// Wall-clock (tokio time) decides only *when* the runtime appends; the delay and the criterion are
// facts in the log, so the replay is deterministic (race-engine.html §6).

use gridfpv_engine::heat::{GraceWindow, ProtestWindow};
use gridfpv_engine::scoring::race_end_reached;
use gridfpv_server::control_handler::open_protest_count;
use gridfpv_server::events::{RoundDef, StartProcedure};

/// How often the completion driver re-evaluates the win condition over the running passes.
const COMPLETION_POLL: Duration = Duration::from_millis(100);

/// The per-heat runtime config the clock needs: the round's start procedure, win condition, and
/// grace window. Resolved from the heat's `HeatScheduled.round` against the event meta; a heat with
/// no round (a sim / free-text heat) uses the documented defaults so the clock still drives it.
struct HeatClockConfig {
    start_procedure: StartProcedure,
    win_condition: gridfpv_engine::scoring::WinCondition,
    grace_window: GraceWindow,
    /// The round's optional **time limit** in seconds (open-practice refinement): when set, the
    /// completion driver auto-ends the heat (`Running → Unofficial`) once its elapsed running time
    /// reaches this — independent of the win condition. `None` ⇒ the heat ends only on its win
    /// condition (or the RD's `ForceEnd`). Carried for every heat but acted on for the open-practice
    /// case (a practice with no win condition relies on it).
    time_limit_secs: Option<u32>,
    /// The round's **protest window** (marshaling Slice 5): when [`ProtestWindow::After`], the
    /// auto-official driver arms the auto-finalize timer once the heat enters `Unofficial`. The
    /// default [`ProtestWindow::Off`] means manual finalize only (no auto-official driver work).
    protest_window: ProtestWindow,
}

/// Resolve the clock config for `heat`: find the heat's most-recent `HeatScheduled.round`, look that
/// round up in the event meta, and read its `start_procedure` / `win_condition` / `grace_window`. A
/// heat with no round tag, or whose round is gone, falls back to the round defaults (a sane
/// randomized start delay + a bounded grace + a Timed win condition is *not* assumed — see below).
fn heat_clock_config(
    state: &AppState,
    registry: &EventRegistry,
    event_id: &EventId,
    heat: &HeatId,
) -> HeatClockConfig {
    let round_id = round_of_heat(state, heat);
    let round: Option<RoundDef> = round_id.and_then(|rid| {
        registry
            .rounds_of(event_id)
            .and_then(|rounds| rounds.into_iter().find(|r| r.id == rid))
    });
    match round {
        Some(r) => HeatClockConfig {
            start_procedure: r.start_procedure,
            win_condition: r.win_condition,
            grace_window: r.grace_window,
            time_limit_secs: r.time_limit_secs,
            protest_window: r.protest_window,
        },
        // No round (sim/free-text): default the start procedure + grace so the auto-start still
        // fires; the win condition defaults to the sim's `FirstToLaps` over the sim lap count so a
        // round-less sim heat still auto-completes (the sim emits a fixed number of laps). No time
        // limit (a round-less heat has no practice duration).
        None => HeatClockConfig {
            start_procedure: StartProcedure::default(),
            win_condition: default_sim_win_condition(),
            grace_window: gridfpv_server::events::default_grace_window(),
            time_limit_secs: None,
            // A round-less heat (sim/free-text) has no protest window: manual finalize only.
            protest_window: ProtestWindow::Off,
        },
    }
}

/// The win condition a **round-less** heat (a sim / free-text heat with no `RoundDef`) auto-completes
/// under: first-to-`DEFAULT_SIM_LAPS` laps. The sim emits a holeshot + `DEFAULT_SIM_LAPS` laps per
/// pilot, so the leader reaching that count is the natural completion point — letting the clock drive
/// a bare sim heat all the way to `Unofficial` without a configured round.
fn default_sim_win_condition() -> gridfpv_engine::scoring::WinCondition {
    gridfpv_engine::scoring::WinCondition::FirstToLaps {
        n: DEFAULT_SIM_LAPS,
    }
}

/// The `RoundId` tag on `heat`'s most-recent `HeatScheduled`, if any.
fn round_of_heat(state: &AppState, heat: &HeatId) -> Option<gridfpv_events::RoundId> {
    let stored = state.log().lock().ok()?.read_all().ok()?;
    let mut round = None;
    for s in stored {
        if let Event::HeatScheduled {
            heat: h, round: r, ..
        } = s.event
        {
            if &h == heat {
                round = r;
            }
        }
    }
    round
}

/// Pick the randomized start delay for a `start_procedure`, in milliseconds — the **one** place the
/// runtime rolls the dice (heat-lifecycle Slice 2). Seeded from the wall clock so each real arm is
/// unpredictable; the chosen value is then logged as a fact, so the *replay* never calls this again.
fn pick_start_delay_ms(procedure: &StartProcedure) -> u32 {
    match procedure {
        StartProcedure::RandomizedDelay {
            min_delay_ms,
            max_delay_ms,
            ..
        } => {
            let (lo, hi) = (*min_delay_ms, *max_delay_ms);
            // Defensively clamp a mis-ordered pair to a point delay rather than panicking.
            if hi <= lo {
                return lo;
            }
            let span = (hi - lo) as u64 + 1;
            lo + (runtime_rng() % span) as u32
        }
    }
}

/// A tiny wall-clock-seeded RNG draw (a `u64`), used **only** at emission time in the runtime to pick
/// the start delay — never in the engine/projection fold. Avoids pulling in the `rand` crate for one
/// draw: a SplitMix64 step over the current monotonic-ish nanos is plenty random for a start hold.
fn runtime_rng() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    // SplitMix64 finalizer — decorrelates successive nanos so close-together arms don't draw alike.
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The race-start origin for `heat`'s win-condition evaluation: the source time of the heat's
/// **first lap-gate pass** while `Running`, matching how `completed_heats` derives `race_start`. The
/// passes carry the source clock; the first crossing opens the shared race clock. `None` until the
/// first pass lands (no crossing yet ⇒ the race-end criterion cannot be met).
fn race_start_of(passes: &[Pass]) -> Option<SourceTime> {
    passes.first().map(|p| p.at)
}

/// The grace hold, as a wall-clock `Duration`, for the completion driver: how long to keep the heat
/// `Running` for trailing pilots after the win condition is met (heat-lifecycle Slice 2). An open
/// [`GraceWindow::UntilScored`] would never auto-fire, so it is treated as **zero** here (the RD
/// would `ForceEnd` / `Finalize` such a round); a bounded [`GraceWindow::Duration`] maps its source
/// microseconds to a real-time hold of the same length.
fn grace_hold(grace: GraceWindow) -> Duration {
    match grace {
        GraceWindow::Duration { micros } if micros > 0 => Duration::from_micros(micros as u64),
        _ => Duration::ZERO,
    }
}

/// Spawn the **start driver** for a heat that just entered `Armed` (heat-lifecycle Slice 2).
///
/// Reads the heat's start procedure, picks the randomized delay **once**, appends the
/// `Event::HeatStarting { delay_ms }` fact immediately (so the console can cue the start tone and a
/// replay reads this exact delay), then — after `delay_ms` of real time — appends the
/// `HeatStateChanged { Running }` auto-transition through the shared append path (waking `/stream`).
/// The returned task is cancelled by the bridge if the heat leaves `Armed` before it fires (an abort
/// / restart / a manual `SkipCountdown`), so a superseded countdown never appends a stale `Running`.
fn spawn_start_driver(
    state: &AppState,
    registry: &EventRegistry,
    event_id: &EventId,
    heat: HeatId,
) -> JoinHandle<()> {
    let config = heat_clock_config(state, registry, event_id, &heat);
    let delay_ms = pick_start_delay_ms(&config.start_procedure);
    let state = state.clone();
    let spawn_watermark = read_tail(&state, 0)
        .ok()
        .map(|events| {
            let events: Vec<Event> = events.into_iter().map(|(_, e)| e).collect();
            latest_transition_offset(&events, &heat)
        })
        .unwrap_or(None);
    tokio::spawn(async move {
        // The chosen delay is logged as a fact *before* the hold, so a console cueing the tone and a
        // replay both read it; only the append timing below uses wall-clock.
        if let Err(e) = state.append(
            Event::HeatStarting {
                heat: heat.clone(),
                delay_ms,
            },
            None,
        ) {
            eprintln!("gridfpv: start driver could not log HeatStarting: {e:?}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;
        // Auto-advance Armed → Running — but RE-CHECK the heat is still Armed at fire time,
        // under the command serialization lock. The bridge's cancel is poll-paced (~150ms), so
        // a manual SkipCountdown/Abort landing just before this fired used to race a duplicate
        // (or stale) `Running` into the log.
        let still_armed = {
            let h = heat.clone();
            move |events: &[Event]| {
                gridfpv_engine::heat::heat_state(events, &h)
                    == Some(gridfpv_engine::heat::HeatState::Armed)
                    // …and it is the SAME arming (an abort + re-arm during this hold would
                    // read Armed again — but with a NEW countdown; this one is superseded).
                    && latest_transition_offset(events, &h) == spawn_watermark
            }
        };
        match state.append_checked(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Running,
            },
            None,
            still_armed,
        ) {
            Ok(_) => {}
            Err(e) => eprintln!("gridfpv: start driver could not append Running: {e:?}"),
        }
    })
}

/// Spawn the **completion driver** for a heat that just entered `Running` (heat-lifecycle Slice 2).
///
/// Polls the heat's running passes every [`COMPLETION_POLL`]; once the round's win condition is met
/// (the pure [`race_end_reached`] over the passes + the race-start time), it holds the configured
/// **grace window** for trailing pilots, then appends the `HeatStateChanged { Finished }`
/// auto-transition (the `Running → Unofficial` step). Cancelled by the bridge if the heat leaves
/// `Running` first (an abort / restart / a manual `ForceEnd`), so a superseded heat never appends a
/// stale `Finished`. A round whose win condition has no intrinsic end (a bare qual — see
/// [`race_end_reached`]) simply never fires here; the RD ends it with `ForceEnd`.
///
/// **Timed-window fallback:** a [`Timed`](gridfpv_engine::scoring::WinCondition::Timed) round's
/// pass-based criterion needs a crossing at/after the cutoff to fire — if every pilot lands at the
/// buzzer, no such pass ever arrives. The driver therefore also closes a Timed heat on the wall
/// clock once the window plus the grace hold has elapsed (anchored to the first observed pass, or
/// to race-go when nobody ever crossed), so a timed heat always ends on its own.
///
/// **Open-practice time limit (open-practice refinement):** when the round carries a
/// [`time_limit_secs`](gridfpv_server::events::RoundDef::time_limit_secs), the driver auto-ends the
/// heat once its elapsed running time reaches the limit — **independent of the win condition**. A
/// practice round created without one stores the inert
/// [`default_win_condition`](gridfpv_server::events::default_win_condition) (`BestLap`), which by
/// construction never ends a heat, so the time limit is in practice its only automatic end. The
/// elapsed clock starts when the heat enters `Running` (this driver's spawn), so it is the same
/// deterministic, logged transition the other autos key off — a 1-hour practice ends itself an hour
/// after Start. With no limit set, only the win-condition path can fire (the RD ends an open
/// practice manually). Nothing here branches on the format: practice runs the same driver as every
/// other round, over the same logged passes.
fn spawn_completion_driver(
    state: &AppState,
    registry: &EventRegistry,
    event_id: &EventId,
    heat: HeatId,
) -> JoinHandle<()> {
    let config = heat_clock_config(state, registry, event_id, &heat);
    // The Timed window, for the wall-clock fallback below. Every format goes through the same
    // branch — open practice included (D5, reversed 2026-08-24). A practice round created without
    // a win condition stores the inert `default_win_condition` (`BestLap`), which is not `Timed`,
    // so no window is armed and its `time_limit_secs` / the RD's `ForceEnd` remain its only end
    // conditions; a practice round that *was* given a real win condition now honours it, like any
    // other round.
    let timed_window = match config.win_condition {
        gridfpv_engine::scoring::WinCondition::Timed { window_micros } => {
            Some(Duration::from_micros(window_micros.max(0) as u64))
        }
        _ => None,
    };
    let state = state.clone();
    // The run this driver belongs to, as a spawn-time watermark (the heat's latest transition
    // offset) — the fire-time recheck stands down if ANY transition landed since.
    let spawn_watermark = read_tail(&state, 0)
        .ok()
        .map(|events| {
            let events: Vec<Event> = events.into_iter().map(|(_, e)| e).collect();
            latest_transition_offset(&events, &heat)
        })
        .unwrap_or(None);
    // The running clock origin: the moment the heat entered `Running` (this spawn). The time-limit
    // deadline, when set, is measured from here — a deterministic wall-clock span (a test drives it
    // with a short limit; production with the practice duration).
    let running_since = tokio::time::Instant::now();
    let time_limit = config
        .time_limit_secs
        .map(|secs| Duration::from_secs(secs as u64));
    let mut ticker = tokio::time::interval(COMPLETION_POLL);
    tokio::spawn(async move {
        // The wall-clock instant this driver first OBSERVED a running pass — the fallback's
        // race-clock anchor. Observation lags the true crossing by up to a poll tick (+ transport),
        // so a deadline anchored here is never *early* relative to the pass-anchored cutoff.
        let mut first_pass_seen: Option<tokio::time::Instant> = None;
        loop {
            ticker.tick().await;
            // Time-limit auto-end (open-practice refinement): once the elapsed running time reaches
            // the round's duration, close the heat regardless of any win condition or passes. For a
            // practice round created with no win condition (the inert `BestLap`, which never ends a
            // heat) this is its only automatic end. Logged like the other autos.
            if let Some(limit) = time_limit {
                if running_since.elapsed() >= limit {
                    if let Err(e) = append_finished_if_running(&state, &heat, spawn_watermark) {
                        eprintln!(
                            "gridfpv: completion driver could not append time-limit Finished: {e:?}"
                        );
                    }
                    return;
                }
            }
            let passes = heat_running_passes(&state, &heat);
            if first_pass_seen.is_none() && !passes.is_empty() {
                first_pass_seen = Some(tokio::time::Instant::now());
            }
            // Timed-window wall-clock fallback: `race_end_reached` for a Timed round only fires
            // when a lap-gate pass lands AT/AFTER the cutoff — if nobody crosses again after the
            // window ends (pilots land at the buzzer; a short time trial), the pass-based path
            // never triggers and the heat would stay `Running` forever. Once the window PLUS the
            // grace hold has elapsed on the wall clock — measured from the race-clock origin (the
            // first observed pass; race-go when nobody ever crossed) — close the heat. Grace-window
            // crossings before this deadline still land in the log and score normally; a
            // post-cutoff crossing still ends the heat earlier via the pass-based path below.
            if let Some(window) = timed_window {
                let anchor = first_pass_seen.unwrap_or(running_since);
                if anchor.elapsed() >= window + grace_hold(config.grace_window) {
                    if let Err(e) = append_finished_if_running(&state, &heat, spawn_watermark) {
                        eprintln!(
                            "gridfpv: completion driver could not append timed-window Finished: {e:?}"
                        );
                    }
                    return;
                }
            }
            let Some(race_start) = race_start_of(&passes) else {
                continue; // no crossing yet — the race clock hasn't opened
            };
            if race_end_reached(&passes, config.win_condition, race_start) {
                // The race-end criterion is met: hold the grace window for late crossings, then
                // close the race. The hold is wall-clock; the *decision* was pure.
                tokio::time::sleep(grace_hold(config.grace_window)).await;
                if let Err(e) = append_finished_if_running(&state, &heat, spawn_watermark) {
                    eprintln!("gridfpv: completion driver could not append Finished: {e:?}");
                }
                return;
            }
        }
    })
}

/// The offset of `heat`'s LATEST `HeatStateChanged` — the spawn-time watermark a driver
/// captures so its fire-time recheck can tell "still the SAME state" from "the same state
/// AGAIN": a heat that was Force-Ended and re-raced during a driver's hold is Running like
/// before, but it is a NEW run and the stale driver must stand down (state alone can't tell).
fn latest_transition_offset(events: &[Event], heat: &HeatId) -> Option<usize> {
    events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Event::HeatStateChanged { heat: h, .. } if h == heat => Some(i),
            _ => None,
        })
        .next_back()
}

/// Append `Finished` for `heat` iff it is STILL `Running` at fire time — the completion
/// driver's checked append (under the command serialization lock). The bridge's cancel is
/// poll-paced, so a ForceEnd/Abort landing just before an expiring clock used to race a
/// duplicate/stale `Finished` into the log.
fn append_finished_if_running(
    state: &AppState,
    heat: &HeatId,
    spawn_watermark: Option<usize>,
) -> Result<(), gridfpv_server::error::ProtocolError> {
    let h = heat.clone();
    state
        .append_checked(
            Event::HeatStateChanged {
                heat: heat.clone(),
                transition: HeatTransition::Finished,
            },
            None,
            move |events| {
                gridfpv_engine::heat::heat_state(events, &h)
                    == Some(gridfpv_engine::heat::HeatState::Running)
                    // …and it is the SAME run this driver was spawned for: any transition
                    // since spawn (ForceEnd + Restart + re-race in one hold) supersedes it.
                    && latest_transition_offset(events, &h) == spawn_watermark
            },
        )
        .map(|_| ())
}

/// Server wall-clock time in **microseconds** since the Unix epoch — the basis for the auto-official
/// deadline (marshaling Slice 5). Matches the server's own `recorded_at` stamping (the `Finalize` the
/// driver appends is stamped from the same clock), so `at` and the eventual transition's
/// `recorded_at` agree to within the hold's scheduling jitter.
fn now_micros() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Spawn the **auto-official driver** for a heat that just entered `Unofficial` (marshaling Slice 5).
///
/// Reads the heat's round [`ProtestWindow`]. With the default [`ProtestWindow::Off`] there is no
/// auto-official timer — the driver returns immediately and the heat stays provisional until the RD
/// finalizes manually (today's behaviour). With [`ProtestWindow::After { micros }`] it:
///   1. logs the **deadline** as a fact — `Event::HeatFinalizing { at: now + micros }` — so the
///      console can render a live "auto-official in M:SS" countdown and a replay reads the same
///      deadline (mirroring how the start driver logs `HeatStarting`);
///   2. holds `micros` of real time;
///   3. appends the auto `HeatStateChanged { Finalized }` (the `Unofficial → Final` step) —
///      **unless the heat has an open protest** (issue #338, below).
///
/// **The open-protest gate (issue #338).** The manual `Finalize` command is gated on open protests
/// (release-hardening P1-4): a filed, unresolved protest means the result is still contested. The
/// auto-official append does not run through the command handler, so it checks the *same shared
/// predicate* ([`open_protest_count`]) at window expiry. If protests are open when the window
/// elapses, the driver **stands down**: it appends nothing and leaves the heat `Unofficial` for
/// the RD. There is deliberately **no retry**: a protest pulls a human into the loop, and the RD's
/// follow-up may be several rulings (resolve, then a penalty, then finalize) — an auto-finalize
/// firing at the surprising instant the last protest resolves could race those. The RD's manual
/// `Finalize` (one click, re-checked by the same gate on the command path) closes the heat.
///
/// The returned task is cancelled by the bridge (`cancel_for`) the moment the heat leaves
/// `Unofficial` — a manual early `Finalize`, a `Revert`, an abort/restart — so a superseded window
/// never appends a stale `Finalized`. A `Revert` back to `Unofficial` re-arms a fresh driver (a new
/// window from the new race-end instant). This is an **additive auto-finalize**, never a gate: the
/// RD's manual `Finalize` stays available and simply pre-empts the timer.
fn spawn_auto_official_driver(
    state: &AppState,
    registry: &EventRegistry,
    event_id: &EventId,
    heat: HeatId,
) -> JoinHandle<()> {
    let config = heat_clock_config(state, registry, event_id, &heat);
    let state = state.clone();
    let spawn_watermark = read_tail(&state, 0)
        .ok()
        .map(|events| {
            let events: Vec<Event> = events.into_iter().map(|(_, e)| e).collect();
            latest_transition_offset(&events, &heat)
        })
        .unwrap_or(None);
    tokio::spawn(async move {
        let ProtestWindow::After { micros } = config.protest_window else {
            // Off (the default): no auto-official timer — manual finalize only. Nothing to do.
            return;
        };
        // A non-positive window auto-finalizes immediately (defensive — the form clamps to ≥ 0).
        let hold = Duration::from_micros(micros.max(0) as u64);
        let deadline = now_micros().saturating_add(micros.max(0));
        // Log the deadline as a fact *before* the hold, so the console can count down to it and a
        // replay reads the same instant; only the append timing below uses wall-clock.
        if let Err(e) = state.append(
            Event::HeatFinalizing {
                heat: heat.clone(),
                at: deadline,
            },
            None,
        ) {
            eprintln!("gridfpv: auto-official driver could not log HeatFinalizing: {e:?}");
            return;
        }
        tokio::time::sleep(hold).await;
        // The expired window must not finalize over an **open protest** (issue #338): check the
        // same predicate the manual `Finalize` command is gated on (P1-4). A protest filed during
        // (or before) the window means the result is still contested — stand down and leave the
        // heat `Unofficial` for the RD (see the doc comment for why there is no retry). A log read
        // failure also stands down: fail closed, never finalize blind.
        let open = state
            .log()
            .lock()
            .ok()
            .and_then(|g| g.read_all().ok())
            .map(|stored| {
                let events: Vec<Event> = stored.into_iter().map(|s| s.event).collect();
                open_protest_count(&events, &heat)
            });
        match open {
            Some(0) => {}
            Some(n) => {
                eprintln!(
                    "gridfpv: auto-official window for heat {:?} expired with {n} open protest(s); \
                     leaving it Unofficial for the RD to resolve and finalize",
                    heat.0
                );
                return;
            }
            None => {
                eprintln!(
                    "gridfpv: auto-official driver could not read the log for heat {:?}; \
                     leaving it Unofficial",
                    heat.0
                );
                return;
            }
        }
        // Auto-finalize Unofficial → Final — RE-CHECKED at fire time under the command
        // serialization lock: the heat must STILL be Unofficial with no open protest at the
        // instant of the append. The bridge's cancel is poll-paced, so a Revert (or a fresh
        // protest) landing just before the window expired used to race a stale `Finalized` in
        // — instantly re-finalizing the heat the RD had just re-opened.
        let h = heat.clone();
        let gate = move |events: &[Event]| {
            gridfpv_engine::heat::heat_state(events, &h)
                == Some(gridfpv_engine::heat::HeatState::Unofficial)
                // …the SAME provisional window (a revert→finalize→revert chain during the
                // hold reads Unofficial again — with a NEW window; this one is superseded)…
                && latest_transition_offset(events, &h) == spawn_watermark
                && open_protest_count(events, &h) == 0
        };
        match state.append_checked(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Finalized,
            },
            None,
            gate,
        ) {
            Ok(_) => {}
            Err(e) => eprintln!("gridfpv: auto-official driver could not append Finalized: {e:?}"),
        }
    })
}

/// The lap-gate passes attributed to `heat`'s current run: every lap-gate [`Pass`] in the log since
/// the heat last entered `Running`. The completion driver scores over exactly the running window, so
/// an earlier aborted run's passes don't count toward this run's win condition.
fn heat_running_passes(state: &AppState, heat: &HeatId) -> Vec<Pass> {
    let Some(stored) = state.log().lock().ok().and_then(|g| g.read_all().ok()) else {
        return Vec::new();
    };
    // Walk the log: the most recent `Running` for this heat opens the window; collect lap-gate
    // passes after it until the heat leaves Running. (Passes carry no heat id — while a heat is
    // Running it is the only one consuming, mirroring the bridge's single-active-heat rule.)
    let mut running = false;
    let mut passes = Vec::new();
    for s in stored {
        match s.event {
            Event::HeatStateChanged {
                heat: ref h,
                transition,
            } if h == heat => match transition {
                HeatTransition::Running => {
                    running = true;
                    passes.clear(); // a fresh run resets the window
                }
                _ => running = false,
            },
            // A tagged pass belongs to its stamped heat regardless of the positional cursor
            // (same rule as the server's window folds); an untagged (legacy) pass keeps the
            // positional rule. Either way only while this heat's run window is open.
            Event::Pass(p)
                if running && p.gate.is_lap_gate() && p.heat.as_ref().is_none_or(|h| h == heat) =>
            {
                passes.push(p)
            }
            _ => {}
        }
    }
    passes
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

    use gridfpv_events::RoundId;
    use gridfpv_server::events::{CreateEventRequest, EventRegistry};
    use gridfpv_server::live_state::live_state;
    use gridfpv_server::timers::{
        CreateTimerRequest, MOCK_TIMER_ID, TimerId, TimerKind, TimerStatus, UpdateTimerRequest,
    };
    use tokio::time::{Instant, sleep, timeout};

    /// The id of the one event a bridge-test registry holds (its log + default `["mock"]`
    /// selection). There is no built-in event any more (#414): [`fast_registry`] creates one
    /// through the real creation path, and this reads it back.
    fn event_of(registry: &EventRegistry) -> EventId {
        let mut list = registry.list();
        assert_eq!(list.len(), 1, "one created event per bridge-test registry");
        EventId(list.remove(0).id.0)
    }

    /// A registry holding exactly one **created** event — the fixture that replaced the built-in
    /// Practice event (#414). Going through `create` means the bridge tests drive the same kind
    /// of event an RD makes, with the same default `["mock"]` timer selection.
    fn test_registry() -> EventRegistry {
        let registry = EventRegistry::new(None).unwrap();
        registry
            .create(&CreateEventRequest::named("Test Event"))
            .expect("create the test event");
        registry
    }

    /// Build a [`test_registry`] and retune the built-in **Mock** to a fast pace (`lap_ms`) and
    /// the wanted `laps`, so the whole heat runs in a few ms. A new event defaults to selecting
    /// the Mock, so the bridge over it drives this retuned source. The bridge polls at
    /// [`POLL_INTERVAL`], which dominates start-up latency, so tests keep total laps small.
    fn fast_registry(laps: u32, lap_ms: u64) -> EventRegistry {
        let registry = test_registry();
        registry
            .timers()
            .update(
                &TimerId(MOCK_TIMER_ID.to_string()),
                &UpdateTimerRequest {
                    name: None,
                    kind: Some(TimerKind::Mock { laps, lap_ms }),
                    ..Default::default()
                },
            )
            .unwrap();
        registry
    }

    /// Spawn the selection-aware bridge for `registry`'s event, returning the bridge handle and
    /// that event's `AppState` (the same log the bridge polls), so a test appends the
    /// schedule/transition events the bridge reacts to.
    fn spawn_bridge_for(registry: &EventRegistry) -> (JoinHandle<()>, AppState) {
        let event = event_of(registry);
        let state = registry.resolve(&event).unwrap();
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
                event,
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
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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

    /// #412: the seat/tune emits must carry the **real** node index, not the lineup position.
    ///
    /// This is the failure the feature exists to prevent, one layer down: RotorHazard's
    /// `alter_heat` and `set_frequency` are keyed on `seat_index`, so a heat whose third pilot is
    /// pushed as node 2 while the enabled set says node 3 seats them on the dead gate — and their
    /// laps land on somebody else's row.
    #[tokio::test]
    async fn seating_and_tuning_carry_the_real_node_indices_over_a_disabled_node() {
        use gridfpv_server::pilots::CreatePilotRequest;
        use gridfpv_server::timers::SetTimerNodesRequest;

        let registry = test_registry();
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        // The timer reports four nodes; the RD has switched off the third one ("Node 3" = index 2).
        registry.timers().set_reported_nodes(&rh.id, 4);
        registry
            .timers()
            .set_nodes(
                &rh.id,
                &SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some(vec![0, 1, 3]),
                },
            )
            .unwrap();
        let timer = registry.timers().get(&rh.id).unwrap();

        // Three pilots in the directory, so the seating resolves callsigns rather than raw ids.
        let mut refs = Vec::new();
        for callsign in ["Ace", "Bolt", "Cyan"] {
            let pilot = registry
                .pilots()
                .create(&CreatePilotRequest {
                    callsign: callsign.into(),
                    ..Default::default()
                })
                .unwrap();
            refs.push(CompetitorRef(pilot.id.0));
        }

        let state = registry.resolve(&event_of(&registry)).unwrap();
        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: refs.clone(),
                    class: None,
                    round: None,
                    frequencies: vec![
                        (refs[0].clone(), 5658),
                        (refs[1].clone(), 5695),
                        (refs[2].clone(), 5732),
                    ],
                    label: None,
                },
                None,
            )
            .unwrap();

        // The seating emit: node 0, node 1, node **3** — with callsigns, never raw ids.
        let seats = seats_of(&state, &registry, &timer, &heat);
        assert_eq!(
            seats,
            vec![
                (0, "Ace".to_string()),
                (1, "Bolt".to_string()),
                (3, "Cyan".to_string()),
            ],
            "the third pilot sits on node 3, not node 2"
        );

        // The tune emit follows the same seats: node 3 gets Cyan's channel, and the disabled node 2
        // is offered no channel at all.
        let plan = tune_plan_of(&state, &timer, &heat);
        assert_eq!(plan, vec![(0, 5658), (1, 5695), (3, 5732)]);
        assert!(
            !plan.iter().any(|(node, _)| *node == 2),
            "a disabled node must never be tuned: {plan:?}"
        );
    }

    #[tokio::test]
    async fn an_event_selecting_only_rotorhazard_emits_nothing() {
        // The SIM bridge never speaks for a RotorHazard timer: an event whose ONLY selected timer
        // is RotorHazard must emit no *synthetic* passes when its heat runs — its real passes arrive
        // through the RH adapter connection instead (#65/#73).
        use gridfpv_server::timers::CreateTimerRequest;
        let registry = test_registry();
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        // Select only the RotorHazard timer for Practice.
        registry
            .set_timers(&event_of(&registry), vec![rh.id])
            .unwrap();
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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
        let registry = test_registry();
        let timer = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Fast Sim".into(),
                kind: TimerKind::Mock { laps: 2, lap_ms: 1 },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        registry
            .set_timers(&event_of(&registry), vec![timer.id])
            .unwrap();
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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

    // --- race redesign Slice 1a: sim auto-presence reconciler ---------------------------------

    /// Spawn the presence reconciler for `registry`'s event, returning its handle and that
    /// event's `AppState` (the same log it polls), mirroring [`spawn_bridge_for`].
    fn spawn_reconciler_for(registry: &EventRegistry) -> (JoinHandle<()>, AppState) {
        let event = event_of(registry);
        let state = registry.resolve(&event).unwrap();
        let reg = registry.clone();
        let reconciler_state = state.clone();
        let handle = tokio::spawn(async move {
            run_presence_reconciler(reconciler_state, reg, event).await;
        });
        (handle, state)
    }

    /// Read the `CompetitorRegistered` bindings currently in the log, as `(competitor, pilot)`.
    fn bindings_in(state: &AppState) -> Vec<(String, String)> {
        read_all_events(state)
            .into_iter()
            .filter_map(|e| match e {
                Event::CompetitorRegistered {
                    competitor, pilot, ..
                } => Some((competitor.0, pilot.0)),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn seen_player_matching_a_rostered_pilot_is_added_and_bound() {
        use gridfpv_server::pilots::CreatePilotRequest;

        let registry = test_registry();
        // A directory pilot whose callsign matches the sim player name (case/space-insensitively).
        let pilot = registry
            .pilots()
            .create(&CreatePilotRequest {
                callsign: "AcroAce".into(),
                ..Default::default()
            })
            .unwrap();

        let (reconciler, state) = spawn_reconciler_for(&registry);

        // The sim adapter reports a player by name — with surrounding whitespace and different case
        // to prove the match is trimmed + case-insensitive.
        state
            .append(
                Event::CompetitorSeen {
                    adapter: AdapterId(SIM_ADAPTER.into()),
                    competitor: CompetitorRef("  acroace ".into()),
                },
                None,
            )
            .unwrap();

        // The pilot is added to the roster (= present) and a binding is appended.
        let pilot_id = pilot.id.clone();
        timeout(Duration::from_secs(5), async {
            loop {
                let rostered = registry
                    .meta_of(&event_of(&registry))
                    .map(|m| m.roster.contains(&pilot_id))
                    .unwrap_or(false);
                if rostered && !bindings_in(&state).is_empty() {
                    return;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the matching pilot should be auto-added and bound");

        let bindings = bindings_in(&state);
        assert_eq!(
            bindings,
            vec![("  acroace ".to_string(), pilot.id.0.clone())]
        );

        // Idempotent: re-seeing the SAME player (identical adapter+competitor — what the sim
        // adapter actually re-emits) adds no second roster entry and no second binding.
        state
            .append(
                Event::CompetitorSeen {
                    adapter: AdapterId(SIM_ADAPTER.into()),
                    competitor: CompetitorRef("  acroace ".into()),
                },
                None,
            )
            .unwrap();
        sleep(POLL_INTERVAL * 3).await;
        assert_eq!(
            registry.meta_of(&event_of(&registry)).unwrap().roster,
            vec![pilot.id.clone()],
            "presence is set-membership — no duplicate roster entry"
        );
        assert_eq!(
            bindings_in(&state).len(),
            1,
            "the already-bound competitor is not re-bound"
        );

        reconciler.abort();
    }

    #[tokio::test]
    async fn seen_player_with_no_matching_pilot_is_a_no_op() {
        let registry = test_registry();
        // No directory pilot named "Stranger".
        let (reconciler, state) = spawn_reconciler_for(&registry);

        state
            .append(
                Event::CompetitorSeen {
                    adapter: AdapterId(SIM_ADAPTER.into()),
                    competitor: CompetitorRef("Stranger".into()),
                },
                None,
            )
            .unwrap();

        // Wait past several poll cycles: the roster stays empty and no binding is appended.
        sleep(POLL_INTERVAL * 4).await;
        assert!(
            registry
                .meta_of(&event_of(&registry))
                .unwrap()
                .roster
                .is_empty(),
            "an unmatched seen player must not be added to the roster"
        );
        assert!(
            bindings_in(&state).is_empty(),
            "an unmatched seen player must not be bound"
        );
        reconciler.abort();
    }

    // --- open practice (open-practice format, Slice 1): laps in memory, not logged ----------------

    /// Add an **open-practice** round to Practice (open-practice format) over `channels` (node
    /// indices) and return its `RoundId`. Uses the registry's `add_round` so the bridge resolves the
    /// round through `rounds_of` exactly as it does in production.
    fn add_open_practice_round(registry: &EventRegistry, channels: Vec<usize>) -> RoundId {
        add_open_practice_round_with_limit(registry, channels, None)
    }

    /// As [`add_open_practice_round`], but with an optional **time limit** (open-practice refinement)
    /// — an open-practice round that has **no win condition** (the form omits it; the inert default is
    /// stored) and whose only end condition is the `time_limit_secs` practice duration.
    fn add_open_practice_round_with_limit(
        registry: &EventRegistry,
        channels: Vec<usize>,
        time_limit_secs: Option<u32>,
    ) -> RoundId {
        use gridfpv_server::events::{NewRoundReq, SeedingRule};
        use gridfpv_server::scope::EventId as ScopeEventId;
        let req = NewRoundReq {
            label: "Open Practice".into(),
            classes: vec![],
            format: "open_practice".into(),
            params: std::collections::BTreeMap::new(),
            // No win condition supplied — an open-practice round does no scoring (the inert default
            // is stored by `add_round`). The practice ends on the time limit (or the RD's ForceEnd).
            win_condition: None,
            time_limit_secs,
            seeding: SeedingRule::AllChannels { channels },
            channel_mode: None,
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        };
        registry
            .add_round(&ScopeEventId(event_of(registry).0), req)
            .expect("open-practice round added")
            .id
    }

    /// Schedule an open-practice heat (tagged with `round`, the channel lineup) and drive it to
    /// `Running` on Practice's log — exactly what `FillRound` + the control path append.
    fn start_open_practice_heat(state: &AppState, round: &RoundId, channels: &[usize]) -> HeatId {
        let heat = HeatId("open-practice".into());
        let lineup: Vec<CompetitorRef> = channels
            .iter()
            .map(|i| CompetitorRef(format!("node-{i}")))
            .collect();
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup,
                    class: None,
                    round: Some(round.clone()),
                    frequencies: vec![],
                    label: None,
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
        heat
    }

    #[tokio::test]
    async fn open_practice_laps_are_logged_like_every_other_format_and_drive_live_state() {
        // D5, reversed (#398): an open-practice heat's passes are appended to the **durable log**
        // exactly like any other format's — stamped with the heat — and the ordinary log fold is
        // what shows the per-channel laps. There is no accumulator and no overlay.
        let laps = 3u32;
        let registry = fast_registry(laps, 1);
        let round = add_open_practice_round(&registry, vec![0, 1]);
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = start_open_practice_heat(&state, &round, &[0, 1]);

        // Wait until the LOG-derived live state shows both channels' laps (holeshot + `laps`).
        let target = heat.clone();
        timeout(Duration::from_secs(5), async {
            loop {
                let ls = gridfpv_server::live_state::live_state(&read_all_events(&state));
                if ls.current_heat.as_ref() == Some(&target)
                    && ls.progress.len() == 2
                    && ls.progress.iter().all(|p| p.laps_completed >= laps)
                {
                    return;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the log fold should show the practice laps");

        // (a) The passes really are on the durable log, tagged with the practice heat.
        let events = read_all_events(&state);
        assert!(
            count_passes(&events) > 0,
            "an open-practice heat appends its Pass events to the log like any other heat"
        );
        assert!(
            events.iter().all(|e| match e {
                Event::Pass(p) => p.heat.as_ref() == Some(&heat),
                _ => true,
            }),
            "every practice pass is stamped with the heat it was flown in"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::HeatScheduled { round: Some(r), .. } if *r == round)),
            "the heat's HeatScheduled (the session) is logged"
        );

        // (b) The live state shows per-channel laps, each channel unbound (pilot: None) — practice
        // seats need no pilot binding for their laps to be logged and folded.
        let ls = gridfpv_server::live_state::live_state(&events);
        assert_eq!(ls.progress.len(), 2);
        assert!(ls.progress.iter().all(|p| p.pilot.is_none()));
        assert!(ls.progress.iter().all(|p| p.laps_completed >= laps));
        assert_eq!(ls.current_heat, Some(heat.clone()));

        // (c) A reset drops the run's laps through the SAME rule every format uses — the heat window
        // starts past its latest `Aborted`/`Restarted`/`Discarded` — not through a special clear.
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat.clone(),
                    transition: HeatTransition::Aborted,
                },
                None,
            )
            .unwrap();
        let after_reset = gridfpv_server::live_state::live_state(&read_all_events(&state));
        assert!(
            after_reset.progress.iter().all(|p| p.laps_completed == 0),
            "an Abort resets the practice lap counts, exactly as it does for a qualifying heat"
        );
        // The passes themselves stay on the log — a reset windows them out, it does not erase them.
        assert!(count_passes(&read_all_events(&state)) > 0);
        bridge.abort();
    }

    #[tokio::test]
    async fn open_practice_time_limit_auto_ends_the_running_heat() {
        // Open-practice refinement: an open-practice round with **no win condition** but a
        // `time_limit_secs` auto-ends its running heat (Running → Unofficial / a `Finished`
        // transition) once the elapsed running time reaches the limit. Its passes ARE logged
        // (D5, reversed) — the win-condition path stays silent because the stored inert
        // `default_win_condition` (`BestLap`) has no end criterion, not because the log is empty.
        let registry = fast_registry(3, 1);
        // A 1s practice duration (the minimum the seconds field allows): short enough for a test,
        // long enough that we can assert it does NOT fire immediately.
        let round = add_open_practice_round_with_limit(&registry, vec![0, 1], Some(1));
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = start_open_practice_heat(&state, &round, &[0, 1]);

        // The heat must still be Running shortly after Start — the limit has not elapsed yet, so no
        // premature `Finished` is appended.
        sleep(Duration::from_millis(200)).await;
        assert_eq!(
            gridfpv_engine::heat::heat_state(&read_all_events(&state), &heat),
            Some(gridfpv_engine::heat::HeatState::Running),
            "the practice must keep running before its time limit elapses"
        );

        // Within a little over the 1s limit, the runtime auto-appends exactly one `Finished` (the
        // Running → Unofficial step) with no `ForceEnd` ever sent.
        let target = heat.clone();
        timeout(
            Duration::from_secs(4),
            wait_until(&state, Duration::from_secs(4), move |events| {
                gridfpv_engine::heat::heat_state(events, &target)
                    == Some(gridfpv_engine::heat::HeatState::Unofficial)
            }),
        )
        .await
        .expect("the time limit should auto-end the open-practice heat");

        let events = read_all_events(&state);
        let finished = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Event::HeatStateChanged {
                        heat: h,
                        transition: HeatTransition::Finished,
                    } if *h == heat
                )
            })
            .count();
        assert_eq!(
            finished, 1,
            "the time limit auto-appends exactly one Finished (Running → Unofficial)"
        );
        // The practice heat's passes ARE on the log — the time limit, not an empty log, is what
        // ended it (the inert `BestLap` win condition never fires).
        assert!(count_passes(&events) > 0);
        bridge.abort();
    }

    #[tokio::test]
    async fn timed_heat_auto_ends_when_no_pass_lands_after_the_window() {
        // The timed-window wall-clock fallback: a Timed round's pass-based end criterion
        // (`race_end_reached`) needs a lap-gate pass AT/AFTER the cutoff — here every pass lands
        // well BEFORE it (the sim finishes its laps in a few ms; the pilots "landed at the
        // buzzer"), so without the fallback the heat would stay `Running` forever. The completion
        // driver must close it on the wall clock once window + grace elapses.
        use gridfpv_engine::scoring::WinCondition;
        use gridfpv_server::events::{NewRoundReq, SeedingRule};
        use gridfpv_server::scope::EventId as ScopeEventId;

        let registry = fast_registry(2, 1); // holeshot + 2 laps per pilot, all inside ~10ms
        let req = NewRoundReq {
            label: "Short Time".into(),
            classes: vec![],
            format: "timed_qual".into(),
            params: std::collections::BTreeMap::new(),
            // A 1s window: long enough to assert no premature Finished, short enough for a test.
            win_condition: Some(WinCondition::Timed {
                window_micros: 1_000_000,
            }),
            time_limit_secs: None,
            seeding: SeedingRule::FromRoster,
            channel_mode: None,
            staging_timer_secs: None,
            start_procedure: None,
            // Zero grace so the fallback deadline IS the window end.
            grace_window: Some(GraceWindow::Duration { micros: 0 }),
            protest_window: None,
            min_lap_secs: None,
        };
        let round = registry
            .add_round(&ScopeEventId(event_of(&registry).0), req)
            .expect("timed round added")
            .id;
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: Some(round),
                    frequencies: vec![],
                    label: None,
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

        // All passes land within a few ms — long before the 1s cutoff — and the heat must still be
        // Running mid-window (no premature close).
        sleep(Duration::from_millis(300)).await;
        assert_eq!(
            gridfpv_engine::heat::heat_state(&read_all_events(&state), &heat),
            Some(gridfpv_engine::heat::HeatState::Running),
            "the timed heat must keep running until its window elapses"
        );

        // Once the window (anchored at the first pass) elapses, the fallback appends Finished even
        // though no pass ever landed at/after the cutoff.
        let target = heat.clone();
        timeout(
            Duration::from_secs(4),
            wait_until(&state, Duration::from_secs(4), move |events| {
                gridfpv_engine::heat::heat_state(events, &target)
                    == Some(gridfpv_engine::heat::HeatState::Unofficial)
            }),
        )
        .await
        .expect("the timed window should auto-end the heat with no post-cutoff pass");

        let events = read_all_events(&state);
        let finished = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Event::HeatStateChanged {
                        heat: h,
                        transition: HeatTransition::Finished,
                    } if *h == heat
                )
            })
            .count();
        assert_eq!(finished, 1, "exactly one auto Finished");
        bridge.abort();
    }

    #[tokio::test]
    async fn bridge_startup_replays_nothing_over_a_finished_log() {
        // The restart-replay bug: a fresh bridge over a log holding a fully-raced heat used to
        // re-fire the historical transitions (a spurious HeatStarting, re-spawned sim sources
        // whose passes corrupted the scored window). Startup must append NOTHING for history.
        let registry = fast_registry(2, 1);
        let state = registry.resolve(&event_of(&registry)).unwrap();
        let heat = HeatId("q-old".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
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
        }
        for (at, seq) in [(1_000_000, 1), (2_000_000, 2)] {
            state
                .append(
                    Event::Pass(Pass {
                        adapter: AdapterId(SIM_ADAPTER.to_string()),
                        competitor: CompetitorRef("A".into()),
                        at: SourceTime::from_micros(at),
                        sequence: Some(seq),
                        gate: GateIndex::LAP,
                        signal: None,
                        heat: Some(heat.clone()),
                    }),
                    None,
                )
                .unwrap();
        }
        for transition in [HeatTransition::Finished, HeatTransition::Finalized] {
            state
                .append(
                    Event::HeatStateChanged {
                        heat: heat.clone(),
                        transition,
                    },
                    None,
                )
                .unwrap();
        }
        let before = read_all_events(&state).len();

        // A fresh bridge over that log (the Director restart): give it a moment to (not) act.
        let (bridge, state) = spawn_bridge_for(&registry);
        sleep(Duration::from_millis(600)).await;
        let after = read_all_events(&state).len();
        assert_eq!(
            before, after,
            "startup must not replay history (no spurious HeatStarting/Running/passes)"
        );
        bridge.abort();
    }

    #[tokio::test]
    async fn overlapping_protest_windows_do_not_orphan_the_older_heats_timer() {
        // The single-slot detach bug: heat 1 finishes (protest window armed), heat 2 finishes
        //
        // while heat 1's window is still open — installing heat 2's timer used to DETACH heat
        // 1's, so discarding heat 1 could not cancel it and the orphan later force-finalized
        // the discarded heat. With per-heat timers + the fire-time recheck, heat 1 must stay
        // Scheduled after its discard, no matter what the old timer thought.
        let registry = fast_registry(2, 1);
        let round = add_protest_window_round(&registry, 700_000); // 0.7s windows
        let (bridge, state) = spawn_bridge_for(&registry);

        let schedule = |id: &str| Event::HeatScheduled {
            heat: HeatId(id.into()),
            lineup: vec![CompetitorRef("A".into())],
            class: None,
            round: Some(round.clone()),
            frequencies: vec![],
            label: None,
        };
        let changed = |id: &str, t: HeatTransition| Event::HeatStateChanged {
            heat: HeatId(id.into()),
            transition: t,
        };
        // Heat 1 finishes -> its 0.7s auto-official window arms.
        state.append(schedule("q-1"), None).unwrap();
        state
            .append(changed("q-1", HeatTransition::Finished), None)
            .unwrap();
        // Heat 2 finishes inside heat 1's window -> a SECOND live protest timer.
        sleep(Duration::from_millis(200)).await;
        state.append(schedule("q-2"), None).unwrap();
        state
            .append(changed("q-2", HeatTransition::Finished), None)
            .unwrap();
        // The RD discards heat 1 while its window is still open.
        sleep(Duration::from_millis(100)).await;
        state
            .append(changed("q-1", HeatTransition::Discarded), None)
            .unwrap();

        // Well past both windows: heat 1 must still be Scheduled (the discard stands); heat 2
        // auto-finalized normally.
        let target1 = HeatId("q-1".into());
        let target2 = HeatId("q-2".into());
        timeout(
            Duration::from_secs(4),
            wait_until(&state, Duration::from_secs(4), move |events| {
                gridfpv_engine::heat::heat_state(events, &target2)
                    == Some(gridfpv_engine::heat::HeatState::Final)
            }),
        )
        .await
        .expect("heat 2's window should auto-finalize it");
        sleep(Duration::from_millis(500)).await;
        assert_eq!(
            gridfpv_engine::heat::heat_state(&read_all_events(&state), &target1),
            Some(gridfpv_engine::heat::HeatState::Scheduled),
            "the discarded heat must never be finalized by an orphaned timer"
        );
        bridge.abort();
    }

    // --- issue #338: the auto-official driver respects the open-protest gate ---------------------

    /// Add a normal scored round (`timed_qual`) with an **armed protest window** to Practice and
    /// return its `RoundId` — the config the auto-official driver reads (`ProtestWindow::After` ⇒
    /// auto-finalize once the window elapses). Uses the registry's `add_round` so the bridge
    /// resolves the round through `rounds_of` exactly as in production.
    fn add_protest_window_round(registry: &EventRegistry, window_micros: i64) -> RoundId {
        use gridfpv_engine::scoring::WinCondition;
        use gridfpv_server::events::{NewRoundReq, SeedingRule};
        use gridfpv_server::scope::EventId as ScopeEventId;
        let req = NewRoundReq {
            label: "Qualifying".into(),
            classes: vec![],
            format: "timed_qual".into(),
            params: std::collections::BTreeMap::new(),
            win_condition: Some(WinCondition::Timed {
                window_micros: 120_000_000,
            }),
            time_limit_secs: None,
            seeding: SeedingRule::FromRoster,
            channel_mode: None,
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: Some(ProtestWindow::After {
                micros: window_micros,
            }),
            min_lap_secs: None,
        };
        registry
            .add_round(&ScopeEventId(event_of(registry).0), req)
            .expect("protest-window round added")
            .id
    }

    /// Schedule a heat tagged with `round` and end its race directly (`Finished` lands it in
    /// `Unofficial`) — the transition the bridge observes to arm the auto-official driver.
    fn finish_round_heat(state: &AppState, round: &RoundId) -> HeatId {
        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: Some(round.clone()),
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat.clone(),
                    transition: HeatTransition::Finished,
                },
                None,
            )
            .unwrap();
        heat
    }

    /// Whether the log carries the auto (or manual) `Finalized` transition for `heat`.
    fn finalized_in(events: &[Event], heat: &HeatId) -> bool {
        events.iter().any(|e| {
            matches!(
                e,
                Event::HeatStateChanged {
                    heat: h,
                    transition: HeatTransition::Finalized,
                } if h == heat
            )
        })
    }

    #[tokio::test]
    async fn auto_official_finalizes_after_the_window_with_no_protests() {
        // The happy path (marshaling Slice 5): a round with a protest window auto-finalizes its
        // Unofficial heat once the window elapses — the driver logs the `HeatFinalizing` deadline,
        // holds the window, and appends the `Finalized` transition, with no protest on file.
        let registry = fast_registry(3, 1);
        let round = add_protest_window_round(&registry, 200_000); // a 0.2 s window
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = finish_round_heat(&state, &round);

        let target = heat.clone();
        timeout(
            Duration::from_secs(5),
            wait_until(&state, Duration::from_secs(5), move |events| {
                finalized_in(events, &target)
            }),
        )
        .await
        .expect("the auto-official driver should finalize once the window elapses");

        let events = read_all_events(&state);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::HeatFinalizing { heat: h, .. } if *h == heat)),
            "the driver logs the deadline fact before the hold"
        );
        assert_eq!(
            gridfpv_engine::heat::heat_state(&events, &heat),
            Some(gridfpv_engine::heat::HeatState::Final),
            "the heat folds to Final after the auto-official append"
        );
        bridge.abort();
    }

    #[tokio::test]
    async fn auto_official_stands_down_on_an_open_protest_until_resolved() {
        // Issue #338: the protest window expiring must NOT finalize over an OPEN protest — the
        // same gate the manual `Finalize` command enforces (P1-4). The driver stands down and
        // leaves the heat Unofficial; resolving the protest then lets the RD's manual `Finalize`
        // (the chosen behavior — no auto-retry) close the heat.
        use gridfpv_events::{LogRef, ProtestOutcome};
        use gridfpv_server::control::Command;
        use gridfpv_server::control_handler::apply_command;

        let registry = fast_registry(3, 1);
        let round = add_protest_window_round(&registry, 200_000); // a 0.2 s window
        let (bridge, state) = spawn_bridge_for(&registry);

        // File the protest BEFORE the race ends, so it is open for the whole window — no timing
        // race between the filing and the driver's expiry check.
        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: Some(round.clone()),
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        let filed = state
            .append(
                Event::ProtestFiled {
                    heat: heat.clone(),
                    competitor: CompetitorRef("A".into()),
                    note: "contested line cut".into(),
                },
                None,
            )
            .unwrap();
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat.clone(),
                    transition: HeatTransition::Finished,
                },
                None,
            )
            .unwrap();

        // The driver still arms (it logs the deadline — the console countdown runs as usual)...
        let target = heat.clone();
        timeout(
            Duration::from_secs(5),
            wait_until(&state, Duration::from_secs(5), move |events| {
                events
                    .iter()
                    .any(|e| matches!(e, Event::HeatFinalizing { heat: h, .. } if *h == target))
            }),
        )
        .await
        .expect("the driver logs the deadline even with a protest on file");

        // ...but well past the window (bridge poll + 0.2 s hold + slack) it has appended NO
        // `Finalized`: the heat stays Unofficial for the RD.
        sleep(Duration::from_millis(800)).await;
        let events = read_all_events(&state);
        assert!(
            !finalized_in(&events, &heat),
            "the expired window must not finalize over an open protest"
        );
        assert_eq!(
            gridfpv_engine::heat::heat_state(&events, &heat),
            Some(gridfpv_engine::heat::HeatState::Unofficial),
            "the heat is left Unofficial for the RD"
        );

        // The manual path agrees while the protest is open (the shared predicate)...
        let ack = apply_command(&state, Command::Finalize { heat: heat.clone() });
        assert!(
            !ack.ok,
            "manual Finalize is blocked by the same open-protest gate"
        );

        // ...and resolving the protest unblocks the RD's manual Finalize (the chosen behavior:
        // once a protest pulled a human into the loop, closing the heat is the RD's click).
        state
            .append(
                Event::ProtestResolved {
                    target: LogRef(filed),
                    outcome: ProtestOutcome::Denied,
                },
                None,
            )
            .unwrap();
        let ack = apply_command(&state, Command::Finalize { heat: heat.clone() });
        assert!(ack.ok, "Finalize succeeds once the protest is resolved");
        assert_eq!(
            gridfpv_engine::heat::heat_state(&read_all_events(&state), &heat),
            Some(gridfpv_engine::heat::HeatState::Final),
            "the resolved-then-finalized heat folds to Final"
        );
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

    // --- issue #112: primary/alternate roles + single-active-source feed + failover -------------

    /// Create a second fast **Mock** timer in `registry` and return its id (a redundant timer for
    /// the failover/double-count tests).
    fn create_mock(registry: &EventRegistry, name: &str, laps: u32, lap_ms: u64) -> TimerId {
        registry
            .timers()
            .create(&CreateTimerRequest {
                name: name.into(),
                kind: TimerKind::Mock { laps, lap_ms },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap()
            .id
    }

    /// Drive a Running heat for `lineup` on Practice's log and return its `HeatId`.
    fn start_heat(state: &AppState, lineup: Vec<CompetitorRef>) -> HeatId {
        let heat = HeatId("q-1".into());
        state
            .append(
                Event::HeatScheduled {
                    heat: heat.clone(),
                    lineup,
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
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
        heat
    }

    #[tokio::test]
    async fn two_healthy_timers_feed_only_the_primary_no_double_count() {
        // The double-count fix (issue #112): two redundant Mock timers at the same gate are BOTH
        // healthy, but only the **primary** feeds the log — so the same crossing is counted once,
        // not twice. Before #112 every selected timer fed, doubling every pass.
        let laps = 3u32;
        let registry = fast_registry(laps, 1);
        let primary = TimerId(MOCK_TIMER_ID.to_string());
        let alternate = create_mock(&registry, "Backup", laps, 1);
        // Select both; the first (the built-in Mock) is the default primary.
        registry
            .set_timers(&event_of(&registry), vec![primary.clone(), alternate])
            .unwrap();
        let (bridge, state) = spawn_bridge_for(&registry);

        start_heat(&state, vec![CompetitorRef("A".into())]);

        // One pilot, holeshot + `laps` = laps+1 passes — for ONE timer, even though two are healthy.
        let expected = laps as usize + 1;
        timeout(
            Duration::from_secs(5),
            wait_until(&state, Duration::from_secs(5), move |events| {
                count_passes(events) >= expected
            }),
        )
        .await
        .expect("the primary should emit all its passes");
        // Settle past several poll/lap cycles and assert the count never exceeded one timer's worth.
        sleep(POLL_INTERVAL * 4).await;
        assert_eq!(
            count_passes(&read_all_events(&state)),
            expected,
            "only the primary feeds — two healthy timers must NOT double-count"
        );
        bridge.abort();
    }

    #[tokio::test]
    async fn fails_over_to_alternate_when_the_primary_drops_mid_heat() {
        // Primary = an RH timer (its health is the Director-driven connection status, toggled here),
        // alternate = a fast Mock. While the RH primary is `Connected` it is the active source — but
        // a non-`live` build has no RH connection feeding, so NO passes land (the Mock alternate is
        // gated off, hot standby). Dropping the RH primary fails over to the Mock alternate, whose
        // synthetic passes then take over — exactly the "primary RH drops → Mock alternate takes
        // over" scenario, proven in-process without Docker.
        let registry = test_registry();
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap()
            .id;
        // A long, slow Mock so it is still mid-emission (passes left to feed) when the primary drops
        // — a hot-standby alternate runs its emission in real time, so a failover only catches the
        // passes that have yet to be emitted.
        let mock = create_mock(&registry, "Backup Mock", 200, 30);
        registry
            .set_timers(&event_of(&registry), vec![rh.clone(), mock.clone()])
            .unwrap();
        registry
            .set_primary_timer(&event_of(&registry), Some(rh.clone()))
            .unwrap();
        // Bring the RH primary "up" — it is the active source, so the Mock alternate is gated off.
        registry.timers().set_status(&rh, TimerStatus::Connected);

        let (bridge, state) = spawn_bridge_for(&registry);
        start_heat(&state, vec![CompetitorRef("A".into())]);

        // While the RH primary is healthy, no passes land (no in-process RH feed; Mock is standby).
        sleep(POLL_INTERVAL * 2).await;
        assert_eq!(
            count_passes(&read_all_events(&state)),
            0,
            "the healthy RH primary is the active source; the Mock alternate must stay gated off"
        );

        // Drop the RH primary: the bridge re-evaluates each poll and fails over to the Mock
        // alternate, whose passes now take over.
        registry.timers().set_status(&rh, TimerStatus::Disconnected);
        timeout(
            Duration::from_secs(5),
            wait_until(&state, Duration::from_secs(5), |events| {
                count_passes(events) >= 2
            }),
        )
        .await
        .expect("the Mock alternate should take over once the RH primary drops");

        // Recovery (primary-preferred): bring the RH back and assert the Mock stops feeding.
        registry.timers().set_status(&rh, TimerStatus::Connected);
        sleep(POLL_INTERVAL * 2).await;
        let settled = count_passes(&read_all_events(&state));
        sleep(POLL_INTERVAL * 4).await;
        assert_eq!(
            count_passes(&read_all_events(&state)),
            settled,
            "on primary recovery the active source switches back; the Mock alternate stops feeding"
        );
        bridge.abort();
    }
}
