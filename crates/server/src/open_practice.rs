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
//! # Live delivery — the **real log's** phase/clock, overlaid with the per-channel laps
//!
//! The live-state fold is normally a pure fold of the log; the non-logged practice laps can't drive
//! it through that fold. Earlier this accumulator served a **fully synthetic** [`LiveRaceState`]
//! (its own `Running` start plus a shadow-recorded transition list) *in place of* the log fold —
//! but that synthetic phase/clock **drifted** from the real heat: a `Restart` resetting the heat to
//! `Scheduled` was never reflected (so the console kept rendering `Unofficial`/Final buttons and
//! `Restart` then errored), and re-computing the synthetic slice reset the clock basis (the
//! ~0.104 s clock bump after the time limit fired).
//!
//! So the overlay is now **laps-only**, and phase/clock are authoritative from the log. The
//! live-state fold computes the [`LiveRaceState`] from the **real log** exactly as for any heat
//! (the real `HeatScheduled` + the real `HeatStateChanged` transitions → truthful `current_heat`,
//! `phase`, lineup, on-deck), then calls [`OpenPracticeLive::merge_into`] to splice the
//! accumulator's per-channel laps into that base when its active heat *is* the current heat. The
//! phase/clock therefore **cannot drift** — they are always the log's — while the non-logged laps
//! still show. Every accumulator mutation **and every clear** wakes the same append-notify
//! ([`AppState::appended`](crate::app::AppState)) a real `append` would, so a parked stream re-folds
//! and pushes a fresh-value envelope; when the accumulator clears, that re-fold is overlay-free and
//! the console immediately settles back onto the bare log state (no stale frame).
//!
//! # The laps come from the same projection (no second lap definition)
//!
//! The per-channel laps are derived by feeding a synthetic event slice — the heat's `HeatScheduled`
//! over the active channels plus the accumulated lap-gate passes — straight to
//! [`live_state`](crate::live_state::live_state). So consecutive passes become laps via the exact
//! same fold the logged path uses; a channel is just an unbound competitor (`node-{i}`), so its
//! `PilotProgress.pilot` is naturally `None`. Only the per-channel `progress` is read off that
//! slice; the slice's own phase is ignored (the served phase is the log's).

use std::sync::{Arc, RwLock};

use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition, Pass};

use crate::live_state::{LiveRaceState, PilotProgress, live_state};

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
    /// Synthesize the event slice the lap fold consumes: the heat's `HeatScheduled` over the active
    /// channels, a `Running` transition, then the accumulated passes. Reusing [`live_state`] over
    /// this slice gives per-channel laps with `pilot: None` for free — the same fold the logged path
    /// uses, no second lap definition. Only the resulting `progress` is read; the phase the slice
    /// folds to is **ignored** (the served phase comes from the real log, not from here).
    fn synthetic_events(&self) -> Vec<Event> {
        let mut events = Vec::with_capacity(self.passes.len() + 2);
        events.push(Event::HeatScheduled {
            heat: self.heat.clone(),
            lineup: self.channels.clone(),
            class: None,
            round: None,
            frequencies: Vec::new(),
            label: None,
        });
        // A `Running` base so the lap fold treats the passes as in-heat laps. This is *only* for lap
        // derivation; the phase served to clients is always the real log's.
        events.push(Event::HeatStateChanged {
            heat: self.heat.clone(),
            transition: HeatTransition::Running,
        });
        events.extend(self.passes.iter().cloned().map(Event::Pass));
        events
    }

    /// The accumulator's per-channel live race-state: per-channel laps from the accumulated passes,
    /// each channel an unbound competitor (`pilot: None`). Its phase is always `Running` (the
    /// synthetic base) and is **not** what clients are served — only its [`progress`](LiveRaceState::progress)
    /// is spliced into the log-authoritative base by [`merge_into`](OpenPracticeLive::merge_into).
    fn live(&self) -> LiveRaceState {
        live_state(&self.synthetic_events())
    }
}

/// The shared **open-practice live accumulator** for one event (open-practice format, Slice 1).
///
/// Holds the active open-practice heat's in-memory per-channel passes, or `None` when no
/// open-practice heat is active. Cloning shares the one cell (`Arc<RwLock<…>>`) between the source
/// bridge (which writes passes / starts / clears it) and the `/stream` live-state fold (which merges
/// its laps onto the log-authoritative state). One per event, alongside the event's
/// [`AppState`](crate::app::AppState).
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
    /// open-practice heat leaves `Running` (a terminal / abort / restart transition) or a new
    /// heat/round takes over. Returns whether anything was actually cleared (so the caller wakes
    /// `/stream` to re-fold the now-overlay-free log state only when needed).
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

    /// The open-practice heat currently accumulating, if any. Used to decide whether a Heat-scope
    /// fold should have the overlay laps spliced in.
    pub fn active_heat(&self) -> Option<HeatId> {
        self.read().as_ref().map(|a| a.heat.clone())
    }

    /// Splice the accumulator's per-channel laps into a **log-authoritative** `base` live state.
    ///
    /// `base` is the [`LiveRaceState`] folded from the real log — so its `current_heat`, `phase`,
    /// lineup, running clock basis and on-deck are always truthful (a `Restart → Scheduled` is
    /// reflected, the time-limit `Running → Unofficial` is reflected, and the clock basis never
    /// resets). When the active open-practice heat **is** that current heat, this overlays the
    /// non-logged per-channel lap counts / last-lap onto the matching `progress` rows and recomputes
    /// the running order; otherwise (no active heat, or a different heat is current) `base` is
    /// returned unchanged. Phase/clock therefore can never drift from the log — only the laps come
    /// from the overlay.
    pub fn merge_into(&self, mut base: LiveRaceState) -> LiveRaceState {
        let guard = self.read();
        let Some(active) = guard.as_ref() else {
            return base;
        };
        // Only splice laps when the overlay's heat is the one the log says is current. Across a
        // `Restart` (heat back to `Scheduled`) the bridge clears the accumulator, but guard anyway so a
        // stale overlay can never bleed laps onto a different heat.
        if base.current_heat.as_ref() != Some(&active.heat) {
            return base;
        }

        // Index the accumulator's per-channel laps by competitor.
        let overlay = active.live();
        let laps_by_ref: std::collections::BTreeMap<&CompetitorRef, &PilotProgress> = overlay
            .progress
            .iter()
            .map(|p| (&p.competitor, p))
            .collect();

        for row in &mut base.progress {
            if let Some(src) = laps_by_ref.get(&row.competitor) {
                row.laps_completed = src.laps_completed;
                row.last_lap_micros = src.last_lap_micros;
            }
        }
        base.running_order = crate::live_state::running_order(&base.progress);
        base
    }

    /// The accumulator's standalone per-channel [`LiveRaceState`] (laps only; phase always
    /// `Running`), or `None` when no open-practice heat is active.
    ///
    /// This is the accumulator's **internal** view — used by the bridge tests to observe the laps
    /// filling. Clients are **not** served this directly; the served live state is the log fold with
    /// these laps spliced in via [`merge_into`](Self::merge_into), so the served phase/clock is
    /// always the log's.
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
    use crate::live_state::live_state;
    use crate::snapshot::HeatPhase;
    use gridfpv_events::{AdapterId, GateIndex, HeatTransition, SourceTime};

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

    /// Build the real-log [`LiveRaceState`] base for an open-practice heat at a given lifecycle
    /// point — the heat's real `HeatScheduled` over its channels plus the real transitions so far.
    /// This is exactly what the fold sites compute before calling [`merge_into`].
    fn log_base(
        heat: &HeatId,
        channels: &[CompetitorRef],
        transitions: &[HeatTransition],
    ) -> LiveRaceState {
        let mut events = vec![Event::HeatScheduled {
            heat: heat.clone(),
            lineup: channels.to_vec(),
            class: None,
            round: None,
            frequencies: Vec::new(),
            label: None,
        }];
        for t in transitions {
            events.push(Event::HeatStateChanged {
                heat: heat.clone(),
                transition: *t,
            });
        }
        live_state(&events)
    }

    #[test]
    fn idle_accumulator_has_no_live_state() {
        let live = OpenPracticeLive::new();
        assert!(live.live_state().is_none());
        assert!(live.active_heat().is_none());
        assert!(!live.clear(), "clearing an idle accumulator is a no-op");
        assert!(
            !live.record(pass(0, 0, 0)),
            "a pass with no active heat is dropped"
        );
    }

    #[test]
    fn idle_accumulator_merge_is_the_log_base_unchanged() {
        // With no active heat the merge returns the log base verbatim — the served state is purely
        // the log fold.
        let live = OpenPracticeLive::new();
        let heat = HeatId("q-1".into());
        let base = log_base(&heat, &[chan(0)], &[HeatTransition::Running]);
        assert_eq!(live.merge_into(base.clone()), base);
    }

    #[test]
    fn merge_splices_per_channel_laps_onto_the_log_phase() {
        // The accumulator holds the non-logged per-channel laps; the merge keeps the LOG's phase and
        // lineup and only fills in the lap counts / last-lap + running order.
        let live = OpenPracticeLive::new();
        let heat = HeatId("open-practice".into());
        let channels = vec![chan(0), chan(2)];
        live.begin(heat.clone(), channels.clone());

        // node-0: holeshot + 2 laps (last lap 2.5s). node-2: holeshot + 1 lap (3.0s).
        assert!(live.record(pass(0, 1_000_000, 0)));
        assert!(live.record(pass(2, 1_500_000, 0)));
        assert!(live.record(pass(0, 4_000_000, 1)));
        assert!(live.record(pass(2, 4_500_000, 1)));
        assert!(live.record(pass(0, 6_500_000, 2)));

        // The log base says the heat is Running (its real transition), with the two channels and no
        // laps (the log carries no passes for an open-practice heat).
        let base = log_base(&heat, &channels, &[HeatTransition::Running]);
        assert_eq!(base.phase, HeatPhase::Running);
        assert!(base.progress.iter().all(|p| p.laps_completed == 0));

        let merged = live.merge_into(base);
        // Phase + heat are the LOG's; laps are the overlay's.
        assert_eq!(merged.current_heat, Some(heat));
        assert_eq!(merged.phase, HeatPhase::Running);
        assert_eq!(merged.progress.len(), 2);
        assert!(merged.progress.iter().all(|p| p.pilot.is_none()));

        let n0 = merged
            .progress
            .iter()
            .find(|p| p.competitor == chan(0))
            .unwrap();
        assert_eq!(n0.laps_completed, 2);
        assert_eq!(n0.last_lap_micros, Some(2_500_000));

        let n2 = merged
            .progress
            .iter()
            .find(|p| p.competitor == chan(2))
            .unwrap();
        assert_eq!(n2.laps_completed, 1);

        // Running order reflects the spliced laps (node-0 leads with 2 laps).
        assert_eq!(merged.running_order, vec![chan(0), chan(2)]);
    }

    #[test]
    fn merge_follows_the_log_phase_through_unofficial_then_scheduled_on_restart() {
        // The core desync fix: the served phase is the LOG's at every step. The accumulator holds
        // laps throughout `Unofficial`; on `Restart` the bridge clears it, and the log base reads
        // `Scheduled` (Restart fully resets, like Abort) — so the merge yields `Scheduled` with NO
        // stale `Unofficial`.
        let live = OpenPracticeLive::new();
        let heat = HeatId("open-practice".into());
        let channels = vec![chan(0)];
        live.begin(heat.clone(), channels.clone());
        live.record(pass(0, 1_000_000, 0)); // holeshot
        live.record(pass(0, 4_000_000, 1)); // +1 lap (3.0s)

        // Running: log phase Running, laps spliced.
        let running = live.merge_into(log_base(&heat, &channels, &[HeatTransition::Running]));
        assert_eq!(running.phase, HeatPhase::Running);
        assert_eq!(running.progress[0].laps_completed, 1);

        // Time limit fires → the LOG records `Finished` (Running → Unofficial). The accumulator is
        // NOT cleared (laps held through Unofficial), so the merge keeps the laps but the phase is
        // the log's `Unofficial`.
        let unofficial = live.merge_into(log_base(
            &heat,
            &channels,
            &[HeatTransition::Running, HeatTransition::Finished],
        ));
        assert_eq!(
            unofficial.phase,
            HeatPhase::Unofficial,
            "the served phase follows the log to Unofficial"
        );
        assert_eq!(unofficial.progress[0].laps_completed, 1);

        // RD hits Restart → the bridge clears the accumulator and the LOG records `Restarted`
        // (heat back to `Scheduled`). The merge of the now-empty accumulator onto the `Scheduled`
        // base is the bare log state: phase `Scheduled`, no stale `Unofficial`, laps cleared.
        assert!(live.clear(), "restart clears the accumulator");
        let scheduled = live.merge_into(log_base(
            &heat,
            &channels,
            &[
                HeatTransition::Running,
                HeatTransition::Finished,
                HeatTransition::Restarted,
            ],
        ));
        assert_eq!(
            scheduled.phase,
            HeatPhase::Scheduled,
            "after Restart the served phase is the log's Scheduled — never a stale Unofficial"
        );
        assert_eq!(
            scheduled.progress[0].laps_completed, 0,
            "the per-channel laps are cleared after Restart"
        );
    }

    #[test]
    fn merge_does_not_splice_onto_a_different_current_heat() {
        // A guard against a stale overlay bleeding laps onto another heat: if the log's current heat
        // is not the accumulator's heat, the base is returned untouched.
        let live = OpenPracticeLive::new();
        live.begin(HeatId("op".into()), vec![chan(0)]);
        live.record(pass(0, 1_000_000, 0));
        live.record(pass(0, 4_000_000, 1));

        let other = HeatId("q-7".into());
        let base = log_base(&other, &[chan(0)], &[HeatTransition::Running]);
        let merged = live.merge_into(base.clone());
        assert_eq!(merged, base, "laps are not spliced onto a non-active heat");
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
        assert!(live.active_heat().is_none());
    }

    #[test]
    fn begin_replaces_a_prior_heat() {
        let live = OpenPracticeLive::new();
        live.begin(HeatId("h1".into()), vec![chan(0)]);
        live.record(pass(0, 0, 0));
        // A new heat takes over: the prior heat's laps are dropped.
        live.begin(HeatId("h2".into()), vec![chan(1)]);
        assert!(live.is_active(&HeatId("h2".into())));
        assert_eq!(live.active_heat(), Some(HeatId("h2".into())));
        let state = live.live_state().unwrap();
        assert_eq!(state.active_pilots, vec![chan(1)]);
    }
}
