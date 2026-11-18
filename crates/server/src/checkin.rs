//! Check-in for pilots: taking an open seat part-way through a race, and dropping it
//! (`docs/primitives.md` §11.9).
//!
//! The director does this through the command door like any other fact (`SeatTaken`,
//! `SeatDropped`). These two routes are for a pilot acting for themselves, from their phone
//! or a kiosk. They need a pilot's sign-in and act only for that pilot: the pilot's id is
//! never read from the request.
//!
//! | Route | What it does |
//! |---|---|
//! | `POST /public/pilot/checkin/take` | the signed-in pilot takes an open seat |
//! | `POST /public/pilot/checkin/drop` | the signed-in pilot gives their seat up |

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use gridfpv_events::{AdapterId, Command, Event, RaceId, SeatRef};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::control::{CommandOutcome, with_event};
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// `POST /public/pilot/checkin/take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TakeSeatRequest {
    pub race: RaceId,
    pub adapter: AdapterId,
    pub seat: SeatRef,
}

/// `POST /public/pilot/checkin/drop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DropSeatRequest {
    pub race: RaceId,
}

/// `POST /public/pilot/checkin/take` — the signed-in pilot takes an open seat.
pub async fn post_take(
    State(registry): State<EventRegistry>,
    headers: HeaderMap,
    Json(body): Json<TakeSeatRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    let (event, pilot) = crate::signup::acting(&registry, &headers)?;
    with_event(&registry, &event, |session| {
        session.commands(vec![Command::Record(Box::new(Event::SeatTaken {
            race: body.race,
            adapter: body.adapter,
            seat: body.seat,
            pilot,
        }))])?;
        Ok(Json(session.outcome()))
    })
}

/// `POST /public/pilot/checkin/drop` — the signed-in pilot gives up the seat they have.
/// Which seat is worked out here, so a pilot can only ever drop their own.
pub async fn post_drop(
    State(registry): State<EventRegistry>,
    headers: HeaderMap,
    Json(body): Json<DropSeatRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    let (event, pilot) = crate::signup::acting(&registry, &headers)?;
    with_event(&registry, &event, |session| {
        let Some((adapter, seat)) = session
            .state()
            .check_ins
            .seat_of(&body.race, &pilot)
            .map(|(adapter, seat)| (adapter.clone(), seat.clone()))
        else {
            return Err(ProtocolError::new(
                ErrorCode::Refused,
                "You do not have a seat to drop.",
            ));
        };
        session.commands(vec![Command::Record(Box::new(Event::SeatDropped {
            race: body.race,
            adapter,
            seat,
        }))])?;
        Ok(Json(session.outcome()))
    })
}
