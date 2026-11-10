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
    ClassId, Command, EndCondition, Event, Format, LineupEntry, PhaseId, PhaseKind, PilotId,
    ProtestId, RaceId, RaceSetup, StartOwner,
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

/// One event's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventState {
    pub tree: Tree,
    pub races: Races,
    pub laps: RaceLaps,
    pub rulings: Rulings,
    /// Each class in the event and its roster, in roster order.
    pub rosters: BTreeMap<ClassId, Vec<PilotId>>,
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
            _ => {}
        }
        self.applied = offset + 1;
    }

    /// Work out every phase: fields, lineups, rankings and what is complete.
    pub fn results(&self) -> BTreeMap<PhaseId, PhaseResult> {
        self.tree.evaluate(self)
    }

    /// The lap filters a heat's races are read under: from the format in force.
    pub fn filters_for(&self, heat: &PhaseId) -> LapFilters {
        let rules = self.tree.format_for(heat).map(|format| &format.race);
        LapFilters {
            min_lap_micros: rules.and_then(|rules| rules.min_lap_micros),
            // No timer gives the tone on its own clock yet (the OFT adapter will).
            start_at: None,
            first_lap: rules.map(|rules| rules.first_lap).unwrap_or_default(),
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

    /// When a race that started at `started` stops by itself: a timed race, after its time
    /// and its grace window. Other races run until the director stops them.
    fn runs_out(&self, race: &Race, started: i64) -> Option<i64> {
        match self.tree.format_for(&race.setup.heat)?.race.end {
            EndCondition::Timed {
                micros,
                grace_micros,
            } => Some(started + micros + grace_micros),
            EndCondition::FirstToLaps(_) | EndCondition::Open => None,
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

            Event::LapRemoved { race, crossing, .. }
            | Event::LapRestored { race, crossing }
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
        let filters = self.filters_for(heat);
        self.races
            .of_heat(heat)
            .filter(|race| race.counts())
            .map(|race| {
                race.setup
                    .lineup
                    .iter()
                    .filter_map(|entry| {
                        let pilot = entry.pilot.clone()?;
                        let laps =
                            self.laps
                                .seat(&race.setup.race, &entry.adapter, &entry.seat, filters);
                        Some(RaceEntry { pilot, laps })
                    })
                    .collect()
            })
            .collect()
    }

    fn director_done(&self, heat: &PhaseId) -> bool {
        self.rulings.director_done(heat)
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
}
