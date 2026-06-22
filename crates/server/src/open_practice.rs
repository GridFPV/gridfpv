//! The **open-practice live accumulator** (open-practice format, Slice 1) — the per-channel,
//! **in-memory (NOT logged)** lap store and its live delivery onto the existing `/stream`.
//!
//! # Why this exists — laps that are never logged
//!
//! Open practice is casual: a round is one open heat over the active **channels** (`node-{i}`
//! seats), and each channel's laps are shown live but **deliberately not recorded**. The session
//! itself *is* recorded (the heat's `HeatScheduled` + its start/stop `HeatStateChanged` are logged),
//! but the practice *passes* are not — so the durable log carries the session boundaries and zero
//! `Pass` events for an open-practice heat. The source bridge therefore routes an open-practice
//! heat's timer passes **here**, into [`OpenPracticeLive`], instead of `AppState::append`.
//!
//! # Live delivery — a fresh-value re-snapshot on the existing `/stream`
//!
//! The change stream folds the [`LiveRaceState`] purely from the log; non-logged laps cannot drive
//! it through the fold. So the accumulator is a small **overlay** the live-state fold checks: while
//! an open-practice heat is active, the fold returns the accumulator's computed [`LiveRaceState`]
//! (per channel, `pilot: None`) **instead of** the log fold — and every accumulator mutation wakes
//! the same append-notify ([`AppState::appended`](crate::app::AppState)) that an `append` would, so a
//! parked stream re-folds and pushes a fresh-value envelope. This reuses the whole existing
//! snapshot and change-stream machinery (the [`LiveRaceState`]/[`PilotProgress`] shape, the
//! fresh-value envelope, the WS transport) with **no new channel and no new wire type** — the one
//! cost is this per-event overlay cell the live-state fold consults.
//!
//! # The laps come from the same projection (no second lap definition)
//!
//! The per-channel laps are derived by feeding a synthetic event slice — the heat's `HeatScheduled`
//! over the active channels plus the accumulated lap-gate passes — straight to
//! [`live_state`](crate::live_state::live_state). So consecutive passes become laps via the exact
//! same fold the logged path uses; a channel is just an unbound competitor (`node-{i}`), so its
//! `PilotProgress.pilot` is naturally `None`.

use std::sync::{Arc, RwLock};

use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition, Pass};

use crate::live_state::{LiveRaceState, live_state};

/// One open-practice heat's **in-memory** state: the active heat, its channel lineup, the
/// accumulated lap-gate passes (never logged), and the heat's **current loop phase**.
#[derive(Debug, Clone)]
struct ActivePractice {
    /// The open-practice heat currently accumulating laps.
    heat: HeatId,
    /// The active channels as `node-{i}` competitor refs, in node order — the heat's lineup.
    channels: Vec<CompetitorRef>,
    /// The lap-gate passes seen for this heat so far, in arrival order. These are **not** appended
    /// to the event log; they live only here and drive the live per-channel laps.
    passes: Vec<Pass>,
    /// The heat-loop transitions this open-practice heat has gone through *after* `Running`, in
    /// order — threaded in by the source bridge as it observes the heat's `HeatStateChanged`
    /// (open-practice overlay-phase fix). Empty while the practice is live (`Running`); gains
    /// `Finished` when the time limit (or a `ForceEnd`) closes the race, so the overlay reports
    /// `Unofficial` and the console clock **freezes** — and any subsequent step (e.g. `Finalized`)
    /// the heat reaches while its laps are still shown. Re-synthesized into the live fold so the
    /// overlay's phase tracks the *real* heat phase, not a hardcoded `Running`.
    transitions: Vec<HeatTransition>,
}

impl ActivePractice {
    /// Synthesize the event slice the live-state fold consumes: the heat's `HeatScheduled` over the
    /// active channels, a `Running` transition, the **actual subsequent transitions** the heat has
    /// reached (e.g. `Finished` once its time limit fires, so the live phase reads `Unofficial`),
    /// then the accumulated passes. Reusing [`live_state`] over this slice gives per-channel laps
    /// with `pilot: None` *and* the heat's real current phase for free — the same fold the logged
    /// path uses, no second lap definition and no hardcoded phase.
    fn synthetic_events(&self) -> Vec<Event> {
        let mut events = Vec::with_capacity(self.passes.len() + 2 + self.transitions.len());
        events.push(Event::HeatScheduled {
            heat: self.heat.clone(),
            lineup: self.channels.clone(),
            class: None,
            round: None,
            frequencies: Vec::new(),
        });
        // The race is live from `Running`; the bridge threads in any later transition (e.g. the
        // time-limit `Finished` → `Unofficial`) so the overlay phase — and so the console clock —
        // follows the heat. The passes follow the transitions; the lap fold is order-independent
        // over them, and `live_state` resolves the phase from the transition sequence.
        events.push(Event::HeatStateChanged {
            heat: self.heat.clone(),
            transition: HeatTransition::Running,
        });
        events.extend(
            self.transitions
                .iter()
                .copied()
                .map(|transition| Event::HeatStateChanged {
                    heat: self.heat.clone(),
                    transition,
                }),
        );
        events.extend(self.passes.iter().cloned().map(Event::Pass));
        events
    }

    /// The live race-state for this open-practice heat: per-channel laps from the accumulated
    /// passes, each channel an unbound competitor (`pilot: None`).
    fn live(&self) -> LiveRaceState {
        live_state(&self.synthetic_events())
    }
}

/// The shared **open-practice live accumulator** for one event (open-practice format, Slice 1).
///
/// Holds the active open-practice heat's in-memory per-channel passes, or `None` when no
/// open-practice heat is active. Cloning shares the one cell (`Arc<RwLock<…>>`) between the source
/// bridge (which writes passes / starts / clears it) and the `/stream` live-state fold (which reads
/// its computed live state). One per event, alongside the event's [`AppState`](crate::app::AppState).
#[derive(Clone, Default)]
pub struct OpenPracticeLive {
    inner: Arc<RwLock<Option<ActivePractice>>>,
}

impl OpenPracticeLive {
    /// An accumulator with no active open-practice heat.
    pub fn new() -> Self {
        Self::default()
    }

    /// **Begin** accumulating for an open-practice `heat` over `channels` (its `node-{i}` lineup),
    /// clearing any prior open-practice state. Called when an open-practice heat goes `Running`.
    pub fn begin(&self, heat: HeatId, channels: Vec<CompetitorRef>) {
        *self.write() = Some(ActivePractice {
            heat,
            channels,
            passes: Vec::new(),
            transitions: Vec::new(),
        });
    }

    /// Thread one heat-loop `transition` for the active open-practice `heat` into the overlay
    /// (open-practice overlay-phase fix). The source bridge calls this as it observes the heat's
    /// `HeatStateChanged`, so the overlay reports the heat's **real** current phase: `Running` while
    /// the practice is live, then `Unofficial` once the time limit (or a `ForceEnd`) closes it —
    /// which freezes the console race clock at the practice duration — while the per-channel laps
    /// stay visible. The accumulator is **not** cleared here (the clear-on-terminal path drops it);
    /// `Running` is implicit (the base of every synthetic slice) so re-recording it is a no-op.
    ///
    /// A no-op when `heat` is not the active open-practice heat. Returns whether the overlay phase
    /// actually changed, so the caller wakes `/stream` only when the live state moved.
    pub fn transition(&self, heat: &HeatId, transition: HeatTransition) -> bool {
        let mut guard = self.write();
        match guard.as_mut() {
            Some(active) if &active.heat == heat && transition != HeatTransition::Running => {
                active.transitions.push(transition);
                true
            }
            _ => false,
        }
    }

    /// Record one lap-gate `pass` for the active open-practice heat (in memory, **not** logged).
    ///
    /// A no-op when there is no active open-practice heat (a stray pass after a clear). Returns
    /// whether the pass was accepted, so the caller can wake `/stream` only when the live state
    /// actually changed.
    pub fn record(&self, pass: Pass) -> bool {
        let mut guard = self.write();
        match guard.as_mut() {
            Some(active) => {
                active.passes.push(pass);
                true
            }
            None => false,
        }
    }

    /// **Clear** the accumulator — drop all in-memory open-practice laps. Called when the
    /// open-practice heat leaves `Running` (a terminal / abort transition) or a new heat/round takes
    /// over. Returns whether anything was actually cleared (so the caller wakes `/stream` to push the
    /// now-idle live state only when needed).
    pub fn clear(&self) -> bool {
        let mut guard = self.write();
        if guard.is_some() {
            *guard = None;
            true
        } else {
            false
        }
    }

    /// Whether `heat` is the open-practice heat currently accumulating.
    pub fn is_active(&self, heat: &HeatId) -> bool {
        self.read().as_ref().map(|a| &a.heat) == Some(heat)
    }

    /// The current open-practice [`LiveRaceState`] overlay, or `None` when no open-practice heat is
    /// active (the `/stream` fold then uses the normal log fold). Per channel, `pilot: None`.
    pub fn live_state(&self) -> Option<LiveRaceState> {
        self.read().as_ref().map(ActivePractice::live)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Option<ActivePractice>> {
        self.inner
            .read()
            .expect("open-practice accumulator poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Option<ActivePractice>> {
        self.inner
            .write()
            .expect("open-practice accumulator poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::HeatPhase;
    use gridfpv_events::{AdapterId, GateIndex, SourceTime};

    fn chan(i: usize) -> CompetitorRef {
        CompetitorRef(format!("node-{i}"))
    }

    fn pass(node: usize, at: i64, seq: u64) -> Pass {
        Pass {
            adapter: AdapterId("sim".into()),
            competitor: chan(node),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
        }
    }

    #[test]
    fn idle_accumulator_has_no_live_state() {
        let live = OpenPracticeLive::new();
        assert!(live.live_state().is_none());
        assert!(!live.clear(), "clearing an idle accumulator is a no-op");
        assert!(
            !live.record(pass(0, 0, 0)),
            "a pass with no active heat is dropped"
        );
    }

    #[test]
    fn per_channel_laps_are_derived_with_no_pilot_bound() {
        let live = OpenPracticeLive::new();
        let heat = HeatId("open-practice".into());
        live.begin(heat.clone(), vec![chan(0), chan(2)]);
        assert!(live.is_active(&heat));

        // node-0: holeshot + 2 laps (last lap 2.5s). node-2: holeshot + 1 lap (3.0s).
        assert!(live.record(pass(0, 1_000_000, 0)));
        assert!(live.record(pass(2, 1_500_000, 0)));
        assert!(live.record(pass(0, 4_000_000, 1)));
        assert!(live.record(pass(2, 4_500_000, 1)));
        assert!(live.record(pass(0, 6_500_000, 2)));

        let state = live
            .live_state()
            .expect("an active open-practice live state");
        assert_eq!(state.current_heat, Some(heat));
        // While the practice is live (no transition threaded past `Running`), the overlay phase is
        // `Running` — so the console race clock ticks.
        assert_eq!(state.phase, HeatPhase::Running);
        // Two channels, each a row; neither bound to a pilot (open practice is per channel).
        assert_eq!(state.progress.len(), 2);
        assert!(state.progress.iter().all(|p| p.pilot.is_none()));

        let n0 = state
            .progress
            .iter()
            .find(|p| p.competitor == chan(0))
            .unwrap();
        assert_eq!(n0.laps_completed, 2);
        assert_eq!(n0.last_lap_micros, Some(2_500_000));

        let n2 = state
            .progress
            .iter()
            .find(|p| p.competitor == chan(2))
            .unwrap();
        assert_eq!(n2.laps_completed, 1);
    }

    #[test]
    fn clear_drops_the_accumulator() {
        let live = OpenPracticeLive::new();
        let heat = HeatId("open-practice".into());
        live.begin(heat.clone(), vec![chan(0)]);
        live.record(pass(0, 0, 0));
        assert!(live.live_state().is_some());

        assert!(
            live.clear(),
            "clearing an active accumulator reports a change"
        );
        assert!(live.live_state().is_none());
        assert!(!live.is_active(&heat));
    }

    #[test]
    fn finished_transition_reports_unofficial_with_laps_intact() {
        // Open-practice overlay-phase fix: when the heat's time limit fires `Running → Unofficial`
        // (a `Finished` transition), the overlay must report **`Unofficial`** (so the console clock
        // freezes) while the per-channel laps stay visible — not the old hardcoded `Running`.
        let live = OpenPracticeLive::new();
        let heat = HeatId("open-practice".into());
        live.begin(heat.clone(), vec![chan(0)]);
        live.record(pass(0, 1_000_000, 0)); // holeshot
        live.record(pass(0, 4_000_000, 1)); // +1 lap (3.0s)

        // While racing the overlay is `Running`.
        assert_eq!(live.live_state().unwrap().phase, HeatPhase::Running);

        // The time limit closes the race: thread the real `Finished` transition in.
        assert!(
            live.transition(&heat, HeatTransition::Finished),
            "threading Finished into the active open-practice heat reports a change"
        );

        let state = live
            .live_state()
            .expect("the overlay survives Running → Unofficial");
        assert_eq!(
            state.phase,
            HeatPhase::Unofficial,
            "the overlay reports Unofficial once the heat finishes, so the clock freezes"
        );
        // The laps are NOT cleared by the Running → Unofficial step.
        let n0 = state
            .progress
            .iter()
            .find(|p| p.competitor == chan(0))
            .unwrap();
        assert_eq!(n0.laps_completed, 1);
        assert_eq!(n0.last_lap_micros, Some(3_000_000));

        // A subsequent `Finalized` step folds to `Final`, still carrying the laps.
        assert!(live.transition(&heat, HeatTransition::Finalized));
        let state = live.live_state().unwrap();
        assert_eq!(state.phase, HeatPhase::Final);
        assert_eq!(
            state
                .progress
                .iter()
                .find(|p| p.competitor == chan(0))
                .unwrap()
                .laps_completed,
            1
        );
    }

    #[test]
    fn transition_is_a_no_op_for_a_stale_or_idle_heat() {
        let live = OpenPracticeLive::new();
        // No active heat → no-op.
        assert!(!live.transition(&HeatId("h1".into()), HeatTransition::Finished));

        live.begin(HeatId("h1".into()), vec![chan(0)]);
        // A transition for a *different* heat is ignored (the overlay phase doesn't move).
        assert!(!live.transition(&HeatId("h2".into()), HeatTransition::Finished));
        assert_eq!(live.live_state().unwrap().phase, HeatPhase::Running);
        // Re-recording `Running` is a no-op (it is the implicit base of the slice).
        assert!(!live.transition(&HeatId("h1".into()), HeatTransition::Running));
        assert_eq!(live.live_state().unwrap().phase, HeatPhase::Running);
    }

    #[test]
    fn begin_replaces_a_prior_heat() {
        let live = OpenPracticeLive::new();
        live.begin(HeatId("h1".into()), vec![chan(0)]);
        live.record(pass(0, 0, 0));
        // A new heat takes over: the prior heat's laps are dropped.
        live.begin(HeatId("h2".into()), vec![chan(1)]);
        assert!(live.is_active(&HeatId("h2".into())));
        let state = live.live_state().unwrap();
        assert_eq!(state.active_pilots, vec![chan(1)]);
    }
}
