//! A pilot signing themselves up for a heat (docs/primitives.md §11.5).
//!
//! The rules are the engine's (`gridfpv_engine::signup`). The director changes a sign-up
//! through the ordinary command route, `POST /events/{id}/commands`, with a `Signup`
//! command. These routes are the **pilot's** way in, from their own phone or the kiosk:
//!
//! - `POST /public/pilot/signup/take` — take a seat in a heat.
//! - `POST /public/pilot/signup/next` — take the first free spot on a seat.
//! - `POST /public/pilot/signup/leave` — give a spot up.
//!
//! Three things are decided here and nowhere else:
//!
//! - **Who is acting** is the pilot the request is signed in as. A pilot is never named in
//!   the body, so nobody can sign another pilot up, or take another pilot's spot away.
//! - **Which event** is the one that is open. A sign-in is for that event only.
//! - **By whom** is always "the pilot", so every rule of the phase applies. Nothing a
//!   phone can send makes it the director.
//!
//! A change that is refused answers `409` with the reason, in words, and changes nothing.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use gridfpv_events::{Command, PhaseId, PilotId, SignupBy, SignupCommand, SignupSeat};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::control::{CommandOutcome, with_event};
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventId, EventRegistry};

/// `POST /public/pilot/signup/take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TakeSpotRequest {
    pub heat: PhaseId,
    pub seat: SignupSeat,
}

/// `POST /public/pilot/signup/next`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NextSpotRequest {
    /// The sign-up phase.
    pub phase: PhaseId,
    pub seat: SignupSeat,
}

/// `POST /public/pilot/signup/leave`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LeaveSpotRequest {
    pub heat: PhaseId,
}

/// The pilot a request is signed in as, and the open event, or the reason there is none.
pub(crate) fn acting(
    registry: &EventRegistry,
    headers: &HeaderMap,
) -> Result<(EventId, PilotId), ProtocolError> {
    let Some(event) = registry.active().map(|event| event.id) else {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            "No event is open.",
        ));
    };
    let Some(pilot) = crate::pilot_access::signed_in_pilot(registry, headers) else {
        return Err(ProtocolError::new(
            ErrorCode::Unauthorized,
            "Sign in with your code first.",
        ));
    };
    Ok((event, pilot.id))
}

/// Run one sign-up command for the signed-in pilot.
fn run(
    registry: &EventRegistry,
    headers: &HeaderMap,
    command: impl FnOnce(PilotId) -> SignupCommand,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    let (event, pilot) = acting(registry, headers)?;
    with_event(registry, &event, |session| {
        session.commands(vec![Command::Signup(command(pilot))])?;
        Ok(Json(session.outcome()))
    })
}

/// Decide an ask a pilot made on the linked server (docs/primitives.md §13.4).
///
/// It is decided exactly as if the pilot had asked at the race: as the pilot's own, under
/// every rule of the phase. The server says which pilot; the Director checks it has that
/// pilot, and only ever acts on the event that is open.
pub fn decide(
    registry: &EventRegistry,
    request: &crate::remote::PushRequest,
) -> Result<(), String> {
    use crate::remote::PilotAsk;
    let open = registry.active().map(|event| event.id);
    if open.as_ref() != Some(&request.event) {
        return Err("That event is not the one open at the race right now.".to_owned());
    }
    if !registry.pilots().exists(&request.pilot) {
        return Err("The race computer does not have that pilot.".to_owned());
    }
    let pilot = request.pilot.clone();
    let by = SignupBy::Pilot;
    let command = match request.ask.clone() {
        PilotAsk::Take { heat, seat } => SignupCommand::Take {
            heat,
            seat,
            pilot,
            by,
        },
        PilotAsk::Next { phase, seat } => SignupCommand::TakeNext {
            phase,
            seat,
            pilot,
            by,
        },
        PilotAsk::Leave { heat } => SignupCommand::Leave { heat, pilot, by },
    };
    with_event(registry, &request.event, |session| {
        session.commands(vec![Command::Signup(command)])
    })
    .map_err(|refused| refused.message)
}

/// `POST /public/pilot/signup/take` — the signed-in pilot takes a seat in a heat.
pub async fn post_take(
    State(registry): State<EventRegistry>,
    headers: HeaderMap,
    Json(body): Json<TakeSpotRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    run(&registry, &headers, |pilot| SignupCommand::Take {
        heat: body.heat,
        seat: body.seat,
        pilot,
        by: SignupBy::Pilot,
    })
}

/// `POST /public/pilot/signup/next` — the signed-in pilot takes the first free spot on a seat.
pub async fn post_next(
    State(registry): State<EventRegistry>,
    headers: HeaderMap,
    Json(body): Json<NextSpotRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    run(&registry, &headers, |pilot| SignupCommand::TakeNext {
        phase: body.phase,
        seat: body.seat,
        pilot,
        by: SignupBy::Pilot,
    })
}

/// `POST /public/pilot/signup/leave` — the signed-in pilot gives their spot in a heat up.
pub async fn post_leave(
    State(registry): State<EventRegistry>,
    headers: HeaderMap,
    Json(body): Json<LeaveSpotRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    run(&registry, &headers, |pilot| SignupCommand::Leave {
        heat: body.heat,
        pilot,
        by: SignupBy::Pilot,
    })
}
