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
//! - [`round_robin`]: rounds of heats where the pilots are dealt differently each round,
//!   ranked by points summed across every heat.

use gridfpv_events::{
    AdapterId, CombineRule, FieldSlice, Format, HeatSeat, PhaseDef, PhaseId, PhaseKind,
    RankingRule, SeatRef, SeatSource,
};
use thiserror::Error;

/// The display hint a single elimination bracket carries.
pub const HINT_SINGLE_ELIMINATION: &str = "single-elimination";
/// The display hint a double elimination bracket carries.
pub const HINT_DOUBLE_ELIMINATION: &str = "double-elimination";
/// The display hint stacked mains carry.
pub const HINT_MAINS: &str = "mains";
/// The display hint a round robin carries.
pub const HINT_ROUND_ROBIN: &str = "round-robin";

/// Why a preset could not be generated.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PresetError {
    /// A heat needs at least two seats.
    #[error("a heat needs at least two seats, not {0}")]
    HeatTooSmall(usize),
    /// At least one pilot must advance from a heat, and at most half of it.
    #[error("{advance} cannot advance from a heat of {heat_size}: it must be 1 to {max}")]
    BadAdvance {
        advance: u32,
        heat_size: usize,
        max: usize,
    },
    /// A preset needs somebody in it.
    #[error("there must be at least one pilot")]
    NoEntrants,
    /// A round robin needs a round.
    #[error("a round robin needs at least one round")]
    NoRounds,
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
        class: None,
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
    /// The phase the bracket holds for this round. Every round is a phase that holds its
    /// heats, even a final with one heat, so every round reads and opens the same way.
    top: PhaseDef,
    /// The round's heats, under `top`.
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
        let forwards = (index / count).is_multiple_of(2);
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
        class: None,
    };
    let mut heats = Vec::new();
    let mut sizes = Vec::new();
    for (index, slots) in dealt.iter().enumerate() {
        let heat_id = child(&id, &format!("heat-{}", index + 1));
        sizes.push((heat_id.clone(), slots.len()));
        // A round's only heat takes the round's name: "Final", not "Final Heat 1".
        let heat_label = if dealt.len() == 1 {
            label.to_owned()
        } else {
            format!("{label} Heat {}", index + 1)
        };
        heats.push(heat(heat_id, heat_label, &id, seats, sources(slots), None));
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
/// A field that fits in one heat is just a final. Every round is a phase that holds its heats,
/// the final too.
///
/// **Ids.** `‹id›/final`, and for double elimination `‹id›/winners-final`,
/// `‹id›/losers-final`, `‹id›/winners-N`, `‹id›/losers-N`; for single elimination
/// `‹id›/round-N`. Each is a group holding its heats, `…/heat-M`.
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
        class: None,
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
        class: None,
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

/// How many swaps the round robin deal tries, at most, when it mixes the rounds.
const DEAL_TRIES: usize = 200_000;

/// Deal `pilots` (seeds `0..pilots`) into heats for each of `rounds` rounds. The answer is,
/// for each round, each heat's pilots as (seat index, seed).
///
/// Seed `i` is in row `i / heats`. A row is the pilots who share a seat: in every round one
/// of them goes to each heat. That is what keeps the heats even and a pilot on one seat.
///
/// Round 1 puts column `c` of every row in heat `c`, which spreads the seeds. The later
/// rounds are then mixed together, not one at a time, so that the meetings between pilots
/// come out as even as they can across the whole round robin: it tries swapping two of a
/// row's pilots between heats in some round, takes the swap when it evens the meetings out,
/// and at first also when it is a little worse, by less and less as the tries run out. The
/// swaps are picked by a fixed sequence of numbers, so the same request always deals the
/// same heats.
fn deal_rounds(pilots: usize, heat_size: usize, rounds: usize) -> Vec<Vec<Vec<(usize, usize)>>> {
    let heats = pilots.div_ceil(heat_size);
    let row_count = pilots.div_ceil(heats);
    let row_of = |seed: usize| seed / heats;
    // `heat_of[round][seed]`. Every round starts with each row turned a different way.
    let mut heat_of: Vec<Vec<usize>> = (0..rounds)
        .map(|round| {
            (0..pilots)
                .map(|seed| (seed % heats + round * row_of(seed)) % heats)
                .collect()
        })
        .collect();
    // How many times each pair of pilots shares a heat.
    let mut met = vec![vec![0i64; pilots]; pilots];
    for round in &heat_of {
        for a in 0..pilots {
            for b in 0..pilots {
                if a != b && round[a] == round[b] {
                    met[a][b] += 1;
                }
            }
        }
    }
    // The pilot of `row` in `heat` in a round, if the row has one there.
    let pilot_in = |heat_of: &[Vec<usize>], round: usize, row: usize, heat: usize| {
        (row * heats..pilots.min((row + 1) * heats)).find(|seed| heat_of[round][*seed] == heat)
    };
    // What moving `seed` from heat `from` to heat `to` in `round` does to the unevenness
    // (the sum of every pair's meetings squared), leaving out the pilot `swapped` with it.
    let moving = |heat_of: &[Vec<usize>],
                  met: &[Vec<i64>],
                  round: usize,
                  seed: usize,
                  from: usize,
                  to: usize|
     -> i64 {
        (0..pilots)
            .filter(|other| row_of(*other) != row_of(seed))
            .map(|other| match heat_of[round][other] {
                heat if heat == from => 1 - 2 * met[seed][other],
                heat if heat == to => 1 + 2 * met[seed][other],
                _ => 0,
            })
            .sum()
    };
    // Swap the pilots of `row` in heats `x` and `y` of `round` (either heat may have none
    // of that row) if the change is no worse than `slack`. Returns whether it swapped.
    let mut try_swap = |heat_of: &mut Vec<Vec<usize>>,
                        round: usize,
                        row: usize,
                        x: usize,
                        y: usize,
                        slack: i64|
     -> bool {
        let at_x = pilot_in(heat_of, round, row, x);
        let at_y = pilot_in(heat_of, round, row, y);
        if at_x.is_none() && at_y.is_none() {
            return false;
        }
        let change = at_x.map_or(0, |seed| moving(heat_of, &met, round, seed, x, y))
            + at_y.map_or(0, |seed| moving(heat_of, &met, round, seed, y, x));
        if change > slack {
            return false;
        }
        for (seed, from, to) in [(at_x, x, y), (at_y, y, x)] {
            let Some(seed) = seed else { continue };
            for other in 0..pilots {
                if row_of(other) == row_of(seed) {
                    continue;
                }
                let step = match heat_of[round][other] {
                    heat if heat == from => -1,
                    heat if heat == to => 1,
                    _ => 0,
                };
                met[seed][other] += step;
                met[other][seed] += step;
            }
        }
        if let Some(seed) = at_x {
            heat_of[round][seed] = y;
        }
        if let Some(seed) = at_y {
            heat_of[round][seed] = x;
        }
        true
    };

    // Round 1 and row 0 stay as they are: turning them only renames heats.
    if heats > 1 && row_count > 1 && rounds > 1 {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = |below: usize| -> usize {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) as usize) % below
        };
        let tries = (2_000 * row_count * rounds).min(DEAL_TRIES);
        for attempt in 0..tries {
            let round = 1 + next(rounds - 1);
            let row = 1 + next(row_count - 1);
            let x = next(heats);
            let y = (x + 1 + next(heats - 1)) % heats;
            // How much worse a swap may be and still be taken: from 4 down to 0.
            let slack = (4 * (tries - attempt) / tries) as i64;
            try_swap(&mut heat_of, round, row, x, y, slack);
        }
        // Last, take every swap that still helps, until none does.
        for _ in 0..pilots {
            let mut better = false;
            for round in 1..rounds {
                for row in 1..row_count {
                    for x in 0..heats {
                        for y in x + 1..heats {
                            better |= try_swap(&mut heat_of, round, row, x, y, -1);
                        }
                    }
                }
            }
            if !better {
                break;
            }
        }
    }

    heat_of
        .iter()
        .map(|round| {
            (0..heats)
                .map(|heat| {
                    (0..pilots)
                        .filter(|seed| round[*seed] == heat)
                        .map(|seed| (row_of(seed), seed))
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// What a round robin is built from.
pub struct RoundRobin {
    /// The id of the phase that holds every round.
    pub id: PhaseId,
    /// Its name ("Round Robin").
    pub label: String,
    pub parent: Option<PhaseId>,
    /// The phase's field.
    pub field: Vec<FieldSlice>,
    /// Who flies, in seed order: named pilots, or places of another phase's ranking.
    pub entrants: Vec<SeatSource>,
    /// The seats a heat is flown on. Their count is the most pilots in one heat.
    pub seats: Vec<(AdapterId, SeatRef)>,
    /// How many rounds. Every pilot flies one heat in each.
    pub rounds: u32,
    /// The format for every heat. Its points table scores each race.
    pub format: Format,
    /// How the pilots are ranked across every heat (points first, as a rule).
    pub ranking: RankingRule,
}

/// A round robin: `rounds` rounds of heats, every pilot in one heat each round, ranked by
/// the points (or whatever `ranking` names) they score across all of them.
///
/// **Shape.** Each round has as few heats as hold the field, the same size or one apart.
/// Round 1 spreads the seeds across the heats. Each later round deals the pilots again so
/// that, as far as it can, a pilot meets the pilots they have met least.
///
/// **A pilot keeps their seat.** Every heat a pilot flies is on the same seat, so they stay
/// on one video channel all day. The price is that two pilots who share a seat never meet.
///
/// **Ids.** `‹id›/round-N` is a group holding that round's heats, `‹id›/round-N/heat-M`.
/// The rounds are kept in the order they are flown.
///
/// **Ranking.** The phase pools every race beneath it and sorts by `ranking`. Each round
/// ranks its own heats the same way, so a round has a table too.
///
/// Every heat's lineup is known from the start, so the whole round robin can be flown in
/// any order.
pub fn round_robin(spec: RoundRobin) -> Result<Vec<PhaseDef>, PresetError> {
    let RoundRobin {
        id,
        label,
        parent,
        field,
        entrants,
        seats,
        rounds,
        format,
        ranking,
    } = spec;
    if seats.len() < 2 {
        return Err(PresetError::HeatTooSmall(seats.len()));
    }
    if entrants.is_empty() {
        return Err(PresetError::NoEntrants);
    }
    if rounds == 0 {
        return Err(PresetError::NoRounds);
    }
    let dealt = deal_rounds(entrants.len(), seats.len(), rounds as usize);
    let heats = dealt[0].len();

    let mut defs = vec![PhaseDef {
        id: id.clone(),
        label,
        parent,
        kind: PhaseKind::Group {
            field,
            combine: CombineRule::Measures(ranking.clone()),
        },
        format: Some(format),
        false_start_limit: None,
        display_hint: Some(HINT_ROUND_ROBIN.to_owned()),
        class: None,
    }];
    for (round, dealt) in (1..=rounds).zip(&dealt) {
        let round_id = child(&id, &format!("round-{round}"));
        let round_label = format!("Round {round}");
        defs.push(PhaseDef {
            id: round_id.clone(),
            label: round_label.clone(),
            parent: Some(id.clone()),
            kind: PhaseKind::Group {
                field: Vec::new(),
                combine: CombineRule::Measures(ranking.clone()),
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
            class: None,
        });
        for (index, pilots) in dealt.iter().enumerate() {
            // A round's only heat takes the round's name: "Round 1", not "Round 1 Heat 1".
            let heat_label = if heats == 1 {
                round_label.clone()
            } else {
                format!("{round_label} Heat {}", index + 1)
            };
            // The row is the seat, so a pilot is on the same seat in every round.
            let (heat_seats, sources): (Vec<_>, Vec<_>) = pilots
                .iter()
                .map(|(row, seed)| (seats[*row].clone(), entrants[*seed].clone()))
                .unzip();
            defs.push(heat(
                child(&round_id, &format!("heat-{}", index + 1)),
                heat_label,
                &round_id,
                &heat_seats,
                sources,
                None,
            ));
        }
    }
    Ok(defs)
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
                stop_after_all_finish_micros: None,
                max_micros: None,
                drop_idle_seat_micros: None,
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
    fn every_round_is_a_phase_that_holds_its_heats_even_a_round_of_one() {
        let spec = Bracket {
            id: pid("bracket"),
            label: "Bracket".into(),
            parent: None,
            field: vec![FieldSlice::Imported(pilots(8))],
            entrants: pilots(8).into_iter().map(SeatSource::Pilot).collect(),
            seats: seats_of(4),
            advance: 2,
            elimination: Elimination::Double,
            format: one_race(),
            final_format: None,
        };
        let defs = bracket(spec).unwrap();
        let bracket_id = pid("bracket");
        for def in &defs {
            match &def.kind {
                // No heat sits directly in the bracket: each is under its round.
                PhaseKind::Heat { .. } => {
                    assert_ne!(def.parent.as_ref(), Some(&bracket_id), "{:?}", def.id);
                }
                PhaseKind::Group { .. } if def.id != bracket_id => {
                    assert_eq!(def.parent.as_ref(), Some(&bracket_id), "{:?}", def.id);
                    assert!(
                        defs.iter()
                            .any(|heat| heat.parent.as_ref() == Some(&def.id)),
                        "{:?} holds no heat",
                        def.id
                    );
                }
                PhaseKind::Group { .. } => {}
            }
        }
        // A round's only heat takes the round's name. A round of two numbers them.
        let label = |id: &str| {
            defs.iter()
                .find(|def| def.id == pid(id))
                .map(|def| def.label.clone())
        };
        assert_eq!(label("bracket/final").as_deref(), Some("Final"));
        assert_eq!(label("bracket/final/heat-1").as_deref(), Some("Final"));
        assert_eq!(
            label("bracket/winners-1/heat-2").as_deref(),
            Some("Winners Round 1 Heat 2")
        );
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
            class: None,
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
            class: None,
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
            class: None,
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
            class: None,
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
            class: None,
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
        assert_eq!(world.0.races[&pid("mains/main-1/final/heat-1")].len(), 2);
        assert_eq!(world.0.races[&pid("mains/main-2/final/heat-1")].len(), 1);

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

    // --- Round robin ---------------------------------------------------------------

    fn points_race() -> Format {
        Format {
            points: vec![4, 3, 2, 1],
            ..one_race()
        }
    }

    fn by_points() -> RankingRule {
        RankingRule {
            sort: vec![Measure::Points, Measure::Wins, Measure::BestLap],
        }
    }

    fn robin_spec(entrants: usize, heat_size: usize, rounds: u32) -> RoundRobin {
        RoundRobin {
            id: pid("robin"),
            label: "Round Robin".into(),
            parent: None,
            field: vec![FieldSlice::Imported(pilots(entrants))],
            entrants: pilots(entrants)
                .into_iter()
                .map(SeatSource::Pilot)
                .collect(),
            seats: seats_of(heat_size),
            rounds,
            format: points_race(),
            ranking: by_points(),
        }
    }

    fn robin(entrants: usize, heat_size: usize, rounds: u32) -> Tree {
        let mut tree = Tree::new();
        for def in round_robin(robin_spec(entrants, heat_size, rounds)).unwrap() {
            tree.put(def);
        }
        tree
    }

    /// Each heat as (round, the pilots with their seats), in the order generated.
    fn robin_heats(defs: &[PhaseDef]) -> Vec<(String, Vec<(String, String)>)> {
        defs.iter()
            .filter_map(|def| match &def.kind {
                PhaseKind::Heat { lineup } => Some((
                    def.parent.clone().unwrap().0,
                    lineup
                        .iter()
                        .map(|seat| match &seat.source {
                            SeatSource::Pilot(pilot) => (pilot.0.clone(), seat.seat.0.clone()),
                            other => panic!("unexpected source {other:?}"),
                        })
                        .collect(),
                )),
                PhaseKind::Group { .. } => None,
            })
            .collect()
    }

    #[test]
    fn a_round_robin_puts_every_pilot_in_one_heat_of_every_round() {
        for (entrants, heat_size, rounds) in
            [(8, 4, 3), (10, 4, 4), (5, 2, 5), (3, 4, 2), (13, 6, 3)]
        {
            let defs = round_robin(robin_spec(entrants, heat_size, rounds)).unwrap();
            let heats = robin_heats(&defs);
            for round in 1..=rounds {
                let mut flying: Vec<String> = heats
                    .iter()
                    .filter(|(parent, _)| parent == &format!("robin/round-{round}"))
                    .flat_map(|(_, lineup)| lineup.iter().map(|(pilot, _)| pilot.clone()))
                    .collect();
                flying.sort();
                let expected: Vec<String> = pilots(entrants).into_iter().map(|p| p.0).collect();
                assert_eq!(flying, expected, "{entrants} pilots, round {round}");
            }
        }
    }

    #[test]
    fn a_round_has_as_few_heats_as_hold_the_field_and_they_are_even() {
        for (entrants, heat_size, heats_wanted) in [(8, 4, 2), (10, 4, 3), (13, 6, 3), (3, 4, 1)] {
            let defs = round_robin(robin_spec(entrants, heat_size, 3)).unwrap();
            let heats = robin_heats(&defs);
            assert_eq!(heats.len(), heats_wanted * 3);
            let sizes: Vec<usize> = heats.iter().map(|(_, lineup)| lineup.len()).collect();
            let (least, most) = (sizes.iter().min().unwrap(), sizes.iter().max().unwrap());
            assert!(most - least <= 1, "{entrants} pilots: {sizes:?}");
            assert!(*most <= heat_size);
        }
    }

    #[test]
    fn a_pilot_keeps_the_same_seat_in_every_round() {
        let defs = round_robin(robin_spec(10, 4, 5)).unwrap();
        let mut seat_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (_, lineup) in robin_heats(&defs) {
            // No seat is used twice in a heat.
            let seats: BTreeSet<&String> = lineup.iter().map(|(_, seat)| seat).collect();
            assert_eq!(seats.len(), lineup.len());
            for (pilot, seat) in lineup {
                seat_of.entry(pilot).or_default().insert(seat);
            }
        }
        assert_eq!(seat_of.len(), 10);
        assert!(seat_of.values().all(|seats| seats.len() == 1));
    }

    #[test]
    fn round_one_spreads_the_top_seeds_across_the_heats() {
        let defs = round_robin(robin_spec(8, 4, 1)).unwrap();
        let heats = robin_heats(&defs);
        let pilots_of = |index: usize| -> Vec<&str> {
            heats[index]
                .1
                .iter()
                .map(|(pilot, _)| pilot.as_str())
                .collect()
        };
        assert_eq!(pilots_of(0), vec!["p01", "p03", "p05", "p07"]);
        assert_eq!(pilots_of(1), vec!["p02", "p04", "p06", "p08"]);
    }

    /// How many times each pair shared a heat, for pairs that can meet (different seats).
    fn meetings(defs: &[PhaseDef]) -> Vec<u32> {
        let heats = robin_heats(defs);
        let mut seat_of: BTreeMap<String, String> = BTreeMap::new();
        let mut met: BTreeMap<(String, String), u32> = BTreeMap::new();
        for (_, lineup) in &heats {
            for (a, seat) in lineup {
                seat_of.insert(a.clone(), seat.clone());
                for (b, _) in lineup {
                    if a < b {
                        *met.entry((a.clone(), b.clone())).or_default() += 1;
                    }
                }
            }
        }
        let everyone: Vec<&String> = seat_of.keys().collect();
        let mut counts = Vec::new();
        for a in &everyone {
            for b in &everyone {
                if a < b && seat_of[*a] != seat_of[*b] {
                    counts.push(met.get(&((*a).clone(), (*b).clone())).copied().unwrap_or(0));
                }
            }
        }
        counts
    }

    #[test]
    fn later_rounds_mix_the_pilots_so_meetings_stay_even() {
        // Eight pilots in two heats of four: over four rounds every pair that can meet
        // meets exactly twice.
        let counts = meetings(&round_robin(robin_spec(8, 4, 4)).unwrap());
        assert_eq!(counts.len(), 24);
        assert!(counts.iter().all(|met| *met == 2), "{counts:?}");

        // Nine pilots in three heats of three: over three rounds nobody meets twice.
        let counts = meetings(&round_robin(robin_spec(9, 3, 3)).unwrap());
        assert!(counts.iter().all(|met| *met == 1), "{counts:?}");

        // Bigger and uneven fields: no pair is far ahead of another.
        for (entrants, heat_size, rounds) in [
            (16, 4, 5),
            (10, 4, 4),
            (13, 6, 4),
            (12, 4, 4),
            (8, 2, 6),
            (6, 2, 5),
            (20, 6, 5),
        ] {
            let counts = meetings(&round_robin(robin_spec(entrants, heat_size, rounds)).unwrap());
            let (least, most) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
            assert!(most - least <= 2, "{entrants} pilots: {least} to {most}");
        }

        // Full heats of eight leave less room to mix, since eight pilots share each seat's
        // row. Even so, no two pilots fly together in every round, and most pairs sit at
        // the middle of the range.
        for (entrants, heat_size, rounds) in [(24, 8, 6), (32, 8, 4)] {
            let counts = meetings(&round_robin(robin_spec(entrants, heat_size, rounds)).unwrap());
            let most = *counts.iter().max().unwrap();
            assert!(most < rounds, "{entrants} pilots: a pair met {most} times");
            let far = counts.iter().filter(|met| **met + 1 >= rounds).count();
            assert!(far * 20 <= counts.len(), "{entrants} pilots: {far} pairs");
        }
    }

    #[test]
    fn the_same_request_always_deals_the_same_heats() {
        assert_eq!(
            round_robin(robin_spec(13, 6, 4)).unwrap(),
            round_robin(robin_spec(13, 6, 4)).unwrap()
        );
    }

    #[test]
    fn a_round_robin_is_rounds_of_heats_under_one_phase_in_flying_order() {
        let defs = round_robin(robin_spec(8, 4, 2)).unwrap();
        let shape: Vec<(&str, &str, Option<&str>)> = defs
            .iter()
            .map(|def| {
                (
                    def.id.0.as_str(),
                    def.label.as_str(),
                    def.parent.as_ref().map(|p| p.0.as_str()),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("robin", "Round Robin", None),
                ("robin/round-1", "Round 1", Some("robin")),
                (
                    "robin/round-1/heat-1",
                    "Round 1 Heat 1",
                    Some("robin/round-1")
                ),
                (
                    "robin/round-1/heat-2",
                    "Round 1 Heat 2",
                    Some("robin/round-1")
                ),
                ("robin/round-2", "Round 2", Some("robin")),
                (
                    "robin/round-2/heat-1",
                    "Round 2 Heat 1",
                    Some("robin/round-2")
                ),
                (
                    "robin/round-2/heat-2",
                    "Round 2 Heat 2",
                    Some("robin/round-2")
                ),
            ]
        );
        assert_eq!(defs[0].display_hint.as_deref(), Some(HINT_ROUND_ROBIN));
        assert_eq!(defs[0].format, Some(points_race()));
        // Only the top carries the format and the hint; the rest inherit.
        assert!(
            defs[1..]
                .iter()
                .all(|def| def.format.is_none() && def.display_hint.is_none())
        );
    }

    #[test]
    fn a_field_that_fits_one_heat_flies_the_same_heat_each_round_named_for_the_round() {
        let defs = round_robin(robin_spec(3, 4, 2)).unwrap();
        let labels: Vec<&str> = defs.iter().map(|def| def.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Round Robin", "Round 1", "Round 1", "Round 2", "Round 2"]
        );
    }

    #[test]
    fn a_round_robin_ranks_by_points_summed_across_every_heat() {
        let tree = robin(8, 4, 3);
        let mut world = world(8);
        // Every heat can be flown from the start: nothing waits on another heat.
        let ready = tree.evaluate(&world);
        assert!(tree.iter().all(|def| !ready[&def.id].waiting));
        assert_eq!(world.fly_everything(&tree), 6);

        let results = tree.evaluate(&world);
        let table = &results[&pid("robin")];
        assert!(table.complete);
        assert_eq!(table.ranking.len(), 8);
        let total: u32 = table.ranking.iter().map(|p| p.result.points).sum();
        // Two heats of four a round, 4+3+2+1 points a heat, three rounds.
        assert_eq!(total, 60);
        // The fastest pilot wins every heat they fly; the slowest is last in every one.
        assert_eq!(table.ranking[0].pilot, PilotId("p01".into()));
        assert_eq!(table.ranking[0].result.points, 12);
        assert_eq!(table.ranking[7].pilot, PilotId("p08".into()));
        assert_eq!(table.ranking[7].result.points, 3);
        let points: Vec<u32> = table.ranking.iter().map(|p| p.result.points).collect();
        assert!(
            points.windows(2).all(|pair| pair[0] >= pair[1]),
            "{points:?}"
        );

        // A round has a table of its own, from that round's heats only.
        let round = &results[&pid("robin/round-1")];
        assert!(round.complete);
        assert_eq!(
            round.ranking.iter().map(|p| p.result.points).sum::<u32>(),
            20
        );
    }

    #[test]
    fn a_round_robin_is_not_complete_until_every_round_is_flown() {
        let tree = robin(4, 2, 2);
        let mut world = world(4);
        let first = tree
            .iter()
            .find(|def| matches!(def.kind, PhaseKind::Heat { .. }))
            .unwrap()
            .id
            .clone();
        let field = tree.evaluate(&world)[&first].field.clone();
        let entries = field
            .iter()
            .map(|pilot| RaceEntry {
                pilot: pilot.clone(),
                laps: world.laps(pilot),
            })
            .collect();
        world.races.entry(first).or_default().push(entries);
        let results = tree.evaluate(&world);
        assert!(!results[&pid("robin")].complete);
        // The table already counts the heat that was flown.
        assert_eq!(
            results[&pid("robin")]
                .ranking
                .iter()
                .map(|p| p.result.points)
                .sum::<u32>(),
            7
        );
    }

    #[test]
    fn a_round_robin_can_be_seeded_from_another_phase() {
        // Qualifying is one heat of eight. Its ranking seeds the round robin.
        let mut tree = Tree::new();
        tree.put(PhaseDef {
            id: pid("quali"),
            label: "Qualifying".into(),
            parent: None,
            kind: PhaseKind::Group {
                field: vec![FieldSlice::Imported(pilots(8))],
                combine: CombineRule::Stack,
            },
            format: Some(one_race()),
            false_start_limit: None,
            display_hint: None,
            class: None,
        });
        tree.put(heat(
            pid("quali/heat"),
            "Qualifying Heat".into(),
            &pid("quali"),
            &seats_of(8),
            pilots(8).into_iter().map(SeatSource::Pilot).collect(),
            None,
        ));
        for def in round_robin(RoundRobin {
            field: vec![FieldSlice::FromPhase {
                phase: pid("quali"),
                from: 1,
                to: 8,
            }],
            entrants: (1..=8).map(|place| from(&pid("quali"), place)).collect(),
            ..robin_spec(8, 4, 2)
        })
        .unwrap()
        {
            tree.put(def);
        }
        let mut world = world(8);
        // The slowest pilot is fastest today, so qualifying turns the order round.
        for (i, pilot) in pilots(8).into_iter().enumerate() {
            world.speed.insert(pilot, 8 - i as i64);
        }
        // Until qualifying is flown, nobody knows who is in which heat.
        assert!(tree.evaluate(&world)[&pid("robin")].waiting);
        assert_eq!(world.fly_everything(&tree), 5);
        let results = tree.evaluate(&world);
        assert!(results[&pid("robin")].complete);
        assert_eq!(results[&pid("robin")].ranking.len(), 8);
        // The top seed is whoever won qualifying, and they fly in round 1 heat 1.
        assert_eq!(
            results[&pid("robin/round-1/heat-1")].field[0],
            PilotId("p08".into())
        );
    }

    #[test]
    fn a_round_robin_refuses_a_shape_that_cannot_work() {
        assert_eq!(
            round_robin(robin_spec(4, 1, 2)).unwrap_err(),
            PresetError::HeatTooSmall(1)
        );
        assert_eq!(
            round_robin(robin_spec(0, 4, 2)).unwrap_err(),
            PresetError::NoEntrants
        );
        assert_eq!(
            round_robin(robin_spec(4, 4, 0)).unwrap_err(),
            PresetError::NoRounds
        );
    }
}
