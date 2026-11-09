//! The race: one start-to-stop flight (`docs/primitives.md` §5.1).
//!
//! ```text
//! (staging) → running → stopped → saved → discarded
//!                               → discarded
//! ```
//!
//! - Staging is the countdown inside the start. It is optional: a race the source starts
//!   has none.
//! - A stopped race is **undecided** until the director saves or discards it, and no other
//!   race can start until they do.
//! - A saved race can still be discarded, in one step.
//! - There is no "restart": a restart is a discard followed by the next race of the same
//!   heat.
//!
//! [`Races`] is the fold; [`Races::check`] turns a [`RaceCommand`] into the facts to append.

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{DiscardReason, Event, HeatId, LineupEntry, RaceId, RaceSetup};

/// Where a race is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RaceState {
    /// The countdown is running; the tone has not sounded.
    Staging,
    /// The race is on.
    Running,
    /// The race has stopped and waits for the director to save or discard it.
    Stopped,
    /// The director saved it: it counts (provisionally, until a phase is locked).
    Saved,
    /// The director discarded it: it stays in the log and counts toward nothing.
    Discarded,
}

impl RaceState {
    /// A race that still blocks every other race from starting: it is staging, running, or
    /// stopped and not yet saved or discarded.
    pub fn is_undecided(self) -> bool {
        matches!(self, Self::Staging | Self::Running | Self::Stopped)
    }
}

/// One race, as folded from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Race {
    /// The race's id, heat, lineup and start owner.
    pub setup: RaceSetup,
    /// Where it is in its life.
    pub state: RaceState,
    /// The planned start tone (server clock, microseconds), when the race was staged.
    pub tone_at: Option<i64>,
    /// When the log recorded the start (server clock, microseconds), where the append
    /// carried a time. This is the anchor of the race clock.
    pub started_at: Option<i64>,
    /// When the log recorded the stop.
    pub stopped_at: Option<i64>,
    /// Set by a save: the race is on the marshal's to-do list.
    pub needs_marshal: bool,
    /// The note from the save or the discard, whichever came last.
    pub note: Option<String>,
    /// Why it was discarded, when a reason was picked.
    pub discard_reason: Option<DiscardReason>,
}

impl Race {
    /// Whether this race counts toward rankings: only a saved race does.
    pub fn counts(&self) -> bool {
        self.state == RaceState::Saved
    }
}

/// Every race in an event's log, in the order they were first logged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Races {
    by_id: BTreeMap<RaceId, Race>,
    order: Vec<RaceId>,
}

impl Races {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a whole log. Each item is an event and the time the log recorded it, if any.
    pub fn fold<'a>(events: impl IntoIterator<Item = (Option<i64>, &'a Event)>) -> Self {
        let mut races = Self::new();
        for (recorded_at, event) in events {
            races.apply(recorded_at, event);
        }
        races
    }

    /// Apply one logged event. Events that are not race facts are ignored.
    ///
    /// The fold is **total**: it never panics and never rejects. A fact that makes no sense
    /// for the race's current state (a stop for a race that is not running, a fact for a race
    /// that was never started) is ignored, so replaying any log gives a state. Legality is
    /// [`Races::check`]'s job, before the append.
    pub fn apply(&mut self, recorded_at: Option<i64>, event: &Event) {
        match event {
            Event::RaceStarting { setup, tone_at } => {
                if !self.by_id.contains_key(&setup.race) {
                    self.insert(Race {
                        setup: setup.clone(),
                        state: RaceState::Staging,
                        tone_at: Some(*tone_at),
                        started_at: None,
                        stopped_at: None,
                        needs_marshal: false,
                        note: None,
                        discard_reason: None,
                    });
                }
            }
            Event::RaceStarted { setup } => match self.by_id.get_mut(&setup.race) {
                Some(race) if race.state == RaceState::Staging => {
                    race.setup = setup.clone();
                    race.state = RaceState::Running;
                    race.started_at = recorded_at;
                }
                Some(_) => {}
                None => self.insert(Race {
                    setup: setup.clone(),
                    state: RaceState::Running,
                    tone_at: None,
                    started_at: recorded_at,
                    stopped_at: None,
                    needs_marshal: false,
                    note: None,
                    discard_reason: None,
                }),
            },
            Event::RaceStopped { race } => {
                if let Some(race) = self.by_id.get_mut(race) {
                    if race.state == RaceState::Running {
                        race.state = RaceState::Stopped;
                        race.stopped_at = recorded_at;
                    }
                }
            }
            Event::RaceSaved {
                race,
                needs_marshal,
                note,
            } => {
                if let Some(race) = self.by_id.get_mut(race) {
                    if race.state == RaceState::Stopped {
                        race.state = RaceState::Saved;
                        race.needs_marshal = *needs_marshal;
                        race.note = note.clone();
                    }
                }
            }
            Event::RaceDiscarded { race, reason, note } => {
                if let Some(race) = self.by_id.get_mut(race) {
                    if race.state != RaceState::Discarded {
                        race.state = RaceState::Discarded;
                        race.discard_reason = *reason;
                        race.needs_marshal = false;
                        if note.is_some() {
                            race.note = note.clone();
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn insert(&mut self, race: Race) {
        self.order.push(race.setup.race.clone());
        self.by_id.insert(race.setup.race.clone(), race);
    }

    /// One race by id.
    pub fn get(&self, race: &RaceId) -> Option<&Race> {
        self.by_id.get(race)
    }

    /// Every race, in the order they were first logged.
    pub fn iter(&self) -> impl Iterator<Item = &Race> {
        self.order.iter().filter_map(|id| self.by_id.get(id))
    }

    /// The races of one heat, in order. A heat can be raced more than once.
    pub fn of_heat<'a>(&'a self, heat: &'a HeatId) -> impl Iterator<Item = &'a Race> {
        self.iter().filter(move |race| &race.setup.heat == heat)
    }

    /// The race that is staging, running, or stopped and not yet decided, if there is one.
    /// While it exists, no other race can start.
    pub fn undecided(&self) -> Option<&Race> {
        self.iter().find(|race| race.state.is_undecided())
    }

    /// The race a pass should be stamped with: the one that is running now.
    pub fn running(&self) -> Option<&Race> {
        self.iter().find(|race| race.state == RaceState::Running)
    }

    /// Check a command against the current state. Gives the facts to append, in order, or
    /// the reason the command is refused. Appends nothing itself.
    pub fn check(&self, command: RaceCommand) -> Result<Vec<Event>, RaceError> {
        match command {
            RaceCommand::Stage { setup, tone_at } => {
                self.check_new(&setup)?;
                Ok(vec![Event::RaceStarting { setup, tone_at }])
            }
            RaceCommand::Start { setup } => {
                match self.by_id.get(&setup.race) {
                    // The tone after a staging: the same race goes from staging to running.
                    Some(race) if race.state == RaceState::Staging => {
                        if race.setup != setup {
                            return Err(RaceError::SetupChanged(setup.race));
                        }
                    }
                    Some(_) => return Err(RaceError::RaceIdUsed(setup.race)),
                    None => self.check_new(&setup)?,
                }
                Ok(vec![Event::RaceStarted { setup }])
            }
            RaceCommand::Stop { race } => {
                let found = self.require(&race)?;
                if found.state != RaceState::Running {
                    return Err(RaceError::WrongState {
                        race,
                        state: found.state,
                        wanted: "stop",
                    });
                }
                Ok(vec![Event::RaceStopped { race }])
            }
            RaceCommand::Save {
                race,
                needs_marshal,
                note,
            } => {
                let found = self.require(&race)?;
                if found.state != RaceState::Stopped {
                    return Err(RaceError::WrongState {
                        race,
                        state: found.state,
                        wanted: "save",
                    });
                }
                Ok(vec![Event::RaceSaved {
                    race,
                    needs_marshal,
                    note,
                }])
            }
            RaceCommand::Discard { race, reason, note } => {
                let found = self.require(&race)?;
                let mut facts = Vec::new();
                match found.state {
                    RaceState::Discarded => {
                        return Err(RaceError::WrongState {
                            race,
                            state: found.state,
                            wanted: "discard",
                        });
                    }
                    // A false start called mid-race: the race is stopped and discarded.
                    RaceState::Running => facts.push(Event::RaceStopped { race: race.clone() }),
                    RaceState::Staging | RaceState::Stopped | RaceState::Saved => {}
                }
                facts.push(Event::RaceDiscarded { race, reason, note });
                Ok(facts)
            }
        }
    }

    fn require(&self, race: &RaceId) -> Result<&Race, RaceError> {
        self.by_id
            .get(race)
            .ok_or_else(|| RaceError::UnknownRace(race.clone()))
    }

    /// The checks every brand-new race must pass.
    fn check_new(&self, setup: &RaceSetup) -> Result<(), RaceError> {
        if self.by_id.contains_key(&setup.race) {
            return Err(RaceError::RaceIdUsed(setup.race.clone()));
        }
        if let Some(open) = self.undecided() {
            return Err(RaceError::AnotherRaceUndecided {
                race: open.setup.race.clone(),
                state: open.state,
            });
        }
        check_lineup(&setup.lineup)
    }
}

/// A lineup must seat someone, must not use a seat twice, and must not seat a pilot twice.
fn check_lineup(lineup: &[LineupEntry]) -> Result<(), RaceError> {
    if lineup.is_empty() {
        return Err(RaceError::EmptyLineup);
    }
    let mut seats = BTreeSet::new();
    let mut pilots = BTreeSet::new();
    for entry in lineup {
        if !seats.insert((&entry.adapter, &entry.seat)) {
            return Err(RaceError::SeatUsedTwice {
                adapter: entry.adapter.0.clone(),
                seat: entry.seat.0.clone(),
            });
        }
        if let Some(pilot) = &entry.pilot {
            if !pilots.insert(pilot) {
                return Err(RaceError::PilotSeatedTwice(pilot.0.clone()));
            }
        }
    }
    Ok(())
}

/// What the director (or a source that owns its own start) asks of a race.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RaceCommand {
    /// Begin the countdown for a new race. `tone_at` is the planned tone on the server
    /// clock, in microseconds.
    Stage { setup: RaceSetup, tone_at: i64 },
    /// The tone: the staged race is now running. Also starts a race that was never staged
    /// (a source that owns its start).
    Start { setup: RaceSetup },
    /// Stop a running race.
    Stop { race: RaceId },
    /// Save a stopped race, so it counts.
    Save {
        race: RaceId,
        needs_marshal: bool,
        note: Option<String>,
    },
    /// Discard a race. Allowed while staging, running, stopped, or already saved.
    Discard {
        race: RaceId,
        reason: Option<DiscardReason>,
        note: Option<String>,
    },
}

/// Why a race command was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RaceError {
    /// No race has this id.
    #[error("there is no race {0:?}")]
    UnknownRace(RaceId),
    /// A new race must have a new id.
    #[error("race id {0:?} is already used")]
    RaceIdUsed(RaceId),
    /// Another race is staging, running, or waiting to be saved or discarded.
    #[error("race {race:?} is {state:?}; save or discard it before starting another")]
    AnotherRaceUndecided { race: RaceId, state: RaceState },
    /// The tone arrived with a different setup than the one that was staged.
    #[error("race {0:?} was staged with a different heat, lineup or start owner")]
    SetupChanged(RaceId),
    /// The race is not in a state this command applies to.
    #[error("cannot {wanted} race {race:?} while it is {state:?}")]
    WrongState {
        race: RaceId,
        state: RaceState,
        wanted: &'static str,
    },
    /// A race needs at least one seat in its lineup.
    #[error("a race needs at least one seat in its lineup")]
    EmptyLineup,
    /// One seat appears twice in the lineup.
    #[error("seat {seat:?} on timer {adapter:?} is in the lineup twice")]
    SeatUsedTwice { adapter: String, seat: String },
    /// One pilot is on two seats.
    #[error("pilot {0:?} is in the lineup twice")]
    PilotSeatedTwice(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{AdapterId, PilotId, SeatRef, StartOwner};

    fn entry(seat: &str, pilot: Option<&str>) -> LineupEntry {
        LineupEntry {
            adapter: AdapterId("oft".into()),
            seat: SeatRef(seat.into()),
            pilot: pilot.map(|p| PilotId(p.into())),
        }
    }

    fn setup(race: &str, heat: &str) -> RaceSetup {
        RaceSetup {
            race: RaceId(race.into()),
            heat: HeatId(heat.into()),
            lineup: vec![entry("seat-0", Some("ana")), entry("seat-1", Some("ben"))],
            owner: StartOwner::Grid,
        }
    }

    fn id(race: &str) -> RaceId {
        RaceId(race.into())
    }

    /// Check a command and apply the facts it gives, as the Director would.
    fn run(races: &mut Races, at: i64, command: RaceCommand) -> Result<(), RaceError> {
        for fact in races.check(command)? {
            races.apply(Some(at), &fact);
        }
        Ok(())
    }

    fn start(races: &mut Races, race: &str, heat: &str) {
        run(
            races,
            10,
            RaceCommand::Start {
                setup: setup(race, heat),
            },
        )
        .unwrap();
    }

    fn stop(races: &mut Races, race: &str) {
        run(races, 20, RaceCommand::Stop { race: id(race) }).unwrap();
    }

    fn save(races: &mut Races, race: &str) {
        let command = RaceCommand::Save {
            race: id(race),
            needs_marshal: false,
            note: None,
        };
        run(races, 30, command).unwrap();
    }

    fn discard(races: &mut Races, race: &str, reason: Option<DiscardReason>) {
        let command = RaceCommand::Discard {
            race: id(race),
            reason,
            note: None,
        };
        run(races, 40, command).unwrap();
    }

    fn state(races: &Races, race: &str) -> RaceState {
        races.get(&id(race)).unwrap().state
    }

    #[test]
    fn a_race_runs_stage_start_stop_save() {
        let mut races = Races::new();
        let stage = RaceCommand::Stage {
            setup: setup("r1", "h1"),
            tone_at: 5_000_000,
        };
        run(&mut races, 1, stage).unwrap();
        assert_eq!(state(&races, "r1"), RaceState::Staging);
        assert_eq!(races.get(&id("r1")).unwrap().tone_at, Some(5_000_000));

        start(&mut races, "r1", "h1");
        assert_eq!(state(&races, "r1"), RaceState::Running);
        assert_eq!(races.get(&id("r1")).unwrap().started_at, Some(10));
        assert_eq!(races.running().unwrap().setup.race, id("r1"));

        stop(&mut races, "r1");
        assert_eq!(state(&races, "r1"), RaceState::Stopped);
        assert_eq!(races.get(&id("r1")).unwrap().stopped_at, Some(20));
        assert!(races.running().is_none());

        let command = RaceCommand::Save {
            race: id("r1"),
            needs_marshal: true,
            note: Some("check seat 1".into()),
        };
        run(&mut races, 30, command).unwrap();
        let race = races.get(&id("r1")).unwrap();
        assert_eq!(race.state, RaceState::Saved);
        assert!(race.counts());
        assert!(race.needs_marshal);
        assert_eq!(race.note.as_deref(), Some("check seat 1"));
        assert!(races.undecided().is_none());
    }

    #[test]
    fn a_source_owned_race_starts_with_no_staging() {
        let mut races = Races::new();
        let mut sim = setup("r1", "h1");
        sim.owner = StartOwner::Source;
        run(&mut races, 10, RaceCommand::Start { setup: sim }).unwrap();
        let race = races.get(&id("r1")).unwrap();
        assert_eq!(race.state, RaceState::Running);
        assert_eq!(race.tone_at, None);
        assert_eq!(race.setup.owner, StartOwner::Source);
    }

    #[test]
    fn the_director_must_decide_a_race_before_the_next_can_start() {
        let mut races = Races::new();
        start(&mut races, "r1", "h1");

        // Running blocks, and so does stopped-but-undecided — for any heat.
        for _ in 0..2 {
            let refused = races.check(RaceCommand::Start {
                setup: setup("r2", "h2"),
            });
            assert!(matches!(
                refused,
                Err(RaceError::AnotherRaceUndecided { ref race, .. }) if *race == id("r1")
            ));
            if state(&races, "r1") == RaceState::Running {
                stop(&mut races, "r1");
            }
        }
        let staged = races.check(RaceCommand::Stage {
            setup: setup("r2", "h2"),
            tone_at: 0,
        });
        assert!(matches!(
            staged,
            Err(RaceError::AnotherRaceUndecided { .. })
        ));

        save(&mut races, "r1");
        start(&mut races, "r2", "h2");
        assert_eq!(state(&races, "r2"), RaceState::Running);
    }

    #[test]
    fn a_stopped_race_does_not_count_until_it_is_saved() {
        let mut races = Races::new();
        start(&mut races, "r1", "h1");
        stop(&mut races, "r1");
        assert!(!races.get(&id("r1")).unwrap().counts());
        // Nothing saves it automatically: it is still undecided.
        assert_eq!(races.undecided().unwrap().setup.race, id("r1"));
    }

    #[test]
    fn a_restart_is_a_discard_then_the_next_race_of_the_same_heat() {
        let mut races = Races::new();
        start(&mut races, "r1", "h1");
        stop(&mut races, "r1");
        discard(&mut races, "r1", Some(DiscardReason::CrashOrObstruction));
        start(&mut races, "r2", "h1");
        stop(&mut races, "r2");
        save(&mut races, "r2");

        let heat = HeatId("h1".into());
        let of_heat: Vec<_> = races
            .of_heat(&heat)
            .map(|r| (r.setup.race.clone(), r.state))
            .collect();
        assert_eq!(
            of_heat,
            vec![
                (id("r1"), RaceState::Discarded),
                (id("r2"), RaceState::Saved)
            ]
        );
        // The discarded race stays in the fold, with its reason, and counts toward nothing.
        let first = races.get(&id("r1")).unwrap();
        assert!(!first.counts());
        assert_eq!(
            first.discard_reason,
            Some(DiscardReason::CrashOrObstruction)
        );
    }

    #[test]
    fn a_heat_can_be_raced_more_than_once_and_every_saved_race_counts() {
        let mut races = Races::new();
        for race in ["r1", "r2", "r3"] {
            start(&mut races, race, "final");
            stop(&mut races, race);
            save(&mut races, race);
        }
        let heat = HeatId("final".into());
        assert_eq!(races.of_heat(&heat).filter(|r| r.counts()).count(), 3);
    }

    #[test]
    fn a_saved_race_can_be_discarded_in_one_step() {
        // A false start called after the full race was run and saved.
        let mut races = Races::new();
        start(&mut races, "r1", "h1");
        stop(&mut races, "r1");
        let command = RaceCommand::Save {
            race: id("r1"),
            needs_marshal: true,
            note: None,
        };
        run(&mut races, 30, command).unwrap();

        let facts = races
            .check(RaceCommand::Discard {
                race: id("r1"),
                reason: Some(DiscardReason::FalseStart),
                note: None,
            })
            .unwrap();
        assert_eq!(facts.len(), 1, "no un-save step first");
        discard(&mut races, "r1", Some(DiscardReason::FalseStart));
        let race = races.get(&id("r1")).unwrap();
        assert_eq!(race.state, RaceState::Discarded);
        assert!(!race.counts());
        assert!(
            !race.needs_marshal,
            "a discarded race is off the marshal's list"
        );
    }

    #[test]
    fn discarding_a_running_race_stops_it_first() {
        let mut races = Races::new();
        start(&mut races, "r1", "h1");
        let facts = races
            .check(RaceCommand::Discard {
                race: id("r1"),
                reason: Some(DiscardReason::FalseStart),
                note: None,
            })
            .unwrap();
        assert!(matches!(facts[0], Event::RaceStopped { .. }));
        assert!(matches!(facts[1], Event::RaceDiscarded { .. }));
        for fact in &facts {
            races.apply(Some(15), fact);
        }
        let race = races.get(&id("r1")).unwrap();
        assert_eq!(race.state, RaceState::Discarded);
        assert_eq!(race.stopped_at, Some(15));
        assert!(races.undecided().is_none());
    }

    #[test]
    fn a_countdown_can_be_called_off() {
        let mut races = Races::new();
        let stage = RaceCommand::Stage {
            setup: setup("r1", "h1"),
            tone_at: 5,
        };
        run(&mut races, 1, stage).unwrap();
        discard(&mut races, "r1", None);
        assert_eq!(state(&races, "r1"), RaceState::Discarded);
        assert_eq!(races.get(&id("r1")).unwrap().discard_reason, None);
        start(&mut races, "r2", "h1");
    }

    #[test]
    fn the_discard_reason_is_optional() {
        let mut races = Races::new();
        start(&mut races, "r1", "h1");
        stop(&mut races, "r1");
        discard(&mut races, "r1", None);
        assert_eq!(state(&races, "r1"), RaceState::Discarded);
    }

    #[test]
    fn commands_out_of_order_are_refused() {
        let mut races = Races::new();
        assert_eq!(
            races.check(RaceCommand::Stop { race: id("nope") }),
            Err(RaceError::UnknownRace(id("nope")))
        );
        start(&mut races, "r1", "h1");
        // Save before stop.
        let early_save = races.check(RaceCommand::Save {
            race: id("r1"),
            needs_marshal: false,
            note: None,
        });
        assert!(matches!(
            early_save,
            Err(RaceError::WrongState { wanted: "save", .. })
        ));
        stop(&mut races, "r1");
        // Stop twice.
        let again = races.check(RaceCommand::Stop { race: id("r1") });
        assert!(matches!(
            again,
            Err(RaceError::WrongState { wanted: "stop", .. })
        ));
        discard(&mut races, "r1", None);
        // Discard twice, and save after discard.
        let twice = races.check(RaceCommand::Discard {
            race: id("r1"),
            reason: None,
            note: None,
        });
        assert!(matches!(
            twice,
            Err(RaceError::WrongState {
                wanted: "discard",
                ..
            })
        ));
        let late_save = races.check(RaceCommand::Save {
            race: id("r1"),
            needs_marshal: false,
            note: None,
        });
        assert!(matches!(
            late_save,
            Err(RaceError::WrongState { wanted: "save", .. })
        ));
    }

    #[test]
    fn a_race_id_is_never_reused() {
        let mut races = Races::new();
        start(&mut races, "r1", "h1");
        stop(&mut races, "r1");
        discard(&mut races, "r1", None);
        assert_eq!(
            races.check(RaceCommand::Start {
                setup: setup("r1", "h1")
            }),
            Err(RaceError::RaceIdUsed(id("r1")))
        );
    }

    #[test]
    fn the_tone_must_match_what_was_staged() {
        let mut races = Races::new();
        let stage = RaceCommand::Stage {
            setup: setup("r1", "h1"),
            tone_at: 5,
        };
        run(&mut races, 1, stage).unwrap();
        let mut changed = setup("r1", "h1");
        changed.lineup.pop();
        assert_eq!(
            races.check(RaceCommand::Start { setup: changed }),
            Err(RaceError::SetupChanged(id("r1")))
        );
    }

    #[test]
    fn a_lineup_is_checked() {
        let races = Races::new();
        let mut empty = setup("r1", "h1");
        empty.lineup.clear();
        assert_eq!(
            races.check(RaceCommand::Start { setup: empty }),
            Err(RaceError::EmptyLineup)
        );

        let mut same_seat = setup("r1", "h1");
        same_seat.lineup = vec![entry("seat-0", Some("ana")), entry("seat-0", Some("ben"))];
        assert!(matches!(
            races.check(RaceCommand::Start { setup: same_seat }),
            Err(RaceError::SeatUsedTwice { .. })
        ));

        let mut same_pilot = setup("r1", "h1");
        same_pilot.lineup = vec![entry("seat-0", Some("ana")), entry("seat-1", Some("ana"))];
        assert_eq!(
            races.check(RaceCommand::Start { setup: same_pilot }),
            Err(RaceError::PilotSeatedTwice("ana".into()))
        );

        // Open seats (practice) are fine, and two of them are not "the same pilot twice".
        let mut open = setup("r1", "h1");
        open.lineup = vec![entry("seat-0", None), entry("seat-1", None)];
        assert!(races.check(RaceCommand::Start { setup: open }).is_ok());
    }

    #[test]
    fn the_fold_ignores_facts_that_make_no_sense_and_never_panics() {
        // A log is replayed as it is. Facts for an unknown race, or out of order, change
        // nothing; legality was the check's job before the append.
        let unknown = id("ghost");
        let events = [
            Event::RaceStopped {
                race: unknown.clone(),
            },
            Event::RaceSaved {
                race: unknown.clone(),
                needs_marshal: false,
                note: None,
            },
            Event::RaceDiscarded {
                race: unknown,
                reason: None,
                note: None,
            },
            Event::RaceStarted {
                setup: setup("r1", "h1"),
            },
            // Save while still running: ignored.
            Event::RaceSaved {
                race: id("r1"),
                needs_marshal: false,
                note: None,
            },
            // A second start for the same id: ignored.
            Event::RaceStarted {
                setup: setup("r1", "other-heat"),
            },
        ];
        let races = Races::fold(events.iter().map(|e| (None, e)));
        assert_eq!(races.iter().count(), 1);
        let race = races.get(&id("r1")).unwrap();
        assert_eq!(race.state, RaceState::Running);
        assert_eq!(race.setup.heat, HeatId("h1".into()));
    }

    #[test]
    fn replaying_the_same_log_gives_the_same_state() {
        let mut live = Races::new();
        let mut log: Vec<(Option<i64>, Event)> = Vec::new();
        let commands = vec![
            RaceCommand::Stage {
                setup: setup("r1", "h1"),
                tone_at: 5,
            },
            RaceCommand::Start {
                setup: setup("r1", "h1"),
            },
            RaceCommand::Discard {
                race: id("r1"),
                reason: Some(DiscardReason::FalseStart),
                note: None,
            },
            RaceCommand::Start {
                setup: setup("r2", "h1"),
            },
            RaceCommand::Stop { race: id("r2") },
            RaceCommand::Save {
                race: id("r2"),
                needs_marshal: false,
                note: None,
            },
        ];
        for (n, command) in commands.into_iter().enumerate() {
            for fact in live.check(command).unwrap() {
                live.apply(Some(n as i64), &fact);
                log.push((Some(n as i64), fact));
            }
        }
        let replayed = Races::fold(log.iter().map(|(at, e)| (*at, e)));
        assert_eq!(replayed, live);
    }
}
