//! **Open practice's one and only difference: it is not scored.**
//!
//! An open-practice round is an *ordinary* round in every respect the rest of this codebase can
//! see. Its heat is scheduled, staged, started and ended like any other; its timer passes are
//! appended to the durable event log as plain [`Event::Pass`](gridfpv_events::Event::Pass) facts
//! stamped with the heat; the live state, the lap list, marshaling, the audit trail and the
//! crossing tones all fold that log through the *same* projections every other format uses. There
//! is no parallel lap path, no in-memory accumulator and no live-state overlay.
//!
//! The single thing that still sets practice apart is what this module owns: **a practice round
//! never contributes to results, standings or rankings.** That is a *scoring/display* rule, not a
//! recording rule — the laps are on the log, they are simply not adjudicated into anyone's
//! placement. Every scoring surface consults [`excluded_from_scoring`] (or, for the class join,
//! [`scoring_meta`]) and nothing else, so this file is the whole of the special case.
//!
//! # Why it works this way (D5, reversed 2026-08-24 — see `docs/decisions.html`)
//!
//! Practice laps used to be **in-memory only**, held in an `OpenPracticeLive` accumulator and
//! spliced onto the log-folded live state by a `merge_into` overlay, on the rationale that logging
//! them would bloat the durable log. Measurement on real hardware killed that rationale: one real
//! practice heat wrote **41,413 bytes across 57 `SignalHistory`/`SignalChunk` events** while
//! refusing to write the **~288 bytes** its passes would have cost — practice already logged ~144×
//! more signal than the passes it declined to record. The overlay was a second implementation of
//! the lap fold, re-derived on every stream wake-up, and it cost more than it saved: it re-pushed a
//! "changed" `LiveRaceState` on every signal append (the repeated lap callouts of #396), and every
//! live-state feature had to be written and tested twice. Logging the passes deletes all of it.
//!
//! # What is *not* special about practice
//!
//! - **Unbound seats are fine.** A practice heat's lineup is `node-{i}` competitor refs (the timer
//!   seats), with no pilot binding — so its `PilotProgress.pilot` is naturally `None`. Logging does
//!   not require a pilot; "unbound" never implied "ephemeral".
//! - **Phase, clock, restart and the time limit** are the log's, exactly as for any heat. A
//!   `Restart` drops the run's laps because `app::heat_window_offsets` windows every heat past its
//!   latest reset — the same rule that clears an aborted qualifying run.
//! - **The completion clock** treats a practice round's win condition like anyone else's. A round
//!   created without one stores the inert
//!   [`default_win_condition`](crate::events::default_win_condition) (`BestLap`), which by
//!   construction never ends a heat — so an open practice runs until its `time_limit_secs` or the
//!   RD's `ForceEnd`, with no practice-specific branch anywhere in the runtime.

use gridfpv_engine::format::OpenPractice;

use crate::events::{EventMeta, RoundDef};

/// **Whether `round` is excluded from results / standings / rankings.**
///
/// This is the *one* place practice differs from every other format. It is consulted by:
///
/// - `GET /events/{id}/rounds/{round}/ranking` — an excluded round ranks nobody;
/// - `GET /events/{id}/rounds/{round}/standings` — an excluded round has no standings rows;
/// - `GET /events/{id}/classes/{class}/standings` — via [`scoring_meta`], which drops excluded
///   rounds before the class join folds them;
/// - the heat-scope `result` projection (`GET /events/{id}/snapshot/heat/{heat}?projection=result`)
///   — an excluded round's heat scores an empty [`HeatResult`](gridfpv_engine::scoring::HeatResult).
///
/// Everything else — the live state, the lap list, marshaling, the audit trail — treats an excluded
/// round exactly like any other, because the laps really are on the log.
///
/// Keyed on the **format alone**, deliberately more inclusive than
/// [`round_engine::is_open_practice`](crate::round_engine::is_open_practice) (which also requires
/// [`AllChannels`](crate::events::SeedingRule::AllChannels) seeding before it will auto-create the
/// heat and lay the channels out). A "half-open-practice" round — the practice format with some
/// other seeding, reachable only through the raw API — must still not be scored: erring toward
/// exclusion can only withhold a scoreboard nobody asked for, while erring the other way would
/// publish practice laps as results.
pub fn excluded_from_scoring(round: &RoundDef) -> bool {
    round.format == OpenPractice::NAME
}

/// As [`excluded_from_scoring`], for a heat whose round may not resolve. A heat with **no** round
/// (an ad-hoc / sim heat) is *not* excluded — it scores under the neutral fallback rule, unchanged.
pub fn heat_excluded_from_scoring(round: Option<&RoundDef>) -> bool {
    round.is_some_and(excluded_from_scoring)
}

/// `meta` with every [`excluded_from_scoring`] round removed — the view the **class standings**
/// join folds over.
///
/// A practice round carries no `classes`, so the class join would skip it anyway; filtering here
/// makes the exclusion deliberate and total rather than incidental, so a practice round that
/// somehow acquired a class still cannot reach the standings.
pub fn scoring_meta(meta: &EventMeta) -> EventMeta {
    let mut meta = meta.clone();
    meta.rounds.retain(|round| !excluded_from_scoring(round));
    meta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        ChannelMode, SeedingRule, StartProcedure, default_grace_window, default_staging_timer_secs,
        default_win_condition,
    };
    use crate::scope::{ClassId, EventId};
    use gridfpv_engine::heat::ProtestWindow;
    use gridfpv_events::RoundId;

    fn round(id: &str, format: &str) -> RoundDef {
        RoundDef {
            id: RoundId(id.into()),
            label: id.into(),
            classes: vec![],
            format: format.into(),
            params: std::collections::BTreeMap::new(),
            win_condition: default_win_condition(),
            seeding: SeedingRule::FromRoster,
            channel_mode: ChannelMode::PerHeat,
            staging_timer_secs: default_staging_timer_secs(),
            start_procedure: StartProcedure::default(),
            grace_window: default_grace_window(),
            protest_window: ProtestWindow::Off,
            min_lap_secs: None,
            time_limit_secs: None,
        }
    }

    fn meta(rounds: Vec<RoundDef>) -> EventMeta {
        EventMeta {
            id: EventId("e".into()),
            name: "E".into(),
            created_at: 0,
            persistent: false,
            date: None,
            location: None,
            description: None,
            organizer: None,
            timers: vec![],
            primary_timer: None,
            roster: vec![],
            classes: vec![ClassId("open".into())],
            classes_membership: vec![],
            rounds,
            channel_layers: vec![],
        }
    }

    #[test]
    fn only_open_practice_is_excluded_from_scoring() {
        assert!(excluded_from_scoring(&round("op", "open_practice")));
        assert!(!excluded_from_scoring(&round("q", "timed_qual")));
        assert!(!excluded_from_scoring(&round("h", "head_to_head")));
        assert!(!excluded_from_scoring(&round("z", "zippyq")));
    }

    #[test]
    fn a_heat_with_no_round_is_not_excluded() {
        // An ad-hoc / sim heat keeps the neutral fallback scoring rule — only a *practice round's*
        // heat is excluded.
        assert!(!heat_excluded_from_scoring(None));
        assert!(!heat_excluded_from_scoring(Some(&round("q", "timed_qual"))));
        assert!(heat_excluded_from_scoring(Some(&round(
            "op",
            "open_practice"
        ))));
    }

    #[test]
    fn scoring_meta_drops_practice_rounds_even_when_they_carry_a_class() {
        let mut practice = round("op", "open_practice");
        // A practice round normally has no class; give it one to prove the filter is deliberate and
        // not merely riding on an empty `classes` list.
        practice.classes = vec![ClassId("open".into())];
        let meta = meta(vec![
            round("q1", "timed_qual"),
            practice,
            round("q2", "zippyq"),
        ]);

        let filtered = scoring_meta(&meta);
        assert_eq!(
            filtered
                .rounds
                .iter()
                .map(|r| r.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["q1", "q2"],
            "the practice round is dropped before the class join folds the rounds"
        );
    }
}
