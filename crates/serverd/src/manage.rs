//! Running a club's own things from the server's page: its name, its link tokens, and
//! which of its events are listed or removed.
//!
//! | route | what |
//! |-------|------|
//! | `PATCH /api/clubs/{club}` | give the club a new name |
//! | `GET /api/clubs/{club}/tokens` | the club's link tokens, those taken back too |
//! | `POST /api/clubs/{club}/tokens` | make one. The token itself is in the answer, once |
//! | `DELETE /api/clubs/{club}/tokens/{id}` | take one back |
//! | `PUT /api/clubs/{club}/events/{event}/listing` | hide an event from the list, or show it |
//! | `DELETE /api/clubs/{club}/events/{event}` | remove an event from the server |
//!
//! Every one is for a director of the club, or an owner of the server (§13.2). A director
//! makes the tokens their own Grid pushes with (§13.6).
//!
//! **A hidden event** is left out of the club's list for everyone else. Anyone who has its
//! address can still open it (§13.9: unlisted).
//!
//! **A removed event** is gone from the server: its copy is deleted. A Director that still
//! holds it is told so and does not send it again until its director says to (`push`).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use gridfpv_server::error::ErrorCode;
use gridfpv_server::events::EventId;
use gridfpv_server::remote::PushClub;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::Act;
use crate::app::AppState;
use crate::book::{BookError, LinkToken};
use crate::people::{Signed, in_club};
use crate::push::ApiError;
use crate::store::valid_event_id;

/// A link token as a list shows it. The token itself is never in this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkTokenView {
    /// A short id, to take it back by.
    pub id: String,
    /// What it is for, in the words of whoever made it.
    pub label: String,
    /// When it was made, in milliseconds since 1970.
    #[ts(type = "number")]
    pub created_at: i64,
    /// When it was taken back. A token that was taken back does not work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub revoked_at: Option<i64>,
}

/// A request for a new link token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkTokenRequest {
    pub label: String,
}

/// A link token that was just made. **`secret` is shown this once.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkTokenMade {
    pub token: LinkTokenView,
    /// The token to paste into Grid. The server does not keep it.
    pub secret: String,
}

/// A new name for a club.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClubChange {
    pub name: String,
}

/// Whether an event is hidden from the club's list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventListing {
    pub hidden: bool,
}

pub(crate) fn book_error(e: BookError) -> ApiError {
    match e {
        BookError::Invalid(why) => ApiError::bad(why),
        BookError::NoSuchClub(_) => ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownScope,
            "This server has no such club.",
        ),
        other => {
            eprintln!("gridfpv-server: the server's records could not be written: {other}");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "The server could not write its records.",
            )
        }
    }
}

fn view(token: LinkToken) -> LinkTokenView {
    LinkTokenView {
        id: token.id,
        label: token.label,
        created_at: token.created_at,
        revoked_at: token.revoked_at,
    }
}

fn no_event() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::UnknownScope,
        "This server holds no such event.",
    )
}

/// `PATCH /api/clubs/{club}`.
pub async fn patch_club(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Json(change): Json<ClubChange>,
) -> Result<Json<PushClub>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    let club = state
        .book
        .rename_club(&club, &change.name)
        .map_err(book_error)?;
    Ok(Json(PushClub {
        id: club.id.0,
        name: club.name,
    }))
}

/// `GET /api/clubs/{club}/tokens`: newest first.
pub async fn get_tokens(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Vec<LinkTokenView>>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    let mut tokens: Vec<LinkTokenView> = state
        .book
        .tokens()
        .into_iter()
        .filter(|token| token.club == club)
        .map(view)
        .collect();
    tokens.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    Ok(Json(tokens))
}

/// `POST /api/clubs/{club}/tokens`.
pub async fn post_token(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Json(request): Json<LinkTokenRequest>,
) -> Result<Json<LinkTokenMade>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    let (token, secret) = state
        .book
        .new_token(&club, &request.label)
        .map_err(book_error)?;
    Ok(Json(LinkTokenMade {
        token: view(token),
        secret,
    }))
}

/// `DELETE /api/clubs/{club}/tokens/{id}`.
pub async fn delete_token(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    if state
        .book
        .revoke_club_token(&club, &id)
        .map_err(book_error)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownScope,
            "This club has no such link token, or it was already taken back.",
        ))
    }
}

/// `PUT /api/clubs/{club}/events/{event}/listing`.
pub async fn put_listing(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): Path<(String, String)>,
    Json(listing): Json<EventListing>,
) -> Result<Json<EventListing>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    if !valid_event_id(&event) || state.store.get(&club, &EventId(event.clone()))?.is_none() {
        return Err(no_event());
    }
    let note = state
        .book
        .note_event(&club, &event, |note| note.hidden = listing.hidden)
        .map_err(book_error)?;
    Ok(Json(EventListing {
        hidden: note.hidden,
    }))
}

/// `DELETE /api/clubs/{club}/events/{event}`.
pub async fn delete_event(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    if !valid_event_id(&event) || state.store.get(&club, &EventId(event.clone()))?.is_none() {
        return Err(no_event());
    }
    // The mark first: a Director that pushes between the two steps is refused, and the
    // copy it could have put back is not made.
    state
        .book
        .note_event(&club, &event, |note| {
            note.hidden = false;
            note.removed_at = Some(crate::people::now_ms());
        })
        .map_err(book_error)?;
    state.store.delete(&club, &EventId(event))?;
    Ok(StatusCode::NO_CONTENT)
}
