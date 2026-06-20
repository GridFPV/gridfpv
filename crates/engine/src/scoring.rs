//! Heat scoring — turn a heat's lap-gate passes + a win condition into a ranking.
//!
//! Scoring is **shared engine logic over the canonical passes** (race-engine.html
//! §4): passes become laps, laps and a [`WinCondition`] become a [`HeatResult`].
//! Every source is scored identically because the input is the source-agnostic
//! [`Pass`] stream.
//!
//! # What a lap is here
//!
//! As in the lap projection ([`gridfpv_projection::lap_list`]), a lap is two
//! consecutive lap-gate passes for one competitor: the first crossing is the
//! holeshot / start (race-engine.html §4, decision "Start / holeshot & lap model"),
//! and a lap *completes* on each subsequent crossing. Unlike the projection, the
//! scorer keeps each completed lap's **absolute completion time** (the completing
//! pass's [`SourceTime`]) as well as its duration, because the win conditions need
//! to compare *when* a lap was finished (timed cutoff, first-to-N), not just how
//! long it took.
//!
//! # Win conditions (race-engine.html §7.1, resolved)
//!
//! The catalogue is [`WinCondition::Timed`] (most laps in a window),
//! [`WinCondition::FirstToLaps`], and the qualifying pair
//! [`WinCondition::BestLap`] / [`WinCondition::BestConsecutive`]. Ties break by the
//! recorded rule per condition (see each variant); every comparison is **total and
//! deterministic** — there is no clock or RNG read anywhere in this module, so the
//! same passes always produce the same ranking and a recorded session replays
//! identically (race-engine.html §6).
//!
//! A genuine, unresolvable tie (identical metric *and* identical deciding times) is
//! left as a **shared position** here; resolving it by a recorded coin flip is a
//! separate adjudication event handled later (#32 / E5), not in this pure scorer.
//!
//! # Provisional / live ranking (race-engine.html §7.4)
//!
//! [`score`] is also the live-ranking function: called on a *partial* pass list
//! (the passes seen so far, mid-heat) it yields the current standing under the same
//! rules. "Mid-heat position is by current lap, then split" falls straight out —
//! ordering is by lap count then completion time of the last counted lap. Nothing
//! about the function distinguishes a finished heat from an in-progress one.
#![forbid(unsafe_code)]

use gridfpv_events::{Event, Pass, SourceTime};
use gridfpv_projection::CompetitorKey;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How a heat is won — the configured per-heat / per-format scoring rule
/// (race-engine.html §4, §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum WinCondition {
    /// **Most laps in a time window.** A shared race clock runs from `race_start`
    /// for `window_micros`. A lap counts only if its completing pass falls
    /// *strictly before* the cutoff (`at < race_start + window_micros`) — a **hard
    /// cutoff**: a lap still in progress when the window closes does not count, and
    /// neither does one whose completing crossing lands exactly on the cutoff. Rank
    /// by counted-lap count (descending); ties break by the earlier completion time
    /// of the last counted lap (whoever banked their final lap first).
    Timed {
        /// Window length in microseconds, measured from the race start.
        /// Renders as a plain TS `number` (bounded far below 2^53).
        #[ts(type = "number")]
        window_micros: i64,
    },
    /// **First to N laps.** Rank by who completed lap `n` earliest. Competitors who
    /// never reached `n` rank after everyone who did, ordered among themselves by
    /// lap count (descending) then the completion time of their last lap.
    FirstToLaps {
        /// The target lap count.
        n: u32,
    },
    /// **Fastest single lap** (qualifying). Rank by smallest lap duration; a
    /// competitor with no completed lap ranks last. Ties break by who *set* that
    /// fastest lap earlier (its completion time).
    BestLap,
    /// **Fastest consecutive `n` laps** (qualifying). Rank by the smallest sum of
    /// any `n` consecutive laps; a competitor with fewer than `n` laps ranks after
    /// everyone who has a window, ordered by lap count (descending). Ties break by
    /// the completion time of the last lap in the best window.
    BestConsecutive {
        /// How many consecutive laps the window spans.
        n: u32,
    },
}

/// One competitor's place in a scored heat.
///
/// `position` is **1-based and dense at the top but tie-aware**: tied competitors
/// share the same `position`, and the next distinct competitor's `position` skips
/// past them (standard "1, 2, 2, 4" competition ranking). `laps` is the number of
/// laps that counted under the win condition (for [`WinCondition::Timed`] that is
/// the number inside the window, not the raw laps flown). `metric` carries the
/// condition-specific deciding value for display / debugging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Placement {
    /// Which source-local competitor this placement is for.
    pub competitor: CompetitorKey,
    /// 1-based finishing position; tied competitors share a position.
    pub position: u32,
    /// Laps that counted under the win condition.
    pub laps: u32,
    /// The condition-specific deciding metric for this competitor.
    pub metric: Metric,
}

/// The condition-specific value a [`Placement`] was ranked on, kept for display and
/// for tests to assert against exact numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Metric {
    /// [`WinCondition::Timed`]: completion time (µs, source clock) of the last
    /// counted lap, or `None` if no lap counted.
    LastLapAt(Option<SourceTime>),
    /// [`WinCondition::FirstToLaps`]: completion time of lap `n`, or `None` if the
    /// competitor never reached `n`.
    ReachedAt(Option<SourceTime>),
    /// [`WinCondition::BestLap`]: fastest lap duration (µs), or `None` if no lap.
    BestLapMicros(#[ts(type = "number | null")] Option<i64>),
    /// [`WinCondition::BestConsecutive`]: smallest sum (µs) of `n` consecutive laps,
    /// or `None` if fewer than `n` laps were completed.
    BestConsecutiveMicros(#[ts(type = "number | null")] Option<i64>),
}

/// The scored heat: every competitor's [`Placement`], best position first.
///
/// Ties share a `position` (see [`Placement::position`]). The order within a tie
/// group is still deterministic — competitors are ordered by [`CompetitorKey`] as
/// the final, total tie-break — but their `position` numbers are equal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct HeatResult {
    /// Placements in finishing order (ties adjacent, sharing a position).
    pub places: Vec<Placement>,
}

/// A single completed lap, with both its absolute completion time and its duration.
#[derive(Debug, Clone, Copy)]
struct ScoredLap {
    /// When the completing pass crossed the lap gate (source clock).
    at: SourceTime,
    /// Lap duration in microseconds (`at - previous_pass.at`).
    duration_micros: i64,
}

/// One competitor's ordered completed laps, derived from their lap-gate passes.
struct Run {
    competitor: CompetitorKey,
    laps: Vec<ScoredLap>,
}

impl Run {
    /// Build per-competitor runs from lap-gate passes: group by `(adapter,
    /// competitor)`, order within a group, then each pass after the holeshot
    /// completes a lap.
    fn group(passes: &[Pass]) -> Vec<Run> {
        use std::collections::BTreeMap;

        // BTreeMap keeps competitors in deterministic key order regardless of
        // arrival order — the same total-order tie-break the projection uses.
        let mut by_competitor: BTreeMap<CompetitorKey, Vec<&Pass>> = BTreeMap::new();
        for pass in passes {
            if pass.gate.is_lap_gate() {
                by_competitor
                    .entry(CompetitorKey {
                        adapter: pass.adapter.clone(),
                        competitor: pass.competitor.clone(),
                    })
                    .or_default()
                    .push(pass);
            }
        }

        by_competitor
            .into_iter()
            .map(|(competitor, mut group)| {
                // Same ordering rule as `gridfpv_projection::lap_list`: sequenced
                // passes first (ascending sequence), then unsequenced, `at` last.
                group.sort_by_key(|p| (p.sequence.is_none(), p.sequence, p.at));
                let laps = group
                    .windows(2)
                    .map(|pair| ScoredLap {
                        at: pair[1].at,
                        duration_micros: pair[1].at.micros_since(pair[0].at),
                    })
                    .collect();
                Run { competitor, laps }
            })
            .collect()
    }
}

/// Score a heat from its lap-gate passes under `condition`.
///
/// `passes` is the heat's lap-gate [`Pass`]es (split passes are ignored; pass any
/// slice — unordered is fine, it is grouped and ordered internally). `race_start`
/// is the shared race clock origin used by [`WinCondition::Timed`]; the qualifying
/// and first-to-N conditions use absolute pass times directly and ignore it.
///
/// Returns a [`HeatResult`] whose `places` are in finishing order with ties sharing
/// a position. Called on a partial pass list this is the **provisional / live
/// ranking** (see the module docs).
pub fn score(passes: &[Pass], condition: WinCondition, race_start: SourceTime) -> HeatResult {
    let runs = Run::group(passes);
    match condition {
        WinCondition::Timed { window_micros } => score_timed(runs, race_start, window_micros),
        WinCondition::FirstToLaps { n } => score_first_to_laps(runs, n),
        WinCondition::BestLap => score_best_lap(runs),
        WinCondition::BestConsecutive { n } => score_best_consecutive(runs, n),
    }
}

/// Convenience wrapper over a canonical [`Event`] log: filters to lap-gate
/// [`Pass`]es and scores them, so callers holding a heat's event log (e.g. from the
/// mock-RH harness) need not pre-filter.
pub fn score_events(
    events: &[Event],
    condition: WinCondition,
    race_start: SourceTime,
) -> HeatResult {
    let passes: Vec<Pass> = events
        .iter()
        .filter_map(|e| match e {
            Event::Pass(p) if p.gate.is_lap_gate() => Some(p.clone()),
            _ => None,
        })
        .collect();
    score(&passes, condition, race_start)
}

/// Assemble a [`HeatResult`] from `(competitor, laps, metric, rank_key)` rows.
///
/// `rank_key` is a total, deterministic ordering key (smaller = better). Rows are
/// sorted by `(rank_key, competitor)` so the competitor key is the final tie-break
/// and the order is always total; competitors whose `rank_key` is *equal* (the part
/// the condition cares about) share a position, with the next distinct group's
/// position skipping past them (1, 2, 2, 4).
fn rank<K: Ord + Clone>(rows: Vec<(CompetitorKey, u32, Metric, K)>) -> HeatResult {
    let mut rows = rows;
    // Total order: rank key first, competitor key as the final deterministic
    // tie-break so two rows are never "equal" for sorting purposes.
    rows.sort_by(|a, b| a.3.cmp(&b.3).then_with(|| a.0.cmp(&b.0)));

    let mut places = Vec::with_capacity(rows.len());
    let mut prev_key: Option<K> = None;
    let mut position = 0u32;
    for (index, (competitor, laps, metric, key)) in rows.into_iter().enumerate() {
        // A new position whenever the *ranking* key changes; equal ranking keys
        // share the position of the first row in their group.
        if prev_key.as_ref() != Some(&key) {
            position = (index as u32) + 1;
            prev_key = Some(key.clone());
        }
        places.push(Placement {
            competitor,
            position,
            laps,
            metric,
        });
    }
    HeatResult { places }
}

/// Timed: count laps whose completing pass is strictly before the cutoff, rank by
/// count desc then earlier last-counted-lap completion.
fn score_timed(runs: Vec<Run>, race_start: SourceTime, window_micros: i64) -> HeatResult {
    let cutoff = race_start.micros + window_micros;
    let rows = runs
        .into_iter()
        .map(|run| {
            // HARD cutoff: strictly-before. A lap completing exactly at the cutoff
            // (or after) does not count — no finishing the in-progress lap.
            let counted: Vec<&ScoredLap> = run
                .laps
                .iter()
                .filter(|lap| lap.at.micros < cutoff)
                .collect();
            let count = counted.len() as u32;
            let last_at = counted.last().map(|lap| lap.at);
            // Rank key: fewer laps is worse (negate count so smaller = better), then
            // earlier last-lap completion is better. `i64::MAX` for "no lap" sorts a
            // lapless competitor behind everyone with a lap at the same (zero) count.
            let key = (
                -(count as i64),
                last_at.map(|t| t.micros).unwrap_or(i64::MAX),
            );
            (run.competitor, count, Metric::LastLapAt(last_at), key)
        })
        .collect();
    rank(rows)
}

/// First-to-N: rank by who reached lap `n` earliest; non-reachers after, by laps
/// desc then last-lap completion.
fn score_first_to_laps(runs: Vec<Run>, n: u32) -> HeatResult {
    let rows = runs
        .into_iter()
        .map(|run| {
            let count = run.laps.len() as u32;
            // `n` laps means the n-th completed lap (1-based) — index n-1.
            let reached_at = if n >= 1 && count >= n {
                Some(run.laps[(n - 1) as usize].at)
            } else {
                None
            };
            let last_at = run.laps.last().map(|lap| lap.at);
            // Reachers (group 0) sort ahead of non-reachers (group 1). Within
            // reachers, earlier reach-time wins. Within non-reachers, more laps then
            // earlier last-lap completion.
            let key = match reached_at {
                Some(t) => (0i8, t.micros, 0i64, 0i64),
                None => (
                    1i8,
                    0,
                    -(count as i64),
                    last_at.map(|t| t.micros).unwrap_or(i64::MAX),
                ),
            };
            (run.competitor, count, Metric::ReachedAt(reached_at), key)
        })
        .collect();
    rank(rows)
}

/// Best single lap: rank by smallest lap duration; ties break by when that lap was
/// set; no-lap competitors last.
fn score_best_lap(runs: Vec<Run>) -> HeatResult {
    let rows = runs
        .into_iter()
        .map(|run| {
            let count = run.laps.len() as u32;
            // Fastest lap = smallest duration; tie-break on the earlier-set one.
            let best = run
                .laps
                .iter()
                .min_by(|a, b| {
                    a.duration_micros
                        .cmp(&b.duration_micros)
                        .then(a.at.micros.cmp(&b.at.micros))
                })
                .copied();
            let best_micros = best.map(|lap| lap.duration_micros);
            // Smaller duration is better; `i64::MAX` parks no-lap competitors last.
            // Second key (set-time) makes equal-duration laps a total order.
            let key = (
                best_micros.unwrap_or(i64::MAX),
                best.map(|lap| lap.at.micros).unwrap_or(i64::MAX),
            );
            (
                run.competitor,
                count,
                Metric::BestLapMicros(best_micros),
                key,
            )
        })
        .collect();
    rank(rows)
}

/// Best consecutive `n`: rank by smallest sum over any `n` consecutive laps; ties
/// break by the completion time of the window's last lap; under-`n` competitors
/// last.
fn score_best_consecutive(runs: Vec<Run>, n: u32) -> HeatResult {
    let n = n.max(1) as usize;
    let rows = runs
        .into_iter()
        .map(|run| {
            let count = run.laps.len() as u32;
            // Slide an `n`-wide window over the laps; pick the smallest sum, tie-
            // broken by the earlier window-end (last lap's completion time).
            let best = run
                .laps
                .windows(n)
                .map(|w| {
                    let sum: i64 = w.iter().map(|lap| lap.duration_micros).sum();
                    let end_at = w[n - 1].at.micros;
                    (sum, end_at)
                })
                .min_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
            let best_sum = best.map(|(sum, _)| sum);
            // Smaller sum is better; no window (fewer than `n` laps) sorts last,
            // ordered among themselves by lap count desc.
            let key = match best {
                Some((sum, end_at)) => (0i8, sum, end_at, 0i64),
                None => (1i8, 0, 0, -(count as i64)),
            };
            (
                run.competitor,
                count,
                Metric::BestConsecutiveMicros(best_sum),
                key,
            )
        })
        .collect();
    rank(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{AdapterId, CompetitorRef, GateIndex};

    const ADAPTER: &str = "vd";

    /// A lap-gate pass for `competitor` at `at` µs (sequence keeps ordering tidy).
    fn pass(competitor: &str, at: i64, seq: u64) -> Pass {
        Pass {
            adapter: AdapterId(ADAPTER.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
        }
    }

    fn key(competitor: &str) -> CompetitorKey {
        CompetitorKey {
            adapter: AdapterId(ADAPTER.into()),
            competitor: CompetitorRef(competitor.into()),
        }
    }

    /// Build a competitor's whole run as consecutive passes from `at`s.
    fn run(competitor: &str, ats: &[i64]) -> Vec<Pass> {
        ats.iter()
            .enumerate()
            .map(|(i, &at)| pass(competitor, at, i as u64))
            .collect()
    }

    /// Look up a placement by competitor name.
    fn place<'a>(r: &'a HeatResult, competitor: &str) -> &'a Placement {
        r.places
            .iter()
            .find(|p| p.competitor == key(competitor))
            .unwrap_or_else(|| panic!("no placement for {competitor}"))
    }

    fn start() -> SourceTime {
        SourceTime::from_micros(0)
    }

    // --- Timed --------------------------------------------------------------

    #[test]
    fn timed_most_laps_in_window_wins() {
        // A: 4 passes ⇒ 3 laps, all well inside a 30s window.
        // B: 3 passes ⇒ 2 laps. A wins on lap count.
        let mut passes = run("A", &[1_000_000, 6_000_000, 12_000_000, 18_000_000]);
        passes.extend(run("B", &[2_000_000, 9_000_000, 16_000_000]));

        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 30_000_000,
            },
            start(),
        );

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "A").laps, 3);
        assert_eq!(place(&r, "B").position, 2);
        assert_eq!(place(&r, "B").laps, 2);
        assert_eq!(
            place(&r, "A").metric,
            Metric::LastLapAt(Some(SourceTime::from_micros(18_000_000)))
        );
    }

    #[test]
    fn timed_hard_cutoff_excludes_lap_completing_at_or_after_window() {
        // Window = 10s from start 0, so cutoff = 10_000_000.
        // A completes its 2nd lap exactly AT the cutoff (10_000_000) — excluded.
        // A's 1st lap completes at 5_000_000 — counts. So A has 1 counted lap.
        // B completes both laps strictly before (4s, 9s) — 2 counted laps. B wins.
        let mut passes = run("A", &[0, 5_000_000, 10_000_000]);
        passes.extend(run("B", &[0, 4_000_000, 9_000_000]));

        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 10_000_000,
            },
            start(),
        );

        // Hard cutoff: A's lap at exactly 10_000_000 does NOT count.
        assert_eq!(place(&r, "A").laps, 1);
        assert_eq!(
            place(&r, "A").metric,
            Metric::LastLapAt(Some(SourceTime::from_micros(5_000_000)))
        );
        assert_eq!(place(&r, "B").laps, 2);
        assert_eq!(place(&r, "B").position, 1);
        assert_eq!(place(&r, "A").position, 2);
    }

    #[test]
    fn timed_just_inside_cutoff_counts() {
        // Same as above but A's 2nd lap completes one µs BEFORE the cutoff: it counts,
        // and now A (2 laps, last at 9_999_999) beats B (2 laps, last at 9_000_000)?
        // No — equal lap count, tie-break is EARLIER last-lap completion, so B (9.0s)
        // ranks ahead of A (9.999999s).
        let mut passes = run("A", &[0, 5_000_000, 9_999_999]);
        passes.extend(run("B", &[0, 4_000_000, 9_000_000]));

        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 10_000_000,
            },
            start(),
        );

        assert_eq!(place(&r, "A").laps, 2);
        assert_eq!(place(&r, "B").laps, 2);
        assert_eq!(place(&r, "B").position, 1); // earlier last lap
        assert_eq!(place(&r, "A").position, 2);
    }

    #[test]
    fn timed_respects_nonzero_race_start() {
        // Race starts at 100s; window 10s ⇒ cutoff 110s. A lap completing at 109.9s
        // counts; one at 110s does not.
        let passes = run("A", &[100_000_000, 105_000_000, 109_900_000, 110_000_000]);
        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 10_000_000,
            },
            SourceTime::from_micros(100_000_000),
        );
        // Laps complete at 105 (count), 109.9 (count), 110 (excluded) ⇒ 2 counted.
        assert_eq!(place(&r, "A").laps, 2);
        assert_eq!(
            place(&r, "A").metric,
            Metric::LastLapAt(Some(SourceTime::from_micros(109_900_000)))
        );
    }

    // --- FirstToLaps --------------------------------------------------------

    #[test]
    fn first_to_laps_early_finisher_wins_despite_fewer_total_laps() {
        // Target: 3 laps.
        // A reaches lap 3 at 9_000_000, then stops (3 laps total).
        // B reaches lap 3 at 10_000_000 but keeps going to 5 laps total.
        // First-to-N: A wins — it banked lap 3 first, even though B flew more laps.
        let mut passes = run("A", &[0, 3_000_000, 6_000_000, 9_000_000]);
        passes.extend(run(
            "B",
            &[0, 4_000_000, 7_000_000, 10_000_000, 12_000_000, 14_000_000],
        ));

        let r = score(&passes, WinCondition::FirstToLaps { n: 3 }, start());

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(
            place(&r, "A").metric,
            Metric::ReachedAt(Some(SourceTime::from_micros(9_000_000)))
        );
        assert_eq!(place(&r, "B").position, 2);
        assert_eq!(place(&r, "B").laps, 5);
    }

    #[test]
    fn first_to_laps_non_reachers_rank_after_by_laps() {
        // Target 5 laps; neither C nor D reaches it.
        // A reaches 5. C has 3 laps, D has 2 laps. Order: A, then C (more laps), D.
        let mut passes = run(
            "A",
            &[0, 1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000],
        );
        passes.extend(run("C", &[0, 2_000_000, 4_000_000, 6_000_000]));
        passes.extend(run("D", &[0, 3_000_000, 7_000_000]));

        let r = score(&passes, WinCondition::FirstToLaps { n: 5 }, start());

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "C").position, 2);
        assert_eq!(place(&r, "C").laps, 3);
        assert_eq!(place(&r, "D").position, 3);
        assert_eq!(place(&r, "D").laps, 2);
        assert_eq!(place(&r, "C").metric, Metric::ReachedAt(None));
    }

    // --- BestLap ------------------------------------------------------------

    #[test]
    fn best_lap_fastest_single_lap_wins() {
        // A's laps: 3s, 2s, 4s ⇒ best 2s. B's: 2.5s, 2.2s ⇒ best 2.2s. A wins.
        let mut passes = run("A", &[0, 3_000_000, 5_000_000, 9_000_000]);
        passes.extend(run("B", &[0, 2_500_000, 4_700_000]));

        let r = score(&passes, WinCondition::BestLap, start());

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(
            place(&r, "A").metric,
            Metric::BestLapMicros(Some(2_000_000))
        );
        assert_eq!(place(&r, "B").position, 2);
        assert_eq!(
            place(&r, "B").metric,
            Metric::BestLapMicros(Some(2_200_000))
        );
    }

    #[test]
    fn best_lap_no_lap_competitor_ranks_last() {
        // A has a lap; Z has a single pass (no completed lap).
        let mut passes = run("A", &[0, 3_000_000]);
        passes.extend(run("Z", &[1_000_000]));

        let r = score(&passes, WinCondition::BestLap, start());

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "Z").position, 2);
        assert_eq!(place(&r, "Z").metric, Metric::BestLapMicros(None));
        assert_eq!(place(&r, "Z").laps, 0);
    }

    // --- BestConsecutive ----------------------------------------------------

    #[test]
    fn best_consecutive_fastest_sum_wins() {
        // n = 2.
        // A laps: 3s, 2s, 2s, 5s. Consecutive-2 sums: 5, 4, 7 ⇒ best 4s.
        // B laps: 2.4s, 2.4s, 2.4s. Sums: 4.8, 4.8 ⇒ best 4.8s. A wins (4 < 4.8).
        let mut passes = run("A", &[0, 3_000_000, 5_000_000, 7_000_000, 12_000_000]);
        passes.extend(run("B", &[0, 2_400_000, 4_800_000, 7_200_000]));

        let r = score(&passes, WinCondition::BestConsecutive { n: 2 }, start());

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(
            place(&r, "A").metric,
            Metric::BestConsecutiveMicros(Some(4_000_000))
        );
        assert_eq!(place(&r, "B").position, 2);
        assert_eq!(
            place(&r, "B").metric,
            Metric::BestConsecutiveMicros(Some(4_800_000))
        );
    }

    #[test]
    fn best_consecutive_under_n_laps_ranks_last() {
        // n = 3. A has 3 laps (a window); S has only 2 laps (no window) ⇒ S last.
        let mut passes = run("A", &[0, 2_000_000, 4_000_000, 6_000_000]);
        passes.extend(run("S", &[0, 2_000_000, 3_500_000]));

        let r = score(&passes, WinCondition::BestConsecutive { n: 3 }, start());

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(
            place(&r, "A").metric,
            Metric::BestConsecutiveMicros(Some(6_000_000))
        );
        assert_eq!(place(&r, "S").position, 2);
        assert_eq!(place(&r, "S").metric, Metric::BestConsecutiveMicros(None));
    }

    // --- Multi-competitor ordering & ties ----------------------------------

    #[test]
    fn multi_competitor_full_ordering() {
        // Timed, 30s. Lap counts: A=4, B=3, C=2 ⇒ positions 1, 2, 3.
        let mut passes = run("A", &[0, 5_000_000, 10_000_000, 15_000_000, 20_000_000]);
        passes.extend(run("B", &[0, 6_000_000, 13_000_000, 21_000_000]));
        passes.extend(run("C", &[0, 7_000_000, 16_000_000]));

        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 30_000_000,
            },
            start(),
        );

        assert_eq!(r.places.len(), 3);
        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "A").laps, 4);
        assert_eq!(place(&r, "B").position, 2);
        assert_eq!(place(&r, "B").laps, 3);
        assert_eq!(place(&r, "C").position, 3);
        assert_eq!(place(&r, "C").laps, 2);
    }

    #[test]
    fn genuine_tie_shares_a_position() {
        // Two competitors, identical lap count AND identical last-lap completion
        // time ⇒ genuine tie: they share position 1, and the next competitor skips
        // to position 3 (1, 1, 3 competition ranking).
        let mut passes = run("A", &[0, 5_000_000, 10_000_000]);
        passes.extend(run("B", &[0, 6_000_000, 10_000_000])); // same last-lap time
        passes.extend(run("C", &[0, 8_000_000]));

        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );

        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "B").position, 1);
        assert_eq!(place(&r, "C").position, 3);
        // The tie group is ordered by competitor key as the final deterministic
        // tie-break: A before B.
        let names: Vec<&CompetitorRef> =
            r.places.iter().map(|p| &p.competitor.competitor).collect();
        assert_eq!(names[0].0, "A");
        assert_eq!(names[1].0, "B");
    }

    // --- Provisional / live ranking ----------------------------------------

    #[test]
    fn provisional_ranking_mid_heat() {
        // The very same `score` on a partial pass list gives the current standing.
        // Mid-heat: B is ahead (2 laps) of A (1 lap).
        let mut partial = run("A", &[0, 5_000_000]); // 1 lap so far
        partial.extend(run("B", &[0, 4_000_000, 8_000_000])); // 2 laps so far

        let live = score(
            &partial,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );
        assert_eq!(place(&live, "B").position, 1);
        assert_eq!(place(&live, "A").position, 2);

        // Later A surges to 4 laps and takes the lead — same function, more passes.
        let mut full = run("A", &[0, 5_000_000, 9_000_000, 12_000_000, 15_000_000]);
        full.extend(run("B", &[0, 4_000_000, 8_000_000]));
        let final_r = score(
            &full,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );
        assert_eq!(place(&final_r, "A").position, 1);
        assert_eq!(place(&final_r, "B").position, 2);
    }

    // --- score_events wrapper & non-lap-gate filtering ----------------------

    #[test]
    fn score_events_filters_splits_and_lifecycle() {
        let mut events: Vec<Event> = vec![
            Event::AdapterConnected {
                adapter: AdapterId(ADAPTER.into()),
            },
            // A split pass (gate 2) must be ignored by the scorer.
            Event::Pass(Pass {
                gate: GateIndex(2),
                ..pass("A", 2_500_000, 99)
            }),
        ];
        events.extend(
            run("A", &[0, 3_000_000, 6_000_000])
                .into_iter()
                .map(Event::Pass),
        );

        let r = score_events(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );
        // Only the two lap-gate laps count; the split did not add a lap.
        assert_eq!(place(&r, "A").laps, 2);
    }

    #[test]
    fn passes_grouped_by_adapter_and_competitor() {
        // Same competitor ref on two adapters is two distinct runs.
        let mut passes: Vec<Pass> = run("node-0", &[0, 3_000_000]);
        let other: Vec<Pass> = run("node-0", &[0, 2_000_000, 4_000_000])
            .into_iter()
            .map(|mut p| {
                p.adapter = AdapterId("rh".into());
                p
            })
            .collect();
        passes.extend(other);

        let r = score(&passes, WinCondition::BestLap, start());
        assert_eq!(r.places.len(), 2);
    }
}
