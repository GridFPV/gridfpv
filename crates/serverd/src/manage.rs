//! Running a club's own things from the server's page: its name, its link tokens, and
//! which of its events are listed or removed.
//!
//! | route | what |
//! |-------|------|
//! | `POST /api/clubs` | make a club. For an owner of the server |
//! | `PUT /api/clubs/{club}/archived` | archive a club, or bring it back. For an owner |
//! | `PATCH /api/clubs/{club}` | give the club a new name |
//! | `GET /api/clubs/{club}/tokens` | the club's link tokens, those taken back too |
//! | `POST /api/clubs/{club}/tokens` | make one. The token itself is in the answer, once |
//! | `DELETE /api/clubs/{club}/tokens/{id}` | take one back |
//! | `PUT /api/clubs/{club}/events/{event}/listing` | hide an event from the list, or show it |
//! | `DELETE /api/clubs/{club}/events/{event}` | remove an event from the server |
//!
//! Every one but the first two is for a director of the club, or an owner of the server
//! (§13.2). Only an owner makes a club or archives one.
//!
//! **An archived club** is read-only and off the list of clubs. Nothing in it can be
//! changed ([`not_archived`]), a Director's pushes are refused (`push`), and its invites
//! stop working. Its events still open by their links. An owner can bring it back; invites
//! are made again.
//!
//! The rest: A director
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
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::accounts::Act;
use crate::app::AppState;
use crate::book::{BookError, Club, ClubId, LinkToken};
use crate::people::{Signed, in_club};
use crate::push::ApiError;
use crate::store::valid_event_id;

/// A club, as the server's page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ServerClub {
    /// Never changes. The page's own links use it.
    pub id: String,
    pub name: String,
    /// The friendly address, for handing out: `…/#/c/<word>`. It follows the name.
    pub word: String,
    /// Read-only, and off the list of clubs.
    pub archived: bool,
    /// A public club: anyone with an account may join it. Otherwise a person asks to
    /// join, uses an invite, or is added by a director (§13.7).
    pub open: bool,
}

impl From<Club> for ServerClub {
    fn from(club: Club) -> Self {
        Self {
            id: club.id.0,
            name: club.name,
            word: club.word,
            archived: club.archived_at.is_some(),
            open: club.open,
        }
    }
}

/// `POST /api/clubs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NewClub {
    pub name: String,
}

/// `PUT /api/clubs/{club}/archived`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClubArchived {
    pub archived: bool,
}

pub(crate) const ARCHIVED: &str = "This club is archived, so nothing in it can be changed. An owner of this server can bring it back.";

fn only_owners() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
        "Only an owner of this server can do that.",
    )
}

/// Refuse a request that would change something in an archived club.
///
/// It looks only at the address: anything under `/api/clubs/{club}/` that is not a read.
/// So a route added later is covered without being told. Bringing the club back is the one
/// change let through.
pub async fn not_archived(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let reads = matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    if !reads {
        let mut parts = request.uri().path().split('/').skip(1);
        let under_club = parts.next() == Some("api") && parts.next() == Some("clubs");
        if under_club && let Some(club) = parts.next() {
            let rest: Vec<&str> = parts.collect();
            let archived = state
                .book
                .club(&ClubId(club.to_owned()))
                .is_some_and(|club| club.archived_at.is_some());
            if archived && rest != ["archived"] {
                return ApiError::new(StatusCode::CONFLICT, ErrorCode::Refused, ARCHIVED)
                    .into_response();
            }
        }
    }
    next.run(request).await
}

/// `POST /api/clubs`.
pub async fn post_club(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(new): Json<NewClub>,
) -> Result<Json<ServerClub>, ApiError> {
    if !standing.may(Act::RunServer) {
        return Err(only_owners());
    }
    Ok(Json(
        state.book.new_club(&new.name).map_err(book_error)?.into(),
    ))
}

/// `PUT /api/clubs/{club}/archived`.
pub async fn put_archived(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Json(change): Json<ClubArchived>,
) -> Result<Json<ServerClub>, ApiError> {
    if !standing.may(Act::RunServer) {
        return Err(only_owners());
    }
    let club = state
        .book
        .archive_club(&ClubId(club), change.archived)
        .map_err(book_error)?;
    if change.archived {
        // An invite to a club nobody can join is no invite.
        state
            .accounts
            .revoke_invites_of(&club.id, crate::people::now_ms())?;
        // And an ask to join one has no answer coming.
        state.accounts.drop_join_asks_of(&club.id)?;
    }
    Ok(Json(club.into()))
}

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
) -> Result<Json<ServerClub>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    let club = state
        .book
        .rename_club(&club, &change.name)
        .map_err(book_error)?;
    Ok(Json(club.into()))
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
