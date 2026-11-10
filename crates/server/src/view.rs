//! The event snapshot a console draws from (`GET /events/{id}/state`).
//!
//! One read gives everything about an event's competition: its rosters, every phase with
//! its lineup and ranking, every race with its laps, and what is waiting on the director.
//! It is worked out from the engine's fold of the log each time it is asked for, so it is
//! never stale and there is no second copy of the truth to keep in step.
//!
//! Ids here are wire handles. A console resolves them to names before showing them.

use gridfpv_engine::heat::{RaceEntry, race_finish};
use gridfpv_engine::laps::{LapKind, SeatLaps, Uncounted};
use gridfpv_engine::phase::{PhaseResult, SeatWho};
use gridfpv_engine::race::{Race, RaceState};
use gridfpv_engine::state::EventState;
use gridfpv_events::{
    AdapterId, ClassId, DiscardReason, EndCondition, Format, LogRef, PhaseDef, PhaseId, PilotId,
    ProtestId, ProtestOutcome, RaceId, RemovalReason, SeatRef, SourceTime, StartOwner,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Everything about one event's competition, at one log cursor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventView {
    /// How many entries the event's log held when this was worked out. The change feed
    /// sends this number whenever it moves.
    #[ts(type = "number")]
    pub cursor: u64,
    /// The Director's clock when this was worked out, in epoch microseconds.
    #[ts(type = "number")]
    pub now: i64,
    /// The classes in the event and their rosters.
    pub rosters: Vec<RosterView>,
    /// Every phase, in the order they were added (a parent before its children).
    pub phases: Vec<PhaseView>,
    /// Every race, oldest first.
    pub races: Vec<RaceView>,
    /// The race that is staging, running, or stopped and waiting to be saved or discarded.
    /// While there is one, no other race can start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub current_race: Option<RaceId>,
    /// The run queue: the heats that can be raced now, in the suggested order. A
    /// suggestion only: any of them can be started.
    pub queue: Vec<PhaseId>,
    /// Every protest, open or resolved.
    pub protests: Vec<ProtestView>,
    /// Pilots who have reached a phase's false start limit. The director decides.
    pub proposed_disqualifications: Vec<ProposedDisqualificationView>,
}

/// A class in the event and its roster, in roster order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RosterView {
    pub class: ClassId,
    pub pilots: Vec<PilotId>,
    /// The phases that count toward the class's standings, the deciding one first.
    pub counted_phases: Vec<PhaseId>,
    /// Everyone on the roster, 1 to n.
    pub standings: Vec<ClassPlaceView>,
}

/// One pilot's place in a class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClassPlaceView {
    pub pilot: PilotId,
    /// 1 for the top.
    pub position: u32,
    /// The phase that placed them. Absent for a pilot no counted phase has placed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub placed_by: Option<PhaseId>,
}

/// One phase and what has been worked out for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PhaseView {
    /// The phase as it was defined.
    pub def: PhaseDef,
    /// The format in force: the phase's own, or the nearest one set above it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub format: Option<Format>,
    /// Whether the phase is locked, by a lock on it or on a phase above it.
    pub locked: bool,
    /// Whether the lock sits on this phase itself (so this is the one to unlock).
    pub locked_here: bool,
    /// Whether the phase still waits on another phase to fill its field or seats.
    pub waiting: bool,
    pub complete: bool,
    /// Whether the director called this heat done.
    pub director_done: bool,
    /// The pilots in seed order.
    pub field: Vec<PilotId>,
    /// A heat's seats. Empty for a phase that holds phases.
    pub lineup: Vec<SeatView>,
    /// The field in ranked order.
    pub ranking: Vec<PlacedView>,
}

/// One seat of a heat's lineup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SeatView {
    pub adapter: AdapterId,
    pub seat: SeatRef,
    pub who: SeatWhoView,
}

/// Who is on a seat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum SeatWhoView {
    Pilot(PilotId),
    /// Nobody named: whoever flies is timed (practice).
    Open,
    /// Nobody: the place this seat points at was not filled.
    Nobody,
    /// Not known yet: position `position` of `phase`, once that phase finishes.
    Waiting {
        phase: PhaseId,
        position: u32,
    },
}

/// One pilot's place in a phase, with the numbers behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PlacedView {
    pub pilot: PilotId,
    /// 1 for the top.
    pub position: u32,
    pub disqualified: bool,
    /// Whether the director set this position by hand.
    pub position_set: bool,
    /// Whether the pilot made a crossing in a saved race here.
    pub flew: bool,
    pub wins: u32,
    pub points: u32,
    /// The most laps in one race.
    pub laps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub best_lap_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub best_finish: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_finish: Option<u32>,
}

/// Where a race is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum RaceStateView {
    Staging,
    Running,
    Stopped,
    Saved,
    Discarded,
}

/// One race and its laps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RaceView {
    pub race: RaceId,
    pub heat: PhaseId,
    pub state: RaceStateView,
    pub owner: StartOwner,
    /// How the race ends, from the format in force.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub end: Option<EndCondition>,
    /// The start tone, in epoch microseconds on the Director's clock. Count down to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub tone_at: Option<i64>,
    /// When the race started, on the Director's clock. Count the race clock from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub stopped_at: Option<i64>,
    /// On the marshal's to-do list.
    pub needs_marshal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub discard_reason: Option<DiscardReason>,
    /// Whether a phase that contains the race is locked: its result is official.
    pub locked: bool,
    /// Each seat of the lineup, in lineup order.
    pub seats: Vec<RaceSeatView>,
}

/// One seat's part in a race.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RaceSeatView {
    pub adapter: AdapterId,
    pub seat: SeatRef,
    /// The pilot on the seat. Absent for an open seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pilot: Option<PilotId>,
    /// Whether a false start is recorded against the pilot in this race.
    pub false_start: bool,
    /// The seat's place in the race so far, 1 first. Absent for an open seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub finish: Option<u32>,
    /// The crossing that started lap 1, when lap 1 starts at the hole shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub hole_shot: Option<CrossingView>,
    /// Laps and marked restarts, in order.
    pub laps: Vec<LapView>,
    /// Crossings that do not count, kept so they can be restored.
    pub uncounted: Vec<UncountedView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub best_lap_micros: Option<i64>,
    /// From the start of lap 1 to the last counted crossing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub total_micros: Option<i64>,
}

/// One crossing: the log entry that made it, and when, on the timer's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CrossingView {
    pub crossing: LogRef,
    pub at: SourceTime,
}

/// One timed interval between two counted crossings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LapView {
    /// The crossing that ended it. Rulings about this lap name this crossing.
    pub crossing: LogRef,
    pub started_at: SourceTime,
    pub ended_at: SourceTime,
    #[ts(type = "number")]
    pub micros: i64,
    /// A marked restart: it has no lap time and does not count.
    pub restart: bool,
}

/// Why a crossing does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum UncountedWhy {
    /// A person removed it.
    Removed,
    /// The minimum lap time filtered it out.
    UnderMinLap,
    /// It came before the start tone.
    BeforeStart,
}

/// A crossing that does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UncountedView {
    pub crossing: LogRef,
    pub at: SourceTime,
    pub why: UncountedWhy,
    /// The label picked when it was removed, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<RemovalReason>,
}

/// One protest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ProtestView {
    pub protest: ProtestId,
    pub race: RaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub by: Option<PilotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    /// Absent while the protest is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome: Option<ProtestOutcome>,
}

/// A pilot who has reached a phase's false start limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ProposedDisqualificationView {
    pub phase: PhaseId,
    pub pilot: PilotId,
    pub false_starts: u32,
    pub limit: u32,
}

impl EventView {
    /// Work the snapshot out from an event's state at `now` (epoch microseconds).
    pub fn build(state: &EventState, now: i64) -> Self {
        let results = state.results();
        let phases = state
            .tree
            .iter()
            .map(|def| {
                let empty = PhaseResult::default();
                let result = results.get(&def.id).unwrap_or(&empty);
                let set = state.rulings.positions_set(&def.id);
                PhaseView {
                    def: def.clone(),
                    format: state.tree.format_for(&def.id).cloned(),
                    locked: state.rulings.is_locked(&state.tree, &def.id),
                    locked_here: state.rulings.is_locked_here(&def.id),
                    waiting: result.waiting,
                    complete: result.complete,
                    director_done: state.rulings.director_done(&def.id),
                    field: result.field.clone(),
                    lineup: result
                        .lineup
                        .iter()
                        .map(|seat| SeatView {
                            adapter: seat.adapter.clone(),
                            seat: seat.seat.clone(),
                            who: match &seat.who {
                                SeatWho::Pilot(pilot) => SeatWhoView::Pilot(pilot.clone()),
                                SeatWho::Open => SeatWhoView::Open,
                                SeatWho::Nobody => SeatWhoView::Nobody,
                                SeatWho::Waiting { phase, position } => SeatWhoView::Waiting {
                                    phase: phase.clone(),
                                    position: *position,
                                },
                            },
                        })
                        .collect(),
                    ranking: result
                        .ranking
                        .iter()
                        .map(|placed| PlacedView {
                            pilot: placed.pilot.clone(),
                            position: placed.position,
                            disqualified: placed.disqualified,
                            position_set: set.iter().any(|(pilot, _)| pilot == &placed.pilot),
                            flew: placed.result.flew,
                            wins: placed.result.wins,
                            points: placed.result.points,
                            laps: placed.result.laps,
                            best_lap_micros: placed.result.best_lap_micros,
                            best_finish: placed.result.best_finish,
                            last_finish: placed.result.last_finish,
                        })
                        .collect(),
                }
            })
            .collect();

        Self {
            cursor: state.applied,
            now,
            rosters: state
                .rosters
                .iter()
                .map(|(class, pilots)| RosterView {
                    class: class.clone(),
                    pilots: pilots.clone(),
                    counted_phases: state.class_phases.get(class).cloned().unwrap_or_default(),
                    standings: state
                        .class_standings(class, &results)
                        .into_iter()
                        .map(|place| ClassPlaceView {
                            pilot: place.pilot,
                            position: place.position,
                            placed_by: place.placed_by,
                        })
                        .collect(),
                })
                .collect(),
            phases,
            races: state
                .races
                .iter()
                .map(|race| race_view(state, race))
                .collect(),
            current_race: state.races.undecided().map(|race| race.setup.race.clone()),
            queue: state.queue(&results),
            protests: state
                .rulings
                .protests()
                .map(|(id, protest)| ProtestView {
                    protest: id.clone(),
                    race: protest.race.clone(),
                    by: protest.by.clone(),
                    note: protest.note.clone(),
                    outcome: protest.outcome,
                })
                .collect(),
            proposed_disqualifications: state
                .rulings
                .proposed_disqualifications(&state.tree, &state.races)
                .into_iter()
                .map(|proposed| ProposedDisqualificationView {
                    phase: proposed.phase,
                    pilot: proposed.pilot,
                    false_starts: proposed.false_starts,
                    limit: proposed.limit,
                })
                .collect(),
        }
    }
}

fn race_view(state: &EventState, race: &Race) -> RaceView {
    let id = &race.setup.race;
    let end = state
        .format_for(&race.setup.heat)
        .map(|format| format.race.end);
    let laps: Vec<SeatLaps> = race
        .setup
        .lineup
        .iter()
        .map(|entry| {
            let filters = state.filters_for(race, &entry.adapter);
            state.laps.seat(id, &entry.adapter, &entry.seat, filters)
        })
        .collect();
    // The order so far, among named pilots, with lineup order as the last tie-break.
    let entries: Vec<RaceEntry> = race
        .setup
        .lineup
        .iter()
        .zip(&laps)
        .filter_map(|(entry, laps)| {
            Some(RaceEntry {
                pilot: entry.pilot.clone()?,
                laps: laps.clone(),
            })
        })
        .collect();
    let seed_of = |pilot: &PilotId| {
        race.setup
            .lineup
            .iter()
            .position(|entry| entry.pilot.as_ref() == Some(pilot))
            .unwrap_or(usize::MAX)
    };
    let finish = race_finish(&entries, &end.unwrap_or(EndCondition::Open), seed_of);

    RaceView {
        race: id.clone(),
        heat: race.setup.heat.clone(),
        state: match race.state {
            RaceState::Staging => RaceStateView::Staging,
            RaceState::Running => RaceStateView::Running,
            RaceState::Stopped => RaceStateView::Stopped,
            RaceState::Saved => RaceStateView::Saved,
            RaceState::Discarded => RaceStateView::Discarded,
        },
        owner: race.setup.owner,
        end,
        tone_at: race.tone_at,
        started_at: race.started_at,
        stopped_at: race.stopped_at,
        needs_marshal: race.needs_marshal,
        note: race.note.clone(),
        discard_reason: race.discard_reason,
        locked: state.rulings.race_is_locked(&state.tree, &state.races, id),
        seats: race
            .setup
            .lineup
            .iter()
            .zip(&laps)
            .map(|(entry, laps)| RaceSeatView {
                adapter: entry.adapter.clone(),
                seat: entry.seat.clone(),
                pilot: entry.pilot.clone(),
                false_start: entry
                    .pilot
                    .as_ref()
                    .is_some_and(|pilot| state.rulings.has_false_start(id, pilot)),
                finish: entry.pilot.as_ref().and_then(|pilot| {
                    finish
                        .iter()
                        .find(|line| &line.pilot == pilot)
                        .map(|line| line.position)
                }),
                hole_shot: laps
                    .hole_shot
                    .map(|(crossing, at)| CrossingView { crossing, at }),
                laps: laps
                    .intervals
                    .iter()
                    .map(|lap| LapView {
                        crossing: lap.end,
                        started_at: lap.started_at,
                        ended_at: lap.ended_at,
                        micros: lap.micros(),
                        restart: lap.kind == LapKind::Restart,
                    })
                    .collect(),
                uncounted: laps
                    .uncounted
                    .iter()
                    .map(|uncounted| {
                        let (why, reason) = match uncounted.why {
                            Uncounted::Removed(reason) => (UncountedWhy::Removed, reason),
                            Uncounted::UnderMinLap => (UncountedWhy::UnderMinLap, None),
                            Uncounted::BeforeStart => (UncountedWhy::BeforeStart, None),
                        };
                        UncountedView {
                            crossing: uncounted.crossing,
                            at: uncounted.at,
                            why,
                            reason,
                        }
                    })
                    .collect(),
                best_lap_micros: laps.best_lap_micros(),
                total_micros: laps.total_micros(),
            })
            .collect(),
    }
}
