//! The routes a Director pushes to (§13.4). The shapes are in `gridfpv_server::remote`.
//!
//! | route | what |
//! |-------|------|
//! | `GET /push/hello` | which club the token pushes into |
//! | `GET /push/events/{id}` | how much of the event's log the server holds |
//! | `PUT /push/events/{id}` | the event's header, in place of the one before |
//! | `POST /push/events/{id}/log` | a run of log entries |
//! | `DELETE /push/events/{id}` | remove the server's copy, so it can be sent again |
//! | `GET /push/events` | every event the server knows of for the club |
//!
//! **An event removed on the server is not taken again by itself.** Someone who runs the
//! club removed it (`manage`), and a Director that still holds it would put it straight
//! back. So its routes answer `409` with [`PushRefusal::Removed`] until the Director's
//! `DELETE` says its director wants it sent again.
//!
//! Every one needs a link token (`Authorization: Bearer gfs_…`), and reaches only the
//! events of the club that token belongs to (§13.6).
//!
//! A batch is taken whole or not at all. One that does not continue the copy gets `409`
//! and a [`PushRefused`] saying why and what is held. One the server cannot read gets `400`
//! and is not kept: an entry that cannot be read would spoil the whole copy.

use axum::Json;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use gridfpv_events::Event;
use gridfpv_server::error::{ErrorCode, ProtocolError};
use gridfpv_server::events::EventId;
use gridfpv_server::remote::{
    PushClub, PushHeader, PushHeld, PushHello, PushLog, PushRefusal, PushRefused, PushStatus,
};
use gridfpv_storage::Chain;

use crate::app::AppState;
use crate::book::Club;
use crate::store::{Held, Refusal, StoreError, valid_event_id};

/// The most entries one batch may hold. A Director sends a long log in several.
pub const BATCH_MAX: usize = 5_000;

/// A failure, as a status and the same error body the Director's own API gives.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: ProtocolError,
}

impl ApiError {
    pub fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ProtocolError::new(code, message),
        }
    }
    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
    }
    fn not_held(id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownScope,
            format!("This server holds no event with the id {id:?} for this club."),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        // The reason goes to the server's own output, not to the caller: it may name paths.
        eprintln!("gridfpv-server: storage failed: {e}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "The server could not read or write its copy of the event.",
        )
    }
}

/// A caller holding a link token, and the club it pushes into.
pub struct Pusher(pub Club);

impl FromRequestParts<AppState> for Pusher {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                let (scheme, token) = value.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
            });
        let club = token
            .and_then(|token| state.book.club_for_token(token))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    ErrorCode::Unauthorized,
                    "A link token is needed to push to this server. It may have been taken back.",
                )
            })?;
        // An archived club is read-only (§13.2). The token is right, so the Director is
        // told the real reason and not that its token is wrong.
        if club.archived_at.is_some() {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                format!(
                    "{} is archived on this server and takes nothing new. An owner of the server can bring it back.",
                    club.name
                ),
            ));
        }
        Ok(Pusher(club))
    }
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

fn status(held: Held) -> PushStatus {
    PushStatus {
        len: held.len,
        chain: held.chain.to_hex(),
    }
}

fn event_id(id: String) -> Result<EventId, ApiError> {
    if valid_event_id(&id) {
        Ok(EventId(id))
    } else {
        Err(ApiError::bad(
            "An event's id is lower-case letters, digits and hyphens.",
        ))
    }
}

/// The answer for an event that was removed on the server.
fn removed() -> Response {
    (
        StatusCode::CONFLICT,
        Json(PushRefused {
            reason: PushRefusal::Removed,
            held: status(Held {
                len: 0,
                chain: Chain::start(),
            }),
        }),
    )
        .into_response()
}

fn is_removed(state: &AppState, club: &Club, id: &EventId) -> bool {
    state.book.event_note(&club.id, &id.0).removed_at.is_some()
}

/// `GET /push/events`: what the server holds of each of the club's events, and the ones
/// removed here. A Director reads it now and then, to notice a copy that changed under it.
pub async fn get_events(
    State(state): State<AppState>,
    Pusher(club): Pusher,
) -> Result<Json<Vec<PushHeld>>, ApiError> {
    let mut all = Vec::new();
    for id in state.store.list(&club.id)? {
        let Some(copy) = state.store.get(&club.id, &id)? else {
            continue;
        };
        let held = copy.held();
        all.push(PushHeld {
            id,
            len: held.len,
            chain: held.chain.to_hex(),
            removed: false,
        });
    }
    for note in state.book.event_notes(&club.id) {
        if note.removed_at.is_some() && !all.iter().any(|held| held.id.0 == note.event) {
            all.push(PushHeld {
                id: EventId(note.event),
                len: 0,
                chain: Chain::start().to_hex(),
                removed: true,
            });
        }
    }
    Ok(Json(all))
}

/// `GET /push/hello`.
pub async fn get_hello(Pusher(club): Pusher) -> Json<PushHello> {
    Json(PushHello {
        version: gridfpv_server::BUILD_VERSION.to_owned(),
        club: PushClub {
            id: club.id.0,
            name: club.name,
        },
    })
}

/// `GET /push/events/{id}`.
pub async fn get_event(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let id = event_id(id)?;
    if is_removed(&state, &club, &id) {
        return Ok(removed());
    }
    let copy = state
        .store
        .get(&club.id, &id)?
        .ok_or_else(|| ApiError::not_held(&id.0))?;
    Ok(Json(status(copy.held())).into_response())
}

/// `PUT /push/events/{id}`.
pub async fn put_event(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(id): Path<String>,
    Json(header): Json<PushHeader>,
) -> Result<Response, ApiError> {
    let id = event_id(id)?;
    if is_removed(&state, &club, &id) {
        return Ok(removed());
    }
    if header.meta.id != id {
        return Err(ApiError::bad(
            "The header is for another event than the one in the address.",
        ));
    }
    let json = serde_json::to_string(&header).map_err(|e| ApiError::bad(e.to_string()))?;
    let copy = state
        .store
        .get_or_create(&club.id, &id)?
        .ok_or_else(|| ApiError::not_held(&id.0))?;
    copy.set_header(&json)?;
    // The club now knows these pilots, whatever becomes of the event (§13.7).
    crate::claims::note(&state, &club.id, header.pilots.iter())?;
    Ok(Json(status(copy.held())).into_response())
}

/// `POST /push/events/{id}/log`.
pub async fn post_log(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(id): Path<String>,
    Json(log): Json<PushLog>,
) -> Result<Response, ApiError> {
    let id = event_id(id)?;
    if is_removed(&state, &club, &id) {
        return Ok(removed());
    }
    let before = Chain::from_hex(&log.before)
        .ok_or_else(|| ApiError::bad("`before` is not a chain value."))?;
    if log.entries.len() > BATCH_MAX {
        return Err(ApiError::bad(format!(
            "A batch holds {BATCH_MAX} entries at most."
        )));
    }
    // Every entry must read as an event before any is kept.
    for (i, entry) in log.entries.iter().enumerate() {
        if let Err(e) = serde_json::from_str::<Event>(entry.event.get()) {
            return Err(ApiError::bad(format!(
                "The server could not read entry {} ({e}). It may be an older build than the Director.",
                log.from + i as u64
            )));
        }
    }
    let entries: Vec<(String, Option<i64>)> = log
        .entries
        .iter()
        .map(|entry| (entry.event.get().to_owned(), entry.recorded_at))
        .collect();
    let copy = state
        .store
        .get_or_create(&club.id, &id)?
        .ok_or_else(|| ApiError::not_held(&id.0))?;
    Ok(match copy.append(log.from, before, &entries)? {
        Ok(held) => {
            // Only a Director whose log this is says what the clock is.
            if let Some(sent_at) = log.sent_at {
                copy.set_clock(sent_at, now_micros())?;
            }
            Json(status(held)).into_response()
        }
        Err((refusal, held)) => (
            StatusCode::CONFLICT,
            Json(PushRefused {
                reason: match refusal {
                    Refusal::Gap => PushRefusal::Gap,
                    Refusal::Different => PushRefusal::Different,
                },
                held: status(held),
            }),
        )
            .into_response(),
    })
}

/// `DELETE /push/events/{id}`.
pub async fn delete_event(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id = event_id(id)?;
    // The director says to send it again: an event removed here may come back.
    let was_removed = is_removed(&state, &club, &id);
    if was_removed {
        state
            .book
            .note_event(&club.id, &id.0, |note| note.removed_at = None)
            .map_err(crate::manage::book_error)?;
    }
    if state.store.delete(&club.id, &id)? || was_removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_held(&id.0))
    }
}
