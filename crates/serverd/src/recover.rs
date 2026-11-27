//! Helping someone who can no longer sign in (docs/primitives.md §13.8).
//!
//! There are no passwords here, so there is nothing to reset. A person signs in with a
//! provider, and when that sign-in is lost (a closed Discord account, a stolen one), an
//! owner of the server does two things for them:
//!
//! 1. **Takes the old sign-in off the account**, when it should no longer work. The account
//!    is signed out everywhere at once.
//! 2. **Makes a re-connect link** and sends it to the person. They open it and sign in with
//!    a provider account they do have, and that sign-in is put on their old account. Their
//!    roles, their pilots and their results are all still there.
//!
//! | route | who | what |
//! |-------|-----|------|
//! | `GET /api/accounts/{account}/sign-ins` | an owner | how an account signs in, and whether it has a link waiting |
//! | `DELETE /api/accounts/{account}/sign-ins/{provider}/{subject}` | an owner | take a sign-in off it |
//! | `POST /api/accounts/{account}/reconnect` | an owner | make a re-connect link. Its secret is in the answer, once |
//! | `DELETE /api/accounts/{account}/reconnect` | an owner | take the link back |
//! | `GET /api/reconnect/{secret}` | anyone with the link | whose account the link is for |
//!
//! **Only an owner.** An account is for the whole server (§13.2), so a link into it is a
//! link into every club it is in. A club's director has no say in the others.
//!
//! **A link is as good as the account.** It works once, lasts seven days, and an account
//! has one at a time: making another takes back the one before. The owner must be sure who
//! they send it to.
//!
//! An owner who has lost their own sign-in uses the command line on the server:
//! `gridfpv-server account reconnect <id>`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use gridfpv_server::error::ErrorCode;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::{AccountError, AccountId, Act};
use crate::app::AppState;
use crate::people::{MySignIn, Signed, now_ms};
use crate::push::ApiError;

/// How an account signs in, for an owner helping its holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct AccountSignIns {
    pub id: String,
    pub name: String,
    pub sign_ins: Vec<MySignIn>,
    /// When the re-connect link it has waiting stops working, in milliseconds since 1970.
    /// Absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub link_until: Option<i64>,
}

/// A new re-connect link. **The secret is given this once.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ReconnectMade {
    /// What goes in the link: `…/#/reconnect/<secret>`.
    pub secret: String,
    /// When it stops working, in milliseconds since 1970.
    #[ts(type = "number")]
    pub expires_at: i64,
}

/// What a re-connect link is for, as whoever opens it is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ReconnectView {
    /// What the account is called.
    pub name: String,
}

fn only_owners() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
        "Only an owner of this server can do that.",
    )
}

/// The account an owner asked about. Someone who is not an owner is not told whether there
/// is one.
fn helped(
    state: &AppState,
    standing: &crate::accounts::Standing,
    account: String,
) -> Result<crate::accounts::Account, ApiError> {
    if !standing.may(Act::RunServer) {
        return Err(only_owners());
    }
    Ok(state
        .accounts
        .account(&AccountId(account))?
        .ok_or(AccountError::NoSuchAccount)?)
}

fn shown(state: &AppState, account: crate::accounts::Account) -> Result<AccountSignIns, ApiError> {
    Ok(AccountSignIns {
        sign_ins: state
            .accounts
            .sign_ins(&account.id)?
            .into_iter()
            .map(|s| MySignIn {
                provider: s.provider,
                subject: s.subject,
                label: s.label,
            })
            .collect(),
        link_until: state
            .accounts
            .reconnect_of(&account.id, now_ms())?
            .map(|link| link.expires_at),
        id: account.id.0,
        name: account.name,
    })
}

/// `GET /api/accounts/{account}/sign-ins`.
pub async fn get_sign_ins(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(account): Path<String>,
) -> Result<Json<AccountSignIns>, ApiError> {
    let account = helped(&state, &standing, account)?;
    Ok(Json(shown(&state, account)?))
}

/// `DELETE /api/accounts/{account}/sign-ins/{provider}/{subject}`.
///
/// The last one may go, which the account's holder cannot do themselves: the owner then
/// sends a link. An owner's own account keeps its last sign-in all the same, so nobody
/// locks themselves out by a slip.
pub async fn delete_sign_in(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((account, provider, subject)): Path<(String, String, String)>,
) -> Result<Json<AccountSignIns>, ApiError> {
    let account = helped(&state, &standing, account)?;
    if account.id == standing.account {
        state
            .accounts
            .remove_sign_in(&account.id, &provider, &subject)?;
    } else {
        state
            .accounts
            .drop_sign_in(&account.id, &provider, &subject)?;
    }
    Ok(Json(shown(&state, account)?))
}

/// `POST /api/accounts/{account}/reconnect`.
pub async fn post_reconnect(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(account): Path<String>,
) -> Result<Json<ReconnectMade>, ApiError> {
    let account = helped(&state, &standing, account)?;
    let (link, secret) =
        state
            .accounts
            .new_reconnect(&account.id, Some(&standing.account), now_ms())?;
    Ok(Json(ReconnectMade {
        secret,
        expires_at: link.expires_at,
    }))
}

/// `DELETE /api/accounts/{account}/reconnect`.
pub async fn delete_reconnect(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(account): Path<String>,
) -> Result<StatusCode, ApiError> {
    let account = helped(&state, &standing, account)?;
    if state.accounts.revoke_reconnect(&account.id, now_ms())? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AccountError::ReconnectDoesNotWork.into())
    }
}

/// `GET /api/reconnect/{secret}`: whose account a link is for, when it still works.
pub async fn get_reconnect(
    State(state): State<AppState>,
    Path(secret): Path<String>,
) -> Result<Json<ReconnectView>, ApiError> {
    let (_, account) = state
        .accounts
        .peek_reconnect(&secret, now_ms())?
        .ok_or(AccountError::ReconnectDoesNotWork)?;
    Ok(Json(ReconnectView { name: account.name }))
}
