//! One event's whole state, folded from its log, and the one door every command goes through.
//!
//! [`EventState`] holds the other folds of this crate side by side: the phase tree, the
//! races, their laps, the rulings and the class rosters. It is built by applying the log in
//! order and can be caught up one event at a time.
//!
//! - [`EventState::check`] takes a console [`Command`] and gives the facts to append, or the
//!   reason it is refused. It appends nothing.
//! - [`EventState::due`] gives the facts that **time alone** has made true: a staged race
//!   reaching its tone, a timed race running out. The Director appends them before it does
//!   anything else, stamped with the moment they fell due, so the log reads the same however
//!   late somebody looked.
//!
//! Like the rest of the crate this is pure: the clock comes in as an argument.

use std::collections::BTreeMap;

use gridfpv_events::{
    AdapterId, ClassId, Command, EndCondition, Event, Format, LapRules, LineupEntry, PhaseId,
    PhaseKind, PilotId, ProtestId, RaceId, RaceSetup, SourceTime, StartOwner,
};
use thiserror::Error;

use crate::heat::RaceEntry;
use crate::laps::{LapFilters, RaceLaps};
use crate::phase::{Inputs, PhaseResult, SeatWho, Tree};
use crate::race::{Race, RaceCommand, RaceError, RaceState, Races};
use crate::rulings::{LockError, Rulings};

/// Why a command was refused. Nothing was appended.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    #[error(transparent)]
    Race(#[from] RaceError),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("there is no phase {0:?}")]
    UnknownPhase(PhaseId),
    #[error("a lap rule cannot be a negative time")]
    NegativeLapRule,
    #[error("{0:?} is not a heat")]
    NotAHeat(PhaseId),
    #[error("{0:?} is locked; unlock it first")]
    Locked(PhaseId),
    #[error("{heat:?} is waiting on {on:?} to finish")]
    HeatWaiting { heat: PhaseId, on: PhaseId },
    #[error("{0:?} is already complete")]
    HeatComplete(PhaseId),
    #[error("{0:?} has nobody to race")]
    NobodyToRace(PhaseId),
    #[error("{0:?} holds other phases; remove those first")]
    HasChildren(PhaseId),
    #[error("{0:?} has races; a heat that was raced cannot be removed")]
    HasRaces(PhaseId),
    #[error("a phase cannot be its own parent, or sit beneath itself")]
    ParentLoop,
    #[error("that crossing is not part of race {0:?}")]
    NotACrossing(RaceId),
    #[error("that seat is not in the lineup of race {0:?}")]
    SeatNotInRace(RaceId),
    #[error("pilot {pilot:?} is not in race {race:?}")]
    PilotNotInRace { race: RaceId, pilot: PilotId },
    #[error("pilot {pilot:?} is not in {phase:?}")]
    PilotNotInPhase { phase: PhaseId, pilot: PilotId },
    #[error("a position starts at 1")]
    BadPosition,
    #[error("there is nothing to undo")]
    NothingToUndo,
    #[error("there is no open protest {0:?}")]
    NoOpenProtest(ProtestId),
    #[error("{0:?} is not locked")]
    NotLocked(PhaseId),
    #[error("only a timer or the Director can log that")]
    NotACommand,
}

/// One pilot's place in a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassPlace {
    pub pilot: PilotId,
    /// 1 for the top. Every pilot on the roster has a different position.
    pub position: u32,
    /// The phase that placed them. `None` for a pilot no counted phase has placed: they
    /// come last, in roster order.
    pub placed_by: Option<PhaseId>,
}

/// One event's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventState {
    pub tree: Tree,
    pub races: Races,
    pub laps: RaceLaps,
    pub rulings: Rulings,
    /// Each class in the event and its roster, in roster order.
    pub rosters: BTreeMap<ClassId, Vec<PilotId>>,
    /// Each timer's clock minus the Director's, as last measured.
    pub clock_offsets: BTreeMap<AdapterId, i64>,
    /// Each race's start on each of its timers' own clocks, where the offset was known,
    /// with the run of that clock it was placed on (a timer that restarts begins its clock
    /// again).
    pub starts_on_timer: BTreeMap<(RaceId, AdapterId), (SourceTime, u32)>,
    /// For a running first-to-N race whose pilots have all finished: when the last of them
    /// did, on the Director's clock. Cleared if a ruling takes a finish away.
    pub all_finished_at: BTreeMap<RaceId, i64>,
    /// The phases that count toward each class's standings, the deciding one first.
    pub class_phases: BTreeMap<ClassId, Vec<PhaseId>>,
    /// The order the director set for the run queue. Heats not in it follow in the default
    /// order.
    pub queue_order: Vec<PhaseId>,
    /// The event's lap rules, as last set.
    pub lap_rules: LapRules,
    /// Where in the log each race begins: the offset of its first fact. A race's signal is
    /// logged from there on, so this is where to start reading for it.
    pub race_log_start: BTreeMap<RaceId, u64>,
    /// How many log entries have been applied: the offset the next one will have.
    pub applied: u64,
}

impl EventState {
    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the log entry stored at `offset`. Entries must be applied in order; one at or
    /// below what was already applied is skipped, so catching up twice is harmless.
    pub fn apply(&mut self, offset: u64, recorded_at: Option<i64>, event: &Event) {
        if offset < self.applied {
            return;
        }
        self.tree.apply(event);
        self.races.apply(recorded_at, event);
        self.laps.apply(offset, event);
        self.rulings.apply(event);
        match event {
            Event::RosterSet { class, pilots } => {
                self.rosters.insert(class.clone(), pilots.clone());
            }
            Event::RosterRemoved { class } => {
                self.rosters.remove(class);
            }
            Event::QueueSet { heats } => self.queue_order = heats.clone(),
            Event::LapRulesSet { rules } => self.lap_rules = *rules,
            Event::StandingsSet { class, phases } => {
                self.class_phases.insert(class.clone(), phases.clone());
            }
            Event::ClockOffset {
                adapter,
                offset_micros,
                ..
            } => {
                self.clock_offsets.insert(adapter.clone(), *offset_micros);
                // A timer measured after the start still places that start, if nothing
                // had placed it yet.
                self.place_starts();
            }
            Event::SessionStarted { adapter, .. } => {
                // The timer started up: its clock began again, so the old offset is for a
                // clock that no longer exists. It must not place a start until measured.
                self.clock_offsets.remove(adapter);
            }
            Event::RaceStarting { setup, .. } => {
                self.race_log_start
                    .entry(setup.race.clone())
                    .or_insert(offset);
            }
            Event::RaceStarted { setup } => {
                self.race_log_start
                    .entry(setup.race.clone())
                    .or_insert(offset);
                self.place_starts();
            }
            _ => {}
        }
        self.note_all_finished(recorded_at);
        self.applied = offset + 1;
    }

    /// Keep [`EventState::all_finished_at`] true for the running race, after an entry
    /// recorded at `recorded_at` was applied.
    fn note_all_finished(&mut self, recorded_at: Option<i64>) {
        let Some(race) = self.races.running() else {
            self.all_finished_at.clear();
            return;
        };
        let id = race.setup.race.clone();
        let target = match self.tree.format_for(&race.setup.heat).map(|f| f.race.end) {
            Some(EndCondition::FirstToLaps(n)) => n as usize,
            _ => return,
        };
        // Only named pilots count. An open seat has nobody to wait for.
        let mut pilots = race
            .setup
            .lineup
            .iter()
            .filter(|entry| entry.pilot.is_some());
        let all = pilots.clone().next().is_some()
            && pilots.all(|entry| {
                self.laps
                    .seat(
                        &id,
                        &entry.adapter,
                        &entry.seat,
                        self.filters_for(race, &entry.adapter),
                    )
                    .lap_count()
                    >= target
            });
        match (all, recorded_at) {
            (true, Some(at)) => {
                self.all_finished_at.entry(id).or_insert(at);
            }
            (true, None) => {}
            (false, _) => {
                self.all_finished_at.remove(&id);
            }
        }
    }

    /// A class's standings: everyone on its roster, 1 to n.
    ///
    /// The class's counted phases are read in order. The first one's ranking places the
    /// pilots it holds. Each later phase places only pilots still unplaced, below everyone
    /// already placed. Pilots no counted phase holds come last, in roster order. A pilot
    /// who is not on the roster is left out.
    pub fn class_standings(
        &self,
        class: &ClassId,
        results: &BTreeMap<PhaseId, PhaseResult>,
    ) -> Vec<ClassPlace> {
        let roster = self.rosters.get(class).cloned().unwrap_or_default();
        let mut out: Vec<ClassPlace> = Vec::new();
        let phases = self.class_phases.get(class).map_or(&[][..], Vec::as_slice);
        for phase in phases {
            let Some(result) = results.get(phase) else {
                continue;
            };
            for placed in &result.ranking {
                let fresh = roster.contains(&placed.pilot)
                    && !out.iter().any(|place| place.pilot == placed.pilot);
                if fresh {
                    out.push(ClassPlace {
                        pilot: placed.pilot.clone(),
                        position: out.len() as u32 + 1,
                        placed_by: Some(phase.clone()),
                    });
                }
            }
        }
        for pilot in roster {
            if !out.iter().any(|place| place.pilot == pilot) {
                out.push(ClassPlace {
                    pilot,
                    position: out.len() as u32 + 1,
                    placed_by: None,
                });
            }
        }
        out
    }

    /// The race a crossing belongs to, when it is logged at `now` (Director's clock).
    ///
    /// Normally that is the race running now. But a timer reports a crossing a little after
    /// it happened (it waits for the signal to fall), so a pilot's last crossing can arrive
    /// just after the race was stopped. Such a crossing still belongs to that race, as long
    /// as the race has not been saved or discarded yet:
    ///
    /// - where the timer's clock is known, when the crossing happened at or before the stop;
    /// - where it is not, when the crossing arrives within [`Self::LATE_PASS_MICROS`] of it.
    ///
    /// Anything else belongs to no race.
    pub fn race_for_pass(&self, pass: &gridfpv_events::Pass, now: i64) -> Option<RaceId> {
        if let Some(running) = self.races.running() {
            return Some(running.setup.race.clone());
        }
        let stopped = self
            .races
            .undecided()
            .filter(|race| race.state == RaceState::Stopped)?;
        let on_this_timer = stopped
            .setup
            .lineup
            .iter()
            .any(|entry| entry.adapter == pass.adapter);
        let stopped_at = stopped.stopped_at?;
        let in_time = match self.clock_offsets.get(&pass.adapter) {
            Some(offset) => pass.at.micros <= stopped_at + offset,
            None => now - stopped_at <= Self::LATE_PASS_MICROS,
        };
        (on_this_timer && in_time).then(|| stopped.setup.race.clone())
    }

    /// How long after a stop a crossing is still taken for the stopped race, on a timer
    /// whose clock is not known.
    pub const LATE_PASS_MICROS: i64 = 1_000_000;

    /// The heats that can be raced now: every seat resolved, somebody to race, not complete,
    /// not locked. In tree order.
    fn ready_heats(&self, results: &BTreeMap<PhaseId, PhaseResult>) -> Vec<PhaseId> {
        self.tree
            .iter()
            .filter(|def| matches!(def.kind, PhaseKind::Heat { .. }))
            .filter(|def| {
                results.get(&def.id).is_some_and(|result| {
                    !result.waiting
                        && !result.complete
                        && result
                            .lineup
                            .iter()
                            .any(|seat| matches!(seat.who, SeatWho::Pilot(_) | SeatWho::Open))
                }) && !self.rulings.is_locked(&self.tree, &def.id)
            })
            .map(|def| def.id.clone())
            .collect()
    }

    /// The run queue: the heats that can be raced now, in the order suggested. Heats the
    /// director put in order come first, in that order; the rest follow in the order the
    /// phases were set up, which runs one phase to completion before the next.
    pub fn queue(&self, results: &BTreeMap<PhaseId, PhaseResult>) -> Vec<PhaseId> {
        let mut ready = self.ready_heats(results);
        let place = |heat: &PhaseId| {
            self.queue_order
                .iter()
                .position(|queued| queued == heat)
                .unwrap_or(usize::MAX)
        };
        // Stable: heats the director did not place keep their tree order.
        ready.sort_by_key(place);
        ready
    }

    /// A helper order for the run queue that spaces pilots out: the heat whose pilots have
    /// waited longest goes first, so nobody flies back to back when it can be avoided. The
    /// director asks for this; it is never applied by itself.
    pub fn queue_spaced(&self, results: &BTreeMap<PhaseId, PhaseResult>) -> Vec<PhaseId> {
        // Each pilot's latest race, by its place in the order races were flown.
        let mut last_flew: BTreeMap<&PilotId, usize> = BTreeMap::new();
        for (index, race) in self.races.iter().enumerate() {
            for entry in &race.setup.lineup {
                if let Some(pilot) = &entry.pilot {
                    last_flew.insert(pilot, index);
                }
            }
        }
        let mut waiting: Vec<PhaseId> = self.ready_heats(results);
        let mut out = Vec::new();
        let mut next = self.races.iter().count();
        // Pick one heat at a time, then count its pilots as having just flown.
        while !waiting.is_empty() {
            let rested = |heat: &PhaseId| -> Option<usize> {
                results[heat]
                    .field
                    .iter()
                    .filter_map(|pilot| last_flew.get(pilot).copied())
                    .max()
            };
            let pick = (0..waiting.len())
                .min_by_key(|index| rested(&waiting[*index]))
                .unwrap_or(0);
            let heat = waiting.remove(pick);
            for pilot in &results[&heat].field {
                last_flew.insert(pilot, next);
            }
            next += 1;
            out.push(heat);
        }
        out
    }

    /// The stretch of the log that holds a race's signal: from its first fact up to the
    /// first fact of the next race to begin (or the end of the log). `None` for a race that
    /// does not exist.
    pub fn race_log_range(&self, race: &RaceId) -> Option<(u64, u64)> {
        let from = *self.race_log_start.get(race)?;
        let to = self
            .race_log_start
            .values()
            .copied()
            .filter(|start| *start > from)
            .min()
            .unwrap_or(self.applied);
        Some((from, to))
    }

    /// Put each started race's start on the clocks of its timers, where that is known and
    /// not yet placed. A placed start is never moved: laps must not shift under a later,
    /// slightly different measurement.
    fn place_starts(&mut self) {
        for race in self.races.iter() {
            let Some(started_at) = race.started_at else {
                continue;
            };
            for entry in &race.setup.lineup {
                let Some(offset) = self.clock_offsets.get(&entry.adapter) else {
                    continue;
                };
                self.starts_on_timer
                    .entry((race.setup.race.clone(), entry.adapter.clone()))
                    .or_insert((
                        SourceTime::from_micros(started_at + offset),
                        self.laps.clock_run(&entry.adapter),
                    ));
            }
        }
    }

    /// Work out every phase: fields, lineups, rankings and what is complete.
    pub fn results(&self) -> BTreeMap<PhaseId, PhaseResult> {
        self.tree.evaluate(self)
    }

    /// The lap filters one timer's seats in a race are read under: the rules of the format
    /// in force, and the race's start on that timer's clock where it is known.
    pub fn filters_for(&self, race: &Race, adapter: &AdapterId) -> LapFilters {
        let rules = self
            .tree
            .format_for(&race.setup.heat)
            .map(|format| &format.race);
        let start = self
            .starts_on_timer
            .get(&(race.setup.race.clone(), adapter.clone()));
        LapFilters {
            min_lap_micros: rules
                .and_then(|rules| rules.min_lap_micros)
                .or(self.lap_rules.min_lap_micros),
            ignore_before_micros: self.lap_rules.ignore_before_micros,
            start_at: start.map(|(at, _)| *at),
            first_lap: rules.map(|rules| rules.first_lap).unwrap_or_default(),
            start_clock_run: start.map_or(0, |(_, run)| *run),
        }
    }

    /// The format in force for a heat, if any phase at or above it sets one.
    pub fn format_for(&self, heat: &PhaseId) -> Option<&Format> {
        self.tree.format_for(heat)
    }

    /// The facts time alone has made true by `now` (server clock, microseconds), each with
    /// the moment it fell due. Append them, in order, stamped with that moment.
    pub fn due(&self, now: i64) -> Vec<(i64, Event)> {
        let mut out = Vec::new();
        for race in self.races.iter() {
            match race.state {
                RaceState::Staging => {
                    if let Some(tone_at) = race.tone_at.filter(|at| *at <= now) {
                        out.push((
                            tone_at,
                            Event::RaceStarted {
                                setup: race.setup.clone(),
                            },
                        ));
                        // The race may also have run out already, if nobody looked for a
                        // long time.
                        if let Some(end) = self.runs_out(race, tone_at).filter(|at| *at <= now) {
                            out.push((
                                end,
                                Event::RaceStopped {
                                    race: race.setup.race.clone(),
                                },
                            ));
                        }
                    }
                }
                RaceState::Running => {
                    let end = race
                        .started_at
                        .and_then(|started| self.runs_out(race, started));
                    if let Some(end) = end.filter(|at| *at <= now) {
                        out.push((
                            end,
                            Event::RaceStopped {
                                race: race.setup.race.clone(),
                            },
                        ));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The next moment [`EventState::due`] will have something new, if any.
    pub fn next_due(&self) -> Option<i64> {
        self.races
            .iter()
            .filter_map(|race| match race.state {
                RaceState::Staging => race.tone_at,
                RaceState::Running => race
                    .started_at
                    .and_then(|started| self.runs_out(race, started)),
                _ => None,
            })
            .min()
    }

    /// When a race that started at `started` stops by itself, if it does.
    ///
    /// - A timed race: after its time and its grace window.
    /// - A first-to-N race whose format asks for it: a set wait after every pilot finished.
    /// - A first-to-N or open race with a time limit: at the limit.
    ///
    /// Whichever comes first. With none of these the director stops it.
    fn runs_out(&self, race: &Race, started: i64) -> Option<i64> {
        let rules = &self.tree.format_for(&race.setup.heat)?.race;
        let limit = rules.max_micros.map(|max| started + max);
        match rules.end {
            EndCondition::Timed {
                micros,
                grace_micros,
            } => Some(started + micros + grace_micros),
            EndCondition::FirstToLaps(_) => {
                let all_done = rules.stop_after_all_finish_micros.and_then(|wait| {
                    let finished = self.all_finished_at.get(&race.setup.race)?;
                    Some(finished + wait)
                });
                match (all_done, limit) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                }
            }
            EndCondition::Open => limit,
        }
    }

    fn heat(&self, id: &PhaseId) -> Result<(), Refusal> {
        match self.tree.get(id).map(|def| &def.kind) {
            None => Err(Refusal::UnknownPhase(id.clone())),
            Some(PhaseKind::Group { .. }) => Err(Refusal::NotAHeat(id.clone())),
            Some(PhaseKind::Heat { .. }) => Ok(()),
        }
    }

    fn unlocked(&self, phase: &PhaseId) -> Result<(), Refusal> {
        if self.tree.get(phase).is_none() {
            return Err(Refusal::UnknownPhase(phase.clone()));
        }
        if self.rulings.is_locked(&self.tree, phase) {
            return Err(Refusal::Locked(phase.clone()));
        }
        Ok(())
    }

    /// A race that exists and is not under a lock.
    fn open_race(&self, race: &RaceId) -> Result<&Race, Refusal> {
        let found = self
            .races
            .get(race)
            .ok_or_else(|| RaceError::UnknownRace(race.clone()))?;
        if self.rulings.race_is_locked(&self.tree, &self.races, race) {
            return Err(Refusal::Locked(found.setup.heat.clone()));
        }
        Ok(found)
    }

    /// Check a command at `now` (server clock, microseconds). Gives the facts to append, in
    /// order, or the reason it is refused.
    pub fn check(&self, command: Command, now: i64) -> Result<Vec<Event>, Refusal> {
        match command {
            Command::StartRace { heat } => self.start(heat, now),
            Command::StopRace { race } => Ok(self.races.check(RaceCommand::Stop { race })?),
            Command::SaveRace {
                race,
                needs_marshal,
                note,
            } => Ok(self.races.check(RaceCommand::Save {
                race,
                needs_marshal,
                note,
            })?),
            Command::DiscardRace { race, reason, note } => {
                self.open_race(&race)?;
                Ok(self
                    .races
                    .check(RaceCommand::Discard { race, reason, note })?)
            }
            Command::FileProtest { race, by, note } => {
                self.open_race(&race)?;
                Ok(vec![Event::ProtestFiled {
                    // The log offset this fact will land on is unique for all time.
                    protest: ProtestId(format!("protest-{}", self.applied)),
                    race,
                    by,
                    note,
                }])
            }
            Command::Record(fact) => {
                self.check_fact(&fact)?;
                Ok(vec![*fact])
            }
        }
    }

    fn start(&self, heat: PhaseId, now: i64) -> Result<Vec<Event>, Refusal> {
        self.heat(&heat)?;
        self.unlocked(&heat)?;
        let results = self.results();
        let result = results.get(&heat).cloned().unwrap_or_default();
        if let Some(on) = result.lineup.iter().find_map(|seat| match &seat.who {
            SeatWho::Waiting { phase, .. } => Some(phase.clone()),
            _ => None,
        }) {
            return Err(Refusal::HeatWaiting { heat, on });
        }
        if result.complete {
            return Err(Refusal::HeatComplete(heat));
        }
        let lineup: Vec<LineupEntry> = result
            .lineup
            .iter()
            .filter_map(|seat| {
                let pilot = match &seat.who {
                    SeatWho::Pilot(pilot) => Some(pilot.clone()),
                    SeatWho::Open => None,
                    SeatWho::Nobody | SeatWho::Waiting { .. } => return None,
                };
                Some(LineupEntry {
                    adapter: seat.adapter.clone(),
                    seat: seat.seat.clone(),
                    pilot,
                })
            })
            .collect();
        if lineup.is_empty() {
            return Err(Refusal::NobodyToRace(heat));
        }
        // "‹heat›#N": the Nth race of this heat, discarded ones included.
        let number = self.races.of_heat(&heat).count() + 1;
        let staging = self
            .tree
            .format_for(&heat)
            .map_or(0, |format| format.race.staging_micros);
        let setup = RaceSetup {
            race: RaceId(format!("{}#{number}", heat.0)),
            heat,
            lineup,
            owner: StartOwner::Grid,
        };
        Ok(self.races.check(if staging > 0 {
            RaceCommand::Stage {
                setup,
                tone_at: now + staging,
            }
        } else {
            RaceCommand::Start { setup }
        })?)
    }

    /// Whether a setup fact or ruling may be recorded as it stands.
    fn check_fact(&self, fact: &Event) -> Result<(), Refusal> {
        match fact {
            Event::PhaseDefined { def } => {
                if let Some(parent) = &def.parent {
                    if self.tree.get(parent).is_none() {
                        return Err(Refusal::UnknownPhase(parent.clone()));
                    }
                    if parent == &def.id || self.tree.is_under(parent, &def.id) {
                        return Err(Refusal::ParentLoop);
                    }
                    if self.rulings.is_locked(&self.tree, parent) {
                        return Err(Refusal::Locked(parent.clone()));
                    }
                }
                if self.tree.get(&def.id).is_some() {
                    self.unlocked(&def.id)?;
                }
                Ok(())
            }
            Event::PhaseRemoved { phase } => {
                self.unlocked(phase)?;
                if self.tree.children(phase).next().is_some() {
                    return Err(Refusal::HasChildren(phase.clone()));
                }
                if self.races.of_heat(phase).next().is_some() {
                    return Err(Refusal::HasRaces(phase.clone()));
                }
                Ok(())
            }
            Event::RosterSet { .. } | Event::RosterRemoved { .. } => Ok(()),
            Event::LapRulesSet { rules } => {
                let negative = |value: Option<i64>| value.is_some_and(|micros| micros < 0);
                if negative(rules.min_lap_micros) || negative(rules.ignore_before_micros) {
                    Err(Refusal::NegativeLapRule)
                } else {
                    Ok(())
                }
            }
            Event::QueueSet { heats } => heats.iter().try_for_each(|heat| self.heat(heat)),
            Event::StandingsSet { phases, .. } => phases.iter().try_for_each(|phase| {
                if self.tree.get(phase).is_none() {
                    return Err(Refusal::UnknownPhase(phase.clone()));
                }
                Ok(())
            }),

            Event::LapRemoved { race, crossing, .. }
            | Event::LapRestored { race, crossing }
            | Event::LapCounted { race, crossing }
            | Event::LapAdjusted { race, crossing, .. }
            | Event::RestartMarked { race, crossing }
            | Event::RestartUnmarked { race, crossing } => {
                self.open_race(race)?;
                if !self.laps.is_crossing_of(race, crossing) {
                    return Err(Refusal::NotACrossing(race.clone()));
                }
                Ok(())
            }
            Event::LapInserted {
                race,
                adapter,
                seat,
                ..
            } => {
                let found = self.open_race(race)?;
                let seated = found
                    .setup
                    .lineup
                    .iter()
                    .any(|entry| &entry.adapter == adapter && &entry.seat == seat);
                if !seated {
                    return Err(Refusal::SeatNotInRace(race.clone()));
                }
                Ok(())
            }

            Event::MarshalFlagSet {
                race,
                needs_marshal,
            } => {
                let found = self.open_race(race)?;
                if found.state != RaceState::Saved {
                    return Err(Refusal::Race(RaceError::WrongState {
                        race: race.clone(),
                        state: found.state,
                        wanted: "flag",
                    }));
                }
                if found.needs_marshal == *needs_marshal {
                    return Err(Refusal::NothingToUndo);
                }
                Ok(())
            }
            Event::HeatDone { heat } | Event::HeatReopened { heat } => {
                self.heat(heat)?;
                self.unlocked(heat)
            }
            Event::FalseStart { race, pilot } => {
                let found = self.open_race(race)?;
                let flying = found
                    .setup
                    .lineup
                    .iter()
                    .any(|entry| entry.pilot.as_ref() == Some(pilot));
                if !flying {
                    return Err(Refusal::PilotNotInRace {
                        race: race.clone(),
                        pilot: pilot.clone(),
                    });
                }
                Ok(())
            }
            Event::FalseStartCleared { race, pilot } => {
                self.open_race(race)?;
                if !self.rulings.has_false_start(race, pilot) {
                    return Err(Refusal::NothingToUndo);
                }
                Ok(())
            }
            Event::Disqualified { phase, .. } => self.unlocked(phase),
            Event::Reinstated { phase, pilot } => {
                self.unlocked(phase)?;
                if !self.rulings.is_disqualified_from(phase, pilot) {
                    return Err(Refusal::NothingToUndo);
                }
                Ok(())
            }
            Event::PositionSet {
                heat,
                pilot,
                position,
                ..
            } => {
                self.heat(heat)?;
                self.unlocked(heat)?;
                if *position == 0 {
                    return Err(Refusal::BadPosition);
                }
                let results = self.results();
                let in_heat = results
                    .get(heat)
                    .is_some_and(|result| result.field.contains(pilot));
                if !in_heat {
                    return Err(Refusal::PilotNotInPhase {
                        phase: heat.clone(),
                        pilot: pilot.clone(),
                    });
                }
                Ok(())
            }
            Event::PositionCleared { heat, pilot } => {
                self.unlocked(heat)?;
                let set = self.rulings.positions_set(heat);
                if !set.iter().any(|(who, _)| who == pilot) {
                    return Err(Refusal::NothingToUndo);
                }
                Ok(())
            }
            Event::PhaseLocked { phase } => {
                Ok(self.rulings.check_lock(&self.tree, &self.races, phase)?)
            }
            Event::PhaseUnlocked { phase } => {
                if !self.rulings.is_locked_here(phase) {
                    return Err(Refusal::NotLocked(phase.clone()));
                }
                Ok(())
            }
            Event::ProtestResolved { protest, .. } => {
                let open = self
                    .rulings
                    .protest(protest)
                    .is_some_and(|found| found.outcome.is_none());
                if !open {
                    return Err(Refusal::NoOpenProtest(protest.clone()));
                }
                Ok(())
            }

            // A protest gets its id from the Director: use `Command::FileProtest`.
            Event::ProtestFiled { .. }
            // Observations and race facts are logged by a timer or by the Director itself.
            | Event::AdapterConnected { .. }
            | Event::AdapterDisconnected { .. }
            | Event::SessionStarted { .. }
            | Event::SessionEnded { .. }
            | Event::SeatSeen { .. }
            | Event::Pass(_)
            | Event::ClockOffset { .. }
            | Event::SignalChunk(_)
            | Event::SignalThresholds(_)
            | Event::SignalHistory(_)
            | Event::RaceStarting { .. }
            | Event::RaceStarted { .. }
            | Event::RaceStopped { .. }
            | Event::RaceSaved { .. }
            | Event::RaceDiscarded { .. } => Err(Refusal::NotACommand),
        }
    }
}

impl Inputs for EventState {
    fn roster(&self, class: &ClassId) -> Vec<PilotId> {
        self.rosters.get(class).cloned().unwrap_or_default()
    }

    fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>> {
        self.races
            .of_heat(heat)
            .filter(|race| race.counts())
            .map(|race| {
                race.setup
                    .lineup
                    .iter()
                    .filter_map(|entry| {
                        let pilot = entry.pilot.clone()?;
                        let laps = self.laps.seat(
                            &race.setup.race,
                            &entry.adapter,
                            &entry.seat,
                            self.filters_for(race, &entry.adapter),
                        );
                        Some(RaceEntry { pilot, laps })
                    })
                    .collect()
            })
            .collect()
    }

    fn director_done(&self, heat: &PhaseId) -> bool {
        self.rulings.director_done(heat)
    }

    fn last_seat(&self, pilot: &PilotId) -> Option<(AdapterId, gridfpv_events::SeatRef)> {
        // The latest race the pilot was in, whatever became of it: the seat was theirs.
        self.races
            .iter()
            .filter_map(|race| {
                race.setup
                    .lineup
                    .iter()
                    .find(|entry| entry.pilot.as_ref() == Some(pilot))
                    .map(|entry| (entry.adapter.clone(), entry.seat.clone()))
            })
            .last()
    }

    fn disqualified(&self, phase: &PhaseId) -> Vec<PilotId> {
        self.rulings.disqualified_from(phase)
    }

    fn places_kept_empty(&self, phase: &PhaseId) -> Vec<PilotId> {
        self.rulings.places_kept_empty(phase)
    }

    fn positions_set(&self, heat: &PhaseId) -> Vec<(PilotId, u32)> {
        self.rulings.positions_set(heat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::{Bracket, Elimination, bracket};
    use gridfpv_events::{
        AdapterId, CompletionRule, FieldSlice, FirstLap, GateIndex, HeatSeat, LogRef, Measure,
        Pass, PhaseDef, RaceRules, RankingRule, SeatRef, SeatSource, SourceTime,
    };

    const S: i64 = 1_000_000;

    fn pid(id: &str) -> PhaseId {
        PhaseId(id.into())
    }
    fn pilot(name: &str) -> PilotId {
        PilotId(name.into())
    }
    fn rid(id: &str) -> RaceId {
        RaceId(id.into())
    }
    fn oft() -> AdapterId {
        AdapterId("oft".into())
    }

    fn format(end: EndCondition, staging_micros: i64) -> Format {
        Format {
            name: "test".into(),
            race: RaceRules {
                end,
                staging_micros,
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

    /// A Director in miniature: a log, the state folded from it, and a clock.
    #[derive(Default)]
    struct Director {
        log: Vec<(Option<i64>, Event)>,
        state: EventState,
        now: i64,
    }

    impl Director {
        fn append(&mut self, at: i64, event: Event) {
            self.state.apply(self.log.len() as u64, Some(at), &event);
            self.log.push((Some(at), event));
        }
        /// What the real Director does before anything else: log what time made true.
        fn catch_up(&mut self) {
            for (at, fact) in self.state.due(self.now) {
                self.append(at, fact);
            }
        }
        fn send(&mut self, command: Command) -> Result<(), Refusal> {
            self.catch_up();
            for fact in self.state.check(command, self.now)? {
                self.append(self.now, fact);
            }
            Ok(())
        }
        fn record(&mut self, fact: Event) -> Result<(), Refusal> {
            self.send(Command::Record(Box::new(fact)))
        }
        /// A crossing on a seat, stamped with the running race as the Director stamps it.
        fn cross(&mut self, seat: &str, at: i64) -> LogRef {
            self.catch_up();
            let race = self.state.races.running().map(|r| r.setup.race.clone());
            let offset = self.log.len() as u64;
            self.append(
                self.now,
                Event::Pass(Pass {
                    adapter: oft(),
                    seat: SeatRef(seat.into()),
                    at: SourceTime::from_micros(at),
                    sequence: None,
                    gate: GateIndex::LAP,
                    signal: None,
                    race,
                }),
            );
            LogRef(offset)
        }
        /// The state folded again from the whole log, to prove catching up loses nothing.
        fn replayed(&self) -> EventState {
            let mut state = EventState::new();
            for (offset, (at, event)) in self.log.iter().enumerate() {
                state.apply(offset as u64, *at, event);
            }
            state
        }
    }

    /// An eight-pilot double elimination bracket, first to one lap, no countdown.
    fn bracket_event() -> Director {
        let pilots: Vec<PilotId> = (1..=8).map(|i| pilot(&format!("p{i}"))).collect();
        let mut director = Director::default();
        for def in bracket(Bracket {
            id: pid("bracket"),
            label: "Bracket".into(),
            parent: None,
            field: vec![FieldSlice::Imported(pilots.clone())],
            entrants: pilots.into_iter().map(SeatSource::Pilot).collect(),
            seats: (0..4)
                .map(|i| (oft(), SeatRef(format!("seat-{i}"))))
                .collect(),
            advance: 2,
            elimination: Elimination::Double,
            format: format(EndCondition::FirstToLaps(1), 0),
            final_format: None,
        })
        .unwrap()
        {
            director.record(Event::PhaseDefined { def }).unwrap();
        }
        director
    }

    /// Fly one race of a heat: the seats finish in seat order.
    fn fly(director: &mut Director, heat: &str) -> RaceId {
        director
            .send(Command::StartRace { heat: pid(heat) })
            .unwrap();
        let race = director.state.races.running().unwrap().setup.clone();
        for (i, entry) in race.lineup.iter().enumerate() {
            director.cross(&entry.seat.0, i as i64 * 1000);
        }
        for (i, entry) in race.lineup.iter().enumerate() {
            director.cross(&entry.seat.0, 10 * S + i as i64 * 1000);
        }
        director
            .send(Command::StopRace {
                race: race.race.clone(),
            })
            .unwrap();
        director
            .send(Command::SaveRace {
                race: race.race.clone(),
                needs_marshal: false,
                note: None,
            })
            .unwrap();
        race.race
    }

    #[test]
    fn a_whole_bracket_runs_through_commands_and_replays_to_the_same_state() {
        let mut director = bracket_event();
        // The final cannot start before the heats that feed it.
        assert!(matches!(
            director.send(Command::StartRace {
                heat: pid("bracket/final")
            }),
            Err(Refusal::HeatWaiting { .. })
        ));
        let first = fly(&mut director, "bracket/winners-1/heat-1");
        assert_eq!(first, rid("bracket/winners-1/heat-1#1"));
        // A finished heat is not raced again by accident.
        assert_eq!(
            director.send(Command::StartRace {
                heat: pid("bracket/winners-1/heat-1")
            }),
            Err(Refusal::HeatComplete(pid("bracket/winners-1/heat-1")))
        );
        for heat in [
            "bracket/winners-1/heat-2",
            "bracket/losers-1",
            "bracket/winners-final",
            "bracket/losers-final",
            "bracket/final",
        ] {
            fly(&mut director, heat);
        }
        let results = director.state.results();
        let bracket = &results[&pid("bracket")];
        assert!(bracket.complete);
        assert_eq!(bracket.ranking.len(), 8);
        // Seat order wins every heat, and seeds are dealt in seat order, so p1 wins.
        assert_eq!(bracket.ranking[0].pilot, pilot("p1"));
        assert_eq!(director.replayed(), director.state);
    }

    #[test]
    fn a_discarded_race_is_followed_by_the_next_race_of_the_same_heat() {
        let mut director = bracket_event();
        let heat = "bracket/winners-1/heat-1";
        director
            .send(Command::StartRace { heat: pid(heat) })
            .unwrap();
        // Nothing else can start while a race is undecided.
        assert!(matches!(
            director.send(Command::StartRace {
                heat: pid("bracket/winners-1/heat-2")
            }),
            Err(Refusal::Race(RaceError::AnotherRaceUndecided { .. }))
        ));
        director
            .send(Command::DiscardRace {
                race: rid(&format!("{heat}#1")),
                reason: Some(gridfpv_events::DiscardReason::FalseStart),
                note: None,
            })
            .unwrap();
        director
            .record(Event::FalseStart {
                race: rid(&format!("{heat}#1")),
                pilot: pilot("p1"),
            })
            .unwrap();
        assert_eq!(fly(&mut director, heat), rid(&format!("{heat}#2")));
    }

    #[test]
    fn a_countdown_becomes_a_start_at_the_tone_however_late_anyone_looks() {
        let mut director = Director::default();
        director
            .record(Event::PhaseDefined {
                def: PhaseDef {
                    id: pid("heat"),
                    label: "Heat".into(),
                    parent: None,
                    kind: PhaseKind::Heat {
                        lineup: vec![HeatSeat {
                            adapter: oft(),
                            seat: SeatRef("seat-0".into()),
                            source: SeatSource::Pilot(pilot("ana")),
                        }],
                    },
                    format: Some(format(
                        EndCondition::Timed {
                            micros: 60 * S,
                            grace_micros: 5 * S,
                        },
                        3 * S,
                    )),
                    false_start_limit: None,
                    display_hint: None,
                },
            })
            .unwrap();
        director.now = 100 * S;
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        let race = rid("heat#1");
        assert_eq!(
            director.state.races.get(&race).unwrap().state,
            RaceState::Staging
        );
        assert_eq!(director.state.next_due(), Some(103 * S));
        // A crossing during the countdown belongs to no race.
        director.now = 101 * S;
        director.cross("seat-0", 0);

        // Nobody looks until a second after the tone. The start is still logged at the tone.
        director.now = 104 * S;
        director.catch_up();
        let running = director.state.races.get(&race).unwrap();
        assert_eq!(running.state, RaceState::Running);
        assert_eq!(running.started_at, Some(103 * S));
        assert_eq!(director.state.next_due(), Some(168 * S));
        director.cross("seat-0", 5 * S);
        director.cross("seat-0", 15 * S);

        // The race runs out by itself: 60 seconds and 5 of grace after the tone.
        director.now = 500 * S;
        director.catch_up();
        let stopped = director.state.races.get(&race).unwrap();
        assert_eq!(stopped.state, RaceState::Stopped);
        assert_eq!(stopped.stopped_at, Some(168 * S));
        assert_eq!(director.state.next_due(), None);
        // It still waits for the director to save it.
        assert!(!director.state.results()[&pid("heat")].complete);
        director
            .send(Command::SaveRace {
                race: race.clone(),
                needs_marshal: false,
                note: None,
            })
            .unwrap();
        let results = director.state.results();
        assert!(results[&pid("heat")].complete);
        // The countdown crossing was not part of the race: one lap, not two.
        assert_eq!(results[&pid("heat")].ranking[0].result.laps, 1);
        assert_eq!(director.replayed(), director.state);
    }

    #[test]
    fn a_race_that_ran_out_before_anyone_looked_is_started_and_stopped_in_order() {
        let mut director = Director::default();
        director
            .record(Event::PhaseDefined {
                def: PhaseDef {
                    id: pid("heat"),
                    label: "Heat".into(),
                    parent: None,
                    kind: PhaseKind::Heat {
                        lineup: vec![HeatSeat {
                            adapter: oft(),
                            seat: SeatRef("seat-0".into()),
                            source: SeatSource::Open,
                        }],
                    },
                    format: Some(format(
                        EndCondition::Timed {
                            micros: 10 * S,
                            grace_micros: 0,
                        },
                        S,
                    )),
                    false_start_limit: None,
                    display_hint: None,
                },
            })
            .unwrap();
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        director.now = 1000 * S;
        director.catch_up();
        let race = director.state.races.get(&rid("heat#1")).unwrap();
        assert_eq!(race.state, RaceState::Stopped);
        assert_eq!(race.started_at, Some(S));
        assert_eq!(race.stopped_at, Some(11 * S));
    }

    #[test]
    fn rulings_are_recorded_as_facts_and_checked_first() {
        let mut director = bracket_event();
        let heat = "bracket/winners-1/heat-1";
        let race = fly(&mut director, heat);

        // A lap ruling must target a crossing of that race.
        assert_eq!(
            director.record(Event::LapRemoved {
                race: race.clone(),
                crossing: LogRef(0),
                reason: None,
                note: None,
            }),
            Err(Refusal::NotACrossing(race.clone()))
        );
        // A position can only be set for a pilot in the heat.
        assert!(matches!(
            director.record(Event::PositionSet {
                heat: pid(heat),
                pilot: pilot("p2"),
                position: 1,
                note: None,
            }),
            Err(Refusal::PilotNotInPhase { .. })
        ));
        director
            .record(Event::PositionSet {
                heat: pid(heat),
                pilot: pilot("p8"),
                position: 1,
                note: None,
            })
            .unwrap();
        assert_eq!(
            director.state.results()[&pid(heat)].ranking[0].pilot,
            pilot("p8")
        );

        // Only a timer or the Director logs a pass or a race fact.
        assert_eq!(
            director.record(Event::RaceStopped { race: race.clone() }),
            Err(Refusal::NotACommand)
        );
        // Undoing something that was never done is refused.
        assert_eq!(
            director.record(Event::Reinstated {
                phase: pid("bracket"),
                pilot: pilot("p1"),
            }),
            Err(Refusal::NothingToUndo)
        );
    }

    #[test]
    fn a_protest_blocks_the_lock_and_a_lock_blocks_rulings() {
        let mut director = bracket_event();
        let heat = "bracket/winners-1/heat-1";
        let race = fly(&mut director, heat);
        director
            .send(Command::FileProtest {
                race: race.clone(),
                by: Some(pilot("p4")),
                note: None,
            })
            .unwrap();
        let (protest, _) = director.state.rulings.open_protests().next().unwrap();
        let protest = protest.clone();
        assert!(matches!(
            director.record(Event::PhaseLocked { phase: pid(heat) }),
            Err(Refusal::Lock(_))
        ));
        director
            .record(Event::ProtestResolved {
                protest: protest.clone(),
                outcome: gridfpv_events::ProtestOutcome::Denied,
                note: None,
            })
            .unwrap();
        // Resolved once.
        assert_eq!(
            director.record(Event::ProtestResolved {
                protest: protest.clone(),
                outcome: gridfpv_events::ProtestOutcome::Upheld,
                note: None,
            }),
            Err(Refusal::NoOpenProtest(protest))
        );
        director
            .record(Event::PhaseLocked { phase: pid(heat) })
            .unwrap();
        // Locked: the race cannot be discarded or protested, the heat cannot be ruled on.
        assert_eq!(
            director.send(Command::DiscardRace {
                race: race.clone(),
                reason: None,
                note: None,
            }),
            Err(Refusal::Locked(pid(heat)))
        );
        assert_eq!(
            director.record(Event::HeatReopened { heat: pid(heat) }),
            Err(Refusal::Locked(pid(heat)))
        );
        // Unlocking is a ruling, and opens it again.
        assert_eq!(
            director.record(Event::PhaseUnlocked {
                phase: pid("bracket")
            }),
            Err(Refusal::NotLocked(pid("bracket")))
        );
        director
            .record(Event::PhaseUnlocked { phase: pid(heat) })
            .unwrap();
        director
            .send(Command::DiscardRace {
                race,
                reason: None,
                note: None,
            })
            .unwrap();
    }

    #[test]
    fn the_tree_is_guarded_against_orphans_loops_and_losing_raced_heats() {
        let mut director = bracket_event();
        let orphan = PhaseDef {
            id: pid("orphan"),
            label: "Orphan".into(),
            parent: Some(pid("nowhere")),
            kind: PhaseKind::Group {
                field: Vec::new(),
                combine: gridfpv_events::CombineRule::Stack,
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
        };
        assert_eq!(
            director.record(Event::PhaseDefined {
                def: orphan.clone()
            }),
            Err(Refusal::UnknownPhase(pid("nowhere")))
        );
        // Moving the bracket beneath one of its own rounds would make a loop.
        let mut looped = director.state.tree.get(&pid("bracket")).unwrap().clone();
        looped.parent = Some(pid("bracket/winners-1"));
        assert_eq!(
            director.record(Event::PhaseDefined { def: looped }),
            Err(Refusal::ParentLoop)
        );
        assert_eq!(
            director.record(Event::PhaseRemoved {
                phase: pid("bracket")
            }),
            Err(Refusal::HasChildren(pid("bracket")))
        );
        fly(&mut director, "bracket/winners-1/heat-1");
        assert_eq!(
            director.record(Event::PhaseRemoved {
                phase: pid("bracket/winners-1/heat-1")
            }),
            Err(Refusal::HasRaces(pid("bracket/winners-1/heat-1")))
        );
        director
            .record(Event::PhaseRemoved {
                phase: pid("bracket/final"),
            })
            .unwrap();
    }

    #[test]
    fn a_class_roster_feeds_a_field() {
        let mut director = Director::default();
        let open = ClassId("open".into());
        director
            .record(Event::RosterSet {
                class: open.clone(),
                pilots: vec![pilot("ana"), pilot("ben")],
            })
            .unwrap();
        director
            .record(Event::PhaseDefined {
                def: PhaseDef {
                    id: pid("qualifying"),
                    label: "Qualifying".into(),
                    parent: None,
                    kind: PhaseKind::Group {
                        field: vec![FieldSlice::Class(open.clone())],
                        combine: gridfpv_events::CombineRule::Stack,
                    },
                    format: None,
                    false_start_limit: None,
                    display_hint: None,
                },
            })
            .unwrap();
        assert_eq!(
            director.state.results()[&pid("qualifying")].field,
            vec![pilot("ana"), pilot("ben")]
        );
        director
            .record(Event::RosterRemoved { class: open })
            .unwrap();
        assert!(
            director.state.results()[&pid("qualifying")]
                .field
                .is_empty()
        );
    }

    fn solo_heat(first_lap: FirstLap) -> Director {
        let mut format = format(EndCondition::Open, 3 * S);
        format.race.first_lap = first_lap;
        let mut director = Director::default();
        director
            .record(Event::PhaseDefined {
                def: PhaseDef {
                    id: pid("heat"),
                    label: "Heat".into(),
                    parent: None,
                    kind: PhaseKind::Heat {
                        lineup: vec![HeatSeat {
                            adapter: oft(),
                            seat: SeatRef("seat-0".into()),
                            source: SeatSource::Pilot(pilot("ana")),
                        }],
                    },
                    format: Some(format),
                    false_start_limit: None,
                    display_hint: None,
                },
            })
            .unwrap();
        director
    }

    #[test]
    fn a_measured_timer_clock_lets_lap_one_be_timed_from_the_tone() {
        let mut director = solo_heat(FirstLap::FromTone);
        // The timer booted 100 seconds after the Director's clock started.
        director.append(
            0,
            Event::ClockOffset {
                adapter: oft(),
                offset_micros: -100 * S,
                error_micros: 200,
            },
        );
        director.now = 100 * S;
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        director.now = 110 * S;
        // The tone was at 103 s on the Director's clock: 3 s on the timer's.
        director.cross("seat-0", 5 * S);
        director.cross("seat-0", 15 * S);
        let race = director.state.races.get(&rid("heat#1")).unwrap();
        let laps = director.state.laps.seat(
            &rid("heat#1"),
            &oft(),
            &SeatRef("seat-0".into()),
            director.state.filters_for(race, &oft()),
        );
        let times: Vec<i64> = laps.laps().map(|lap| lap.micros()).collect();
        assert_eq!(times, vec![2 * S, 10 * S]);

        // A later, slightly different measurement does not move laps already timed.
        director.append(
            director.now,
            Event::ClockOffset {
                adapter: oft(),
                offset_micros: -100 * S + 900,
                error_micros: 100,
            },
        );
        let race = director.state.races.get(&rid("heat#1")).unwrap();
        assert_eq!(
            director.state.filters_for(race, &oft()).start_at,
            Some(SourceTime::from_micros(3 * S))
        );
        assert_eq!(director.replayed(), director.state);
    }

    #[test]
    fn a_timer_that_was_never_measured_falls_back_to_the_hole_shot() {
        let mut director = solo_heat(FirstLap::FromTone);
        director.now = 100 * S;
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        director.now = 110 * S;
        director.cross("seat-0", 5 * S);
        director.cross("seat-0", 15 * S);
        let race = director.state.races.get(&rid("heat#1")).unwrap();
        let filters = director.state.filters_for(race, &oft());
        assert_eq!(filters.start_at, None);
        let laps =
            director
                .state
                .laps
                .seat(&rid("heat#1"), &oft(), &SeatRef("seat-0".into()), filters);
        assert_eq!(laps.lap_count(), 1);
        assert!(laps.hole_shot.is_some());
    }

    #[test]
    fn the_events_lap_rules_hold_for_every_race_and_a_formats_own_minimum_lap_wins() {
        use gridfpv_events::LapRules;
        let mut director = solo_heat(FirstLap::HoleShot);
        director.append(
            0,
            Event::ClockOffset {
                adapter: oft(),
                offset_micros: 0,
                error_micros: 200,
            },
        );
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        // Staging is 3 s, so the tone is at 3 s on the timer's clock too.
        director.now = 60 * S;
        // A take-off under the gate, the hole shot, a bounce, then two laps.
        for at in [4, 10, 11, 20, 30] {
            director.cross("seat-0", at * S);
        }
        let times = |director: &Director| -> Vec<i64> {
            let race = director.state.races.get(&rid("heat#1")).unwrap();
            director
                .state
                .laps
                .seat(
                    &rid("heat#1"),
                    &oft(),
                    &SeatRef("seat-0".into()),
                    director.state.filters_for(race, &oft()),
                )
                .laps()
                .map(|lap| lap.micros())
                .collect()
        };
        // No rules: every crossing after the tone counts.
        assert_eq!(times(&director), vec![6 * S, S, 9 * S, 10 * S]);
        // The rules are set after the laps were flown, and still apply to them.
        director
            .record(Event::LapRulesSet {
                rules: LapRules {
                    min_lap_micros: Some(5 * S),
                    ignore_before_micros: Some(2 * S),
                },
            })
            .unwrap();
        assert_eq!(times(&director), vec![10 * S, 10 * S]);
        assert_eq!(director.replayed(), director.state);
        // A negative time is refused.
        assert_eq!(
            director.record(Event::LapRulesSet {
                rules: LapRules {
                    min_lap_micros: Some(-1),
                    ignore_before_micros: None,
                },
            }),
            Err(Refusal::NegativeLapRule)
        );

        // A format that carries its own minimum lap keeps it.
        let mut own = format(EndCondition::Open, 0);
        own.race.min_lap_micros = Some(20 * S);
        let race = director.state.races.get(&rid("heat#1")).unwrap().clone();
        let mut state = director.state.clone();
        let mut def = state.tree.get(&pid("heat")).unwrap().clone();
        def.format = Some(own);
        state.tree.apply(&Event::PhaseDefined { def });
        assert_eq!(
            state.filters_for(&race, &oft()).min_lap_micros,
            Some(20 * S)
        );
    }

    /// Two pilots, first to one lap, stopping `wait` after both finish (or never).
    fn two_pilot_heat(wait: Option<i64>) -> Director {
        let mut format = format(EndCondition::FirstToLaps(1), 0);
        format.race.stop_after_all_finish_micros = wait;
        let mut director = Director::default();
        director
            .record(Event::PhaseDefined {
                def: PhaseDef {
                    id: pid("heat"),
                    label: "Heat".into(),
                    parent: None,
                    kind: PhaseKind::Heat {
                        lineup: ["ana", "ben"]
                            .iter()
                            .enumerate()
                            .map(|(i, name)| HeatSeat {
                                adapter: oft(),
                                seat: SeatRef(format!("seat-{i}")),
                                source: SeatSource::Pilot(pilot(name)),
                            })
                            .collect(),
                    },
                    format: Some(format),
                    false_start_limit: None,
                    display_hint: None,
                },
            })
            .unwrap();
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        director
    }

    fn state_of(director: &Director) -> RaceState {
        director.state.races.get(&rid("heat#1")).unwrap().state
    }

    #[test]
    fn a_first_to_n_race_can_stop_itself_a_set_wait_after_everyone_finishes() {
        let mut director = two_pilot_heat(Some(60 * S));
        for (now, seat, at) in [
            (S, "seat-0", 0),
            (S, "seat-1", 0),
            (11 * S, "seat-0", 10 * S),
        ] {
            director.now = now;
            director.cross(seat, at);
        }
        // Ana has finished; Ben has not. Nothing is due.
        assert_eq!(director.state.next_due(), None);
        director.now = 14 * S;
        director.cross("seat-1", 13 * S);
        // Both have finished, 14 s in. The race stops 60 s later.
        assert_eq!(director.state.next_due(), Some(74 * S));
        director.now = 73 * S;
        director.catch_up();
        assert_eq!(state_of(&director), RaceState::Running);
        director.now = 200 * S;
        director.catch_up();
        assert_eq!(state_of(&director), RaceState::Stopped);
        assert_eq!(
            director.state.races.get(&rid("heat#1")).unwrap().stopped_at,
            Some(74 * S)
        );
        assert_eq!(director.replayed(), director.state);
    }

    #[test]
    fn removing_a_finishing_lap_during_the_wait_keeps_the_race_going() {
        let mut director = two_pilot_heat(Some(60 * S));
        director.now = S;
        director.cross("seat-0", 0);
        director.cross("seat-1", 0);
        director.now = 11 * S;
        director.cross("seat-0", 10 * S);
        // A false reading "finishes" Ben.
        let stray = director.cross("seat-1", 10 * S + 1);
        assert_eq!(director.state.next_due(), Some(71 * S));
        director.now = 20 * S;
        director
            .record(Event::LapRemoved {
                race: rid("heat#1"),
                crossing: stray,
                reason: None,
                note: None,
            })
            .unwrap();
        assert_eq!(director.state.next_due(), None);
        director.now = 500 * S;
        director.catch_up();
        assert_eq!(state_of(&director), RaceState::Running);
        // Ben really finishes; the wait starts from then.
        director.cross("seat-1", 40 * S);
        assert_eq!(director.state.next_due(), Some(560 * S));
    }

    #[test]
    fn by_default_a_first_to_n_race_waits_for_the_director() {
        let mut director = two_pilot_heat(None);
        director.now = S;
        for (seat, at) in [
            ("seat-0", 0),
            ("seat-1", 0),
            ("seat-0", 10 * S),
            ("seat-1", 11 * S),
        ] {
            director.cross(seat, at);
        }
        assert_eq!(director.state.next_due(), None);
        director.now = 10_000 * S;
        director.catch_up();
        assert_eq!(state_of(&director), RaceState::Running);
    }

    #[test]
    fn a_race_that_is_not_timed_can_carry_a_time_limit() {
        // Open, with a five minute limit.
        let mut format = format(EndCondition::Open, 0);
        format.race.max_micros = Some(300 * S);
        let mut director = Director::default();
        director
            .record(Event::PhaseDefined {
                def: PhaseDef {
                    id: pid("heat"),
                    label: "Heat".into(),
                    parent: None,
                    kind: PhaseKind::Heat {
                        lineup: vec![HeatSeat {
                            adapter: oft(),
                            seat: SeatRef("seat-0".into()),
                            source: SeatSource::Pilot(pilot("ana")),
                        }],
                    },
                    format: Some(format),
                    false_start_limit: None,
                    display_hint: None,
                },
            })
            .unwrap();
        director.now = 10 * S;
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        assert_eq!(director.state.next_due(), Some(310 * S));
        director.now = 1000 * S;
        director.catch_up();
        let race = director.state.races.get(&rid("heat#1")).unwrap();
        assert_eq!(race.state, RaceState::Stopped);
        assert_eq!(race.stopped_at, Some(310 * S));
    }

    #[test]
    fn a_first_to_n_race_stops_at_whichever_comes_first_the_wait_or_the_limit() {
        // Everyone finishes at 14 s; a 60 s wait would stop it at 74 s, but the limit is 30 s.
        let mut director = two_pilot_heat(Some(60 * S));
        let mut def = director.state.tree.get(&pid("heat")).unwrap().clone();
        def.format.as_mut().unwrap().race.max_micros = Some(30 * S);
        director.record(Event::PhaseDefined { def }).unwrap();
        // Nobody has finished: only the limit is due.
        assert_eq!(director.state.next_due(), Some(30 * S));
        for (now, seat, at) in [
            (S, "seat-0", 0),
            (S, "seat-1", 0),
            (11 * S, "seat-0", 10 * S),
            (14 * S, "seat-1", 13 * S),
        ] {
            director.now = now;
            director.cross(seat, at);
        }
        assert_eq!(director.state.next_due(), Some(30 * S));
        director.now = 100 * S;
        director.catch_up();
        assert_eq!(
            director.state.races.get(&rid("heat#1")).unwrap().stopped_at,
            Some(30 * S)
        );
    }

    #[test]
    fn the_run_queue_lists_ready_heats_and_follows_the_directors_order() {
        let mut director = bracket_event();
        let results = director.state.results();
        // Only round 1 can be raced: everything else waits on it.
        assert_eq!(
            director.state.queue(&results),
            vec![
                pid("bracket/winners-1/heat-1"),
                pid("bracket/winners-1/heat-2")
            ]
        );
        director
            .record(Event::QueueSet {
                heats: vec![pid("bracket/winners-1/heat-2")],
            })
            .unwrap();
        let results = director.state.results();
        assert_eq!(
            director.state.queue(&results),
            vec![
                pid("bracket/winners-1/heat-2"),
                pid("bracket/winners-1/heat-1")
            ]
        );
        // A queue can only name heats that exist.
        assert_eq!(
            director.record(Event::QueueSet {
                heats: vec![pid("bracket")]
            }),
            Err(Refusal::NotAHeat(pid("bracket")))
        );
        // A heat that has been raced leaves the queue; what it unlocks joins it.
        fly(&mut director, "bracket/winners-1/heat-2");
        fly(&mut director, "bracket/winners-1/heat-1");
        let results = director.state.results();
        assert_eq!(
            director.state.queue(&results),
            vec![pid("bracket/losers-1"), pid("bracket/winners-final")]
        );
    }

    #[test]
    fn the_spacing_helper_puts_the_heat_whose_pilots_waited_longest_first() {
        // Three one-pilot heats, then a heat with the pilot who just flew.
        let mut director = Director::default();
        let solo = |id: &str, who: &str| PhaseDef {
            id: pid(id),
            label: id.into(),
            parent: None,
            kind: PhaseKind::Heat {
                lineup: vec![HeatSeat {
                    adapter: oft(),
                    seat: SeatRef("seat-0".into()),
                    source: SeatSource::Pilot(pilot(who)),
                }],
            },
            format: Some(format(EndCondition::FirstToLaps(1), 0)),
            false_start_limit: None,
            display_hint: None,
        };
        for (id, who) in [("a1", "ana"), ("a2", "ana"), ("b1", "ben"), ("c1", "cat")] {
            director
                .record(Event::PhaseDefined { def: solo(id, who) })
                .unwrap();
        }
        fly(&mut director, "a1");
        let results = director.state.results();
        // By default the queue runs in the order the heats were set up: Ana again first.
        assert_eq!(
            director.state.queue(&results),
            vec![pid("a2"), pid("b1"), pid("c1")]
        );
        // Spaced out, the pilots who have not flown go first and Ana rests.
        assert_eq!(
            director.state.queue_spaced(&results),
            vec![pid("b1"), pid("c1"), pid("a2")]
        );
    }

    #[test]
    fn class_standings_take_the_first_counted_phase_and_reach_back_for_the_rest() {
        let mut director = Director::default();
        let open = ClassId("open".into());
        let roster: Vec<PilotId> = ["ana", "ben", "cat", "dee", "eli"]
            .iter()
            .map(|name| pilot(name))
            .collect();
        director
            .record(Event::RosterSet {
                class: open.clone(),
                pilots: roster.clone(),
            })
            .unwrap();
        let heat = |id: &str, who: &[&str]| PhaseDef {
            id: pid(id),
            label: id.into(),
            parent: None,
            kind: PhaseKind::Heat {
                lineup: who
                    .iter()
                    .enumerate()
                    .map(|(i, name)| HeatSeat {
                        adapter: oft(),
                        seat: SeatRef(format!("seat-{i}")),
                        source: SeatSource::Pilot(pilot(name)),
                    })
                    .collect(),
            },
            format: Some(format(EndCondition::FirstToLaps(1), 0)),
            false_start_limit: None,
            display_hint: None,
        };
        // Qualifying has four of the five; the final has the two who made it.
        director
            .record(Event::PhaseDefined {
                def: heat("qualifying", &["dee", "cat", "ben", "ana"]),
            })
            .unwrap();
        director
            .record(Event::PhaseDefined {
                def: heat("final", &["ben", "dee"]),
            })
            .unwrap();
        assert_eq!(
            director.record(Event::StandingsSet {
                class: open.clone(),
                phases: vec![pid("nope")],
            }),
            Err(Refusal::UnknownPhase(pid("nope")))
        );
        director
            .record(Event::StandingsSet {
                class: open.clone(),
                phases: vec![pid("final"), pid("qualifying")],
            })
            .unwrap();
        // Seat order wins each heat.
        fly(&mut director, "qualifying");
        fly(&mut director, "final");
        let results = director.state.results();
        let standings = director.state.class_standings(&open, &results);
        let order: Vec<(&str, u32, Option<&str>)> = standings
            .iter()
            .map(|place| {
                (
                    place.pilot.0.as_str(),
                    place.position,
                    place.placed_by.as_ref().map(|phase| phase.0.as_str()),
                )
            })
            .collect();
        assert_eq!(
            order,
            vec![
                // The final decides its two.
                ("ben", 1, Some("final")),
                ("dee", 2, Some("final")),
                // Qualifying is reached back to for the pilots who never made the final,
                // in qualifying order (Dee and Ben are already placed).
                ("cat", 3, Some("qualifying")),
                ("ana", 4, Some("qualifying")),
                // Eli is on the roster and flew nothing: last.
                ("eli", 5, None),
            ]
        );
    }

    #[test]
    fn the_marshals_flag_can_be_cleared_and_set_on_a_saved_race() {
        let mut director = bracket_event();
        let heat = "bracket/winners-1/heat-1";
        director
            .send(Command::StartRace { heat: pid(heat) })
            .unwrap();
        let race = rid(&format!("{heat}#1"));
        let flag = |needs_marshal| Event::MarshalFlagSet {
            race: rid(&format!("{heat}#1")),
            needs_marshal,
        };
        // Not while it is still running.
        assert!(matches!(
            director.record(flag(true)),
            Err(Refusal::Race(RaceError::WrongState { .. }))
        ));
        director
            .send(Command::StopRace { race: race.clone() })
            .unwrap();
        director
            .send(Command::SaveRace {
                race: race.clone(),
                needs_marshal: true,
                note: None,
            })
            .unwrap();
        assert!(director.state.races.get(&race).unwrap().needs_marshal);
        director.record(flag(false)).unwrap();
        assert!(!director.state.races.get(&race).unwrap().needs_marshal);
        // Clearing it twice is nothing to do.
        assert_eq!(director.record(flag(false)), Err(Refusal::NothingToUndo));
        director.record(flag(true)).unwrap();
        assert!(director.state.races.get(&race).unwrap().needs_marshal);
        assert_eq!(director.replayed(), director.state);
    }

    #[test]
    fn a_crossing_that_arrives_just_after_the_stop_still_belongs_to_the_race() {
        fn pass(at: i64) -> Pass {
            Pass {
                adapter: oft(),
                seat: SeatRef("seat-0".into()),
                at: SourceTime::from_micros(at),
                sequence: None,
                gate: GateIndex::LAP,
                signal: None,
                race: None,
            }
        }
        let race = Some(rid("heat#1"));

        // A timer whose clock is known: the crossing's own time decides.
        let mut director = solo_heat(FirstLap::HoleShot);
        director.append(
            0,
            Event::ClockOffset {
                adapter: oft(),
                offset_micros: -100 * S,
                error_micros: 200,
            },
        );
        director.now = 100 * S;
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        director.now = 120 * S;
        director.catch_up();
        assert_eq!(
            director.state.race_for_pass(&pass(15 * S), director.now),
            race
        );
        director
            .send(Command::StopRace {
                race: rid("heat#1"),
            })
            .unwrap();
        // Stopped at 120 s, which is 20 s on the timer. A crossing at 19.9 s is in.
        director.now = 125 * S;
        assert_eq!(
            director
                .state
                .race_for_pass(&pass(19_900_000), director.now),
            race
        );
        // One that happened after the stop is not, however soon it arrives.
        assert_eq!(
            director.state.race_for_pass(&pass(20_100_000), 120 * S + 1),
            None
        );
        // Another timer's crossing is not.
        let mut other = pass(19_900_000);
        other.adapter = gridfpv_events::AdapterId("elsewhere".into());
        assert_eq!(director.state.race_for_pass(&other, director.now), None);
        // Once the race is saved, nothing more joins it.
        director
            .send(Command::SaveRace {
                race: rid("heat#1"),
                needs_marshal: false,
                note: None,
            })
            .unwrap();
        assert_eq!(
            director
                .state
                .race_for_pass(&pass(19_900_000), director.now),
            None
        );

        // A timer whose clock is not known: only arrival time can decide.
        let mut director = solo_heat(FirstLap::HoleShot);
        director.now = 100 * S;
        director
            .send(Command::StartRace { heat: pid("heat") })
            .unwrap();
        director.now = 120 * S;
        director
            .send(Command::StopRace {
                race: rid("heat#1"),
            })
            .unwrap();
        assert_eq!(
            director.state.race_for_pass(&pass(0), 120 * S + 500_000),
            race
        );
        assert_eq!(director.state.race_for_pass(&pass(0), 122 * S), None);
    }

    #[test]
    fn a_pilot_keeps_their_seat_into_the_next_heat_and_the_higher_seed_wins_a_clash() {
        let mut director = bracket_event();
        // Round 1: seeds 1-4-5-8 on seats 0-3 of heat 1, seeds 2-3-6-7 on heat 2.
        fly(&mut director, "bracket/winners-1/heat-1");
        fly(&mut director, "bracket/winners-1/heat-2");
        let results = director.state.results();
        let on = |heat: &str| -> Vec<(String, String)> {
            results[&pid(heat)]
                .lineup
                .iter()
                .map(|seat| {
                    let who = match &seat.who {
                        SeatWho::Pilot(pilot) => pilot.0.clone(),
                        other => format!("{other:?}"),
                    };
                    (seat.seat.0.clone(), who)
                })
                .collect()
        };
        // The winners final takes the top two of each heat. Seat order won, so p1 (seat 0)
        // and p4 (seat 1) come from heat 1, and p2 (seat 0) and p3 (seat 1) from heat 2.
        // The lineup's seeds are p1, p2, p4, p3 (winners first). p1 and p2 both flew seat
        // 0: p1 is seeded higher and keeps it. p4 and p3 both flew seat 1: p4 keeps it.
        // The two who had to move take the seats nobody asked for, in seed order.
        assert_eq!(
            on("bracket/winners-final"),
            vec![
                ("seat-0".to_owned(), "p1".to_owned()),
                ("seat-1".to_owned(), "p4".to_owned()),
                ("seat-2".to_owned(), "p2".to_owned()),
                ("seat-3".to_owned(), "p3".to_owned()),
            ]
        );
        // The losers heat takes 3rd and 4th of each: p5 and p8 flew seats 2 and 3 in heat
        // 1 and are seeded above p6 and p7, so they keep them.
        let losers = on("bracket/losers-1");
        assert!(
            losers.contains(&("seat-2".to_owned(), "p5".to_owned())),
            "{losers:?}"
        );
        assert!(
            losers.contains(&("seat-3".to_owned(), "p8".to_owned())),
            "{losers:?}"
        );
        // The field (the seed order) is not changed by where pilots sit.
        assert_eq!(
            results[&pid("bracket/winners-final")].field,
            vec![pilot("p1"), pilot("p2"), pilot("p4"), pilot("p3")]
        );

        // Once the heat has been flown its lineup stays as it was flown.
        fly(&mut director, "bracket/winners-final");
        let results = director.state.results();
        let after: Vec<String> = results[&pid("bracket/winners-final")]
            .lineup
            .iter()
            .map(|seat| match &seat.who {
                SeatWho::Pilot(pilot) => pilot.0.clone(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(after, vec!["p1", "p4", "p2", "p3"]);
        assert_eq!(director.replayed(), director.state);
    }

    #[test]
    fn a_seat_the_director_gave_to_a_named_pilot_is_never_moved() {
        let mut director = Director::default();
        let heat = |id: &str, who: [&str; 2]| PhaseDef {
            id: pid(id),
            label: id.into(),
            parent: None,
            kind: PhaseKind::Heat {
                lineup: who
                    .iter()
                    .enumerate()
                    .map(|(i, name)| HeatSeat {
                        adapter: oft(),
                        seat: SeatRef(format!("seat-{i}")),
                        source: SeatSource::Pilot(pilot(name)),
                    })
                    .collect(),
            },
            format: Some(format(EndCondition::FirstToLaps(1), 0)),
            false_start_limit: None,
            display_hint: None,
        };
        director
            .record(Event::PhaseDefined {
                def: heat("one", ["ana", "ben"]),
            })
            .unwrap();
        // The director puts them the other way round in the second heat, on purpose.
        director
            .record(Event::PhaseDefined {
                def: heat("two", ["ben", "ana"]),
            })
            .unwrap();
        fly(&mut director, "one");
        let results = director.state.results();
        let two: Vec<&SeatWho> = results[&pid("two")].lineup.iter().map(|s| &s.who).collect();
        assert_eq!(
            two,
            vec![&SeatWho::Pilot(pilot("ben")), &SeatWho::Pilot(pilot("ana"))]
        );
    }
}
