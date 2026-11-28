//! The open event, as a visitor may see it (docs/primitives.md §11.2).
//!
//! A spectator sees the race that is on, the results and standings, and the heats coming
//! up with who is in them. These routes give exactly that for the **one event that is
//! open**, and nothing else the Director knows.
//!
//! - `GET /public/event` — which event is open.
//! - `GET /public/event/state` — its competition snapshot, with the director's own things
//!   taken out.
//! - `GET /public/event/changes` — tells a page when to read the snapshot again.
//! - `GET /public/event/pilots`, `/classes`, `/timers` — the names the snapshot refers to,
//!   and only those.
//! - `GET /public/event/channels` — which video channel each seat is on, so a pilot knows
//!   what to fly on.
//!
//! The shapes are the console's own (`EventView`, `Pilot`, `Class`, `TimerView`), so one
//! page and one set of helpers draws both. What differs is what is left in them.
//!
//! # How a field is kept or left out
//!
//! Every `*_for_visitors` function takes its struct apart **naming every field**, with no
//! `..`. So a field added to one of those structs later does not compile until someone
//! decides, here, whether a visitor may see it. Nothing new reaches a visitor by default.
//!
//! Left out: a race's video (paths on the Grid computer), the director's notes, the
//! marshal's to-do flag, crossings that do not count, protests and proposed
//! disqualifications; a pilot's real name, team, country, outside ids and video gear; a
//! class's notes and outside reference; a timer's address, its own id and its faults.

use std::collections::BTreeSet;
use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event as SseEvent, Sse};
use futures_util::stream::Stream;
use gridfpv_events::{AdapterId, ClassId, PilotId};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::classes::Class;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventId, EventRegistry};
use crate::pilots::Pilot;
use crate::timers::{SeatChannel, Timer, TimerStatus, TimerView};
use crate::view::{EventView, PhaseView, RaceSeatView, RaceView, SeatWhoView};

/// `GET /public/event`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PublicEvent {
    /// The event that is open. Absent when none is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub event: Option<PublicEventName>,
}

/// An event, as far as a visitor needs to know it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PublicEventName {
    /// Changes when the director opens another event. A page that sees it change starts
    /// again from the top.
    pub id: EventId,
    pub name: String,
}

/// One seat's part in a race, for a visitor.
fn seat_for_visitors(seat: RaceSeatView) -> RaceSeatView {
    let RaceSeatView {
        adapter,
        seat,
        pilot,
        start_at,
        false_start,
        // A crash is seen by everyone at the track, so a visitor may see the mark too.
        out,
        // Who is on a seat is called out at the track, so a visitor may see it too.
        holder,
        stints,
        finish,
        hole_shot,
        laps,
        uncounted: _,
        best_lap_micros,
        total_micros,
    } = seat;
    RaceSeatView {
        adapter,
        seat,
        pilot,
        start_at,
        false_start,
        out,
        holder,
        stints,
        finish,
        hole_shot,
        laps,
        // What the marshal set aside is the marshal's business.
        uncounted: Vec::new(),
        best_lap_micros,
        total_micros,
    }
}

/// One race, for a visitor.
fn race_for_visitors(race: RaceView) -> RaceView {
    let RaceView {
        race,
        heat,
        video: _,
        state,
        owner,
        end,
        tone_at,
        started_at,
        stopped_at,
        needs_marshal: _,
        note: _,
        discard_reason,
        locked,
        seats,
    } = race;
    RaceView {
        race,
        heat,
        // Paths on the Grid computer.
        video: None,
        state,
        owner,
        end,
        tone_at,
        started_at,
        stopped_at,
        needs_marshal: false,
        // The director's own note.
        note: None,
        discard_reason,
        locked,
        seats: seats.into_iter().map(seat_for_visitors).collect(),
    }
}

/// The snapshot, for a visitor.
pub fn for_visitors(view: EventView) -> EventView {
    let EventView {
        cursor,
        now,
        lap_rules,
        rosters,
        phases,
        races,
        current_race,
        on_timer,
        queue,
        protests: _,
        proposed_disqualifications: _,
    } = view;
    EventView {
        cursor,
        now,
        lap_rules,
        rosters,
        phases,
        races: races.into_iter().map(race_for_visitors).collect(),
        current_race,
        on_timer,
        queue,
        // Who protested what, and who is close to a disqualification, is not public.
        protests: Vec::new(),
        proposed_disqualifications: Vec::new(),
    }
}

/// A pilot, for a visitor: the callsign and the colour it is drawn in.
pub fn pilot_for_visitors(pilot: Pilot) -> Pilot {
    let Pilot {
        id,
        callsign,
        name: _,
        phonetic: _,
        team: _,
        color,
        country: _,
        vtx_types: _,
        multigp_id: _,
        velocidrone_id: _,
    } = pilot;
    Pilot {
        id,
        callsign,
        name: None,
        phonetic: None,
        team: None,
        color,
        country: None,
        vtx_types: Vec::new(),
        multigp_id: None,
        velocidrone_id: None,
    }
}

/// A class, for a visitor: its name.
pub fn class_for_visitors(class: Class) -> Class {
    let Class {
        id,
        name,
        org,
        reference: _,
        description: _,
        builtin,
        hidden: _,
    } = class;
    Class {
        id,
        name,
        org,
        reference: None,
        description: None,
        builtin,
        hidden: false,
    }
}

/// A timer, for a visitor: its name and its seats, which is what names a seat.
pub fn timer_for_visitors(view: TimerView) -> TimerView {
    let TimerView { timer, status } = view;
    let Timer {
        id,
        name,
        kind,
        address: _,
        own_id: _,
    } = timer;
    let TimerStatus {
        connected,
        problem: _,
        seats,
        clock_error_micros: _,
        seat_thresholds: _,
        keeps_settings: _,
        pass_window: _,
        pass_window_ms: _,
        pass_window_problem: _,
        level_unit: _,
        // Which programs are on the director's network is not a visitor's business.
        control: _,
        control_name: _,
        config_rev: _,
        timer_problems: _,
        // Where the timer is on the director's network.
        manage_url: _,
    } = status;
    TimerView {
        timer: Timer {
            id,
            name,
            kind,
            // Where the timer is on the director's network.
            address: String::new(),
            own_id: None,
        },
        status: TimerStatus {
            connected,
            problem: None,
            seats,
            clock_error_micros: None,
            seat_thresholds: false,
            keeps_settings: false,
            pass_window: false,
            pass_window_ms: None,
            pass_window_problem: None,
            level_unit: None,
            control: None,
            control_name: None,
            config_rev: None,
            timer_problems: Vec::new(),
            manage_url: None,
        },
    }
}

/// Every pilot, class and timer a snapshot refers to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Mentioned {
    pub pilots: BTreeSet<PilotId>,
    pub classes: BTreeSet<ClassId>,
    pub timers: BTreeSet<AdapterId>,
}

/// Collect what a snapshot refers to, so only those names are given out.
pub fn mentioned(view: &EventView) -> Mentioned {
    let mut found = Mentioned::default();
    for roster in &view.rosters {
        found.classes.insert(roster.class.clone());
        found.pilots.extend(roster.pilots.iter().cloned());
        found
            .pilots
            .extend(roster.standings.iter().map(|place| place.pilot.clone()));
    }
    for PhaseView {
        field,
        lineup,
        ranking,
        ..
    } in &view.phases
    {
        found.pilots.extend(field.iter().cloned());
        found
            .pilots
            .extend(ranking.iter().map(|placed| placed.pilot.clone()));
        for seat in lineup {
            found.timers.insert(seat.adapter.clone());
            if let SeatWhoView::Pilot(pilot) = &seat.who {
                found.pilots.insert(pilot.clone());
            }
        }
    }
    for race in &view.races {
        for seat in &race.seats {
            found.timers.insert(seat.adapter.clone());
            found.pilots.extend(seat.pilot.iter().cloned());
        }
    }
    found
}

fn no_event() -> ProtocolError {
    ProtocolError::new(ErrorCode::UnknownScope, "No event is open.")
}

/// The open event's id, or the "no event is open" answer.
fn open_event(registry: &EventRegistry) -> Result<EventId, ProtocolError> {
    registry.active().map(|event| event.id).ok_or_else(no_event)
}

/// The open event's snapshot for a visitor.
fn open_view(registry: &EventRegistry) -> Result<EventView, ProtocolError> {
    let id = open_event(registry)?;
    crate::control::with_event(registry, &id, |session| {
        Ok(for_visitors(EventView::build(session.state(), session.now)))
    })
}

/// `GET /public/event` — which event is open.
pub async fn get_event(State(registry): State<EventRegistry>) -> Json<PublicEvent> {
    Json(PublicEvent {
        event: registry.active().map(|event| PublicEventName {
            id: event.id,
            name: event.name,
        }),
    })
}

/// `GET /public/event/state` — the open event's snapshot, for a visitor. `404` with none open.
pub async fn get_state(
    State(registry): State<EventRegistry>,
) -> Result<Json<EventView>, ProtocolError> {
    Ok(Json(open_view(&registry)?))
}

/// `GET /public/event/changes` — the open event's change feed: a `cursor` event each time
/// its log grows. It follows the event that was open when it was asked for; a page learns
/// that another event was opened from `GET /public/event`.
pub async fn get_changes(
    State(registry): State<EventRegistry>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ProtocolError> {
    let id = open_event(&registry)?;
    crate::control::get_changes(State(registry), Path(id)).await
}

/// `GET /public/event/pilots` — the pilots in the open event: callsign and colour only.
pub async fn get_pilots(
    State(registry): State<EventRegistry>,
) -> Result<Json<Vec<Pilot>>, ProtocolError> {
    let wanted = mentioned(&open_view(&registry)?).pilots;
    Ok(Json(
        registry
            .pilots()
            .list()
            .into_iter()
            .filter(|pilot| wanted.contains(&pilot.id))
            .map(pilot_for_visitors)
            .collect(),
    ))
}

/// `GET /public/event/classes` — the classes in the open event: name only.
pub async fn get_classes(
    State(registry): State<EventRegistry>,
) -> Result<Json<Vec<Class>>, ProtocolError> {
    let wanted = mentioned(&open_view(&registry)?).classes;
    Ok(Json(
        registry
            .classes()
            .list()
            .into_iter()
            .filter(|class| wanted.contains(&class.id))
            .map(class_for_visitors)
            .collect(),
    ))
}

/// `GET /public/event/timers` — the timers the open event's heats are flown on: name and
/// seats only.
pub async fn get_timers(
    State(registry): State<EventRegistry>,
) -> Result<Json<Vec<TimerView>>, ProtocolError> {
    let wanted = mentioned(&open_view(&registry)?).timers;
    Ok(Json(
        registry
            .timers()
            .list()
            .into_iter()
            .filter(|view| wanted.contains(&view.timer.id))
            .map(timer_for_visitors)
            .collect(),
    ))
}

/// `GET /public/event/channels` — the video channel of every seat on the timers the open
/// event is flown on. A bench seat has none. The Director asks a timer for its channels at
/// most once in a while, however many are watching (see [`crate::timers::seat_channels`]).
pub async fn get_channels(
    State(registry): State<EventRegistry>,
) -> Result<Json<Vec<SeatChannel>>, ProtocolError> {
    let wanted = mentioned(&open_view(&registry)?).timers;
    let mut channels = Vec::new();
    for timer in wanted {
        channels.extend(crate::timers::seat_channels(&registry, &timer).await);
    }
    Ok(Json(channels))
}
