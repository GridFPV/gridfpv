//! A club's pilots and the accounts that hold them (docs/primitives.md §13.7): how race
//! results reach an account. The rules are in [`crate::accounts`]; this is who may ask,
//! and the shapes on the wire.
//!
//! A **pilot of a club** is a pilot a Director of that club sent: the id the Director
//! gave them, and their callsign. An account **holds** a pilot when the results flown
//! under that id are the account's. It comes to hold one in three ways:
//!
//! 1. A director says so, from the Director itself (`/push/pilots/…`) or on the club's
//!    page.
//! 2. The person uses a personal invite that was made for that pilot (§13.8).
//! 3. The person asks, and a director or a claims approver of the club says yes.
//!
//! **Knowing a pilot's id is never enough.** Asking gives nothing until someone allowed
//! says yes, and an approver who is not a director cannot say yes to their own ask.
//!
//! | route | who | what |
//! |-------|-----|------|
//! | `GET /api/clubs/{club}/pilots` | member | the club's pilots, and which are taken |
//! | `POST /api/clubs/{club}/pilots/{pilot}/claim` | member | ask for a pilot |
//! | `DELETE /api/clubs/{club}/pilots/{pilot}/claim` | member | stop asking, or let go |
//! | `PUT /api/clubs/{club}/pilots/{pilot}/holder` | approver, director | say yes to an ask; a director may give with no ask |
//! | `DELETE /api/clubs/{club}/pilots/{pilot}/holder` | director | take the pilot from its holder |
//! | `DELETE /api/clubs/{club}/pilots/{pilot}/asks/{account}` | approver, director | say no to an ask |
//! | `GET /api/me/results` | signed in | the events the account's pilots flew |
//! | `GET /push/pilots` | Director | the club's pilots and their holders |
//! | `PUT /push/pilots` | Director | the pilots this Director has |
//! | `GET /push/members` | Director | the club's members |
//! | `PUT /push/pilots/{pilot}/account` | Director | say which member holds a pilot |
//! | `DELETE /push/pilots/{pilot}/account` | Director | take the pilot from its holder |
//!
//! A Director that holds a club's link token speaks for the club's directors (§13.6).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use gridfpv_events::PilotId;
use gridfpv_server::error::ErrorCode;
use gridfpv_server::pilots::Pilot;
use gridfpv_server::remote::{
    PilotProfile, PushAccount, PushClub, PushPilot, PushPilotAccount, ServerEvent,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::{AccountError, AccountId, Act, ClubPilot, valid_pilot_id};
use crate::app::AppState;
use crate::book::ClubId;
use crate::people::{Signed, club_of, now_ms};
use crate::push::{ApiError, Pusher};
use crate::read::{server_event, shown};

// ── What is on the wire ─────────────────────────────────────────────────────

/// A pilot of a club, for someone in the club.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClubPilotView {
    pub id: String,
    pub callsign: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// Whether an account holds this pilot.
    pub taken: bool,
    /// Whether the account asking holds it.
    pub mine: bool,
    /// Whether the account asking has asked for it.
    pub asked: bool,
    /// Who holds it. Told only to those who may say yes to a claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub holder: Option<PushAccount>,
    /// Who has asked for it, oldest first. Told only to those who may say yes.
    #[serde(default)]
    pub asks: Vec<PushAccount>,
}

/// `PUT /api/clubs/{club}/pilots/{pilot}/holder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct HolderChange {
    pub account: String,
}

/// A pilot the signed-in account holds or has asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MyPilot {
    pub club: PushClub,
    pub id: String,
    pub callsign: String,
    /// `false` while the account has only asked.
    pub held: bool,
}

/// Where a pilot stands in one class of an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MyClassResult {
    /// The class's name.
    pub class: String,
    /// 1 for the top.
    pub position: u32,
    /// How many pilots the class has.
    pub of: u32,
    /// Whether racing has placed the pilot yet. Until it has, the position is only the
    /// order of the list.
    pub placed: bool,
}

/// One event one of the account's pilots flew (`GET /api/me/results`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MyResult {
    pub club: PushClub,
    pub event: ServerEvent,
    /// The callsign the pilot flew under.
    pub callsign: String,
    pub classes: Vec<MyClassResult>,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn refuse(message: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, message)
}

fn account_ref(account: &crate::accounts::Account) -> PushAccount {
    PushAccount {
        id: account.id.0.clone(),
        name: account.name.clone(),
        profile: PilotProfile::default(),
    }
}

/// Every account's pilot profile, by account.
type Profiles = std::collections::BTreeMap<AccountId, PilotProfile>;

/// An account with what it says of itself as a pilot: for a Director, which takes it.
fn account_with_profile(account: &crate::accounts::Account, profiles: &Profiles) -> PushAccount {
    PushAccount {
        profile: profiles.get(&account.id).cloned().unwrap_or_default(),
        ..account_ref(account)
    }
}

/// How the held pilots of a club are shown (§13.7): **the account wins where it exists**.
/// A pilot an account holds is shown under that account's callsign and colour, where its
/// profile has them, whatever the event was run with.
pub(crate) struct Faces(std::collections::BTreeMap<String, PilotProfile>);

impl Faces {
    /// The faces of a club's held pilots.
    pub(crate) fn of(state: &AppState, club: &ClubId) -> Result<Self, ApiError> {
        let profiles = state.accounts.profiles()?;
        Ok(Self(
            state
                .accounts
                .claims_in(club)?
                .into_iter()
                .filter(|claim| claim.how.holds())
                .filter_map(|claim| {
                    let profile = profiles.get(&claim.account.id)?.clone();
                    Some((claim.pilot, profile))
                })
                .collect(),
        ))
    }

    /// The callsign to show for a pilot: the holder's, or else the one given.
    pub(crate) fn callsign(&self, pilot: &str, theirs: String) -> String {
        self.0
            .get(pilot)
            .and_then(|profile| profile.callsign.clone())
            .unwrap_or(theirs)
    }

    /// The colour to show for a pilot: the holder's, or else the one given.
    pub(crate) fn color(&self, pilot: &str, theirs: Option<String>) -> Option<String> {
        self.0
            .get(pilot)
            .and_then(|profile| profile.color.clone())
            .or(theirs)
    }
}

/// The club in the address, for someone who may do `act` in it.
fn club_for(
    state: &AppState,
    club: String,
    may: impl FnOnce(&ClubId) -> bool,
    message: &str,
) -> Result<ClubId, ApiError> {
    let club = ClubId(club);
    // Whether a club exists is not told to someone with no say in it.
    if !may(&club) {
        return Err(refuse(message));
    }
    if state.book.club(&club).is_none() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownScope,
            "This server has no such club.",
        ));
    }
    Ok(club)
}

const MEMBERS_ONLY: &str = "Only a member of this club can do that.";
const APPROVERS_ONLY: &str = "Only a director or a claims approver of this club can do that.";
const DIRECTORS_ONLY: &str = "Only a director of this club can do that.";

/// Note the pilots a Director sent for a club.
pub(crate) fn note<'a>(
    state: &AppState,
    club: &ClubId,
    pilots: impl Iterator<Item = &'a Pilot>,
) -> Result<(), ApiError> {
    let pilots: Vec<ClubPilot> = pilots
        .map(|pilot| ClubPilot {
            pilot: pilot.id.0.clone(),
            callsign: pilot.callsign.clone(),
            color: pilot.color.clone(),
        })
        .collect();
    Ok(state.accounts.note_pilots(club, &pilots, now_ms())?)
}

/// The pilots an account holds or has asked for, for its own page.
pub(crate) fn my_pilots(state: &AppState, account: &AccountId) -> Result<Vec<MyPilot>, ApiError> {
    let mut out = Vec::new();
    let mine = state.accounts.profile(account)?;
    for claim in state.accounts.claims_of(account)? {
        // A club that is gone from the server, or a pilot it no longer knows, is not shown.
        let Some(club) = club_of(state, &claim.club) else {
            continue;
        };
        let Some(pilot) = state.accounts.club_pilot(&claim.club, &claim.pilot)? else {
            continue;
        };
        // A pilot they hold is shown as they call themselves. One they only asked for is
        // still the club's.
        let callsign = match (claim.how.holds(), &mine.callsign) {
            (true, Some(callsign)) => callsign.clone(),
            _ => pilot.callsign,
        };
        out.push(MyPilot {
            club,
            id: pilot.pilot,
            callsign,
            held: claim.how.holds(),
        });
    }
    Ok(out)
}

// ── For people in the club ──────────────────────────────────────────────────

/// `GET /api/clubs/{club}/pilots`.
pub async fn get_pilots(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(club): Path<String>,
) -> Result<Json<Vec<ClubPilotView>>, ApiError> {
    let club = club_for(
        &state,
        club,
        |c| standing.may(Act::SeePilots(c)),
        MEMBERS_ONLY,
    )?;
    let sees_who = standing.may(Act::ApproveClaim(&club));
    let claims = state.accounts.claims_in(&club)?;
    let faces = Faces::of(&state, &club)?;
    Ok(Json(
        state
            .accounts
            .club_pilots(&club)?
            .into_iter()
            .map(|pilot| {
                let of_pilot = || claims.iter().filter(|claim| claim.pilot == pilot.pilot);
                let holder = of_pilot().find(|claim| claim.how.holds());
                let mine = |claim: &&crate::accounts::Claim| claim.account.id == standing.account;
                ClubPilotView {
                    taken: holder.is_some(),
                    mine: holder.as_ref().is_some_and(mine),
                    asked: of_pilot().any(|claim| !claim.how.holds() && mine(&claim)),
                    holder: holder
                        .filter(|_| sees_who)
                        .map(|claim| account_ref(&claim.account)),
                    asks: if sees_who {
                        of_pilot()
                            .filter(|claim| !claim.how.holds())
                            .map(|claim| account_ref(&claim.account))
                            .collect()
                    } else {
                        Vec::new()
                    },
                    callsign: faces.callsign(&pilot.pilot, pilot.callsign),
                    color: faces.color(&pilot.pilot, pilot.color),
                    id: pilot.pilot,
                }
            })
            .collect(),
    ))
}

/// `POST /api/clubs/{club}/pilots/{pilot}/claim`: ask for a pilot.
pub async fn post_claim(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, pilot)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = club_for(
        &state,
        club,
        |c| standing.may(Act::SeePilots(c)),
        MEMBERS_ONLY,
    )?;
    // An owner may look at any club, but only a member of a club flies in it.
    state
        .accounts
        .ask_claim(&club, &pilot, &standing.account, now_ms())
        .map_err(|e| match e {
            AccountError::NotAMember => refuse("Join this club first, with an invite."),
            other => other.into(),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/clubs/{club}/pilots/{pilot}/claim`: stop asking, or let go.
pub async fn delete_claim(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, pilot)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = ClubId(club);
    // Letting go of your own needs no say in the club.
    if state
        .accounts
        .drop_claim(&club, &pilot, &standing.account)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AccountError::NoSuchClaim.into())
    }
}

/// `PUT /api/clubs/{club}/pilots/{pilot}/holder`: say an account holds a pilot.
///
/// For an account that asked, this is the yes, from a director or a claims approver. An
/// approver cannot say yes to their own ask. With no ask, only a director may.
pub async fn put_holder(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, pilot)): Path<(String, String)>,
    Json(change): Json<HolderChange>,
) -> Result<StatusCode, ApiError> {
    let club = club_for(
        &state,
        club,
        |c| standing.may(Act::ApproveClaim(c)),
        APPROVERS_ONLY,
    )?;
    let account = AccountId(change.account);
    let now = now_ms();
    if standing.may(Act::GivePilot(&club)) {
        state
            .accounts
            .give_pilot(&club, &pilot, &account, Some(&standing.account), now)?;
        return Ok(StatusCode::NO_CONTENT);
    }
    if account == standing.account {
        return Err(refuse(
            "You cannot say yes to your own ask. Someone else must.",
        ));
    }
    state
        .accounts
        .approve_claim(&club, &pilot, &account, &standing.account, now)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/clubs/{club}/pilots/{pilot}/holder`.
pub async fn delete_holder(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, pilot)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = club_for(
        &state,
        club,
        |c| standing.may(Act::GivePilot(c)),
        DIRECTORS_ONLY,
    )?;
    if state.accounts.free_pilot(&club, &pilot)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AccountError::NoSuchClaim.into())
    }
}

/// `DELETE /api/clubs/{club}/pilots/{pilot}/asks/{account}`: say no to an ask.
pub async fn delete_ask(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, pilot, account)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let club = club_for(
        &state,
        club,
        |c| standing.may(Act::ApproveClaim(c)),
        APPROVERS_ONLY,
    )?;
    let account = AccountId(account);
    let asked = state
        .accounts
        .claims_in(&club)?
        .iter()
        .any(|claim| claim.pilot == pilot && claim.account.id == account && !claim.how.holds());
    // This says no to an ask. Taking a pilot from its holder is the director's to do.
    if asked && state.accounts.drop_claim(&club, &pilot, &account)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AccountError::NoSuchClaim.into())
    }
}

/// `GET /api/me/results`: the events the account's pilots flew, newest first.
pub async fn get_my_results(
    State(state): State<AppState>,
    Signed(standing): Signed,
) -> Result<Json<Vec<MyResult>>, ApiError> {
    // The pilots the account holds, by club. One it has only asked for gives nothing.
    let mut held: BTreeMap<ClubId, Vec<PilotId>> = BTreeMap::new();
    for claim in state.accounts.claims_of(&standing.account)? {
        if claim.how.holds() {
            held.entry(claim.club)
                .or_default()
                .push(PilotId(claim.pilot));
        }
    }
    let mine = state.accounts.profile(&standing.account)?;
    let mut results = Vec::new();
    for (club_id, pilots) in held {
        let Some(club) = club_of(&state, &club_id) else {
            continue;
        };
        for event in state.store.list(&club_id)? {
            // One event that cannot be read must not hide the others.
            let Ok(shown) = shown(&state, &club_id.0, &event.0) else {
                continue;
            };
            let Ok(view) = shown.view() else {
                continue;
            };
            for pilot in &pilots {
                let classes: Vec<MyClassResult> = view
                    .rosters
                    .iter()
                    .filter_map(|roster| {
                        let place = roster.standings.iter().find(|p| &p.pilot == pilot)?;
                        Some(MyClassResult {
                            class: shown
                                .header
                                .classes
                                .iter()
                                .find(|class| class.id == roster.class)
                                .map_or_else(|| "Class".to_owned(), |class| class.name.clone()),
                            position: place.position,
                            of: roster.standings.len() as u32,
                            placed: place.placed_by.is_some(),
                        })
                    })
                    .collect();
                if classes.is_empty() {
                    continue;
                }
                // Their own callsign, once they have given one (§13.7). Until then, the
                // callsign of that day, as the event's own header has it.
                let callsign = match (
                    &mine.callsign,
                    shown.header.pilots.iter().find(|p| &p.id == pilot),
                ) {
                    (Some(callsign), _) => callsign.clone(),
                    (None, Some(found)) => found.callsign.clone(),
                    (None, None) => state
                        .accounts
                        .club_pilot(&club_id, &pilot.0)?
                        .map_or_else(|| "Pilot".to_owned(), |found| found.callsign),
                };
                results.push(MyResult {
                    club: club.clone(),
                    event: server_event(
                        event.clone(),
                        shown.header.meta.clone(),
                        state.book.event_note(&club_id, &event.0).hidden,
                    ),
                    callsign,
                    classes,
                });
            }
        }
    }
    results.sort_by(|a, b| {
        b.event
            .created_at
            .cmp(&a.event.created_at)
            .then_with(|| a.event.id.cmp(&b.event.id))
            .then_with(|| a.callsign.cmp(&b.callsign))
    });
    Ok(Json(results))
}

// ── For a Director ──────────────────────────────────────────────────────────

fn pilot_id(id: String) -> Result<String, ApiError> {
    if valid_pilot_id(&id) {
        Ok(id)
    } else {
        Err(ApiError::bad("That is not a pilot's id."))
    }
}

/// `GET /push/pilots`: the club's pilots, each with the account that holds it.
pub async fn get_push_pilots(
    State(state): State<AppState>,
    Pusher(club): Pusher,
) -> Result<Json<Vec<PushPilot>>, ApiError> {
    let claims = state.accounts.claims_in(&club.id)?;
    let profiles = state.accounts.profiles()?;
    let faces = Faces::of(&state, &club.id)?;
    Ok(Json(
        state
            .accounts
            .club_pilots(&club.id)?
            .into_iter()
            .map(|pilot| PushPilot {
                account: claims
                    .iter()
                    .find(|claim| claim.pilot == pilot.pilot && claim.how.holds())
                    .map(|claim| account_with_profile(&claim.account, &profiles)),
                callsign: faces.callsign(&pilot.pilot, pilot.callsign),
                color: faces.color(&pilot.pilot, pilot.color),
                id: PilotId(pilot.pilot),
            })
            .collect(),
    ))
}

/// `PUT /push/pilots`: the pilots a Director has. They are added to what the club knows;
/// nothing is removed, and who holds a pilot is not changed by this.
pub async fn put_push_pilots(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Json(pilots): Json<Vec<PushPilot>>,
) -> Result<StatusCode, ApiError> {
    let pilots: Vec<ClubPilot> = pilots
        .into_iter()
        .map(|pilot| ClubPilot {
            pilot: pilot.id.0,
            callsign: pilot.callsign,
            color: pilot.color,
        })
        .collect();
    state.accounts.note_pilots(&club.id, &pilots, now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /push/members`: the club's members, by name.
pub async fn get_push_members(
    State(state): State<AppState>,
    Pusher(club): Pusher,
) -> Result<Json<Vec<PushAccount>>, ApiError> {
    let profiles = state.accounts.profiles()?;
    Ok(Json(
        state
            .accounts
            .members(&club.id)?
            .iter()
            .map(|(account, _)| account_with_profile(account, &profiles))
            .collect(),
    ))
}

/// `PUT /push/pilots/{pilot}/account`: the director says which member holds a pilot.
pub async fn put_push_account(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(pilot): Path<String>,
    Json(change): Json<PushPilotAccount>,
) -> Result<StatusCode, ApiError> {
    let pilot = pilot_id(pilot)?;
    state
        .accounts
        .give_pilot(&club.id, &pilot, &AccountId(change.account), None, now_ms())?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /push/pilots/{pilot}/account`. Taking it from nobody is fine: the Director
/// wants it free, and it is.
pub async fn delete_push_account(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(pilot): Path<String>,
) -> Result<StatusCode, ApiError> {
    let pilot = pilot_id(pilot)?;
    state.accounts.free_pilot(&club.id, &pilot)?;
    Ok(StatusCode::NO_CONTENT)
}
