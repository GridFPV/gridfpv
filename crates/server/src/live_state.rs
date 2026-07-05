//! The live race-state projection (protocol.html §1) — issue #41.
//!
//! This is the latency-sensitive core every overlay and spectator watches: the heat
//! currently on the timer and its loop [`HeatPhase`], the active pilots in that heat,
//! each pilot's live lap progress, the running order, and the on-deck (next scheduled)
//! heat. It is a **pure fold of the event log** ([`live_state`]): given the same events
//! it always produces the same [`LiveRaceState`], so a recorded session replays
//! identically and the snapshot is recomputable (protocol.html §2–§3).
//!
//! # What "current heat" means here
//!
//! The event log carries [`Event::HeatScheduled`] and
//! [`Event::HeatStateChanged`](gridfpv_events::Event::HeatStateChanged) for every heat
//! (race-engine.html §2). The **current heat** is the one the RD is showing/controlling in
//! Live control. A freshly-**scheduled** heat must not steal focus (filling a heat only
//! adds it to the list / on-deck), so the current heat is the one referenced by the **last
//! event among `{HeatStateChanged, CurrentHeatSelected}`** — a real heat-loop transition
//! (Stage/Start/…) or the RD's explicit manual selection
//! ([`Event::CurrentHeatSelected`](gridfpv_events::Event::CurrentHeatSelected)). Before
//! either has occurred we fall back to the **first `HeatScheduled`**, so the very first
//! heat is controllable. A heat that has reached the terminal `Final` phase is still
//! reported as the current heat until a *newer* heat takes the timer (a transition or a
//! selection), which mirrors what an overlay shows between heats ("last heat, now scored").
//!
//! # On-deck
//!
//! The **on-deck** heat is the next `Scheduled` heat that is not the current one and has
//! not yet run — the heat the RD will stage next. With no schedule metadata in the raw
//! log (seat/frequency assignment lands later, #36) this is simply the earliest-scheduled
//! heat still sitting in `Scheduled` that isn't already on the timer.
//!
//! # Live progress and running order
//!
//! Per-pilot live progress reuses the existing lap projection
//! ([`gridfpv_projection::lap_list_marshaled`]) filtered to the current heat's lineup, so
//! marshaling adjudications already fold in. The **running order** ranks the active pilots
//! by laps completed (descending) then by the completion time of their last lap (earliest
//! first) — the same "most laps, then who banked the last lap first" rule the scorer uses
//! mid-heat (race-engine.html §7.4), but derived without a win condition (which is heat /
//! format config not present in the raw log). It is therefore a *provisional* live order,
//! not the scored result; the authoritative scored ranking is the
//! [`HeatResult`](gridfpv_engine::scoring::HeatResult) projection.

use std::collections::BTreeMap;

use gridfpv_engine::heat::{HeatState, heat_state};
use gridfpv_events::{ClassId, CompetitorRef, Event, HeatId, HeatTransition, PilotId, RoundId};
use gridfpv_projection::{CompetitorKey, lap_list_marshaled_with_floor, registrations};
use gridfpv_storage::StoredEvent;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::snapshot::HeatPhase;

/// The current **live race-state** projection (protocol.html §1) — the latency-sensitive
/// core every overlay and spectator watches.
///
/// Fleshed out in #41 from the #40 placeholder: it carries the current heat and its
/// [`HeatPhase`], the active pilots in that heat, each pilot's live
/// [`PilotProgress`], the running order, and the on-deck heat. Fields are additive over
/// the placeholder (§7), so the snapshot body and change envelope did not reshape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LiveRaceState {
    /// The heat currently on the timer, if any (`None` before any heat is scheduled).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_heat: Option<HeatId>,
    /// The current heat's loop phase (protocol.html §1, race-engine.html §2). When
    /// `current_heat` is `None` this is [`HeatPhase::Scheduled`] (the idle default).
    pub phase: HeatPhase,
    /// The active pilots in the current heat — its lineup, in lineup (seeding) order.
    /// Empty when there is no current heat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_pilots: Vec<CompetitorRef>,
    /// Per-pilot live lap progress for the current heat, one entry per active pilot,
    /// ordered like [`active_pilots`](Self::active_pilots).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<PilotProgress>,
    /// The provisional running order of the current heat: the active pilots ranked by
    /// live standing (most laps, then who banked their last lap earliest). Best first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub running_order: Vec<CompetitorRef>,
    /// The next heat to run after the current one (the earliest still-`Scheduled` heat
    /// that is not on the timer), if one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub on_deck: Option<HeatId>,
    /// The **server-authoritative race-start instant** of the current heat: the
    /// `recorded_at` (microseconds, server wall clock) of the `Armed → Running`
    /// transition that put it on the timer (the real race-go). `None` before the heat
    /// has started (or for an older log whose transition carried no timestamp).
    ///
    /// This is the anchor every client clock counts from — header and HUD alike — so the
    /// elapsed reads identically and accurately *regardless of when the client mounted*
    /// (the #62 follow-up: kill the per-client wall-clock drift). Renders as a plain TS
    /// `number` (microseconds, bounded far below 2^53).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub race_started_at: Option<i64>,
    /// The **server-authoritative race-end instant** of the current heat: the
    /// `recorded_at` (microseconds, server wall clock) of the `Running → Unofficial`
    /// transition that closed it. `None` while the heat is still running (or has not
    /// started). When set, `race_ended_at - race_started_at` is the **exact** race
    /// duration a frozen clock reads (so a 60s limit reads `1:00.000`, not `1:00.100`).
    /// Renders as a plain TS `number` (microseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub race_ended_at: Option<i64>,
    /// The **server-authoritative staging-start instant** of the current heat while it is
    /// `Staged`: the `recorded_at` (microseconds, server wall clock) of its latest
    /// `Scheduled → Staged` transition. The staging countdown anchors here — a per-client
    /// wall-clock anchor made every console (and every reload) count its own window, so the
    /// RD's console could read overtime while a fresh one read the full window. `None` in
    /// every non-Staged phase. Renders as a plain TS `number` (microseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub staged_at: Option<i64>,
    /// The **server-authoritative start-tone instant** of the current heat while it is `Armed`:
    /// the absolute wall-clock instant (microseconds since the Unix epoch) the start tone fires
    /// and the heat auto-advances `Armed → Running`. Derived from the `recorded_at` of the heat's
    /// [`HeatStarting`](gridfpv_events::Event::HeatStarting) event plus its logged `delay_ms` (the
    /// chosen randomized hold). `None` once the heat is `Running`/past (or before it has armed).
    ///
    /// **RD-console-only:** the start delay is *intentionally random to pilots*, so this is the
    /// anchor the RD's control view counts down to ("tone in 0:03") — the read-only / pilot view
    /// never renders it. It is `None` once `race_started_at` is set, so a late join after race-go
    /// sees no stale countdown. Renders as a plain TS `number` (microseconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub tone_at: Option<i64>,
    /// The **provisional → official lifecycle** of the current heat (marshaling Slice 5,
    /// marshaling.html §3.3), surfaced for the Marshaling/Live UI. `None` until the heat reaches the
    /// `Unofficial` phase (before that there is no result to be provisional about). Once provisional
    /// it carries the auto-official deadline (when a protest window is armed) so the console can show
    /// a "Provisional — auto-official in M:SS" countdown; `Official` once finalized. Derived from the
    /// heat's phase + the logged [`HeatFinalizing`](gridfpv_events::Event::HeatFinalizing) deadline,
    /// so it folds deterministically like the rest of this projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lifecycle: Option<LifecycleState>,
}

/// The provisional → official lifecycle of a heat's result (marshaling Slice 5), projected for the
/// UI from the heat FSM + the logged auto-official deadline.
///
/// This is **not** a new FSM state: it reuses the heat's existing `Unofficial` (provisional,
/// correctable) → `Final` (official) transition. [`Provisional`](Self::Provisional) maps onto
/// `Unofficial`; [`Official`](Self::Official) onto `Final`. The optional `auto_official_at` is the
/// server-clock instant the runtime will auto-finalize, present only when the round armed a
/// [`ProtestWindow::After`](gridfpv_engine::heat::ProtestWindow::After).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum LifecycleState {
    /// The result is **provisional** (the heat is `Unofficial`): correctable, not yet official. When
    /// a protest window is armed, `auto_official_at` is the server wall-clock instant (microseconds
    /// since the Unix epoch) at which the runtime auto-finalizes; the console renders `at − now` as a
    /// countdown. `None` ⇒ no auto-official timer (manual finalize only — the default).
    Provisional {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        auto_official_at: Option<i64>,
    },
    /// The result is **official** (the heat is `Final`). `Revert` can re-open it to `Provisional`.
    Official,
}

impl Default for LiveRaceState {
    /// The idle live state: no current heat, `Scheduled` phase, nothing active.
    fn default() -> Self {
        Self {
            current_heat: None,
            phase: HeatPhase::Scheduled,
            active_pilots: Vec::new(),
            progress: Vec::new(),
            running_order: Vec::new(),
            on_deck: None,
            race_started_at: None,
            race_ended_at: None,
            staged_at: None,
            tone_at: None,
            lifecycle: None,
        }
    }
}

/// One active pilot's live progress in the current heat (protocol.html §1).
///
/// Derived from the heat's lap projection: the number of laps completed so far and the
/// duration of the most recent completed lap (the live "last lap" an overlay shows).
/// Splits are a later refinement; the lap-count + last-lap pair is the live core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PilotProgress {
    /// The source-local competitor this progress is for (a member of the lineup).
    pub competitor: CompetitorRef,
    /// The GridFPV pilot this competitor is bound to, if a registration
    /// ([`Event::CompetitorRegistered`]) has bound it (#60). `None` for an unregistered
    /// competitor, which still appears by its bare [`CompetitorRef`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pilot: Option<PilotId>,
    /// Completed laps so far in the heat.
    pub laps_completed: u32,
    /// Duration (µs, source clock) of the most recently completed lap, or `None` before
    /// the pilot has completed a lap. Renders as a plain TS `number` (bounded far below 2^53).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub last_lap_micros: Option<i64>,
}

/// Fold the event log into the [`LiveRaceState`] (protocol.html §1) — issue #41.
///
/// Pure and order-preserving: scans `events` once to find the current heat (the
/// most-recently-active one, see the module docs), folds its [`HeatState`] into a
/// [`HeatPhase`], reads its lineup, derives each active pilot's [`PilotProgress`] from the
/// (marshaling-aware) lap projection, ranks them into a running order, and finds the
/// on-deck heat. Replaying the same log twice yields the same state.
pub fn live_state(events: &[Event]) -> LiveRaceState {
    let window: Vec<(u64, &Event)> = events
        .iter()
        .enumerate()
        .map(|(i, e)| (i as u64, e))
        .collect();
    live_state_core(events, &window, None)
}

/// Fold a **windowed** log slice with its PRESERVED global offsets — the heat/class-scope
/// entry point (`heat_window_offsets` / `class_window_offsets` output). The marshaling
/// adjudications inside the window (`DetectionVoided`/`LapThrownOut`/…) target global
/// [`LogRef`]s; folding a filtered slice through the plain [`live_state`] re-enumerated it
/// `0,1,2,…`, so a correction to a heat deep in the log silently missed (or, on coincidence,
/// hit the wrong pass) in every heat-scoped live view. `window` must be in log order.
pub fn live_state_over(window: &[(u64, Event)]) -> LiveRaceState {
    live_state_over_with_floor(window, None)
}

/// [`live_state_over`] under the current heat's **min-lap floor** (D26): the live lap fold
/// suppresses under-floor raw passes exactly like the laps/result projections, so the race
/// screen's lap counts and the marshaling list can never disagree about an echo.
pub fn live_state_over_with_floor(
    window: &[(u64, Event)],
    min_lap_micros: Option<i64>,
) -> LiveRaceState {
    // The positional helpers (current heat, phase, lineup, run boundary) read a bare event
    // slice; the offsets matter only to the marshaling-aware lap fold below.
    let events: Vec<Event> = window.iter().map(|(_, e)| e.clone()).collect();
    let pairs: Vec<(u64, &Event)> = window.iter().map(|(o, e)| (*o, e)).collect();
    live_state_core(&events, &pairs, min_lap_micros)
}

/// The shared fold behind [`live_state`] (full log, positional offsets) and
/// [`live_state_over`] (a window with preserved global offsets). `window` is the SAME
/// sequence as `events`, paired with each event's global append offset.
fn live_state_core(
    events: &[Event],
    window: &[(u64, &Event)],
    min_lap_micros: Option<i64>,
) -> LiveRaceState {
    let Some(current_heat) = current_heat(events) else {
        return LiveRaceState::default();
    };

    let phase = heat_state(events, &current_heat)
        .map(phase_of)
        .unwrap_or(HeatPhase::Scheduled);

    let active_pilots = lineup_of(events, &current_heat);

    // The lap projection is keyed on (adapter, competitor); the lineup carries only the
    // competitor handle. Fold the marshaling-aware lap projection and index laps by competitor
    // ref, summing across adapters for a competitor seen on more than one.
    //
    // **Scope to the current run.** Abort/Restart send the heat back to `Scheduled` but leave the
    // aborted run's `Pass`es in the log; counting the whole log would keep showing (and a re-run
    // would double-count) those stale laps. So we window the fold to events at/after the heat's
    // latest reset boundary — the live count is *only* this run's passes (zero right after a reset,
    // counted from zero on the re-run). The global offset each event is fed under is preserved so a
    // marshaling adjudication still addresses the right pass. A heat that never reset has no
    // boundary, so everything counts (a normally-finalized heat is unaffected).
    let run_start = current_run_start(events, &current_heat);
    let pass_ceiling = current_run_pass_ceiling(events, &current_heat);
    let laps = lap_list_marshaled_with_floor(
        window
            .iter()
            .enumerate()
            .filter(|(i, (_, e))| {
                if *i < run_start {
                    return false;
                }
                // Tag-aware pass attribution: a pass stamped for ANOTHER heat never counts
                // toward this one — selecting an older heat as current used to absorb every
                // later heat's passes (they all sit after its run_start). An untagged
                // (legacy) pass keeps the positional rule. Either way a pass landing AFTER
                // the run went official is frozen out (the Final freeze).
                match e {
                    Event::Pass(p) => {
                        *i < pass_ceiling && p.heat.as_ref().is_none_or(|h| h == &current_heat)
                    }
                    _ => true,
                }
            })
            .map(|(_, (offset, e))| (*offset, *e)),
        min_lap_micros,
    );
    // Per ref: lap count, the last lap's DURATION (the wire's `last_lap_micros` display value),
    // and the last lap's COMPLETION time (`at`) — the running-order tie-break. The scorer ranks
    // equal-lap pilots by earlier last-lap completion; ordering on duration here made the live
    // overlay contradict the scored Timed result (a slower-but-ahead pilot showed behind).
    let mut by_ref: BTreeMap<&CompetitorRef, (u32, Option<i64>, Option<i64>)> = BTreeMap::new();
    for cl in &laps.competitors {
        let CompetitorKey { competitor, .. } = &cl.competitor;
        let entry = by_ref.entry(competitor).or_insert((0, None, None));
        entry.0 += cl.lap_count() as u32;
        if let Some(last) = cl.laps.last() {
            // Across adapters, keep the TEMPORALLY latest lap (not whichever adapter
            // iterates later).
            if entry.2.is_none_or(|prev| last.at.micros >= prev) {
                entry.1 = Some(last.duration_micros);
                entry.2 = Some(last.at.micros);
            }
        }
    }

    // Fold the registration bindings and index them by competitor ref. The lineup carries
    // only the bare `CompetitorRef`; registrations are keyed per-source `(adapter,
    // competitor)`, so collapse to a ref→pilot lookup. The fold already applied
    // last-registration-wins per key; iterating the map in (adapter, competitor) order
    // keeps the collapse deterministic when one ref is bound on more than one adapter.
    let bindings = registrations(events);
    let pilot_by_ref: BTreeMap<&CompetitorRef, &PilotId> = bindings
        .iter()
        .map(|(CompetitorKey { competitor, .. }, pilot)| (competitor, pilot))
        .collect();

    let progress: Vec<PilotProgress> = active_pilots
        .iter()
        .map(|competitor| {
            let (laps_completed, last_lap_micros, _) =
                by_ref.get(competitor).copied().unwrap_or((0, None, None));
            PilotProgress {
                competitor: competitor.clone(),
                pilot: pilot_by_ref.get(competitor).map(|p| (*p).clone()),
                laps_completed,
                last_lap_micros,
            }
        })
        .collect();

    // Rank by lap count, ties by EARLIER last-lap completion (the scorer's rule — see
    // `score_timed`), never by lap duration: physically ahead means crossed sooner.
    let last_completion: BTreeMap<&CompetitorRef, i64> = by_ref
        .iter()
        .filter_map(|(competitor, (_, _, at))| at.map(|at| (*competitor, at)))
        .collect();
    let running_order = running_order_by_completion(&progress, &last_completion);

    LiveRaceState {
        current_heat: Some(current_heat.clone()),
        phase,
        active_pilots,
        progress,
        running_order,
        on_deck: on_deck(events, &current_heat),
        // Timing is set by [`with_heat_timing`] from the *stored* log (which carries the
        // `recorded_at` server timestamps the bare `Event` slice lacks). The plain fold over
        // `&[Event]` — the open-practice synthetic path and the unit tests — leaves it `None`.
        race_started_at: None,
        race_ended_at: None,
        staged_at: None,
        // Likewise set by [`with_heat_timing`]: the start-tone instant needs the `HeatStarting`
        // event's `recorded_at`, which the bare-event fold cannot see.
        tone_at: None,
        // The lifecycle is likewise finished by [`with_heat_timing`] from the *stored* log (it needs
        // the `HeatFinalizing` deadline's `recorded_at` context); the bare-event fold leaves it `None`.
        lifecycle: None,
    }
}

/// Fold a heat's **server-authoritative race timing** out of the stored log: the
/// `recorded_at` of its `Armed → Running` transition (the race-start) and of its
/// `Running → Unofficial` transition (the race-end), if each has occurred.
///
/// Pure and recomputable like [`live_state`]: it reads the timestamps the log already
/// records (the runtime stamps each heat transition at append time), so a replayed session
/// folds the same instants. The last matching transition wins (a heat re-run after an
/// abort uses its latest `Running`).
pub fn heat_timing(stored: &[StoredEvent], heat: &HeatId) -> (Option<i64>, Option<i64>) {
    let mut started_at = None;
    let mut ended_at = None;
    for entry in stored {
        if let Event::HeatStateChanged {
            heat: h,
            transition,
        } = &entry.event
        {
            if h != heat {
                continue;
            }
            match transition {
                // `Armed → Running` is the real race-go; a re-run after an abort restamps it,
                // so clear any prior end too — the new run hasn't ended yet.
                HeatTransition::Running => {
                    started_at = entry.recorded_at;
                    ended_at = None;
                }
                // `Running → Unofficial` (the engine names it `Finished`) is the race-end.
                HeatTransition::Finished => ended_at = entry.recorded_at,
                _ => {}
            }
        }
    }
    (started_at, ended_at)
}

/// The current heat's **auto-official deadline** (marshaling Slice 5): the `at` of the most-recent
/// [`HeatFinalizing`](Event::HeatFinalizing) the runtime logged when it armed the protest window,
/// cleared by any later `Running` for the heat (a re-run re-opens the window from scratch).
///
/// Pure and log-derivable like [`heat_timing`]: the runtime writes the deadline once, at emission
/// time (mirroring `HeatStarting`'s logged delay), so a replay folds the same instant and the
/// countdown is identical for every client. `None` when no protest window was armed.
fn heat_finalizing_at(events: &[Event], heat: &HeatId) -> Option<i64> {
    let mut at = None;
    for event in events {
        match event {
            Event::HeatFinalizing { heat: h, at: a } if h == heat => at = Some(*a),
            // A fresh run re-opens the window: drop a stale deadline so an aborted/reverted run's
            // arming never lingers onto the new one (it re-arms with its own `HeatFinalizing`).
            Event::HeatStateChanged {
                heat: h,
                transition: HeatTransition::Running,
            } if h == heat => at = None,
            _ => {}
        }
    }
    at
}

/// The current heat's **start-tone instant** while it is `Armed` (heat-lifecycle Slice 2/3): the
/// absolute server wall-clock instant (microseconds) the start tone fires and the heat
/// auto-advances `Armed → Running`.
///
/// Derived from the heat's most-recent [`HeatStarting`](Event::HeatStarting): the `recorded_at` of
/// that event (the instant the heat armed) plus its logged `delay_ms` (the chosen randomized hold,
/// in **milliseconds** → microseconds). The runtime writes the delay once, at emission time
/// (mirroring `HeatFinalizing`'s deadline), so a replay folds the *same* instant and every
/// controlling client counts down identically.
///
/// Cleared by any later `Running` for the heat (the tone has fired — `race_started_at` takes over)
/// or a re-arm sequence: a fresh `HeatStarting` restamps it, a `Running` drops it. Pure and
/// log-derivable like [`heat_timing`]. `None` when the heat has not armed (no `HeatStarting`), the
/// `HeatStarting` carried no `recorded_at` (an older/untimed entry), or the heat has since run.
fn heat_tone_at(stored: &[StoredEvent], heat: &HeatId) -> Option<i64> {
    let mut at = None;
    for entry in stored {
        match &entry.event {
            // The start procedure armed: the tone fires at this instant + the chosen hold.
            Event::HeatStarting { heat: h, delay_ms } if h == heat => {
                at = entry
                    .recorded_at
                    .map(|armed| armed + i64::from(*delay_ms) * 1_000);
            }
            // The tone has fired (the heat is now Running) — `race_started_at` is the anchor from
            // here, so drop the pending tone instant. A re-arm logs a fresh `HeatStarting` above.
            Event::HeatStateChanged {
                heat: h,
                transition: HeatTransition::Running,
            } if h == heat => at = None,
            _ => {}
        }
    }
    at
}

/// Set a folded [`LiveRaceState`]'s server-authoritative timing **and lifecycle** from the stored
/// log.
///
/// The plain [`live_state`] fold (over `&[Event]`) cannot see `recorded_at`; the snapshot
/// and change-stream paths hold the full [`StoredEvent`] log, so they finish the live state
/// by folding the current heat's race-start / race-end instants in here. A no-op when there
/// is no current heat. Idempotent and order-independent — just a finishing fold.
///
/// The **lifecycle** (marshaling Slice 5) is derived alongside: an `Unofficial` heat is
/// [`Provisional`](LifecycleState::Provisional) (carrying the logged auto-official deadline, if a
/// protest window was armed), a `Final` heat is [`Official`](LifecycleState::Official), and any
/// earlier phase has no lifecycle yet (`None`).
pub fn with_heat_timing(mut live: LiveRaceState, stored: &[StoredEvent]) -> LiveRaceState {
    if let Some(heat) = live.current_heat.clone() {
        let (started, ended) = heat_timing(stored, &heat);
        live.race_started_at = started;
        live.race_ended_at = ended;
        // The staging countdown anchor — present only while the heat is `Staged` (phase-gated
        // like `tone_at`, so a re-run's stale Staged instant never leaks into another phase).
        live.staged_at = match live.phase {
            HeatPhase::Staged => heat_staged_at(stored, &heat),
            _ => None,
        };
        // The start-tone countdown anchor — present only while the heat is `Armed` (the tone has
        // not fired yet). `heat_tone_at` already clears it on `Running`; gating on the phase keeps
        // it absent in every non-Armed phase even if the fold order ever changed. RD-console-only:
        // the field surfaces it, the client gates rendering to a controlling session.
        live.tone_at = match live.phase {
            HeatPhase::Armed => heat_tone_at(stored, &heat),
            _ => None,
        };
        live.lifecycle = match live.phase {
            HeatPhase::Unofficial => {
                let events: Vec<Event> = stored.iter().map(|s| s.event.clone()).collect();
                Some(LifecycleState::Provisional {
                    auto_official_at: heat_finalizing_at(&events, &heat),
                })
            }
            HeatPhase::Final => Some(LifecycleState::Official),
            _ => None,
        };
    }
    live
}

/// The `recorded_at` of `heat`'s **latest `Staged` transition** — the staging-countdown anchor
/// (`LiveRaceState::staged_at`). Last-wins so a re-staged heat (after an abort) anchors its new
/// staging window, not the abandoned one. `None` when the heat never staged or the entry
/// carries no timestamp.
fn heat_staged_at(stored: &[StoredEvent], heat: &HeatId) -> Option<i64> {
    stored
        .iter()
        .filter_map(|entry| match &entry.event {
            Event::HeatStateChanged {
                heat: h,
                transition: HeatTransition::Staged,
            } if h == heat => entry.recorded_at,
            _ => None,
        })
        .next_back()
}

/// Map a folded [`HeatState`] to the projected [`HeatPhase`] the live view reports.
fn phase_of(state: HeatState) -> HeatPhase {
    match state {
        HeatState::Scheduled => HeatPhase::Scheduled,
        HeatState::Staged => HeatPhase::Staged,
        HeatState::Armed => HeatPhase::Armed,
        HeatState::Running => HeatPhase::Running,
        HeatState::Unofficial => HeatPhase::Unofficial,
        HeatState::Final => HeatPhase::Final,
    }
}

/// The current heat: the heat the RD is showing/controlling in Live control.
///
/// A brand-new **scheduled** heat must *not* grab focus (filling heats only adds them to
/// the list / on-deck), so the current heat is the one referenced by the **last event
/// among `{HeatStateChanged, CurrentHeatSelected}`** — a real heat-loop transition
/// (Stage/Start/…) or an explicit manual selection. If there has been neither yet, fall
/// back to the **first `HeatScheduled`** so the very first heat is controllable before any
/// transition. `None` if no heat was ever scheduled.
fn current_heat(events: &[Event]) -> Option<HeatId> {
    let mut active: Option<HeatId> = None;
    let mut first_scheduled: Option<HeatId> = None;
    for event in events {
        match event {
            Event::HeatStateChanged { heat, .. } | Event::CurrentHeatSelected { heat } => {
                active = Some(heat.clone());
            }
            Event::HeatScheduled { heat, .. } if first_scheduled.is_none() => {
                first_scheduled = Some(heat.clone());
            }
            _ => {}
        }
    }
    active.or(first_scheduled)
}

/// The log index where the current heat's **current run** begins — the boundary the live lap count /
/// standings / heat sheet fold from (`>= current_run_start`), so they reflect **only this heat's
/// current run**. It is the latest of, for `heat`:
///
/// - its most recent `Running` transition (the run started here — passes only arrive while Running),
///   **or**
/// - one past its most recent reset (`Aborted` / `Restarted` / `Discarded`).
///
/// This scopes the window two ways at once. **Across heats:** a heat's `Running` is logged after every
/// earlier heat finished, so folding from it excludes those earlier heats' passes — a pilot who flew
/// prior heats (a bracket level, a round-robin, a multi-round qualifier) shows only *this* heat's laps,
/// not a running total. **Within a heat:** a reset leaves the abandoned run's passes in the log; the
/// re-run's `Running` (or the reset boundary, before the re-run) sits after them, so the count restarts
/// from zero. If the heat has neither run nor reset yet (still `Scheduled`) the window is **empty**
/// (`events.len()`) ⇒ zero live laps — never a prior-heat total.
///
/// Pure and order-preserving: it is the *latest* such transition for `heat`.
///
/// Shared with the heat **window** (`app::heat_window_offsets`): scoring, the marshaling lap
/// list, and the audit fold all window from here too, so a Restarted heat's abandoned run
/// never leaks into its result (the same rule this fold applies to the live standings).
pub(crate) fn current_run_start(events: &[Event], heat: &HeatId) -> usize {
    // Default to past-the-end: a heat that has not run yet contributes no live laps (so selecting an
    // unraced heat as current shows zeros, not its pilots' totals from earlier heats).
    let mut start = events.len();
    for (i, event) in events.iter().enumerate() {
        if let Event::HeatStateChanged {
            heat: h,
            transition,
        } = event
        {
            if h == heat {
                match transition {
                    // The run begins at Running — fold this heat's passes (which follow it), excluding
                    // every earlier heat and any abandoned earlier run of this one.
                    HeatTransition::Running => start = i,
                    // A reset with no re-run yet: window past it so the abandoned passes drop out.
                    // `Discarded` is a reset too (Unofficial/Final → Scheduled, the result thrown
                    // away) — without it here a discarded heat kept showing, and re-scoring, the
                    // dead run's laps until its re-run (caught live in the overnight soak).
                    HeatTransition::Aborted
                    | HeatTransition::Restarted
                    | HeatTransition::Discarded => start = i + 1,
                    _ => {}
                }
            }
        }
    }
    start
}

/// The log index where the current run's **official record closes**: the first `Finalized`
/// transition for `heat` at/after [`current_run_start`], or `usize::MAX` while the run is not
/// (yet) official. Passes at/after this offset NEVER join the heat's window — once a result is
/// official it is frozen against late arrivals (a delayed RotorHazard catch-up pass tagged with
/// the heat used to silently change a Final result with no command, no ruling, and no audit
/// entry). A `Revert` re-opens marshaling (rulings are not passes and stay unaffected), but the
/// late passes stay out: the race's observation record ended with its run.
pub(crate) fn current_run_pass_ceiling(events: &[Event], heat: &HeatId) -> usize {
    let run_start = current_run_start(events, heat);
    events
        .iter()
        .enumerate()
        .skip(run_start)
        .find_map(|(i, e)| match e {
            Event::HeatStateChanged {
                heat: h,
                transition: HeatTransition::Finalized,
            } if h == heat => Some(i),
            _ => None,
        })
        .unwrap_or(usize::MAX)
}

/// The lineup of a heat: the competitors from its most recent `HeatScheduled`.
fn lineup_of(events: &[Event], heat: &HeatId) -> Vec<CompetitorRef> {
    let mut lineup = Vec::new();
    for event in events {
        if let Event::HeatScheduled {
            heat: h, lineup: l, ..
        } = event
        {
            if h == heat {
                lineup = l.clone();
            }
        }
    }
    lineup
}

/// The on-deck heat: the next still-`Scheduled` heat **after the current one** in schedule order.
///
/// "Still scheduled" means its folded [`HeatState`] is `Scheduled` (it has been created
/// but not staged). Heats are considered in the order they were first scheduled in the
/// log. The on-deck heat is the first scheduled heat positioned *after* the current heat —
/// this is what both the on-deck display and the Advance control want: the next heat forward,
/// never an earlier one.
///
/// **Fallback:** if there is no still-`Scheduled` heat after the current heat's position (the
/// current heat is last, or it isn't in the schedule at all), fall back to the first
/// still-`Scheduled` heat overall, so the RD can still advance to an earlier-scheduled-but-unrun
/// heat rather than getting stuck. The "after current" candidate is always preferred.
pub(crate) fn on_deck(events: &[Event], current: &HeatId) -> Option<HeatId> {
    let mut seen: Vec<HeatId> = Vec::new();
    for event in events {
        if let Event::HeatScheduled { heat, .. } = event {
            if !seen.contains(heat) {
                seen.push(heat.clone());
            }
        }
    }
    let is_scheduled = |heat: &HeatId| heat_state(events, heat) == Some(HeatState::Scheduled);
    // Position of the current heat in schedule order (if it appears at all).
    let current_idx = seen.iter().position(|heat| heat == current);
    // Prefer the first still-Scheduled heat positioned after the current heat.
    if let Some(idx) = current_idx {
        if let Some(next) = seen[idx + 1..].iter().find(|heat| is_scheduled(heat)) {
            return Some(next.clone());
        }
    }
    // Fallback: the first still-Scheduled heat that is not the current one.
    seen.iter()
        .find(|heat| *heat != current && is_scheduled(heat))
        .cloned()
}

/// The round a heat was scheduled under, from its most recent `HeatScheduled` (`None` for a
/// free-text / untagged heat). Used by the Advance control path to ask that round's generator
/// for the next heat when nothing is already on deck.
pub(crate) fn round_of_heat(events: &[Event], heat: &HeatId) -> Option<RoundId> {
    latest_schedule(events, heat).2
}

/// Rank active pilots into the provisional running order with the SCORER's tie-break:
/// most laps first, ties broken by the **earlier last-lap completion time** (physically
/// ahead = crossed sooner — matching `score_timed`, so the live overlay agrees with the
/// eventual scored order), then by competitor ref for a total, deterministic order.
/// `last_completion` carries each ref's latest lap-closing pass instant (source µs);
/// a ref with laps but no completion entry sorts after those with one.
fn running_order_by_completion(
    progress: &[PilotProgress],
    last_completion: &BTreeMap<&CompetitorRef, i64>,
) -> Vec<CompetitorRef> {
    let mut order: Vec<&PilotProgress> = progress.iter().collect();
    order.sort_by(|a, b| {
        b.laps_completed
            .cmp(&a.laps_completed)
            .then_with(|| {
                match (
                    last_completion.get(&a.competitor),
                    last_completion.get(&b.competitor),
                ) {
                    (Some(x), Some(y)) => x.cmp(y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            })
            .then_with(|| a.competitor.cmp(&b.competitor))
    });
    order.into_iter().map(|p| p.competitor.clone()).collect()
}

/// Rank by lap count with the last-lap **duration** tie-break — the legacy proxy, kept for
/// the open-practice per-channel board (whose in-memory laps carry durations, not global
/// completion instants). The real heat overlay uses [`running_order_by_completion`].
pub(crate) fn running_order(progress: &[PilotProgress]) -> Vec<CompetitorRef> {
    let mut order: Vec<&PilotProgress> = progress.iter().collect();
    order.sort_by(|a, b| {
        b.laps_completed
            .cmp(&a.laps_completed)
            .then_with(|| match (a.last_lap_micros, b.last_lap_micros) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.competitor.cmp(&b.competitor))
    });
    order.into_iter().map(|p| p.competitor.clone()).collect()
}

/// A scheduled heat as the **Heats UI** lists it (race redesign Slice 3b) — the per-heat view
/// model the Rounds & Heats stage renders under each round.
///
/// Unlike [`LiveRaceState`] (which is *only* about the current heat), this is one entry per heat
/// the log ever scheduled, carrying the round/class tag the scheduler assigned so the UI can group
/// heats by round and resolve their lineup to pilots. Folded from the log by
/// [`heat_summaries`] — pure and recomputable like every other projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct HeatSummary {
    /// The heat's id (its scheduled handle, the same one the live/control path drives).
    pub heat: HeatId,
    /// The heat's lineup — the competitors from its most recent `HeatScheduled`, in lineup order.
    pub lineup: Vec<CompetitorRef>,
    /// The class this heat was tagged with, when the scheduler assigned one (`None` for the
    /// free-text / untagged path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub class: Option<ClassId>,
    /// The round this heat was tagged with, when the scheduler assigned one. The Heats UI groups
    /// the list by this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub round: Option<RoundId>,
    /// The per-pilot **frequency assignment** of this heat, in raw MHz, paired with the competitor
    /// it is assigned to — taken from the most recent `HeatScheduled` (race redesign Slice 4b). The
    /// Heats/Live UI resolves each raw MHz back to a band+channel label via the channel catalog.
    /// Empty when none was assigned (a sim heat, or the free-text path), in which case the UI shows
    /// "—". Additive — defaults empty so older logs round-trip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frequencies: Vec<(CompetitorRef, u16)>,
    /// The **optional human label** the RD typed when building this heat by hand, from the most
    /// recent `HeatScheduled`. When present the Heats/Live UI shows it as the heat's display name
    /// (overriding the derived "‹Round› Heat N" / tier convention); `None` for a generator-filled
    /// heat, which keeps the auto-name. Additive — defaults absent so older logs round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub label: Option<String>,
    /// The heat's folded loop phase (its derived status: scheduled / running / final / …).
    pub phase: HeatPhase,
    /// Whether this heat is the one currently on the timer (the live `current_heat`).
    pub is_current: bool,
}

/// Fold the event log into the list of **scheduled heats** (race redesign Slice 3b), in
/// first-scheduled order — one [`HeatSummary`] per heat the log ever scheduled.
///
/// Pure and order-preserving, mirroring [`live_state`]: a heat appears once (keyed on its id, the
/// latest `HeatScheduled` wins for lineup/class/round so a re-schedule updates the entry in place),
/// ordered by first appearance in the log (the order the generator emitted them). Each heat's
/// `phase` is its folded [`HeatState`] and `is_current` marks the one on the timer. The Heats UI
/// filters this by `round` to render each round's heats.
pub fn heat_summaries(events: &[Event]) -> Vec<HeatSummary> {
    let current = current_heat(events);

    // First-scheduled order, deduped — collect the heat ids in the order they first appear.
    let mut order: Vec<HeatId> = Vec::new();
    for event in events {
        if let Event::HeatScheduled { heat, .. } = event {
            if !order.contains(heat) {
                order.push(heat.clone());
            }
        }
    }

    order
        .into_iter()
        .map(|heat| {
            let (lineup, class, round, frequencies, label) = latest_schedule(events, &heat);
            let phase = heat_state(events, &heat)
                .map(phase_of)
                .unwrap_or(HeatPhase::Scheduled);
            let is_current = current.as_ref() == Some(&heat);
            HeatSummary {
                heat,
                lineup,
                class,
                round,
                frequencies,
                label,
                phase,
                is_current,
            }
        })
        .collect()
}

/// The lineup + class/round tag a heat carries, taken from its **most recent** `HeatScheduled`
/// (a re-schedule of the same id supersedes the earlier one).
#[allow(clippy::type_complexity)]
fn latest_schedule(
    events: &[Event],
    heat: &HeatId,
) -> (
    Vec<CompetitorRef>,
    Option<ClassId>,
    Option<RoundId>,
    Vec<(CompetitorRef, u16)>,
    Option<String>,
) {
    let mut out = (Vec::new(), None, None, Vec::new(), None);
    for event in events {
        if let Event::HeatScheduled {
            heat: h,
            lineup,
            class,
            round,
            frequencies,
            label,
        } = event
        {
            if h == heat {
                out = (
                    lineup.clone(),
                    class.clone(),
                    round.clone(),
                    frequencies.clone(),
                    label.clone(),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, Pass, SourceTime};

    fn heat() -> HeatId {
        HeatId("q-1".into())
    }

    fn scheduled(id: &str, lineup: &[&str]) -> Event {
        Event::HeatScheduled {
            heat: HeatId(id.into()),
            lineup: lineup.iter().map(|c| CompetitorRef((*c).into())).collect(),
            class: None,
            round: None,
            frequencies: vec![],
            label: None,
        }
    }

    fn changed(id: &str, transition: HeatTransition) -> Event {
        Event::HeatStateChanged {
            heat: HeatId(id.into()),
            transition,
        }
    }

    fn pass(competitor: &str, at: i64, seq: u64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
            heat: None,
        })
    }

    /// A pass STAMPED for `heat` (the bridge's tag-attribution path).
    fn tagged_pass(heat: &str, competitor: &str, at: i64, seq: u64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
            heat: Some(HeatId(heat.into())),
        })
    }

    fn registered(competitor: &str, pilot: &str) -> Event {
        Event::CompetitorRegistered {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef(competitor.into()),
            pilot: PilotId(pilot.into()),
        }
    }

    #[test]
    fn empty_log_is_idle_default() {
        assert_eq!(live_state(&[]), LiveRaceState::default());
        let s = live_state(&[]);
        assert_eq!(s.current_heat, None);
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert!(s.active_pilots.is_empty());
    }

    #[test]
    fn scheduled_heat_reports_lineup_and_scheduled_phase() {
        let events = vec![scheduled("q-1", &["A", "B", "C"])];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(heat()));
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert_eq!(
            s.active_pilots,
            vec![
                CompetitorRef("A".into()),
                CompetitorRef("B".into()),
                CompetitorRef("C".into())
            ]
        );
        // No laps yet: progress entries exist but are zeroed.
        assert_eq!(s.progress.len(), 3);
        assert!(s.progress.iter().all(|p| p.laps_completed == 0));
    }

    #[test]
    fn phase_tracks_the_heat_loop_through_final() {
        // Scheduled → Staged → Armed → Running → Unofficial → Final.
        let steps = [
            (HeatTransition::Staged, HeatPhase::Staged),
            (HeatTransition::Armed, HeatPhase::Armed),
            (HeatTransition::Running, HeatPhase::Running),
            (HeatTransition::Finished, HeatPhase::Unofficial),
            (HeatTransition::Finalized, HeatPhase::Final),
        ];
        let mut events = vec![scheduled("q-1", &["A", "B"])];
        assert_eq!(live_state(&events).phase, HeatPhase::Scheduled);
        for (transition, expected) in steps {
            events.push(changed("q-1", transition));
            assert_eq!(live_state(&events).phase, expected, "after {transition:?}");
        }
    }

    #[test]
    fn live_progress_counts_laps_and_last_lap_per_pilot() {
        // A: 3 passes ⇒ 2 laps (last lap 2.5s). B: 2 passes ⇒ 1 lap (3.0s).
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("B", 1_500_000, 1),
            pass("A", 4_000_000, 2),
            pass("B", 4_500_000, 2),
            pass("A", 6_500_000, 3),
        ];
        let s = live_state(&events);
        assert_eq!(s.phase, HeatPhase::Running);

        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        assert_eq!(a.laps_completed, 2);
        assert_eq!(a.last_lap_micros, Some(2_500_000));

        let b = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("B".into()))
            .unwrap();
        assert_eq!(b.laps_completed, 1);
        assert_eq!(b.last_lap_micros, Some(3_000_000));

        // Running order: A (2 laps) leads B (1 lap).
        assert_eq!(
            s.running_order,
            vec![CompetitorRef("A".into()), CompetitorRef("B".into())]
        );
    }

    #[test]
    fn running_order_breaks_lap_ties_by_last_lap_time() {
        // Both completed 1 lap; B's lap (2.0s) is faster than A's (3.0s) ⇒ B leads.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("B", 1_000_000, 1),
            pass("A", 4_000_000, 2), // A lap = 3.0s
            pass("B", 3_000_000, 2), // B lap = 2.0s
        ];
        let s = live_state(&events);
        assert_eq!(
            s.running_order,
            vec![CompetitorRef("B".into()), CompetitorRef("A".into())]
        );
    }

    #[test]
    fn current_heat_defaults_to_the_first_scheduled_heat() {
        // Before any transition or selection, the first scheduled heat is the controllable one
        // even when a later heat was scheduled afterwards (filling adds, it does not steal focus).
        let events = vec![scheduled("q-1", &["A", "B"]), scheduled("q-2", &["C", "D"])];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-1".into())));
        assert_eq!(
            s.active_pilots,
            vec![CompetitorRef("A".into()), CompetitorRef("B".into())]
        );
    }

    #[test]
    fn freshly_scheduled_heat_does_not_steal_focus() {
        // q-1 is staged (it's the active heat). Filling q-2 (a bare HeatScheduled) must NOT move
        // the current heat — focus only moves on a real transition or an explicit selection.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            scheduled("q-2", &["C", "D"]),
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-1".into())));
        assert_eq!(s.phase, HeatPhase::Staged);
        // q-2 is on deck, not current.
        assert_eq!(s.on_deck, Some(HeatId("q-2".into())));
    }

    #[test]
    fn current_heat_follows_a_heat_state_change() {
        // q-1 runs and scores; q-2 is scheduled (no focus steal) then transitioned (Staged),
        // which moves the current heat to q-2.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            changed("q-1", HeatTransition::Finished),
            changed("q-1", HeatTransition::Finalized),
            scheduled("q-2", &["C", "D"]),
        ];
        // After the bare schedule, focus is still on q-1 (last transition).
        assert_eq!(live_state(&events).current_heat, Some(HeatId("q-1".into())));
        // A real transition on q-2 moves focus to it.
        let mut events = events;
        events.push(changed("q-2", HeatTransition::Staged));
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-2".into())));
        assert_eq!(s.phase, HeatPhase::Staged);
        assert_eq!(
            s.active_pilots,
            vec![CompetitorRef("C".into()), CompetitorRef("D".into())]
        );
    }

    #[test]
    fn current_heat_follows_an_explicit_selection() {
        // q-1 is active (staged); the RD manually selects q-2 in Live control. The current heat
        // follows the explicit CurrentHeatSelected even though q-2 has not transitioned.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            scheduled("q-2", &["C", "D"]),
            Event::CurrentHeatSelected {
                heat: HeatId("q-2".into()),
            },
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-2".into())));
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert_eq!(
            s.active_pilots,
            vec![CompetitorRef("C".into()), CompetitorRef("D".into())]
        );
        // A later selection back to q-1 follows again (last selection wins).
        let mut events = events;
        events.push(Event::CurrentHeatSelected {
            heat: HeatId("q-1".into()),
        });
        assert_eq!(live_state(&events).current_heat, Some(HeatId("q-1".into())));
    }

    #[test]
    fn on_deck_is_the_next_still_scheduled_heat() {
        // q-1 is running (current); q-2 and q-3 are scheduled and waiting.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            scheduled("q-2", &["C", "D"]),
            scheduled("q-3", &["E", "F"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-1".into())));
        assert_eq!(s.phase, HeatPhase::Running);
        // q-2 is the next still-scheduled heat behind the current one.
        assert_eq!(s.on_deck, Some(HeatId("q-2".into())));
    }

    #[test]
    fn on_deck_is_the_heat_after_current_not_an_earlier_scheduled_one() {
        // The exact bug: the current heat (q-2) is mid-order, and an EARLIER heat (q-1) is still
        // Scheduled (never staged/run). On-deck — and therefore Advance — must pick the heat
        // *after* q-2 (q-3), NOT roll backward to the earlier still-scheduled q-1.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            scheduled("q-2", &["C", "D"]),
            scheduled("q-3", &["E", "F"]),
            // q-2 is the current heat (it's running); q-1 was skipped and stays Scheduled.
            Event::CurrentHeatSelected {
                heat: HeatId("q-2".into()),
            },
            changed("q-2", HeatTransition::Staged),
            changed("q-2", HeatTransition::Armed),
            changed("q-2", HeatTransition::Running),
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-2".into())));
        // Forward, not backward: q-3 (after current), never q-1 (earlier-scheduled).
        assert_eq!(s.on_deck, Some(HeatId("q-3".into())));
    }

    #[test]
    fn on_deck_falls_back_to_an_earlier_scheduled_heat_when_current_is_last() {
        // The current heat (q-3) is last in schedule order, so there is no scheduled heat after
        // it. Rather than getting stuck, on-deck falls back to the first still-Scheduled heat
        // overall (q-1, which was skipped) so the RD can still advance to it.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            scheduled("q-2", &["C", "D"]),
            scheduled("q-3", &["E", "F"]),
            // q-2 already ran and finalized; q-1 was skipped (stays Scheduled).
            changed("q-2", HeatTransition::Staged),
            changed("q-2", HeatTransition::Armed),
            changed("q-2", HeatTransition::Running),
            changed("q-2", HeatTransition::Finished),
            changed("q-2", HeatTransition::Finalized),
            // q-3 is now the current heat and running (it's last in order).
            changed("q-3", HeatTransition::Staged),
            changed("q-3", HeatTransition::Armed),
            changed("q-3", HeatTransition::Running),
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-3".into())));
        // No scheduled heat after q-3 → fall back to the first still-Scheduled heat overall.
        assert_eq!(s.on_deck, Some(HeatId("q-1".into())));
    }

    #[test]
    fn registered_competitor_surfaces_its_pilot_unregistered_stays_bare() {
        // A is bound to a pilot; B is never registered. A's progress carries the pilot;
        // B's pilot stays `None` (it appears by its bare CompetitorRef).
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            registered("A", "acroace"),
            changed("q-1", HeatTransition::Running),
        ];
        let s = live_state(&events);
        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        assert_eq!(a.pilot, Some(PilotId("acroace".into())));
        let b = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("B".into()))
            .unwrap();
        assert_eq!(b.pilot, None);
    }

    #[test]
    fn last_registration_wins_when_a_competitor_is_rebound() {
        // A is bound to acroace, then re-bound to zoomer: the live state shows zoomer.
        let events = vec![
            scheduled("q-1", &["A"]),
            registered("A", "acroace"),
            registered("A", "zoomer"),
        ];
        let s = live_state(&events);
        let a = &s.progress[0];
        assert_eq!(a.pilot, Some(PilotId("zoomer".into())));
    }

    #[test]
    fn fold_is_deterministic() {
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2),
        ];
        assert_eq!(live_state(&events), live_state(&events));
    }

    #[test]
    fn abort_resets_live_lap_count_to_zero() {
        // A heat runs and banks laps, then is aborted (→ Scheduled, no new run yet). The aborted
        // run's passes stay in the log, but the live count must be 0 for every pilot — `progress`
        // (Leaderboard / heat sheet) and `running_order` reset together.
        let mut events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2), // A: 1 lap before the abort
            pass("B", 1_500_000, 1),
        ];
        // Before the abort, A has a lap.
        let pre = live_state(&events);
        assert_eq!(
            pre.progress
                .iter()
                .find(|p| p.competitor == CompetitorRef("A".into()))
                .unwrap()
                .laps_completed,
            1
        );

        // Abort: heat back to Scheduled, the prior run's passes linger in the log.
        events.push(changed("q-1", HeatTransition::Aborted));
        let s = live_state(&events);
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert!(
            s.progress.iter().all(|p| p.laps_completed == 0),
            "every pilot's live lap count resets to 0 after Abort"
        );
        assert!(
            s.progress.iter().all(|p| p.last_lap_micros.is_none()),
            "the stale last-lap also clears after Abort"
        );
    }

    #[test]
    fn discard_resets_live_lap_count_to_zero() {
        // Discard is a reset like Abort/Restart (Unofficial/Final → Scheduled, result thrown
        // away): the discarded run's passes stay in the log, but the live count must be 0 — the
        // overnight soak caught a discarded heat still showing the dead run's laps.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2), // A: 1 lap before the discard
            pass("B", 1_500_000, 1),
            changed("q-1", HeatTransition::Finished),
            changed("q-1", HeatTransition::Discarded),
        ];
        let s = live_state(&events);
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert!(
            s.progress.iter().all(|p| p.laps_completed == 0),
            "every pilot's live lap count resets to 0 after Discard"
        );
        assert!(
            s.progress.iter().all(|p| p.last_lap_micros.is_none()),
            "the stale last-lap also clears after Discard"
        );
    }

    #[test]
    fn tagged_passes_survive_a_mid_race_heat_schedule() {
        // The scheduling-eats-laps bug: a HeatScheduled for ANOTHER heat lands mid-race
        // (an RD filling the next round), which closes the running heat's positional span.
        // Tagged passes attribute by their stamp, so the laps after it still count.
        let events = vec![
            scheduled("q-1", &["A"]),
            changed("q-1", HeatTransition::Running),
            tagged_pass("q-1", "A", 1_000_000, 1),
            tagged_pass("q-1", "A", 3_000_000, 2), // lap 1 banked
            scheduled("q-2", &["B"]),              // mid-race schedule (span closer)
            tagged_pass("q-1", "A", 5_000_000, 3), // lap 2 — used to vanish
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-1".into())));
        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        assert_eq!(
            a.laps_completed, 2,
            "the post-schedule lap must still count for the running heat"
        );
    }

    #[test]
    fn tagged_passes_do_not_leak_into_an_older_selected_heat() {
        // Heats race back to back; the RD selects the FINISHED first heat as current.
        // Its live view must show ITS laps only — the later heat's tagged passes all sit
        // after q-1's run_start, so the positional rule alone absorbed them.
        let events = vec![
            scheduled("q-1", &["A"]),
            changed("q-1", HeatTransition::Running),
            tagged_pass("q-1", "A", 1_000_000, 1),
            tagged_pass("q-1", "A", 3_000_000, 2), // q-1: 1 lap
            changed("q-1", HeatTransition::Finished),
            scheduled("q-2", &["A"]),
            changed("q-2", HeatTransition::Running),
            tagged_pass("q-2", "A", 11_000_000, 3),
            tagged_pass("q-2", "A", 13_000_000, 4),
            tagged_pass("q-2", "A", 15_000_000, 5), // q-2: 2 laps
            changed("q-2", HeatTransition::Finished),
            Event::CurrentHeatSelected {
                heat: HeatId("q-1".into()),
            },
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-1".into())));
        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        assert_eq!(a.laps_completed, 1, "only q-1's own lap counts");
    }

    #[test]
    fn running_order_ties_break_on_last_lap_completion_not_duration() {
        // A completes lap 2 at t=20s (a slow 10s lap); B completes lap 2 at t=33s (a fast
        // 8s lap). A is physically ahead — the scorer ranks by earlier completion, and the
        // live order must agree (it used to rank B first on the shorter duration).
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Running),
            pass("A", 0, 1),
            pass("B", 5_000_000, 1),
            pass("A", 10_000_000, 2), // A lap 1 @10s
            pass("B", 25_000_000, 2), // B lap 1 @25s (20s lap)
            pass("A", 20_000_000, 3), // A lap 2 @20s (10s lap)
            pass("B", 33_000_000, 3), // B lap 2 @33s (8s lap)
        ];
        let s = live_state(&events);
        assert_eq!(
            s.running_order,
            vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
            "equal laps: the EARLIER last completion leads"
        );
    }

    #[test]
    fn rerun_after_abort_counts_from_zero_not_old_plus_new() {
        // After an abort, a fresh re-run (re-Stage → Start → new passes) counts ONLY the new run's
        // laps — the aborted run's passes are excluded, so it is N-from-zero, never old+new.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            // The aborted run: A banks 2 laps (3 passes), B banks 1 lap.
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2),
            pass("A", 7_000_000, 3),
            pass("B", 1_500_000, 1),
            pass("B", 4_500_000, 2),
            changed("q-1", HeatTransition::Aborted),
            // The fresh re-run: A banks 1 lap (2 passes), B none.
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            pass("A", 20_000_000, 4),
            pass("A", 22_500_000, 5), // new lap = 2.5s
        ];
        let s = live_state(&events);
        assert_eq!(s.phase, HeatPhase::Running);
        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        // 1 (this run), NOT 2 (aborted run) + 1 = 3.
        assert_eq!(a.laps_completed, 1);
        assert_eq!(a.last_lap_micros, Some(2_500_000));
        let b = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("B".into()))
            .unwrap();
        assert_eq!(b.laps_completed, 0, "B's aborted-run lap is excluded");
        // Running order ranks by the current run: A (1 lap) leads B (0).
        assert_eq!(
            s.running_order,
            vec![CompetitorRef("A".into()), CompetitorRef("B".into())]
        );
    }

    #[test]
    fn restart_resets_live_lap_count_to_zero_and_reruns_from_zero() {
        // Same as Abort but via Restart (a committed heat restarted → Scheduled). After Restart the
        // count is 0; the re-run counts from zero.
        let mut events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2), // A: 1 lap
            changed("q-1", HeatTransition::Restarted),
        ];
        let s = live_state(&events);
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert!(
            s.progress.iter().all(|p| p.laps_completed == 0),
            "every pilot's live lap count resets to 0 after Restart"
        );

        // Re-run: A banks 2 laps from zero.
        events.extend([
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            pass("A", 30_000_000, 3),
            pass("A", 33_000_000, 4),
            pass("A", 36_000_000, 5),
        ]);
        let s = live_state(&events);
        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        assert_eq!(a.laps_completed, 2, "counted from zero, not 1 + 2");
    }

    #[test]
    fn normal_finalize_without_a_reset_counts_the_whole_heat() {
        // A heat that finalizes normally was never reset → the run begins at its Running, so the
        // whole run's laps count (finalized results are unaffected by the run-scoping).
        let events = vec![
            scheduled("q-1", &["A", "B"]),           // 0
            changed("q-1", HeatTransition::Staged),  // 1
            changed("q-1", HeatTransition::Armed),   // 2
            changed("q-1", HeatTransition::Running), // 3 — the run starts here
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2),
            pass("A", 7_000_000, 3), // A: 2 laps
            changed("q-1", HeatTransition::Finished),
            changed("q-1", HeatTransition::Finalized),
        ];
        assert_eq!(current_run_start(&events, &heat()), 3);
        let s = live_state(&events);
        assert_eq!(s.phase, HeatPhase::Final);
        let a = s
            .progress
            .iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .unwrap();
        assert_eq!(
            a.laps_completed, 2,
            "a normally-finalized heat counts every lap"
        );
    }

    #[test]
    fn current_run_start_finds_the_latest_reset_boundary() {
        // Two aborts: the boundary is one past the *latest* one. A reset on a *different* heat does
        // not move this heat's boundary.
        let events = vec![
            scheduled("q-1", &["A"]),                  // 0
            changed("q-1", HeatTransition::Running),   // 1
            pass("A", 1_000_000, 1),                   // 2
            changed("q-1", HeatTransition::Aborted),   // 3
            scheduled("q-2", &["B"]),                  // 4
            changed("q-2", HeatTransition::Aborted),   // 5 — a DIFFERENT heat's reset
            changed("q-1", HeatTransition::Running),   // 6
            pass("A", 2_000_000, 2),                   // 7
            changed("q-1", HeatTransition::Restarted), // 8 — q-1's latest reset
            changed("q-1", HeatTransition::Running),   // 9
        ];
        // q-1's run starts at its latest Running (9) — after the latest restart (8).
        assert_eq!(current_run_start(&events, &HeatId("q-1".into())), 9);
        // A heat that has not run yet (Scheduled, no Running/reset) windows past the end — no
        // current-run passes, so the live count is zero (never a prior-heat total).
        let only_scheduled = [scheduled("q-9", &["Z"])];
        assert_eq!(
            current_run_start(&only_scheduled, &HeatId("q-9".into())),
            only_scheduled.len()
        );
    }

    #[test]
    fn current_run_start_scopes_to_this_heats_run_not_earlier_heats() {
        // A flies q-1 (two laps), then flies q-2 (one lap): q-2's run begins at its own Running, so
        // q-1's crossings are excluded — the live count is this heat's run, not a cross-heat total.
        let events = vec![
            scheduled("q-1", &["A"]),                // 0
            changed("q-1", HeatTransition::Running), // 1
            pass("A", 1_000_000, 1),                 // 2 — q-1 holeshot
            pass("A", 2_000_000, 2),                 // 3 — q-1 lap 1
            pass("A", 3_000_000, 3),                 // 4 — q-1 lap 2
            scheduled("q-2", &["A"]),                // 5
            changed("q-2", HeatTransition::Running), // 6
            pass("A", 10_000_000, 1),                // 7 — q-2 holeshot
            pass("A", 13_000_000, 2),                // 8 — q-2 lap 1
        ];
        // Before the fix this was 0 (the whole log), so q-2's live count summed in q-1's laps.
        assert_eq!(current_run_start(&events, &HeatId("q-2".into())), 6);
        // End to end: the current heat (q-2) shows A with one lap — its q-2 run only, not three.
        let a = live_state(&events)
            .progress
            .into_iter()
            .find(|p| p.competitor == CompetitorRef("A".into()))
            .expect("A is in the current heat");
        assert_eq!(
            a.laps_completed, 1,
            "q-2 live count excludes A's earlier q-1 laps"
        );
    }

    #[test]
    fn run_scoped_lap_count_is_deterministic_on_replay() {
        let events = vec![
            scheduled("q-1", &["A"]),
            changed("q-1", HeatTransition::Running),
            pass("A", 1_000_000, 1),
            pass("A", 4_000_000, 2),
            changed("q-1", HeatTransition::Aborted),
            changed("q-1", HeatTransition::Running),
            pass("A", 9_000_000, 3),
        ];
        assert_eq!(live_state(&events), live_state(&events));
    }

    fn scheduled_tagged(id: &str, lineup: &[&str], class: &str, round: &str) -> Event {
        Event::HeatScheduled {
            heat: HeatId(id.into()),
            lineup: lineup.iter().map(|c| CompetitorRef((*c).into())).collect(),
            class: Some(ClassId(class.into())),
            round: Some(RoundId(round.into())),
            frequencies: vec![],
            label: None,
        }
    }

    #[test]
    fn heat_summaries_lists_each_heat_with_tag_phase_and_current() {
        // q-1 (round r1) ran and scored; q-2 (round r1) is scheduled and current; q-x is an
        // untagged free-text heat that still appears (no round/class).
        let events = vec![
            scheduled_tagged("q-1", &["A", "B"], "open", "r1"),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            changed("q-1", HeatTransition::Finished),
            changed("q-1", HeatTransition::Finalized),
            scheduled_tagged("q-2", &["C", "D"], "open", "r1"),
            scheduled("q-x", &["E"]),
        ];
        // q-1's last event is a transition (Finalized); q-2 and q-x are bare schedules that don't
        // steal focus — so the current heat is q-1 (its `Final` heat stays on the timer between
        // heats, "last heat, now scored"), not the freshly-scheduled q-x.
        let summaries = heat_summaries(&events);
        assert_eq!(summaries.len(), 3);

        // First-scheduled order is preserved.
        assert_eq!(
            summaries
                .iter()
                .map(|h| h.heat.0.clone())
                .collect::<Vec<_>>(),
            vec!["q-1", "q-2", "q-x"]
        );

        let q1 = &summaries[0];
        assert_eq!(q1.round, Some(RoundId("r1".into())));
        assert_eq!(q1.class, Some(ClassId("open".into())));
        assert_eq!(q1.phase, HeatPhase::Final);
        assert!(q1.is_current);
        assert_eq!(
            q1.lineup,
            vec![CompetitorRef("A".into()), CompetitorRef("B".into())]
        );

        let q2 = &summaries[1];
        assert_eq!(q2.round, Some(RoundId("r1".into())));
        assert_eq!(q2.phase, HeatPhase::Scheduled);
        assert!(!q2.is_current);

        let qx = &summaries[2];
        assert_eq!(qx.round, None);
        assert_eq!(qx.class, None);
        assert!(!qx.is_current);
    }

    #[test]
    fn heat_summaries_empty_log_is_empty() {
        assert!(heat_summaries(&[]).is_empty());
    }

    /// Wrap an event as a stored log entry with an explicit `recorded_at` (µs).
    fn stored_at(event: Event, recorded_at: i64) -> StoredEvent {
        StoredEvent {
            offset: 0,
            recorded_at: Some(recorded_at),
            event,
        }
    }

    /// Wrap an event as a stored log entry with no `recorded_at` (an older / untimed entry).
    fn stored(event: Event) -> StoredEvent {
        StoredEvent {
            offset: 0,
            recorded_at: None,
            event,
        }
    }

    #[test]
    fn heat_timing_folds_from_running_and_finished_transitions() {
        // The race-start is the `Running` transition's `recorded_at`; the race-end is the
        // `Finished` (→ Unofficial) transition's. Other transitions carry no timing.
        let log = vec![
            stored(scheduled("q-1", &["A", "B"])),
            stored_at(changed("q-1", HeatTransition::Staged), 100),
            stored_at(changed("q-1", HeatTransition::Armed), 200),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 61_000_000),
        ];
        let (started, ended) = heat_timing(&log, &HeatId("q-1".into()));
        assert_eq!(started, Some(1_000_000));
        assert_eq!(ended, Some(61_000_000));
        // Exactly a 60s duration — the value a frozen clock reads.
        assert_eq!(ended.unwrap() - started.unwrap(), 60_000_000);
    }

    #[test]
    fn heat_timing_running_only_has_no_end_yet() {
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 5_000_000),
        ];
        let (started, ended) = heat_timing(&log, &HeatId("q-1".into()));
        assert_eq!(started, Some(5_000_000));
        assert_eq!(ended, None);
    }

    #[test]
    fn heat_timing_rerun_after_abort_restamps_start_and_clears_end() {
        // A run, an abort, then a fresh run: the latest `Running` is the start and the
        // earlier end is cleared (the new run has not finished).
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
            stored_at(changed("q-1", HeatTransition::Aborted), 2_500_000),
            stored_at(changed("q-1", HeatTransition::Running), 9_000_000),
        ];
        let (started, ended) = heat_timing(&log, &HeatId("q-1".into()));
        assert_eq!(started, Some(9_000_000));
        assert_eq!(ended, None);
    }

    #[test]
    fn with_heat_timing_sets_the_current_heats_instants() {
        let log = vec![
            stored(scheduled("q-1", &["A", "B"])),
            stored_at(changed("q-1", HeatTransition::Running), 7_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 67_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.current_heat, Some(HeatId("q-1".into())));
        assert_eq!(live.race_started_at, Some(7_000_000));
        assert_eq!(live.race_ended_at, Some(67_000_000));
    }

    #[test]
    fn with_heat_timing_no_current_heat_leaves_timing_none() {
        let live = with_heat_timing(live_state(&[]), &[]);
        assert_eq!(live.race_started_at, None);
        assert_eq!(live.race_ended_at, None);
        assert_eq!(live.lifecycle, None);
    }

    // --- the start-tone countdown anchor (RD-only tone countdown while Armed) -----------

    fn starting(id: &str, delay_ms: u32) -> Event {
        Event::HeatStarting {
            heat: HeatId(id.into()),
            delay_ms,
        }
    }

    #[test]
    fn tone_at_is_armed_recorded_instant_plus_delay() {
        // The heat armed at t=1_000_000µs with a 3000ms (3s) hold ⇒ the tone fires at 4_000_000µs.
        let log = vec![
            stored(scheduled("q-1", &["A", "B"])),
            stored_at(changed("q-1", HeatTransition::Staged), 500_000),
            stored_at(changed("q-1", HeatTransition::Armed), 1_000_000),
            stored_at(starting("q-1", 3000), 1_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.phase, HeatPhase::Armed);
        assert_eq!(live.tone_at, Some(4_000_000));
    }

    #[test]
    fn tone_at_is_cleared_once_the_heat_is_running() {
        // Once the heat advances to Running the tone has fired; `race_started_at` is the anchor now,
        // so `tone_at` is `None` (no stale countdown for a late join after race-go).
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(starting("q-1", 3000), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Running), 4_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.phase, HeatPhase::Running);
        assert_eq!(live.tone_at, None);
        assert_eq!(live.race_started_at, Some(4_000_000));
    }

    #[test]
    fn tone_at_is_none_before_the_heat_arms() {
        // A merely-staged heat (no HeatStarting yet) has no tone instant.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Staged), 500_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.phase, HeatPhase::Staged);
        assert_eq!(live.tone_at, None);
    }

    #[test]
    fn tone_at_uses_the_latest_starting_on_a_re_arm() {
        // The heat armed, was aborted back, and re-armed: the latest `HeatStarting` wins (a fresh
        // random hold). The `Running` between them (the abort path never reaches Running here) does
        // not apply; the second arming's instant + delay is the live tone.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(starting("q-1", 2000), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Aborted), 1_500_000),
            stored_at(changed("q-1", HeatTransition::Staged), 2_000_000),
            stored_at(changed("q-1", HeatTransition::Armed), 2_500_000),
            stored_at(starting("q-1", 5000), 2_500_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.phase, HeatPhase::Armed);
        // 2_500_000µs armed + 5000ms hold = 7_500_000µs.
        assert_eq!(live.tone_at, Some(7_500_000));
    }

    #[test]
    fn tone_at_is_none_for_an_untimed_starting_entry() {
        // An older / untimed `HeatStarting` (no `recorded_at`) yields no absolute instant.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Armed), 1_000_000),
            stored(starting("q-1", 3000)),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.phase, HeatPhase::Armed);
        assert_eq!(live.tone_at, None);
    }

    #[test]
    fn tone_at_folds_deterministically() {
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Armed), 1_000_000),
            stored_at(starting("q-1", 3000), 1_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let a = with_heat_timing(live_state(&events), &log);
        let b = with_heat_timing(live_state(&events), &log);
        assert_eq!(a.tone_at, b.tone_at);
    }

    // --- the provisional → official lifecycle (marshaling Slice 5) -------------------

    fn finalizing(id: &str, at: i64) -> Event {
        Event::HeatFinalizing {
            heat: HeatId(id.into()),
            at,
        }
    }

    #[test]
    fn lifecycle_is_none_before_the_heat_is_unofficial() {
        // A running heat has no result yet — no provisional/official lifecycle to show.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.lifecycle, None);
    }

    #[test]
    fn unofficial_without_a_protest_window_is_provisional_manual() {
        // No `HeatFinalizing` logged ⇒ manual finalize only: Provisional with no auto-official deadline.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(
            live.lifecycle,
            Some(LifecycleState::Provisional {
                auto_official_at: None
            })
        );
    }

    #[test]
    fn unofficial_with_a_protest_window_carries_the_auto_official_deadline() {
        // The runtime logged a deadline ⇒ Provisional with the auto-official instant for the countdown.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
            stored_at(finalizing("q-1", 122_000_000), 2_000_100),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(
            live.lifecycle,
            Some(LifecycleState::Provisional {
                auto_official_at: Some(122_000_000)
            })
        );
    }

    #[test]
    fn final_heat_is_official() {
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
            stored_at(changed("q-1", HeatTransition::Finalized), 3_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(live.lifecycle, Some(LifecycleState::Official));
    }

    #[test]
    fn revert_re_opens_provisional() {
        // Finalize, then Revert back to Unofficial: the heat is Provisional again (the lifecycle
        // tracks the FSM, so a reverted result is correctable once more). The runtime re-arms the
        // auto-official timer by logging a fresh `HeatFinalizing`; here we just assert it re-opened.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
            stored_at(changed("q-1", HeatTransition::Finalized), 3_000_000),
            stored_at(changed("q-1", HeatTransition::Reverted), 4_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(
            live.lifecycle,
            Some(LifecycleState::Provisional {
                auto_official_at: None
            })
        );
    }

    #[test]
    fn a_fresh_run_drops_a_stale_auto_official_deadline() {
        // A heat finished + armed a window, then was restarted and re-run: the new run's window has
        // not been armed yet, so the stale deadline from the previous run is cleared on the `Running`.
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
            stored_at(finalizing("q-1", 122_000_000), 2_000_100),
            stored_at(changed("q-1", HeatTransition::Restarted), 2_500_000),
            stored_at(changed("q-1", HeatTransition::Running), 9_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 10_000_000),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let live = with_heat_timing(live_state(&events), &log);
        assert_eq!(
            live.lifecycle,
            Some(LifecycleState::Provisional {
                auto_official_at: None
            })
        );
    }

    #[test]
    fn lifecycle_folds_deterministically() {
        let log = vec![
            stored(scheduled("q-1", &["A"])),
            stored_at(changed("q-1", HeatTransition::Running), 1_000_000),
            stored_at(changed("q-1", HeatTransition::Finished), 2_000_000),
            stored_at(finalizing("q-1", 122_000_000), 2_000_100),
        ];
        let events: Vec<Event> = log.iter().map(|s| s.event.clone()).collect();
        let a = with_heat_timing(live_state(&events), &log);
        let b = with_heat_timing(live_state(&events), &log);
        assert_eq!(a.lifecycle, b.lifecycle);
    }

    #[test]
    fn heat_summaries_carry_the_frequency_assignment_and_default_empty() {
        // Race redesign Slice 4b: a heat scheduled with per-pilot frequencies surfaces them on the
        // summary (so the Heats UI can label them); a sim/free-text heat with none reads back empty.
        let assigned = Event::HeatScheduled {
            heat: HeatId("q-1".into()),
            lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
            class: None,
            round: None,
            frequencies: vec![
                (CompetitorRef("A".into()), 5658),
                (CompetitorRef("B".into()), 5800),
            ],
            label: None,
        };
        let summaries = heat_summaries(&[assigned, scheduled("q-2", &["C"])]);
        assert_eq!(
            summaries[0].frequencies,
            vec![
                (CompetitorRef("A".into()), 5658),
                (CompetitorRef("B".into()), 5800),
            ]
        );
        assert!(
            summaries[1].frequencies.is_empty(),
            "a heat scheduled with no frequencies has an empty assignment"
        );
    }
}
