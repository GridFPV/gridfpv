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
    /// The competitor's **fastest single completed lap** in this heat, in microseconds, or
    /// `None` if they completed no timed lap. Computed from the run's lap durations
    /// **independently of the win condition** (so e.g. a [`WinCondition::FirstToLaps`]
    /// placement still carries a real best-lap, not the win metric), with thrown-out laps
    /// excluded. Cross-heat rankings use it to break ties — round-robin breaks pilots equal
    /// on points by their faster best lap. Defaults to `None` so older snapshots that
    /// predate the field still deserialise.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub best_lap_micros: Option<i64>,
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
            best_lap_micros: None,
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
    /// The append offset of this lap's **end pass** — its stable identity, and the offset a
    /// [`Event::LapThrownOut`](gridfpv_events::Event::LapThrownOut) targets to exclude it.
    end_ref: u64,
}

/// One competitor's ordered completed laps, derived from their lap-gate passes.
struct Run {
    competitor: CompetitorKey,
    laps: Vec<ScoredLap>,
}

impl Run {
    /// Build per-competitor runs from `(offset, lap-gate pass)` pairs: group by `(adapter,
    /// competitor)`, order within a group, then each pass after the holeshot completes a lap. Each
    /// lap carries its **end pass offset** so a [`thrown-out`](Adjudications::is_thrown) lap can be
    /// excluded by stable identity downstream.
    ///
    /// The offset is the pass's global append offset on the offset-aware paths
    /// ([`score_with_global_offsets`] / [`apply_adjudications`]); the offset-free [`score`] entry
    /// supplies synthetic positional offsets, which is harmless because throw-outs only arrive via
    /// the offset-aware adjudicated paths.
    fn group(passes: &[(u64, Pass)]) -> Vec<Run> {
        use std::collections::BTreeMap;

        // BTreeMap keeps competitors in deterministic key order regardless of
        // arrival order — the same total-order tie-break the projection uses.
        let mut by_competitor: BTreeMap<CompetitorKey, Vec<(u64, &Pass)>> = BTreeMap::new();
        for (offset, pass) in passes {
            if pass.gate.is_lap_gate() {
                by_competitor
                    .entry(CompetitorKey {
                        adapter: pass.adapter.clone(),
                        competitor: pass.competitor.clone(),
                    })
                    .or_default()
                    .push((*offset, pass));
            }
        }

        by_competitor
            .into_iter()
            .map(|(competitor, mut group)| {
                // Same ordering rule as `gridfpv_projection::lap_list`: sequenced
                // passes first (ascending sequence), then unsequenced, `at` last.
                group.sort_by_key(|(_, p)| (p.sequence.is_none(), p.sequence, p.at));
                let laps = group
                    .windows(2)
                    .map(|pair| ScoredLap {
                        at: pair[1].1.at,
                        duration_micros: pair[1].1.at.micros_since(pair[0].1.at),
                        end_ref: pair[1].0,
                    })
                    .collect();
                Run { competitor, laps }
            })
            .collect()
    }

    /// This competitor's **counted** laps — those not thrown out — in order. The single place the
    /// throw-out exclusion is applied, so every win condition counts the same set.
    fn counted<'a>(&'a self, adj: &'a Adjudications) -> Vec<&'a ScoredLap> {
        self.laps
            .iter()
            .filter(|lap| !adj.is_thrown(lap.end_ref))
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
            // `race_end_reached` runs on the *live* pass stream (no adjudications yet), so positional
            // offsets suffice — it only counts laps, never resolves a throw-out.
            Run::group(&with_positional_offsets(passes))
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
    score_inner(
        &with_positional_offsets(passes),
        condition,
        race_start,
        &Adjudications::default(),
    )
}

/// Pair each pass with its **positional** index as a synthetic offset — the offset-free entry
/// points' bridge to the offset-aware [`Run::group`]. Safe for the non-adjudicated paths because a
/// throw-out (the only ruling that reads a lap's offset) only ever arrives through the
/// offset-aware paths that thread the *true* global offsets.
fn with_positional_offsets(passes: &[Pass]) -> Vec<(u64, Pass)> {
    passes
        .iter()
        .enumerate()
        .map(|(i, p)| (i as u64, p.clone()))
        .collect()
}

/// The adjudications a heat's [`Event::PenaltyApplied`] / [`Event::HeatVoided`] log
/// distils to (with any [`Event::RulingReversed`] un-applying a targeted penalty), applied
/// on top of the pure on-track scoring (race-engine.html §7.1, #13).
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
    /// Lap-gate pass **offsets** whose lap was [`thrown out`](Event::LapThrownOut): the lap
    /// *ending* at one of these offsets is a real lap but is excluded from the scored count
    /// (marshaling.html §3.3). A set, so the exclusion is a pure, order-independent membership
    /// test (the throw-out determinism risk, marshaling-plan.html §4).
    thrown_out: BTreeSet<u64>,
    /// Whether the whole heat was voided ([`Event::HeatVoided`]).
    voided: bool,
}

impl Adjudications {
    /// Distil a heat's event log into its adjudications. Ignores everything that is not a
    /// [`Event::PenaltyApplied`] / [`Event::HeatVoided`] / [`Event::RulingReversed`];
    /// `TimeAdded` penalties accumulate, any `Disqualify` disqualifies, any `HeatVoided`
    /// voids. Deterministic — pure fold (no clock / RNG), order-independent in effect.
    ///
    /// A [`Event::RulingReversed { target }`](Event::RulingReversed) **un-applies** the
    /// penalty at the `target` offset: a reversed `Disqualify` no longer disqualifies and a
    /// reversed `TimeAdded` is excluded from the accumulation — so "DQ reversed" reads as a
    /// first-class undo rather than overloading the lap-level void. The offset is the event's
    /// slice index (the storage layer assigns dense append offsets; the scorer is fed the
    /// heat window in append order, the same convention `corrected_passes` uses). Reversals
    /// are gathered first so they apply regardless of whether they precede or follow the
    /// penalty in the slice, keeping the fold order-independent.
    /// Collect adjudications from `(global offset, event)` pairs.
    ///
    /// A [`Event::RulingReversed { target }`] is matched against the **global append offset** the
    /// ruling was logged at — the same offset a [`LogRef`](gridfpv_events::LogRef) command carries
    /// and the audit/lap projections expose (#55). The caller MUST feed the true global offsets (not
    /// a re-enumerated window), or a reversal targets the wrong ruling: the result snapshot threads
    /// the heat window's preserved global offsets exactly for this reason.
    ///
    /// **Generalized reversal (Slice 6):** a reversal un-applies *any* targeted ruling — a
    /// [`PenaltyApplied`](Event::PenaltyApplied) (DQ / time; points are standings-only and folded
    /// elsewhere), a [`LapThrownOut`](Event::LapThrownOut), or a [`HeatVoided`](Event::HeatVoided) —
    /// keyed purely on the target's offset, so the throw-out / void / penalty all undo through the
    /// one structural mechanism.
    fn collect<'a, I>(events: I) -> Self
    where
        I: IntoIterator<Item = (u64, &'a Event)>,
    {
        // Materialize once so we can scan reversals first, then apply (order-independent).
        let events: Vec<(u64, &Event)> = events.into_iter().collect();
        // First, the offsets every `RulingReversed` targets — a ruling at one of these is
        // un-applied. Gathered first so a reversal applies whether it precedes or follows its
        // target in the slice, keeping the fold order-independent.
        let reversed: BTreeSet<u64> = events
            .iter()
            .filter_map(|(_, e)| match e {
                Event::RulingReversed { target } => Some(target.0),
                _ => None,
            })
            .collect();

        let mut adj = Adjudications::default();
        for (offset, event) in events {
            // A ruling whose own append offset was reversed is dropped from the result.
            if reversed.contains(&offset) {
                continue;
            }
            match event {
                Event::PenaltyApplied {
                    competitor,
                    penalty,
                    ..
                } => match penalty {
                    Penalty::Disqualify { .. } => {
                        adj.disqualified.insert(competitor.clone());
                    }
                    Penalty::TimeAdded { micros } => {
                        *adj.time_added.entry(competitor.clone()).or_default() += *micros;
                    }
                    // Points penalties are **standings-only** (marshaling.html §3.3): they never
                    // touch the per-heat lap result, so the heat scorer ignores them here. The
                    // standings projection (`class_standings`) folds them instead.
                    Penalty::PointsDeducted { .. } | Penalty::PointsAdded { .. } => {}
                },
                // A thrown-out lap: record its end-pass offset; the per-condition scorers exclude
                // the lap ending there from the counted set (order-independent set membership).
                Event::LapThrownOut { target } => {
                    adj.thrown_out.insert(target.0);
                }
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

    /// Whether the lap whose **end pass** is at append offset `end_ref` was thrown out — excluded
    /// from the scored count (a pure, order-independent membership test).
    fn is_thrown(&self, end_ref: u64) -> bool {
        self.thrown_out.contains(&end_ref)
    }
}

/// Score `passes` under `condition`, then apply `adj`'s adjudications: [`Penalty::TimeAdded`]
/// worsens the deciding time used to rank a competitor, [`Penalty::Disqualify`] sinks a
/// competitor below every non-disqualified one (flagging [`Placement::disqualified`]), and
/// [`Event::HeatVoided`] flags the whole [`HeatResult`] voided.
fn score_inner(
    passes: &[(u64, Pass)],
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
    // Positional offsets: callers of this entry point pass the *full* log (or a window that is the
    // whole log), so the slice index equals the global append offset. The snapshot's windowed path
    // uses [`score_with_global_offsets`] instead, which carries the real global offsets so a
    // `RulingReversed` matches the right penalty (#55).
    score_with_global_offsets(
        events.iter().enumerate().map(|(i, e)| (i as u64, e)),
        condition,
        race_start,
    )
}

/// Score a heat's events under `condition`, applying adjudications keyed on the **global append
/// offset** each event carries (#55).
///
/// This is the offset-correct sibling of [`score_with_adjudications`]: it is fed `(global_offset,
/// &Event)` pairs (e.g. a heat window that preserved its global offsets), so a
/// [`Event::RulingReversed`] un-applies the penalty at its true global `target` — not a
/// window-relative position. The result snapshot uses this so a UI reversal hits the right ruling.
pub fn score_with_global_offsets<'a, I>(
    events: I,
    condition: WinCondition,
    race_start: SourceTime,
) -> HeatResult
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    let events: Vec<(u64, &Event)> = events.into_iter().collect();
    // Keep each lap-gate pass paired with its **global** offset, so a `LapThrownOut` targeting that
    // pass's offset excludes the right lap (the throw-out's target is the lap's end-pass offset).
    let passes: Vec<(u64, Pass)> = events
        .iter()
        .filter_map(|(o, e)| match e {
            Event::Pass(p) if p.gate.is_lap_gate() => Some((*o, p.clone())),
            _ => None,
        })
        .collect();
    score_inner(
        &passes,
        condition,
        race_start,
        &Adjudications::collect(events.iter().map(|(o, e)| (*o, *e))),
    )
}

/// Score already-grouped/corrected `passes`, applying the adjudications carried by `events`.
///
/// Used by the marshaling-aware path ([`crate::event::score_marshaled`]): it has already
/// folded void/insert/adjust into a corrected pass stream, so here we only re-derive the
/// adjudications from the same log and apply them — penalties and heat-void compose with
/// marshaling without either fold knowing about the other.
pub(crate) fn apply_adjudications(
    passes: &[(u64, Pass)],
    condition: WinCondition,
    race_start: SourceTime,
    events: &[Event],
) -> HeatResult {
    // Positional offsets — `score_marshaled` passes the full heat log, so slice index == global
    // append offset (the same convention `corrected_passes` uses for the marshaling fold). The
    // `passes` carry each corrected pass's addressable offset (its `end_ref`), so a `LapThrownOut`
    // targeting that offset excludes the matching lap from the count.
    score_inner(
        passes,
        condition,
        race_start,
        &Adjudications::collect(events.iter().enumerate().map(|(i, e)| (i as u64, e))),
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
    rows: Vec<(CompetitorKey, u32, Metric, Option<i64>, K)>,
    adj: &Adjudications,
) -> HeatResult {
    // Pair each row with its DQ flag; the flag is the *primary* sort key (false < true),
    // so every disqualified competitor ranks after every non-disqualified one.
    let mut rows: Vec<(bool, CompetitorKey, u32, Metric, Option<i64>, K)> = rows
        .into_iter()
        .map(|(competitor, laps, metric, best_lap_micros, key)| {
            (
                adj.is_dq(&competitor.competitor),
                competitor,
                laps,
                metric,
                best_lap_micros,
                key,
            )
        })
        .collect();
    // Total order: DQ first, then rank key, then competitor key as the final
    // deterministic tie-break so two rows are never "equal" for sorting purposes.
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.5.cmp(&b.5))
            .then_with(|| a.1.cmp(&b.1))
    });

    let mut places = Vec::with_capacity(rows.len());
    // A position groups by the *ranking* identity that competitors share: the DQ flag
    // plus the rank key. A DQ'd competitor never shares a position with a non-DQ'd one.
    let mut prev_group: Option<(bool, K)> = None;
    let mut position = 0u32;
    for (index, (disqualified, competitor, laps, metric, best_lap_micros, key)) in
        rows.into_iter().enumerate()
    {
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
            best_lap_micros,
            disqualified,
        });
    }
    HeatResult {
        places,
        voided: false,
    }
}

/// The competitor's **fastest single completed lap** (smallest duration, µs) among `laps`, or
/// `None` if they completed none. Win-condition-independent: every per-condition scorer reports
/// it on its [`Placement`] so a cross-heat ranking can break ties on best lap regardless of how
/// the heat was won. `laps` are the **counted** laps (thrown-out laps already excluded), so a
/// thrown-out lap can never be a competitor's best lap.
fn fastest_lap_micros(laps: &[&ScoredLap]) -> Option<i64> {
    laps.iter().map(|lap| lap.duration_micros).min()
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
            // (or after) does not count — no finishing the in-progress lap. Thrown-out laps are
            // already excluded by `counted` before the cutoff filter.
            let all_counted = run.counted(adj);
            // Best single lap is win-condition-independent: a lap the competitor actually flew is
            // a real lap even if it landed outside the timed window, so it is taken over every
            // counted lap, not just the windowed ones.
            let best_lap = fastest_lap_micros(&all_counted);
            let counted: Vec<&ScoredLap> = all_counted
                .into_iter()
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
            (
                run.competitor,
                count,
                Metric::LastLapAt(last_at),
                best_lap,
                key,
            )
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
            // Score the **counted** laps (thrown-out laps excluded) — so a throw-out can drop a
            // reacher below `n`, exactly as if the lap had not been flown for scoring purposes.
            let laps = run.counted(adj);
            let count = laps.len() as u32;
            // Best single lap, independent of the first-to-N win metric (which is a reach-time).
            let best_lap = fastest_lap_micros(&laps);
            // `n` laps means the n-th completed lap (1-based) — index n-1.
            let reached_at = if n >= 1 && count >= n {
                Some(laps[(n - 1) as usize].at)
            } else {
                None
            };
            let last_at = laps.last().map(|lap| lap.at);
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
            (
                run.competitor,
                count,
                Metric::ReachedAt(reached_at),
                best_lap,
                key,
            )
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
            // A thrown-out lap is not eligible to be a competitor's best lap.
            let laps = run.counted(adj);
            let count = laps.len() as u32;
            // Fastest lap = smallest duration; tie-break on the earlier-set one.
            let best = laps
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
                // The win metric here *is* the fastest single lap, so report it directly.
                best_micros,
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
            // Consecutive windows are over the **counted** laps — a thrown-out lap breaks the run
            // of consecutiveness (it is excluded, not skipped-over), matching the lap-count model.
            let laps = run.counted(adj);
            let count = laps.len() as u32;
            // Best single lap, independent of the consecutive-sum win metric.
            let best_lap = fastest_lap_micros(&laps);
            // Slide an `n`-wide window over the laps; pick the smallest sum, tie-
            // broken by the earlier window-end (last lap's completion time).
            let best = laps
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
                best_lap,
                key,
            )
        })
        .collect();
    rank(rows, adj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{AdapterId, CompetitorRef, GateIndex, HeatId, LogRef};

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

    // --- best_lap_micros (win-condition-independent fastest single lap) ------

    #[test]
    fn first_to_laps_placement_carries_fastest_single_lap_not_the_win_metric() {
        // A's laps: 3s, 2s, 4s ⇒ fastest single lap 2s. The win metric is ReachedAt (a time),
        // so best_lap_micros must be the 2s lap duration, independent of the win condition.
        let passes = run("A", &[0, 3_000_000, 5_000_000, 9_000_000]);
        let r = score(&passes, WinCondition::FirstToLaps { n: 3 }, start());
        assert_eq!(place(&r, "A").best_lap_micros, Some(2_000_000));
        // And the win metric is still the reach-time, not the best lap.
        assert_eq!(
            place(&r, "A").metric,
            Metric::ReachedAt(Some(SourceTime::from_micros(9_000_000)))
        );
    }

    #[test]
    fn timed_placement_carries_fastest_single_lap_even_outside_the_window() {
        // Window 10s. A's laps complete at 5s (dur 5s) and 12s (dur 7s); the 2nd lands past the
        // cutoff so it does not COUNT, but it is a real flown lap. Fastest single lap is still the
        // 5s one. (Here the windowed lap is also the fastest, but the point is best lap is taken
        // over every counted lap, not just the windowed ones.)
        let passes = run("A", &[0, 5_000_000, 12_000_000]);
        let r = score(
            &passes,
            WinCondition::Timed {
                window_micros: 10_000_000,
            },
            start(),
        );
        assert_eq!(place(&r, "A").laps, 1, "only the windowed lap counts");
        assert_eq!(
            place(&r, "A").best_lap_micros,
            Some(5_000_000),
            "best lap is the fastest of every flown lap"
        );
    }

    #[test]
    fn placement_with_no_completed_lap_has_no_best_lap() {
        // Z has a single pass (no completed lap) ⇒ best_lap_micros is None under any condition.
        let passes = run("Z", &[1_000_000]);
        let r = score(&passes, WinCondition::FirstToLaps { n: 1 }, start());
        assert_eq!(place(&r, "Z").best_lap_micros, None);
        assert_eq!(place(&r, "Z").laps, 0);
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
        events.push(penalty("A", Penalty::Disqualify { reason: None }));

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
        events.push(penalty("A", Penalty::Disqualify { reason: None }));
        events.push(penalty("B", Penalty::Disqualify { reason: None }));

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
        events.push(penalty("B", Penalty::Disqualify { reason: None }));

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

    // --- RulingReversed (Slice 2): un-apply a targeted penalty -----------------

    #[test]
    fn ruling_reversed_unapplies_a_dq() {
        // A leads on track (3 laps), B has 2. DQ A — A sinks to last — then REVERSE that DQ:
        // A is restored to the lead and no longer flagged disqualified.
        use gridfpv_events::LogRef;

        let mut events = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]); // offsets 0..3
        events.extend(pass_events("B", &[0, 5_000_000, 10_000_000])); // offsets 4..6
        let dq_offset = events.len() as u64; // offset 7
        events.push(penalty("A", Penalty::Disqualify { reason: None })); // offset 7
        events.push(Event::RulingReversed {
            target: LogRef(dq_offset),
        }); // offset 8 — reverse the DQ

        let r = score_events(
            &events,
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );
        // The DQ is un-applied: A leads and is not flagged.
        assert_eq!(place(&r, "A").position, 1);
        assert!(!place(&r, "A").disqualified);
        assert_eq!(place(&r, "B").position, 2);
    }

    #[test]
    fn ruling_reversed_targets_the_global_offset_not_a_window_position() {
        // The load-bearing #55 offset fix in the scoring path: when scoring a heat WINDOW (events
        // that start at a non-zero global offset), a `RulingReversed` must match the penalty by its
        // GLOBAL offset, not by the window-relative index. We score via `score_with_global_offsets`
        // feeding offsets that start at 100 — the DQ at global 107, reversed by targeting 107.
        use gridfpv_events::LogRef;

        let mut events = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]); // window idx 0..3
        events.extend(pass_events("B", &[0, 5_000_000, 10_000_000])); // window idx 4..6
        events.push(penalty("A", Penalty::Disqualify { reason: None })); // window idx 7 → GLOBAL 107
        events.push(Event::RulingReversed {
            target: LogRef(107), // the DQ's GLOBAL offset, NOT window index 7
        });

        // Tag with global offsets 100.. (as a heat window that preserved them would).
        let r = score_with_global_offsets(
            events.iter().enumerate().map(|(i, e)| (100 + i as u64, e)),
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );
        // The reversal hit the right penalty: A is restored to the lead, not flagged.
        assert_eq!(place(&r, "A").position, 1);
        assert!(!place(&r, "A").disqualified);

        // Sanity: targeting the *window-relative* offset (7) would NOT reverse the global-107 DQ.
        let mut wrong = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]);
        wrong.extend(pass_events("B", &[0, 5_000_000, 10_000_000]));
        wrong.push(penalty("A", Penalty::Disqualify { reason: None }));
        wrong.push(Event::RulingReversed { target: LogRef(7) }); // wrong: a window index
        let r2 = score_with_global_offsets(
            wrong.iter().enumerate().map(|(i, e)| (100 + i as u64, e)),
            WinCondition::Timed {
                window_micros: 60_000_000,
            },
            start(),
        );
        assert!(
            place(&r2, "A").disqualified,
            "a window-relative target must NOT reverse the global-offset DQ"
        );
    }

    #[test]
    fn ruling_reversed_unapplies_a_time_penalty() {
        // BestLap: A 2.0s, B 1.5s. A +1.0s time penalty would let B win; reversing it
        // restores A's raw 2.0s deciding time? No — B is faster, so reversal lets B win on
        // its raw lap. Concretely: with the penalty, A(2.0) beats B(1.5+1.0=2.5); reversed,
        // B(1.5) beats A(2.0). The reversal flips the order — proof it was un-applied.
        use gridfpv_events::LogRef;

        let mut events = pass_events("A", &[0, 2_000_000]); // offsets 0..1
        events.extend(pass_events("B", &[0, 1_500_000])); // offsets 2..3
        let pen_offset = events.len() as u64; // offset 4
        events.push(penalty("B", Penalty::TimeAdded { micros: 1_000_000 })); // offset 4

        // With the penalty in force, A wins.
        let with_pen = score_events(&events, WinCondition::BestLap, start());
        assert_eq!(place(&with_pen, "A").position, 1);

        // Reverse B's time penalty: B's raw 1.5s now wins.
        events.push(Event::RulingReversed {
            target: LogRef(pen_offset),
        }); // offset 5
        let reversed = score_events(&events, WinCondition::BestLap, start());
        assert_eq!(place(&reversed, "B").position, 1);
        assert_eq!(place(&reversed, "A").position, 2);
    }

    #[test]
    fn ruling_reversed_is_order_independent_and_deterministic() {
        // The fold gathers reversals first, so a reversal that PRECEDES its penalty in the
        // slice un-applies it just the same — and scoring the same log twice is identical
        // (no clock / RNG; the scorer-purity invariant holds with reversals folded in).
        use gridfpv_events::LogRef;

        // Reversal at offset 0, the penalty it targets at offset 1 — reversal comes first.
        let mut events: Vec<Event> = vec![Event::RulingReversed { target: LogRef(1) }];
        events.push(penalty("A", Penalty::Disqualify { reason: None })); // offset 1 — reversed by offset 0
        events.extend(pass_events("A", &[0, 3_000_000, 6_000_000])); // A: 2 laps
        events.extend(pass_events("B", &[0, 5_000_000])); // B: 1 lap

        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        let first = score_events(&events, cond, start());
        let second = score_events(&events, cond, start());
        assert_eq!(first, second, "scoring with a reversal replays identically");
        // The DQ was un-applied despite the reversal preceding it: A leads, not flagged.
        assert_eq!(place(&first, "A").position, 1);
        assert!(!place(&first, "A").disqualified);
    }

    // --- LapThrownOut (Slice 6): exclude a valid lap from the scored count -----------------------

    #[test]
    fn lap_thrown_out_excludes_a_valid_lap_from_the_count() {
        // A flies 3 laps (4 passes at offsets 0..3); throw out A's 2nd lap (the lap ENDING at the
        // pass at offset 2). A's counted laps drop 3 → 2, but the lap stays a real lap on track.
        let mut events = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]); // offsets 0..3
        events.push(Event::LapThrownOut { target: LogRef(2) }); // throw out lap ending at offset 2

        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        let r = score_events(&events, cond, start());
        assert_eq!(
            place(&r, "A").laps,
            2,
            "the thrown-out lap is excluded from the counted set"
        );
    }

    #[test]
    fn lap_thrown_out_recompute_is_order_independent() {
        // Determinism risk (marshaling-plan §4): the thrown-out set is a pure membership test, so
        // the order the throw-out appears in the log cannot change the result. Fold the same facts
        // in two different orders and assert identical scores.
        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };

        // Order 1: passes then the throw-out.
        let mut a = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]);
        a.push(Event::LapThrownOut { target: LogRef(2) });
        let ra = score_events(&a, cond, start());

        // Order 2: the throw-out FIRST (offset 0), then the passes (offsets 1..4). The target must
        // move to the new end-pass offset (4) — the throw-out is keyed on the lap's end-pass offset.
        let mut b: Vec<Event> = vec![Event::LapThrownOut { target: LogRef(4) }];
        b.extend(pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000])); // offsets 1..4
        let rb = score_events(&b, cond, start());

        assert_eq!(place(&ra, "A").laps, 2);
        assert_eq!(place(&rb, "A").laps, 2);
        // Folding either twice is identical (no clock/RNG); the scorer stays pure.
        assert_eq!(score_events(&a, cond, start()), ra);
        assert_eq!(score_events(&b, cond, start()), rb);
    }

    #[test]
    fn lap_thrown_out_reorders_a_first_to_laps_result() {
        // First-to-3: A reaches lap 3 at 9.0s, B at 10.0s → A wins. Throw out A's lap-2 (ending at
        // offset 2): A now has only 2 *counted* laps and never reaches 3, so B (who reached 3) wins.
        let mut events = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]); // offsets 0..3
        events.extend(pass_events("B", &[0, 4_000_000, 7_000_000, 10_000_000])); // offsets 4..7
        events.push(Event::LapThrownOut { target: LogRef(2) }); // throw out A's lap ending at offset 2

        let r = score_events(&events, WinCondition::FirstToLaps { n: 3 }, start());
        assert_eq!(
            place(&r, "B").position,
            1,
            "B reached 3 counted laps; A did not"
        );
        assert_eq!(place(&r, "A").position, 2);
        assert_eq!(place(&r, "A").laps, 2);
    }

    #[test]
    fn lap_thrown_out_is_reversible() {
        // Generalized reversal (Slice 6): reversing the `LapThrownOut` restores the excluded lap.
        let mut events = pass_events("A", &[0, 3_000_000, 6_000_000, 9_000_000]); // offsets 0..3
        let throw_offset = events.len() as u64; // offset 4
        events.push(Event::LapThrownOut { target: LogRef(2) }); // offset 4
        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        assert_eq!(place(&score_events(&events, cond, start()), "A").laps, 2);

        events.push(Event::RulingReversed {
            target: LogRef(throw_offset),
        }); // reverse the throw-out
        assert_eq!(
            place(&score_events(&events, cond, start()), "A").laps,
            3,
            "reversing the throw-out restores the lap"
        );
    }

    #[test]
    fn dq_time_and_throwout_stack_predictably() {
        // All three per-heat adjudications compose on one log, deterministically. Timed window.
        // A: 3 laps, throw out one → 2 counted. B: 3 laps, +big time penalty (worsens tie-break).
        // C: 3 laps clean. D: 3 laps but DQ'd. Expected order: C (clean 3), A (2 counted),
        //   B (3 but penalty-worsened tie-break still beats A's lower count), then D (DQ) last.
        // Concretely we assert the DQ sinks D last, the throw-out drops A's count, and the result
        // is identical across two runs (purity).
        let mut events = pass_events("A", &[0, 5_000_000, 10_000_000, 15_000_000]); // 0..3, 3 laps
        events.extend(pass_events("B", &[0, 5_000_000, 10_000_000, 15_000_000])); // 4..7, 3 laps
        events.extend(pass_events("C", &[0, 5_000_000, 10_000_000, 15_000_000])); // 8..11, 3 laps
        events.extend(pass_events("D", &[0, 5_000_000, 10_000_000, 15_000_000])); // 12..15, 3 laps
        events.push(Event::LapThrownOut { target: LogRef(3) }); // throw A's last lap → A: 2 counted
        events.push(penalty("B", Penalty::TimeAdded { micros: 50_000_000 })); // worsen B's tie-break
        events.push(penalty("D", Penalty::Disqualify { reason: None })); // DQ D

        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        let r = score_events(&events, cond, start());
        assert_eq!(place(&r, "A").laps, 2, "A lost a lap to the throw-out");
        assert_eq!(place(&r, "C").laps, 3);
        assert!(place(&r, "D").disqualified, "D is DQ'd");
        // The DQ sinks D below everyone non-DQ'd.
        let d_pos = place(&r, "D").position;
        for p in &r.places {
            if !p.disqualified {
                assert!(p.position < d_pos);
            }
        }
        // C (clean 3 laps) leads.
        assert_eq!(place(&r, "C").position, 1);
        // Deterministic on replay.
        assert_eq!(score_events(&events, cond, start()), r);
    }

    #[test]
    fn points_penalties_do_not_touch_the_heat_result() {
        // PointsDeducted / PointsAdded are standings-only (folded by `class_standings`): the per-heat
        // scorer must ignore them, so a heat result with a points penalty equals the clean result.
        let mut with_points = pass_events("A", &[0, 3_000_000, 6_000_000]);
        with_points.extend(pass_events("B", &[0, 4_000_000]));
        let clean = with_points.clone();
        with_points.push(penalty("A", Penalty::PointsDeducted { points: 10 }));
        with_points.push(penalty("B", Penalty::PointsAdded { points: 5 }));

        let cond = WinCondition::Timed {
            window_micros: 60_000_000,
        };
        assert_eq!(
            score_events(&with_points, cond, start()),
            score_events(&clean, cond, start()),
            "points penalties leave the per-heat result unchanged"
        );
    }
}
