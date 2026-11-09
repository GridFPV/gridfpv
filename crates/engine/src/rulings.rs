//! Rulings about pilots and phases: false starts, disqualification, locking and protests
//! (`docs/primitives.md` §5.7, §5.8).
//!
//! A ruling is a human decision recorded as a fact. Grid counts and proposes; it never
//! rules on its own. [`Rulings`] is the fold of those facts. Its questions need the phase
//! tree and the races too, so they take them as arguments; the fold itself stays simple.
//!
//! (Lap rulings — remove, insert, adjust, restart — live with the laps they change, in
//! [`crate::laps`].)

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{Event, PhaseId, PilotId, ProtestId, ProtestOutcome, RaceId};

use crate::phase::Tree;
use crate::race::{RaceState, Races};

/// One protest, as folded from the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protest {
    pub race: RaceId,
    pub by: Option<PilotId>,
    pub note: Option<String>,
    /// `None` while the protest is open. A protest is resolved once.
    pub outcome: Option<ProtestOutcome>,
}

/// A disqualification Grid proposes because a pilot reached a phase's false start limit.
/// The director decides whether to make it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedDisqualification {
    /// The phase that carries the limit.
    pub phase: PhaseId,
    pub pilot: PilotId,
    /// How many false starts the pilot has beneath that phase.
    pub false_starts: u32,
    pub limit: u32,
}

/// Why a lock was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LockError {
    #[error("there is no phase {0:?}")]
    UnknownPhase(PhaseId),
    #[error("phase {0:?} is already locked")]
    AlreadyLocked(PhaseId),
    /// A race beneath the phase still has an open protest.
    #[error("protest {protest:?} on race {race:?} is still open")]
    OpenProtest { protest: ProtestId, race: RaceId },
    /// A race beneath the phase is staging, running, or waiting to be saved or discarded.
    #[error("race {0:?} is not decided yet")]
    UndecidedRace(RaceId),
}

/// The fold of every pilot and phase ruling in an event's log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rulings {
    false_starts: BTreeSet<(RaceId, PilotId)>,
    /// Each disqualification, and whether the pilot's earned place stays empty.
    disqualified: BTreeMap<(PhaseId, PilotId), bool>,
    /// Positions the director set by hand, per heat.
    positions: BTreeMap<(PhaseId, PilotId), u32>,
    locked: BTreeSet<PhaseId>,
    done: BTreeSet<PhaseId>,
    protests: BTreeMap<ProtestId, Protest>,
}

impl Rulings {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a whole log.
    pub fn fold<'a>(events: impl IntoIterator<Item = &'a Event>) -> Self {
        let mut rulings = Self::new();
        for event in events {
            rulings.apply(event);
        }
        rulings
    }

    /// Apply one logged event. Other events are ignored; the fold is total.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::FalseStart { race, pilot } => {
                self.false_starts.insert((race.clone(), pilot.clone()));
            }
            Event::FalseStartCleared { race, pilot } => {
                self.false_starts.remove(&(race.clone(), pilot.clone()));
            }
            Event::Disqualified {
                phase,
                pilot,
                keep_place_empty,
                ..
            } => {
                self.disqualified
                    .insert((phase.clone(), pilot.clone()), *keep_place_empty);
            }
            Event::PositionSet {
                heat,
                pilot,
                position,
                ..
            } => {
                self.positions
                    .insert((heat.clone(), pilot.clone()), *position);
            }
            Event::PositionCleared { heat, pilot } => {
                self.positions.remove(&(heat.clone(), pilot.clone()));
            }
            Event::Reinstated { phase, pilot } => {
                self.disqualified.remove(&(phase.clone(), pilot.clone()));
            }
            Event::PhaseLocked { phase } => {
                self.locked.insert(phase.clone());
            }
            Event::PhaseUnlocked { phase } => {
                self.locked.remove(phase);
            }
            Event::HeatDone { heat } => {
                self.done.insert(heat.clone());
            }
            Event::HeatReopened { heat } => {
                self.done.remove(heat);
            }
            Event::ProtestFiled {
                protest,
                race,
                by,
                note,
            } => {
                self.protests
                    .entry(protest.clone())
                    .or_insert_with(|| Protest {
                        race: race.clone(),
                        by: by.clone(),
                        note: note.clone(),
                        outcome: None,
                    });
            }
            Event::ProtestResolved {
                protest, outcome, ..
            } => {
                if let Some(found) = self.protests.get_mut(protest) {
                    // Resolved once: a second resolution changes nothing.
                    found.outcome.get_or_insert(*outcome);
                }
            }
            _ => {}
        }
    }

    /// Whether the director has said a heat is done.
    pub fn director_done(&self, heat: &PhaseId) -> bool {
        self.done.contains(heat)
    }

    /// The pilots disqualified from exactly this phase.
    pub fn disqualified_from(&self, phase: &PhaseId) -> Vec<PilotId> {
        self.disqualified
            .keys()
            .filter(|(p, _)| p == phase)
            .map(|(_, pilot)| pilot.clone())
            .collect()
    }

    /// The pilots disqualified from exactly this phase whose earned place stays empty
    /// instead of passing to the next pilot.
    pub fn places_kept_empty(&self, phase: &PhaseId) -> Vec<PilotId> {
        self.disqualified
            .iter()
            .filter(|((p, _), keep)| p == phase && **keep)
            .map(|((_, pilot), _)| pilot.clone())
            .collect()
    }

    /// The positions the director set by hand in a heat.
    pub fn positions_set(&self, heat: &PhaseId) -> Vec<(PilotId, u32)> {
        self.positions
            .iter()
            .filter(|((h, _), _)| h == heat)
            .map(|((_, pilot), position)| (pilot.clone(), *position))
            .collect()
    }

    /// One protest.
    pub fn protest(&self, id: &ProtestId) -> Option<&Protest> {
        self.protests.get(id)
    }

    /// The protests that are still open, with their ids.
    pub fn open_protests(&self) -> impl Iterator<Item = (&ProtestId, &Protest)> {
        self.protests.iter().filter(|(_, p)| p.outcome.is_none())
    }

    /// Whether a phase is locked: by a lock on it, or on any phase above it.
    pub fn is_locked(&self, tree: &Tree, phase: &PhaseId) -> bool {
        tree.with_ancestors(phase)
            .iter()
            .any(|def| self.locked.contains(&def.id))
    }

    /// Whether a race's results are official: a phase that contains its heat is locked.
    /// Until then the race can be protested, ruled on, and discarded even if saved.
    pub fn race_is_locked(&self, tree: &Tree, races: &Races, race: &RaceId) -> bool {
        races
            .get(race)
            .is_some_and(|found| self.is_locked(tree, &found.setup.heat))
    }

    /// How many false starts a pilot has in the races beneath a phase. A false start
    /// counts whatever became of its race: the race is discarded, the false start stands.
    pub fn false_starts(
        &self,
        tree: &Tree,
        races: &Races,
        phase: &PhaseId,
        pilot: &PilotId,
    ) -> u32 {
        self.false_starts
            .iter()
            .filter(|(race, who)| {
                who == pilot
                    && races
                        .get(race)
                        .is_some_and(|found| tree.is_under(&found.setup.heat, phase))
            })
            .count() as u32
    }

    /// The disqualifications Grid proposes: every pilot who has reached the false start
    /// limit of a phase and is not already disqualified from it (or from a phase above).
    pub fn proposed_disqualifications(
        &self,
        tree: &Tree,
        races: &Races,
    ) -> Vec<ProposedDisqualification> {
        let pilots: BTreeSet<&PilotId> = self.false_starts.iter().map(|(_, pilot)| pilot).collect();
        let mut out = Vec::new();
        for def in tree.iter() {
            let Some(limit) = def.false_start_limit else {
                continue;
            };
            for pilot in &pilots {
                let count = self.false_starts(tree, races, &def.id, pilot);
                let already = tree.with_ancestors(&def.id).iter().any(|above| {
                    self.disqualified
                        .contains_key(&(above.id.clone(), (*pilot).clone()))
                });
                if count >= limit && !already {
                    out.push(ProposedDisqualification {
                        phase: def.id.clone(),
                        pilot: (*pilot).clone(),
                        false_starts: count,
                        limit,
                    });
                }
            }
        }
        out
    }

    /// Check that a phase can be locked: it exists, is not locked already, and every race
    /// beneath it is decided and free of open protests.
    pub fn check_lock(&self, tree: &Tree, races: &Races, phase: &PhaseId) -> Result<(), LockError> {
        if tree.get(phase).is_none() {
            return Err(LockError::UnknownPhase(phase.clone()));
        }
        if self.locked.contains(phase) {
            return Err(LockError::AlreadyLocked(phase.clone()));
        }
        for race in races.iter() {
            if !tree.is_under(&race.setup.heat, phase) {
                continue;
            }
            if race.state.is_undecided() {
                return Err(LockError::UndecidedRace(race.setup.race.clone()));
            }
            // A protest on a discarded race has nothing left to change.
            if race.state == RaceState::Discarded {
                continue;
            }
            if let Some((id, _)) = self
                .open_protests()
                .find(|(_, p)| p.race == race.setup.race)
            {
                return Err(LockError::OpenProtest {
                    protest: id.clone(),
                    race: race.setup.race.clone(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heat::RaceEntry;
    use crate::laps::{LapFilters, RaceLaps};
    use crate::phase::{Inputs, SeatWho};
    use crate::race::RaceCommand;
    use gridfpv_events::{
        AdapterId, ClassId, CombineRule, CompletionRule, EndCondition, FieldSlice, Format,
        GateIndex, HeatSeat, LineupEntry, Measure, Pass, PhaseDef, PhaseKind, RaceRules, RaceSetup,
        RankingRule, SeatRef, SeatSource, SourceTime, StartOwner,
    };

    fn pid(id: &str) -> PhaseId {
        PhaseId(id.into())
    }
    fn pilot(name: &str) -> PilotId {
        PilotId(name.into())
    }
    fn rid(id: &str) -> RaceId {
        RaceId(id.into())
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

    fn group(id: &str, parent: Option<&str>, limit: Option<u32>) -> PhaseDef {
        PhaseDef {
            id: pid(id),
            label: id.into(),
            parent: parent.map(pid),
            kind: PhaseKind::Group {
                field: vec![FieldSlice::Imported(
                    ["ana", "ben", "cat", "dee"]
                        .iter()
                        .map(|n| pilot(n))
                        .collect(),
                )],
                combine: CombineRule::Stack,
            },
            format: Some(one_race()),
            false_start_limit: limit,
            display_hint: None,
        }
    }

    fn heat(id: &str, parent: &str, sources: Vec<SeatSource>) -> PhaseDef {
        PhaseDef {
            id: pid(id),
            label: id.into(),
            parent: Some(pid(parent)),
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

    fn named(name: &str) -> SeatSource {
        SeatSource::Pilot(pilot(name))
    }

    /// Qualifying (no limit) beside Mains (limit 2) holding a D-Main and a B-Main heat, the
    /// B-Main taking the D-Main's winner.
    fn event() -> Tree {
        let mut tree = Tree::new();
        tree.put(group("qualifying", None, None));
        tree.put(heat(
            "q-heat",
            "qualifying",
            vec![named("ana"), named("ben")],
        ));
        tree.put(group("mains", None, Some(2)));
        tree.put(heat(
            "b-main",
            "mains",
            vec![
                named("cat"),
                named("dee"),
                SeatSource::FromPhase {
                    phase: pid("d-main"),
                    position: 1,
                },
            ],
        ));
        tree.put(heat("d-main", "mains", vec![named("ana"), named("ben")]));
        tree
    }

    /// The whole event state a Director would hold, with helpers that log facts.
    #[derive(Default)]
    struct Log {
        events: Vec<Event>,
    }

    impl Log {
        fn push(&mut self, event: Event) {
            self.events.push(event);
        }
        fn races(&self) -> Races {
            Races::fold(self.events.iter().map(|e| (None, e)))
        }
        fn rulings(&self) -> Rulings {
            Rulings::fold(self.events.iter())
        }
        fn command(&mut self, command: RaceCommand) {
            for fact in self.races().check(command).unwrap() {
                self.push(fact);
            }
        }
        /// Start a race of `heat` with these pilots; it is left running.
        fn start(&mut self, race: &str, heat: &str, pilots: &[&str]) {
            let setup = RaceSetup {
                race: rid(race),
                heat: pid(heat),
                lineup: pilots
                    .iter()
                    .enumerate()
                    .map(|(i, name)| LineupEntry {
                        adapter: AdapterId("oft".into()),
                        seat: SeatRef(format!("seat-{i}")),
                        pilot: Some(pilot(name)),
                    })
                    .collect(),
                owner: StartOwner::Grid,
            };
            self.command(RaceCommand::Start { setup });
        }
        /// Each pilot crosses twice, in the order given, one second apart.
        fn cross(&mut self, race: &str, order: &[&str]) {
            for lap in 0..2 {
                for (i, name) in order.iter().enumerate() {
                    self.push(Event::Pass(Pass {
                        adapter: AdapterId("oft".into()),
                        seat: SeatRef((*name).into()),
                        at: SourceTime::from_micros(lap * 10_000_000 + i as i64 * 1_000_000),
                        sequence: None,
                        gate: GateIndex::LAP,
                        signal: None,
                        race: Some(rid(race)),
                    }));
                }
            }
        }
        fn stop_and_save(&mut self, race: &str) {
            self.command(RaceCommand::Stop { race: rid(race) });
            self.command(RaceCommand::Save {
                race: rid(race),
                needs_marshal: false,
                note: None,
            });
        }
        /// A false start: the race is discarded, and the pilot is named.
        fn false_start(&mut self, race: &str, who: &str) {
            self.command(RaceCommand::Discard {
                race: rid(race),
                reason: Some(gridfpv_events::DiscardReason::FalseStart),
                note: None,
            });
            self.push(Event::FalseStart {
                race: rid(race),
                pilot: pilot(who),
            });
        }
    }

    /// The phase inputs, read from a log the way the Director will.
    struct FromLog<'a>(&'a Log);

    impl Inputs for FromLog<'_> {
        fn roster(&self, _: &ClassId) -> Vec<PilotId> {
            Vec::new()
        }
        fn saved_races(&self, heat: &PhaseId) -> Vec<Vec<RaceEntry>> {
            let laps = RaceLaps::fold(self.0.events.iter().enumerate().map(|(i, e)| (i as u64, e)));
            self.0
                .races()
                .of_heat(heat)
                .filter(|race| race.counts())
                .map(|race| {
                    race.setup
                        .lineup
                        .iter()
                        .filter_map(|entry| {
                            let pilot = entry.pilot.clone()?;
                            // In these tests a pilot's seat handle is their name.
                            let laps = laps.seat(
                                &race.setup.race,
                                &entry.adapter,
                                &SeatRef(pilot.0.clone()),
                                LapFilters::default(),
                            );
                            Some(RaceEntry { pilot, laps })
                        })
                        .collect()
                })
                .collect()
        }
        fn director_done(&self, heat: &PhaseId) -> bool {
            self.0.rulings().director_done(heat)
        }
        fn disqualified(&self, phase: &PhaseId) -> Vec<PilotId> {
            self.0.rulings().disqualified_from(phase)
        }
        fn places_kept_empty(&self, phase: &PhaseId) -> Vec<PilotId> {
            self.0.rulings().places_kept_empty(phase)
        }
        fn positions_set(&self, heat: &PhaseId) -> Vec<(PilotId, u32)> {
            self.0.rulings().positions_set(heat)
        }
    }

    #[test]
    fn false_starts_add_up_across_every_race_beneath_the_phase_that_holds_the_limit() {
        let tree = event();
        let mut log = Log::default();
        // Ana false starts in the D-Main, wins the re-run, bumps up, and false starts again
        // in the B-Main.
        log.start("d1", "d-main", &["ana", "ben"]);
        log.false_start("d1", "ana");
        assert!(
            log.rulings()
                .proposed_disqualifications(&tree, &log.races())
                .is_empty()
        );

        log.start("d2", "d-main", &["ana", "ben"]);
        log.cross("d2", &["ana", "ben"]);
        log.stop_and_save("d2");
        log.start("b1", "b-main", &["cat", "dee", "ana"]);
        log.false_start("b1", "ana");

        let rulings = log.rulings();
        let races = log.races();
        assert_eq!(
            rulings.false_starts(&tree, &races, &pid("mains"), &pilot("ana")),
            2
        );
        assert_eq!(
            rulings.false_starts(&tree, &races, &pid("d-main"), &pilot("ana")),
            1
        );
        assert_eq!(
            rulings.false_starts(&tree, &races, &pid("qualifying"), &pilot("ana")),
            0
        );
        assert_eq!(
            rulings.proposed_disqualifications(&tree, &races),
            vec![ProposedDisqualification {
                phase: pid("mains"),
                pilot: pilot("ana"),
                false_starts: 2,
                limit: 2,
            }]
        );
    }

    #[test]
    fn qualifying_has_no_limit_so_it_never_proposes_anything() {
        let tree = event();
        let mut log = Log::default();
        for race in ["q1", "q2", "q3"] {
            log.start(race, "q-heat", &["ana", "ben"]);
            log.false_start(race, "ana");
        }
        let rulings = log.rulings();
        assert_eq!(
            rulings.false_starts(&tree, &log.races(), &pid("qualifying"), &pilot("ana")),
            3
        );
        assert!(
            rulings
                .proposed_disqualifications(&tree, &log.races())
                .is_empty()
        );
    }

    #[test]
    fn grid_only_proposes_and_the_director_rules() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.false_start("d1", "ana");
        log.start("d2", "d-main", &["ana", "ben"]);
        log.false_start("d2", "ana");
        // Reaching the limit disqualifies nobody by itself.
        assert!(log.rulings().disqualified_from(&pid("mains")).is_empty());
        assert_eq!(
            log.rulings()
                .proposed_disqualifications(&tree, &log.races())
                .len(),
            1
        );

        log.push(Event::Disqualified {
            phase: pid("mains"),
            pilot: pilot("ana"),
            reason: Some("two false starts".into()),
            keep_place_empty: false,
        });
        assert_eq!(
            log.rulings().disqualified_from(&pid("mains")),
            vec![pilot("ana")]
        );
        assert!(
            log.rulings()
                .proposed_disqualifications(&tree, &log.races())
                .is_empty(),
            "nothing left to propose once the director has ruled"
        );
    }

    #[test]
    fn a_cleared_false_start_no_longer_counts() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.false_start("d1", "ana");
        log.push(Event::FalseStartCleared {
            race: rid("d1"),
            pilot: pilot("ana"),
        });
        assert_eq!(
            log.rulings()
                .false_starts(&tree, &log.races(), &pid("mains"), &pilot("ana")),
            0
        );
    }

    #[test]
    fn a_disqualified_pilot_loses_their_place_in_later_lineups_and_ranks_below_those_who_raced() {
        let tree = event();
        let mut log = Log::default();
        // Ana wins the D-Main, so she is due to bump up into the B-Main.
        log.start("d1", "d-main", &["ana", "ben"]);
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");
        let before = tree.evaluate(&FromLog(&log));
        assert_eq!(
            before[&pid("b-main")].lineup[2].who,
            SeatWho::Pilot(pilot("ana"))
        );

        log.push(Event::Disqualified {
            phase: pid("mains"),
            pilot: pilot("ana"),
            reason: None,
            keep_place_empty: false,
        });
        let after = tree.evaluate(&FromLog(&log));
        // She now ranks last in the D-Main, so its winner — the pilot who bumps up — is Ben.
        // Her place in the B-Main, which has not raced yet, goes to him.
        assert_eq!(
            after[&pid("b-main")].lineup[2].who,
            SeatWho::Pilot(pilot("ben"))
        );
        assert_eq!(
            after[&pid("b-main")].field,
            vec![pilot("cat"), pilot("dee"), pilot("ben")]
        );
        // The D-Main was already flown: its lineup stands as it was raced.
        assert!(after[&pid("d-main")].field.contains(&pilot("ana")));

        log.start("b1", "b-main", &["cat", "dee", "ben"]);
        log.cross("b1", &["dee", "cat", "ben"]);
        log.stop_and_save("b1");
        let results = tree.evaluate(&FromLog(&log));
        let mains = &results[&pid("mains")];
        let order: Vec<&str> = mains.ranking.iter().map(|p| p.pilot.0.as_str()).collect();
        // Ana won her heat but is disqualified from the mains: below everyone who raced.
        assert_eq!(order, vec!["dee", "cat", "ben", "ana"]);
        assert!(mains.ranking[3].disqualified);
        assert!(!mains.ranking[2].disqualified);
        assert_eq!(
            mains.ranking.iter().map(|p| p.position).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        // The disqualification does not reach qualifying, a different phase.
        assert!(
            results[&pid("qualifying")]
                .ranking
                .iter()
                .all(|p| !p.disqualified)
        );
    }

    #[test]
    fn a_disqualified_pilot_named_on_a_seat_leaves_it_empty() {
        let tree = event();
        let mut log = Log::default();
        log.push(Event::Disqualified {
            phase: pid("mains"),
            pilot: pilot("cat"),
            reason: None,
            keep_place_empty: false,
        });
        let results = tree.evaluate(&FromLog(&log));
        assert_eq!(results[&pid("b-main")].lineup[0].who, SeatWho::Nobody);
        assert_eq!(results[&pid("b-main")].field, vec![pilot("dee")]);
    }

    #[test]
    fn a_disqualification_can_be_lifted() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");
        log.push(Event::Disqualified {
            phase: pid("mains"),
            pilot: pilot("ana"),
            reason: None,
            keep_place_empty: false,
        });
        log.push(Event::Reinstated {
            phase: pid("mains"),
            pilot: pilot("ana"),
        });
        let results = tree.evaluate(&FromLog(&log));
        assert_eq!(
            results[&pid("b-main")].lineup[2].who,
            SeatWho::Pilot(pilot("ana"))
        );
    }

    #[test]
    fn locking_a_phase_locks_everything_beneath_it_and_can_be_undone() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");
        assert!(
            !log.rulings()
                .race_is_locked(&tree, &log.races(), &rid("d1"))
        );

        log.rulings()
            .check_lock(&tree, &log.races(), &pid("mains"))
            .unwrap();
        log.push(Event::PhaseLocked {
            phase: pid("mains"),
        });
        let rulings = log.rulings();
        assert!(rulings.is_locked(&tree, &pid("mains")));
        assert!(rulings.is_locked(&tree, &pid("d-main")));
        assert!(!rulings.is_locked(&tree, &pid("qualifying")));
        assert!(rulings.race_is_locked(&tree, &log.races(), &rid("d1")));
        assert_eq!(
            rulings.check_lock(&tree, &log.races(), &pid("mains")),
            Err(LockError::AlreadyLocked(pid("mains")))
        );

        log.push(Event::PhaseUnlocked {
            phase: pid("mains"),
        });
        assert!(
            !log.rulings()
                .race_is_locked(&tree, &log.races(), &rid("d1"))
        );
    }

    #[test]
    fn a_phase_cannot_be_locked_over_an_open_protest_or_an_undecided_race() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        assert_eq!(
            log.rulings().check_lock(&tree, &log.races(), &pid("mains")),
            Err(LockError::UndecidedRace(rid("d1")))
        );
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");

        log.push(Event::ProtestFiled {
            protest: ProtestId("p1".into()),
            race: rid("d1"),
            by: Some(pilot("ben")),
            note: Some("ana cut the gate".into()),
        });
        assert_eq!(
            log.rulings().check_lock(&tree, &log.races(), &pid("mains")),
            Err(LockError::OpenProtest {
                protest: ProtestId("p1".into()),
                race: rid("d1"),
            })
        );
        // The protest is on a mains race: qualifying can still be locked.
        assert!(
            log.rulings()
                .check_lock(&tree, &log.races(), &pid("qualifying"))
                .is_ok()
        );

        log.push(Event::ProtestResolved {
            protest: ProtestId("p1".into()),
            outcome: ProtestOutcome::Denied,
            note: None,
        });
        assert!(
            log.rulings()
                .check_lock(&tree, &log.races(), &pid("mains"))
                .is_ok()
        );
        assert_eq!(
            log.rulings()
                .check_lock(&tree, &log.races(), &pid("nowhere")),
            Err(LockError::UnknownPhase(pid("nowhere")))
        );
    }

    #[test]
    fn a_protest_is_resolved_once() {
        let mut log = Log::default();
        let id = ProtestId("p1".into());
        log.push(Event::ProtestFiled {
            protest: id.clone(),
            race: rid("d1"),
            by: None,
            note: None,
        });
        assert_eq!(log.rulings().open_protests().count(), 1);
        for outcome in [ProtestOutcome::Upheld, ProtestOutcome::Denied] {
            log.push(Event::ProtestResolved {
                protest: id.clone(),
                outcome,
                note: None,
            });
        }
        assert_eq!(
            log.rulings().protest(&id).unwrap().outcome,
            Some(ProtestOutcome::Upheld)
        );
        assert_eq!(log.rulings().open_protests().count(), 0);
    }

    #[test]
    fn a_protest_on_a_discarded_race_does_not_block_a_lock() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.push(Event::ProtestFiled {
            protest: ProtestId("p1".into()),
            race: rid("d1"),
            by: None,
            note: None,
        });
        log.false_start("d1", "ana");
        assert!(
            log.rulings()
                .check_lock(&tree, &log.races(), &pid("mains"))
                .is_ok()
        );
    }

    #[test]
    fn the_tree_and_the_director_done_flag_fold_from_the_log() {
        let mut log = Log::default();
        for def in event().iter() {
            log.push(Event::PhaseDefined { def: def.clone() });
        }
        log.push(Event::PhaseRemoved {
            phase: pid("q-heat"),
        });
        log.push(Event::HeatDone {
            heat: pid("d-main"),
        });
        let mut tree = Tree::new();
        for event in &log.events {
            tree.apply(event);
        }
        assert!(tree.get(&pid("q-heat")).is_none());
        assert_eq!(tree.iter().count(), 4);
        assert!(log.rulings().director_done(&pid("d-main")));
        log.push(Event::HeatReopened {
            heat: pid("d-main"),
        });
        assert!(!log.rulings().director_done(&pid("d-main")));
    }

    #[test]
    fn a_disqualified_pilots_place_can_be_kept_empty_instead_of_passing_down() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");
        log.push(Event::Disqualified {
            phase: pid("mains"),
            pilot: pilot("ana"),
            reason: None,
            keep_place_empty: true,
        });
        let results = tree.evaluate(&FromLog(&log));
        // Ana had won the bump-up. Nobody takes it: Ben stays in the D-Main.
        assert_eq!(results[&pid("b-main")].lineup[2].who, SeatWho::Nobody);
        assert_eq!(
            results[&pid("b-main")].field,
            vec![pilot("cat"), pilot("dee")]
        );
        // In the D-Main's own results she still ranks last, below Ben.
        let d_main: Vec<&str> = results[&pid("d-main")]
            .ranking
            .iter()
            .map(|p| p.pilot.0.as_str())
            .collect();
        assert_eq!(d_main, vec!["ben", "ana"]);
        assert_eq!(
            results[&pid("d-main")].places,
            vec![None, Some(pilot("ben"))]
        );
    }

    #[test]
    fn a_position_set_by_hand_holds_and_feeds_the_next_heat() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");
        // The director rules that Ben finished first (Ana cut a gate).
        log.push(Event::PositionSet {
            heat: pid("d-main"),
            pilot: pilot("ben"),
            position: 1,
            note: None,
        });
        let results = tree.evaluate(&FromLog(&log));
        let d_main = &results[&pid("d-main")];
        assert_eq!(d_main.ranking[0].pilot, pilot("ben"));
        assert_eq!(d_main.ranking[0].position, 1);
        assert_eq!(d_main.ranking[1].pilot, pilot("ana"));
        assert_eq!(d_main.ranking[1].position, 2);
        // From there on: Ben takes the bump-up.
        assert_eq!(
            results[&pid("b-main")].lineup[2].who,
            SeatWho::Pilot(pilot("ben"))
        );

        log.push(Event::PositionCleared {
            heat: pid("d-main"),
            pilot: pilot("ben"),
        });
        let results = tree.evaluate(&FromLog(&log));
        assert_eq!(results[&pid("d-main")].ranking[0].pilot, pilot("ana"));
    }

    #[test]
    fn a_position_set_past_the_end_of_the_heat_puts_the_pilot_last() {
        let tree = event();
        let mut log = Log::default();
        log.start("d1", "d-main", &["ana", "ben"]);
        log.cross("d1", &["ana", "ben"]);
        log.stop_and_save("d1");
        log.push(Event::PositionSet {
            heat: pid("d-main"),
            pilot: pilot("ana"),
            position: 9,
            note: None,
        });
        let results = tree.evaluate(&FromLog(&log));
        let order: Vec<u32> = results[&pid("d-main")]
            .ranking
            .iter()
            .map(|p| p.position)
            .collect();
        assert_eq!(order, vec![1, 2]);
        assert_eq!(results[&pid("d-main")].ranking[1].pilot, pilot("ana"));
    }

    #[test]
    fn the_director_can_call_a_lone_pilots_heat_done_so_they_advance_without_flying() {
        let mut tree = Tree::new();
        tree.put(group("mains", None, None));
        tree.put(heat(
            "b-main",
            "mains",
            vec![
                named("cat"),
                SeatSource::FromPhase {
                    phase: pid("d-main"),
                    position: 1,
                },
            ],
        ));
        tree.put(heat("d-main", "mains", vec![named("ana")]));
        let mut log = Log::default();
        // The format wants one saved race, so the heat is not done by itself.
        let before = tree.evaluate(&FromLog(&log));
        assert!(!before[&pid("d-main")].complete);
        assert!(matches!(
            before[&pid("b-main")].lineup[1].who,
            SeatWho::Waiting { .. }
        ));

        log.push(Event::HeatDone {
            heat: pid("d-main"),
        });
        let after = tree.evaluate(&FromLog(&log));
        assert!(after[&pid("d-main")].complete);
        assert_eq!(
            after[&pid("b-main")].lineup[1].who,
            SeatWho::Pilot(pilot("ana"))
        );
    }
}
