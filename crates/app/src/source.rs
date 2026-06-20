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

use std::sync::Arc;
use std::time::Duration;

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, HeatId, HeatTransition, Pass, SourceTime,
};
use gridfpv_server::app::AppState;
use gridfpv_storage::Offset;
use tokio::task::JoinHandle;

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

    /// Materialise the boxed [`LapSource`] the bridge drives.
    fn into_source(self) -> Arc<dyn LapSource> {
        match self {
            SourceConfig::Sim(sim) => Arc::new(sim),
        }
    }
}

/// Spawn the control→source bridge as a background task, returning its [`JoinHandle`].
///
/// The task polls the log tail (see the module docs) until `state`'s log handle is dropped
/// at shutdown; the Director spawns it alongside `axum::serve` and lets it run for the
/// process lifetime. `adapter` is the [`AdapterId`] emitted passes carry.
pub fn spawn_bridge(state: AppState, source: SourceConfig, adapter: AdapterId) -> JoinHandle<()> {
    let source = source.into_source();
    tokio::spawn(async move {
        run_bridge(state, source, adapter).await;
    })
}

/// The bridge loop: poll the log tail, drive the source on `Running`, cancel on
/// `Finished`/`Aborted`/`Scored`/`Restarted` (or a newer heat going `Running`).
///
/// Exposed (crate-internal) so the test harness can run it directly against an in-memory
/// [`AppState`] and assert on the appended passes.
pub(crate) async fn run_bridge(state: AppState, source: Arc<dyn LapSource>, adapter: AdapterId) {
    let mut cursor: Offset = 0;
    // The in-flight heat task, if a heat is currently emitting. At most one at a time.
    let mut active: Option<ActiveHeat> = None;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);

    loop {
        ticker.tick().await;

        // Reap a finished/cancelled source task so a heat that ran to the end clears the
        // slot (without it, a re-Start of the same heat would be ignored).
        if let Some(running) = &active {
            if running.handle.is_finished() {
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
                handle_transition(&state, &source, &adapter, &mut active, heat, transition);
            }
        }
    }
}

/// React to a heat-loop transition: start emitting on `Running`, stop on a terminal /
/// off-ramp transition for the heat that is currently emitting.
fn handle_transition(
    state: &AppState,
    source: &Arc<dyn LapSource>,
    adapter: &AdapterId,
    active: &mut Option<ActiveHeat>,
    heat: HeatId,
    transition: HeatTransition,
) {
    match transition {
        HeatTransition::Running => {
            // A different heat taking the timer cancels the previous one (only one heat
            // emits at a time). Re-running the *same* heat also restarts cleanly.
            if let Some(running) = active.take() {
                running.handle.abort();
            }
            let Some(lineup) = lineup_of(state, &heat) else {
                // No HeatScheduled for this heat (or the log read failed): nothing to emit.
                return;
            };
            if lineup.is_empty() {
                return;
            }
            let sink = PassSink::new(state.clone(), adapter.clone());
            let run = HeatRun {
                heat: heat.clone(),
                lineup,
            };
            let source = Arc::clone(source);
            let handle = tokio::spawn(async move {
                if let Err(e) = source.run_heat(run, sink).await {
                    eprintln!("gridfpv: sim source stopped: {e}");
                }
            });
            *active = Some(ActiveHeat { heat, handle });
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
                        running.handle.abort();
                    }
                }
            }
        }
        // Staged/Armed are pre-Running: nothing to cancel (the heat isn't emitting yet).
        HeatTransition::Staged | HeatTransition::Armed => {}
    }
}

/// A heat currently emitting passes: which heat, and the task driving its source.
struct ActiveHeat {
    heat: HeatId,
    handle: JoinHandle<()>,
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

    use gridfpv_server::live_state::live_state;
    use gridfpv_storage::InMemoryLog;
    use tokio::time::{Instant, sleep, timeout};

    /// Build an `AppState` over a fresh in-memory log.
    fn fresh_state() -> AppState {
        AppState::new(InMemoryLog::new())
    }

    /// A fast sim source so the whole heat runs in a few ms (no seconds-long sleeps): a 1ms
    /// lap pace. The bridge polls at [`POLL_INTERVAL`], which dominates start-up latency, so
    /// we keep total laps small.
    fn fast_source(laps: u32) -> Arc<dyn LapSource> {
        Arc::new(SimSource::new(laps, Duration::from_millis(1)))
    }

    /// Drive the bridge in the background over `state`, returning its abort handle. Uses a
    /// fast source so emission completes quickly.
    fn spawn_test_bridge(state: AppState, laps: u32) -> JoinHandle<()> {
        let source = fast_source(laps);
        let adapter = AdapterId(SIM_ADAPTER.to_string());
        tokio::spawn(async move { run_bridge(state, source, adapter).await })
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
        let state = fresh_state();
        let laps = 3u32;
        let bridge = spawn_test_bridge(state.clone(), laps);

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
        let state = fresh_state();
        // Many laps at a slightly slower pace so the heat is still mid-emission when we
        // Finish it — proving the Finish actually cancels in-flight emission.
        let source: Arc<dyn LapSource> = Arc::new(SimSource::new(50, Duration::from_millis(3)));
        let adapter = AdapterId(SIM_ADAPTER.to_string());
        let bridge = {
            let state = state.clone();
            tokio::spawn(async move { run_bridge(state, source, adapter).await })
        };

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
        let state = fresh_state();
        let bridge = spawn_test_bridge(state.clone(), 40);

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
