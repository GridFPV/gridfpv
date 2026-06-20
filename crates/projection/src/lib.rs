//! Projection engine — folds the append-only log into derived read models.
//!
//! Projections are recomputable from the log with no hidden state. The first
//! projection (a lap list) lands in #7; this crate is where it and later
//! projections (standings, brackets, stats) live.
//!
//! # Lap projection (#7)
//!
//! A **lap is two consecutive lap-gate passes** for the same competitor,
//! computed identically for every source. [`lap_list`] folds a sequence of
//! [`Event`]s into a [`LapList`] with no hidden state: folding the same events
//! always yields the same result, so the read model can be rebuilt from the log
//! at any time.
//!
//! # Marshaling (#31)
//!
//! Corrections are never mutations (architecture.html §3): the raw [`Pass`]es
//! stay byte-identical in the log forever, and a marshal's ruling is a *new*
//! appended event that the projection **folds in** over them.
//! [`lap_list_marshaled`] is the marshaling-aware lap projection — it takes each
//! event paired with its append **offset** and folds the adjudications
//! ([`Event::DetectionVoided`], [`Event::LapInserted`], [`Event::LapAdjusted`])
//! into a *corrected view* of the lap-gate passes, then derives laps from that
//! view exactly as [`lap_list`] does. [`lap_list`] is the no-adjudications case:
//! it is a thin wrapper that assigns positional offsets and folds the same way,
//! so a log with no rulings projects identically through either entry point.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::{AdapterId, CompetitorRef, Event, Pass, SourceTime};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifies a competitor *within a single timing source*.
///
/// A source-local [`CompetitorRef`] is only meaningful relative to the adapter
/// that emitted it (node 2 on RotorHazard is unrelated to node 2 on a second
/// timer), so laps are grouped on the `(AdapterId, CompetitorRef)` pair. Binding
/// these per-source competitors to a single GridFPV pilot is a later registration
/// concern (Architecture §9) and deliberately out of scope here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CompetitorKey {
    /// The timing source the competitor belongs to.
    pub adapter: AdapterId,
    /// The source-local competitor handle.
    pub competitor: CompetitorRef,
}

impl CompetitorKey {
    /// Build a key from the `(adapter, competitor)` pair of a [`Pass`].
    fn from_pass(pass: &Pass) -> Self {
        Self {
            adapter: pass.adapter.clone(),
            competitor: pass.competitor.clone(),
        }
    }
}

/// A single completed lap: the interval between two consecutive lap-gate passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Lap {
    /// 1-based lap number within the competitor's run.
    pub number: u32,
    /// Lap duration in microseconds on the source clock
    /// (`pass[n + 1].at - pass[n].at`). Always `>= 0` for in-order passes.
    pub duration_micros: i64,
}

/// Every lap a single competitor completed, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CompetitorLaps {
    /// Which source-local competitor these laps belong to.
    pub competitor: CompetitorKey,
    /// Completed laps, ordered by lap number (1-based, ascending).
    pub laps: Vec<Lap>,
}

impl CompetitorLaps {
    /// Number of completed laps (`K - 1` for `K` lap-gate passes).
    pub fn lap_count(&self) -> usize {
        self.laps.len()
    }

    /// Sum of all lap durations in microseconds; `0` when there are no laps.
    pub fn total_micros(&self) -> i64 {
        self.laps.iter().map(|lap| lap.duration_micros).sum()
    }

    /// The fastest lap, or `None` when no laps were completed.
    pub fn best(&self) -> Option<&Lap> {
        self.laps.iter().min_by_key(|lap| lap.duration_micros)
    }
}

/// The lap-list read model: per-competitor lap lists derived from the log.
///
/// Competitors are ordered deterministically by [`CompetitorKey`] so the
/// projection is stable across runs regardless of event arrival order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LapList {
    /// Per-competitor laps, ordered by competitor key.
    pub competitors: Vec<CompetitorLaps>,
}

impl LapList {
    /// Look up a single competitor's laps by key, if present.
    pub fn competitor(&self, key: &CompetitorKey) -> Option<&CompetitorLaps> {
        self.competitors.iter().find(|c| &c.competitor == key)
    }
}

/// Fold a sequence of events into the lap-list read model.
///
/// Only [`Event::Pass`]es over the **lap gate** ([`is_lap_gate`]) contribute;
/// lifecycle events and split passes are ignored. Passes are grouped by
/// `(adapter, competitor)` and ordered within each group, then consecutive pairs
/// become laps.
///
/// # Ordering and tie-breaks
///
/// Within a competitor, passes are ordered by `at` (source timestamp), with
/// `sequence` as the tie-break for passes that share an instant (sequenced ahead
/// of unsequenced, then by `sequence` ascending). A real source either numbers its
/// passes monotonically in step with its clock or carries no sequence at all, so
/// ordering by `at` reproduces the source's timeline either way; the `sequence`
/// tie-break just keeps coincident passes deterministic. (This is the same key the
/// marshaling fold uses — see [`lap_list_marshaled`] — so the un-marshaled and
/// marshaled projections agree on ordering.)
///
/// The sort is *stable*, so passes with fully equal keys keep their original log
/// order.
///
/// Accepts anything iterable over `&Event` (e.g. `&[Event]`), so it is decoupled
/// from storage and trivially testable. This is the no-adjudications wrapper over
/// [`lap_list_marshaled`]; see it for the marshaling-aware fold.
///
/// [`is_lap_gate`]: gridfpv_events::GateIndex::is_lap_gate
pub fn lap_list<'a, I>(events: I) -> LapList
where
    I: IntoIterator<Item = &'a Event>,
{
    // The un-marshaled case is just the marshaling fold over a log that happens
    // to carry no adjudications: tag each event with its positional offset and
    // defer to `lap_list_marshaled`. With no `DetectionVoided`/`LapInserted`/
    // `LapAdjusted` present the corrected view is the raw view, so this projects
    // byte-for-byte identically to the original lap fold.
    lap_list_marshaled(events.into_iter().enumerate().map(|(i, e)| (i as u64, e)))
}

/// A lap-gate pass in the **corrected view** the marshaling fold builds.
///
/// It is never a mutation of a raw [`Pass`] — it is a derived datum carrying just
/// the `(adapter, competitor, at, sequence)` the lap derivation needs, sourced
/// either from a raw `Pass` (possibly re-timed by a [`Event::LapAdjusted`]) or
/// synthesised from a [`Event::LapInserted`]. The raw log is untouched.
#[derive(Debug, Clone)]
struct CorrectedPass {
    competitor: CompetitorKey,
    at: SourceTime,
    sequence: Option<u64>,
}

/// Fold a sequence of `(offset, event)` pairs into the lap-list read model,
/// applying marshaling adjudications keyed on the target's append **offset** (#31).
///
/// This is the marshaling-aware sibling of [`lap_list`]. Each event is paired with
/// its append [`LogRef`](gridfpv_events::LogRef) offset; rulings reference the raw
/// event they correct by that offset. The fold builds a **corrected view** of the
/// lap-gate passes and then derives laps from it exactly like [`lap_list`] — the
/// raw [`Pass`]es in the input are never mutated (architecture.html §3).
///
/// # Adjudications folded
///
/// - [`Event::DetectionVoided { target }`](Event::DetectionVoided) — drop the
///   correction at `target` offset, as if it was never detected.
/// - [`Event::LapInserted { adapter, competitor, at }`](Event::LapInserted) — add
///   a synthetic lap-gate pass for that competitor at `at` (a lap the timer missed).
///   The insert's own offset becomes a valid `target` for a later ruling.
/// - [`Event::LapAdjusted { target, at }`](Event::LapAdjusted) — re-time the pass
///   at `target` offset to `at`.
///
/// # Offsets and last-writer-wins
///
/// Every fold-relevant entry — a raw lap-gate [`Pass`] *or* an adjudication — owns
/// the offset it was appended at, and a ruling addresses its target by that offset.
/// Rulings are applied **in log (offset) order**, so the *last writer to a given
/// target wins*: a [`Event::LapAdjusted`] then a later [`Event::DetectionVoided`]
/// of the same target leaves the pass voided; a later adjust of an already-adjusted
/// pass re-times from the original raw pass to the newest `at` (adjusts are not
/// cumulative — each re-times the *target's* raw value).
///
/// # "Void the void"
///
/// Because an adjudication is itself addressable by its offset, a
/// [`Event::DetectionVoided`] may target *another adjudication* rather than a raw
/// pass — the architecture.html §3 "void the void". A marshal who ruled wrongly
/// appends a higher-offset ruling that supersedes the earlier one:
///
/// - voiding a [`Event::LapInserted`] removes that synthetic pass again (the
///   inserted lap never makes it into the view);
/// - voiding a [`Event::LapAdjusted`] cancels the re-time, so its target raw pass
///   reverts to its original timestamp (the void supersedes the adjust);
/// - voiding a [`Event::DetectionVoided`] un-voids that earlier void's target — the
///   originally-voided raw pass comes back.
///
/// Resolution is purely last-writer-wins by offset and nothing is ever lost, so the
/// fold stays deterministic and recomputable.
///
/// # Heat / result-level rulings
///
/// [`Event::HeatVoided`] and [`Event::PenaltyApplied`] are *not* lap-level — they
/// reshape the heat result, not the per-competitor lap list — so the lap projection
/// ignores them here. They are consumed by scoring/results (#30, #33+), which fold
/// the same log alongside this lap view.
pub fn lap_list_marshaled<'a, I>(events: I) -> LapList
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    // First pass: record, by offset, every entry a later ruling could target —
    // raw lap-gate passes and the adjudications themselves — plus the rulings to
    // apply. We resolve targets against this map so "void the void" (a ruling whose
    // target is another ruling) and last-writer-wins both fall out of offset order.
    #[derive(Clone)]
    enum Entry<'a> {
        /// A raw lap-gate pass observed by an adapter (never mutated).
        RawPass(&'a Pass),
        /// A synthetic lap-gate pass inserted by marshaling.
        Inserted {
            competitor: CompetitorKey,
            at: SourceTime,
        },
        /// A re-time ruling: the target pass's `at` is overridden to this value.
        Adjusted { target: u64, at: SourceTime },
        /// A void ruling: the target entry is dropped from the corrected view.
        Voided { target: u64 },
    }

    let mut entries: BTreeMap<u64, Entry<'a>> = BTreeMap::new();
    for (offset, event) in events {
        match event {
            Event::Pass(pass) if pass.gate.is_lap_gate() => {
                entries.insert(offset, Entry::RawPass(pass));
            }
            Event::LapInserted {
                adapter,
                competitor,
                at,
            } => {
                entries.insert(
                    offset,
                    Entry::Inserted {
                        competitor: CompetitorKey {
                            adapter: adapter.clone(),
                            competitor: competitor.clone(),
                        },
                        at: *at,
                    },
                );
            }
            Event::LapAdjusted { target, at } => {
                entries.insert(
                    offset,
                    Entry::Adjusted {
                        target: target.0,
                        at: *at,
                    },
                );
            }
            Event::DetectionVoided { target } => {
                entries.insert(offset, Entry::Voided { target: target.0 });
            }
            // Splits, lifecycle, heat transitions, and the heat/result-level
            // rulings (`HeatVoided`, `PenaltyApplied`) never touch the lap view.
            _ => {}
        }
    }

    // Resolve each entry to its effective state by walking the chain of rulings in
    // offset order (BTreeMap iterates ascending). `voided[off]` marks an offset as
    // dropped from the view; `retime[off]` overrides a raw pass's timestamp. A
    // ruling targeting another ruling is the "void the void" / re-rule case — we
    // apply it against the target's *kind*, last writer winning by construction
    // (we process offsets ascending, so a later ruling overwrites an earlier one).
    let mut voided: BTreeMap<u64, bool> = BTreeMap::new();
    let mut retime: BTreeMap<u64, SourceTime> = BTreeMap::new();
    for (_offset, entry) in entries.iter() {
        match entry {
            Entry::RawPass(_) | Entry::Inserted { .. } => {}
            Entry::Adjusted { target, at } => {
                // Re-time the target raw/inserted pass, and un-void it: an adjust is
                // the newest ruling on that target, so it supersedes an earlier void.
                voided.insert(*target, false);
                retime.insert(*target, *at);
            }
            Entry::Voided { target } => {
                // Void the target. If the target is itself a ruling, supersede it:
                // voiding an adjust cancels its re-time (revert to the raw `at`);
                // voiding a void un-voids *that* void's target.
                match entries.get(target) {
                    Some(Entry::Adjusted {
                        target: inner_target,
                        ..
                    }) => {
                        // Cancel the adjust: drop its re-time so the inner target
                        // reverts to its original timestamp, and leave the inner
                        // target present (the adjust, not the pass, was voided).
                        retime.remove(inner_target);
                    }
                    Some(Entry::Voided {
                        target: inner_target,
                    }) => {
                        // Void the void: resurrect the originally-voided target.
                        voided.insert(*inner_target, false);
                    }
                    // Voiding a raw pass or an inserted pass simply drops it.
                    _ => {
                        voided.insert(*target, true);
                    }
                }
            }
        }
    }

    // Build the corrected view: every raw/inserted pass that survived voiding,
    // with any re-time applied. Group by competitor for lap derivation.
    let mut by_competitor: BTreeMap<CompetitorKey, Vec<CorrectedPass>> = BTreeMap::new();
    for (offset, entry) in entries.iter() {
        if voided.get(offset).copied().unwrap_or(false) {
            continue;
        }
        let corrected = match entry {
            Entry::RawPass(pass) => CorrectedPass {
                competitor: CompetitorKey::from_pass(pass),
                at: retime.get(offset).copied().unwrap_or(pass.at),
                sequence: pass.sequence,
            },
            Entry::Inserted { competitor, at } => CorrectedPass {
                competitor: competitor.clone(),
                // An inserted lap can itself be re-timed by a later adjust.
                at: retime.get(offset).copied().unwrap_or(*at),
                // Synthetic passes carry no source sequence; ordered by `at`.
                sequence: None,
            },
            // Rulings are not passes in the view.
            Entry::Adjusted { .. } | Entry::Voided { .. } => continue,
        };
        by_competitor
            .entry(corrected.competitor.clone())
            .or_default()
            .push(corrected);
    }

    let competitors = by_competitor
        .into_iter()
        .map(|(competitor, mut passes)| {
            passes.sort_by_key(corrected_order_key);
            CompetitorLaps {
                competitor,
                laps: laps_from_corrected(&passes),
            }
        })
        .collect();

    LapList { competitors }
}

/// Ordering key for a corrected pass.
///
/// A *corrected view* is a single coherent timeline: a re-timed pass moves to its
/// new instant and a synthetic inserted pass slots in chronologically, so the
/// view is ordered by `at` first. `sequence` is only a tie-break for passes that
/// share a timestamp (sequenced passes ahead of unsequenced, then by sequence),
/// keeping the fold deterministic. This subsumes the un-marshaled rule from
/// [`lap_list`]: when there are no rulings, a source either numbers its passes
/// monotonically in step with `at` or carries no sequence at all, so ordering by
/// `at` yields the same lap list.
fn corrected_order_key(pass: &CorrectedPass) -> (SourceTime, bool, Option<u64>) {
    (pass.at, pass.sequence.is_none(), pass.sequence)
}

/// Turn an ordered run of corrected lap-gate passes into laps: `K` passes ⇒
/// `K - 1` laps, each spanning a consecutive pair.
fn laps_from_corrected(passes: &[CorrectedPass]) -> Vec<Lap> {
    passes
        .windows(2)
        .enumerate()
        .map(|(idx, pair)| Lap {
            number: (idx + 1) as u32,
            duration_micros: pair[1].at.micros_since(pair[0].at),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{GateIndex, SessionId, SignalContext};

    /// Build a lap-gate pass with the given competitor, timestamp and sequence.
    fn pass(adapter: &str, competitor: &str, at: i64, sequence: Option<u64>) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    /// Build a split (non-lap-gate) pass.
    fn split(adapter: &str, competitor: &str, at: i64, gate: u32) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: None,
            gate: GateIndex(gate),
            signal: Some(SignalContext { rssi_peak: None }),
        })
    }

    fn key(adapter: &str, competitor: &str) -> CompetitorKey {
        CompetitorKey {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
        }
    }

    #[test]
    fn clean_multi_lap_run_yields_k_minus_one_laps() {
        // 4 lap-gate passes ⇒ 3 laps with exact integer-microsecond durations.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "A", 6_500_000, Some(3)),
            pass("vd", "A", 11_000_000, Some(4)),
        ];
        let result = lap_list(&events);
        let laps = &result.competitor(&key("vd", "A")).unwrap().laps;
        assert_eq!(
            laps,
            &vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000,
                },
                Lap {
                    number: 2,
                    duration_micros: 2_500_000,
                },
                Lap {
                    number: 3,
                    duration_micros: 4_500_000,
                },
            ]
        );
        let cl = result.competitor(&key("vd", "A")).unwrap();
        assert_eq!(cl.lap_count(), 3);
        assert_eq!(cl.total_micros(), 10_000_000);
        assert_eq!(cl.best(), Some(&laps[1]));
    }

    #[test]
    fn single_pass_yields_zero_laps() {
        let events = vec![pass("vd", "A", 1_000_000, Some(1))];
        let result = lap_list(&events);
        let cl = result.competitor(&key("vd", "A")).unwrap();
        assert_eq!(cl.laps, vec![]);
        assert_eq!(cl.lap_count(), 0);
        assert_eq!(cl.total_micros(), 0);
        assert_eq!(cl.best(), None);
    }

    #[test]
    fn empty_log_yields_empty_lap_list() {
        let events: Vec<Event> = vec![];
        assert_eq!(lap_list(&events), LapList::default());
    }

    #[test]
    fn multiple_competitors_interleaved_are_grouped_independently() {
        // Two competitors on the same adapter, passes interleaved in the log.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "B", 1_500_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "B", 5_500_000, Some(2)),
            pass("vd", "A", 6_000_000, Some(3)),
        ];
        let result = lap_list(&events);

        let a = result.competitor(&key("vd", "A")).unwrap();
        assert_eq!(
            a.laps,
            vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000,
                },
                Lap {
                    number: 2,
                    duration_micros: 2_000_000,
                },
            ]
        );

        let b = result.competitor(&key("vd", "B")).unwrap();
        assert_eq!(
            b.laps,
            vec![Lap {
                number: 1,
                duration_micros: 4_000_000,
            }]
        );
    }

    #[test]
    fn same_ref_on_different_adapters_is_two_competitors() {
        // CompetitorRef is per-source: "node-2" on two adapters never merges.
        let events = vec![
            pass("rh-a", "node-2", 0, Some(1)),
            pass("rh-a", "node-2", 2_000_000, Some(2)),
            pass("rh-b", "node-2", 0, Some(1)),
            pass("rh-b", "node-2", 3_000_000, Some(2)),
        ];
        let result = lap_list(&events);
        assert_eq!(result.competitors.len(), 2);
        assert_eq!(
            result.competitor(&key("rh-a", "node-2")).unwrap().laps,
            vec![Lap {
                number: 1,
                duration_micros: 2_000_000,
            }]
        );
        assert_eq!(
            result.competitor(&key("rh-b", "node-2")).unwrap().laps,
            vec![Lap {
                number: 1,
                duration_micros: 3_000_000,
            }]
        );
    }

    #[test]
    fn split_passes_are_ignored() {
        // Splits between lap-gate passes must not become laps or shift durations.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            split("vd", "A", 2_000_000, 1),
            split("vd", "A", 3_000_000, 2),
            pass("vd", "A", 5_000_000, Some(2)),
        ];
        let result = lap_list(&events);
        assert_eq!(
            result.competitor(&key("vd", "A")).unwrap().laps,
            vec![Lap {
                number: 1,
                duration_micros: 4_000_000,
            }]
        );
    }

    #[test]
    fn lifecycle_events_are_ignored() {
        let events = vec![
            Event::AdapterConnected {
                adapter: AdapterId("vd".into()),
            },
            Event::SessionStarted {
                adapter: AdapterId("vd".into()),
                session: SessionId("heat-1".into()),
            },
            Event::CompetitorSeen {
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
            },
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 3_000_000, Some(2)),
            Event::SessionEnded {
                adapter: AdapterId("vd".into()),
                session: SessionId("heat-1".into()),
            },
        ];
        let result = lap_list(&events);
        assert_eq!(
            result.competitor(&key("vd", "A")).unwrap().laps,
            vec![Lap {
                number: 1,
                duration_micros: 2_000_000,
            }]
        );
    }

    #[test]
    fn passes_are_ordered_by_sequence_not_log_order() {
        // Out-of-order arrival, but sequence is authoritative: 1 -> 2 -> 3.
        // Timestamps deliberately disagree with log order to prove sequence wins.
        let events = vec![
            pass("vd", "A", 6_000_000, Some(3)),
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
        ];
        let result = lap_list(&events);
        assert_eq!(
            result.competitor(&key("vd", "A")).unwrap().laps,
            vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000, // 4.0s - 1.0s
                },
                Lap {
                    number: 2,
                    duration_micros: 2_000_000, // 6.0s - 4.0s
                },
            ]
        );
    }

    #[test]
    fn passes_without_sequence_are_ordered_by_timestamp() {
        // No sequence anywhere: fall back to `at` ascending despite log order.
        let events = vec![
            pass("vd", "A", 7_000_000, None),
            pass("vd", "A", 2_000_000, None),
            pass("vd", "A", 5_000_000, None),
        ];
        let result = lap_list(&events);
        assert_eq!(
            result.competitor(&key("vd", "A")).unwrap().laps,
            vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000, // 5.0s - 2.0s
                },
                Lap {
                    number: 2,
                    duration_micros: 2_000_000, // 7.0s - 5.0s
                },
            ]
        );
    }

    #[test]
    fn lap_list_serde_round_trips() {
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 3_500_000, Some(2)),
        ];
        let result = lap_list(&events);
        let json = serde_json::to_string(&result).unwrap();
        let back: LapList = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
    }
}

/// Marshaling fold golden cases (#31): hand-authored `(offset, Event)` logs with
/// explicit offsets, one per adjudication, asserting the corrected [`LapList`].
#[cfg(test)]
mod marshaling_tests {
    use super::*;
    use gridfpv_events::{GateIndex, HeatId, LogRef, Penalty};

    /// Build a lap-gate pass event.
    fn pass(adapter: &str, competitor: &str, at: i64, sequence: Option<u64>) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    fn voided(target: u64) -> Event {
        Event::DetectionVoided {
            target: LogRef(target),
        }
    }

    fn inserted(adapter: &str, competitor: &str, at: i64) -> Event {
        Event::LapInserted {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
        }
    }

    fn adjusted(target: u64, at: i64) -> Event {
        Event::LapAdjusted {
            target: LogRef(target),
            at: SourceTime::from_micros(at),
        }
    }

    fn key(adapter: &str, competitor: &str) -> CompetitorKey {
        CompetitorKey {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
        }
    }

    /// Tag a log with positional offsets (0, 1, 2, …) — the storage layer assigns
    /// the same dense append offsets, so this mirrors a real on-disk log.
    fn tagged(events: &[Event]) -> Vec<(u64, &Event)> {
        events
            .iter()
            .enumerate()
            .map(|(i, e)| (i as u64, e))
            .collect()
    }

    fn laps_of(list: &LapList, adapter: &str, competitor: &str) -> Vec<Lap> {
        list.competitor(&key(adapter, competitor))
            .map(|c| c.laps.clone())
            .unwrap_or_default()
    }

    #[test]
    fn no_adjudications_matches_lap_list() {
        // With no rulings, `lap_list_marshaled` projects identically to `lap_list`.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "A", 6_500_000, Some(3)),
        ];
        assert_eq!(lap_list_marshaled(tagged(&events)), lap_list(&events));
    }

    #[test]
    fn detection_voided_drops_the_targeted_pass() {
        // Three raw passes; the middle one (offset 1) is a phantom and is voided.
        // The corrected view is just passes 0 and 2 ⇒ a single lap spanning them.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1 — phantom
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
            voided(1),                           // offset 3 — voids the phantom
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![Lap {
                number: 1,
                duration_micros: 5_000_000, // 6.0s - 1.0s, the 4.0s pass is gone
            }]
        );
    }

    #[test]
    fn lap_inserted_adds_a_synthetic_pass() {
        // Two raw passes; a missed lap is recovered by inserting a pass between them.
        // The synthetic pass at 4.0s splits the 6.0s span into two laps.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1
            inserted("vd", "A", 4_000_000),      // offset 2 — recovered lap
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000, // 4.0s - 1.0s
                },
                Lap {
                    number: 2,
                    duration_micros: 3_000_000, // 7.0s - 4.0s
                },
            ]
        );
    }

    #[test]
    fn lap_adjusted_retimes_the_targeted_pass() {
        // The middle pass was detected late; re-time offset 1 from 5.0s to 4.0s.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1 — detected late
            pass("vd", "A", 7_000_000, Some(3)), // offset 2
            adjusted(1, 4_000_000),              // offset 3 — re-time to 4.0s
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000, // 4.0s - 1.0s (was 4.0s before adjust)
                },
                Lap {
                    number: 2,
                    duration_micros: 3_000_000, // 7.0s - 4.0s (was 2.0s before adjust)
                },
            ]
        );
    }

    #[test]
    fn last_writer_wins_void_supersedes_adjust() {
        // offset 1 is adjusted (offset 3) and then voided (offset 4): the later void
        // wins, so the pass is gone entirely — not merely re-timed.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1
            pass("vd", "A", 8_000_000, Some(3)), // offset 2
            adjusted(1, 4_000_000),              // offset 3 — re-time...
            voided(1),                           // offset 4 — ...then void (wins)
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![Lap {
                number: 1,
                duration_micros: 7_000_000, // 8.0s - 1.0s, offset 1 voided
            }]
        );
    }

    #[test]
    fn void_the_void_resurrects_the_original_pass() {
        // architecture.html §3 "void the void": offset 1 is voided (offset 3), then a
        // marshal realises that was wrong and voids *the void* (offset 4) — the
        // originally-voided pass comes back, so all three laps' passes are present.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
            voided(1),                           // offset 3 — void the pass
            voided(3),                           // offset 4 — void the void
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![
                Lap {
                    number: 1,
                    duration_micros: 3_000_000, // 4.0s - 1.0s — pass 1 is back
                },
                Lap {
                    number: 2,
                    duration_micros: 2_000_000, // 6.0s - 4.0s
                },
            ]
        );
    }

    #[test]
    fn voiding_an_insert_removes_the_synthetic_pass() {
        // A void may target a `LapInserted` (an adjudication) — voiding offset 2
        // removes the synthetic lap again, leaving only the two raw passes.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1
            inserted("vd", "A", 4_000_000),      // offset 2 — synthetic
            voided(2),                           // offset 3 — void the insert
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![Lap {
                number: 1,
                duration_micros: 6_000_000, // 7.0s - 1.0s, the insert is gone
            }]
        );
    }

    #[test]
    fn voiding_an_adjust_reverts_to_the_raw_timestamp() {
        // A void targeting a `LapAdjusted` cancels the re-time: the target raw pass
        // reverts to its original timestamp rather than being dropped.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1 — original 5.0s
            adjusted(1, 4_000_000),              // offset 2 — re-time to 4.0s...
            voided(2),                           // offset 3 — ...cancel the re-time
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![Lap {
                number: 1,
                duration_micros: 4_000_000, // 5.0s - 1.0s, reverted from the 4.0s adjust
            }]
        );
    }

    #[test]
    fn heat_and_result_level_rulings_are_ignored_by_the_lap_view() {
        // `HeatVoided` / `PenaltyApplied` are result-level — scoring consumes them,
        // not the lap list. They must not perturb the lap projection.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            Event::HeatVoided {
                heat: HeatId("q-1".into()),
            },
            Event::PenaltyApplied {
                heat: HeatId("q-1".into()),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::TimeAdded { micros: 2_000_000 },
            },
        ];
        assert_eq!(lap_list_marshaled(tagged(&events)), lap_list(&events));
        assert_eq!(
            laps_of(&lap_list_marshaled(tagged(&events)), "vd", "A"),
            vec![Lap {
                number: 1,
                duration_micros: 3_000_000,
            }]
        );
    }

    #[test]
    fn fold_is_idempotent_recompute_equivalence() {
        // Folding the same log twice yields the same result — the projection is a
        // pure function of the log with no hidden state (recompute-equivalence).
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 5_000_000, Some(2)),
            pass("vd", "A", 8_000_000, Some(3)),
            adjusted(1, 4_000_000),
            voided(2),
            inserted("vd", "A", 9_000_000),
        ];
        let first = lap_list_marshaled(tagged(&events));
        let second = lap_list_marshaled(tagged(&events));
        assert_eq!(first, second);
    }

    #[test]
    fn raw_passes_are_byte_identical_before_and_after_folding() {
        // The fold builds a *corrected view*; it must never mutate the raw log. We
        // snapshot the raw `Pass`es' serialized bytes, fold (with adjudications), and
        // assert every raw pass round-trips byte-for-byte unchanged.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 5_000_000, Some(2)),
            pass("vd", "A", 8_000_000, Some(3)),
            adjusted(1, 4_000_000), // would "change" a pass if we mutated
            voided(2),              // would "drop" a pass if we mutated
        ];
        let before: Vec<String> = events
            .iter()
            .filter(|e| matches!(e, Event::Pass(_)))
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();

        // Fold — and use the result so the corrected view genuinely differs.
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![Lap {
                number: 1,
                duration_micros: 3_000_000, // 4.0s - 1.0s (adjusted), offset 2 voided
            }]
        );

        let after: Vec<String> = events
            .iter()
            .filter(|e| matches!(e, Event::Pass(_)))
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        assert_eq!(
            before, after,
            "raw passes must be byte-identical after folding"
        );
    }

    #[test]
    fn adjust_targeting_a_synthetic_inserted_pass_retimes_it() {
        // An inserted lap is itself addressable; a later adjust re-times it.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 9_000_000, Some(2)), // offset 1
            inserted("vd", "A", 4_000_000),      // offset 2 — synthetic at 4.0s
            adjusted(2, 5_000_000),              // offset 3 — re-time insert to 5.0s
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![
                Lap {
                    number: 1,
                    duration_micros: 4_000_000, // 5.0s - 1.0s
                },
                Lap {
                    number: 2,
                    duration_micros: 4_000_000, // 9.0s - 5.0s
                },
            ]
        );
    }
}
