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

/// One open-practice heat's **in-memory** state: the active heat, its channel lineup, and the
/// accumulated lap-gate passes (never logged).
#[derive(Debug, Clone)]
struct ActivePractice {
    /// The open-practice heat currently accumulating laps.
    heat: HeatId,
    /// The active channels as `node-{i}` competitor refs, in node order — the heat's lineup.
    channels: Vec<CompetitorRef>,
    /// The lap-gate passes seen for this heat so far, in arrival order. These are **not** appended
    /// to the event log; they live only here and drive the live per-channel laps.
    passes: Vec<Pass>,
}

impl ActivePractice {
    /// Synthesize the event slice the live-state fold consumes: the heat's `HeatScheduled` over the
    /// active channels, a `Running` transition (so the live phase reads `Running`), then the
    /// accumulated passes. Reusing [`live_state`] over this slice gives per-channel laps with
    /// `pilot: None` for free — the same fold the logged path uses, no second lap definition.
    fn synthetic_events(&self) -> Vec<Event> {
        let mut events = Vec::with_capacity(self.passes.len() + 2);
        events.push(Event::HeatScheduled {
            heat: self.heat.clone(),
            lineup: self.channels.clone(),
            class: None,
            round: None,
            frequencies: Vec::new(),
        });
        events.push(Event::HeatStateChanged {
            heat: self.heat.clone(),
            transition: HeatTransition::Running,
        });
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
        });
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
