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

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{CompetitorRef, Event, Pass, Penalty, SourceTime};
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
    /// Whether this competitor was **disqualified** by an adjudication
    /// ([`gridfpv_events::Penalty::Disqualify`] via
    /// [`gridfpv_events::Event::PenaltyApplied`]). A disqualified competitor is ranked
    /// **after every non-disqualified competitor**, regardless of their on-track result
    /// (see [`score_with_adjudications`]). Defaults to `false` and is omitted from the
    /// wire when false, so clean results carry no extra bytes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disqualified: bool,
}

/// serde `skip_serializing_if` predicate: omit additive `bool` flags when false so a
/// clean result serialises exactly as it did before these fields existed.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for Placement {
    /// An empty placeholder placement. Exists so constructors (chiefly test fixtures)
    /// can spread `..Default::default()` and only set the fields they care about, which
    /// keeps additive [`Placement`] fields from rippling into every struct-literal again
    /// (a later field defaults rather than breaking the build). `CompetitorKey` has no
    /// `Default` of its own, so this supplies an empty one; callers always overwrite it.
    fn default() -> Self {
        Placement {
            competitor: CompetitorKey {
                adapter: gridfpv_events::AdapterId(String::new()),
                competitor: CompetitorRef(String::new()),
            },
            position: 0,
            laps: 0,
            metric: Metric::LastLapAt(None),
            disqualified: false,
        }
    }
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct HeatResult {
    /// Placements in finishing order (ties adjacent, sharing a position).
    pub places: Vec<Placement>,
    /// Whether the whole heat was **voided** by an adjudication
    /// ([`gridfpv_events::Event::HeatVoided`]). A voided result is nullified: its
    /// `places` are still scored (so the on-track standing is visible) but the heat does
    /// not count. Defaults to `false` and is omitted from the wire when false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub voided: bool,
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

/// Whether a heat's **race-end criterion is met** under `condition`, given its lap-gate
/// `passes` and the shared `race_start` (heat-lifecycle Slice 2).
///
/// This is the pure predicate the Director's **runtime completion clock** evaluates each poll to
/// decide whether to begin the grace window and auto-append the `Running → Unofficial` transition.
/// Like [`score`], it reads **no clock and no RNG** — it derives the answer entirely from the
/// logged passes + the race start — so a replay reaches completion at the exact same point.
///
/// The criterion per condition:
/// - [`WinCondition::Timed`]: met once a counted crossing lands **at or after** the window close
///   (`race_start + window_micros`). A pass at/after the cutoff is the observable signal that the
///   window has elapsed on the source clock; until one lands the window is still open.
/// - [`WinCondition::FirstToLaps`]: met once **any** competitor has completed `n` laps (the leader
///   reached the target).
/// - [`WinCondition::BestLap`] / [`WinCondition::BestConsecutive`] (qualifying): there is no
///   lap/leader criterion intrinsic to the passes — a qual session ends on its **time window**,
///   which these conditions do not carry. This predicate returns `false` for them; such rounds end
///   via the RD's [`ForceEnd`](crate::heat::HeatCommand::ForceEnd) override (or a Timed-bounded
///   qual). This keeps the function total and pure without inventing a window the condition lacks.
///
/// `passes` may be the partial mid-heat list (the runtime calls it on the running passes); it is
/// grouped/ordered internally exactly as [`score`] does.
pub fn race_end_reached(passes: &[Pass], condition: WinCondition, race_start: SourceTime) -> bool {
    match condition {
        WinCondition::Timed { window_micros } => {
            let cutoff = race_start.micros + window_micros;
            passes
                .iter()
                .any(|p| p.gate.is_lap_gate() && p.at.micros >= cutoff)
        }
        WinCondition::FirstToLaps { n } => {
            if n == 0 {
                return true;
            }
            // A competitor reaches `n` laps after `n + 1` lap-gate crossings (the holeshot opens
            // the count). Reuse the scorer's grouping so the lap model matches the ranking exactly.
            Run::group(passes)
                .iter()
                .any(|run| run.laps.len() as u32 >= n)
        }
        // Qualifying conditions carry no intrinsic end criterion — see the doc above.
        WinCondition::BestLap | WinCondition::BestConsecutive { .. } => false,
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
    score_inner(passes, condition, race_start, &Adjudications::default())
}

/// The adjudications a heat's [`Event::PenaltyApplied`] / [`Event::HeatVoided`] log
/// distils to, applied on top of the pure on-track scoring (race-engine.html §7.1, #13).
///
/// Built by [`Adjudications::collect`] from a heat's events; pure data, so the same log
/// always yields the same adjudications and the scored result replays identically (no
/// clock or RNG). Penalties are keyed by [`CompetitorRef`] because that is what the
/// penalty events carry; a heat's events are all one heat, so the ref is unambiguous.
#[derive(Debug, Clone, Default)]
struct Adjudications {
    /// Per-competitor microseconds added to their deciding time, **accumulated** across
    /// every [`Penalty::TimeAdded`] for that competitor (multiple penalties stack).
    time_added: BTreeMap<CompetitorRef, i64>,
    /// Competitors disqualified by a [`Penalty::Disqualify`] — ranked after everyone not
    /// disqualified and flagged [`Placement::disqualified`].
    disqualified: BTreeSet<CompetitorRef>,
    /// Whether the whole heat was voided ([`Event::HeatVoided`]).
    voided: bool,
}

impl Adjudications {
    /// Distil a heat's event log into its adjudications. Ignores everything that is not a
    /// [`Event::PenaltyApplied`] / [`Event::HeatVoided`]; `TimeAdded` penalties accumulate,
    /// any `Disqualify` disqualifies, any `HeatVoided` voids. Deterministic — pure fold.
    fn collect(events: &[Event]) -> Self {
        let mut adj = Adjudications::default();
        for event in events {
            match event {
                Event::PenaltyApplied {
                    competitor,
                    penalty,
                    ..
                } => match penalty {
                    Penalty::Disqualify => {
                        adj.disqualified.insert(competitor.clone());
                    }
                    Penalty::TimeAdded { micros } => {
                        *adj.time_added.entry(competitor.clone()).or_default() += *micros;
                    }
                },
                Event::HeatVoided { .. } => adj.voided = true,
                _ => {}
            }
        }
        adj
    }

    /// Microseconds to add to `competitor`'s deciding time (0 if none).
    fn added(&self, competitor: &CompetitorRef) -> i64 {
        self.time_added.get(competitor).copied().unwrap_or_default()
    }

    /// Whether `competitor` was disqualified.
    fn is_dq(&self, competitor: &CompetitorRef) -> bool {
        self.disqualified.contains(competitor)
    }
}

/// Score `passes` under `condition`, then apply `adj`'s adjudications: [`Penalty::TimeAdded`]
/// worsens the deciding time used to rank a competitor, [`Penalty::Disqualify`] sinks a
/// competitor below every non-disqualified one (flagging [`Placement::disqualified`]), and
/// [`Event::HeatVoided`] flags the whole [`HeatResult`] voided.
fn score_inner(
    passes: &[Pass],
    condition: WinCondition,
    race_start: SourceTime,
    adj: &Adjudications,
) -> HeatResult {
    let runs = Run::group(passes);
    let mut result = match condition {
        WinCondition::Timed { window_micros } => score_timed(runs, race_start, window_micros, adj),
        WinCondition::FirstToLaps { n } => score_first_to_laps(runs, n, adj),
        WinCondition::BestLap => score_best_lap(runs, adj),
        WinCondition::BestConsecutive { n } => score_best_consecutive(runs, n, adj),
    };
    result.voided = adj.voided;
    result
}

/// Score a heat's event log under `condition`, applying its **adjudications**
/// ([`Event::PenaltyApplied`] / [`Event::HeatVoided`], #13).
///
/// This is the single home of penalty / heat-void application. It scores the raw
/// lap-gate passes the log carries, then folds in the heat's adjudications. Marshaling
/// corrections (void/insert/adjust) are a *separate* fold ([`crate::event::score_marshaled`]);
/// that path calls [`apply_adjudications`] on the corrected stream so penalties and
/// marshaling compose without either fold knowing about the other.
pub fn score_with_adjudications(
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
    score_inner(
        &passes,
        condition,
        race_start,
        &Adjudications::collect(events),
    )
}

/// Score already-grouped/corrected `passes`, applying the adjudications carried by `events`.
///
/// Used by the marshaling-aware path ([`crate::event::score_marshaled`]): it has already
/// folded void/insert/adjust into a corrected pass stream, so here we only re-derive the
/// adjudications from the same log and apply them — penalties and heat-void compose with
/// marshaling without either fold knowing about the other.
pub(crate) fn apply_adjudications(
    passes: &[Pass],
    condition: WinCondition,
    race_start: SourceTime,
    events: &[Event],
) -> HeatResult {
    score_inner(
        passes,
        condition,
        race_start,
        &Adjudications::collect(events),
    )
}

/// Convenience wrapper over a canonical [`Event`] log: filters to lap-gate [`Pass`]es,
/// scores them, and applies any adjudications the log carries
/// ([`Event::PenaltyApplied`] / [`Event::HeatVoided`]) — so callers holding a heat's
/// event log (e.g. from the mock-RH harness) get the fully-adjudicated result without
/// pre-filtering. A log with no penalties scores exactly as [`score`] does.
pub fn score_events(
    events: &[Event],
    condition: WinCondition,
    race_start: SourceTime,
) -> HeatResult {
    score_with_adjudications(events, condition, race_start)
}

/// Assemble a [`HeatResult`] from `(competitor, laps, metric, rank_key)` rows, applying
/// `adj`'s disqualifications.
///
/// `rank_key` is a total, deterministic ordering key (smaller = better; any
/// [`Penalty::TimeAdded`] is already folded into it by the per-condition scorer). Rows
/// are sorted by `(disqualified, rank_key, competitor)`: **disqualified competitors sink
/// below every non-disqualified one** regardless of on-track result, then within each
/// group the rank key orders and the competitor key is the final deterministic tie-break.
/// Competitors whose `(disqualified, rank_key)` is *equal* share a position, with the next
/// distinct group's position skipping past them (1, 2, 2, 4). DQ'd placements carry
/// [`Placement::disqualified`] `= true`.
fn rank<K: Ord + Clone>(
    rows: Vec<(CompetitorKey, u32, Metric, K)>,
    adj: &Adjudications,
) -> HeatResult {
    // Pair each row with its DQ flag; the flag is the *primary* sort key (false < true),
    // so every disqualified competitor ranks after every non-disqualified one.
    let mut rows: Vec<(bool, CompetitorKey, u32, Metric, K)> = rows
        .into_iter()
        .map(|(competitor, laps, metric, key)| {
            (
                adj.is_dq(&competitor.competitor),
                competitor,
                laps,
                metric,
                key,
            )
        })
        .collect();
    // Total order: DQ first, then rank key, then competitor key as the final
    // deterministic tie-break so two rows are never "equal" for sorting purposes.
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.4.cmp(&b.4))
            .then_with(|| a.1.cmp(&b.1))
    });

    let mut places = Vec::with_capacity(rows.len());
    // A position groups by the *ranking* identity that competitors share: the DQ flag
    // plus the rank key. A DQ'd competitor never shares a position with a non-DQ'd one.
    let mut prev_group: Option<(bool, K)> = None;
    let mut position = 0u32;
    for (index, (disqualified, competitor, laps, metric, key)) in rows.into_iter().enumerate() {
        let group = (disqualified, key);
        if prev_group.as_ref() != Some(&group) {
            position = (index as u32) + 1;
            prev_group = Some(group);
        }
        places.push(Placement {
            competitor,
            position,
            laps,
            metric,
            disqualified,
        });
    }
    HeatResult {
        places,
        voided: false,
    }
}

/// Timed: count laps whose completing pass is strictly before the cutoff, rank by
/// count desc then earlier last-counted-lap completion.
///
/// `TimeAdded` here is a **pure lap-count** condition, so the penalty cannot change the
/// lap count; per the recorded rule it is folded into the **tie-break time** (the last
/// counted lap's completion), worsening a penalised competitor's standing against others
/// on the same lap count without inventing or removing laps.
fn score_timed(
    runs: Vec<Run>,
    race_start: SourceTime,
    window_micros: i64,
    adj: &Adjudications,
) -> HeatResult {
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
            let added = adj.added(&run.competitor.competitor);
            // Rank key: fewer laps is worse (negate count so smaller = better), then
            // earlier last-lap completion is better, with any TimeAdded worsening it.
            // `i64::MAX` for "no lap" sorts a lapless competitor behind everyone with a
            // lap at the same (zero) count.
            let key = (
                -(count as i64),
                last_at
                    .map(|t| t.micros.saturating_add(added))
                    .unwrap_or(i64::MAX),
            );
            (run.competitor, count, Metric::LastLapAt(last_at), key)
        })
        .collect();
    rank(rows, adj)
}

/// First-to-N: rank by who reached lap `n` earliest; non-reachers after, by laps
/// desc then last-lap completion.
///
/// `TimeAdded` worsens the **deciding time**: a reacher's reach-time and a non-reacher's
/// last-lap tie-break time both shift later by the accumulated penalty.
fn score_first_to_laps(runs: Vec<Run>, n: u32, adj: &Adjudications) -> HeatResult {
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
            let added = adj.added(&run.competitor.competitor);
            // Reachers (group 0) sort ahead of non-reachers (group 1). Within
            // reachers, earlier (penalty-worsened) reach-time wins. Within non-reachers,
            // more laps then earlier (penalty-worsened) last-lap completion.
            let key = match reached_at {
                Some(t) => (0i8, t.micros.saturating_add(added), 0i64, 0i64),
                None => (
                    1i8,
                    0,
                    -(count as i64),
                    last_at
                        .map(|t| t.micros.saturating_add(added))
                        .unwrap_or(i64::MAX),
                ),
            };
            (run.competitor, count, Metric::ReachedAt(reached_at), key)
        })
        .collect();
    rank(rows, adj)
}

/// Best single lap: rank by smallest lap duration; ties break by when that lap was
/// set; no-lap competitors last.
///
/// `TimeAdded` worsens the **deciding time** by lengthening the best-lap duration the
/// competitor is ranked on (the on-track `metric` is left unchanged for display).
fn score_best_lap(runs: Vec<Run>, adj: &Adjudications) -> HeatResult {
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
            let added = adj.added(&run.competitor.competitor);
            // Smaller (penalty-lengthened) duration is better; `i64::MAX` parks no-lap
            // competitors last. Second key (set-time) makes equal-duration laps a total
            // order.
            let key = (
                best_micros
                    .map(|d| d.saturating_add(added))
                    .unwrap_or(i64::MAX),
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
    rank(rows, adj)
}

/// Best consecutive `n`: rank by smallest sum over any `n` consecutive laps; ties
/// break by the completion time of the window's last lap; under-`n` competitors
/// last.
///
/// `TimeAdded` worsens the **deciding time** by adding to the best window's sum the
/// competitor is ranked on (the on-track `metric` is left unchanged for display).
fn score_best_consecutive(runs: Vec<Run>, n: u32, adj: &Adjudications) -> HeatResult {
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
            let added = adj.added(&run.competitor.competitor);
            // Smaller (penalty-lengthened) sum is better; no window (fewer than `n` laps)
            // sorts last, ordered among themselves by lap count desc.
            let key = match best {
                Some((sum, end_at)) => (0i8, sum.saturating_add(added), end_at, 0i64),
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
    rank(rows, adj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{AdapterId, CompetitorRef, GateIndex, HeatId};

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

    // --- race_end_reached (heat-lifecycle Slice 2 completion predicate) -----

    #[test]
    fn timed_race_end_reached_when_a_pass_lands_at_or_after_the_cutoff() {
        let cond = WinCondition::Timed {
            window_micros: 10_000_000,
        };
        // All passes strictly before the cutoff (10s): not yet reached.
        let early = run("A", &[0, 5_000_000, 9_000_000]);
        assert!(!race_end_reached(&early, cond, start()));
        // A pass exactly at the cutoff signals the window has elapsed: reached.
        let at_cutoff = run("A", &[0, 5_000_000, 10_000_000]);
        assert!(race_end_reached(&at_cutoff, cond, start()));
        // …and well after, too.
        let after = run("A", &[0, 5_000_000, 12_000_000]);
        assert!(race_end_reached(&after, cond, start()));
    }

    #[test]
    fn first_to_laps_race_end_reached_when_leader_hits_n() {
        let cond = WinCondition::FirstToLaps { n: 3 };
        // 3 crossings ⇒ 2 laps: not yet.
        let two = run("A", &[0, 3_000_000, 6_000_000]);
        assert!(!race_end_reached(&two, cond, start()));
        // 4 crossings ⇒ 3 laps: the leader reached the target.
        let three = run("A", &[0, 3_000_000, 6_000_000, 9_000_000]);
        assert!(race_end_reached(&three, cond, start()));
    }

    #[test]
    fn qualifying_conditions_have_no_intrinsic_race_end() {
        // BestLap / BestConsecutive end on a time window the condition does not carry, so the
        // predicate is always false (the RD ForceEnds, or a Timed-bounded qual is used).
        let passes = run("A", &[0, 2_000_000, 4_000_000, 6_000_000]);
        assert!(!race_end_reached(&passes, WinCondition::BestLap, start()));
        assert!(!race_end_reached(
            &passes,
            WinCondition::BestConsecutive { n: 2 },
            start()
        ));
    }

    // --- Adjudications: penalties & heat-void (#13) -------------------------

    /// The lap-gate passes of a whole run, wrapped as `Event::Pass`es.
    fn pass_events(competitor: &str, ats: &[i64]) -> Vec<Event> {
        run(competitor, ats).into_iter().map(Event::Pass).collect()
    }

    /// A `PenaltyApplied` for `competitor` in a fixed heat.
    fn penalty(competitor: &str, penalty: Penalty) -> Event {
        Event::PenaltyApplied {
            heat: HeatId("h".into()),
            competitor: CompetitorRef(competitor.into()),
            penalty,
        }
    }

    #[test]
    fn clean_log_score_events_matches_pure_score() {
        // No adjudications: the adjudicated path equals the pure scorer exactly, and
        // the additive flags are all false.
        let events = pass_events("A", &[0, 2_000_000, 4_000_000]);
        let cond = WinCondition::BestLap;
        let r = score_events(&events, cond, start());
        let pure = score(&run("A", &[0, 2_000_000, 4_000_000]), cond, start());
        assert_eq!(r, pure);
        assert!(!r.voided);
        assert!(r.places.iter().all(|p| !p.disqualified));
    }

    #[test]
    fn disqualify_drops_leader_to_last_and_shifts_others_up() {
        // Timed: A leads (3 laps), B (2), C (1) → A,B,C. DQ A: A sinks to last, B and C
        // shift up, and A is flagged disqualified.
        let mut events = pass_events("A", &[0, 5_000_000, 10_000_000, 15_000_000]);
        events.extend(pass_events("B", &[0, 6_000_000, 13_000_000]));
        events.extend(pass_events("C", &[0, 7_000_000]));
        events.push(penalty("A", Penalty::Disqualify));

        let r = score_events(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );

        assert_eq!(place(&r, "B").position, 1);
        assert_eq!(place(&r, "C").position, 2);
        assert_eq!(place(&r, "A").position, 3);
        assert!(place(&r, "A").disqualified);
        assert!(!place(&r, "B").disqualified);
        assert!(!place(&r, "C").disqualified);
        // The DQ does not erase A's on-track laps in the metric — only the ranking moves.
        assert_eq!(place(&r, "A").laps, 3);
        assert!(!r.voided);
    }

    #[test]
    fn disqualify_two_leaders_both_sink_below_the_field() {
        // DQ both A (3 laps) and B (2): C (1 lap) is now first; the two DQ'd competitors
        // rank behind it, ordered among themselves by their on-track standing then key.
        let mut events = pass_events("A", &[0, 5_000_000, 10_000_000, 15_000_000]);
        events.extend(pass_events("B", &[0, 6_000_000, 13_000_000]));
        events.extend(pass_events("C", &[0, 7_000_000]));
        events.push(penalty("A", Penalty::Disqualify));
        events.push(penalty("B", Penalty::Disqualify));

        let r = score_events(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );

        assert_eq!(place(&r, "C").position, 1);
        assert!(!place(&r, "C").disqualified);
        // A (3 laps) still beats B (2 laps) *within* the disqualified group.
        assert_eq!(place(&r, "A").position, 2);
        assert_eq!(place(&r, "B").position, 3);
        assert!(place(&r, "A").disqualified);
        assert!(place(&r, "B").disqualified);
    }

    #[test]
    fn time_added_reorders_a_best_lap_result() {
        // BestLap: A's best lap is 2.0s, B's is 2.2s → A first. Add 0.5s to A's deciding
        // time (2.0 → 2.5) and now B (2.2) wins; A drops to 2nd. The on-track metric is
        // unchanged (still 2.0s for A) — only the ranking reflects the penalty.
        let mut events = pass_events("A", &[0, 3_000_000, 5_000_000, 9_000_000]);
        events.extend(pass_events("B", &[0, 2_500_000, 4_700_000]));
        events.push(penalty("A", Penalty::TimeAdded { micros: 500_000 }));

        let r = score_events(&events, WinCondition::BestLap, start());

        assert_eq!(place(&r, "B").position, 1);
        assert_eq!(place(&r, "A").position, 2);
        assert_eq!(
            place(&r, "A").metric,
            Metric::BestLapMicros(Some(2_000_000))
        );
    }

    #[test]
    fn time_added_reorders_a_timed_tiebreak() {
        // Timed, equal lap count (2 each). Without penalty B (last lap 9.0s) beats A
        // (last lap 9.5s). Add 1.0s to B's deciding time (9.0 → 10.0): A (9.5) now wins.
        let mut events = pass_events("A", &[0, 5_000_000, 9_500_000]);
        events.extend(pass_events("B", &[0, 4_000_000, 9_000_000]));
        events.push(penalty("B", Penalty::TimeAdded { micros: 1_000_000 }));

        let r = score_events(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );

        assert_eq!(place(&r, "A").laps, 2);
        assert_eq!(place(&r, "B").laps, 2);
        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "B").position, 2);
    }

    #[test]
    fn time_added_accumulates_across_penalties() {
        // Two +1.0s penalties on B stack to +2.0s. BestLap: A 2.0s, B 1.5s. B's deciding
        // time becomes 1.5 + 2.0 = 3.5s, so A (2.0) wins despite B's faster raw lap.
        let mut events = pass_events("A", &[0, 2_000_000]);
        events.extend(pass_events("B", &[0, 1_500_000]));
        events.push(penalty("B", Penalty::TimeAdded { micros: 1_000_000 }));
        events.push(penalty("B", Penalty::TimeAdded { micros: 1_000_000 }));

        let r = score_events(&events, WinCondition::BestLap, start());
        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "B").position, 2);
    }

    #[test]
    fn heat_voided_flags_the_result() {
        // A clean 2-competitor heat, then a HeatVoided: the result is flagged voided but
        // its on-track places are still scored (the standing remains visible).
        let mut events = pass_events("A", &[0, 5_000_000, 10_000_000]);
        events.extend(pass_events("B", &[0, 6_000_000]));
        events.push(Event::HeatVoided {
            heat: HeatId("h".into()),
        });

        let r = score_events(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );

        assert!(r.voided);
        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "B").position, 2);
    }

    #[test]
    fn penalty_and_marshaling_compose() {
        // A's middle pass is a phantom voided by marshaling (A: 2 laps → 1), and B is
        // disqualified. Score through the adjudicated wrapper over the *corrected* stream
        // (here via crate::event::score_marshaled). Expect A first (its sole remaining
        // lap), B disqualified to last.
        use crate::event::score_marshaled;
        use gridfpv_events::LogRef;

        let mut events: Vec<Event> = Vec::new();
        events.push(Event::Pass(pass("A", 0, 0))); // offset 0
        events.push(Event::Pass(pass("A", 2_000_000, 1))); // offset 1 — phantom
        events.push(Event::Pass(pass("A", 6_000_000, 2))); // offset 2
        events.extend(pass_events("B", &[0, 4_000_000, 8_000_000])); // B: 2 laps
        events.push(Event::DetectionVoided { target: LogRef(1) });
        events.push(penalty("B", Penalty::Disqualify));

        let r = score_marshaled(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );

        // Marshaling collapsed A to a single lap (0 → 6.0s).
        assert_eq!(place(&r, "A").laps, 1);
        // B disqualified despite 2 on-track laps: ranked last and flagged.
        assert_eq!(place(&r, "A").position, 1);
        assert_eq!(place(&r, "B").position, 2);
        assert!(place(&r, "B").disqualified);
    }
}
