//! Presets: generators that write plain phases (`docs/primitives.md` §6).
//!
//! A preset runs once and returns ordinary [`PhaseDef`]s. After that nothing in the engine
//! knows which preset made them, and any part can be edited by hand. The only trace a
//! preset leaves is the **display hint** on the phase it creates, which views and overlays
//! read to pick a layout.
//!
//! Built so far:
//!
//! - [`bracket`]: single or double elimination, for any number of entrants, with heats of
//!   two pilots and up, and any number advancing from each heat (up to half of it).
//! - [`mains`]: brackets stacked with bump-ups, for big fields.

use gridfpv_events::{
    AdapterId, CombineRule, FieldSlice, Format, HeatSeat, PhaseDef, PhaseId, PhaseKind, SeatRef,
    SeatSource,
};
use thiserror::Error;

/// The display hint a single elimination bracket carries.
pub const HINT_SINGLE_ELIMINATION: &str = "single-elimination";
/// The display hint a double elimination bracket carries.
pub const HINT_DOUBLE_ELIMINATION: &str = "double-elimination";
/// The display hint stacked mains carry.
pub const HINT_MAINS: &str = "mains";

/// Why a preset could not be generated.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PresetError {
    /// A heat needs at least two seats.
    #[error("a bracket heat needs at least two seats, not {0}")]
    HeatTooSmall(usize),
    /// At least one pilot must advance from a heat, and at most half of it.
    #[error("{advance} cannot advance from a heat of {heat_size}: it must be 1 to {max}")]
    BadAdvance {
        advance: u32,
        heat_size: usize,
        max: usize,
    },
    /// A bracket needs somebody in it.
    #[error("a bracket needs at least one entrant")]
    NoEntrants,
    /// A main must hold more pilots than bump up into it.
    #[error("{bump} cannot bump up into a main of {per_main}")]
    BadBump { bump: u32, per_main: u32 },
    /// The mains named cannot hold the field.
    #[error("{mains} mains hold {capacity} pilots, not {pilots}: {needed} mains are needed")]
    TooManyPilots {
        pilots: u32,
        mains: usize,
        capacity: u32,
        needed: usize,
    },
}

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
    label: String,
    parent: &PhaseId,
    seats: &[(AdapterId, SeatRef)],
    sources: Vec<SeatSource>,
    format: Option<Format>,
) -> PhaseDef {
    PhaseDef {
        id,
        label,
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

/// How many losses put a pilot out of a bracket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elimination {
    /// Fail to advance once and you are out.
    Single,
    /// Fail to advance once and you drop to the losers side; fail there and you are out.
    Double,
}

/// What a bracket is built from.
pub struct Bracket {
    /// The id of the bracket phase. Its rounds and heats get ids beneath it ("‹id›/final").
    pub id: PhaseId,
    /// The director's name for the bracket ("C-Main").
    pub label: String,
    /// The phase that will hold the bracket.
    pub parent: Option<PhaseId>,
    /// The bracket's field, in seed order. A field of positions that are not decided yet
    /// (bump-ups from another bracket) is fine: those seats wait.
    pub field: Vec<FieldSlice>,
    /// Who starts on each seeded place, top seed first. The bracket is shaped for exactly
    /// this many.
    pub entrants: Vec<SeatSource>,
    /// The seats every heat is flown on. How many there are is the heat size.
    pub seats: Vec<(AdapterId, SeatRef)>,
    /// How many pilots move on from each heat. At most half the heat.
    pub advance: u32,
    pub elimination: Elimination,
    /// The format for the bracket's heats.
    pub format: Format,
    /// A different format for the final only (Chase the Ace on an A-Main).
    pub final_format: Option<Format>,
}

/// One place in a round that is still to be dealt into a heat.
#[derive(Clone)]
struct Slot {
    source: SeatSource,
    /// The heat this place comes out of, if it comes out of one. Two places out of the same
    /// heat are kept apart in the next round where they can be.
    out_of: Option<PhaseId>,
}

/// One round once it is built.
struct Round {
    /// The phase the bracket holds for this round: the heat itself, or a group of heats.
    top: PhaseDef,
    /// The round's heats under a group. Empty when `top` is the only heat.
    heats: Vec<PhaseDef>,
    /// Each heat's id and how many places it has.
    sizes: Vec<(PhaseId, usize)>,
}

impl Round {
    /// The places `from..=to` of every heat, best places first.
    fn places(&self, first: u32, last: u32) -> Vec<Slot> {
        let mut out = Vec::new();
        for position in first..=last {
            for (id, size) in &self.sizes {
                if position as usize <= *size {
                    out.push(Slot {
                        source: from(id, position),
                        out_of: Some(id.clone()),
                    });
                }
            }
        }
        out
    }
}

/// Deal places, best first, into as few heats as hold them. Heats come out the same size
/// (or one apart). The deal snakes — 1, 2, 3, 3, 2, 1 — so the top places are spread, and it
/// steps aside to keep two places out of the same heat apart where it can.
fn deal(slots: Vec<Slot>, heat_size: usize) -> Vec<Vec<Slot>> {
    let count = slots.len().div_ceil(heat_size).max(1);
    let mut heats: Vec<Vec<Slot>> = vec![Vec::new(); count];
    for (index, slot) in slots.into_iter().enumerate() {
        let fewest = heats.iter().map(Vec::len).min().unwrap_or(0);
        let forwards = (index / count) % 2 == 0;
        let mut open: Vec<usize> = (0..count).filter(|h| heats[*h].len() == fewest).collect();
        if !forwards {
            open.reverse();
        }
        let clear = |h: &usize| {
            slot.out_of.is_none() || heats[*h].iter().all(|other| other.out_of != slot.out_of)
        };
        let pick = open.iter().copied().find(clear).unwrap_or(open[0]);
        heats[pick].push(slot);
    }
    heats
}

/// Build one round: deal the places into heats and write the phases.
fn round(
    bracket: &PhaseId,
    name: &str,
    label: &str,
    slots: Vec<Slot>,
    seats: &[(AdapterId, SeatRef)],
    format: Option<Format>,
) -> Round {
    let dealt = deal(slots, seats.len());
    let id = child(bracket, name);
    let sources =
        |slots: &Vec<Slot>| -> Vec<SeatSource> { slots.iter().map(|s| s.source.clone()).collect() };
    if let [only] = &dealt[..] {
        return Round {
            top: heat(
                id.clone(),
                label.to_owned(),
                bracket,
                seats,
                sources(only),
                format,
            ),
            heats: Vec::new(),
            sizes: vec![(id, only.len())],
        };
    }
    let top = PhaseDef {
        id: id.clone(),
        label: label.to_owned(),
        parent: Some(bracket.clone()),
        // Pilots who go out in the same round are placed by where they finished their heat.
        kind: PhaseKind::Group {
            field: Vec::new(),
            combine: CombineRule::ByHeatPosition,
        },
        format,
        false_start_limit: None,
        display_hint: None,
    };
    let mut heats = Vec::new();
    let mut sizes = Vec::new();
    for (index, slots) in dealt.iter().enumerate() {
        let heat_id = child(&id, &format!("heat-{}", index + 1));
        sizes.push((heat_id.clone(), slots.len()));
        heats.push(heat(
            heat_id,
            format!("{label} Heat {}", index + 1),
            &id,
            seats,
            sources(slots),
            None,
        ));
    }
    Round { top, heats, sizes }
}

/// A single or double elimination bracket.
///
/// **Shape.** Round 1 deals the entrants into as few heats as hold them, seeds spread by a
/// snake (with eight pilots and four seats: 1-4-5-8 against 2-3-6-7). The top `advance` of
/// each heat move on to the next round, until one heat is left.
///
/// - *Single elimination:* that last heat is the final. Everyone else is out when they fail
///   to advance.
/// - *Double elimination:* the rounds above are the winners side. Pilots who fail to advance
///   there drop to the losers side, which races down between each drop. The top `advance` of
///   the winners final and of the losers final meet in the final.
///
/// A field that fits in one heat is just a final.
///
/// **Ids.** `‹id›/final`, and for double elimination `‹id›/winners-final`,
/// `‹id›/losers-final`, `‹id›/winners-N`, `‹id›/losers-N`; for single elimination
/// `‹id›/round-N`. A round with several heats is a group holding `…/heat-M`.
///
/// **Ranking.** The bracket stacks its rounds latest first, so every pilot is placed by the
/// last round they flew. Pilots who go out in the same round are ordered by their place in
/// their heat.
///
/// A heat that ends up with one pilot (a bye) can be flown, or the director can call it
/// done so the pilot advances without flying.
pub fn bracket(spec: Bracket) -> Result<Vec<PhaseDef>, PresetError> {
    let Bracket {
        id,
        label,
        parent,
        field,
        entrants,
        seats,
        advance,
        elimination,
        format,
        final_format,
    } = spec;
    let heat_size = seats.len();
    if heat_size < 2 {
        return Err(PresetError::HeatTooSmall(heat_size));
    }
    if advance == 0 || advance as usize * 2 > heat_size {
        return Err(PresetError::BadAdvance {
            advance,
            heat_size,
            max: heat_size / 2,
        });
    }
    if entrants.is_empty() {
        return Err(PresetError::NoEntrants);
    }
    let double = elimination == Elimination::Double && entrants.len() > heat_size;
    let keep = advance as usize;

    // The winners side (the whole bracket, for single elimination): round after round
    // until one heat is left.
    let mut winners: Vec<Round> = Vec::new();
    let mut pool: Vec<Slot> = entrants
        .into_iter()
        .map(|source| Slot {
            source,
            out_of: None,
        })
        .collect();
    let mut final_format = final_format;
    loop {
        let last = pool.len() <= heat_size;
        let number = winners.len() + 1;
        let (name, title, format) = match (last, double) {
            (true, false) => ("final".to_owned(), "Final".to_owned(), final_format.take()),
            (true, true) => ("winners-final".to_owned(), "Winners Final".to_owned(), None),
            (false, false) => (format!("round-{number}"), format!("Round {number}"), None),
            (false, true) => (
                format!("winners-{number}"),
                format!("Winners Round {number}"),
                None,
            ),
        };
        let built = round(&id, &name, &title, pool, &seats, format);
        pool = built.places(1, advance);
        winners.push(built);
        if last {
            break;
        }
    }

    // The losers side: race down to the size of the next drop from the winners side, take
    // the drop in, and go again. After the last drop, race down to one heat.
    let mut losers: Vec<Round> = Vec::new();
    let mut last_heat: Option<Round> = None;
    if double {
        let drops: Vec<Vec<Slot>> = winners
            .iter()
            .map(|round| round.places(advance + 1, heat_size as u32))
            .collect();
        let mut drops = drops.into_iter();
        let mut pool = drops.next().unwrap_or_default();
        let mut number = 0;
        let mut play = |pool: Vec<Slot>, is_final: bool, losers: &mut Vec<Round>| -> Vec<Slot> {
            number += 1;
            let (name, title) = if is_final {
                ("losers-final".to_owned(), "Losers Final".to_owned())
            } else {
                (format!("losers-{number}"), format!("Losers Round {number}"))
            };
            let built = round(&id, &name, &title, pool, &seats, None);
            let survivors = built.places(1, advance);
            losers.push(built);
            survivors
        };
        for drop in drops {
            while pool.len() > drop.len() && pool.len() > keep {
                pool = play(pool, false, &mut losers);
            }
            pool.extend(drop);
        }
        while pool.len() > keep {
            let is_final = pool.len() <= heat_size;
            pool = play(pool, is_final, &mut losers);
        }
        // The final: the best of the winners final, then whoever came through the losers
        // side.
        let mut finalists = winners.last().map_or(Vec::new(), |w| w.places(1, advance));
        finalists.extend(pool);
        last_heat = Some(round(
            &id,
            "final",
            "Final",
            finalists,
            &seats,
            final_format.take(),
        ));
    }

    let mut out = vec![PhaseDef {
        id,
        label,
        parent,
        kind: PhaseKind::Group {
            field,
            combine: CombineRule::Stack,
        },
        format: Some(format),
        false_start_limit: None,
        display_hint: Some(
            match elimination {
                Elimination::Single => HINT_SINGLE_ELIMINATION,
                Elimination::Double => HINT_DOUBLE_ELIMINATION,
            }
            .to_owned(),
        ),
    }];
    // Rounds are listed latest first: that is the order the bracket stacks them in.
    let rounds = last_heat
        .into_iter()
        .chain(losers.into_iter().rev())
        .chain(winners.into_iter().rev());
    for round in rounds {
        out.push(round.top);
        out.extend(round.heats);
    }
    Ok(out)
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
    /// The names of the mains, best first ("A-Main", "B-Main", …). [`mains_needed`] says
    /// how many a field takes.
    pub names: Vec<String>,
    /// How many pilots start in each main, bump-ups included (16 is usual for big events).
    pub per_main: u32,
    /// How many pilots bump up from each main into the one above.
    pub bump: u32,
    /// The seats every heat is flown on.
    pub seats: Vec<(AdapterId, SeatRef)>,
    /// How many pilots move on from each heat of a main's bracket.
    pub advance: u32,
    pub elimination: Elimination,
    /// The format for every heat.
    pub format: Format,
    /// A different format for the top main's final only.
    pub top_final_format: Option<Format>,
}

/// How many mains a field takes, when each starts `per_main` pilots and `bump` of them come
/// up from the main below.
pub fn mains_needed(pilots: u32, per_main: u32, bump: u32) -> usize {
    let direct = per_main.saturating_sub(bump).max(1);
    if pilots <= per_main {
        return 1;
    }
    1 + (pilots - per_main).div_ceil(direct) as usize
}

/// Mains stacked with bump-ups, each one a bracket.
///
/// The best `per_main − bump` qualifiers go straight to the top main, the next
/// `per_main − bump` to the one below, and so on; the bottom main takes whoever is left (up
/// to `per_main`). The top `bump` pilots of each main move up into the one above. The
/// holder stacks the mains in order, so the overall ranking runs from 1 to n across all of
/// them, and a pilot who bumped up is placed by the highest main they reached.
pub fn mains(spec: Mains) -> Result<Vec<PhaseDef>, PresetError> {
    let Mains {
        id,
        label,
        parent,
        seeded_from,
        pilots,
        names,
        per_main,
        bump,
        seats,
        advance,
        elimination,
        format,
        top_final_format,
    } = spec;
    if bump >= per_main {
        return Err(PresetError::BadBump { bump, per_main });
    }
    let direct = per_main - bump;
    let capacity = (names.len() as u32).saturating_sub(1) * direct + per_main;
    if names.is_empty() || pilots > capacity {
        return Err(PresetError::TooManyPilots {
            pilots,
            mains: names.len(),
            capacity: if names.is_empty() { 0 } else { capacity },
            needed: mains_needed(pilots, per_main, bump),
        });
    }
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
    let main_id = |index: usize| child(&id, &format!("main-{}", index + 1));
    let mut top_final_format = top_final_format;
    for (index, name) in names.iter().enumerate() {
        let is_bottom = index + 1 == names.len();
        // The qualifying places that start in this main.
        let first = index as u32 * direct + 1;
        let last = if is_bottom {
            (first + per_main - 1).min(pilots)
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
        if !is_bottom && bump > 0 {
            let below = main_id(index + 1);
            field.push(FieldSlice::FromPhase {
                phase: below.clone(),
                from: 1,
                to: bump,
            });
            entrants.extend((1..=bump).map(|position| from(&below, position)));
        }
        if entrants.is_empty() {
            // A main nobody reaches: named for more pilots than qualified. Leave it out.
            continue;
        }
        out.extend(bracket(Bracket {
            id: main_id(index),
            label: name.clone(),
            parent: Some(id.clone()),
            field,
            entrants,
            seats: seats.clone(),
            advance,
            elimination,
            format: format.clone(),
            final_format: if index == 0 {
                top_final_format.take()
            } else {
                None
            },
        })?);
    }
    Ok(out)
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

    fn seats_of(n: usize) -> Vec<(AdapterId, SeatRef)> {
        (0..n)
            .map(|i| (AdapterId("oft".into()), SeatRef(format!("seat-{i}"))))
            .collect()
    }

    fn seats() -> Vec<(AdapterId, SeatRef)> {
        seats_of(4)
    }

    fn one_race() -> Format {
        Format {
            name: "one race".into(),
            race: RaceRules {
                end: EndCondition::FirstToLaps(1),
                staging_micros: 0,
                min_lap_micros: None,
                first_lap: Default::default(),
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

    fn shaped(entrants: usize, heat_size: usize, advance: u32, elimination: Elimination) -> Tree {
        let mut tree = Tree::new();
        for def in bracket(Bracket {
            id: pid("bracket"),
            label: "Bracket".into(),
            parent: None,
            field: vec![FieldSlice::Imported(pilots(entrants))],
            entrants: pilots(entrants)
                .into_iter()
                .map(SeatSource::Pilot)
                .collect(),
            seats: seats_of(heat_size),
            advance,
            elimination,
            format: one_race(),
            final_format: None,
        })
        .unwrap()
        {
            tree.put(def);
        }
        tree
    }

    fn bracket_of(entrants: usize) -> Tree {
        shaped(entrants, 4, 2, Elimination::Double)
    }

    fn heat_count(tree: &Tree) -> usize {
        tree.iter()
            .filter(|def| matches!(def.kind, PhaseKind::Heat { .. }))
            .count()
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
            .insert(pid("bracket/winners-1/heat-1"), vec![slow_start]);
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
    fn a_bracket_is_shaped_for_its_entrants_so_six_pilots_need_no_empty_seats() {
        let tree = bracket_of(6);
        // Two round 1 heats of three, a winners final, a losers final and the final.
        assert_eq!(heat_count(&tree), 5);
        let mut world = world(6);
        assert_eq!(world.fly_everything(&tree), 5);
        let results = tree.evaluate(&world);
        assert!(
            results
                .values()
                .all(|result| { result.lineup.iter().all(|seat| seat.who != SeatWho::Nobody) })
        );
        let bracket = &results[&pid("bracket")];
        assert!(bracket.complete);
        assert_eq!(
            names(bracket),
            vec!["p01", "p02", "p03", "p04", "p05", "p06"]
        );
    }

    #[test]
    fn round_one_is_seeded_by_a_snake() {
        let tree = bracket_of(8);
        let world = world(8);
        let results = tree.evaluate(&world);
        let field = |heat: &str| -> Vec<String> {
            results[&pid(heat)]
                .field
                .iter()
                .map(|p| p.0.clone())
                .collect()
        };
        assert_eq!(
            field("bracket/winners-1/heat-1"),
            vec!["p01", "p04", "p05", "p08"]
        );
        assert_eq!(
            field("bracket/winners-1/heat-2"),
            vec!["p02", "p03", "p06", "p07"]
        );
    }

    #[test]
    fn two_pilots_out_of_the_same_heat_are_kept_apart_in_the_next_round() {
        // 16 pilots: four round 1 heats, then two winners heats of four.
        let tree = bracket_of(16);
        for heat in ["bracket/winners-2/heat-1", "bracket/winners-2/heat-2"] {
            let PhaseKind::Heat { lineup } = &tree.get(&pid(heat)).unwrap().kind else {
                panic!("{heat} is not a heat");
            };
            let mut came_from: Vec<&PhaseId> = lineup
                .iter()
                .map(|seat| match &seat.source {
                    SeatSource::FromPhase { phase, .. } => phase,
                    other => panic!("unexpected source {other:?}"),
                })
                .collect();
            came_from.sort();
            came_from.dedup();
            assert_eq!(came_from.len(), 4, "{heat} has a rematch");
        }
    }

    #[test]
    fn a_sixteen_pilot_double_elimination_takes_fourteen_heats() {
        let tree = bracket_of(16);
        // Winners 4 + 2 + 1, losers 2 + 2 + 1 + 1, and the final.
        assert_eq!(heat_count(&tree), 14);
        let mut world = world(16);
        world.fly_everything(&tree);
        let results = tree.evaluate(&world);
        let expected: Vec<String> = pilots(16).into_iter().map(|p| p.0).collect();
        assert_eq!(names(&results[&pid("bracket")]), expected);
    }

    #[test]
    fn single_elimination_puts_a_pilot_out_the_first_time_they_fail_to_advance() {
        let tree = shaped(8, 4, 2, Elimination::Single);
        assert_eq!(heat_count(&tree), 3);
        assert_eq!(
            tree.get(&pid("bracket")).unwrap().display_hint.as_deref(),
            Some(HINT_SINGLE_ELIMINATION)
        );
        let mut world = world(8);
        world.fly_everything(&tree);
        let results = tree.evaluate(&world);
        // The four who went out in round 1 are placed by heat position, then seed.
        assert_eq!(
            names(&results[&pid("bracket")]),
            vec!["p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08"]
        );
    }

    #[test]
    fn a_field_that_fits_in_one_heat_is_just_a_final() {
        for elimination in [Elimination::Single, Elimination::Double] {
            let tree = shaped(3, 4, 2, elimination);
            assert_eq!(heat_count(&tree), 1);
            assert!(tree.get(&pid("bracket/final")).is_some());
        }
    }

    #[test]
    fn three_pilots_head_to_head_gives_the_top_seed_a_bye() {
        let tree = shaped(3, 2, 1, Elimination::Double);
        let world_ = world(3);
        let results = tree.evaluate(&world_);
        assert_eq!(
            results[&pid("bracket/winners-1/heat-1")].field,
            vec![PilotId("p01".into())]
        );
        let mut world = world(3);
        world.fly_everything(&tree);
        let results = tree.evaluate(&world);
        assert_eq!(names(&results[&pid("bracket")]), vec!["p01", "p02", "p03"]);
    }

    #[test]
    fn every_shape_finishes_and_ranks_every_pilot_exactly_once() {
        for heat_size in 2..=8usize {
            for advance in 1..=(heat_size / 2) as u32 {
                for elimination in [Elimination::Single, Elimination::Double] {
                    for entrants in [3, 4, 5, 6, 7, 8, 9, 11, 12, 16, 17, 24, 33] {
                        let what = format!(
                            "{entrants} pilots, heats of {heat_size}, {advance} advance, {elimination:?}"
                        );
                        let tree = shaped(entrants, heat_size, advance, elimination);
                        for def in tree.iter() {
                            if let PhaseKind::Heat { lineup } = &def.kind {
                                assert!(!lineup.is_empty(), "{what}: {:?} is empty", def.id);
                                assert!(lineup.len() <= heat_size, "{what}: heat too big");
                            }
                        }
                        let mut world = world(entrants);
                        world.fly_everything(&tree);
                        let results = tree.evaluate(&world);
                        let bracket = &results[&pid("bracket")];
                        assert!(bracket.complete, "{what}: did not finish");
                        let mut ranked = names(bracket);
                        assert_eq!(ranked[0], "p01", "{what}: the fastest pilot did not win");
                        ranked.sort_unstable();
                        let expected: Vec<String> =
                            pilots(entrants).into_iter().map(|p| p.0).collect();
                        assert_eq!(ranked, expected, "{what}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_bracket_refuses_a_shape_that_cannot_work() {
        let spec = |seats: usize, advance: u32, entrants: usize| Bracket {
            id: pid("bracket"),
            label: "Bracket".into(),
            parent: None,
            field: Vec::new(),
            entrants: pilots(entrants)
                .into_iter()
                .map(SeatSource::Pilot)
                .collect(),
            seats: seats_of(seats),
            advance,
            elimination: Elimination::Double,
            format: one_race(),
            final_format: None,
        };
        assert_eq!(
            bracket(spec(1, 1, 8)).unwrap_err(),
            PresetError::HeatTooSmall(1)
        );
        assert!(matches!(
            bracket(spec(4, 3, 8)).unwrap_err(),
            PresetError::BadAdvance { max: 2, .. }
        ));
        assert!(matches!(
            bracket(spec(4, 0, 8)).unwrap_err(),
            PresetError::BadAdvance { .. }
        ));
        assert_eq!(bracket(spec(4, 2, 0)).unwrap_err(), PresetError::NoEntrants);
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
            per_main: 8,
            bump: 2,
            seats: seats(),
            advance: 2,
            elimination: Elimination::Double,
            format: one_race(),
            top_final_format: Some(chase_the_ace()),
        })
        .unwrap()
        {
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
            per_main: 8,
            bump: 0,
            seats: seats(),
            advance: 2,
            elimination: Elimination::Double,
            format: one_race(),
            top_final_format: None,
        })
        .unwrap()
        {
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

    #[test]
    fn a_big_event_runs_as_mains_of_sixteen_and_ranks_everyone() {
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
        // 96 pilots, 16 to a main, two bumping up: 14 go straight into each main.
        assert_eq!(mains_needed(96, 16, 2), 7);
        let mut tree = seeding(96);
        for def in mains(Mains {
            id: pid("mains"),
            label: "Mains".into(),
            parent: None,
            seeded_from: pid("seeding"),
            pilots: 96,
            names: "ABCDEFG".chars().map(|c| format!("{c}-Main")).collect(),
            per_main: 16,
            bump: 2,
            seats: seats(),
            advance: 2,
            elimination: Elimination::Double,
            format: one_race(),
            top_final_format: Some(chase_the_ace()),
        })
        .unwrap()
        {
            tree.put(def);
        }
        let mut world = Seeded(world(96));
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
        let overall = &results[&pid("mains")];
        assert!(overall.complete);
        assert_eq!(
            overall
                .ranking
                .iter()
                .map(|p| p.position)
                .collect::<Vec<_>>(),
            (1..=96).collect::<Vec<_>>()
        );
        // The fastest pilot always wins here, so nobody bumps past a faster pilot: the top
        // six mains finish in seed order. (The bottom main has 12 pilots, so its heats are
        // uneven and two pilots who go out in the same round can swap on heat position.)
        let expected: Vec<String> = pilots(96).into_iter().map(|p| p.0).collect();
        assert_eq!(&names(overall)[..84], &expected[..84]);
        let mut everyone = names(overall);
        everyone.sort_unstable();
        assert_eq!(everyone, expected);
    }

    #[test]
    fn mains_refuse_a_field_they_cannot_hold_and_say_how_many_are_needed() {
        let err = mains(Mains {
            id: pid("mains"),
            label: "Mains".into(),
            parent: None,
            seeded_from: pid("seeding"),
            pilots: 40,
            names: vec!["A-Main".into(), "B-Main".into()],
            per_main: 16,
            bump: 2,
            seats: seats(),
            advance: 2,
            elimination: Elimination::Double,
            format: one_race(),
            top_final_format: None,
        })
        .unwrap_err();
        assert_eq!(
            err,
            PresetError::TooManyPilots {
                pilots: 40,
                mains: 2,
                capacity: 30,
                needed: 3
            }
        );
    }
}
