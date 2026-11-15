//! The phase tree: field in, ranking out, at every level (`docs/primitives.md` §5.2 – §5.6).
//!
//! A [`Tree`] holds an event's phase definitions. [`Tree::evaluate`] works out, for every
//! phase, from the saved races alone:
//!
//! - its **field** — the pilots in seed order, once every slice or seat reference it
//!   depends on has resolved;
//! - a heat's **lineup** — each seat resolved to a pilot, left open, or still waiting on
//!   another phase;
//! - its **ranking** — a strict order of the field, every pilot exactly once;
//! - whether it is **complete**.
//!
//! The evaluation is pure and total. A reference to a phase that does not exist, or a loop
//! of references, leaves the phase waiting; it never panics.
//!
//! Two rules hold at every level. A pilot who never flew in a phase ranks below everyone
//! who did, whatever their seed. And the last tie-break is always seed.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{
    AdapterId, ClassId, CombineRule, CompletionRule, EndCondition, FieldSlice, FirstLap, Format,
    Measure, PhaseDef, PhaseId, PhaseKind, PilotId, RaceRules, RankingRule, SeatRef, SeatSource,
};

use crate::heat::{RaceEntry, Standing, heat_complete, heat_standings};

/// What the evaluation needs from outside the tree. All of it is derived from the log and
/// the directories; the tree itself holds no results.
pub trait Inputs {
    /// A class's roster, in roster order.
    fn roster(&self, class: &ClassId) -> Vec<PilotId>;
    /// A heat's **saved** races, oldest first: for each, the laps of each named pilot in
    /// that race's lineup.
    fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>>;
    /// Whether the director has said this heat is done.
    fn director_done(&self, heat: &PhaseId) -> bool;
    /// The pilots the director has disqualified from exactly this phase. A disqualification
    /// also covers every phase beneath it.
    fn disqualified(&self, _phase: &PhaseId) -> Vec<PilotId> {
        Vec::new()
    }
    /// Of those, the pilots whose earned place stays empty instead of passing to the next
    /// pilot (the director's choice when disqualifying).
    fn places_kept_empty(&self, _phase: &PhaseId) -> Vec<PilotId> {
        Vec::new()
    }
    /// The seat a pilot flew on in their latest race, in this event. Used to keep a pilot on
    /// the same seat (so the same channel) from one heat to the next.
    fn last_seat(&self, _pilot: &PilotId) -> Option<(AdapterId, SeatRef)> {
        None
    }
    /// The positions the director set by hand in a heat (1 for the top).
    fn positions_set(&self, _heat: &PhaseId) -> Vec<(PilotId, u32)> {
        Vec::new()
    }
}

/// One seat of a heat's lineup, as far as it can be resolved now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSeat {
    pub adapter: AdapterId,
    pub seat: SeatRef,
    pub who: SeatWho,
}

/// Who is on a resolved seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatWho {
    /// A pilot.
    Pilot(PilotId),
    /// Nobody named (practice).
    Open,
    /// Nobody: the seat points at a position its source phase finished without. A bracket
    /// built for eight with six pilots entered leaves seats like this.
    Nobody,
    /// Still waiting on another phase: "position N of that phase". The UI shows the phase's
    /// label, never its id.
    Waiting { phase: PhaseId, position: u32 },
}

/// One pilot's place in a phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub pilot: PilotId,
    /// 1 for the top. Every pilot in the field has a different position.
    pub position: u32,
    /// Whether the pilot is disqualified from this phase (here or in a phase above). A
    /// disqualified pilot ranks below everyone who kept racing.
    pub disqualified: bool,
    /// The numbers behind the place: for a heat, its standing; for a phase that holds
    /// phases, every race the pilot flew beneath it, pooled.
    pub result: Standing,
}

/// Everything worked out for one phase.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PhaseResult {
    /// The pilots in seed order. Partial while `waiting` is true.
    pub field: Vec<PilotId>,
    /// Whether some slice or seat this phase depends on has not resolved yet.
    pub waiting: bool,
    /// A heat's lineup. Empty for a phase that holds phases.
    pub lineup: Vec<ResolvedSeat>,
    /// The field in ranked order.
    pub ranking: Vec<Placed>,
    /// What "position N of this phase" hands on to another phase, top first. It is the
    /// ranking, except where a disqualified pilot's place is kept empty: there the slot
    /// holds nobody, and the pilots below do not move up into it.
    pub places: Vec<Option<PilotId>>,
    /// Whether the phase is done.
    pub complete: bool,
}

impl PhaseResult {
    /// The pilot at a 1-based position, once the phase is complete.
    fn at(&self, position: u32) -> Option<&PilotId> {
        if !self.complete {
            return None;
        }
        self.places
            .get(position.checked_sub(1)? as usize)
            .and_then(Option::as_ref)
    }
}

/// An event's phases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    defs: BTreeMap<PhaseId, PhaseDef>,
    /// Ids in the order the phases were added. Children of a phase keep this order, which
    /// is the order a [`CombineRule::Stack`] stacks them in.
    order: Vec<PhaseId>,
}

/// The format a heat falls back to when no phase above it sets one: an open race, unranked,
/// done when the director says so.
fn no_format() -> Format {
    Format {
        name: String::new(),
        race: RaceRules {
            end: EndCondition::Open,
            staging_micros: 0,
            min_lap_micros: None,
            first_lap: FirstLap::default(),
            stop_after_all_finish_micros: None,
            max_micros: None,
        },
        completion: CompletionRule::DirectorDecides,
        ranking: None,
        points: Vec::new(),
    }
}

impl Tree {
    /// An empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a phase, or replace the one with the same id (keeping its place in the order).
    pub fn put(&mut self, def: PhaseDef) {
        if !self.defs.contains_key(&def.id) {
            self.order.push(def.id.clone());
        }
        self.defs.insert(def.id.clone(), def);
    }

    /// One phase's definition.
    pub fn get(&self, id: &PhaseId) -> Option<&PhaseDef> {
        self.defs.get(id)
    }

    /// Every phase, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &PhaseDef> {
        self.order.iter().filter_map(|id| self.defs.get(id))
    }

    /// A phase's children, in the order they were added.
    pub fn children<'a>(&'a self, id: &'a PhaseId) -> impl Iterator<Item = &'a PhaseDef> {
        self.iter()
            .filter(move |def| def.parent.as_ref() == Some(id))
    }

    /// The format in force for a phase: its own, or the nearest one set above it.
    pub fn format_for(&self, id: &PhaseId) -> Option<&Format> {
        let mut seen = BTreeSet::new();
        let mut at = self.defs.get(id);
        while let Some(def) = at {
            if let Some(format) = &def.format {
                return Some(format);
            }
            if !seen.insert(&def.id) {
                return None; // a loop of parents
            }
            at = def.parent.as_ref().and_then(|parent| self.defs.get(parent));
        }
        None
    }

    /// Apply one logged event: a phase defined or removed. Other events are ignored.
    pub fn apply(&mut self, event: &gridfpv_events::Event) {
        match event {
            gridfpv_events::Event::PhaseDefined { def } => self.put(def.clone()),
            gridfpv_events::Event::PhaseRemoved { phase } => {
                self.defs.remove(phase);
                self.order.retain(|id| id != phase);
            }
            _ => {}
        }
    }

    /// A phase and every phase above it, nearest first. Stops at a loop of parents.
    pub fn with_ancestors<'a>(&'a self, id: &PhaseId) -> Vec<&'a PhaseDef> {
        let mut out: Vec<&PhaseDef> = Vec::new();
        let mut at = self.defs.get(id);
        while let Some(def) = at {
            if out.iter().any(|seen| seen.id == def.id) {
                break;
            }
            out.push(def);
            at = def.parent.as_ref().and_then(|parent| self.defs.get(parent));
        }
        out
    }

    /// Whether `phase` is `ancestor` or sits somewhere beneath it.
    pub fn is_under(&self, phase: &PhaseId, ancestor: &PhaseId) -> bool {
        self.with_ancestors(phase)
            .iter()
            .any(|def| &def.id == ancestor)
    }

    /// Every heat beneath a phase (or the phase itself, if it is a heat), in tree order.
    fn heats_under(&self, id: &PhaseId, out: &mut Vec<PhaseId>, seen: &mut BTreeSet<PhaseId>) {
        if !seen.insert(id.clone()) {
            return;
        }
        match self.defs.get(id).map(|def| &def.kind) {
            Some(PhaseKind::Heat { .. }) => out.push(id.clone()),
            Some(PhaseKind::Group { .. }) => {
                for child in self.children(id) {
                    self.heats_under(&child.id, out, seen);
                }
            }
            None => {}
        }
    }

    /// Work out every phase.
    pub fn evaluate(&self, inputs: &dyn Inputs) -> BTreeMap<PhaseId, PhaseResult> {
        let mut eval = Eval {
            tree: self,
            inputs,
            done: BTreeMap::new(),
            visiting: BTreeSet::new(),
        };
        for id in &self.order {
            eval.phase(id);
        }
        eval.done
    }
}

struct Eval<'a> {
    tree: &'a Tree,
    inputs: &'a dyn Inputs,
    done: BTreeMap<PhaseId, PhaseResult>,
    /// Phases being worked out right now. Meeting one of these again means a loop of
    /// references; the phase that asked is left waiting.
    visiting: BTreeSet<PhaseId>,
}

impl Eval<'_> {
    /// The pilots disqualified from a phase: by a ruling on it, or on any phase above it.
    fn disqualified(&self, id: &PhaseId) -> BTreeSet<PilotId> {
        self.tree
            .with_ancestors(id)
            .iter()
            .flat_map(|def| self.inputs.disqualified(&def.id))
            .collect()
    }

    /// Of the pilots disqualified from a phase, those whose earned place stays empty.
    fn kept_empty(&self, id: &PhaseId) -> BTreeSet<PilotId> {
        self.tree
            .with_ancestors(id)
            .iter()
            .flat_map(|def| self.inputs.places_kept_empty(&def.id))
            .collect()
    }

    /// The result for a phase, working it out first if needed. `None` when the phase does
    /// not exist or is part of a loop.
    fn phase(&mut self, id: &PhaseId) -> Option<&PhaseResult> {
        if !self.done.contains_key(id) {
            let def = self.tree.defs.get(id)?;
            if !self.visiting.insert(id.clone()) {
                return None;
            }
            let result = match &def.kind {
                PhaseKind::Heat { lineup } => self.heat(def, lineup),
                PhaseKind::Group { field, combine } => self.group(def, field, combine),
            };
            self.visiting.remove(id);
            self.done.insert(id.clone(), result);
        }
        self.done.get(id)
    }

    fn heat(&mut self, def: &PhaseDef, seats: &[gridfpv_events::HeatSeat]) -> PhaseResult {
        let mut out = PhaseResult::default();
        let disqualified = self.disqualified(&def.id);
        let races = self.inputs.saved_races(&def.id);
        for seat in seats {
            let who = match &seat.source {
                SeatSource::Pilot(pilot) => SeatWho::Pilot(pilot.clone()),
                SeatSource::Open => SeatWho::Open,
                SeatSource::FromPhase { phase, position } => match self.phase(phase) {
                    Some(source) if source.complete => match source.at(*position) {
                        Some(pilot) => SeatWho::Pilot(pilot.clone()),
                        None => SeatWho::Nobody,
                    },
                    _ => SeatWho::Waiting {
                        phase: phase.clone(),
                        position: *position,
                    },
                },
            };
            // A disqualified pilot is dropped from later lineups: a heat with no saved race
            // yet loses them. A heat already raced keeps its lineup as it was flown.
            let who = match who {
                SeatWho::Pilot(pilot) if races.is_empty() && disqualified.contains(&pilot) => {
                    SeatWho::Nobody
                }
                other => other,
            };
            match &who {
                // A pilot can only hold one seat; a second reference to them is ignored
                // here and refused when the race is started.
                SeatWho::Pilot(pilot) if !out.field.contains(pilot) => {
                    out.field.push(pilot.clone())
                }
                SeatWho::Waiting { .. } => out.waiting = true,
                _ => {}
            }
            out.lineup.push(ResolvedSeat {
                adapter: seat.adapter.clone(),
                seat: seat.seat.clone(),
                who,
            });
        }

        if !out.waiting {
            self.keep_seats(seats, &mut out.lineup);
        }

        let fallback = no_format();
        let format = self.tree.format_for(&def.id).unwrap_or(&fallback);
        let standings = heat_standings(&out.field, &races, format);
        // A heat whose seats have all resolved to nobody has nothing to race: it is done.
        let nobody_to_race =
            !out.lineup.is_empty() && out.lineup.iter().all(|seat| seat.who == SeatWho::Nobody);
        out.complete = !out.waiting
            && (nobody_to_race
                || heat_complete(
                    &format.completion,
                    &standings,
                    races.len(),
                    self.inputs.director_done(&def.id),
                ));
        let kept_empty = self.kept_empty(&def.id);
        let set = self.inputs.positions_set(&def.id);
        (out.ranking, out.places) = place(standings, &disqualified, &kept_empty, &set, |_, _| {
            Ordering::Equal
        });
        out
    }

    /// Keep each pilot on the seat they last flew on, where that can be done
    /// (`docs/primitives.md` §5.4). A seat is a channel, and a pilot who keeps theirs does
    /// not have to change video channel between heats.
    ///
    /// Only seats filled **from another phase** are moved around, and only among
    /// themselves: a seat the director gave to a named pilot stays theirs. When two pilots
    /// last flew on the same seat, the one seeded higher in this heat (earlier in its
    /// lineup) keeps it and the other takes a seat nobody asked for.
    fn keep_seats(&self, seats: &[gridfpv_events::HeatSeat], lineup: &mut [ResolvedSeat]) {
        // The seats that may be swapped, in lineup order, with the pilot each resolved to.
        let movable: Vec<usize> = (0..lineup.len())
            .filter(|i| {
                matches!(seats[*i].source, SeatSource::FromPhase { .. })
                    && matches!(lineup[*i].who, SeatWho::Pilot(_))
            })
            .collect();
        if movable.len() < 2 {
            return;
        }
        let pilots: Vec<PilotId> = movable
            .iter()
            .filter_map(|i| match &lineup[*i].who {
                SeatWho::Pilot(pilot) => Some(pilot.clone()),
                _ => None,
            })
            .collect();
        // Where each pilot ends up: an index into `movable`.
        let mut seat_of: Vec<Option<usize>> = vec![None; pilots.len()];
        let mut taken = vec![false; movable.len()];
        // First, in seed order, everyone whose last seat is one of these and still free.
        for (who, pilot) in pilots.iter().enumerate() {
            let Some((adapter, seat)) = self.inputs.last_seat(pilot) else {
                continue;
            };
            let wanted = movable
                .iter()
                .position(|i| lineup[*i].adapter == adapter && lineup[*i].seat == seat);
            if let Some(slot) = wanted.filter(|slot| !taken[*slot]) {
                taken[slot] = true;
                seat_of[who] = Some(slot);
            }
        }
        // Then everyone else, in seed order, onto the seats left, in lineup order.
        for placed in seat_of.iter_mut().filter(|placed| placed.is_none()) {
            let free = taken.iter().position(|t| !t);
            if let Some(slot) = free {
                taken[slot] = true;
                *placed = Some(slot);
            }
        }
        for (who, pilot) in pilots.into_iter().enumerate() {
            if let Some(slot) = seat_of[who] {
                lineup[movable[slot]].who = SeatWho::Pilot(pilot);
            }
        }
    }

    /// Resolve field slices to pilots, in order, each pilot once. The flag says whether a
    /// slice is still waiting on another phase.
    fn slices(&mut self, slices: &[FieldSlice]) -> (Vec<PilotId>, bool) {
        let mut field: Vec<PilotId> = Vec::new();
        let mut waiting = false;
        let add = |field: &mut Vec<PilotId>, pilot: &PilotId| {
            if !field.contains(pilot) {
                field.push(pilot.clone());
            }
        };
        for slice in slices {
            match slice {
                FieldSlice::Class(class) => {
                    for pilot in self.inputs.roster(class) {
                        add(&mut field, &pilot);
                    }
                }
                FieldSlice::Pilot(pilot) => add(&mut field, pilot),
                FieldSlice::Imported(pilots) => {
                    for pilot in pilots {
                        add(&mut field, pilot);
                    }
                }
                FieldSlice::FromPhase { phase, from, to } => {
                    let picked: Option<Vec<PilotId>> =
                        self.phase(phase).filter(|s| s.complete).map(|s| {
                            (*from..=*to)
                                .filter_map(|position| s.at(position).cloned())
                                .collect()
                        });
                    match picked {
                        Some(pilots) => {
                            for pilot in &pilots {
                                add(&mut field, pilot);
                            }
                        }
                        None => waiting = true,
                    }
                }
            }
        }
        (field, waiting)
    }

    fn group(
        &mut self,
        def: &PhaseDef,
        slices: &[FieldSlice],
        combine: &CombineRule,
    ) -> PhaseResult {
        let mut out = PhaseResult::default();
        let children: Vec<PhaseId> = self.tree.children(&def.id).map(|c| c.id.clone()).collect();
        for child in &children {
            self.phase(child);
        }

        // The field: the slices in order, or — with no slices — whoever is in the children,
        // in the seed order of the nearest phase above that has a field of its own (a
        // bracket's round takes its seeds from the bracket).
        if slices.is_empty() {
            for child in &children {
                match self.done.get(child) {
                    Some(result) => {
                        out.waiting |= result.waiting;
                        for pilot in &result.field {
                            if !out.field.contains(pilot) {
                                out.field.push(pilot.clone());
                            }
                        }
                    }
                    None => out.waiting = true,
                }
            }
            let above: Option<Vec<FieldSlice>> = self
                .tree
                .with_ancestors(&def.id)
                .iter()
                .skip(1)
                .find_map(|def| match &def.kind {
                    PhaseKind::Group { field, .. } if !field.is_empty() => Some(field.clone()),
                    _ => None,
                });
            if let Some(above) = above {
                let (seeds, _) = self.slices(&above);
                let seed = |pilot: &PilotId| {
                    seeds
                        .iter()
                        .position(|seeded| seeded == pilot)
                        .unwrap_or(usize::MAX)
                };
                out.field.sort_by_key(seed);
            }
        } else {
            let (field, waiting) = self.slices(slices);
            out.field = field;
            out.waiting |= waiting;
        }

        // Pool every race each pilot flew in the heats beneath this phase.
        let mut heats = Vec::new();
        self.tree
            .heats_under(&def.id, &mut heats, &mut BTreeSet::new());
        let pooled: Vec<Standing> = out
            .field
            .iter()
            .enumerate()
            .map(|(seed, pilot)| pool(pilot, seed, heats.iter().filter_map(|h| self.done.get(h))))
            .collect();

        // Where each pilot stands in the children, for the rules that read child rankings.
        let place_in_children = |pilot: &PilotId| -> Option<(usize, u32)> {
            children.iter().enumerate().find_map(|(index, child)| {
                let result = self.done.get(child)?;
                let placed = result.ranking.iter().find(|p| &p.pilot == pilot)?;
                Some((index, placed.position))
            })
        };
        let disqualified = self.disqualified(&def.id);
        let kept_empty = self.kept_empty(&def.id);
        (out.ranking, out.places) =
            place(
                pooled,
                &disqualified,
                &kept_empty,
                &[],
                |a, b| match combine {
                    CombineRule::Stack => {
                        let key = |s: &Standing| {
                            place_in_children(&s.pilot).unwrap_or((usize::MAX, u32::MAX))
                        };
                        key(a).cmp(&key(b))
                    }
                    CombineRule::ByHeatPosition => {
                        let key =
                            |s: &Standing| place_in_children(&s.pilot).map_or(u32::MAX, |(_, p)| p);
                        key(a).cmp(&key(b))
                    }
                    CombineRule::Measures(rule) => by_measures(rule, a, b),
                },
            );

        out.complete = !out.waiting
            && !children.is_empty()
            && children
                .iter()
                .all(|child| self.done.get(child).is_some_and(|result| result.complete));
        out
    }
}

/// Put results in their final order and number them 1 to n. The order is: pilots still in
/// the phase above disqualified pilots, pilots who flew above pilots who never did, then
/// `rule`, then seed. For a heat `results` arrive already sorted by its ranking rule, and
/// the stable sort keeps that order within each group. Last, each position the director set
/// by hand (`set`) moves that pilot to it.
///
/// Returns the ranking and the places it hands on (see [`PhaseResult::places`]): the same
/// order, except that a pilot in `kept_empty` holds the place they had earned as an empty
/// slot, so nobody moves up into it.
fn place(
    results: Vec<Standing>,
    disqualified: &BTreeSet<PilotId>,
    kept_empty: &BTreeSet<PilotId>,
    set: &[(PilotId, u32)],
    rule: impl Fn(&Standing, &Standing) -> Ordering,
) -> (Vec<Placed>, Vec<Option<PilotId>>) {
    let presorted: BTreeMap<PilotId, usize> = results
        .iter()
        .enumerate()
        .map(|(index, s)| (s.pilot.clone(), index))
        .collect();
    let order = |out: &dyn Fn(&Standing) -> bool| -> Vec<Standing> {
        let mut sorted = results.clone();
        sorted.sort_by(|a, b| {
            out(a)
                .cmp(&out(b))
                .then_with(|| b.flew.cmp(&a.flew))
                .then_with(|| rule(a, b))
                .then_with(|| presorted[&a.pilot].cmp(&presorted[&b.pilot]))
        });
        // The director's hand-set positions, lowest first so each lands where it was put.
        let mut moves: Vec<&(PilotId, u32)> = set.iter().collect();
        moves.sort_by_key(|(_, position)| *position);
        for (pilot, position) in moves {
            if let Some(from) = sorted.iter().position(|s| &s.pilot == pilot) {
                let moved = sorted.remove(from);
                let to = (position.saturating_sub(1) as usize).min(sorted.len());
                sorted.insert(to, moved);
            }
        }
        sorted
    };

    // What is handed on: pilots whose place is kept empty stay where they had earned it,
    // as an empty slot.
    let places = order(&|s| disqualified.contains(&s.pilot) && !kept_empty.contains(&s.pilot))
        .into_iter()
        .map(|s| {
            let empty = disqualified.contains(&s.pilot) && kept_empty.contains(&s.pilot);
            (!empty).then_some(s.pilot)
        })
        .collect();
    let ranking = order(&|s| disqualified.contains(&s.pilot))
        .into_iter()
        .enumerate()
        .map(|(index, mut result)| {
            result.position = index as u32 + 1;
            Placed {
                pilot: result.pilot.clone(),
                position: result.position,
                disqualified: disqualified.contains(&result.pilot),
                result,
            }
        })
        .collect();
    (ranking, places)
}

/// One pilot's numbers pooled across several heats.
fn pool<'a>(
    pilot: &PilotId,
    seed: usize,
    heats: impl Iterator<Item = &'a PhaseResult>,
) -> Standing {
    let mut pooled = Standing {
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
    };
    for heat in heats {
        let Some(placed) = heat.ranking.iter().find(|p| &p.pilot == pilot) else {
            continue;
        };
        let s = &placed.result;
        pooled.flew |= s.flew;
        pooled.wins += s.wins;
        pooled.points += s.points;
        pooled.laps = pooled.laps.max(s.laps);
        pooled.best_lap_micros = match (pooled.best_lap_micros, s.best_lap_micros) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        pooled.best_finish = match (pooled.best_finish, s.best_finish) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if s.last_finish.is_some() {
            pooled.last_finish = s.last_finish;
        }
        pooled.races.extend(s.races.iter().cloned());
    }
    pooled
}

/// Compare two pooled results on a ranking rule's measures, first one first.
fn by_measures(rule: &RankingRule, a: &Standing, b: &Standing) -> Ordering {
    let more_is_better = |m: Measure| matches!(m, Measure::Wins | Measure::Points | Measure::Laps);
    rule.sort
        .iter()
        .map(|measure| match (a.value(*measure), b.value(*measure)) {
            (Some(x), Some(y)) if more_is_better(*measure) => y.cmp(&x),
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .find(|order| *order != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::laps::{LapFilters, RaceLaps};
    use gridfpv_events::{Event, GateIndex, HeatSeat, Pass, RaceId, SourceTime};

    const S: i64 = 1_000_000;

    fn pid(id: &str) -> PhaseId {
        PhaseId(id.into())
    }

    fn pilot(name: &str) -> PilotId {
        PilotId(name.into())
    }

    /// Test inputs: rosters, and each heat's saved races as (pilot, crossing seconds).
    #[derive(Default)]
    struct World {
        rosters: BTreeMap<ClassId, Vec<PilotId>>,
        races: BTreeMap<PhaseId, Vec<Vec<RaceEntry>>>,
        done: BTreeSet<PhaseId>,
    }

    impl World {
        /// Save one race of a heat. Each pilot's crossings are given in seconds.
        fn race(&mut self, heat: &str, entries: &[(&str, &[i64])]) {
            let race = RaceId("r".into());
            let entries = entries
                .iter()
                .map(|(name, crossings)| {
                    let events: Vec<Event> = crossings
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
                    let laps =
                        RaceLaps::fold(events.iter().enumerate().map(|(i, e)| (i as u64, e))).seat(
                            &race,
                            &AdapterId("oft".into()),
                            &SeatRef("s".into()),
                            LapFilters::default(),
                        );
                    RaceEntry {
                        pilot: pilot(name),
                        laps,
                    }
                })
                .collect();
            self.races.entry(pid(heat)).or_default().push(entries);
        }

        /// Save a one-lap race of a heat that finishes in the order given.
        fn finish(&mut self, heat: &str, order: &[&str]) {
            let crossings: Vec<[i64; 2]> = (0..order.len()).map(|i| [0, 10 + i as i64]).collect();
            let entries: Vec<(&str, &[i64])> = order
                .iter()
                .zip(&crossings)
                .map(|(name, c)| (*name, c.as_slice()))
                .collect();
            self.race(heat, &entries);
        }
    }

    impl Inputs for World {
        fn roster(&self, class: &ClassId) -> Vec<PilotId> {
            self.rosters.get(class).cloned().unwrap_or_default()
        }
        fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>> {
            self.races.get(heat).cloned().unwrap_or_default()
        }
        fn director_done(&self, heat: &PhaseId) -> bool {
            self.done.contains(heat)
        }
    }

    fn one_race(laps: u32) -> Format {
        Format {
            name: "one race".into(),
            race: RaceRules {
                end: EndCondition::FirstToLaps(laps),
                staging_micros: 0,
                min_lap_micros: None,
                first_lap: Default::default(),
                stop_after_all_finish_micros: None,
                max_micros: None,
            },
            completion: CompletionRule::SavedRaces(1),
            ranking: Some(RankingRule {
                sort: vec![Measure::LastFinish],
            }),
            points: Vec::new(),
        }
    }

    fn named(name: &str) -> SeatSource {
        SeatSource::Pilot(pilot(name))
    }

    fn from(phase: &str, position: u32) -> SeatSource {
        SeatSource::FromPhase {
            phase: pid(phase),
            position,
        }
    }

    fn heat(id: &str, parent: Option<&str>, sources: Vec<SeatSource>) -> PhaseDef {
        PhaseDef {
            id: pid(id),
            label: id.to_uppercase(),
            parent: parent.map(pid),
            kind: PhaseKind::Heat {
                lineup: sources
                    .into_iter()
                    .enumerate()
                    .map(|(i, source)| HeatSeat {
                        adapter: AdapterId("oft".into()),
                        seat: SeatRef(format!("seat-{i}")),
                        source,
                    })
                    .collect(),
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
        }
    }

    fn group(
        id: &str,
        parent: Option<&str>,
        field: Vec<FieldSlice>,
        combine: CombineRule,
    ) -> PhaseDef {
        PhaseDef {
            id: pid(id),
            label: id.to_uppercase(),
            parent: parent.map(pid),
            kind: PhaseKind::Group { field, combine },
            format: None,
            false_start_limit: None,
            display_hint: None,
        }
    }

    fn with_format(mut def: PhaseDef, format: Format) -> PhaseDef {
        def.format = Some(format);
        def
    }

    fn order(result: &PhaseResult) -> Vec<&str> {
        result.ranking.iter().map(|p| p.pilot.0.as_str()).collect()
    }

    /// A four-pilot bracket: two semi-finals in a round, then a final and a small final.
    fn bracket() -> Tree {
        let mut tree = Tree::new();
        tree.put(with_format(
            group("bracket", None, vec![], CombineRule::Stack),
            one_race(1),
        ));
        // Children of the bracket are listed best first: the final, the small final, then
        // the semi-final round.
        tree.put(heat(
            "final",
            Some("bracket"),
            vec![from("semi-1", 1), from("semi-2", 1)],
        ));
        tree.put(heat(
            "small-final",
            Some("bracket"),
            vec![from("semi-1", 2), from("semi-2", 2)],
        ));
        tree.put(group(
            "semis",
            Some("bracket"),
            vec![],
            CombineRule::ByHeatPosition,
        ));
        tree.put(heat(
            "semi-1",
            Some("semis"),
            vec![named("ana"), named("dee")],
        ));
        tree.put(heat(
            "semi-2",
            Some("semis"),
            vec![named("ben"), named("cat")],
        ));
        tree
    }

    #[test]
    fn a_seat_reference_waits_then_resolves_when_its_source_heat_is_complete() {
        let tree = bracket();
        let mut world = World::default();
        let before = tree.evaluate(&world);
        let final_heat = &before[&pid("final")];
        assert!(final_heat.waiting);
        assert!(!final_heat.complete);
        assert_eq!(
            final_heat.lineup[0].who,
            SeatWho::Waiting {
                phase: pid("semi-1"),
                position: 1
            }
        );

        world.finish("semi-1", &["dee", "ana"]);
        let mid = tree.evaluate(&world);
        assert!(mid[&pid("semi-1")].complete);
        assert_eq!(
            mid[&pid("final")].lineup[0].who,
            SeatWho::Pilot(pilot("dee"))
        );
        assert!(
            mid[&pid("final")].waiting,
            "the other semi-final has not been flown"
        );

        world.finish("semi-2", &["ben", "cat"]);
        let after = tree.evaluate(&world);
        let final_heat = &after[&pid("final")];
        assert!(!final_heat.waiting);
        assert_eq!(final_heat.field, vec![pilot("dee"), pilot("ben")]);
        assert_eq!(
            after[&pid("small-final")].field,
            vec![pilot("ana"), pilot("cat")]
        );
    }

    #[test]
    fn a_bracket_ranks_every_pilot_once_by_how_far_they_got() {
        let tree = bracket();
        let mut world = World::default();
        world.finish("semi-1", &["dee", "ana"]);
        world.finish("semi-2", &["ben", "cat"]);
        world.finish("final", &["ben", "dee"]);
        world.finish("small-final", &["cat", "ana"]);
        let results = tree.evaluate(&world);
        let bracket = &results[&pid("bracket")];
        assert!(bracket.complete);
        assert_eq!(order(bracket), vec!["ben", "dee", "cat", "ana"]);
        assert_eq!(
            bracket
                .ranking
                .iter()
                .map(|p| p.position)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        // The semi-final round on its own: both winners, then both seconds, by seed.
        assert_eq!(
            order(&results[&pid("semis")]),
            vec!["dee", "ben", "ana", "cat"]
        );
    }

    #[test]
    fn a_bracket_is_not_complete_until_every_heat_is() {
        let tree = bracket();
        let mut world = World::default();
        world.finish("semi-1", &["dee", "ana"]);
        world.finish("semi-2", &["ben", "cat"]);
        world.finish("final", &["ben", "dee"]);
        let results = tree.evaluate(&world);
        assert!(results[&pid("semis")].complete);
        assert!(
            !results[&pid("bracket")].complete,
            "the small final is still to fly"
        );
    }

    /// Two mains. The B-Main's winner bumps up into the A-Main.
    fn mains() -> Tree {
        let mut tree = Tree::new();
        tree.put(with_format(
            group(
                "mains",
                None,
                vec![FieldSlice::Imported(
                    ["q1", "q2", "q3", "q4", "q5", "q6"]
                        .iter()
                        .map(|n| pilot(n))
                        .collect(),
                )],
                CombineRule::Stack,
            ),
            one_race(1),
        ));
        tree.put(heat(
            "a-main",
            Some("mains"),
            vec![named("q1"), named("q2"), named("q3"), from("b-main", 1)],
        ));
        tree.put(heat(
            "b-main",
            Some("mains"),
            vec![named("q4"), named("q5"), named("q6")],
        ));
        tree
    }

    #[test]
    fn mains_stack_one_to_n_and_a_bumped_pilot_is_placed_by_the_higher_main() {
        let tree = mains();
        let mut world = World::default();
        world.finish("b-main", &["q6", "q4", "q5"]);
        let mid = tree.evaluate(&world);
        assert_eq!(
            mid[&pid("a-main")].field,
            vec![pilot("q1"), pilot("q2"), pilot("q3"), pilot("q6")]
        );
        world.finish("a-main", &["q2", "q6", "q1", "q3"]);
        let results = tree.evaluate(&world);
        let mains = &results[&pid("mains")];
        assert!(mains.complete);
        // The A-Main in its order (with q6, who bumped up, second), then the pilots who
        // stayed in the B-Main in its order.
        assert_eq!(order(mains), vec!["q2", "q6", "q1", "q3", "q4", "q5"]);
        assert_eq!(
            mains.ranking.iter().map(|p| p.position).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn a_pilot_who_never_flew_drops_below_everyone_who_did_across_the_whole_phase() {
        let tree = mains();
        let mut world = World::default();
        world.finish("b-main", &["q6", "q4", "q5"]);
        // q1, the top qualifier, misses the A-Main.
        world.race(
            "a-main",
            &[
                ("q1", &[]),
                ("q2", &[0, 10]),
                ("q3", &[0, 12]),
                ("q6", &[0, 11]),
            ],
        );
        let results = tree.evaluate(&world);
        assert_eq!(
            order(&results[&pid("mains")]),
            vec!["q2", "q6", "q3", "q4", "q5", "q1"],
            "the no-show is last overall, not last of the A-Main"
        );
    }

    #[test]
    fn a_field_can_be_built_from_a_slice_of_another_phase_plus_named_pilots() {
        let mut tree = mains();
        tree.put(group(
            "next",
            None,
            vec![
                FieldSlice::FromPhase {
                    phase: pid("mains"),
                    from: 1,
                    to: 2,
                },
                FieldSlice::Pilot(pilot("wildcard")),
            ],
            CombineRule::Stack,
        ));
        let mut world = World::default();
        assert!(tree.evaluate(&world)[&pid("next")].waiting);
        world.finish("b-main", &["q6", "q4", "q5"]);
        world.finish("a-main", &["q2", "q6", "q1", "q3"]);
        let results = tree.evaluate(&world);
        let next = &results[&pid("next")];
        assert!(!next.waiting);
        assert_eq!(
            next.field,
            vec![pilot("q2"), pilot("q6"), pilot("wildcard")]
        );
    }

    #[test]
    fn a_time_trial_takes_each_pilots_best_run_across_rounds() {
        let best3 = RankingRule {
            sort: vec![Measure::BestConsecutive(3)],
        };
        let run = Format {
            name: "run".into(),
            race: RaceRules {
                end: EndCondition::Open,
                staging_micros: 0,
                min_lap_micros: None,
                first_lap: Default::default(),
                stop_after_all_finish_micros: None,
                max_micros: None,
            },
            completion: CompletionRule::SavedRaces(1),
            ranking: Some(best3.clone()),
            points: Vec::new(),
        };
        let mut tree = Tree::new();
        tree.put(with_format(
            group("qualifying", None, vec![], CombineRule::Measures(best3)),
            run,
        ));
        tree.put(heat(
            "round-1",
            Some("qualifying"),
            vec![named("ana"), named("ben")],
        ));
        tree.put(heat(
            "round-2",
            Some("qualifying"),
            vec![named("ana"), named("ben")],
        ));
        let mut world = World::default();
        // Round 1: ana 30, ben 28. Round 2: ana 27, ben 33.
        world.race(
            "round-1",
            &[("ana", &[0, 10, 20, 30]), ("ben", &[0, 9, 18, 28])],
        );
        world.race(
            "round-2",
            &[("ana", &[0, 9, 18, 27]), ("ben", &[0, 11, 22, 33])],
        );
        let results = tree.evaluate(&world);
        let qualifying = &results[&pid("qualifying")];
        assert!(qualifying.complete);
        assert_eq!(order(qualifying), vec!["ana", "ben"]);
        assert_eq!(
            qualifying.ranking[0]
                .result
                .value(Measure::BestConsecutive(3)),
            Some(27 * S)
        );
    }

    #[test]
    fn a_round_robin_sums_points_across_its_heats() {
        let mut points = one_race(1);
        points.points = vec![3, 1];
        let mut tree = Tree::new();
        tree.put(with_format(
            group(
                "round-robin",
                None,
                vec![],
                CombineRule::Measures(RankingRule {
                    sort: vec![Measure::Points, Measure::Wins],
                }),
            ),
            points,
        ));
        tree.put(heat(
            "ab",
            Some("round-robin"),
            vec![named("a"), named("b")],
        ));
        tree.put(heat(
            "ac",
            Some("round-robin"),
            vec![named("a"), named("c")],
        ));
        tree.put(heat(
            "bc",
            Some("round-robin"),
            vec![named("b"), named("c")],
        ));
        let mut world = World::default();
        world.finish("ab", &["b", "a"]);
        world.finish("ac", &["a", "c"]);
        world.finish("bc", &["b", "c"]);
        let results = tree.evaluate(&world);
        let table = &results[&pid("round-robin")];
        assert_eq!(order(table), vec!["b", "a", "c"]);
        assert_eq!(
            table
                .ranking
                .iter()
                .map(|p| p.result.points)
                .collect::<Vec<_>>(),
            vec![6, 4, 2]
        );
    }

    #[test]
    fn a_format_is_inherited_and_the_nearest_one_wins() {
        let mut tree = bracket();
        let mut chase = one_race(1);
        chase.name = "Chase the Ace".into();
        chase.completion = CompletionRule::AnyPilotReaches {
            measure: Measure::Wins,
            at_least: 2,
        };
        chase.ranking = Some(RankingRule {
            sort: vec![Measure::Wins, Measure::LastFinish],
        });
        let final_heat = tree.get(&pid("final")).unwrap().clone();
        tree.put(with_format(final_heat, chase));

        assert_eq!(tree.format_for(&pid("semi-1")).unwrap().name, "one race");
        assert_eq!(
            tree.format_for(&pid("final")).unwrap().name,
            "Chase the Ace"
        );
        assert!(tree.format_for(&pid("nowhere")).is_none());

        // The final now needs two wins; the small final is still one race.
        let mut world = World::default();
        world.finish("semi-1", &["dee", "ana"]);
        world.finish("semi-2", &["ben", "cat"]);
        world.finish("small-final", &["cat", "ana"]);
        world.finish("final", &["ben", "dee"]);
        assert!(!tree.evaluate(&world)[&pid("final")].complete);
        world.finish("final", &["dee", "ben"]);
        assert!(!tree.evaluate(&world)[&pid("final")].complete);
        world.finish("final", &["dee", "ben"]);
        let results = tree.evaluate(&world);
        assert!(results[&pid("final")].complete);
        assert_eq!(
            order(&results[&pid("bracket")]),
            vec!["dee", "ben", "cat", "ana"]
        );
    }

    #[test]
    fn a_class_roster_is_a_field_and_practice_seats_can_be_open() {
        let mut tree = Tree::new();
        tree.put(group(
            "practice",
            None,
            vec![FieldSlice::Class(ClassId("open".into()))],
            CombineRule::Stack,
        ));
        tree.put(heat(
            "free-fly",
            Some("practice"),
            vec![SeatSource::Open, SeatSource::Open],
        ));
        let mut world = World::default();
        world
            .rosters
            .insert(ClassId("open".into()), vec![pilot("ana"), pilot("ben")]);
        let results = tree.evaluate(&world);
        assert_eq!(
            results[&pid("practice")].field,
            vec![pilot("ana"), pilot("ben")]
        );
        let heat = &results[&pid("free-fly")];
        assert!(!heat.waiting);
        assert!(heat.field.is_empty());
        assert_eq!(heat.lineup[0].who, SeatWho::Open);
        // No format anywhere above: only the director ends it.
        assert!(!heat.complete);
        world.done.insert(pid("free-fly"));
        assert!(tree.evaluate(&world)[&pid("free-fly")].complete);
    }

    #[test]
    fn a_loop_or_a_missing_phase_leaves_a_seat_waiting_and_never_panics() {
        let mut tree = Tree::new();
        tree.put(heat("x", None, vec![from("y", 1)]));
        tree.put(heat("y", None, vec![from("x", 1)]));
        tree.put(heat("z", None, vec![from("gone", 1), named("ana")]));
        let results = tree.evaluate(&World::default());
        for id in ["x", "y", "z"] {
            assert!(results[&pid(id)].waiting, "{id} should be waiting");
            assert!(!results[&pid(id)].complete);
        }
        assert_eq!(results[&pid("z")].field, vec![pilot("ana")]);
    }

    #[test]
    fn every_ranking_holds_its_whole_field_exactly_once() {
        let tree = mains();
        let mut world = World::default();
        // Check at every stage of the event, not only the end.
        type Stage = Box<dyn Fn(&mut World)>;
        let stages: Vec<Stage> = vec![
            Box::new(|_| {}),
            Box::new(|w| w.finish("b-main", &["q6", "q4", "q5"])),
            Box::new(|w| {
                w.race(
                    "a-main",
                    &[
                        ("q1", &[]),
                        ("q2", &[0, 10]),
                        ("q3", &[0, 10]),
                        ("q6", &[0]),
                    ],
                )
            }),
        ];
        for stage in stages {
            stage(&mut world);
            for (id, result) in tree.evaluate(&world) {
                let mut ranked: Vec<&PilotId> = result.ranking.iter().map(|p| &p.pilot).collect();
                let mut field: Vec<&PilotId> = result.field.iter().collect();
                ranked.sort();
                field.sort();
                assert_eq!(ranked, field, "{id:?}: ranking and field differ");
                let positions: Vec<u32> = result.ranking.iter().map(|p| p.position).collect();
                assert_eq!(
                    positions,
                    (1..=result.field.len() as u32).collect::<Vec<_>>()
                );
            }
        }
    }
}
