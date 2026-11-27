//! Getting into a club (docs/primitives.md §13.7).
//!
//! A pilot's account belongs to the whole server. They get into a club by being given a
//! role in it. A director sets their club **public** or **private**:
//!
//! - **Public:** anyone with an account joins, as a pilot.
//! - **Private:** a person asks to join and a director says yes. Or they use an invite, or
//!   a director adds them.
//!
//! | route | who | what |
//! |-------|-----|------|
//! | `PUT /api/clubs/{club}/open` | who runs the club | make it public or private |
//! | `POST /api/clubs/{club}/join` | an account | join a public club, or ask to join a private one |
//! | `DELETE /api/clubs/{club}/join` | an account | take the ask back, or leave the club |
//! | `GET /api/clubs/{club}/join-asks` | who runs the club | who has asked |
//! | `PUT /api/clubs/{club}/join-asks/{account}` | who runs the club | say yes |
//! | `DELETE /api/clubs/{club}/join-asks/{account}` | who runs the club | say no |
//! | `GET /api/clubs/{club}/people?q=` | who runs the club | accounts not in the club, by name |
//! | `POST /api/clubs/{club}/members` | who runs the club | add an account, as a pilot |
//!
//! An archived club takes none of the changes: `manage::not_archived` refuses them all.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use gridfpv_server::error::ErrorCode;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::{AccountError, AccountId, Act};
use crate::app::AppState;
use crate::book::ClubId;
use crate::manage::{ServerClub, book_error};
use crate::people::{Me, Signed, in_club, me_of, now_ms};
use crate::push::ApiError;

/// The fewest letters a director types before the server looks for people.
const FIND_MIN: usize = 2;

/// The most people one look gives back.
const FIND_MAX: usize = 20;

/// `PUT /api/clubs/{club}/open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClubOpen {
    /// Public: anyone with an account may join.
    pub open: bool,
}

/// Someone who asked to join a club, or who could be added to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PersonView {
    pub id: String,
    /// Their account's name.
    pub name: String,
    /// The callsign in their pilot profile, when they gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub callsign: Option<String>,
    /// When they asked to join, for someone who asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub asked_at: Option<i64>,
}

/// `GET /api/clubs/{club}/people?q=`.
#[derive(Debug, Clone, Deserialize)]
pub struct Find {
    #[serde(default)]
    pub q: String,
}

/// `POST /api/clubs/{club}/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NewMember {
    pub account: String,
}

fn no_club() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::UnknownScope,
        "This server has no such club.",
    )
}

/// `PUT /api/clubs/{club}/open`.
pub async fn put_open(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Json(change): Json<ClubOpen>,
) -> Result<Json<ServerClub>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::RunClub(c)))?;
    let club = state
        .book
        .set_club_open(&club, change.open)
        .map_err(book_error)?;
    Ok(Json(club.into()))
}

/// `POST /api/clubs/{club}/join`: join a public club, or ask to join a private one.
/// Answers with the account as it now stands: in the club, or with the ask noted.
pub async fn post_join(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Me>, ApiError> {
    // The list of clubs is open to anyone, so there is nothing to hide in saying a club
    // is not there.
    let club = state.book.club(&ClubId(club)).ok_or_else(no_club)?;
    let now = now_ms();
    if club.open {
        state
            .accounts
            .add_member(&club.id, &standing.account, now)?;
    } else {
        state
            .accounts
            .ask_to_join(&club.id, &standing.account, now)?;
    }
    Ok(Json(me_of(&state, &standing.account)?))
}

/// `DELETE /api/clubs/{club}/join`: take back an ask to join, or leave the club.
pub async fn delete_join(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Me>, ApiError> {
    let club = state.book.club(&ClubId(club)).ok_or_else(no_club)?;
    let asked = state.accounts.drop_join_ask(&club.id, &standing.account)?;
    match state.accounts.remove_member(&club.id, &standing.account) {
        Ok(()) => {}
        // Not in the club: taking the ask back was all there was to do.
        Err(AccountError::NotAMember) if asked => {}
        Err(AccountError::NotAMember) => {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                ErrorCode::UnknownScope,
                "You are not in that club, and have not asked to join it.",
            ));
        }
        Err(other) => return Err(other.into()),
    }
    Ok(Json(me_of(&state, &standing.account)?))
}

/// `GET /api/clubs/{club}/join-asks`: who has asked, the one who asked first at the top.
pub async fn get_asks(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Vec<PersonView>>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    let profiles = state.accounts.profiles()?;
    Ok(Json(
        state
            .accounts
            .join_asks(&club)?
            .into_iter()
            .map(|(account, asked_at)| PersonView {
                callsign: profiles
                    .get(&account.id)
                    .and_then(|profile| profile.callsign.clone()),
                id: account.id.0,
                name: account.name,
                asked_at: Some(asked_at),
            })
            .collect(),
    ))
}

/// `PUT /api/clubs/{club}/join-asks/{account}`: say yes.
pub async fn put_ask(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, account)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    state
        .accounts
        .accept_join_ask(&club, &AccountId(account), now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/clubs/{club}/join-asks/{account}`: say no.
pub async fn delete_ask(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, account)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    if !state.accounts.drop_join_ask(&club, &AccountId(account))? {
        return Err(AccountError::NoSuchAsk.into());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/clubs/{club}/people?q=`: accounts on this server that are not in the club,
/// whose name or callsign holds what was typed. For a director adding someone.
///
/// Fewer than two letters gives nobody: the server's accounts are not listed whole.
pub async fn get_people(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Query(find): Query<Find>,
) -> Result<Json<Vec<PersonView>>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    if find.q.trim().chars().count() < FIND_MIN {
        return Ok(Json(Vec::new()));
    }
    Ok(Json(
        state
            .accounts
            .find_outside(&club, &find.q, FIND_MAX)?
            .into_iter()
            .map(|(account, callsign)| PersonView {
                id: account.id.0,
                name: account.name,
                callsign,
                asked_at: None,
            })
            .collect(),
    ))
}

/// `POST /api/clubs/{club}/members`: add an account to the club, as a pilot. Someone
/// already in it keeps the role they have.
pub async fn post_member(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Json(new): Json<NewMember>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    state
        .accounts
        .add_member(&club, &AccountId(new.account), now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}
