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
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::{AdapterId, CompetitorRef, Event, Pass, SourceTime};
use serde::{Deserialize, Serialize};

/// Identifies a competitor *within a single timing source*.
///
/// A source-local [`CompetitorRef`] is only meaningful relative to the adapter
/// that emitted it (node 2 on RotorHazard is unrelated to node 2 on a second
/// timer), so laps are grouped on the `(AdapterId, CompetitorRef)` pair. Binding
/// these per-source competitors to a single GridFPV pilot is a later registration
/// concern (Architecture §9) and deliberately out of scope here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lap {
    /// 1-based lap number within the competitor's run.
    pub number: u32,
    /// Lap duration in microseconds on the source clock
    /// (`pass[n + 1].at - pass[n].at`). Always `>= 0` for in-order passes.
    pub duration_micros: i64,
}

/// Every lap a single competitor completed, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
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
/// Within a competitor, passes are ordered by `sequence` when present, else by
/// `at` (source timestamp). The sort key is `(sequence_present, sequence, at)`:
///
/// - Passes that carry a `sequence` sort ahead of those that do not, then by
///   `sequence` ascending — the source's monotonic counter is authoritative when
///   it exists and survives clock adjustments.
/// - Passes without a `sequence` fall back to `at` ascending.
/// - `at` is the final tie-break in every case, so equal keys stay deterministic.
///
/// The sort is *stable*, so passes with fully equal keys keep their original log
/// order. Mixing sequenced and unsequenced passes for one competitor is not
/// expected from a real source (a source either numbers its passes or it does
/// not); the rule above just keeps the fold total and deterministic.
///
/// Accepts anything iterable over `&Event` (e.g. `&[Event]`), so it is decoupled
/// from storage and trivially testable.
///
/// [`is_lap_gate`]: gridfpv_events::GateIndex::is_lap_gate
pub fn lap_list<'a, I>(events: I) -> LapList
where
    I: IntoIterator<Item = &'a Event>,
{
    // Group lap-gate passes by competitor. BTreeMap keeps competitors in a
    // deterministic key order independent of arrival order.
    let mut by_competitor: BTreeMap<CompetitorKey, Vec<&Pass>> = BTreeMap::new();
    for event in events {
        if let Event::Pass(pass) = event {
            if pass.gate.is_lap_gate() {
                by_competitor
                    .entry(CompetitorKey::from_pass(pass))
                    .or_default()
                    .push(pass);
            }
        }
    }

    let competitors = by_competitor
        .into_iter()
        .map(|(competitor, mut passes)| {
            passes.sort_by_key(|p| pass_order_key(p));
            CompetitorLaps {
                competitor,
                laps: laps_from_passes(&passes),
            }
        })
        .collect();

    LapList { competitors }
}

/// Ordering key for a pass: sequenced passes first (ascending sequence), then
/// unsequenced ones, with `at` as the final tie-break. See [`lap_list`].
fn pass_order_key(pass: &Pass) -> (bool, Option<u64>, SourceTime) {
    // `sequence.is_none()` is `false` (sorts first) for sequenced passes.
    (pass.sequence.is_none(), pass.sequence, pass.at)
}

/// Turn an ordered run of lap-gate passes into laps: `K` passes ⇒ `K - 1` laps,
/// each spanning a consecutive pair.
fn laps_from_passes(passes: &[&Pass]) -> Vec<Lap> {
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
