//! The heat: a lineup that is raced one or more times, and what its saved races add up to
//! (`docs/primitives.md` §5.4, §5.5).
//!
//! Three pure steps, each built on the one before:
//!
//! 1. [`race_finish`] — the finishing order of one race, from each pilot's laps.
//! 2. [`heat_standings`] — every pilot's measures across the heat's saved races, sorted by
//!    the format's ranking rule into a strict order of the whole field.
//! 3. [`heat_complete`] — whether the format's completion rule says the heat is done.
//!
//! The rules themselves are data ([`gridfpv_events::RankingRule`],
//! [`gridfpv_events::CompletionRule`]); nothing here knows what "Chase the Ace" is.

use std::cmp::Ordering;

use gridfpv_events::{CompletionRule, EndCondition, Format, Measure, PilotId, RankingRule};

use crate::laps::SeatLaps;

/// One pilot's laps in one saved race.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RaceEntry {
    pub pilot: PilotId,
    pub laps: SeatLaps,
}

/// One pilot's line in the finishing order of one race.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    pub pilot: PilotId,
    /// 1 for the winner. Every pilot in the race has a different position.
    pub position: u32,
    /// The laps that counted toward the finish (capped at the target in a first-to-N race).
    pub laps: u32,
}

/// The finishing order of one race.
///
/// - **First to N laps:** pilots who finished N laps, in the order they finished the Nth.
///   Laps flown after the Nth do not count. Then everyone else, as below.
/// - **Timed or open:** more laps first; on equal laps, whoever finished their last lap
///   first.
/// - A pilot with a hole shot and no lap goes below every pilot with a lap; a pilot with no
///   crossing at all goes last. Any tie left is broken by `seed_of` (lower is better), so
///   the order is always strict.
///
/// Times are compared on the timer's clock, so this assumes the race's seats are on one
/// timer.
pub fn race_finish(
    entries: &[RaceEntry],
    end: &EndCondition,
    seed_of: impl Fn(&PilotId) -> usize,
) -> Vec<Finish> {
    let target = match end {
        EndCondition::FirstToLaps(n) => Some(*n as usize),
        EndCondition::Timed { .. } | EndCondition::Open => None,
    };
    // (laps counted, when the last counted lap ended, made any crossing, seed, pilot)
    let mut lines: Vec<(usize, Option<i64>, bool, usize, &PilotId)> = entries
        .iter()
        .map(|entry| {
            let ends: Vec<i64> = entry.laps.laps().map(|lap| lap.ended_at.micros).collect();
            let counted = target.map_or(ends.len(), |n| ends.len().min(n));
            let finished_at = counted.checked_sub(1).map(|last| ends[last]);
            let crossed = entry.laps.hole_shot.is_some();
            (
                counted,
                finished_at,
                crossed,
                seed_of(&entry.pilot),
                &entry.pilot,
            )
        })
        .collect();
    lines.sort_by(|a, b| {
        b.0.cmp(&a.0) // more laps first
            .then_with(|| a.1.cmp(&b.1)) // then the earlier finish (both Some when laps > 0)
            .then_with(|| b.2.cmp(&a.2)) // then a pilot who at least crossed
            .then_with(|| a.3.cmp(&b.3)) // then seed
    });
    lines
        .into_iter()
        .enumerate()
        .map(|(index, (laps, _, _, _, pilot))| Finish {
            pilot: pilot.clone(),
            position: index as u32 + 1,
            laps: laps as u32,
        })
        .collect()
}

/// One pilot's place in a heat, with the numbers behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub pilot: PilotId,
    /// 1 for the top of the heat. Every pilot in the field has a different position.
    pub position: u32,
    /// The pilot's place in the heat's field, 0 first. The last tie-break.
    pub seed: usize,
    /// Whether the pilot made at least one crossing in a saved race of this heat. A pilot
    /// who never flew ranks below everyone who did, whatever their seed.
    pub flew: bool,
    pub wins: u32,
    pub points: u32,
    /// The most laps in one race.
    pub laps: u32,
    pub best_lap_micros: Option<i64>,
    pub best_finish: Option<u32>,
    pub last_finish: Option<u32>,
    /// The pilot's laps in each saved race they were in, oldest first. Kept so a measure
    /// such as "best 3 consecutive" can be read for any N.
    pub races: Vec<SeatLaps>,
}

impl Standing {
    /// The pilot's value for a measure, or `None` when they have none (no lap, no finish).
    pub fn value(&self, measure: Measure) -> Option<i64> {
        match measure {
            Measure::Wins => Some(i64::from(self.wins)),
            Measure::Points => Some(i64::from(self.points)),
            Measure::Laps => Some(i64::from(self.laps)),
            Measure::BestLap => self.best_lap_micros,
            Measure::BestConsecutive(n) => self
                .races
                .iter()
                .filter_map(|laps| laps.best_consecutive_micros(n as usize))
                .min(),
            Measure::BestFinish => self.best_finish.map(i64::from),
            Measure::LastFinish => self.last_finish.map(i64::from),
        }
    }
}

/// Whether a bigger value of the measure is the better one.
fn more_is_better(measure: Measure) -> bool {
    matches!(measure, Measure::Wins | Measure::Points | Measure::Laps)
}

/// Compare two pilots on one measure: `Less` means `a` ranks above `b`. A pilot with no
/// value ranks below one with a value.
fn compare_on(measure: Measure, a: &Standing, b: &Standing) -> Ordering {
    match (a.value(measure), b.value(measure)) {
        (Some(x), Some(y)) if more_is_better(measure) => y.cmp(&x),
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The standings of a heat: every pilot in `field` exactly once, in a strict order.
///
/// `field` is the heat's pilots in seed order. `races` is the heat's **saved** races, oldest
/// first; each holds the laps of the pilots who were in that race's lineup. The order is:
/// pilots who flew above pilots who never did, then the ranking rule's measures in turn,
/// then seed. With no ranking rule (an unranked heat) the order is flew-then-seed.
pub fn heat_standings(
    field: &[PilotId],
    races: &[Vec<RaceEntry>],
    format: &Format,
) -> Vec<Standing> {
    let seed_of = |pilot: &PilotId| field.iter().position(|p| p == pilot).unwrap_or(usize::MAX);
    let mut standings: Vec<Standing> = field
        .iter()
        .enumerate()
        .map(|(seed, pilot)| Standing {
            pilot: pilot.clone(),
            position: 0,
            seed,
            flew: false,
            wins: 0,
            points: 0,
            laps: 0,
            best_lap_micros: None,
            best_finish: None,
            last_finish: None,
            races: Vec::new(),
        })
        .collect();

    for race in races {
        let finish = race_finish(race, &format.race.end, seed_of);
        for entry in race {
            let Some(standing) = standings.iter_mut().find(|s| s.pilot == entry.pilot) else {
                continue; // a pilot in the race who is not in the heat's field
            };
            let line = finish
                .iter()
                .find(|f| f.pilot == entry.pilot)
                .expect("every race entry has a finish line");
            let crossed = entry.laps.hole_shot.is_some();
            standing.flew |= crossed;
            standing.laps = standing.laps.max(entry.laps.lap_count() as u32);
            standing.best_lap_micros =
                match (standing.best_lap_micros, entry.laps.best_lap_micros()) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            // A pilot who never crossed in this race did not finish it: no position, no
            // win, no points.
            if crossed {
                standing.last_finish = Some(line.position);
                standing.best_finish = Some(
                    standing
                        .best_finish
                        .map_or(line.position, |b| b.min(line.position)),
                );
                if line.position == 1 && line.laps > 0 {
                    standing.wins += 1;
                }
                standing.points += format
                    .points
                    .get(line.position as usize - 1)
                    .copied()
                    .unwrap_or(0);
            } else {
                standing.last_finish = None;
            }
            standing.races.push(entry.laps.clone());
        }
    }

    let no_rule = RankingRule::default();
    let rule = format.ranking.as_ref().unwrap_or(&no_rule);
    standings.sort_by(|a, b| {
        b.flew
            .cmp(&a.flew)
            .then_with(|| {
                rule.sort
                    .iter()
                    .map(|measure| compare_on(*measure, a, b))
                    .find(|order| *order != Ordering::Equal)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.seed.cmp(&b.seed))
    });
    for (index, standing) in standings.iter_mut().enumerate() {
        standing.position = index as u32 + 1;
    }
    standings
}

/// Whether a heat is done, by its format's completion rule.
///
/// `saved_races` is how many of the heat's races are saved. `director_done` is whether the
/// director has said the heat is done, which is the only thing that completes a
/// "director decides" heat.
pub fn heat_complete(
    rule: &CompletionRule,
    standings: &[Standing],
    saved_races: usize,
    director_done: bool,
) -> bool {
    // The director can always call a heat done: a lone pilot who advances without flying,
    // or a heat cut short.
    if director_done {
        return true;
    }
    match rule {
        CompletionRule::SavedRaces(n) => saved_races >= *n as usize,
        CompletionRule::AnyPilotReaches { measure, at_least } => standings.iter().any(|s| {
            s.value(*measure).is_some_and(|value| {
                if more_is_better(*measure) {
                    value >= *at_least
                } else {
                    value <= *at_least
                }
            })
        }),
        CompletionRule::DirectorDecides => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::laps::{LapFilters, RaceLaps};
    use gridfpv_events::{
        AdapterId, Event, GateIndex, LogRef, Pass, RaceId, RaceRules, SeatRef, SourceTime,
    };

    const S: i64 = 1_000_000;

    fn pilot(name: &str) -> PilotId {
        PilotId(name.into())
    }

    /// A pilot's laps from crossing times in seconds, with optional restart marks (by
    /// crossing index).
    fn laps_with(crossings: &[i64], restarts: &[u64]) -> SeatLaps {
        let race = RaceId("r".into());
        let mut events: Vec<Event> = crossings
            .iter()
            .map(|at| {
                Event::Pass(Pass {
                    adapter: AdapterId("oft".into()),
                    seat: SeatRef("s".into()),
                    at: SourceTime::from_micros(at * S),
                    sequence: None,
                    gate: GateIndex::LAP,
                    signal: None,
                    race: Some(race.clone()),
                })
            })
            .collect();
        for crossing in restarts {
            events.push(Event::RestartMarked {
                race: race.clone(),
                crossing: LogRef(*crossing),
            });
        }
        RaceLaps::fold(events.iter().enumerate().map(|(i, e)| (i as u64, e))).seat(
            &race,
            &AdapterId("oft".into()),
            &SeatRef("s".into()),
            LapFilters::default(),
        )
    }

    fn entry(name: &str, crossings: &[i64]) -> RaceEntry {
        RaceEntry {
            pilot: pilot(name),
            laps: laps_with(crossings, &[]),
        }
    }

    fn format(
        end: EndCondition,
        completion: CompletionRule,
        sort: &[Measure],
        points: &[u32],
    ) -> Format {
        Format {
            name: "test".into(),
            race: RaceRules {
                end,
                staging_micros: 0,
                min_lap_micros: None,
                first_lap: Default::default(),
            },
            completion,
            ranking: Some(RankingRule {
                sort: sort.to_vec(),
            }),
            points: points.to_vec(),
        }
    }

    fn order(standings: &[Standing]) -> Vec<&str> {
        standings.iter().map(|s| s.pilot.0.as_str()).collect()
    }

    fn finish_order(finish: &[Finish]) -> Vec<(&str, u32)> {
        finish
            .iter()
            .map(|f| (f.pilot.0.as_str(), f.laps))
            .collect()
    }

    #[test]
    fn first_to_n_laps_is_won_by_whoever_finishes_lap_n_first() {
        // Ana finishes lap 3 at 31 s, Ben at 29 s. Ben flew a 4th lap; it does not count.
        let race = vec![
            entry("ana", &[1, 11, 21, 31]),
            entry("ben", &[2, 10, 20, 29, 40]),
            entry("cat", &[1, 12, 30]), // only 2 laps
        ];
        let finish = race_finish(&race, &EndCondition::FirstToLaps(3), |_| 0);
        assert_eq!(
            finish_order(&finish),
            vec![("ben", 3), ("ana", 3), ("cat", 2)]
        );
        assert_eq!(finish[0].position, 1);
        assert_eq!(finish[2].position, 3);
    }

    #[test]
    fn a_timed_race_is_most_laps_then_whoever_finished_their_last_lap_first() {
        let race = vec![
            entry("ana", &[1, 11, 21, 31]),    // 3 laps, done at 31
            entry("ben", &[2, 12, 22, 30]),    // 3 laps, done at 30
            entry("cat", &[1, 9, 17, 25, 33]), // 4 laps
        ];
        let end = EndCondition::Timed {
            micros: 30 * S,
            grace_micros: 5 * S,
        };
        let finish = race_finish(&race, &end, |_| 0);
        assert_eq!(
            finish_order(&finish),
            vec![("cat", 4), ("ben", 3), ("ana", 3)]
        );
    }

    #[test]
    fn a_pilot_with_no_lap_or_no_crossing_finishes_below_everyone_who_lapped() {
        let field = [pilot("dee"), pilot("eli"), pilot("ana")];
        let seed_of = |p: &PilotId| field.iter().position(|f| f == p).unwrap();
        let race = vec![
            entry("dee", &[]),      // never crossed (top seed)
            entry("eli", &[3]),     // hole shot only
            entry("ana", &[1, 11]), // one lap (bottom seed)
        ];
        let finish = race_finish(&race, &EndCondition::Open, seed_of);
        assert_eq!(
            finish_order(&finish),
            vec![("ana", 1), ("eli", 0), ("dee", 0)]
        );
    }

    #[test]
    fn a_dead_heat_is_broken_by_seed_so_positions_are_always_different() {
        let field = [pilot("ben"), pilot("ana")];
        let seed_of = |p: &PilotId| field.iter().position(|f| f == p).unwrap();
        let race = vec![entry("ana", &[1, 11]), entry("ben", &[1, 11])];
        let finish = race_finish(&race, &EndCondition::Open, seed_of);
        assert_eq!(finish_order(&finish), vec![("ben", 1), ("ana", 1)]);
        assert_eq!((finish[0].position, finish[1].position), (1, 2));
    }

    #[test]
    fn a_one_race_heat_is_ranked_by_finish_order() {
        let field = [pilot("ana"), pilot("ben"), pilot("cat")];
        let fmt = format(
            EndCondition::FirstToLaps(2),
            CompletionRule::SavedRaces(1),
            &[Measure::LastFinish],
            &[],
        );
        let race = vec![
            entry("ana", &[1, 12, 23]),
            entry("ben", &[1, 11, 21]),
            entry("cat", &[1, 13, 26]),
        ];
        let standings = heat_standings(&field, &[race], &fmt);
        assert_eq!(order(&standings), vec!["ben", "ana", "cat"]);
        assert_eq!(standings[0].wins, 1);
        assert!(heat_complete(&fmt.completion, &standings, 1, false));
        assert!(!heat_complete(&fmt.completion, &standings, 0, false));
    }

    #[test]
    fn chase_the_ace_runs_until_a_pilot_has_two_wins() {
        let field = [pilot("ana"), pilot("ben"), pilot("cat")];
        let fmt = format(
            EndCondition::FirstToLaps(1),
            CompletionRule::AnyPilotReaches {
                measure: Measure::Wins,
                at_least: 2,
            },
            &[Measure::Wins, Measure::LastFinish],
            &[],
        );
        let ana_wins = vec![
            entry("ana", &[1, 10]),
            entry("ben", &[1, 11]),
            entry("cat", &[1, 12]),
        ];
        let ben_wins = vec![
            entry("ana", &[1, 11]),
            entry("ben", &[1, 10]),
            entry("cat", &[1, 12]),
        ];
        let cat_wins = vec![
            entry("ana", &[1, 12]),
            entry("ben", &[1, 11]),
            entry("cat", &[1, 10]),
        ];

        // Three races, three different winners: nobody has two wins yet.
        let mut races = vec![ana_wins.clone(), ben_wins.clone(), cat_wins];
        let standings = heat_standings(&field, &races, &fmt);
        assert!(!heat_complete(
            &fmt.completion,
            &standings,
            races.len(),
            false
        ));

        // Ben wins again: done, and Ben is on top with two wins.
        races.push(ben_wins);
        let standings = heat_standings(&field, &races, &fmt);
        assert!(heat_complete(
            &fmt.completion,
            &standings,
            races.len(),
            false
        ));
        assert_eq!(order(&standings), vec!["ben", "ana", "cat"]);
        assert_eq!(standings[0].wins, 2);
        // Ana and Cat both have one win; the last race's finish order splits them.
        assert_eq!((standings[1].wins, standings[2].wins), (1, 1));
    }

    #[test]
    fn points_are_summed_across_the_races_of_one_heat() {
        // The pro-spec style: one heat, three races, points per race, total decides.
        let field = [pilot("ana"), pilot("ben"), pilot("cat")];
        let fmt = format(
            EndCondition::FirstToLaps(1),
            CompletionRule::SavedRaces(3),
            &[Measure::Points, Measure::BestFinish],
            &[10, 6, 3],
        );
        let r1 = vec![
            entry("ana", &[1, 10]),
            entry("ben", &[1, 11]),
            entry("cat", &[1, 12]),
        ];
        let r2 = vec![
            entry("ana", &[1, 12]),
            entry("ben", &[1, 10]),
            entry("cat", &[1, 11]),
        ];
        let r3 = vec![
            entry("ana", &[1, 11]),
            entry("ben", &[1, 10]),
            entry("cat", &[1, 12]),
        ];
        let races = vec![r1, r2, r3];
        let standings = heat_standings(&field, &races, &fmt);
        // ana 10+3+6 = 19, ben 6+10+10 = 26, cat 3+6+3 = 12
        assert_eq!(order(&standings), vec!["ben", "ana", "cat"]);
        assert_eq!(
            standings.iter().map(|s| s.points).collect::<Vec<_>>(),
            vec![26, 19, 12]
        );
        assert!(heat_complete(&fmt.completion, &standings, 3, false));
        assert!(!heat_complete(&fmt.completion, &standings, 2, false));
    }

    #[test]
    fn a_time_trial_takes_each_pilots_best_three_consecutive_across_their_runs() {
        let field = [pilot("ana"), pilot("ben")];
        let fmt = format(
            EndCondition::Open,
            CompletionRule::DirectorDecides,
            &[Measure::BestConsecutive(3), Measure::BestLap],
            &[],
        );
        // Ana: run 1 best three = 10+10+10 = 30; run 2 best three = 9+9+9 = 27.
        // Ben: one run, best three = 9+9+10 = 28.
        let run1 = vec![
            entry("ana", &[0, 10, 20, 30]),
            entry("ben", &[0, 9, 18, 28]),
        ];
        let run2 = vec![entry("ana", &[0, 9, 18, 27])];
        let standings = heat_standings(&field, &[run1, run2], &fmt);
        assert_eq!(order(&standings), vec!["ana", "ben"]);
        assert_eq!(
            standings[0].value(Measure::BestConsecutive(3)),
            Some(27 * S)
        );
        assert_eq!(
            standings[1].value(Measure::BestConsecutive(3)),
            Some(28 * S)
        );
        // Only the director ends it.
        assert!(!heat_complete(&fmt.completion, &standings, 2, false));
        assert!(heat_complete(&fmt.completion, &standings, 2, true));
    }

    #[test]
    fn best_consecutive_in_standings_does_not_span_a_restart() {
        let field = [pilot("ana")];
        let fmt = format(
            EndCondition::Open,
            CompletionRule::DirectorDecides,
            &[Measure::BestConsecutive(3)],
            &[],
        );
        // Laps 10, 9, [restart], 8, 7: no stretch of three.
        let run = vec![RaceEntry {
            pilot: pilot("ana"),
            laps: laps_with(&[0, 10, 19, 23, 31, 38], &[3]),
        }];
        let standings = heat_standings(&field, &[run], &fmt);
        assert_eq!(standings[0].value(Measure::BestConsecutive(3)), None);
        assert_eq!(
            standings[0].value(Measure::BestConsecutive(2)),
            Some(15 * S)
        );
    }

    #[test]
    fn a_pilot_with_fewer_laps_than_the_window_ranks_below_one_with_a_time() {
        let field = [pilot("ana"), pilot("ben")];
        let fmt = format(
            EndCondition::Open,
            CompletionRule::DirectorDecides,
            &[Measure::BestConsecutive(3)],
            &[],
        );
        // Ana (top seed) flew two fast laps; Ben flew three slow ones.
        let run = vec![entry("ana", &[0, 5, 10]), entry("ben", &[0, 20, 40, 60])];
        let standings = heat_standings(&field, &[run], &fmt);
        assert_eq!(order(&standings), vec!["ben", "ana"]);
    }

    #[test]
    fn a_pilot_who_never_flew_ranks_below_everyone_who_did_whatever_their_seed() {
        // The top seed misses the heat; everyone else races.
        let field = [pilot("top"), pilot("ana"), pilot("ben")];
        let fmt = format(
            EndCondition::FirstToLaps(1),
            CompletionRule::SavedRaces(1),
            &[Measure::LastFinish],
            &[],
        );
        let race = vec![
            entry("top", &[]),
            entry("ana", &[1, 12]),
            entry("ben", &[1, 10]),
        ];
        let standings = heat_standings(&field, &[race], &fmt);
        assert_eq!(order(&standings), vec!["ben", "ana", "top"]);
        assert!(!standings[2].flew);
        // And a pilot removed from the lineup altogether is still in the standings, last.
        let race = vec![entry("ana", &[1, 12]), entry("ben", &[1, 10])];
        let standings = heat_standings(&field, &[race], &fmt);
        assert_eq!(order(&standings), vec!["ben", "ana", "top"]);
    }

    #[test]
    fn several_no_shows_are_ordered_by_seed() {
        let field = [pilot("a"), pilot("b"), pilot("c"), pilot("d")];
        let fmt = format(
            EndCondition::FirstToLaps(1),
            CompletionRule::SavedRaces(1),
            &[Measure::LastFinish],
            &[],
        );
        let race = vec![entry("c", &[1, 10])];
        let standings = heat_standings(&field, &[race], &fmt);
        assert_eq!(order(&standings), vec!["c", "a", "b", "d"]);
    }

    #[test]
    fn standings_hold_every_pilot_exactly_once_in_positions_one_to_n() {
        let field: Vec<PilotId> = ["a", "b", "c", "d", "e"].iter().map(|n| pilot(n)).collect();
        let fmt = format(
            EndCondition::Open,
            CompletionRule::DirectorDecides,
            &[Measure::Laps, Measure::BestLap],
            &[],
        );
        // Two pilots tie exactly, one never flies, and the heat has not been raced by "e".
        let race = vec![
            entry("a", &[0, 10, 20]),
            entry("b", &[0, 10, 20]),
            entry("c", &[]),
            entry("d", &[0, 9]),
        ];
        for races in [vec![], vec![race]] {
            let standings = heat_standings(&field, &races, &fmt);
            let mut names = order(&standings);
            let positions: Vec<u32> = standings.iter().map(|s| s.position).collect();
            assert_eq!(positions, vec![1, 2, 3, 4, 5]);
            names.sort_unstable();
            assert_eq!(names, vec!["a", "b", "c", "d", "e"]);
        }
    }

    #[test]
    fn an_unraced_heat_is_in_seed_order() {
        let field = [pilot("a"), pilot("b"), pilot("c")];
        let fmt = format(
            EndCondition::Open,
            CompletionRule::SavedRaces(1),
            &[Measure::LastFinish],
            &[],
        );
        assert_eq!(
            order(&heat_standings(&field, &[], &fmt)),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn an_unranked_heat_orders_by_who_flew_then_seed() {
        let field = [pilot("a"), pilot("b"), pilot("c")];
        let mut fmt = format(
            EndCondition::Open,
            CompletionRule::DirectorDecides,
            &[],
            &[],
        );
        fmt.ranking = None;
        // "c" flew far more laps than "b"; practice ranks nobody by laps.
        let race = vec![entry("b", &[0, 10]), entry("c", &[0, 5, 10, 15])];
        assert_eq!(
            order(&heat_standings(&field, &[race], &fmt)),
            vec!["b", "c", "a"]
        );
    }

    #[test]
    fn a_no_show_in_one_race_scores_nothing_for_it() {
        let field = [pilot("ana"), pilot("ben")];
        let fmt = format(
            EndCondition::FirstToLaps(1),
            CompletionRule::SavedRaces(2),
            &[Measure::Points],
            &[10, 6],
        );
        let r1 = vec![entry("ana", &[1, 10]), entry("ben", &[1, 11])];
        // Ben does not show for race 2: second "place" by default must not score 6.
        let r2 = vec![entry("ana", &[1, 10]), entry("ben", &[])];
        let standings = heat_standings(&field, &[r1, r2], &fmt);
        assert_eq!(standings[0].points, 20);
        assert_eq!(standings[1].points, 6);
        assert_eq!(standings[1].last_finish, None);
        assert!(standings[1].flew, "ben flew race 1");
    }
}
