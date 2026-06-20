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
//! (race-engine.html §2). The **current heat** is the most-recently-active one: the heat
//! whose latest state-changing event appears last in the log and that is not yet
//! `Scored`/`Advanced` past the timer. We resolve it by scanning the log in order and
//! tracking the last heat to receive a `HeatScheduled` or `HeatStateChanged`. A heat
//! that has reached the terminal `Scored` phase is still reported as the current heat
//! until a *newer* heat takes the timer (a freshly-scheduled or transitioned heat),
//! which mirrors what an overlay shows between heats ("last heat, now scored").
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
use gridfpv_events::{CompetitorRef, Event, HeatId, PilotId};
use gridfpv_projection::{CompetitorKey, lap_list_marshaled, registrations};
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
    let Some(current_heat) = current_heat(events) else {
        return LiveRaceState::default();
    };

    let phase = heat_state(events, &current_heat)
        .map(phase_of)
        .unwrap_or(HeatPhase::Scheduled);

    let active_pilots = lineup_of(events, &current_heat);

    // The lap projection is keyed on (adapter, competitor); the lineup carries only the
    // competitor handle. Fold the whole log once (marshaling-aware) and index laps by
    // competitor ref, summing across adapters for a competitor seen on more than one.
    let laps = lap_list_marshaled(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
    let mut by_ref: BTreeMap<&CompetitorRef, (u32, Option<i64>)> = BTreeMap::new();
    for cl in &laps.competitors {
        let CompetitorKey { competitor, .. } = &cl.competitor;
        let entry = by_ref.entry(competitor).or_insert((0, None));
        entry.0 += cl.lap_count() as u32;
        entry.1 = cl.laps.last().map(|l| l.duration_micros).or(entry.1);
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
            let (laps_completed, last_lap_micros) =
                by_ref.get(competitor).copied().unwrap_or((0, None));
            PilotProgress {
                competitor: competitor.clone(),
                pilot: pilot_by_ref.get(competitor).map(|p| (*p).clone()),
                laps_completed,
                last_lap_micros,
            }
        })
        .collect();

    let running_order = running_order(&progress);

    LiveRaceState {
        current_heat: Some(current_heat.clone()),
        phase,
        active_pilots,
        progress,
        running_order,
        on_deck: on_deck(events, &current_heat),
    }
}

/// Map a folded [`HeatState`] to the projected [`HeatPhase`] the live view reports.
fn phase_of(state: HeatState) -> HeatPhase {
    match state {
        HeatState::Scheduled => HeatPhase::Scheduled,
        HeatState::Staged => HeatPhase::Staged,
        HeatState::Armed => HeatPhase::Armed,
        HeatState::Running => HeatPhase::Running,
        HeatState::Finished => HeatPhase::Finished,
        HeatState::Scored => HeatPhase::Scored,
    }
}

/// The current heat: the heat whose latest `HeatScheduled` / `HeatStateChanged` event
/// appears last in the log. `None` if no heat was ever scheduled.
fn current_heat(events: &[Event]) -> Option<HeatId> {
    let mut current: Option<HeatId> = None;
    for event in events {
        match event {
            Event::HeatScheduled { heat, .. } | Event::HeatStateChanged { heat, .. } => {
                current = Some(heat.clone());
            }
            _ => {}
        }
    }
    current
}

/// The lineup of a heat: the competitors from its most recent `HeatScheduled`.
fn lineup_of(events: &[Event], heat: &HeatId) -> Vec<CompetitorRef> {
    let mut lineup = Vec::new();
    for event in events {
        if let Event::HeatScheduled { heat: h, lineup: l } = event {
            if h == heat {
                lineup = l.clone();
            }
        }
    }
    lineup
}

/// The on-deck heat: the earliest still-`Scheduled` heat that is not the current one.
///
/// "Still scheduled" means its folded [`HeatState`] is `Scheduled` (it has been created
/// but not staged). Heats are considered in the order they were first scheduled in the
/// log, so the on-deck heat is the next one queued behind the current heat.
fn on_deck(events: &[Event], current: &HeatId) -> Option<HeatId> {
    let mut seen: Vec<HeatId> = Vec::new();
    for event in events {
        if let Event::HeatScheduled { heat, .. } = event {
            if !seen.contains(heat) {
                seen.push(heat.clone());
            }
        }
    }
    seen.into_iter()
        .find(|heat| heat != current && heat_state(events, heat) == Some(HeatState::Scheduled))
}

/// Rank active pilots into the provisional running order: most laps first, ties broken
/// by the shorter last-lap time (a proxy for who is pacing ahead), then by competitor
/// ref for a total, deterministic order.
fn running_order(progress: &[PilotProgress]) -> Vec<CompetitorRef> {
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
    fn phase_tracks_the_heat_loop_through_scored() {
        // Scheduled → Staged → Armed → Running → Finished → Scored.
        let steps = [
            (HeatTransition::Staged, HeatPhase::Staged),
            (HeatTransition::Armed, HeatPhase::Armed),
            (HeatTransition::Running, HeatPhase::Running),
            (HeatTransition::Finished, HeatPhase::Finished),
            (HeatTransition::Scored, HeatPhase::Scored),
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
    fn current_heat_follows_the_most_recently_active_heat() {
        // q-1 runs and scores; q-2 is then scheduled and becomes current.
        let events = vec![
            scheduled("q-1", &["A", "B"]),
            changed("q-1", HeatTransition::Staged),
            changed("q-1", HeatTransition::Armed),
            changed("q-1", HeatTransition::Running),
            changed("q-1", HeatTransition::Finished),
            changed("q-1", HeatTransition::Scored),
            scheduled("q-2", &["C", "D"]),
        ];
        let s = live_state(&events);
        assert_eq!(s.current_heat, Some(HeatId("q-2".into())));
        assert_eq!(s.phase, HeatPhase::Scheduled);
        assert_eq!(
            s.active_pilots,
            vec![CompetitorRef("C".into()), CompetitorRef("D".into())]
        );
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
}
