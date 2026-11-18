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

use crate::laps::{Lap, LapKind, SeatLaps};

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

    /// Whether a log entry falls inside this stint: after the seat was taken and before
    /// the stint was over.
    fn spans(&self, entry: LogRef) -> bool {
        entry.0 > self.from.0 && self.until.is_none_or(|until| entry.0 < until.0)
    }

    /// Whether an interval of the seat (a lap, or a marked restart) falls inside this
    /// stint: it started and ended while the pilot had the seat.
    fn covers(&self, lap: &Lap) -> bool {
        lap.start.is_some_and(|start| self.spans(start)) && self.spans(lap.end)
    }

    /// Whether a lap belongs to this stint: started and ended after the seat was taken,
    /// and ended before the stint was over.
    pub fn holds(&self, lap: &Lap) -> bool {
        lap.is_lap() && self.covers(lap)
    }

    /// The part of a seat's race that is this stint's, as laps of its own: the pilot's
    /// first crossing on the seat is their hole shot, and what follows until the stint
    /// ended is theirs. `None` when the pilot never crossed the gate in the stint.
    fn own_laps(&self, seat: &SeatLaps) -> Option<SeatLaps> {
        let crossings = seat
            .hole_shot
            .into_iter()
            .chain(seat.intervals.iter().map(|lap| (lap.end, lap.ended_at)));
        let hole_shot = crossings
            .filter(|(entry, _)| self.spans(*entry))
            .min_by_key(|(entry, _)| entry.0)?;
        Some(SeatLaps {
            hole_shot: Some(hole_shot),
            tone_at: None,
            intervals: seat
                .intervals
                .iter()
                .filter(|lap| self.covers(lap))
                .copied()
                .collect(),
            uncounted: Vec::new(),
        })
    }

    /// The stint's laps, out of everything the seat flew in the race.
    pub fn laps<'a>(&'a self, seat: &'a SeatLaps) -> impl Iterator<Item = &'a Lap> {
        seat.laps().filter(|lap| self.holds(lap))
    }
}

/// A pilot's laps in one race from every stint they had in it, put together as if they had
/// flown one seat: what the heat is ranked on. `stints` are the pilot's stints in log order,
/// each with the laps of the seat it was on.
///
/// The time between two stints is not a lap. It is joined as a break, the same way a timer
/// restart is, so no lap and no run of laps in a row spans it.
pub fn laps_across<'a>(stints: impl IntoIterator<Item = (&'a Stint, &'a SeatLaps)>) -> SeatLaps {
    let mut all = SeatLaps::default();
    for piece in stints
        .into_iter()
        .filter_map(|(stint, seat)| stint.own_laps(seat))
    {
        let Some((entry, at)) = piece.hole_shot else {
            continue;
        };
        let last = all
            .intervals
            .last()
            .map(|lap| (lap.end, lap.ended_at))
            .or(all.hole_shot);
        match last {
            None => all.hole_shot = Some((entry, at)),
            Some((before, before_at)) => all.intervals.push(Lap {
                start: Some(before),
                end: entry,
                started_at: before_at,
                ended_at: at,
                kind: LapKind::TimerRestarted,
            }),
        }
        all.intervals.extend(piece.intervals);
    }
    all
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

    /// The pilots who took a seat in a race, in the order they first did.
    pub fn pilots(&self, race: &RaceId) -> Vec<PilotId> {
        let mut first: Vec<(u64, &PilotId)> = Vec::new();
        for stint in self
            .stints
            .iter()
            .filter(|((in_race, _, _), _)| in_race == race)
            .flat_map(|(_, stints)| stints)
        {
            match first.iter_mut().find(|(_, pilot)| *pilot == &stint.pilot) {
                Some(known) => known.0 = known.0.min(stint.from.0),
                None => first.push((stint.from.0, &stint.pilot)),
            }
        }
        first.sort_by_key(|(from, _)| *from);
        first.into_iter().map(|(_, pilot)| pilot.clone()).collect()
    }

    /// Every stint a pilot had in a race, in log order, each with the seat it was on.
    pub fn stints_of(&self, race: &RaceId, pilot: &PilotId) -> Vec<(&AdapterId, &SeatRef, &Stint)> {
        let mut theirs: Vec<(&AdapterId, &SeatRef, &Stint)> = self
            .stints
            .iter()
            .filter(|((in_race, _, _), _)| in_race == race)
            .flat_map(|((_, adapter, seat), stints)| {
                stints
                    .iter()
                    .filter(|stint| &stint.pilot == pilot)
                    .map(move |stint| (adapter, seat, stint))
            })
            .collect();
        theirs.sort_by_key(|(_, _, stint)| stint.from.0);
        theirs
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
