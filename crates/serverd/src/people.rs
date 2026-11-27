//! Accounts over HTTP (docs/primitives.md §13.2, §13.7, §13.8): who is signed in, invites,
//! and a club's members. The rules themselves are in [`crate::accounts`]; this is who may
//! ask, and the shapes on the wire.
//!
//! | route | who | what |
//! |-------|-----|------|
//! | `GET /api/me` | anyone | who this browser is signed in as, if anyone |
//! | `PATCH /api/me` | signed in | change the account's name |
//! | `DELETE /api/me/sign-ins/{provider}/{subject}` | signed in | take a sign-in off the account |
//! | `POST /api/sign-out` | anyone | end this browser's session |
//! | `GET /api/invites/{secret}` | anyone | what an invite link is for |
//! | `POST /api/invites/{secret}/accept` | signed in | use it as this account |
//! | `GET /api/clubs/{club}/invites` | director | the club's invites |
//! | `POST /api/clubs/{club}/invites` | director | make one; the link is shown once |
//! | `DELETE /api/clubs/{club}/invites/{id}` | director | take one back |
//! | `GET /api/clubs/{club}/members` | director | the club's members |
//! | `PUT /api/clubs/{club}/members/{account}` | director | give a member a role |
//! | `DELETE /api/clubs/{club}/members/{account}` | director | remove a member |
//!
//! "Director" means a director of **that** club, or a server owner (§13.2).
//!
//! # The session
//!
//! A signed-in browser holds a cookie, [`SESSION_COOKIE`]. It cannot be read by a page's
//! script, is not sent on a request another site makes, and is marked for HTTPS only when
//! the server is reached over HTTPS.
//!
//! # A request another site makes
//!
//! A browser says which site a request came from (`Origin`). A request that changes
//! something and comes from another site is refused, signed in or not ([`same_site`]).

use axum::Json;
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::header::{COOKIE, HOST, ORIGIN, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use gridfpv_server::error::ErrorCode;
use gridfpv_server::remote::{PilotProfile, PushClub};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::{
    AccountError, AccountId, Act, Invite, InviteKind, NewInvite, Role, SESSION_MS, Standing,
};
use crate::app::AppState;
use crate::book::ClubId;
use crate::push::ApiError;

/// The cookie a signed-in browser holds.
pub const SESSION_COOKIE: &str = "grid_session";

/// The most places a group invite may have.
const GROUP_USES_MAX: u32 = 500;

/// The longest a group invite may last, in days.
const GROUP_DAYS_MAX: u32 = 90;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── What is on the wire ─────────────────────────────────────────────────────

/// A role in a club (§13.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ClubRole {
    Pilot,
    Approver,
    Director,
}

impl From<Role> for ClubRole {
    fn from(role: Role) -> Self {
        match role {
            Role::Pilot => Self::Pilot,
            Role::Approver => Self::Approver,
            Role::Director => Self::Director,
        }
    }
}

impl From<ClubRole> for Role {
    fn from(role: ClubRole) -> Self {
        match role {
            ClubRole::Pilot => Self::Pilot,
            ClubRole::Approver => Self::Approver,
            ClubRole::Director => Self::Director,
        }
    }
}

/// One way the signed-in account signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MySignIn {
    pub provider: String,
    /// The provider's own id for the person. Needed to take the sign-in off.
    pub subject: String,
    /// What to show for it, such as a Discord username.
    pub label: String,
}

/// The signed-in account's place in one club.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MyClub {
    pub club: PushClub,
    pub role: ClubRole,
}

/// The account this browser is signed in as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MyAccount {
    pub id: String,
    pub name: String,
    /// A server owner: may do anything in any club.
    pub owner: bool,
    pub sign_ins: Vec<MySignIn>,
    pub clubs: Vec<MyClub>,
    /// The club the front page opens on for this account (§13.2): the one it marked, or
    /// else the one it joined first. Never an archived club. Absent for an account in no
    /// club that has marked none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub primary: Option<String>,
    /// The pilots this account holds, and those it has asked for.
    #[serde(default)]
    pub pilots: Vec<crate::claims::MyPilot>,
    /// What they say of themselves as a pilot (§13.7): callsign, colour, and the ids they
    /// have elsewhere. The same in every club.
    #[serde(default)]
    pub profile: PilotProfile,
    /// The clubs they asked to join and have not heard from (§13.7).
    #[serde(default)]
    pub join_asks: Vec<PushClub>,
    /// The email address they gave, when they gave one (§13.8). Shown to nobody else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub email: Option<crate::email::MyEmail>,
}

/// `GET /api/server`: what anyone may know of how this server is run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ServerInfo {
    /// Whether someone with no invite may make an account here (§13.7). The owner's
    /// choice. When `false`, an account needs an invite.
    pub open_sign_up: bool,
    /// Whether people may sign in with a username and password kept here (§13.8). The
    /// owner's choice.
    #[serde(default)]
    pub password_sign_in: bool,
    /// Whether this server sends email: to confirm an address, and to let someone set a
    /// forgotten password by themselves (§13.8).
    #[serde(default)]
    pub email: bool,
}

/// How a server is run, as anyone may know it.
pub(crate) fn server_info(state: &AppState) -> ServerInfo {
    ServerInfo {
        open_sign_up: state.book.open_sign_up(),
        password_sign_in: state.book.password_sign_in(),
        email: crate::email::mailer_now(state).is_some() && state.public_url.is_some(),
    }
}

/// `PUT /api/server/sign-up`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignUpChange {
    /// Let anyone who can sign in make an account.
    pub open: bool,
}

/// `GET /api/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Me {
    /// Absent when this browser is not signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<MyAccount>,
}

/// `PUT /api/me/primary-club`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PrimaryClub {
    pub club: String,
}

/// `PATCH /api/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MeChange {
    pub name: String,
}

/// What an invite is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum InviteFor {
    /// To be an owner of this server.
    Owner,
    /// One person, to one club.
    Personal,
    /// Anyone with the link, to one club, as a pilot.
    Group,
}

/// The pilot an invite is made for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct InvitePilot {
    pub id: String,
    pub callsign: String,
}

/// An invite, without its link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct InviteView {
    /// A short id to name it by. Not the link.
    pub id: String,
    pub kind: InviteFor,
    /// The club it is to. Absent for an owner invite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub club: Option<PushClub>,
    /// The role it gives in that club.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<ClubRole>,
    /// The club's pilot it is made for. Using it gives that pilot's results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pilot: Option<InvitePilot>,
    pub label: String,
    pub max_uses: u32,
    pub uses: u32,
    /// When it stops working, in milliseconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub expires_at: Option<i64>,
    /// Whether it was taken back.
    pub revoked: bool,
    /// Whether it can still be used.
    pub works: bool,
}

/// `POST /api/clubs/{club}/invites`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct InviteRequest {
    /// `Personal` or `Group`. An owner invite is not made here.
    pub kind: InviteFor,
    pub label: String,
    /// For a personal invite: the role it gives. A pilot when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub role: Option<ClubRole>,
    /// For a personal invite: the club's pilot it is for. Whoever uses the invite gets
    /// that pilot's results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pilot: Option<String>,
    /// For a group invite: how many people may use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_uses: Option<u32>,
    /// For a group invite: how many days it lasts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub days: Option<u32>,
}

/// A new invite, and its link's secret. **The secret is given this once.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct InviteMade {
    pub invite: InviteView,
    /// What goes in the link: `…/#/invite/<secret>`.
    pub secret: String,
}

/// A member of a club.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub role: ClubRole,
}

/// `PUT /api/clubs/{club}/members/{account}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RoleChange {
    pub role: ClubRole,
}

// ── Errors ──────────────────────────────────────────────────────────────────

impl From<AccountError> for ApiError {
    fn from(e: AccountError) -> Self {
        let (status, code) = match &e {
            AccountError::InviteDoesNotWork
            | AccountError::ReconnectDoesNotWork
            | AccountError::NoSuchAccount
            | AccountError::NotAMember
            | AccountError::NoSuchAsk
            | AccountError::NoSuchPilot
            | AccountError::NoSuchClaim => (StatusCode::NOT_FOUND, ErrorCode::UnknownScope),
            AccountError::SignInTaken
            | AccountError::LastSignIn
            | AccountError::LastDirector
            | AccountError::PilotTaken => (StatusCode::CONFLICT, ErrorCode::Refused),
            AccountError::Invalid(_) => (StatusCode::BAD_REQUEST, ErrorCode::BadRequest),
            AccountError::Storage(why) => {
                eprintln!("gridfpv-server: accounts failed: {why}");
                return Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "The server could not read or write its accounts.",
                );
            }
        };
        Self::new(status, code, e.to_string())
    }
}

fn not_signed_in() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthorized,
        "Sign in first.",
    )
}

pub(crate) fn not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
        "Only a director of this club can do that.",
    )
}

fn no_club() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::UnknownScope,
        "This server has no such club.",
    )
}

// ── The session ─────────────────────────────────────────────────────────────

/// The session secret in a request's cookies, if there is one.
pub fn session_secret(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

/// Whether the browser reached the server over HTTPS, as far as the server can tell: it is
/// set to say so (`GRIDFPV_SERVER_PUBLIC_URL`), or the web server in front says so.
fn over_https(state: &AppState, headers: &HeaderMap) -> bool {
    if let Some(url) = &state.public_url {
        return url.starts_with("https://");
    }
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

/// The `Set-Cookie` that signs a browser in with a session.
pub fn session_cookie(state: &AppState, headers: &HeaderMap, secret: &str) -> HeaderValue {
    let secure = if over_https(state, headers) {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={secret}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        SESSION_MS / 1000
    ))
    .expect("a session secret is plain text")
}

/// The `Set-Cookie` that signs a browser out.
fn no_session_cookie(state: &AppState, headers: &HeaderMap) -> HeaderValue {
    let secure = if over_https(state, headers) {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}"
    ))
    .expect("plain text")
}

/// The account a request is signed in as. Refused with 401 when it is not.
pub struct Signed(pub Standing);

/// The account a request is signed in as, if it is.
pub struct MaybeSigned(pub Option<Standing>);

impl FromRequestParts<AppState> for MaybeSigned {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let Some(secret) = session_secret(&parts.headers) else {
            return Ok(Self(None));
        };
        Ok(Self(state.accounts.session(secret, now_ms())?))
    }
}

impl FromRequestParts<AppState> for Signed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        MaybeSigned::from_request_parts(parts, state)
            .await?
            .0
            .map(Signed)
            .ok_or_else(not_signed_in)
    }
}

/// Refuse a request that changes something and was made by another site.
///
/// A browser sends `Origin` on such a request. It must name this server: the address the
/// request itself was sent to (`Host`), or the address the server is set to be reached at.
/// A request with no `Origin` is not from a page in a browser (a program, a Director), and
/// has no cookie to misuse.
pub async fn same_site(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let reads = matches!(
        *request.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    if !reads && let Some(origin) = request.headers().get(ORIGIN) {
        let origin = origin.to_str().unwrap_or_default();
        let host_of = |url: &str| -> Option<String> {
            let rest = url.split_once("://")?.1;
            Some(rest.split('/').next()?.to_ascii_lowercase())
        };
        let asked = request
            .headers()
            .get(HOST)
            .and_then(|h| h.to_str().ok())
            .map(str::to_ascii_lowercase);
        let ours = state.public_url.as_deref().and_then(host_of);
        let from = host_of(origin);
        let same = from.is_some() && (from == asked || from == ours);
        if !same {
            return ApiError::new(
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "This request came from another website.",
            )
            .into_response();
        }
    }
    next.run(request).await
}

// ── Building the shapes ─────────────────────────────────────────────────────

pub(crate) fn club_of(state: &AppState, id: &ClubId) -> Option<PushClub> {
    state.book.club(id).map(|club| PushClub {
        id: club.id.0,
        name: club.name,
    })
}

fn invite_view(state: &AppState, invite: &Invite, now: i64) -> InviteView {
    InviteView {
        id: invite.id.clone(),
        kind: match invite.kind {
            InviteKind::Owner => InviteFor::Owner,
            InviteKind::Personal { .. } => InviteFor::Personal,
            InviteKind::Group { .. } => InviteFor::Group,
        },
        club: invite.kind.club().and_then(|club| club_of(state, club)),
        role: invite.kind.role().map(ClubRole::from),
        pilot: match &invite.kind {
            InviteKind::Personal {
                club,
                pilot: Some(pilot),
                ..
            } => state
                .accounts
                .club_pilot(club, pilot)
                .ok()
                .flatten()
                .map(|found| InvitePilot {
                    id: found.pilot,
                    callsign: found.callsign,
                }),
            _ => None,
        },
        label: invite.label.clone(),
        max_uses: invite.max_uses,
        uses: invite.uses,
        expires_at: invite.expires_at,
        revoked: invite.revoked_at.is_some(),
        works: invite.stands(now),
    }
}

fn my_account(state: &AppState, standing: &Standing) -> Result<Option<MyAccount>, ApiError> {
    let Some(account) = state.accounts.account(&standing.account)? else {
        return Ok(None);
    };
    Ok(Some(MyAccount {
        id: account.id.0,
        name: account.name,
        owner: standing.owner,
        sign_ins: state
            .accounts
            .sign_ins(&standing.account)?
            .into_iter()
            .map(|s| MySignIn {
                provider: s.provider,
                subject: s.subject,
                label: s.label,
            })
            .collect(),
        clubs: standing
            .clubs
            .iter()
            // A club that is gone from the server is not shown.
            .filter_map(|m| {
                Some(MyClub {
                    club: club_of(state, &m.club)?,
                    role: m.role.into(),
                })
            })
            .collect(),
        primary: primary_of(state, standing)?,
        pilots: crate::claims::my_pilots(state, &standing.account)?,
        profile: state.accounts.profile(&standing.account)?,
        email: crate::email::my_email(state, &standing.account)?,
        join_asks: state
            .accounts
            .my_join_asks(&standing.account)?
            .iter()
            // An ask to a club that is gone, or archived, has no answer coming.
            .filter(|club| {
                state
                    .book
                    .club(club)
                    .is_some_and(|found| found.archived_at.is_none())
            })
            .filter_map(|club| club_of(state, club))
            .collect(),
    }))
}

/// An account as its own browser is shown it, read fresh: after a change to which clubs
/// it is in, the standing a request came with is out of date.
pub(crate) fn me_of(state: &AppState, account: &AccountId) -> Result<Me, ApiError> {
    let Some(standing) = state.accounts.standing(account)? else {
        return Ok(Me { account: None });
    };
    Ok(Me {
        account: my_account(state, &standing)?,
    })
}

/// Whether an account may have a club as its primary one: a club that is on the list, and
/// that the account is in. An owner, who looks after every club, may pick any.
fn may_be_primary(state: &AppState, standing: &Standing, club: &ClubId) -> bool {
    state
        .book
        .club(club)
        .is_some_and(|found| found.archived_at.is_none())
        && (standing.owner || standing.role_in(club).is_some())
}

/// The club an account's front page opens on. What it marked, while that still holds;
/// otherwise the first club it joined that is still on the list.
fn primary_of(state: &AppState, standing: &Standing) -> Result<Option<String>, ApiError> {
    if let Some(marked) = state.accounts.primary_club(&standing.account)?
        && may_be_primary(state, standing, &marked)
    {
        return Ok(Some(marked.0));
    }
    Ok(state
        .accounts
        .clubs_joined(&standing.account)?
        .into_iter()
        .find(|club| may_be_primary(state, standing, club))
        .map(|club| club.0))
}

/// The club in the address, and the right to do `act` in it.
pub(crate) fn in_club(
    state: &AppState,
    club: String,
    act: impl FnOnce(&ClubId) -> bool,
) -> Result<ClubId, ApiError> {
    let club = ClubId(club);
    // Whether a club exists is not told to someone with no say in it.
    if !act(&club) {
        return Err(not_allowed());
    }
    if state.book.club(&club).is_none() {
        return Err(no_club());
    }
    Ok(club)
}

// ── The routes ──────────────────────────────────────────────────────────────

/// `GET /api/me`.
pub async fn get_me(
    State(state): State<AppState>,
    MaybeSigned(standing): MaybeSigned,
) -> Result<Json<Me>, ApiError> {
    let account = match standing {
        Some(standing) => my_account(&state, &standing)?,
        None => None,
    };
    Ok(Json(Me { account }))
}

/// `PATCH /api/me`.
pub async fn patch_me(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(change): Json<MeChange>,
) -> Result<Json<Me>, ApiError> {
    state.accounts.rename(&standing.account, &change.name)?;
    Ok(Json(Me {
        account: my_account(&state, &standing)?,
    }))
}

/// `PUT /api/me/profile`: set the pilot profile, all of it. A part left out is cleared.
pub async fn put_profile(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(profile): Json<PilotProfile>,
) -> Result<Json<Me>, ApiError> {
    state.accounts.set_profile(&standing.account, profile)?;
    Ok(Json(Me {
        account: my_account(&state, &standing)?,
    }))
}

/// `GET /api/server`.
pub async fn get_server(State(state): State<AppState>) -> Json<ServerInfo> {
    Json(server_info(&state))
}

/// `PUT /api/server/sign-up`: an owner says whether an account needs an invite.
pub async fn put_sign_up(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(change): Json<SignUpChange>,
) -> Result<Json<ServerInfo>, ApiError> {
    if !standing.may(Act::RunServer) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "Only an owner of this server can do that.",
        ));
    }
    state
        .book
        .set_open_sign_up(change.open)
        .map_err(crate::manage::book_error)?;
    Ok(Json(server_info(&state)))
}

/// `PUT /api/me/primary-club`: mark the club the front page opens on.
pub async fn put_primary_club(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(change): Json<PrimaryClub>,
) -> Result<Json<Me>, ApiError> {
    let club = ClubId(change.club);
    if !may_be_primary(&state, &standing, &club) {
        // The same answer for a club that is not there, one that is archived, and one the
        // account is not in.
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            ErrorCode::Refused,
            "Pick a club you are in. An archived club cannot be picked.",
        ));
    }
    state.accounts.set_primary_club(&standing.account, &club)?;
    Ok(Json(Me {
        account: my_account(&state, &standing)?,
    }))
}

/// `DELETE /api/me/sign-ins/{provider}/{subject}`.
pub async fn delete_my_sign_in(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((provider, subject)): Path<(String, String)>,
) -> Result<Json<Me>, ApiError> {
    state
        .accounts
        .remove_sign_in(&standing.account, &provider, &subject)?;
    Ok(Json(Me {
        account: my_account(&state, &standing)?,
    }))
}

/// `POST /api/sign-out`. Signing out when not signed in is fine.
pub async fn post_sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(secret) = session_secret(&headers) {
        state.accounts.end_session(secret)?;
    }
    Ok((
        [(SET_COOKIE, no_session_cookie(&state, &headers))],
        Json(Me { account: None }),
    )
        .into_response())
}

fn no_invite() -> ApiError {
    AccountError::InviteDoesNotWork.into()
}

/// `GET /api/invites/{secret}`: what a link is for, when it still works.
pub async fn get_invite(
    State(state): State<AppState>,
    Path(secret): Path<String>,
) -> Result<Json<InviteView>, ApiError> {
    let now = now_ms();
    let invite = state
        .accounts
        .peek_invite(&secret, now)?
        .ok_or_else(no_invite)?;
    Ok(Json(invite_view(&state, &invite, now)))
}

/// `POST /api/invites/{secret}/accept`: use a link as the signed-in account.
pub async fn post_accept_invite(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(secret): Path<String>,
) -> Result<Json<Me>, ApiError> {
    state
        .accounts
        .accept_invite(&secret, &standing.account, now_ms())?;
    let standing = state
        .accounts
        .standing(&standing.account)?
        .ok_or_else(not_signed_in)?;
    Ok(Json(Me {
        account: my_account(&state, &standing)?,
    }))
}

/// `GET /api/clubs/{club}/invites`.
pub async fn get_club_invites(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Vec<InviteView>>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SeeMembers(c)))?;
    let now = now_ms();
    Ok(Json(
        state
            .accounts
            .invites_of(&club)?
            .iter()
            .map(|invite| invite_view(&state, invite, now))
            .collect(),
    ))
}

/// `POST /api/clubs/{club}/invites`.
pub async fn post_club_invite(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
    Json(request): Json<InviteRequest>,
) -> Result<Json<InviteMade>, ApiError> {
    let role = Role::from(request.role.unwrap_or(ClubRole::Pilot));
    let club = in_club(&state, club, |c| standing.may(Act::Invite(c, role)))?;
    let now = now_ms();
    let bad = |why: &str| ApiError::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, why);
    let spec = match request.kind {
        InviteFor::Owner => return Err(bad("An owner invite is not made for a club.")),
        InviteFor::Personal => NewInvite {
            kind: InviteKind::Personal {
                club,
                role,
                pilot: request.pilot.filter(|pilot| !pilot.is_empty()),
            },
            label: request.label,
            max_uses: 1,
            expires_at: None,
        },
        InviteFor::Group => {
            if request.pilot.is_some() {
                return Err(bad("A group invite is not for one pilot."));
            }
            if request.role.is_some_and(|role| role != ClubRole::Pilot) {
                return Err(bad("A group invite always joins as a pilot."));
            }
            let uses = request.max_uses.unwrap_or(0);
            let days = request.days.unwrap_or(0);
            if !(1..=GROUP_USES_MAX).contains(&uses) {
                return Err(bad("A group invite is for 1 to 500 people."));
            }
            if !(1..=GROUP_DAYS_MAX).contains(&days) {
                return Err(bad("A group invite lasts 1 to 90 days."));
            }
            NewInvite {
                kind: InviteKind::Group { club },
                label: request.label,
                max_uses: uses,
                expires_at: Some(now + i64::from(days) * 24 * 60 * 60 * 1000),
            }
        }
    };
    let (invite, secret) = state
        .accounts
        .new_invite(spec, Some(&standing.account), now)?;
    Ok(Json(InviteMade {
        invite: invite_view(&state, &invite, now),
        secret,
    }))
}

/// `DELETE /api/clubs/{club}/invites/{id}`.
pub async fn delete_club_invite(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| {
        standing.may(Act::Invite(c, Role::Director))
    })?;
    // Only an invite of this club: a director of one club cannot take back another's.
    let ours = state
        .accounts
        .invites_of(&club)?
        .into_iter()
        .any(|invite| invite.id == id);
    if ours && state.accounts.revoke_invite(&id, now_ms())? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(no_invite())
    }
}

/// `GET /api/clubs/{club}/members`.
pub async fn get_members(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Vec<MemberView>>, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SeeMembers(c)))?;
    Ok(Json(
        state
            .accounts
            .members(&club)?
            .into_iter()
            .map(|(account, role)| MemberView {
                id: account.id.0,
                name: account.name,
                role: role.into(),
            })
            .collect(),
    ))
}

/// `PUT /api/clubs/{club}/members/{account}`.
pub async fn put_member(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, account)): Path<(String, String)>,
    Json(change): Json<RoleChange>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    state
        .accounts
        .set_role(&club, &AccountId(account), change.role.into())?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/clubs/{club}/members/{account}`.
pub async fn delete_member(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, account)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = in_club(&state, club, |c| standing.may(Act::SetRole(c)))?;
    state.accounts.remove_member(&club, &AccountId(account))?;
    Ok(StatusCode::NO_CONTENT)
}
