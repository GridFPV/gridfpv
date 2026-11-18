//! Check-in: a pilot takes an open seat for a while, part-way through a race
//! (`docs/primitives.md` §11.9).
//!
//! This is for practice run as one long heat with open seats. The timer just runs. A pilot
//! who wants to fly **takes** a seat, and the laps on that seat are theirs until someone
//! else takes it or they **drop** it. A **stint** is one such spell on a seat.
//!
//! Which laps belong to a stint is decided by log order, never by a clock: a lap is the
//! pilot's when the crossing that started it and the crossing that ended it were both
//! logged after they took the seat, and the ending one before the stint was over. So the
//! pilot's first crossing after taking the seat starts their first lap.

use std::collections::BTreeMap;

use gridfpv_events::{AdapterId, Event, GateIndex, LogRef, PilotId, RaceId, SeatRef};

use crate::laps::{Lap, SeatLaps};

/// One spell of one pilot on one seat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stint {
    pub pilot: PilotId,
    /// The log entry that put the pilot on the seat.
    pub from: LogRef,
    /// The log entry that ended the stint: a drop, or the next pilot taking the seat.
    /// `None` while the pilot still has the seat.
    pub until: Option<LogRef>,
    /// When the seat last showed life, on the server clock: when it was taken, or its
    /// latest crossing since. `None` when the log does not say when.
    pub active_at: Option<i64>,
}

impl Stint {
    /// Whether the pilot still has the seat.
    pub fn is_open(&self) -> bool {
        self.until.is_none()
    }

    /// Whether a lap belongs to this stint: started and ended after the seat was taken,
    /// and ended before the stint was over.
    pub fn holds(&self, lap: &Lap) -> bool {
        lap.is_lap()
            && lap.start.is_some_and(|start| start.0 > self.from.0)
            && lap.end.0 > self.from.0
            && self.until.is_none_or(|until| lap.end.0 < until.0)
    }

    /// The stint's laps, out of everything the seat flew in the race.
    pub fn laps<'a>(&'a self, seat: &'a SeatLaps) -> impl Iterator<Item = &'a Lap> {
        seat.laps().filter(|lap| self.holds(lap))
    }
}

/// Every stint, folded from the log.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckIns {
    stints: BTreeMap<(RaceId, AdapterId, SeatRef), Vec<Stint>>,
}

impl CheckIns {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the log entry stored at `offset`, recorded at `recorded_at` on the server
    /// clock. Other events are ignored; the fold is total.
    pub fn apply(&mut self, offset: u64, recorded_at: Option<i64>, event: &Event) {
        match event {
            Event::SeatTaken {
                race,
                adapter,
                seat,
                pilot,
            } => {
                // A pilot is on one seat at a time, and a seat holds one pilot at a time.
                for ((in_race, _, _), stints) in &mut self.stints {
                    if in_race == race {
                        close(stints, offset, |stint| &stint.pilot == pilot);
                    }
                }
                let stints = self
                    .stints
                    .entry((race.clone(), adapter.clone(), seat.clone()))
                    .or_default();
                close(stints, offset, |_| true);
                stints.push(Stint {
                    pilot: pilot.clone(),
                    from: LogRef(offset),
                    until: None,
                    active_at: recorded_at,
                });
            }
            Event::SeatDropped {
                race,
                adapter,
                seat,
            } => {
                if let Some(stints) =
                    self.stints
                        .get_mut(&(race.clone(), adapter.clone(), seat.clone()))
                {
                    close(stints, offset, |_| true);
                }
            }
            Event::Pass(pass) if pass.gate == GateIndex::LAP => {
                let Some(race) = &pass.race else { return };
                let open = self
                    .stints
                    .get_mut(&(race.clone(), pass.adapter.clone(), pass.seat.clone()))
                    .and_then(|stints| stints.last_mut())
                    .filter(|stint| stint.is_open());
                if let (Some(stint), Some(at)) = (open, recorded_at) {
                    stint.active_at = Some(at);
                }
            }
            _ => {}
        }
    }

    /// A seat's stints in a race, oldest first.
    pub fn stints(&self, race: &RaceId, adapter: &AdapterId, seat: &SeatRef) -> &[Stint] {
        self.stints
            .get(&(race.clone(), adapter.clone(), seat.clone()))
            .map_or(&[], Vec::as_slice)
    }

    /// Who has a seat now, if anyone has taken it.
    pub fn holder(&self, race: &RaceId, adapter: &AdapterId, seat: &SeatRef) -> Option<&PilotId> {
        self.stints(race, adapter, seat)
            .last()
            .filter(|stint| stint.is_open())
            .map(|stint| &stint.pilot)
    }

    /// The seat a pilot has now in a race, if they have one.
    pub fn seat_of(&self, race: &RaceId, pilot: &PilotId) -> Option<(&AdapterId, &SeatRef)> {
        self.stints
            .iter()
            .filter(|((in_race, _, _), _)| in_race == race)
            .find(|(_, stints)| {
                stints
                    .last()
                    .is_some_and(|stint| stint.is_open() && &stint.pilot == pilot)
            })
            .map(|((_, adapter, seat), _)| (adapter, seat))
    }

    /// Every seat of a race that someone has now, with its open stint.
    pub fn held(&self, race: &RaceId) -> impl Iterator<Item = (&AdapterId, &SeatRef, &Stint)> {
        self.stints
            .iter()
            .filter(move |((in_race, _, _), _)| in_race == race)
            .filter_map(|((_, adapter, seat), stints)| {
                let stint = stints.last().filter(|stint| stint.is_open())?;
                Some((adapter, seat, stint))
            })
    }
}

/// End the open stint, if it matches.
fn close(stints: &mut [Stint], offset: u64, matches: impl Fn(&Stint) -> bool) {
    if let Some(stint) = stints.last_mut().filter(|stint| stint.is_open())
        && matches(stint)
    {
        stint.until = Some(LogRef(offset));
    }
}
