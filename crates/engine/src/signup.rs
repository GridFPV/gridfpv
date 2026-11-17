//! Pilot sign-up (docs/primitives.md §11.5): pilots put themselves into heats.
//!
//! A phase that holds heats can be set so that pilots sign up for them. Sign-up writes
//! **plain heats**: a spot is a seat in a heat's lineup with a pilot on it, and a heat that
//! "appears" is a heat defined under the phase. Nothing downstream (the run queue, a race's
//! lineup, the ranking) knows or cares that a pilot got there by signing up.
//!
//! What the lineups cannot say is kept beside them ([`Signups`]): the phase's rules, a
//! pilot's own allowance of runs, a drop the director ruled counts as a run, and a heat
//! the director closed to pilots.
//!
//! Everything here is pure. [`EventState::signup`] turns one [`SignupCommand`] into the
//! facts it stands for, or says why not. [`EventState::signup_sheet`] is the read side:
//! the heats with who is on each seat, and how many runs each pilot has had and has left.
//!
//! # The rules
//!
//! - Every heat of the phase has the same seats, and a seat is the same channel in each.
//! - Heats are numbered in the order they appear and fly in that order.
//! - At most `open_heats` heats that are **open to pilots and not full** exist at once.
//!   When there are fewer, the next heat appears. So a new heat opens when one fills, and
//!   also when one is started or closed.
//! - A heat that is not full can still fly.
//! - A **pilot** can take a free seat, or leave theirs, while sign-up is open and the heat
//!   is open: not closed by the director, and never started. They must be in the phase's
//!   field, hold no more spots than `held_limit`, have runs left under `run_limit` (plus
//!   their own allowance), and keep `rest_heats` heats between any two of theirs.
//! - The **director** is bound only by what cannot be otherwise: a seat that is taken, a
//!   pilot already in the heat, and a heat that is racing or has been flown.
//! - A **run** is used by a race that is saved with the pilot in it, or by a drop the
//!   director ruled counts.

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{
    Event, HeatSeat, PhaseDef, PhaseId, PhaseKind, PilotId, SeatSource, SignupBy, SignupCommand,
    SignupRules, SignupSeat,
};

use crate::phase::PhaseResult;
use crate::race::RaceState;
use crate::state::{EventState, Refusal};

/// The most heats that can be open at once. Past this the setting is a slip of the hand.
pub const MAX_OPEN_HEATS: u32 = 20;
/// The most seats a heat can have.
pub const MAX_SEATS: usize = 16;
/// The most heats to sit out between two. More than this and nobody could fly twice.
pub const MAX_REST: u32 = 20;
/// How far a pilot's own allowance can move the limit, either way.
pub const MAX_EXTRA: i32 = 1000;
/// The most heats one sign-up phase can hold. It stops a run-away, nothing more.
const MAX_HEATS: usize = 2000;

/// What sign-up keeps beside the heats' lineups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signups {
    /// The rules of each phase that has sign-up switched on.
    pub rules: BTreeMap<PhaseId, SignupRules>,
    /// A pilot's own allowance of runs in a phase, added to the phase's limit.
    pub extra: BTreeMap<(PhaseId, PilotId), i32>,
    /// Drops the director ruled count as a run: the phase, the heat, the pilot.
    pub charged: Vec<(PhaseId, PhaseId, PilotId)>,
    /// Heats the director closed to pilots.
    pub closed: BTreeSet<PhaseId>,
}

impl Signups {
    /// Fold one fact in.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::SignupSet { phase, rules } => match rules {
                Some(rules) => {
                    self.rules.insert(phase.clone(), rules.clone());
                }
                None => {
                    self.rules.remove(phase);
                }
            },
            Event::SignupRunsSet {
                phase,
                pilot,
                extra,
            } => {
                let key = (phase.clone(), pilot.clone());
                if *extra == 0 {
                    self.extra.remove(&key);
                } else {
                    self.extra.insert(key, *extra);
                }
            }
            Event::SignupCharged { phase, heat, pilot } => {
                self.charged
                    .push((phase.clone(), heat.clone(), pilot.clone()));
            }
            Event::SignupHeatClosed { heat, closed } => {
                if *closed {
                    self.closed.insert(heat.clone());
                } else {
                    self.closed.remove(heat);
                }
            }
            // A phase that is removed takes what was kept for it along.
            Event::PhaseRemoved { phase } => {
                self.rules.remove(phase);
                self.closed.remove(phase);
                self.extra.retain(|(of, _), _| of != phase);
                self.charged
                    .retain(|(of, heat, _)| of != phase && heat != phase);
            }
            _ => {}
        }
    }
}

/// Where a sign-up heat stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatStatus {
    /// Pilots can join and leave.
    Open,
    /// Closed to pilots: the director closed it, or it was started and thrown out. The
    /// director can still change it.
    Closed,
    /// A race of it is counting down, running, or stopped and not yet decided.
    Racing,
    /// It has a saved race, or is complete. Nobody can change it.
    Flown,
}

impl HeatStatus {
    /// Whether a pilot can join or leave.
    pub fn open_to_pilots(self) -> bool {
        self == Self::Open
    }
    /// Whether the director can change who is in it.
    pub fn open_to_director(self) -> bool {
        matches!(self, Self::Open | Self::Closed)
    }
}

/// One heat of a sign-up phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeatSpots {
    pub heat: PhaseId,
    /// 1 for the first heat of the phase.
    pub number: u32,
    /// Who is on each seat, in the order of the rules' seats. `None` is a free seat.
    pub seats: Vec<Option<PilotId>>,
    pub status: HeatStatus,
}

impl HeatSpots {
    /// Whether every seat has a pilot.
    pub fn full(&self) -> bool {
        self.seats.iter().all(Option::is_some)
    }
    /// Whether a pilot is on one of its seats.
    pub fn holds(&self, pilot: &PilotId) -> bool {
        self.seats.iter().flatten().any(|on| on == pilot)
    }
}

/// One pilot's runs in a sign-up phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotRuns {
    pub pilot: PilotId,
    /// Runs used up: saved races they were in, and drops that count.
    pub used: u32,
    /// Spots held in heats that have not been flown.
    pub held: u32,
    /// Their own allowance, added to the phase's limit.
    pub extra: i32,
    /// The most runs they may have. `None`: no limit.
    pub allowed: Option<u32>,
}

/// A sign-up phase as it stands: its rules, its heats, and each pilot's runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignupSheet {
    pub rules: SignupRules,
    pub heats: Vec<HeatSpots>,
    /// Every pilot who may sign up, and anyone else who holds a spot or has used a run.
    pub pilots: Vec<PilotRuns>,
}

/// A heat while a change is being worked out: its definition, and what the sheet says.
#[derive(Debug, Clone)]
struct Working {
    def: PhaseDef,
    spots: HeatSpots,
    /// Whether the definition differs from the one in the tree (or is new).
    changed: bool,
}

/// The pilot named on a seat, if it names one.
fn pilot_on(lineup: &[HeatSeat], seat: &SignupSeat) -> Option<PilotId> {
    lineup
        .iter()
        .find(|entry| entry.adapter == seat.adapter && entry.seat == seat.seat)
        .and_then(|entry| match &entry.source {
            SeatSource::Pilot(pilot) => Some(pilot.clone()),
            _ => None,
        })
}

fn lineup_of(def: &PhaseDef) -> &[HeatSeat] {
    match &def.kind {
        PhaseKind::Heat { lineup } => lineup,
        PhaseKind::Group { .. } => &[],
    }
}

impl Working {
    /// Put a pilot on a seat, or (`None`) clear it. The lineup keeps the rules' seat order,
    /// and any seat that is not one of the rules' is left exactly as it was.
    fn set(&mut self, rules: &SignupRules, index: usize, pilot: Option<PilotId>) {
        self.spots.seats[index] = pilot;
        let old = lineup_of(&self.def).to_vec();
        let mut lineup: Vec<HeatSeat> = rules
            .seats
            .iter()
            .zip(&self.spots.seats)
            .filter_map(|(seat, on)| {
                on.as_ref().map(|pilot| HeatSeat {
                    adapter: seat.adapter.clone(),
                    seat: seat.seat.clone(),
                    source: SeatSource::Pilot(pilot.clone()),
                })
            })
            .collect();
        lineup.extend(old.into_iter().filter(|entry| {
            !rules
                .seats
                .iter()
                .any(|seat| seat.adapter == entry.adapter && seat.seat == entry.seat)
        }));
        self.def.kind = PhaseKind::Heat { lineup };
        self.changed = true;
    }
}

impl EventState {
    /// The phase a heat's sign-up belongs to: its parent, when that has sign-up on.
    fn signup_phase_of(&self, heat: &PhaseId) -> Result<PhaseId, Refusal> {
        let def = self
            .tree
            .get(heat)
            .ok_or_else(|| Refusal::UnknownPhase(heat.clone()))?;
        if !matches!(def.kind, PhaseKind::Heat { .. }) {
            return Err(Refusal::NotAHeat(heat.clone()));
        }
        match &def.parent {
            Some(parent) if self.signups.rules.contains_key(parent) => Ok(parent.clone()),
            _ => Err(Refusal::NoSignup(heat.clone())),
        }
    }

    fn signup_rules(&self, phase: &PhaseId) -> Result<&SignupRules, Refusal> {
        if self.tree.get(phase).is_none() {
            return Err(Refusal::UnknownPhase(phase.clone()));
        }
        self.signups
            .rules
            .get(phase)
            .ok_or_else(|| Refusal::NoSignup(phase.clone()))
    }

    /// Where one heat stands.
    fn signup_status(
        &self,
        heat: &PhaseId,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> HeatStatus {
        let mut any = false;
        let mut saved = false;
        for race in self.races.of_heat(heat) {
            any = true;
            if race.state.is_undecided() {
                return HeatStatus::Racing;
            }
            saved |= race.state == RaceState::Saved;
        }
        if saved || results.get(heat).is_some_and(|result| result.complete) {
            HeatStatus::Flown
        } else if any
            || self.signups.closed.contains(heat)
            // The heat on the timer is the one being called to the line: pilots cannot
            // join or leave it from a phone any more. The director still can.
            || self.on_timer().as_ref() == Some(heat)
        {
            HeatStatus::Closed
        } else {
            HeatStatus::Open
        }
    }

    /// The heats of a sign-up phase, as they stand in the tree.
    fn signup_heats(
        &self,
        phase: &PhaseId,
        rules: &SignupRules,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> Vec<Working> {
        self.tree
            .children(phase)
            .filter(|def| matches!(def.kind, PhaseKind::Heat { .. }))
            .enumerate()
            .map(|(index, def)| Working {
                def: def.clone(),
                spots: HeatSpots {
                    heat: def.id.clone(),
                    number: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    seats: rules
                        .seats
                        .iter()
                        .map(|seat| pilot_on(lineup_of(def), seat))
                        .collect(),
                    status: self.signup_status(&def.id, results),
                },
                changed: false,
            })
            .collect()
    }

    /// How many runs each pilot has used in a phase: saved races they were in under it,
    /// and drops that count.
    fn signup_used(&self, phase: &PhaseId, heats: &[Working]) -> BTreeMap<PilotId, u32> {
        let mut used: BTreeMap<PilotId, u32> = BTreeMap::new();
        for heat in heats {
            for race in self.races.of_heat(&heat.spots.heat) {
                if race.state != RaceState::Saved {
                    continue;
                }
                for pilot in race
                    .setup
                    .lineup
                    .iter()
                    .filter_map(|entry| entry.pilot.as_ref())
                {
                    *used.entry(pilot.clone()).or_default() += 1;
                }
            }
        }
        for (of, _, pilot) in &self.signups.charged {
            if of == phase {
                *used.entry(pilot.clone()).or_default() += 1;
            }
        }
        used
    }

    /// The most runs a pilot may have in a phase: the limit plus their own allowance, never
    /// below zero. `None`: no limit.
    fn signup_allowed(&self, phase: &PhaseId, rules: &SignupRules, pilot: &PilotId) -> Option<u32> {
        let extra = self
            .signups
            .extra
            .get(&(phase.clone(), pilot.clone()))
            .copied()
            .unwrap_or(0);
        rules.run_limit.map(|limit| {
            u32::try_from((i64::from(limit) + i64::from(extra)).max(0)).unwrap_or(u32::MAX)
        })
    }

    /// Who may sign up for a phase: its field, or with no field set, everyone in the event.
    ///
    /// "No field set" is read from the phase's definition, not from what it works out to: a
    /// phase with no field of its own gathers one from the pilots in its heats, so the
    /// first pilot to sign up would otherwise become the whole field.
    fn signup_field(
        &self,
        phase: &PhaseId,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> Vec<PilotId> {
        let has_field = self.tree.get(phase).is_some_and(|def| match &def.kind {
            PhaseKind::Group { field, .. } => !field.is_empty(),
            PhaseKind::Heat { .. } => false,
        });
        if has_field {
            return results
                .get(phase)
                .map(|result| result.field.clone())
                .unwrap_or_default();
        }
        let mut everyone = Vec::new();
        for pilots in self.rosters.values() {
            for pilot in pilots {
                if !everyone.contains(pilot) {
                    everyone.push(pilot.clone());
                }
            }
        }
        everyone
    }

    /// A sign-up phase as it stands. `None` for a phase with sign-up off.
    pub fn signup_sheet(
        &self,
        phase: &PhaseId,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> Option<SignupSheet> {
        let rules = self.signups.rules.get(phase)?;
        let heats = self.signup_heats(phase, rules, results);
        let used = self.signup_used(phase, &heats);
        let mut listed = self.signup_field(phase, results);
        let others = heats
            .iter()
            .flat_map(|heat| heat.spots.seats.iter().flatten())
            .chain(used.keys());
        for pilot in others {
            if !listed.contains(pilot) {
                listed.push(pilot.clone());
            }
        }
        let pilots = listed
            .into_iter()
            .map(|pilot| PilotRuns {
                used: used.get(&pilot).copied().unwrap_or(0),
                held: held(&heats, &pilot),
                extra: self
                    .signups
                    .extra
                    .get(&(phase.clone(), pilot.clone()))
                    .copied()
                    .unwrap_or(0),
                allowed: self.signup_allowed(phase, rules, &pilot),
                pilot,
            })
            .collect();
        Some(SignupSheet {
            rules: rules.clone(),
            heats: heats.into_iter().map(|heat| heat.spots).collect(),
            pilots,
        })
    }

    /// Why a pilot may not take this seat in this heat themselves, if they may not.
    #[allow(clippy::too_many_arguments)]
    fn signup_pilot_may_take(
        &self,
        phase: &PhaseId,
        rules: &SignupRules,
        heats: &[Working],
        target: usize,
        pilot: &PilotId,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> Result<(), Refusal> {
        if !rules.open {
            return Err(Refusal::SignupClosed(phase.clone()));
        }
        if !self.signup_field(phase, results).contains(pilot) {
            return Err(Refusal::PilotNotInPhase {
                phase: phase.clone(),
                pilot: pilot.clone(),
            });
        }
        let heat = &heats[target].spots;
        if !heat.status.open_to_pilots() {
            return Err(Refusal::HeatClosedToPilots(heat.heat.clone()));
        }
        let holding = held(heats, pilot);
        if let Some(limit) = rules.held_limit
            && holding >= limit
        {
            return Err(Refusal::HoldingTooMany {
                pilot: pilot.clone(),
                limit,
            });
        }
        if let Some(allowed) = self.signup_allowed(phase, rules, pilot) {
            let used = self
                .signup_used(phase, heats)
                .get(pilot)
                .copied()
                .unwrap_or(0);
            if used.saturating_add(holding) >= allowed {
                return Err(Refusal::NoRunsLeft {
                    pilot: pilot.clone(),
                    allowed,
                });
            }
        }
        if rules.rest_heats > 0 {
            let near = heats.iter().any(|other| {
                other.spots.heat != heat.heat
                    && other.spots.holds(pilot)
                    && other.spots.number.abs_diff(heat.number) <= rules.rest_heats
            });
            if near {
                return Err(Refusal::NeedsRest {
                    pilot: pilot.clone(),
                    rest: rules.rest_heats,
                });
            }
        }
        Ok(())
    }

    /// Put a pilot on a seat of one heat, in the working copy.
    #[allow(clippy::too_many_arguments)]
    fn signup_take(
        &self,
        phase: &PhaseId,
        rules: &SignupRules,
        heats: &mut [Working],
        target: usize,
        seat: &SignupSeat,
        pilot: &PilotId,
        by: SignupBy,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> Result<(), Refusal> {
        let heat_id = heats[target].spots.heat.clone();
        let index = rules
            .seats
            .iter()
            .position(|of| of == seat)
            .ok_or_else(|| Refusal::NotASignupSeat(heat_id.clone()))?;
        match by {
            SignupBy::Pilot => {
                self.signup_pilot_may_take(phase, rules, heats, target, pilot, results)?;
            }
            SignupBy::Director => {
                if !heats[target].spots.status.open_to_director() {
                    return Err(Refusal::HeatAlreadyRaced(heat_id));
                }
            }
        }
        if heats[target].spots.holds(pilot) {
            return Err(Refusal::AlreadyInHeat {
                heat: heat_id,
                pilot: pilot.clone(),
            });
        }
        if heats[target].spots.seats[index].is_some() {
            return Err(Refusal::SeatTaken(heat_id));
        }
        heats[target].set(rules, index, Some(pilot.clone()));
        Ok(())
    }

    /// Take a pilot off whichever seat of a heat they are on, in the working copy.
    fn signup_remove(
        rules: &SignupRules,
        heats: &mut [Working],
        target: usize,
        pilot: &PilotId,
    ) -> Result<usize, Refusal> {
        let index = heats[target]
            .spots
            .seats
            .iter()
            .position(|on| on.as_ref() == Some(pilot))
            .ok_or_else(|| Refusal::PilotNotInHeat {
                heat: heats[target].spots.heat.clone(),
                pilot: pilot.clone(),
            })?;
        heats[target].set(rules, index, None);
        Ok(index)
    }

    /// Open heats until there are `open_heats` that are open to pilots and not full.
    fn signup_top_up(&self, phase: &PhaseId, rules: &SignupRules, heats: &mut Vec<Working>) {
        let label = self
            .tree
            .get(phase)
            .map(|def| def.label.clone())
            .unwrap_or_default();
        let joinable = |heats: &[Working]| {
            heats
                .iter()
                .filter(|heat| heat.spots.status.open_to_pilots() && !heat.spots.full())
                .count()
        };
        let wanted = usize::try_from(rules.open_heats.min(MAX_OPEN_HEATS)).unwrap_or(1);
        while joinable(heats) < wanted && heats.len() < MAX_HEATS {
            let number = u32::try_from(heats.len() + 1).unwrap_or(u32::MAX);
            // An id nothing else in the tree has, in case a heat was removed by hand.
            let mut suffix = number;
            let id = loop {
                let id = PhaseId(format!("{}/heat-{suffix}", phase.0));
                if self.tree.get(&id).is_none() && !heats.iter().any(|heat| heat.def.id == id) {
                    break id;
                }
                suffix += 1;
            };
            heats.push(Working {
                def: PhaseDef {
                    id: id.clone(),
                    label: format!("{label} Heat {number}"),
                    parent: Some(phase.clone()),
                    kind: PhaseKind::Heat { lineup: Vec::new() },
                    format: None,
                    false_start_limit: None,
                    display_hint: None,
                },
                spots: HeatSpots {
                    heat: id,
                    number,
                    seats: vec![None; rules.seats.len()],
                    status: HeatStatus::Open,
                },
                changed: true,
            });
        }
    }

    /// The facts for every heat a change touched or opened, in heat order.
    fn signup_facts(heats: Vec<Working>) -> Vec<Event> {
        heats
            .into_iter()
            .filter(|heat| heat.changed)
            .map(|heat| Event::PhaseDefined { def: heat.def })
            .collect()
    }

    /// Check a sign-up command against the state. Gives the facts to append, in order, or
    /// the reason it is refused. Appends nothing itself.
    pub fn signup(&self, command: SignupCommand) -> Result<Vec<Event>, Refusal> {
        let results = self.results();
        match command {
            SignupCommand::Set { phase, rules } => {
                let def = self
                    .tree
                    .get(&phase)
                    .ok_or_else(|| Refusal::UnknownPhase(phase.clone()))?;
                if !matches!(def.kind, PhaseKind::Group { .. }) {
                    return Err(Refusal::NotAGroup(phase));
                }
                if self.rulings.is_locked(&self.tree, &phase) {
                    return Err(Refusal::Locked(phase));
                }
                let Some(rules) = rules else {
                    if !self.signups.rules.contains_key(&phase) {
                        return Err(Refusal::NoSignup(phase));
                    }
                    return Ok(vec![Event::SignupSet { phase, rules: None }]);
                };
                check_rules(&rules)?;
                let mut heats = self.signup_heats(&phase, &rules, &results);
                self.signup_top_up(&phase, &rules, &mut heats);
                let mut facts = vec![Event::SignupSet {
                    phase,
                    rules: Some(rules),
                }];
                facts.extend(Self::signup_facts(heats));
                Ok(facts)
            }
            SignupCommand::Take {
                heat,
                seat,
                pilot,
                by,
            } => {
                let phase = self.signup_phase_of(&heat)?;
                self.signup_unlocked(&phase)?;
                let rules = self.signup_rules(&phase)?.clone();
                let mut heats = self.signup_heats(&phase, &rules, &results);
                let target = position(&heats, &heat)?;
                self.signup_take(
                    &phase, &rules, &mut heats, target, &seat, &pilot, by, &results,
                )?;
                self.signup_top_up(&phase, &rules, &mut heats);
                Ok(Self::signup_facts(heats))
            }
            SignupCommand::TakeNext {
                phase,
                seat,
                pilot,
                by,
            } => {
                self.signup_unlocked(&phase)?;
                let rules = self.signup_rules(&phase)?.clone();
                let heats = self.signup_heats(&phase, &rules, &results);
                if !rules.seats.contains(&seat) {
                    return Err(Refusal::NotASignupSeat(phase));
                }
                // The first heat, in flying order, where taking the seat is allowed. The
                // reason a pilot hears, when there is none, is the one that is about them
                // (no runs left, sign-up closed) and not about one heat's seat.
                let mut about_pilot: Option<Refusal> = None;
                for target in 0..heats.len() {
                    let mut attempt = heats.clone();
                    match self.signup_take(
                        &phase,
                        &rules,
                        &mut attempt,
                        target,
                        &seat,
                        &pilot,
                        by,
                        &results,
                    ) {
                        Ok(()) => {
                            self.signup_top_up(&phase, &rules, &mut attempt);
                            return Ok(Self::signup_facts(attempt));
                        }
                        Err(
                            refusal @ (Refusal::SignupClosed(_)
                            | Refusal::PilotNotInPhase { .. }
                            | Refusal::HoldingTooMany { .. }
                            | Refusal::NoRunsLeft { .. }),
                        ) => {
                            about_pilot.get_or_insert(refusal);
                        }
                        Err(_) => {}
                    }
                }
                Err(about_pilot.unwrap_or(Refusal::NoFreeSpot(phase)))
            }
            SignupCommand::Leave { heat, pilot, by } => {
                let phase = self.signup_phase_of(&heat)?;
                self.signup_unlocked(&phase)?;
                let rules = self.signup_rules(&phase)?.clone();
                let mut heats = self.signup_heats(&phase, &rules, &results);
                let target = position(&heats, &heat)?;
                let status = heats[target].spots.status;
                match by {
                    SignupBy::Pilot if !status.open_to_pilots() => {
                        return Err(Refusal::HeatClosedToPilots(heat));
                    }
                    SignupBy::Director if !status.open_to_director() => {
                        return Err(Refusal::HeatAlreadyRaced(heat));
                    }
                    _ => {}
                }
                Self::signup_remove(&rules, &mut heats, target, &pilot)?;
                self.signup_top_up(&phase, &rules, &mut heats);
                Ok(Self::signup_facts(heats))
            }
            SignupCommand::Drop {
                heat,
                pilot,
                counted,
            } => {
                let phase = self.signup_phase_of(&heat)?;
                self.signup_unlocked(&phase)?;
                let rules = self.signup_rules(&phase)?.clone();
                let mut heats = self.signup_heats(&phase, &rules, &results);
                let target = position(&heats, &heat)?;
                if !heats[target].spots.status.open_to_director() {
                    return Err(Refusal::HeatAlreadyRaced(heat));
                }
                Self::signup_remove(&rules, &mut heats, target, &pilot)?;
                self.signup_top_up(&phase, &rules, &mut heats);
                let mut facts = Self::signup_facts(heats);
                if counted {
                    facts.push(Event::SignupCharged { phase, heat, pilot });
                }
                Ok(facts)
            }
            SignupCommand::Pull { pilot, from, to } => {
                let phase = self.signup_phase_of(&from)?;
                if self.signup_phase_of(&to)? != phase {
                    return Err(Refusal::NotSameSignup);
                }
                self.signup_unlocked(&phase)?;
                let rules = self.signup_rules(&phase)?.clone();
                let mut heats = self.signup_heats(&phase, &rules, &results);
                let source = position(&heats, &from)?;
                let target = position(&heats, &to)?;
                if heats[target].spots.number >= heats[source].spots.number {
                    return Err(Refusal::PullNotForward);
                }
                for index in [source, target] {
                    if !heats[index].spots.status.open_to_director() {
                        return Err(Refusal::HeatAlreadyRaced(heats[index].spots.heat.clone()));
                    }
                }
                if heats[target].spots.holds(&pilot) {
                    return Err(Refusal::AlreadyInHeat { heat: to, pilot });
                }
                // Onto the seat they had: their channel does not change.
                let seat = Self::signup_remove(&rules, &mut heats, source, &pilot)?;
                if heats[target].spots.seats[seat].is_some() {
                    return Err(Refusal::SeatTaken(to));
                }
                heats[target].set(&rules, seat, Some(pilot));
                self.signup_top_up(&phase, &rules, &mut heats);
                Ok(Self::signup_facts(heats))
            }
            SignupCommand::SetRuns {
                phase,
                pilot,
                extra,
            } => {
                self.signup_rules(&phase)?;
                self.signup_unlocked(&phase)?;
                if !(-MAX_EXTRA..=MAX_EXTRA).contains(&extra) {
                    return Err(Refusal::BadSignupRules(
                        "a pilot's own allowance of runs is out of range",
                    ));
                }
                Ok(vec![Event::SignupRunsSet {
                    phase,
                    pilot,
                    extra,
                }])
            }
            SignupCommand::CloseHeat { heat, closed } => {
                let phase = self.signup_phase_of(&heat)?;
                self.signup_unlocked(&phase)?;
                let rules = self.signup_rules(&phase)?.clone();
                let mut heats = self.signup_heats(&phase, &rules, &results);
                let target = position(&heats, &heat)?;
                // A heat that has been started is closed for good; there is nothing to set.
                if self.races.of_heat(&heat).next().is_some() {
                    return Err(Refusal::HeatAlreadyRaced(heat));
                }
                heats[target].spots.status = if closed {
                    HeatStatus::Closed
                } else {
                    HeatStatus::Open
                };
                self.signup_top_up(&phase, &rules, &mut heats);
                let mut facts = vec![Event::SignupHeatClosed { heat, closed }];
                facts.extend(Self::signup_facts(heats));
                Ok(facts)
            }
        }
    }

    fn signup_unlocked(&self, phase: &PhaseId) -> Result<(), Refusal> {
        if self.rulings.is_locked(&self.tree, phase) {
            return Err(Refusal::Locked(phase.clone()));
        }
        Ok(())
    }

    /// The heats to open because a heat of a sign-up phase is being put on the timer: it
    /// is closed to pilots from then on, so there may be room for another. A heat of the
    /// same phase that was on the timer before, and was never started or closed by the
    /// director, is open to pilots again. Empty for any other heat.
    pub fn signup_after_on_timer(&self, heat: &PhaseId) -> Vec<Event> {
        let Ok(phase) = self.signup_phase_of(heat) else {
            return Vec::new();
        };
        let Some(rules) = self.signups.rules.get(&phase).cloned() else {
            return Vec::new();
        };
        let results = self.results();
        let was = self.on_timer();
        let mut heats = self.signup_heats(&phase, &rules, &results);
        for other in heats.iter_mut() {
            let id = &other.spots.heat;
            if id == heat {
                if other.spots.status == HeatStatus::Open {
                    other.spots.status = HeatStatus::Closed;
                }
            } else if Some(id) == was.as_ref()
                && other.spots.status == HeatStatus::Closed
                && !self.signups.closed.contains(id)
                && self.races.of_heat(id).next().is_none()
            {
                other.spots.status = HeatStatus::Open;
            }
        }
        self.signup_top_up(&phase, &rules, &mut heats);
        Self::signup_facts(heats)
    }

    /// The heats to open because a heat of a sign-up phase is being started: it is no
    /// longer open to pilots, so there may be room for another. Empty for any other heat.
    pub fn signup_after_start(&self, heat: &PhaseId) -> Vec<Event> {
        let Ok(phase) = self.signup_phase_of(heat) else {
            return Vec::new();
        };
        let Some(rules) = self.signups.rules.get(&phase).cloned() else {
            return Vec::new();
        };
        let results = self.results();
        let mut heats = self.signup_heats(&phase, &rules, &results);
        if let Some(starting) = heats.iter_mut().find(|other| &other.spots.heat == heat) {
            starting.spots.status = HeatStatus::Racing;
        }
        self.signup_top_up(&phase, &rules, &mut heats);
        Self::signup_facts(heats)
    }
}

/// How many spots a pilot holds in heats that have not been flown.
fn held(heats: &[Working], pilot: &PilotId) -> u32 {
    let count = heats
        .iter()
        .filter(|heat| heat.spots.status != HeatStatus::Flown && heat.spots.holds(pilot))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn position(heats: &[Working], heat: &PhaseId) -> Result<usize, Refusal> {
    heats
        .iter()
        .position(|other| &other.spots.heat == heat)
        .ok_or_else(|| Refusal::NoSignup(heat.clone()))
}

/// Whether a set of rules can be used at all.
pub fn check_rules(rules: &SignupRules) -> Result<(), Refusal> {
    let bad = |why| Err(Refusal::BadSignupRules(why));
    if rules.seats.is_empty() {
        return bad("a sign-up phase needs at least one seat");
    }
    if rules.seats.len() > MAX_SEATS {
        return bad("a sign-up phase has too many seats");
    }
    let unique: BTreeSet<&SignupSeat> = rules.seats.iter().collect();
    if unique.len() != rules.seats.len() {
        return bad("a seat is listed twice");
    }
    if rules.open_heats == 0 {
        return bad("at least one heat must be open at once");
    }
    if rules.open_heats > MAX_OPEN_HEATS {
        return bad("too many heats would be open at once");
    }
    if rules.held_limit == Some(0) {
        return bad("the most spots held at once cannot be 0; leave it out for no limit");
    }
    if rules.run_limit == Some(0) {
        return bad("the most runs cannot be 0; leave it out for no limit");
    }
    if rules.rest_heats > MAX_REST {
        return bad("the rest between heats is too long");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{
        AdapterId, ClassId, CombineRule, Command, CompletionRule, EndCondition, FieldSlice,
        FirstLap, Format, Measure, RaceId, RaceRules, RankingRule, SeatRef,
    };

    const QUALI: &str = "quali";

    fn pid(id: &str) -> PhaseId {
        PhaseId(id.into())
    }
    fn pilot(name: &str) -> PilotId {
        PilotId(name.into())
    }
    fn seat(n: u32) -> SignupSeat {
        SignupSeat {
            adapter: AdapterId("gate".into()),
            seat: SeatRef(n.to_string()),
        }
    }
    fn heat(n: u32) -> PhaseId {
        PhaseId(format!("{QUALI}/heat-{n}"))
    }

    fn format() -> Format {
        Format {
            name: "one race each".into(),
            race: RaceRules {
                end: EndCondition::Open,
                staging_micros: 0,
                min_lap_micros: None,
                first_lap: FirstLap::default(),
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

    /// The usual rules: three seats, three heats open, no limits, sign-up open.
    fn rules() -> SignupRules {
        SignupRules {
            seats: vec![seat(0), seat(1), seat(2)],
            open_heats: 3,
            held_limit: None,
            run_limit: None,
            rest_heats: 0,
            open: true,
        }
    }

    /// A Director in miniature: a log and the state folded from it.
    #[derive(Default)]
    struct Desk {
        log: Vec<Event>,
        state: EventState,
    }

    impl Desk {
        fn append(&mut self, event: Event) {
            self.state.apply(self.log.len() as u64, Some(0), &event);
            self.log.push(event);
        }
        fn send(&mut self, command: Command) -> Result<Vec<Event>, Refusal> {
            let facts = self.state.check(command, 0)?;
            for fact in &facts {
                self.append(fact.clone());
            }
            Ok(facts)
        }
        fn signup(&mut self, command: SignupCommand) -> Result<Vec<Event>, Refusal> {
            self.send(Command::Signup(command))
        }
        fn take(&mut self, n: u32, s: u32, who: &str) -> Result<Vec<Event>, Refusal> {
            self.signup(SignupCommand::Take {
                heat: heat(n),
                seat: seat(s),
                pilot: pilot(who),
                by: SignupBy::Pilot,
            })
        }
        fn direct(&mut self, n: u32, s: u32, who: &str) -> Result<Vec<Event>, Refusal> {
            self.signup(SignupCommand::Take {
                heat: heat(n),
                seat: seat(s),
                pilot: pilot(who),
                by: SignupBy::Director,
            })
        }
        fn leave(&mut self, n: u32, who: &str) -> Result<Vec<Event>, Refusal> {
            self.signup(SignupCommand::Leave {
                heat: heat(n),
                pilot: pilot(who),
                by: SignupBy::Pilot,
            })
        }
        fn set(&mut self, rules: SignupRules) -> Result<Vec<Event>, Refusal> {
            self.signup(SignupCommand::Set {
                phase: pid(QUALI),
                rules: Some(rules),
            })
        }
        fn sheet(&self) -> SignupSheet {
            self.state
                .signup_sheet(&pid(QUALI), &self.state.results())
                .expect("sign-up is on")
        }
        /// Who is on each seat of each heat, as short text: `"ace,-,bee"`.
        fn spots(&self) -> Vec<String> {
            self.sheet()
                .heats
                .iter()
                .map(|heat| {
                    heat.seats
                        .iter()
                        .map(|on| on.as_ref().map_or("-", |pilot| pilot.0.as_str()))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect()
        }
        fn statuses(&self) -> Vec<HeatStatus> {
            self.sheet().heats.iter().map(|heat| heat.status).collect()
        }
        fn runs(&self, who: &str) -> PilotRuns {
            self.sheet()
                .pilots
                .into_iter()
                .find(|runs| runs.pilot == pilot(who))
                .unwrap_or_else(|| panic!("{who} is on the sheet"))
        }
        fn start(&mut self, n: u32) -> RaceId {
            self.send(Command::StartRace { heat: heat(n) }).unwrap();
            self.state
                .races
                .undecided()
                .expect("a race is on")
                .setup
                .race
                .clone()
        }
        fn fly(&mut self, n: u32) -> RaceId {
            let race = self.start(n);
            self.send(Command::StopRace { race: race.clone() }).unwrap();
            self.send(Command::SaveRace {
                race: race.clone(),
                needs_marshal: false,
                note: None,
            })
            .unwrap();
            race
        }
        fn discard(&mut self, race: &RaceId) {
            self.send(Command::DiscardRace {
                race: race.clone(),
                reason: None,
                note: None,
            })
            .unwrap();
        }
        /// The state folded again from the whole log.
        fn replayed(&self) -> EventState {
            let mut state = EventState::new();
            for (offset, event) in self.log.iter().enumerate() {
                state.apply(offset as u64, Some(0), event);
            }
            state
        }
    }

    /// An event with a qualifying phase for the class "open", whose roster is `pilots`.
    fn desk_with(pilots: &[&str]) -> Desk {
        let mut desk = Desk::default();
        desk.send(Command::Record(Box::new(Event::RosterSet {
            class: ClassId("open".into()),
            pilots: pilots.iter().map(|name| pilot(name)).collect(),
        })))
        .unwrap();
        desk.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: pid(QUALI),
                label: "Qualifying".into(),
                parent: None,
                kind: PhaseKind::Group {
                    field: vec![FieldSlice::Class(ClassId("open".into()))],
                    combine: CombineRule::Stack,
                },
                format: Some(format()),
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        desk
    }

    fn desk() -> Desk {
        let mut desk = desk_with(&["ace", "bee", "cat", "dan", "eve", "fox"]);
        desk.set(rules()).unwrap();
        desk
    }

    // ── Turning it on ───────────────────────────────────────────────────────

    #[test]
    fn turning_sign_up_on_opens_the_first_heats_numbered_and_empty() {
        let desk = desk();
        assert_eq!(desk.spots(), ["-,-,-", "-,-,-", "-,-,-"]);
        let sheet = desk.sheet();
        assert_eq!(
            sheet.heats.iter().map(|h| h.number).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(sheet.heats.iter().all(|h| h.status == HeatStatus::Open));
        // They are plain heats under the phase, named after it.
        let quali = pid(QUALI);
        let labels: Vec<&str> = desk
            .state
            .tree
            .children(&quali)
            .map(|def| def.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Qualifying Heat 1",
                "Qualifying Heat 2",
                "Qualifying Heat 3"
            ]
        );
        // An empty heat is not something to race, so the queue does not offer it.
        let results = desk.state.results();
        assert!(desk.state.queue(&results).is_empty());
    }

    #[test]
    fn it_opens_as_many_heats_as_the_rules_say() {
        for open_heats in [1, 2, 5] {
            let mut desk = desk_with(&["ace"]);
            desk.set(SignupRules {
                open_heats,
                ..rules()
            })
            .unwrap();
            assert_eq!(desk.spots().len(), open_heats as usize);
        }
    }

    #[test]
    fn rules_that_cannot_work_are_refused_and_nothing_is_opened() {
        let bad = [
            SignupRules {
                seats: Vec::new(),
                ..rules()
            },
            SignupRules {
                seats: vec![seat(0), seat(0)],
                ..rules()
            },
            SignupRules {
                seats: (0..17).map(seat).collect(),
                ..rules()
            },
            SignupRules {
                open_heats: 0,
                ..rules()
            },
            SignupRules {
                open_heats: MAX_OPEN_HEATS + 1,
                ..rules()
            },
            SignupRules {
                held_limit: Some(0),
                ..rules()
            },
            SignupRules {
                run_limit: Some(0),
                ..rules()
            },
            SignupRules {
                rest_heats: MAX_REST + 1,
                ..rules()
            },
        ];
        for rules in bad {
            let mut desk = desk_with(&["ace"]);
            let refused = desk.set(rules.clone());
            assert!(
                matches!(refused, Err(Refusal::BadSignupRules(_))),
                "{rules:?} gave {refused:?}"
            );
            assert!(desk.state.signups.rules.is_empty());
            assert_eq!(desk.state.tree.children(&pid(QUALI)).count(), 0);
        }
        // The edges that are fine.
        let mut desk = desk_with(&["ace"]);
        desk.set(SignupRules {
            seats: (0..16).map(seat).collect(),
            open_heats: MAX_OPEN_HEATS,
            held_limit: Some(1),
            run_limit: Some(1),
            rest_heats: MAX_REST,
            open: false,
        })
        .unwrap();
        assert_eq!(desk.spots().len(), MAX_OPEN_HEATS as usize);
    }

    #[test]
    fn sign_up_is_for_a_phase_that_holds_heats_and_exists() {
        let mut desk = desk();
        let on = |desk: &mut Desk, phase: PhaseId| {
            desk.signup(SignupCommand::Set {
                phase,
                rules: Some(rules()),
            })
        };
        assert_eq!(
            on(&mut desk, heat(1)),
            Err(Refusal::NotAGroup(heat(1))),
            "a heat cannot have sign-up of its own"
        );
        assert_eq!(
            on(&mut desk, pid("nowhere")),
            Err(Refusal::UnknownPhase(pid("nowhere")))
        );
    }

    #[test]
    fn a_locked_phase_takes_no_sign_up_changes() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.send(Command::Record(Box::new(Event::PhaseLocked {
            phase: pid(QUALI),
        })))
        .unwrap();
        let locked = Err(Refusal::Locked(pid(QUALI)));
        assert_eq!(desk.take(1, 1, "bee"), locked);
        assert_eq!(desk.leave(1, "ace"), locked);
        assert_eq!(desk.set(rules()), locked);
        assert_eq!(
            desk.signup(SignupCommand::Drop {
                heat: heat(1),
                pilot: pilot("ace"),
                counted: false
            }),
            locked
        );
        assert_eq!(
            desk.signup(SignupCommand::SetRuns {
                phase: pid(QUALI),
                pilot: pilot("ace"),
                extra: 1
            }),
            locked
        );
        assert_eq!(
            desk.signup(SignupCommand::CloseHeat {
                heat: heat(2),
                closed: true
            }),
            locked
        );
        assert_eq!(desk.spots()[0], "ace,-,-");
    }

    #[test]
    fn turning_it_off_keeps_the_heats_and_who_is_in_them() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.signup(SignupCommand::Set {
            phase: pid(QUALI),
            rules: None,
        })
        .unwrap();
        assert!(
            desk.state
                .signup_sheet(&pid(QUALI), &desk.state.results())
                .is_none()
        );
        assert_eq!(desk.state.tree.children(&pid(QUALI)).count(), 3);
        // With it off, nobody can sign up, and it cannot be turned off twice.
        assert_eq!(desk.take(1, 1, "bee"), Err(Refusal::NoSignup(heat(1))));
        assert_eq!(
            desk.signup(SignupCommand::Set {
                phase: pid(QUALI),
                rules: None
            }),
            Err(Refusal::NoSignup(pid(QUALI)))
        );
        // On again: the pilot who was in is still in, and no heat is doubled.
        desk.set(rules()).unwrap();
        assert_eq!(desk.spots(), ["ace,-,-", "-,-,-", "-,-,-"]);
    }

    #[test]
    fn changing_the_rules_keeps_who_is_where() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(2, 2, "bee").unwrap();
        desk.set(SignupRules {
            open_heats: 5,
            run_limit: Some(2),
            ..rules()
        })
        .unwrap();
        assert_eq!(
            desk.spots(),
            ["ace,-,-", "-,-,bee", "-,-,-", "-,-,-", "-,-,-"]
        );
        // Fewer open heats: none is taken away.
        desk.set(SignupRules {
            open_heats: 1,
            ..rules()
        })
        .unwrap();
        assert_eq!(desk.spots().len(), 5);
    }

    // ── Taking a spot ───────────────────────────────────────────────────────

    #[test]
    fn a_pilot_takes_a_free_seat_in_a_heat() {
        let mut desk = desk();
        let facts = desk.take(2, 1, "ace").unwrap();
        assert_eq!(desk.spots(), ["-,-,-", "-,ace,-", "-,-,-"]);
        // One fact: the heat, defined again with the pilot on the seat.
        assert_eq!(facts.len(), 1);
        match &facts[0] {
            Event::PhaseDefined { def } => {
                assert_eq!(def.id, heat(2));
                assert_eq!(
                    lineup_of(def),
                    [HeatSeat {
                        adapter: AdapterId("gate".into()),
                        seat: SeatRef("1".into()),
                        source: SeatSource::Pilot(pilot("ace")),
                    }]
                );
            }
            other => panic!("{other:?}"),
        }
        // The heat can now be raced, with just that pilot.
        let results = desk.state.results();
        assert_eq!(desk.state.queue(&results), [heat(2)]);
    }

    #[test]
    fn a_seat_that_is_taken_cannot_be_taken_again_by_anyone() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        assert_eq!(desk.take(1, 0, "bee"), Err(Refusal::SeatTaken(heat(1))));
        assert_eq!(desk.direct(1, 0, "bee"), Err(Refusal::SeatTaken(heat(1))));
        // Not even by the pilot who has it.
        assert!(desk.take(1, 0, "ace").is_err());
        assert_eq!(desk.spots()[0], "ace,-,-");
    }

    #[test]
    fn a_pilot_cannot_be_in_one_heat_twice() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        let twice = Err(Refusal::AlreadyInHeat {
            heat: heat(1),
            pilot: pilot("ace"),
        });
        assert_eq!(desk.take(1, 1, "ace"), twice);
        assert_eq!(desk.direct(1, 2, "ace"), twice);
    }

    #[test]
    fn a_seat_that_is_not_one_of_the_phases_seats_is_refused() {
        let mut desk = desk();
        assert_eq!(
            desk.take(1, 7, "ace"),
            Err(Refusal::NotASignupSeat(heat(1)))
        );
        let other_timer = SignupSeat {
            adapter: AdapterId("other".into()),
            seat: SeatRef("0".into()),
        };
        assert_eq!(
            desk.signup(SignupCommand::Take {
                heat: heat(1),
                seat: other_timer,
                pilot: pilot("ace"),
                by: SignupBy::Director,
            }),
            Err(Refusal::NotASignupSeat(heat(1)))
        );
    }

    #[test]
    fn a_heat_that_is_not_a_sign_up_heat_is_refused() {
        let mut desk = desk();
        assert_eq!(desk.take(9, 0, "ace"), Err(Refusal::UnknownPhase(heat(9))));
        assert_eq!(
            desk.signup(SignupCommand::Take {
                heat: pid(QUALI),
                seat: seat(0),
                pilot: pilot("ace"),
                by: SignupBy::Pilot,
            }),
            Err(Refusal::NotAHeat(pid(QUALI)))
        );
        // A heat somewhere else in the event, with no sign-up above it.
        desk.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: pid("final"),
                label: "Final".into(),
                parent: None,
                kind: PhaseKind::Heat { lineup: Vec::new() },
                format: Some(format()),
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        assert_eq!(
            desk.signup(SignupCommand::Take {
                heat: pid("final"),
                seat: seat(0),
                pilot: pilot("ace"),
                by: SignupBy::Director,
            }),
            Err(Refusal::NoSignup(pid("final")))
        );
    }

    #[test]
    fn only_a_pilot_registered_for_the_phase_can_sign_themselves_up() {
        let mut desk = desk();
        assert_eq!(
            desk.take(1, 0, "stranger"),
            Err(Refusal::PilotNotInPhase {
                phase: pid(QUALI),
                pilot: pilot("stranger")
            })
        );
        // The director can put anyone in.
        desk.direct(1, 0, "stranger").unwrap();
        assert_eq!(desk.spots()[0], "stranger,-,-");
        // And that pilot is then on the sheet, with their spot counted.
        assert_eq!(desk.runs("stranger").held, 1);
    }

    #[test]
    fn with_no_field_set_on_the_phase_anyone_in_the_event_can_sign_up() {
        let mut desk = Desk::default();
        desk.send(Command::Record(Box::new(Event::RosterSet {
            class: ClassId("open".into()),
            pilots: vec![pilot("ace")],
        })))
        .unwrap();
        desk.send(Command::Record(Box::new(Event::RosterSet {
            class: ClassId("spec".into()),
            pilots: vec![pilot("bee"), pilot("ace")],
        })))
        .unwrap();
        desk.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: pid(QUALI),
                label: "Practice".into(),
                parent: None,
                kind: PhaseKind::Group {
                    field: Vec::new(),
                    combine: CombineRule::Stack,
                },
                format: Some(format()),
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        desk.set(rules()).unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.take(1, 1, "bee").unwrap();
        assert!(desk.take(1, 2, "stranger").is_err());
        // Each pilot is listed once, however many classes they are in.
        let listed: Vec<String> = desk
            .sheet()
            .pilots
            .iter()
            .map(|p| p.pilot.0.clone())
            .collect();
        assert_eq!(listed, ["ace", "bee"]);
    }

    #[test]
    fn while_sign_up_is_closed_pilots_cannot_join_or_leave_but_the_director_can() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            open: false,
            ..rules()
        })
        .unwrap();
        assert_eq!(
            desk.take(1, 0, "ace"),
            Err(Refusal::SignupClosed(pid(QUALI)))
        );
        desk.direct(1, 0, "ace").unwrap();
        // Opened: the pilot can act.
        desk.set(rules()).unwrap();
        desk.take(1, 1, "bee").unwrap();
        // Closed again: a pilot who is in can still leave. Closing sign-up stops new
        // spots; it does not hold anyone to one.
        desk.set(SignupRules {
            open: false,
            ..rules()
        })
        .unwrap();
        desk.leave(1, "bee").unwrap();
        assert_eq!(desk.spots()[0], "ace,-,-");
    }

    // ── Heats appear as seats fill ──────────────────────────────────────────

    #[test]
    fn a_new_heat_opens_when_one_fills_and_not_before() {
        // Ryan's example, with three heats open at once.
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(3, 0, "bee").unwrap();
        desk.take(3, 1, "cat").unwrap();
        desk.take(3, 2, "dan").unwrap();
        // Heat 3 is full, so heats 1 and 2 are the only ones not full: heat 4 opens.
        assert_eq!(desk.spots(), ["ace,-,-", "-,-,-", "bee,cat,dan", "-,-,-"]);
        desk.take(2, 0, "bee").unwrap();
        desk.take(2, 1, "cat").unwrap();
        assert_eq!(desk.spots().len(), 4, "heat 2 is not full yet");
        let facts = desk.take(2, 2, "dan").unwrap();
        assert_eq!(
            desk.spots(),
            ["ace,-,-", "bee,cat,dan", "bee,cat,dan", "-,-,-", "-,-,-"]
        );
        // The fact that filled the heat and the fact that opened the next are one step.
        assert_eq!(facts.len(), 2);
        assert!(matches!(&facts[1], Event::PhaseDefined { def } if def.id == heat(5)));
    }

    #[test]
    fn there_are_never_more_open_heats_that_are_not_full_than_the_rules_say() {
        let mut desk = desk_with(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        desk.set(SignupRules {
            seats: vec![seat(0), seat(1)],
            open_heats: 2,
            ..rules()
        })
        .unwrap();
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
        // Every pilot takes the next free spot on a seat, over and over.
        for round in 0..6 {
            for (index, name) in names.iter().enumerate() {
                let _ = desk.signup(SignupCommand::TakeNext {
                    phase: pid(QUALI),
                    seat: seat(u32::try_from((index + round) % 2).unwrap()),
                    pilot: pilot(name),
                    by: SignupBy::Pilot,
                });
                let sheet = desk.sheet();
                let not_full = sheet
                    .heats
                    .iter()
                    .filter(|heat| heat.status == HeatStatus::Open && !heat.full())
                    .count();
                assert_eq!(
                    not_full,
                    2,
                    "after {name} in round {round}: {:?}",
                    desk.spots()
                );
            }
        }
    }

    #[test]
    fn a_spot_given_up_does_not_take_a_heat_away() {
        let mut desk = desk();
        for (seat, who) in ["ace", "bee", "cat"].iter().enumerate() {
            desk.take(1, u32::try_from(seat).unwrap(), who).unwrap();
        }
        assert_eq!(desk.spots().len(), 4);
        desk.leave(1, "bee").unwrap();
        // Four heats are now not full. None is removed: pilots may be in them.
        assert_eq!(desk.spots(), ["ace,-,cat", "-,-,-", "-,-,-", "-,-,-"]);
        // And filling heat 1 again opens no fifth: four not-full heats went to three.
        desk.take(1, 1, "dan").unwrap();
        assert_eq!(desk.spots().len(), 4);
    }

    #[test]
    fn starting_a_heat_that_is_not_full_opens_the_next_one() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        assert_eq!(desk.spots().len(), 3);
        let race = desk.start(1);
        // Heat 1 is racing, so it is no longer open to pilots: heat 4 appears.
        assert_eq!(desk.spots().len(), 4);
        assert_eq!(desk.statuses()[0], HeatStatus::Racing);
        desk.send(Command::StopRace { race: race.clone() }).unwrap();
        desk.send(Command::SaveRace {
            race,
            needs_marshal: false,
            note: None,
        })
        .unwrap();
        assert_eq!(desk.statuses()[0], HeatStatus::Flown);
        assert_eq!(desk.spots().len(), 4, "saving it opens nothing more");
    }

    #[test]
    fn starting_a_heat_outside_sign_up_opens_nothing() {
        let mut desk = desk();
        desk.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: pid("final"),
                label: "Final".into(),
                parent: None,
                kind: PhaseKind::Heat {
                    lineup: vec![HeatSeat {
                        adapter: AdapterId("gate".into()),
                        seat: SeatRef("0".into()),
                        source: SeatSource::Pilot(pilot("ace")),
                    }],
                },
                format: Some(format()),
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        let before = desk.log.len();
        desk.send(Command::StartRace { heat: pid("final") })
            .unwrap();
        assert!(
            desk.log[before..]
                .iter()
                .all(|fact| !matches!(fact, Event::PhaseDefined { .. }))
        );
        assert_eq!(desk.spots().len(), 3);
    }

    // ── Next free spot on a seat ────────────────────────────────────────────

    #[test]
    fn next_free_takes_the_earliest_heat_with_that_seat_free() {
        let mut desk = desk();
        let next = |desk: &mut Desk, s: u32, who: &str| {
            desk.signup(SignupCommand::TakeNext {
                phase: pid(QUALI),
                seat: seat(s),
                pilot: pilot(who),
                by: SignupBy::Pilot,
            })
        };
        next(&mut desk, 1, "ace").unwrap();
        next(&mut desk, 1, "bee").unwrap();
        next(&mut desk, 1, "cat").unwrap();
        next(&mut desk, 0, "dan").unwrap();
        assert_eq!(desk.spots(), ["dan,ace,-", "-,bee,-", "-,cat,-"]);
        // Seat 1 is taken in every open heat, and none is full: there is no spot on it.
        assert_eq!(
            next(&mut desk, 1, "eve"),
            Err(Refusal::NoFreeSpot(pid(QUALI)))
        );
        // A pilot already in the first heats with that seat free is sent past them.
        next(&mut desk, 2, "ace").unwrap();
        assert_eq!(desk.spots()[1], "-,bee,ace", "ace is in heat 1 already");
    }

    #[test]
    fn next_free_skips_heats_closed_to_pilots() {
        let mut desk = desk();
        desk.signup(SignupCommand::CloseHeat {
            heat: heat(1),
            closed: true,
        })
        .unwrap();
        desk.signup(SignupCommand::TakeNext {
            phase: pid(QUALI),
            seat: seat(0),
            pilot: pilot("ace"),
            by: SignupBy::Pilot,
        })
        .unwrap();
        assert_eq!(desk.spots()[..2], ["-,-,-", "ace,-,-"]);
    }

    #[test]
    fn next_free_tells_a_pilot_what_is_wrong_with_them_not_with_a_seat() {
        let mut desk = desk_with(&["ace"]);
        desk.set(SignupRules {
            run_limit: Some(1),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        let next = |desk: &mut Desk, who: &str| {
            desk.signup(SignupCommand::TakeNext {
                phase: pid(QUALI),
                seat: seat(1),
                pilot: pilot(who),
                by: SignupBy::Pilot,
            })
        };
        assert_eq!(
            next(&mut desk, "ace"),
            Err(Refusal::NoRunsLeft {
                pilot: pilot("ace"),
                allowed: 1
            })
        );
        assert!(matches!(
            next(&mut desk, "stranger"),
            Err(Refusal::PilotNotInPhase { .. })
        ));
        desk.set(SignupRules {
            open: false,
            run_limit: Some(1),
            ..rules()
        })
        .unwrap();
        assert_eq!(
            next(&mut desk, "ace"),
            Err(Refusal::SignupClosed(pid(QUALI)))
        );
        // A seat that is not one of the phase's is said as that.
        assert_eq!(
            desk.signup(SignupCommand::TakeNext {
                phase: pid(QUALI),
                seat: seat(9),
                pilot: pilot("ace"),
                by: SignupBy::Director,
            }),
            Err(Refusal::NotASignupSeat(pid(QUALI)))
        );
    }

    // ── Limits ──────────────────────────────────────────────────────────────

    #[test]
    fn a_pilot_can_hold_spots_in_several_heats_even_back_to_back() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(2, 0, "ace").unwrap();
        desk.take(3, 1, "ace").unwrap();
        assert_eq!(desk.runs("ace").held, 3);
        assert_eq!(desk.runs("ace").allowed, None);
    }

    #[test]
    fn the_spots_held_at_once_are_capped_and_free_up_when_a_heat_is_flown() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            held_limit: Some(2),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.take(2, 0, "ace").unwrap();
        let too_many = Err(Refusal::HoldingTooMany {
            pilot: pilot("ace"),
            limit: 2,
        });
        assert_eq!(desk.take(3, 0, "ace"), too_many);
        // Another pilot is not held to that pilot's count.
        desk.take(3, 0, "bee").unwrap();
        // Heat 1 is flown: one spot is no longer held, so there is room for another.
        desk.fly(1);
        assert_eq!(desk.runs("ace").held, 1);
        assert_eq!(desk.runs("ace").used, 1);
        desk.take(3, 1, "ace").unwrap();
        assert_eq!(desk.take(4, 0, "ace"), too_many);
        // Giving one up makes room too.
        desk.leave(3, "ace").unwrap();
        desk.take(4, 0, "ace").unwrap();
    }

    #[test]
    fn the_total_runs_are_capped_counting_what_is_held_and_what_is_flown() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            run_limit: Some(2),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.take(2, 0, "ace").unwrap();
        let none_left = Err(Refusal::NoRunsLeft {
            pilot: pilot("ace"),
            allowed: 2,
        });
        // Two held: a third would be a third run.
        assert_eq!(desk.take(3, 0, "ace"), none_left);
        desk.fly(1);
        desk.fly(2);
        assert_eq!(desk.runs("ace").used, 2);
        assert_eq!(desk.runs("ace").held, 0);
        // Both flown: still no third.
        assert_eq!(desk.take(3, 0, "ace"), none_left);
        // Leaving a held spot gives the run back; a flown one is spent.
        desk.take(3, 0, "bee").unwrap();
        desk.leave(3, "bee").unwrap();
        desk.take(3, 1, "bee").unwrap();
    }

    #[test]
    fn a_race_that_is_thrown_out_uses_no_run() {
        let mut desk = desk_with(&["ace"]);
        desk.set(SignupRules {
            run_limit: Some(1),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        let race = desk.start(1);
        desk.discard(&race);
        assert_eq!(desk.runs("ace").used, 0);
        assert_eq!(
            desk.runs("ace").held,
            1,
            "they are still in the heat, to fly it again"
        );
        // Flown again and saved: now it is a run.
        let again = desk.fly(1);
        assert_eq!(desk.runs("ace").used, 1);
        // A saved race thrown out afterwards gives the run back.
        desk.discard(&again);
        assert_eq!(desk.runs("ace").used, 0);
    }

    #[test]
    fn the_director_raises_the_limit_for_everyone_or_for_one_pilot() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            run_limit: Some(1),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.take(1, 1, "bee").unwrap();
        assert!(desk.take(2, 0, "ace").is_err());
        // One more for Ace only.
        desk.signup(SignupCommand::SetRuns {
            phase: pid(QUALI),
            pilot: pilot("ace"),
            extra: 1,
        })
        .unwrap();
        assert_eq!(desk.runs("ace").allowed, Some(2));
        assert_eq!(desk.runs("ace").extra, 1);
        assert_eq!(desk.runs("bee").allowed, Some(1));
        desk.take(2, 0, "ace").unwrap();
        assert!(desk.take(2, 1, "bee").is_err());
        // One more for everyone: Ace keeps their extra on top.
        desk.set(SignupRules {
            run_limit: Some(2),
            ..rules()
        })
        .unwrap();
        assert_eq!(desk.runs("ace").allowed, Some(3));
        assert_eq!(desk.runs("bee").allowed, Some(2));
        desk.take(2, 1, "bee").unwrap();
        desk.take(3, 0, "ace").unwrap();
        // Back to no extra.
        desk.signup(SignupCommand::SetRuns {
            phase: pid(QUALI),
            pilot: pilot("ace"),
            extra: 0,
        })
        .unwrap();
        assert_eq!(desk.runs("ace").allowed, Some(2));
        assert!(desk.state.signups.extra.is_empty());
    }

    #[test]
    fn a_pilots_allowance_can_take_runs_away_but_never_below_none() {
        let mut desk = desk_with(&["ace"]);
        desk.set(SignupRules {
            run_limit: Some(2),
            ..rules()
        })
        .unwrap();
        let set = |desk: &mut Desk, extra: i32| {
            desk.signup(SignupCommand::SetRuns {
                phase: pid(QUALI),
                pilot: pilot("ace"),
                extra,
            })
        };
        set(&mut desk, -1).unwrap();
        assert_eq!(desk.runs("ace").allowed, Some(1));
        set(&mut desk, -50).unwrap();
        assert_eq!(desk.runs("ace").allowed, Some(0));
        assert!(desk.take(1, 0, "ace").is_err());
        // Out of range is a slip, not a setting.
        assert!(matches!(
            set(&mut desk, MAX_EXTRA + 1),
            Err(Refusal::BadSignupRules(_))
        ));
        assert!(matches!(
            set(&mut desk, -MAX_EXTRA - 1),
            Err(Refusal::BadSignupRules(_))
        ));
        set(&mut desk, MAX_EXTRA).unwrap();
        // With no limit on the phase, an allowance changes nothing.
        desk.set(rules()).unwrap();
        assert_eq!(desk.runs("ace").allowed, None);
        desk.take(1, 0, "ace").unwrap();
    }

    #[test]
    fn an_allowance_needs_a_sign_up_phase() {
        let mut desk = desk_with(&["ace"]);
        assert_eq!(
            desk.signup(SignupCommand::SetRuns {
                phase: pid(QUALI),
                pilot: pilot("ace"),
                extra: 1
            }),
            Err(Refusal::NoSignup(pid(QUALI)))
        );
        assert_eq!(
            desk.signup(SignupCommand::SetRuns {
                phase: pid("nowhere"),
                pilot: pilot("ace"),
                extra: 1
            }),
            Err(Refusal::UnknownPhase(pid("nowhere")))
        );
    }

    #[test]
    fn the_director_is_not_held_to_the_limits() {
        let mut desk = desk_with(&["ace"]);
        desk.set(SignupRules {
            held_limit: Some(1),
            run_limit: Some(1),
            rest_heats: 2,
            open: false,
            ..rules()
        })
        .unwrap();
        desk.direct(1, 0, "ace").unwrap();
        desk.direct(2, 0, "ace").unwrap();
        desk.direct(3, 0, "ace").unwrap();
        assert_eq!(desk.runs("ace").held, 3);
    }

    #[test]
    fn the_rest_rule_keeps_heats_apart_and_is_off_by_default() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            rest_heats: 1,
            open_heats: 5,
            ..rules()
        })
        .unwrap();
        desk.take(3, 0, "ace").unwrap();
        let rest = Err(Refusal::NeedsRest {
            pilot: pilot("ace"),
            rest: 1,
        });
        assert_eq!(desk.take(2, 0, "ace"), rest, "the heat before");
        assert_eq!(desk.take(4, 0, "ace"), rest, "the heat after");
        desk.take(1, 0, "ace").unwrap();
        desk.take(5, 0, "ace").unwrap();
        // It counts heats that are already flown too: the pilot has just landed.
        let mut flown = desk_with(&["ace"]);
        flown
            .set(SignupRules {
                rest_heats: 1,
                ..rules()
            })
            .unwrap();
        flown.take(1, 0, "ace").unwrap();
        flown.fly(1);
        assert!(matches!(
            flown.take(2, 0, "ace"),
            Err(Refusal::NeedsRest { .. })
        ));
        flown.take(3, 0, "ace").unwrap();
        // A rest of two.
        let mut two = desk_with(&["ace"]);
        two.set(SignupRules {
            rest_heats: 2,
            open_heats: 5,
            ..rules()
        })
        .unwrap();
        two.take(1, 0, "ace").unwrap();
        assert!(two.take(2, 0, "ace").is_err());
        assert!(two.take(3, 0, "ace").is_err());
        two.take(4, 0, "ace").unwrap();
    }

    // ── Leaving, dropping, closing ──────────────────────────────────────────

    #[test]
    fn a_pilot_leaves_their_spot_and_only_their_own_heat() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(1, 1, "bee").unwrap();
        desk.leave(1, "ace").unwrap();
        assert_eq!(desk.spots()[0], "-,bee,-");
        assert_eq!(
            desk.leave(1, "ace"),
            Err(Refusal::PilotNotInHeat {
                heat: heat(1),
                pilot: pilot("ace")
            })
        );
        assert!(desk.leave(2, "bee").is_err());
        // The seat is free for someone else.
        desk.take(1, 0, "cat").unwrap();
    }

    #[test]
    fn once_a_heat_is_started_pilots_cannot_join_or_leave_and_nor_can_the_director() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        let race = desk.start(1);
        assert_eq!(
            desk.take(1, 1, "bee"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        assert_eq!(
            desk.leave(1, "ace"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        assert_eq!(
            desk.direct(1, 1, "bee"),
            Err(Refusal::HeatAlreadyRaced(heat(1)))
        );
        assert_eq!(
            desk.signup(SignupCommand::Drop {
                heat: heat(1),
                pilot: pilot("ace"),
                counted: false
            }),
            Err(Refusal::HeatAlreadyRaced(heat(1)))
        );
        // Saved: still nobody.
        desk.send(Command::StopRace { race: race.clone() }).unwrap();
        desk.send(Command::SaveRace {
            race,
            needs_marshal: false,
            note: None,
        })
        .unwrap();
        assert_eq!(
            desk.take(1, 1, "bee"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        assert_eq!(
            desk.direct(1, 1, "bee"),
            Err(Refusal::HeatAlreadyRaced(heat(1)))
        );
        assert_eq!(desk.spots()[0], "ace,-,-");
    }

    #[test]
    fn a_heat_started_and_thrown_out_is_closed_to_pilots_but_the_director_can_still_change_it() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        let race = desk.start(1);
        desk.discard(&race);
        assert_eq!(desk.statuses()[0], HeatStatus::Closed);
        assert_eq!(
            desk.take(1, 1, "bee"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        assert_eq!(
            desk.leave(1, "ace"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        desk.direct(1, 1, "bee").unwrap();
        assert_eq!(desk.spots()[0], "ace,bee,-");
        // It cannot be opened to pilots again: it has been to the line.
        assert_eq!(
            desk.signup(SignupCommand::CloseHeat {
                heat: heat(1),
                closed: false
            }),
            Err(Refusal::HeatAlreadyRaced(heat(1)))
        );
    }

    #[test]
    fn the_director_closes_a_heat_to_pilots_and_opens_it_again() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        let close = |desk: &mut Desk, closed: bool| {
            desk.signup(SignupCommand::CloseHeat {
                heat: heat(1),
                closed,
            })
        };
        close(&mut desk, true).unwrap();
        assert_eq!(desk.statuses()[0], HeatStatus::Closed);
        assert_eq!(
            desk.take(1, 1, "bee"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        assert_eq!(
            desk.leave(1, "ace"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        // The director can still add a pilot, up to the start.
        desk.direct(1, 1, "bee").unwrap();
        // Closing a heat that was not full makes room for another to open.
        assert_eq!(desk.spots().len(), 4);
        close(&mut desk, false).unwrap();
        assert_eq!(desk.statuses()[0], HeatStatus::Open);
        desk.take(1, 2, "cat").unwrap();
        // Opening it again brought the count of not-full heats to four; none is removed.
        assert_eq!(desk.spots().len(), 4);
    }

    #[test]
    fn the_director_drops_a_pilot_and_says_whether_it_counts() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            run_limit: Some(2),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.take(1, 1, "bee").unwrap();
        let drop = |desk: &mut Desk, n: u32, who: &str, counted: bool| {
            desk.signup(SignupCommand::Drop {
                heat: heat(n),
                pilot: pilot(who),
                counted,
            })
        };
        // Not counted: as if they had never signed up.
        drop(&mut desk, 1, "ace", false).unwrap();
        assert_eq!(desk.runs("ace").used, 0);
        assert_eq!(desk.runs("ace").held, 0);
        // Counted: the run is spent.
        drop(&mut desk, 1, "bee", true).unwrap();
        assert_eq!(desk.runs("bee").used, 1);
        assert_eq!(desk.runs("bee").held, 0);
        assert_eq!(desk.spots()[0], "-,-,-");
        // One run left for Bee, two for Ace.
        desk.take(2, 0, "bee").unwrap();
        assert!(desk.take(3, 0, "bee").is_err());
        desk.take(2, 1, "ace").unwrap();
        desk.take(3, 1, "ace").unwrap();
        // A pilot who is not in the heat cannot be dropped from it, counted or not.
        assert!(drop(&mut desk, 1, "ace", true).is_err());
        assert_eq!(desk.runs("ace").used, 0, "a refused drop charges nothing");
    }

    #[test]
    fn a_drop_works_on_a_heat_closed_to_pilots() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.signup(SignupCommand::CloseHeat {
            heat: heat(1),
            closed: true,
        })
        .unwrap();
        desk.signup(SignupCommand::Drop {
            heat: heat(1),
            pilot: pilot("ace"),
            counted: false,
        })
        .unwrap();
        assert_eq!(desk.spots()[0], "-,-,-");
    }

    // ── Pulling forward ─────────────────────────────────────────────────────

    #[test]
    fn the_director_pulls_a_pilot_forward_onto_the_seat_they_had() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(3, 1, "bee").unwrap();
        desk.take(3, 2, "cat").unwrap();
        let pull = |desk: &mut Desk, who: &str, from: u32, to: u32| {
            desk.signup(SignupCommand::Pull {
                pilot: pilot(who),
                from: heat(from),
                to: heat(to),
            })
        };
        let facts = pull(&mut desk, "bee", 3, 1).unwrap();
        assert_eq!(desk.spots(), ["ace,bee,-", "-,-,-", "-,-,cat"]);
        assert_eq!(facts.len(), 2, "both heats are defined again, in one step");
        pull(&mut desk, "cat", 3, 1).unwrap();
        assert_eq!(desk.spots()[0], "ace,bee,cat");
        // Heat 1 filled: with heats 2 and 3 not full, a fourth opens.
        assert_eq!(desk.spots().len(), 4);
    }

    #[test]
    fn a_pull_is_only_forward_onto_a_free_seat_and_never_changes_the_channel() {
        let mut desk = desk();
        desk.take(1, 1, "ace").unwrap();
        desk.take(2, 1, "bee").unwrap();
        desk.take(2, 0, "ace").unwrap();
        let pull = |desk: &mut Desk, who: &str, from: u32, to: u32| {
            desk.signup(SignupCommand::Pull {
                pilot: pilot(who),
                from: heat(from),
                to: heat(to),
            })
        };
        // Bee is on seat 1 in heat 2; seat 1 of heat 1 is Ace's. Seats 0 and 2 are free
        // there, but that would move Bee to another channel.
        assert_eq!(
            pull(&mut desk, "bee", 2, 1),
            Err(Refusal::SeatTaken(heat(1)))
        );
        // Backwards, or to the same heat, is not a pull.
        assert_eq!(pull(&mut desk, "ace", 1, 2), Err(Refusal::PullNotForward));
        assert_eq!(pull(&mut desk, "ace", 1, 1), Err(Refusal::PullNotForward));
        // A pilot already in the earlier heat cannot be pulled into it again.
        assert_eq!(
            pull(&mut desk, "ace", 2, 1),
            Err(Refusal::AlreadyInHeat {
                heat: heat(1),
                pilot: pilot("ace")
            })
        );
        // A pilot who is not in the later heat.
        assert!(matches!(
            pull(&mut desk, "cat", 2, 1),
            Err(Refusal::PilotNotInHeat { .. })
        ));
        // Nothing moved in any of that.
        assert_eq!(desk.spots(), ["-,ace,-", "ace,bee,-", "-,-,-"]);
    }

    #[test]
    fn a_pull_cannot_touch_a_heat_that_has_started_or_another_phase() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(2, 1, "bee").unwrap();
        desk.start(1);
        assert_eq!(
            desk.signup(SignupCommand::Pull {
                pilot: pilot("bee"),
                from: heat(2),
                to: heat(1)
            }),
            Err(Refusal::HeatAlreadyRaced(heat(1)))
        );
        // A second sign-up phase: pilots are not pulled across.
        let mut two = desk_with(&["ace"]);
        two.set(rules()).unwrap();
        two.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: pid("practice"),
                label: "Practice".into(),
                parent: None,
                kind: PhaseKind::Group {
                    field: Vec::new(),
                    combine: CombineRule::Stack,
                },
                format: Some(format()),
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        two.signup(SignupCommand::Set {
            phase: pid("practice"),
            rules: Some(rules()),
        })
        .unwrap();
        two.take(2, 0, "ace").unwrap();
        assert_eq!(
            two.signup(SignupCommand::Pull {
                pilot: pilot("ace"),
                from: heat(2),
                to: pid("practice/heat-1")
            }),
            Err(Refusal::NotSameSignup)
        );
    }

    #[test]
    fn a_pull_is_possible_on_a_heat_the_director_closed() {
        let mut desk = desk();
        desk.take(2, 0, "ace").unwrap();
        desk.signup(SignupCommand::CloseHeat {
            heat: heat(1),
            closed: true,
        })
        .unwrap();
        desk.signup(SignupCommand::Pull {
            pilot: pilot("ace"),
            from: heat(2),
            to: heat(1),
        })
        .unwrap();
        assert_eq!(desk.spots()[0], "ace,-,-");
    }

    // ── The sheet, and the log ──────────────────────────────────────────────

    #[test]
    fn the_sheet_lists_every_registered_pilot_and_anyone_else_who_is_in() {
        let mut desk = desk_with(&["ace", "bee"]);
        desk.set(SignupRules {
            run_limit: Some(3),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.direct(1, 1, "guest").unwrap();
        desk.fly(1);
        desk.take(2, 0, "ace").unwrap();
        let sheet = desk.sheet();
        let listed: Vec<(&str, u32, u32, Option<u32>)> = sheet
            .pilots
            .iter()
            .map(|p| (p.pilot.0.as_str(), p.used, p.held, p.allowed))
            .collect();
        assert_eq!(
            listed,
            [
                ("ace", 1, 1, Some(3)),
                ("bee", 0, 0, Some(3)),
                ("guest", 1, 0, Some(3))
            ]
        );
    }

    #[test]
    fn a_seat_someone_else_put_in_a_heat_by_hand_is_left_alone() {
        // The director can still edit a heat's lineup on the Phases page. A seat that is
        // not one of the sign-up seats must survive a pilot signing up beside it.
        let mut desk = desk();
        desk.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: heat(1),
                label: "Qualifying Heat 1".into(),
                parent: Some(pid(QUALI)),
                kind: PhaseKind::Heat {
                    lineup: vec![HeatSeat {
                        adapter: AdapterId("bench".into()),
                        seat: SeatRef("seat-1".into()),
                        source: SeatSource::Open,
                    }],
                },
                format: None,
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        desk.take(1, 1, "ace").unwrap();
        desk.take(1, 0, "bee").unwrap();
        let def = desk.state.tree.get(&heat(1)).unwrap().clone();
        let lineup = lineup_of(&def);
        assert_eq!(lineup.len(), 3);
        // The sign-up seats come first, in the rules' order, then the one that was there.
        assert_eq!(lineup[0].source, SeatSource::Pilot(pilot("bee")));
        assert_eq!(lineup[1].source, SeatSource::Pilot(pilot("ace")));
        assert_eq!(lineup[2].adapter, AdapterId("bench".into()));
        assert_eq!(lineup[2].source, SeatSource::Open);
        desk.leave(1, "ace").unwrap();
        assert_eq!(lineup_of(desk.state.tree.get(&heat(1)).unwrap()).len(), 2);
    }

    #[test]
    fn a_heat_removed_by_hand_does_not_break_the_numbering_or_the_next_one() {
        let mut desk = desk();
        desk.send(Command::Record(Box::new(Event::PhaseRemoved {
            phase: heat(2),
        })))
        .unwrap();
        // Two heats are left, numbered 1 and 2 by where they stand.
        assert_eq!(
            desk.sheet()
                .heats
                .iter()
                .map(|h| h.number)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        // The next change tops up to three again, with an id nothing else has.
        desk.take(1, 0, "ace").unwrap();
        let ids: Vec<String> = desk
            .sheet()
            .heats
            .iter()
            .map(|h| h.heat.0.clone())
            .collect();
        assert_eq!(ids.len(), 3);
        let unique: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), 3);
        assert!(ids.contains(&"quali/heat-1".to_string()));
        assert!(ids.contains(&"quali/heat-3".to_string()));
    }

    #[test]
    fn removing_the_phase_forgets_its_sign_up() {
        let mut desk = desk_with(&["ace"]);
        desk.set(SignupRules {
            open_heats: 1,
            run_limit: Some(2),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.signup(SignupCommand::SetRuns {
            phase: pid(QUALI),
            pilot: pilot("ace"),
            extra: 1,
        })
        .unwrap();
        desk.signup(SignupCommand::Drop {
            heat: heat(1),
            pilot: pilot("ace"),
            counted: true,
        })
        .unwrap();
        desk.signup(SignupCommand::CloseHeat {
            heat: heat(1),
            closed: true,
        })
        .unwrap();
        // Take the heats away, then the phase.
        let quali = pid(QUALI);
        let heats: Vec<PhaseId> = desk
            .state
            .tree
            .children(&quali)
            .map(|def| def.id.clone())
            .collect();
        for heat in heats {
            desk.send(Command::Record(Box::new(Event::PhaseRemoved {
                phase: heat,
            })))
            .unwrap();
        }
        desk.send(Command::Record(Box::new(Event::PhaseRemoved {
            phase: pid(QUALI),
        })))
        .unwrap();
        assert_eq!(desk.state.signups, Signups::default());
    }

    #[test]
    fn sign_up_facts_cannot_be_logged_raw_only_through_a_command() {
        let mut desk = desk();
        for fact in [
            Event::SignupSet {
                phase: pid(QUALI),
                rules: None,
            },
            Event::SignupRunsSet {
                phase: pid(QUALI),
                pilot: pilot("ace"),
                extra: 99,
            },
            Event::SignupCharged {
                phase: pid(QUALI),
                heat: heat(1),
                pilot: pilot("ace"),
            },
            Event::SignupHeatClosed {
                heat: heat(1),
                closed: true,
            },
        ] {
            assert_eq!(
                desk.send(Command::Record(Box::new(fact))),
                Err(Refusal::NotACommand)
            );
        }
    }

    #[test]
    fn a_refused_command_appends_nothing() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        let before = desk.log.len();
        assert!(desk.take(1, 0, "bee").is_err());
        assert!(desk.take(1, 1, "stranger").is_err());
        assert!(desk.leave(2, "ace").is_err());
        assert!(desk.take(8, 0, "bee").is_err());
        assert_eq!(desk.log.len(), before);
    }

    #[test]
    fn the_whole_day_replays_from_the_log_to_the_same_state() {
        let mut desk = desk_with(&["ace", "bee", "cat", "dan"]);
        desk.set(SignupRules {
            run_limit: Some(3),
            held_limit: Some(2),
            ..rules()
        })
        .unwrap();
        desk.take(1, 0, "ace").unwrap();
        desk.take(1, 1, "bee").unwrap();
        desk.take(1, 2, "cat").unwrap();
        desk.take(2, 0, "dan").unwrap();
        desk.leave(1, "bee").unwrap();
        desk.direct(1, 1, "dan").unwrap();
        desk.signup(SignupCommand::CloseHeat {
            heat: heat(2),
            closed: true,
        })
        .unwrap();
        desk.signup(SignupCommand::Drop {
            heat: heat(2),
            pilot: pilot("dan"),
            counted: true,
        })
        .unwrap();
        desk.signup(SignupCommand::SetRuns {
            phase: pid(QUALI),
            pilot: pilot("dan"),
            extra: 2,
        })
        .unwrap();
        desk.fly(1);
        desk.take(3, 1, "bee").unwrap();
        desk.take(4, 1, "bee").unwrap();
        desk.signup(SignupCommand::Pull {
            pilot: pilot("bee"),
            from: heat(4),
            to: heat(2),
        })
        .unwrap();
        let replayed = desk.replayed();
        assert_eq!(replayed, desk.state);
        assert_eq!(
            replayed.signup_sheet(&pid(QUALI), &replayed.results()),
            Some(desk.sheet())
        );
        assert_eq!(desk.runs("dan").used, 2, "one flown, one drop that counts");
        assert_eq!(desk.runs("dan").allowed, Some(5));
    }

    #[test]
    fn a_flown_sign_up_heat_ranks_like_any_other_heat() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.take(1, 1, "bee").unwrap();
        desk.fly(1);
        let results = desk.state.results();
        let ranked: Vec<&str> = results[&heat(1)]
            .ranking
            .iter()
            .map(|placed| placed.pilot.0.as_str())
            .collect();
        assert_eq!(ranked.len(), 2);
        assert!(ranked.contains(&"ace") && ranked.contains(&"bee"));
        assert!(results[&heat(1)].complete);
        // The phase above still evaluates with empty heats under it.
        assert!(results.contains_key(&pid(QUALI)));
    }

    // ── On the timer ─────────────────────────────────────────────────────────

    impl Desk {
        fn put(&mut self, n: u32) -> Result<Vec<Event>, Refusal> {
            self.send(Command::PutOnTimer { heat: heat(n) })
        }
    }
    use HeatStatus::{Closed, Flown, Open, Racing};

    #[test]
    fn nothing_is_on_the_timer_until_the_director_puts_a_heat_there() {
        let mut desk = desk();
        assert_eq!(desk.state.on_timer(), None);
        assert_eq!(
            desk.put(2).unwrap()[0],
            Event::HeatOnTimer { heat: heat(2) }
        );
        assert_eq!(desk.state.on_timer(), Some(heat(2)));
        // Putting the same heat there again says nothing new.
        assert_eq!(desk.put(2).unwrap(), Vec::<Event>::new());
        assert_eq!(desk.replayed().on_timer(), Some(heat(2)));
    }

    #[test]
    fn only_a_heat_of_the_event_can_go_on_the_timer() {
        let mut desk = desk();
        assert_eq!(
            desk.send(Command::PutOnTimer { heat: pid(QUALI) }),
            Err(Refusal::NotAHeat(pid(QUALI)))
        );
        assert_eq!(
            desk.send(Command::PutOnTimer { heat: pid("nope") }),
            Err(Refusal::UnknownPhase(pid("nope")))
        );
        assert_eq!(desk.state.on_timer(), None);
        // And the fact cannot be written straight into the log by a console.
        assert_eq!(
            desk.send(Command::Record(Box::new(Event::HeatOnTimer {
                heat: heat(1)
            }))),
            Err(Refusal::NotACommand)
        );
    }

    #[test]
    fn a_heat_on_the_timer_is_closed_to_pilots_and_open_to_the_director() {
        let mut desk = desk();
        desk.take(1, 0, "ace").unwrap();
        desk.put(1).unwrap();
        assert_eq!(desk.statuses()[0], Closed);
        assert_eq!(
            desk.take(1, 1, "bee"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        assert_eq!(
            desk.leave(1, "ace"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
        // The director adds a pilot up to the start.
        desk.direct(1, 1, "bee").unwrap();
        assert_eq!(desk.spots()[0], "ace,bee,-");
        // Next free on that seat skips the heat on the timer.
        desk.signup(SignupCommand::TakeNext {
            phase: pid(QUALI),
            seat: seat(2),
            pilot: pilot("cat"),
            by: SignupBy::Pilot,
        })
        .unwrap();
        assert_eq!(desk.spots()[0], "ace,bee,-");
        assert_eq!(desk.spots()[1], "-,-,cat");
    }

    #[test]
    fn putting_a_heat_on_the_timer_opens_another_so_pilots_still_have_three() {
        let mut desk = desk();
        assert_eq!(desk.statuses(), [Open, Open, Open]);
        desk.put(1).unwrap();
        assert_eq!(desk.statuses(), [Closed, Open, Open, Open]);
        // The same heat again opens nothing more.
        desk.put(1).unwrap();
        assert_eq!(desk.statuses().len(), 4);
        assert_eq!(
            desk.replayed()
                .signup_sheet(&pid(QUALI), &desk.state.results()),
            Some(desk.sheet())
        );
    }

    #[test]
    fn moving_to_another_heat_reopens_the_one_that_was_never_started() {
        let mut desk = desk();
        desk.put(1).unwrap();
        desk.put(2).unwrap();
        // Heat 1 is open to pilots again; heat 2 is the closed one. No extra heat is made:
        // three are open already.
        assert_eq!(desk.statuses(), [Open, Closed, Open, Open]);
        desk.take(1, 0, "ace").unwrap();
        assert_eq!(
            desk.take(2, 0, "bee"),
            Err(Refusal::HeatClosedToPilots(heat(2)))
        );
    }

    #[test]
    fn a_heat_the_director_closed_stays_closed_when_it_leaves_the_timer() {
        let mut desk = desk();
        desk.signup(SignupCommand::CloseHeat {
            heat: heat(1),
            closed: true,
        })
        .unwrap();
        desk.put(1).unwrap();
        desk.put(2).unwrap();
        assert_eq!(desk.statuses()[0], Closed);
        assert_eq!(
            desk.take(1, 0, "ace"),
            Err(Refusal::HeatClosedToPilots(heat(1)))
        );
    }

    #[test]
    fn starting_a_race_puts_its_heat_on_the_timer_and_it_stays_after_the_save() {
        let mut desk = desk();
        desk.direct(2, 0, "ace").unwrap();
        desk.put(1).unwrap();
        let race = desk.start(2);
        assert_eq!(desk.state.on_timer(), Some(heat(2)));
        assert_eq!(desk.statuses()[..2], [Open, Racing]);
        desk.send(Command::StopRace { race: race.clone() }).unwrap();
        desk.send(Command::SaveRace {
            race,
            needs_marshal: false,
            note: None,
        })
        .unwrap();
        // The heat that was just flown is still the one on the timer, until another is put there.
        assert_eq!(desk.state.on_timer(), Some(heat(2)));
        assert_eq!(desk.statuses()[..2], [Open, Flown]);
        assert_eq!(desk.replayed().on_timer(), Some(heat(2)));
    }

    #[test]
    fn while_a_race_is_on_no_other_heat_can_go_on_the_timer() {
        let mut desk = desk();
        desk.direct(1, 0, "ace").unwrap();
        let race = desk.start(1);
        assert_eq!(desk.put(2), Err(Refusal::RaceStillOn(heat(1))));
        assert_eq!(desk.state.on_timer(), Some(heat(1)));
        // Its own heat is already there: nothing to say, and no refusal.
        assert_eq!(desk.put(1).unwrap(), Vec::<Event>::new());
        // Stopped is still not decided.
        desk.send(Command::StopRace { race: race.clone() }).unwrap();
        assert_eq!(desk.put(2), Err(Refusal::RaceStillOn(heat(1))));
        desk.discard(&race);
        desk.put(2).unwrap();
        assert_eq!(desk.state.on_timer(), Some(heat(2)));
        // The heat whose race was thrown out stays closed to pilots, as before.
        assert_eq!(desk.statuses()[0], Closed);
    }

    #[test]
    fn a_heat_that_is_removed_is_no_longer_on_the_timer() {
        let mut desk = desk();
        desk.put(3).unwrap();
        desk.send(Command::Record(Box::new(Event::PhaseRemoved {
            phase: heat(3),
        })))
        .unwrap();
        assert_eq!(desk.state.on_timer(), None);
        assert_eq!(desk.replayed().on_timer(), None);
    }

    #[test]
    fn a_heat_outside_sign_up_goes_on_the_timer_with_no_sign_up_facts() {
        let mut desk = desk();
        desk.send(Command::Record(Box::new(Event::PhaseDefined {
            def: PhaseDef {
                id: pid("final"),
                label: "Final".into(),
                parent: None,
                kind: PhaseKind::Heat { lineup: vec![] },
                format: Some(format()),
                false_start_limit: None,
                display_hint: None,
            },
        })))
        .unwrap();
        let before = desk.sheet();
        assert_eq!(
            desk.send(Command::PutOnTimer { heat: pid("final") })
                .unwrap(),
            [Event::HeatOnTimer { heat: pid("final") }]
        );
        assert_eq!(desk.sheet(), before);
        // Leaving a sign-up heat for it reopens that heat.
        desk.put(1).unwrap();
        assert_eq!(desk.statuses()[0], Closed);
        desk.send(Command::PutOnTimer { heat: pid("final") })
            .unwrap();
        assert_eq!(desk.statuses()[0], Open);
    }

    #[test]
    fn an_old_log_with_no_on_timer_fact_reads_as_nothing_on_the_timer_until_a_race_starts() {
        let mut desk = desk();
        desk.direct(1, 0, "ace").unwrap();
        assert_eq!(desk.state.on_timer(), None);
        desk.fly(1);
        assert_eq!(desk.state.on_timer(), Some(heat(1)));
    }
}
