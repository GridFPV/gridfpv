//! Laps: what a race's crossings add up to, per seat (`docs/primitives.md` §3.3, §5.7).
//!
//! A timer reports **crossings** (passes). A lap is the time between two crossings in a row
//! on one seat, measured on the timer's own clock. The first crossing a seat makes in a race
//! is its **hole shot**: it starts the first lap and has no lap time of its own.
//!
//! On top of the raw crossings sit two things:
//!
//! - **Lap filters**, from the race rules. A crossing closer than the minimum lap time to
//!   the one before it is filtered out, the same way a removal would take it out.
//! - **Rulings**, made by a person and logged:
//!   - *Remove*: the crossing is ignored. Take laps A, B, C; removing B leaves A as it was,
//!     and C now starts where A ended. The reason is a label and changes nothing.
//!   - *Restart*: the lap the crossing ended has no lap time. A and C keep theirs, but they
//!     can never be used together: a run of consecutive laps may not span a restart.
//!   - *Insert*: a crossing the timer missed. Inside a lap, this splits it.
//!   - *Adjust*: move a crossing in time.
//!
//! [`RaceLaps`] is the fold. It is pure and total, like [`crate::race::Races`].

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{AdapterId, Event, LogRef, RaceId, RemovalReason, SeatRef, SourceTime};

/// The lap filters a race runs under. They come from the race rules of the format in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LapFilters {
    /// The shortest time a lap may take, in microseconds. A crossing that follows the
    /// counted crossing before it by less than this is filtered out. `None` filters nothing.
    pub min_lap_micros: Option<i64>,
}

/// One timed interval between two counted crossings on a seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lap {
    /// The crossing that started it.
    pub start: LogRef,
    /// The crossing that ended it. Rulings about this lap target this crossing.
    pub end: LogRef,
    /// When it started, on the timer's clock.
    pub started_at: SourceTime,
    /// When it ended, on the timer's clock.
    pub ended_at: SourceTime,
    /// Whether this interval is a lap or a marked restart.
    pub kind: LapKind,
}

impl Lap {
    /// How long the interval took, in microseconds on the timer's clock.
    pub fn micros(&self) -> i64 {
        self.ended_at.micros_since(self.started_at)
    }

    /// Whether this is a lap that counts (not a restart).
    pub fn is_lap(&self) -> bool {
        self.kind == LapKind::Lap
    }
}

/// What an interval between two crossings is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LapKind {
    /// A lap. It has a lap time and counts.
    Lap,
    /// A restart: the pilot flew back through the gate. It has no lap time and does not
    /// count, and consecutive laps may not span it.
    Restart,
}

/// Why a crossing does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uncounted {
    /// A person removed it, with the label they picked, if any.
    Removed(Option<RemovalReason>),
    /// The minimum lap time filtered it out.
    UnderMinLap,
}

/// A crossing that was left out, and why. Kept so the marshal can see and restore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UncountedCrossing {
    pub crossing: LogRef,
    pub at: SourceTime,
    pub why: Uncounted,
}

/// One seat's laps in one race.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SeatLaps {
    /// The seat's first counted crossing: it starts lap 1.
    pub hole_shot: Option<(LogRef, SourceTime)>,
    /// Every interval after the hole shot, in order: laps and marked restarts.
    pub intervals: Vec<Lap>,
    /// Crossings that were removed or filtered, in time order.
    pub uncounted: Vec<UncountedCrossing>,
}

impl SeatLaps {
    /// The laps that count, in order (restarts left out).
    pub fn laps(&self) -> impl Iterator<Item = &Lap> {
        self.intervals.iter().filter(|lap| lap.is_lap())
    }

    /// How many laps count.
    pub fn lap_count(&self) -> usize {
        self.laps().count()
    }

    /// The fastest single lap, in microseconds.
    pub fn best_lap_micros(&self) -> Option<i64> {
        self.laps().map(Lap::micros).min()
    }

    /// The time from the hole shot to the last counted crossing, in microseconds. This
    /// includes any restart intervals: it is time on the clock, not a sum of laps.
    pub fn total_micros(&self) -> Option<i64> {
        let (_, start) = self.hole_shot?;
        let last = self.intervals.last()?;
        Some(last.ended_at.micros_since(start))
    }

    /// The fastest run of `n` laps in a row, in microseconds. A run may not span a restart.
    /// `None` when there is no such run (including `n == 0`).
    pub fn best_consecutive_micros(&self, n: usize) -> Option<i64> {
        if n == 0 {
            return None;
        }
        let mut best: Option<i64> = None;
        // Each stretch between restarts is searched on its own.
        for stretch in self.intervals.split(|lap| !lap.is_lap()) {
            for window in stretch.windows(n) {
                let total: i64 = window.iter().map(Lap::micros).sum();
                best = Some(best.map_or(total, |b| b.min(total)));
            }
        }
        best
    }
}

/// One crossing as the fold tracks it: where it sits and what has been ruled about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Crossing {
    race: RaceId,
    adapter: AdapterId,
    seat: SeatRef,
    at: SourceTime,
    removed: Option<Option<RemovalReason>>,
    restart: bool,
}

/// Every lap-gate crossing that belongs to a race, with the rulings made about them.
///
/// Feed it the whole log with [`RaceLaps::apply`]; ask for a seat's laps with
/// [`RaceLaps::seat`]. Lap filters are given at read time, because they are rules, not facts:
/// the same crossings read under a different minimum lap give different laps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RaceLaps {
    crossings: BTreeMap<LogRef, Crossing>,
}

impl RaceLaps {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a whole log. Each item is an event and the log offset it was stored at.
    pub fn fold<'a>(events: impl IntoIterator<Item = (u64, &'a Event)>) -> Self {
        let mut laps = Self::new();
        for (offset, event) in events {
            laps.apply(offset, event);
        }
        laps
    }

    /// Apply one logged event, stored at `offset`. Events that are not crossings or lap
    /// rulings are ignored, and so is a ruling whose target is not a crossing of the race it
    /// names: the fold never panics and never rejects.
    pub fn apply(&mut self, offset: u64, event: &Event) {
        match event {
            // Only lap-gate passes stamped with a race become crossings. A pass logged
            // while no race was running belongs to no race.
            Event::Pass(pass) if pass.gate.is_lap_gate() => {
                if let Some(race) = &pass.race {
                    self.crossings.insert(
                        LogRef(offset),
                        Crossing {
                            race: race.clone(),
                            adapter: pass.adapter.clone(),
                            seat: pass.seat.clone(),
                            at: pass.at,
                            removed: None,
                            restart: false,
                        },
                    );
                }
            }
            Event::LapInserted {
                race,
                adapter,
                seat,
                at,
            } => {
                self.crossings.insert(
                    LogRef(offset),
                    Crossing {
                        race: race.clone(),
                        adapter: adapter.clone(),
                        seat: seat.clone(),
                        at: *at,
                        removed: None,
                        restart: false,
                    },
                );
            }
            Event::LapRemoved {
                race,
                crossing,
                reason,
                ..
            } => {
                if let Some(found) = self.target(race, crossing) {
                    found.removed = Some(*reason);
                }
            }
            Event::LapRestored { race, crossing } => {
                if let Some(found) = self.target(race, crossing) {
                    found.removed = None;
                }
            }
            Event::LapAdjusted { race, crossing, at } => {
                if let Some(found) = self.target(race, crossing) {
                    found.at = *at;
                }
            }
            Event::RestartMarked { race, crossing } => {
                if let Some(found) = self.target(race, crossing) {
                    found.restart = true;
                }
            }
            Event::RestartUnmarked { race, crossing } => {
                if let Some(found) = self.target(race, crossing) {
                    found.restart = false;
                }
            }
            _ => {}
        }
    }

    /// The crossing a ruling targets, if it exists and belongs to the race the ruling names.
    fn target(&mut self, race: &RaceId, crossing: &LogRef) -> Option<&mut Crossing> {
        self.crossings
            .get_mut(crossing)
            .filter(|found| &found.race == race)
    }

    /// Whether `crossing` is a crossing of `race` — what a ruling must target.
    pub fn is_crossing_of(&self, race: &RaceId, crossing: &LogRef) -> bool {
        self.crossings
            .get(crossing)
            .is_some_and(|found| &found.race == race)
    }

    /// The seats that made at least one crossing in a race.
    pub fn seats(&self, race: &RaceId) -> BTreeSet<(AdapterId, SeatRef)> {
        self.crossings
            .values()
            .filter(|c| &c.race == race)
            .map(|c| (c.adapter.clone(), c.seat.clone()))
            .collect()
    }

    /// One seat's laps in one race, read under the given lap filters.
    pub fn seat(
        &self,
        race: &RaceId,
        adapter: &AdapterId,
        seat: &SeatRef,
        filters: LapFilters,
    ) -> SeatLaps {
        // The seat's crossings in time order. Two at the same instant keep log order.
        let mut crossings: Vec<(&LogRef, &Crossing)> = self
            .crossings
            .iter()
            .filter(|(_, c)| &c.race == race && &c.adapter == adapter && &c.seat == seat)
            .collect();
        crossings.sort_by_key(|(offset, c)| (c.at.micros, **offset));

        let mut out = SeatLaps::default();
        // The last crossing that counted: where the next lap starts.
        let mut previous: Option<(LogRef, SourceTime)> = None;
        for (offset, crossing) in crossings {
            if let Some(reason) = crossing.removed {
                out.uncounted.push(UncountedCrossing {
                    crossing: *offset,
                    at: crossing.at,
                    why: Uncounted::Removed(reason),
                });
                continue;
            }
            let Some((start, started_at)) = previous else {
                out.hole_shot = Some((*offset, crossing.at));
                previous = Some((*offset, crossing.at));
                continue;
            };
            let too_short = filters
                .min_lap_micros
                .is_some_and(|min| crossing.at.micros_since(started_at) < min);
            // A marked restart is a person's call and is never filtered away.
            if too_short && !crossing.restart {
                out.uncounted.push(UncountedCrossing {
                    crossing: *offset,
                    at: crossing.at,
                    why: Uncounted::UnderMinLap,
                });
                continue;
            }
            out.intervals.push(Lap {
                start,
                end: *offset,
                started_at,
                ended_at: crossing.at,
                kind: if crossing.restart {
                    LapKind::Restart
                } else {
                    LapKind::Lap
                },
            });
            previous = Some((*offset, crossing.at));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{GateIndex, Pass};

    const S: i64 = 1_000_000;

    fn race() -> RaceId {
        RaceId("r1".into())
    }

    fn pass_on(race: Option<&str>, seat: &str, at: i64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId("oft".into()),
            seat: SeatRef(seat.into()),
            at: SourceTime::from_micros(at),
            sequence: None,
            gate: GateIndex::LAP,
            signal: None,
            race: race.map(|r| RaceId(r.into())),
        })
    }

    fn pass(at: i64) -> Event {
        pass_on(Some("r1"), "seat-0", at)
    }

    /// Fold events stored at offsets 0, 1, 2, … and read seat-0 of race r1.
    fn read(events: &[Event], filters: LapFilters) -> SeatLaps {
        let laps = RaceLaps::fold(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
        laps.seat(
            &race(),
            &AdapterId("oft".into()),
            &SeatRef("seat-0".into()),
            filters,
        )
    }

    fn times(laps: &SeatLaps) -> Vec<i64> {
        laps.laps().map(Lap::micros).collect()
    }

    fn none() -> LapFilters {
        LapFilters::default()
    }

    #[test]
    fn laps_are_the_time_between_crossings_and_the_first_is_the_hole_shot() {
        let laps = read(
            &[pass(2 * S), pass(12 * S), pass(21 * S), pass(31 * S)],
            none(),
        );
        assert_eq!(
            laps.hole_shot,
            Some((LogRef(0), SourceTime::from_micros(2 * S)))
        );
        assert_eq!(times(&laps), vec![10 * S, 9 * S, 10 * S]);
        assert_eq!(laps.lap_count(), 3);
        assert_eq!(laps.best_lap_micros(), Some(9 * S));
        assert_eq!(laps.total_micros(), Some(29 * S));
    }

    #[test]
    fn a_seat_with_one_crossing_has_a_hole_shot_and_no_laps() {
        let laps = read(&[pass(2 * S)], none());
        assert!(laps.hole_shot.is_some());
        assert_eq!(laps.lap_count(), 0);
        assert_eq!(laps.best_lap_micros(), None);
        assert_eq!(laps.total_micros(), None);
    }

    #[test]
    fn lap_times_come_from_the_timers_clock_not_log_order() {
        // The same crossings logged out of order (a catch-up after a dropped connection).
        let laps = read(&[pass(21 * S), pass(2 * S), pass(12 * S)], none());
        assert_eq!(times(&laps), vec![10 * S, 9 * S]);
    }

    #[test]
    fn removing_lap_b_leaves_a_alone_and_folds_bs_time_into_c() {
        // Laps A (10 s), B (4 s), C (9 s). Lap B is ended by the crossing at offset 2.
        let mut events = vec![pass(0), pass(10 * S), pass(14 * S), pass(23 * S)];
        assert_eq!(times(&read(&events, none())), vec![10 * S, 4 * S, 9 * S]);

        events.push(Event::LapRemoved {
            race: race(),
            crossing: LogRef(2),
            reason: Some(RemovalReason::FalseDetection),
            note: None,
        });
        let laps = read(&events, none());
        // A is untouched; C now starts where A ended, so B's time is part of C.
        assert_eq!(times(&laps), vec![10 * S, 13 * S]);
        assert_eq!(
            laps.uncounted,
            vec![UncountedCrossing {
                crossing: LogRef(2),
                at: SourceTime::from_micros(14 * S),
                why: Uncounted::Removed(Some(RemovalReason::FalseDetection)),
            }]
        );
    }

    #[test]
    fn the_removal_reason_never_changes_the_result() {
        let base = vec![pass(0), pass(10 * S), pass(14 * S), pass(23 * S)];
        let mut results = Vec::new();
        for reason in [
            None,
            Some(RemovalReason::FalseDetection),
            Some(RemovalReason::IncorrectLap),
            Some(RemovalReason::Other),
        ] {
            let mut events = base.clone();
            events.push(Event::LapRemoved {
                race: race(),
                crossing: LogRef(2),
                reason,
                note: None,
            });
            results.push(times(&read(&events, none())));
        }
        assert!(results.iter().all(|r| *r == vec![10 * S, 13 * S]));
    }

    #[test]
    fn a_removal_can_be_undone() {
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(14 * S),
            pass(23 * S),
            Event::LapRemoved {
                race: race(),
                crossing: LogRef(2),
                reason: None,
                note: None,
            },
            Event::LapRestored {
                race: race(),
                crossing: LogRef(2),
            },
        ];
        let laps = read(&events, none());
        assert_eq!(times(&laps), vec![10 * S, 4 * S, 9 * S]);
        assert!(laps.uncounted.is_empty());
    }

    #[test]
    fn removing_the_hole_shot_makes_the_next_crossing_the_hole_shot() {
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(19 * S),
            Event::LapRemoved {
                race: race(),
                crossing: LogRef(0),
                reason: None,
                note: None,
            },
        ];
        let laps = read(&events, none());
        assert_eq!(
            laps.hole_shot,
            Some((LogRef(1), SourceTime::from_micros(10 * S)))
        );
        assert_eq!(times(&laps), vec![9 * S]);
    }

    #[test]
    fn a_restart_has_no_lap_time_and_a_and_c_both_stand() {
        // Laps A (10 s), B (4 s, the fly-back), C (9 s).
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(14 * S),
            pass(23 * S),
            Event::RestartMarked {
                race: race(),
                crossing: LogRef(2),
            },
        ];
        let laps = read(&events, none());
        // A and C keep their own times. B is not a lap.
        assert_eq!(times(&laps), vec![10 * S, 9 * S]);
        assert_eq!(laps.lap_count(), 2);
        assert_eq!(laps.intervals[1].kind, LapKind::Restart);
        // Time on the clock still runs through the restart.
        assert_eq!(laps.total_micros(), Some(23 * S));
    }

    #[test]
    fn consecutive_laps_may_not_span_a_restart() {
        // 10, 9, [restart 4], 8, 7, 11
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(19 * S),
            pass(23 * S),
            pass(31 * S),
            pass(38 * S),
            pass(49 * S),
            Event::RestartMarked {
                race: race(),
                crossing: LogRef(3),
            },
        ];
        let laps = read(&events, none());
        assert_eq!(times(&laps), vec![10 * S, 9 * S, 8 * S, 7 * S, 11 * S]);
        // Best two in a row: 8 + 7, inside the second stretch.
        assert_eq!(laps.best_consecutive_micros(2), Some(15 * S));
        // Best three in a row can only come from the second stretch: 8 + 7 + 11. The
        // faster 9 + 8 + 7 would span the restart, so it is not allowed.
        assert_eq!(laps.best_consecutive_micros(3), Some(26 * S));
        // No stretch has four laps.
        assert_eq!(laps.best_consecutive_micros(4), None);
        assert_eq!(laps.best_consecutive_micros(0), None);
    }

    #[test]
    fn a_restart_mark_can_be_undone() {
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(14 * S),
            Event::RestartMarked {
                race: race(),
                crossing: LogRef(2),
            },
            Event::RestartUnmarked {
                race: race(),
                crossing: LogRef(2),
            },
        ];
        assert_eq!(times(&read(&events, none())), vec![10 * S, 4 * S]);
    }

    #[test]
    fn best_consecutive_with_no_restart_is_the_best_window() {
        let laps = read(
            &[
                pass(0),
                pass(10 * S),
                pass(18 * S),
                pass(27 * S),
                pass(39 * S),
            ],
            none(),
        );
        assert_eq!(times(&laps), vec![10 * S, 8 * S, 9 * S, 12 * S]);
        assert_eq!(laps.best_consecutive_micros(1), Some(8 * S));
        assert_eq!(laps.best_consecutive_micros(3), Some(27 * S));
    }

    #[test]
    fn an_inserted_crossing_splits_a_lap_and_can_itself_be_removed() {
        // The timer missed a crossing: one 20 s "lap" is really two.
        let mut events = vec![
            pass(0),
            pass(20 * S),
            Event::LapInserted {
                race: race(),
                adapter: AdapterId("oft".into()),
                seat: SeatRef("seat-0".into()),
                at: SourceTime::from_micros(11 * S),
            },
        ];
        assert_eq!(times(&read(&events, none())), vec![11 * S, 9 * S]);
        // The inserted crossing is targeted by the offset of the fact that made it.
        events.push(Event::LapRemoved {
            race: race(),
            crossing: LogRef(2),
            reason: None,
            note: None,
        });
        assert_eq!(times(&read(&events, none())), vec![20 * S]);
    }

    #[test]
    fn adjusting_a_crossing_moves_the_laps_either_side() {
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(20 * S),
            Event::LapAdjusted {
                race: race(),
                crossing: LogRef(1),
                at: SourceTime::from_micros(9 * S),
            },
        ];
        assert_eq!(times(&read(&events, none())), vec![9 * S, 11 * S]);
    }

    #[test]
    fn the_minimum_lap_filters_a_crossing_the_same_way_a_removal_would() {
        // A double-detection 0.5 s after a real crossing.
        let events = vec![pass(0), pass(10 * S), pass(10 * S + S / 2), pass(19 * S)];
        let filters = LapFilters {
            min_lap_micros: Some(2 * S),
        };
        let laps = read(&events, filters);
        assert_eq!(times(&laps), vec![10 * S, 9 * S]);
        assert_eq!(laps.uncounted.len(), 1);
        assert_eq!(laps.uncounted[0].why, Uncounted::UnderMinLap);
        // The filter is a rule, not a fact: read with no filter, the crossing counts.
        assert_eq!(
            times(&read(&events, none())),
            vec![10 * S, S / 2, 8 * S + S / 2]
        );
    }

    #[test]
    fn the_minimum_lap_is_measured_from_the_last_crossing_that_counted() {
        // Three quick bounces after a real crossing: each is under 2 s from the one that
        // counted (10 s), not merely from the bounce before it.
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(10 * S + 800_000),
            pass(10 * S + 1_600_000),
            pass(10 * S + 2_400_000),
        ];
        let filters = LapFilters {
            min_lap_micros: Some(2 * S),
        };
        let laps = read(&events, filters);
        assert_eq!(times(&laps), vec![10 * S, 2_400_000]);
        assert_eq!(laps.uncounted.len(), 2);
    }

    #[test]
    fn a_marked_restart_is_never_filtered_away() {
        // A fast fly-back under the minimum lap, marked as a restart by the marshal.
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(11 * S),
            pass(20 * S),
            Event::RestartMarked {
                race: race(),
                crossing: LogRef(2),
            },
        ];
        let filters = LapFilters {
            min_lap_micros: Some(2 * S),
        };
        let laps = read(&events, filters);
        assert_eq!(laps.intervals[1].kind, LapKind::Restart);
        assert_eq!(times(&laps), vec![10 * S, 9 * S]);
    }

    #[test]
    fn passes_outside_the_race_and_on_other_seats_are_left_out() {
        let events = vec![
            pass_on(None, "seat-0", 0),       // no race was running
            pass_on(Some("r0"), "seat-0", S), // an earlier race
            pass(2 * S),
            pass_on(Some("r1"), "seat-1", 3 * S),
            pass(12 * S),
        ];
        let laps = read(&events, none());
        assert_eq!(times(&laps), vec![10 * S]);
        let fold = RaceLaps::fold(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
        assert_eq!(fold.seats(&race()).len(), 2);
    }

    #[test]
    fn a_split_gate_pass_is_not_a_lap_crossing() {
        let mut split = match pass(5 * S) {
            Event::Pass(p) => p,
            _ => unreachable!(),
        };
        split.gate = GateIndex(1);
        let laps = read(&[pass(0), Event::Pass(split), pass(10 * S)], none());
        assert_eq!(times(&laps), vec![10 * S]);
    }

    #[test]
    fn a_ruling_that_names_the_wrong_race_or_no_crossing_changes_nothing() {
        let events = vec![
            pass(0),
            pass(10 * S),
            pass(19 * S),
            // Right crossing, wrong race.
            Event::LapRemoved {
                race: RaceId("other".into()),
                crossing: LogRef(1),
                reason: None,
                note: None,
            },
            // No such crossing.
            Event::RestartMarked {
                race: race(),
                crossing: LogRef(99),
            },
        ];
        let fold = RaceLaps::fold(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
        assert!(fold.is_crossing_of(&race(), &LogRef(1)));
        assert!(!fold.is_crossing_of(&RaceId("other".into()), &LogRef(1)));
        assert!(!fold.is_crossing_of(&race(), &LogRef(99)));
        assert_eq!(times(&read(&events, none())), vec![10 * S, 9 * S]);
    }
}
