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
use gridfpv_server::timers::{TimerId, TimerKind, TimerRegistry};
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
    /// The **open-practice** heat this sink feeds, if any (open-practice format, Slice 1). When set,
    /// the sink routes passes into the event's in-memory per-channel accumulator (NOT the log) and
    /// wakes `/stream` to push the fresh per-channel live state — so an open-practice session's
    /// passes are *never* appended to the durable log (only its `HeatScheduled` + start/stop are).
    open_practice: Option<HeatId>,
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
            open_practice: None,
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
            open_practice: None,
        }
    }

    /// Mark this sink as feeding the **open-practice** `heat` (open-practice format, Slice 1):
    /// passes are routed into the event's in-memory per-channel accumulator and `/stream` is woken,
    /// rather than appended to the log. Builder style — applied to a gated/plain sink for an
    /// open-practice heat so its laps are tracked live but never logged.
    pub fn for_open_practice(mut self, heat: HeatId) -> Self {
        self.open_practice = Some(heat);
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
        };
        // Open practice (open-practice format, Slice 1): route the pass into the in-memory
        // per-channel accumulator and wake `/stream` — it is **never** appended to the log.
        if self.open_practice.is_some() {
            if self.state.open_practice().record(pass) {
                self.state.wake_streams();
            }
            return Ok(());
        }
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
        // Open practice (open-practice format, Slice 1): a lap-gate pass from a live RH source is
        // routed into the in-memory accumulator (not logged); any non-pass event still appends.
        if self.open_practice.is_some() {
            if let Event::Pass(pass) = event {
                if pass.gate.is_lap_gate() && self.state.open_practice().record(pass) {
                    self.state.wake_streams();
                }
                return Ok(());
            }
        }
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
/// on `Finished`/`Aborted`/`Finalized`/`Restarted` (or a newer heat going `Running`).
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
    // The runtime-clock drivers (heat-lifecycle Slice 2): the start countdown for a heat in `Armed`
    // and the completion clock for a heat in `Running`. Each is `(heat, task)` so the bridge can
    // cancel a superseded one (the heat left the relevant state) before it appends a stale auto.
    let mut clock = HeatClock::default();
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
        for (offset, event) in new_events {
            cursor = offset + 1;
            if let Event::CompetitorSeen {
                adapter,
                competitor,
            } = event
            {
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
            // Open practice (open-practice format, Slice 1): an open-practice heat's passes are
            // tracked **in memory, per channel — never logged**. Begin the accumulator over the
            // heat's channel lineup (this also clears any prior open-practice state, e.g. a
            // superseded heat) and mark each source's sink so its passes route there + wake
            // `/stream`. A non-open-practice heat leaves the accumulator untouched.
            let open_practice = is_open_practice_heat(state, registry, event_id, &heat);
            if open_practice {
                state.open_practice().begin(heat.clone(), lineup.clone());
                // Push the cleared/initial live state so a subscriber sees the fresh empty heat.
                state.wake_streams();
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
                let mut sink =
                    PassSink::gated(state.clone(), adapter.clone(), gate.clone(), timer_id);
                if open_practice {
                    sink = sink.for_open_practice(heat.clone());
                }
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
                    let mut sink = PassSink::gated(
                        state.clone(),
                        adapter.clone(),
                        gate.clone(),
                        timer_id.clone(),
                    );
                    if open_practice {
                        sink = sink.for_open_practice(heat.clone());
                    }
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
            clock.completion = Some((
                heat.clone(),
                spawn_completion_driver(state, registry, event_id, heat.clone()),
            ));

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
            // Open practice (open-practice format, Slice 1): **clear on stop**. Drop the in-memory
            // per-channel accumulator when the open-practice heat reaches a terminal / abort / restart
            // transition, then wake `/stream` so it re-folds the now-overlay-free log state (the
            // console settles back onto the bare log — no stale laps frame). The `Finished`
            // (Running → Unofficial) step is *kept* so the RD still sees the final practice laps
            // before finalizing; the true terminals below clear it. A new heat/round becoming active
            // also clears it (via `begin`).
            //
            // The overlay is **laps-only** now: the heat's phase/clock are always the real log's
            // (this same `HeatStateChanged` was already appended and woke the stream), so the served
            // phase follows the log to `Unofficial` here and to `Scheduled` on a `Restart` with no
            // shadow-tracking. We therefore only need to clear the laps on the terminals and wake.
            if state.open_practice().is_active(&heat)
                && !matches!(transition, HeatTransition::Finished)
                && state.open_practice().clear()
            {
                // Wake-on-clear: re-fold without the overlay so the laps drop immediately.
                state.wake_streams();
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
                clock.protest = Some((
                    heat.clone(),
                    spawn_auto_official_driver(state, registry, event_id, heat.clone()),
                ));
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
                let plan = tune_plan_of(state, &heat);
                let seats = seats_of(state, registry, &heat);
                for timer_id in selected_rh_timers(registry, timers, event_id) {
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
            clock.start = Some((
                heat.clone(),
                spawn_start_driver(state, registry, event_id, heat),
            ));
        }
    }
}

/// The runtime-clock drivers in flight for the bridge (heat-lifecycle Slice 2): the `start`
/// countdown for the heat currently in `Armed` and the `completion` clock for the heat currently in
/// `Running`. Each is `(heat, task)` so a transition can cancel exactly the driver belonging to the
/// heat that moved. At most one of each at a time (the bridge drives one heat at a time).
#[derive(Default)]
struct HeatClock {
    /// The start countdown for a heat in `Armed` (appends `HeatStarting` then auto `Running`).
    start: Option<(HeatId, JoinHandle<()>)>,
    /// The completion clock for a heat in `Running` (appends auto `Finished` on win + grace).
    completion: Option<(HeatId, JoinHandle<()>)>,
    /// The **auto-official timer** for a heat in `Unofficial` (marshaling Slice 5): when the round
    /// armed a protest window, it logs the deadline (`HeatFinalizing`) then appends the auto
    /// `Finalized` once the window elapses. Absent for a round with no protest window (the default).
    protest: Option<(HeatId, JoinHandle<()>)>,
}

impl HeatClock {
    /// Cancel any in-flight start/completion/protest driver belonging to `heat` (it just changed
    /// state, so a pending auto-transition for the *old* state must not land). Drivers for other
    /// heats are left running. Aborting a finished task is a harmless no-op.
    fn cancel_for(&mut self, heat: &HeatId) {
        if let Some((h, task)) = &self.start {
            if h == heat {
                task.abort();
                self.start = None;
            }
        }
        if let Some((h, task)) = &self.completion {
            if h == heat {
                task.abort();
                self.completion = None;
            }
        }
        if let Some((h, task)) = &self.protest {
            if h == heat {
                task.abort();
                self.protest = None;
            }
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
/// Slice 4a), mapped onto **node indices** for the RH `set_frequency` tune: node `n` runs
/// `lineup[n]`, so a competitor's assigned MHz is applied to the node at its lineup position. A heat
/// with no assigned frequencies (a sim/un-channelled heat) yields an empty plan (no tuning).
#[cfg(feature = "live")]
fn tune_plan_of(state: &AppState, heat: &HeatId) -> Vec<(u64, u16)> {
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
                // Map each (competitor, mhz) to the competitor's node seat (its lineup index).
                plan = frequencies
                    .into_iter()
                    .filter_map(|(competitor, mhz)| {
                        lineup
                            .iter()
                            .position(|c| *c == competitor)
                            .map(|node| (node as u64, mhz))
                    })
                    .collect();
            }
        }
    }
    plan
}

/// The heat's **node→pilot seating** for RotorHazard (the laps-attribute fix): one
/// `(node_index, callsign)` per **bound** node of `heat`, read from the heat's lineup (its durable
/// `HeatScheduled` bind — node `n` runs `lineup[n]`).
///
/// `lineup[n]` is the **pilot ref** for node `n` (the round engine builds the field as
/// `CompetitorRef(pilot_id)`), so each bound node resolves to its pilot's **callsign** via the
/// directory (CLAUDE.md: resolve a ref to its friendly name from a durable source, never print the
/// raw id). An open-practice / unchannelled heat seats per **channel** as `node-{i}` refs (no bound
/// pilot) — those are skipped here, leaving an empty plan (RH then races in practice mode). A pilot
/// ref that does not resolve falls back to the raw ref string as a last resort so the node is still
/// seated (RH records there) rather than dropped.
#[cfg(feature = "live")]
fn seats_of(state: &AppState, registry: &EventRegistry, heat: &HeatId) -> Vec<(u64, String)> {
    let Some(lineup) = lineup_of(state, heat) else {
        return Vec::new();
    };
    let pilots = registry.pilots();
    let mut seats = Vec::new();
    for (node, competitor) in lineup.into_iter().enumerate() {
        // An open-practice seat (`node-{i}`) names a channel, not a bound pilot: leave it unseated.
        if competitor
            .0
            .strip_prefix("node-")
            .is_some_and(|s| s.parse::<usize>().is_ok())
        {
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

/// Whether `heat` is an **open-practice** heat (open-practice format, Slice 1): its most-recent
/// `HeatScheduled.round` resolves to a round that [`is_open_practice`](gridfpv_server::round_engine::is_open_practice).
///
/// The bridge uses this on a `Running` transition to decide whether to route the heat's passes into
/// the in-memory per-channel accumulator (open practice) rather than the log. A heat with no round
/// tag, or a round that is not open-practice, returns `false` (the normal logged path).
fn is_open_practice_heat(
    state: &AppState,
    registry: &EventRegistry,
    event_id: &EventId,
    heat: &HeatId,
) -> bool {
    let Some(round_id) = round_of_heat(state, heat) else {
        return false;
    };
    registry
        .rounds_of(event_id)
        .and_then(|rounds| rounds.into_iter().find(|r| r.id == round_id))
        .is_some_and(|r| gridfpv_server::round_engine::is_open_practice(&r))
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
        // Auto-advance Armed → Running. If the heat already left Armed (a manual SkipCountdown / an
        // abort), this task has been cancelled by the bridge and never reaches here.
        if let Err(e) = state.append(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Running,
            },
            None,
        ) {
            eprintln!("gridfpv: start driver could not append Running: {e:?}");
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
/// **Open-practice time limit (open-practice refinement):** when the round carries a
/// [`time_limit_secs`](gridfpv_server::events::RoundDef::time_limit_secs), the driver auto-ends the
/// heat once its elapsed running time reaches the limit — **independent of the win condition** (an
/// open-practice heat does no scoring and its passes are never logged, so the win-condition path
/// never fires for it; the time limit is the only end condition). The elapsed clock starts when the
/// heat enters `Running` (this driver's spawn), so it is the same deterministic, logged transition
/// the other autos key off — a 1-hour practice ends itself an hour after Start. With no limit set,
/// only the win-condition path can fire (the RD ends an open practice manually).
fn spawn_completion_driver(
    state: &AppState,
    registry: &EventRegistry,
    event_id: &EventId,
    heat: HeatId,
) -> JoinHandle<()> {
    let config = heat_clock_config(state, registry, event_id, &heat);
    let state = state.clone();
    // The running clock origin: the moment the heat entered `Running` (this spawn). The time-limit
    // deadline, when set, is measured from here — a deterministic wall-clock span (a test drives it
    // with a short limit; production with the practice duration).
    let running_since = tokio::time::Instant::now();
    let time_limit = config
        .time_limit_secs
        .map(|secs| Duration::from_secs(secs as u64));
    let mut ticker = tokio::time::interval(COMPLETION_POLL);
    tokio::spawn(async move {
        loop {
            ticker.tick().await;
            // Time-limit auto-end (open-practice refinement): once the elapsed running time reaches
            // the practice duration, close the heat regardless of any win condition or passes — the
            // only end condition for an open-practice heat (whose passes are never logged, so the
            // win-condition branch below never fires for it). Logged like the other autos.
            if let Some(limit) = time_limit {
                if running_since.elapsed() >= limit {
                    if let Err(e) = state.append(
                        Event::HeatStateChanged {
                            heat,
                            transition: HeatTransition::Finished,
                        },
                        None,
                    ) {
                        eprintln!(
                            "gridfpv: completion driver could not append time-limit Finished: {e:?}"
                        );
                    }
                    return;
                }
            }
            let passes = heat_running_passes(&state, &heat);
            let Some(race_start) = race_start_of(&passes) else {
                continue; // no crossing yet — the race clock hasn't opened
            };
            if race_end_reached(&passes, config.win_condition, race_start) {
                // The race-end criterion is met: hold the grace window for late crossings, then
                // close the race. The hold is wall-clock; the *decision* was pure.
                tokio::time::sleep(grace_hold(config.grace_window)).await;
                if let Err(e) = state.append(
                    Event::HeatStateChanged {
                        heat,
                        transition: HeatTransition::Finished,
                    },
                    None,
                ) {
                    eprintln!("gridfpv: completion driver could not append Finished: {e:?}");
                }
                return;
            }
        }
    })
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
///   3. appends the auto `HeatStateChanged { Finalized }` (the `Unofficial → Final` step).
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
        // Auto-finalize Unofficial → Final. If the heat already left Unofficial (a manual early
        // Finalize, a Revert, an abort), this task has been cancelled by the bridge and never reaches
        // here — so the auto-finalize never fights a manual action.
        if let Err(e) = state.append(
            Event::HeatStateChanged {
                heat,
                transition: HeatTransition::Finalized,
            },
            None,
        ) {
            eprintln!("gridfpv: auto-official driver could not append Finalized: {e:?}");
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
            Event::Pass(p) if running && p.gate.is_lap_gate() => passes.push(p),
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
    use gridfpv_server::events::{EventRegistry, PRACTICE_EVENT_ID};
    use gridfpv_server::live_state::live_state;
    use gridfpv_server::timers::{
        CreateTimerRequest, MOCK_TIMER_ID, TimerId, TimerKind, TimerStatus, UpdateTimerRequest,
    };
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
                    ..Default::default()
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
                channel_capability: None,
                node_count: None,
                available_channels: None,
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
        let registry = EventRegistry::new(None).unwrap();
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
        registry.set_timers(&practice(), vec![timer.id]).unwrap();
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

    /// Spawn the presence reconciler for `registry`'s Practice event, returning its handle and
    /// Practice's `AppState` (the same log it polls), mirroring [`spawn_bridge_for`].
    fn spawn_reconciler_for(registry: &EventRegistry) -> (JoinHandle<()>, AppState) {
        let state = registry.resolve(&practice()).unwrap();
        let reg = registry.clone();
        let reconciler_state = state.clone();
        let handle = tokio::spawn(async move {
            run_presence_reconciler(reconciler_state, reg, practice()).await;
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

        let registry = EventRegistry::new(None).unwrap();
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
                    .meta_of(&practice())
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
            registry.meta_of(&practice()).unwrap().roster,
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
        let registry = EventRegistry::new(None).unwrap();
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
            registry.meta_of(&practice()).unwrap().roster.is_empty(),
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
        };
        registry
            .add_round(&ScopeEventId(PRACTICE_EVENT_ID.to_string()), req)
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
    async fn open_practice_laps_are_in_memory_not_logged_and_drive_live_state() {
        // An open-practice heat's passes go to the in-memory per-channel accumulator, NOT the log:
        // the log carries the heat's HeatScheduled + start/stop and ZERO Pass events, while the live
        // state shows per-channel laps with no pilot bound; the accumulator clears on stop.
        let laps = 3u32;
        let registry = fast_registry(laps, 1);
        let round = add_open_practice_round(&registry, vec![0, 1]);
        let (bridge, state) = spawn_bridge_for(&registry);

        let heat = start_open_practice_heat(&state, &round, &[0, 1]);
        let op = state.open_practice();

        // Wait until both channels have accumulated their laps in memory (holeshot + `laps`).
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ls) = op.live_state() {
                    if ls.progress.len() == 2
                        && ls.progress.iter().all(|p| p.laps_completed >= laps)
                    {
                        return;
                    }
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the open-practice accumulator should fill from the sim");

        // (a) The LOG has the heat's HeatScheduled + start/stop, but ZERO Pass events.
        let events = read_all_events(&state);
        assert_eq!(
            count_passes(&events),
            0,
            "an open-practice heat appends NO Pass events to the log"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::HeatScheduled { round: Some(r), .. } if *r == round)),
            "the heat's HeatScheduled (the session) is logged"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::HeatStateChanged {
                    transition: HeatTransition::Running,
                    ..
                }
            )),
            "the session start is logged"
        );

        // (b) The live state shows per-channel laps, each channel unbound (pilot: None).
        let ls = op.live_state().expect("an active open-practice live state");
        assert_eq!(ls.progress.len(), 2);
        assert!(ls.progress.iter().all(|p| p.pilot.is_none()));
        assert!(ls.progress.iter().all(|p| p.laps_completed >= laps));
        assert_eq!(ls.current_heat, Some(heat.clone()));

        // (c) Clear on stop: a terminal transition drops the accumulator.
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat.clone(),
                    transition: HeatTransition::Aborted,
                },
                None,
            )
            .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                if op.live_state().is_none() {
                    return;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the accumulator should clear on the terminal transition");

        // Still no Pass events ever reached the log.
        assert_eq!(count_passes(&read_all_events(&state)), 0);
        bridge.abort();
    }

    #[tokio::test]
    async fn open_practice_time_limit_auto_ends_the_running_heat() {
        // Open-practice refinement: an open-practice round with **no win condition** but a
        // `time_limit_secs` auto-ends its running heat (Running → Unofficial / a `Finished`
        // transition) once the elapsed running time reaches the limit — independent of any win
        // condition, and even though an open-practice heat logs NO passes (so the win-condition path
        // never fires). The completion driver's time-limit branch is the only end condition here.
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
        // No passes were ever logged for the open-practice heat (the time limit, not scoring, ended it).
        assert_eq!(count_passes(&events), 0);
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
            .set_timers(&practice(), vec![primary.clone(), alternate])
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
        let registry = EventRegistry::new(None).unwrap();
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
            .set_timers(&practice(), vec![rh.clone(), mock.clone()])
            .unwrap();
        registry
            .set_primary_timer(&practice(), Some(rh.clone()))
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
