//! Presets: generators that write plain phases (`docs/primitives.md` §6).
//!
//! A preset runs once and returns ordinary [`PhaseDef`]s. After that nothing in the engine
//! knows which preset made them, and any part can be edited by hand. The only trace a
//! preset leaves is the **display hint** on the phase it creates, which views and overlays
//! read to pick a layout.
//!
//! Built so far: an eight-pilot double elimination bracket, and mains stacked with bump-ups.
//! Both are the common club shapes; other bracket sizes are more presets, not engine work.

use gridfpv_events::{
    AdapterId, CombineRule, FieldSlice, Format, HeatSeat, PhaseDef, PhaseId, PhaseKind, SeatRef,
    SeatSource,
};

/// The display hint a double elimination bracket carries.
pub const HINT_DOUBLE_ELIMINATION: &str = "double-elimination";
/// The display hint stacked mains carry.
pub const HINT_MAINS: &str = "mains";

fn child(prefix: &PhaseId, name: &str) -> PhaseId {
    PhaseId(format!("{}/{name}", prefix.0))
}

fn from(phase: &PhaseId, position: u32) -> SeatSource {
    SeatSource::FromPhase {
        phase: phase.clone(),
        position,
    }
}

fn heat(
    id: PhaseId,
    label: &str,
    parent: &PhaseId,
    seats: &[(AdapterId, SeatRef)],
    sources: Vec<SeatSource>,
    format: Option<Format>,
) -> PhaseDef {
    PhaseDef {
        id,
        label: label.to_owned(),
        parent: Some(parent.clone()),
        kind: PhaseKind::Heat {
            lineup: seats
                .iter()
                .zip(sources)
                .map(|((adapter, seat), source)| HeatSeat {
                    adapter: adapter.clone(),
                    seat: seat.clone(),
                    source,
                })
                .collect(),
        },
        format,
        false_start_limit: None,
        display_hint: None,
    }
}

/// What a double elimination bracket is built from.
pub struct DoubleElimination {
    /// The id of the bracket phase. Its heats get ids beneath it ("‹id›/final").
    pub id: PhaseId,
    /// The director's name for the bracket ("C-Main").
    pub label: String,
    /// The phase that will hold the bracket.
    pub parent: Option<PhaseId>,
    /// The bracket's field, in seed order. A field of positions that are not decided yet
    /// (bump-ups from another bracket) is fine: those seats wait.
    pub field: Vec<FieldSlice>,
    /// Who starts on each of the eight seeded places, top seed first. Fewer than eight
    /// leaves places empty.
    pub entrants: Vec<SeatSource>,
    /// The four seats every heat is flown on.
    pub seats: [(AdapterId, SeatRef); 4],
    /// The format for the bracket's heats.
    pub format: Format,
    /// A different format for the final only (Chase the Ace on an A-Main).
    pub final_format: Option<Format>,
}

/// An eight-pilot double elimination bracket with four-pilot heats: lose twice and you are
/// out.
///
/// ```text
/// Round 1 (two heats)   top 2 of each → Winners final     bottom 2 of each → Losers round 1
/// Losers round 1        top 2 → Losers final              bottom 2 out (7th, 8th)
/// Winners final         top 2 → Final                     bottom 2 → Losers final
/// Losers final          top 2 → Final                     bottom 2 out (5th, 6th)
/// Final                 1st to 4th
/// ```
///
/// Round 1 is seeded 1-4-5-8 against 2-3-6-7. The bracket stacks its heats best first, so
/// every pilot is placed by the last heat they flew.
pub fn double_elimination(spec: DoubleElimination) -> Vec<PhaseDef> {
    let DoubleElimination {
        id,
        label,
        parent,
        field,
        entrants,
        seats,
        format,
        final_format,
    } = spec;
    let entrant = |seed: usize| entrants.get(seed).cloned();
    let seeded =
        |seeds: [usize; 4]| -> Vec<SeatSource> { seeds.into_iter().filter_map(entrant).collect() };

    let r1a = child(&id, "round-1-a");
    let r1b = child(&id, "round-1-b");
    let l1 = child(&id, "losers-1");
    let wf = child(&id, "winners-final");
    let lf = child(&id, "losers-final");
    let fin = child(&id, "final");

    let bracket = PhaseDef {
        id: id.clone(),
        label,
        parent,
        kind: PhaseKind::Group {
            field,
            combine: CombineRule::Stack,
        },
        format: Some(format),
        false_start_limit: None,
        display_hint: Some(HINT_DOUBLE_ELIMINATION.to_owned()),
    };
    // Children are listed best first: that is the order the bracket stacks them in.
    vec![
        bracket,
        heat(
            fin,
            "Final",
            &id,
            &seats,
            vec![from(&wf, 1), from(&wf, 2), from(&lf, 1), from(&lf, 2)],
            final_format,
        ),
        heat(
            lf.clone(),
            "Losers Final",
            &id,
            &seats,
            vec![from(&l1, 1), from(&l1, 2), from(&wf, 3), from(&wf, 4)],
            None,
        ),
        heat(
            wf,
            "Winners Final",
            &id,
            &seats,
            vec![from(&r1a, 1), from(&r1b, 1), from(&r1a, 2), from(&r1b, 2)],
            None,
        ),
        heat(
            l1,
            "Losers Round 1",
            &id,
            &seats,
            vec![from(&r1a, 3), from(&r1b, 3), from(&r1a, 4), from(&r1b, 4)],
            None,
        ),
        heat(
            r1a,
            "Round 1 Heat 1",
            &id,
            &seats,
            seeded([0, 3, 4, 7]),
            None,
        ),
        heat(
            r1b,
            "Round 1 Heat 2",
            &id,
            &seats,
            seeded([1, 2, 5, 6]),
            None,
        ),
    ]
}

/// What stacked mains are built from.
pub struct Mains {
    /// The id of the phase that holds every main.
    pub id: PhaseId,
    /// Its name ("Mains").
    pub label: String,
    pub parent: Option<PhaseId>,
    /// The phase the mains are seeded from (qualifying).
    pub seeded_from: PhaseId,
    /// How many pilots that phase ranks.
    pub pilots: u32,
    /// The names of the mains, best first ("A-Main", "B-Main", …).
    pub names: Vec<String>,
    /// How many pilots bump up from each main into the one above.
    pub bump: u32,
    /// The four seats every heat is flown on.
    pub seats: [(AdapterId, SeatRef); 4],
    /// The format for every heat.
    pub format: Format,
    /// A different format for the top main's final only.
    pub top_final_format: Option<Format>,
}

/// Mains stacked with bump-ups, each one an eight-pilot double elimination bracket.
///
/// The best `8 − bump` qualifiers go straight to the top main, the next `8 − bump` to the
/// one below, and so on; the bottom main takes whoever is left (up to eight). The top
/// `bump` pilots of each main move up into the one above. The holder stacks the mains in
/// order, so the overall ranking runs from 1 to n across all of them, and a pilot who bumped
/// up is placed by the highest main they reached.
pub fn mains(spec: Mains) -> Vec<PhaseDef> {
    let Mains {
        id,
        label,
        parent,
        seeded_from,
        pilots,
        names,
        bump,
        seats,
        format,
        top_final_format,
    } = spec;
    let mut out = vec![PhaseDef {
        id: id.clone(),
        label,
        parent,
        kind: PhaseKind::Group {
            field: vec![FieldSlice::FromPhase {
                phase: seeded_from.clone(),
                from: 1,
                to: pilots,
            }],
            combine: CombineRule::Stack,
        },
        format: None,
        false_start_limit: None,
        display_hint: Some(HINT_MAINS.to_owned()),
    }];
    let direct = 8 - bump.min(8);
    let main_id = |index: usize| child(&id, &format!("main-{}", index + 1));
    let mut top_final_format = top_final_format;
    for (index, name) in names.iter().enumerate() {
        let is_bottom = index + 1 == names.len();
        // The qualifying places that start in this main.
        let first = index as u32 * direct + 1;
        let last = if is_bottom {
            (first + 7).min(pilots)
        } else {
            (first + direct - 1).min(pilots)
        };
        let mut field = Vec::new();
        let mut entrants = Vec::new();
        if first <= last {
            field.push(FieldSlice::FromPhase {
                phase: seeded_from.clone(),
                from: first,
                to: last,
            });
            entrants.extend((first..=last).map(|position| from(&seeded_from, position)));
        }
        if !is_bottom {
            let below = main_id(index + 1);
            field.push(FieldSlice::FromPhase {
                phase: below.clone(),
                from: 1,
                to: bump,
            });
            entrants.extend((1..=bump).map(|position| from(&below, position)));
        }
        out.extend(double_elimination(DoubleElimination {
            id: main_id(index),
            label: name.clone(),
            parent: Some(id.clone()),
            field,
            entrants,
            seats: seats.clone(),
            format: format.clone(),
            final_format: if index == 0 {
                top_final_format.take()
            } else {
                None
            },
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heat::RaceEntry;
    use crate::laps::{LapFilters, RaceLaps};
    use crate::phase::{Inputs, PhaseResult, SeatWho, Tree};
    use gridfpv_events::{
        ClassId, CompletionRule, EndCondition, Event, GateIndex, Measure, Pass, PilotId, RaceId,
        RaceRules, RankingRule, SourceTime,
    };
    use std::collections::{BTreeMap, BTreeSet};

    fn pid(id: &str) -> PhaseId {
        PhaseId(id.into())
    }

    fn seats() -> [(AdapterId, SeatRef); 4] {
        [0, 1, 2, 3].map(|i| (AdapterId("oft".into()), SeatRef(format!("seat-{i}"))))
    }

    fn one_race() -> Format {
        Format {
            name: "one race".into(),
            race: RaceRules {
                end: EndCondition::FirstToLaps(1),
                staging_micros: 0,
                min_lap_micros: None,
            },
            completion: CompletionRule::SavedRaces(1),
            ranking: Some(RankingRule {
                sort: vec![Measure::LastFinish],
            }),
            points: Vec::new(),
        }
    }

    fn chase_the_ace() -> Format {
        Format {
            name: "Chase the Ace".into(),
            completion: CompletionRule::AnyPilotReaches {
                measure: Measure::Wins,
                at_least: 2,
            },
            ranking: Some(RankingRule {
                sort: vec![Measure::Wins, Measure::LastFinish],
            }),
            ..one_race()
        }
    }

    /// A world where every race is flown and saved in the order `speed` gives: the pilot
    /// with the lower number wins. `no_shows` never cross the gate.
    #[derive(Default)]
    struct World {
        races: BTreeMap<PhaseId, Vec<Vec<RaceEntry>>>,
        speed: BTreeMap<PilotId, i64>,
        no_shows: BTreeSet<PilotId>,
    }

    impl Inputs for World {
        fn roster(&self, _: &ClassId) -> Vec<PilotId> {
            Vec::new()
        }
        fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>> {
            self.races.get(heat).cloned().unwrap_or_default()
        }
        fn director_done(&self, _: &PhaseId) -> bool {
            false
        }
    }

    impl World {
        fn laps(&self, pilot: &PilotId) -> crate::laps::SeatLaps {
            let race = RaceId("r".into());
            let crossings: Vec<i64> = if self.no_shows.contains(pilot) {
                vec![]
            } else {
                vec![0, 10_000_000 + self.speed[pilot]]
            };
            let events: Vec<Event> = crossings
                .into_iter()
                .map(|at| {
                    Event::Pass(Pass {
                        adapter: AdapterId("oft".into()),
                        seat: SeatRef("s".into()),
                        at: SourceTime::from_micros(at),
                        sequence: None,
                        gate: GateIndex::LAP,
                        signal: None,
                        race: Some(race.clone()),
                    })
                })
                .collect();
            RaceLaps::fold(events.iter().enumerate().map(|(i, e)| (i as u64, e))).seat(
                &race,
                &AdapterId("oft".into()),
                &SeatRef("s".into()),
                LapFilters::default(),
            )
        }

        /// Fly every heat that is ready, one race at a time, until nothing is left to fly.
        /// Returns how many races were flown.
        fn fly_everything(&mut self, tree: &Tree) -> usize {
            let mut flown = 0;
            loop {
                let results = tree.evaluate(self);
                let ready = tree.iter().find(|def| {
                    let result = &results[&def.id];
                    matches!(def.kind, PhaseKind::Heat { .. })
                        && !result.waiting
                        && !result.complete
                        && !result.field.is_empty()
                });
                let Some(def) = ready else { return flown };
                let entries = results[&def.id]
                    .field
                    .iter()
                    .map(|pilot| RaceEntry {
                        pilot: pilot.clone(),
                        laps: self.laps(pilot),
                    })
                    .collect();
                self.races.entry(def.id.clone()).or_default().push(entries);
                flown += 1;
                assert!(flown < 500, "the event never finishes");
            }
        }
    }

    fn names(result: &PhaseResult) -> Vec<&str> {
        result.ranking.iter().map(|p| p.pilot.0.as_str()).collect()
    }

    /// Pilots "p01".."pNN", faster the lower the number, unless reordered by the test.
    fn pilots(n: usize) -> Vec<PilotId> {
        (1..=n).map(|i| PilotId(format!("p{i:02}"))).collect()
    }

    fn world(n: usize) -> World {
        let mut world = World::default();
        for (i, pilot) in pilots(n).into_iter().enumerate() {
            world.speed.insert(pilot, i as i64);
        }
        world
    }

    fn bracket_of(entrants: usize) -> Tree {
        let mut tree = Tree::new();
        for def in double_elimination(DoubleElimination {
            id: pid("bracket"),
            label: "Bracket".into(),
            parent: None,
            field: vec![FieldSlice::Imported(pilots(entrants))],
            entrants: pilots(entrants)
                .into_iter()
                .map(SeatSource::Pilot)
                .collect(),
            seats: seats(),
            format: one_race(),
            final_format: None,
        }) {
            tree.put(def);
        }
        tree
    }

    #[test]
    fn a_double_elimination_bracket_places_all_eight_by_how_far_they_got() {
        let tree = bracket_of(8);
        let mut world = world(8);
        // Six heats, one race each.
        assert_eq!(world.fly_everything(&tree), 6);
        let results = tree.evaluate(&world);
        let bracket = &results[&pid("bracket")];
        assert!(bracket.complete);
        // With the faster pilot always winning, the bracket finishes in seed order.
        assert_eq!(
            names(bracket),
            vec!["p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08"]
        );
        assert_eq!(
            tree.get(&pid("bracket")).unwrap().display_hint.as_deref(),
            Some(HINT_DOUBLE_ELIMINATION)
        );
    }

    #[test]
    fn a_pilot_who_loses_once_can_still_come_back_through_the_losers_side() {
        let tree = bracket_of(8);
        let mut world = world(8);
        // p01 has a bad first heat (last of four), then flies like the fastest pilot again.
        // Fly round 1 heat 1 by hand with p01 last.
        let slow_start: Vec<RaceEntry> = ["p04", "p05", "p08", "p01"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut w = World::default();
                w.speed.insert(PilotId((*name).into()), i as i64);
                RaceEntry {
                    pilot: PilotId((*name).into()),
                    laps: w.laps(&PilotId((*name).into())),
                }
            })
            .collect();
        world
            .races
            .insert(pid("bracket/round-1-a"), vec![slow_start]);
        world.fly_everything(&tree);
        let results = tree.evaluate(&world);
        // p01 dropped to the losers side, won through it, and won the final.
        assert!(
            results[&pid("bracket/losers-1")]
                .field
                .contains(&PilotId("p01".into()))
        );
        assert_eq!(names(&results[&pid("bracket")])[0], "p01");
        let mut all = names(&results[&pid("bracket")]);
        all.sort_unstable();
        assert_eq!(
            all,
            vec!["p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08"]
        );
    }

    #[test]
    fn a_bracket_with_six_entrants_leaves_empty_places_and_still_finishes() {
        let tree = bracket_of(6);
        let mut world = world(6);
        world.fly_everything(&tree);
        let results = tree.evaluate(&world);
        // Round 1 has three pilots a side, so each "4th place" seat resolves to nobody.
        let losers_1 = &results[&pid("bracket/losers-1")];
        assert_eq!(
            losers_1
                .lineup
                .iter()
                .filter(|s| s.who == SeatWho::Nobody)
                .count(),
            2
        );
        assert_eq!(losers_1.field.len(), 2);
        let bracket = &results[&pid("bracket")];
        assert!(bracket.complete);
        assert_eq!(
            names(bracket),
            vec!["p01", "p02", "p03", "p04", "p05", "p06"]
        );
    }

    /// The hardest case in the design doc: 20 pilots, three mains, each a double
    /// elimination bracket, the top two of each bumping up, and the A-Main final run as
    /// Chase the Ace.
    fn multi_main() -> Tree {
        let mut tree = Tree::new();
        // Qualifying is a finished phase here: one heat per pilot is not the point of this
        // test, so the seeding comes in as a list.
        tree.put(PhaseDef {
            id: pid("qualifying"),
            label: "Qualifying".into(),
            parent: None,
            kind: PhaseKind::Group {
                field: vec![FieldSlice::Imported(pilots(20))],
                combine: CombineRule::Stack,
            },
            format: Some(one_race()),
            false_start_limit: None,
            display_hint: None,
        });
        // One heat that never needs flying keeps the group honest about being complete.
        tree.put(PhaseDef {
            id: pid("qualifying/seeded"),
            label: "Seeded".into(),
            parent: Some(pid("qualifying")),
            kind: PhaseKind::Heat {
                lineup: vec![HeatSeat {
                    adapter: AdapterId("oft".into()),
                    seat: SeatRef("seat-0".into()),
                    source: from(&pid("nowhere-complete"), 1),
                }],
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
        });
        tree
    }

    fn with_mains(mut tree: Tree, seeded_from: &str) -> Tree {
        for def in mains(Mains {
            id: pid("mains"),
            label: "Mains".into(),
            parent: None,
            seeded_from: pid(seeded_from),
            pilots: 20,
            names: vec!["A-Main".into(), "B-Main".into(), "C-Main".into()],
            bump: 2,
            seats: seats(),
            format: one_race(),
            top_final_format: Some(chase_the_ace()),
        }) {
            tree.put(def);
        }
        tree
    }

    /// A seeding phase that is complete from the start: an imported list ranks in seed
    /// order, and its one heat has nobody to race.
    fn seeding(n: usize) -> Tree {
        let mut tree = Tree::new();
        tree.put(PhaseDef {
            id: pid("seeding"),
            label: "Seeding".into(),
            parent: None,
            kind: PhaseKind::Group {
                field: vec![FieldSlice::Imported(pilots(n))],
                combine: CombineRule::Stack,
            },
            format: Some(one_race()),
            false_start_limit: None,
            display_hint: None,
        });
        tree.put(PhaseDef {
            id: pid("seeding/done"),
            label: "Done".into(),
            parent: Some(pid("seeding")),
            kind: PhaseKind::Heat {
                lineup: vec![HeatSeat {
                    adapter: AdapterId("oft".into()),
                    seat: SeatRef("seat-0".into()),
                    source: from(&pid("seeding/done-source"), 1),
                }],
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
        });
        tree.put(PhaseDef {
            id: pid("seeding/done-source"),
            label: "Empty".into(),
            parent: None,
            kind: PhaseKind::Heat {
                lineup: vec![HeatSeat {
                    adapter: AdapterId("oft".into()),
                    seat: SeatRef("seat-0".into()),
                    source: SeatSource::Open,
                }],
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
        });
        tree
    }

    #[test]
    fn the_mains_wait_while_their_seeding_phase_is_unfinished() {
        let tree = with_mains(multi_main(), "qualifying");
        let mut world = world(20);
        assert_eq!(
            world.fly_everything(&tree),
            0,
            "nothing can fly before qualifying is done"
        );
        let results = tree.evaluate(&world);
        assert!(results[&pid("mains")].waiting);
        assert!(!results[&pid("mains")].complete);
    }

    #[test]
    fn multi_main_double_elimination_ranks_one_to_twenty_across_all_mains() {
        // Seeding phase: complete once the director says its empty heat is done.
        struct Seeded(World);
        impl Inputs for Seeded {
            fn roster(&self, class: &ClassId) -> Vec<PilotId> {
                self.0.roster(class)
            }
            fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>> {
                self.0.saved_races(heat)
            }
            fn director_done(&self, heat: &PhaseId) -> bool {
                heat.0 == "seeding/done-source"
            }
        }
        let tree = with_mains(seeding(20), "seeding");

        // Fly the whole event. The lower number is always faster, except that the two
        // pilots who start at the top of the C-Main (p13, p14) are flying well today: they
        // beat everyone below p03.
        let mut inner = world(20);
        inner.speed.insert(PilotId("p13".into()), 3);
        inner.speed.insert(PilotId("p14".into()), 4);
        for (i, name) in [
            "p04", "p05", "p06", "p07", "p08", "p09", "p10", "p11", "p12",
        ]
        .iter()
        .enumerate()
        {
            inner.speed.insert(PilotId((*name).into()), 5 + i as i64);
        }
        let mut world = Seeded(inner);
        let mut flown = 0;
        loop {
            let results = tree.evaluate(&world);
            let ready = tree.iter().find(|def| {
                let result = &results[&def.id];
                matches!(def.kind, PhaseKind::Heat { .. })
                    && !result.waiting
                    && !result.complete
                    && !result.field.is_empty()
            });
            let Some(def) = ready else { break };
            let entries = results[&def.id]
                .field
                .iter()
                .map(|pilot| RaceEntry {
                    pilot: pilot.clone(),
                    laps: world.0.laps(pilot),
                })
                .collect();
            world
                .0
                .races
                .entry(def.id.clone())
                .or_default()
                .push(entries);
            flown += 1;
            assert!(flown < 500);
        }
        let results = tree.evaluate(&world);

        // Every main finished, and the A-Main final took two races (Chase the Ace).
        for main in ["mains/main-1", "mains/main-2", "mains/main-3", "mains"] {
            assert!(results[&pid(main)].complete, "{main} did not finish");
        }
        assert_eq!(world.0.races[&pid("mains/main-1/final")].len(), 2);
        assert_eq!(world.0.races[&pid("mains/main-2/final")].len(), 1);

        // The bump-ups: p13 and p14 won the C-Main, then the B-Main, and reached the A-Main.
        let c_main = names(&results[&pid("mains/main-3")]);
        assert_eq!(&c_main[..2], ["p13", "p14"]);
        let a_field = &results[&pid("mains/main-1")].field;
        assert!(
            a_field.contains(&PilotId("p13".into())) && a_field.contains(&PilotId("p14".into()))
        );

        // The overall ranking runs 1 to 20: every pilot exactly once.
        let overall = &results[&pid("mains")];
        assert_eq!(
            overall
                .ranking
                .iter()
                .map(|p| p.position)
                .collect::<Vec<_>>(),
            (1..=20).collect::<Vec<_>>()
        );
        let mut everyone = names(overall);
        everyone.sort_unstable();
        let expected: Vec<String> = pilots(20).into_iter().map(|p| p.0).collect();
        assert_eq!(
            everyone,
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );

        // A pilot who bumped up is placed by the highest main they reached: the first eight
        // places are exactly the A-Main's ranking, bumped pilots included.
        let overall_names = names(overall);
        assert_eq!(
            &overall_names[..8],
            &names(&results[&pid("mains/main-1")])[..]
        );
        assert_eq!(&overall_names[..5], ["p01", "p02", "p03", "p13", "p14"]);
        // Then the pilots who finished in the B-Main without bumping up, in B-Main order.
        let b_stayed: Vec<&str> = names(&results[&pid("mains/main-2")])
            .into_iter()
            .filter(|name| !a_field.contains(&PilotId((*name).into())))
            .collect();
        assert_eq!(&overall_names[8..8 + b_stayed.len()], &b_stayed[..]);
        assert_eq!(b_stayed.len(), 6);
        // And last, the pilots who never left the C-Main.
        assert_eq!(overall_names.len() - 8 - b_stayed.len(), 6);
    }

    #[test]
    fn a_seeded_pilot_who_misses_every_heat_finishes_last_overall() {
        // Ryan's example: in a 16 pilot event, the 8th seed misses their heats and everyone
        // else races. They finish 16th, not in a seed-based place.
        struct Seeded(World);
        impl Inputs for Seeded {
            fn roster(&self, class: &ClassId) -> Vec<PilotId> {
                self.0.roster(class)
            }
            fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>> {
                self.0.saved_races(heat)
            }
            fn director_done(&self, heat: &PhaseId) -> bool {
                heat.0 == "seeding/done-source"
            }
        }
        let mut tree = seeding(16);
        for def in mains(Mains {
            id: pid("mains"),
            label: "Mains".into(),
            parent: None,
            seeded_from: pid("seeding"),
            pilots: 16,
            names: vec!["A-Main".into(), "B-Main".into()],
            bump: 0,
            seats: seats(),
            format: one_race(),
            top_final_format: None,
        }) {
            tree.put(def);
        }
        let mut inner = world(16);
        inner.no_shows.insert(PilotId("p08".into()));
        let mut world = Seeded(inner);
        let mut flown = 0;
        loop {
            let results = tree.evaluate(&world);
            let ready = tree.iter().find(|def| {
                let result = &results[&def.id];
                matches!(def.kind, PhaseKind::Heat { .. })
                    && !result.waiting
                    && !result.complete
                    && !result.field.is_empty()
            });
            let Some(def) = ready else { break };
            let entries = results[&def.id]
                .field
                .iter()
                .map(|pilot| RaceEntry {
                    pilot: pilot.clone(),
                    laps: world.0.laps(pilot),
                })
                .collect();
            world
                .0
                .races
                .entry(def.id.clone())
                .or_default()
                .push(entries);
            flown += 1;
            assert!(flown < 500);
        }
        let results = tree.evaluate(&world);
        let overall = names(&results[&pid("mains")]);
        assert_eq!(overall.len(), 16);
        assert_eq!(overall[15], "p08");
    }
}
