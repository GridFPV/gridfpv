//! Pilots and the accounts that hold them on the linked server (docs/primitives.md §13.7):
//! what the Director's Pilots page asks for.
//!
//! | route | what |
//! |-------|------|
//! | `GET /server-link/people` | the club's members and its pilots, as the server has them |
//! | `PUT /pilots/{id}/server-account` | say which member holds a pilot, or that nobody does |
//! | `POST /server-link/pull` | bring pilots in from the server, already tied to their accounts |
//!
//! **The server is where this is decided.** Each of these asks the server there and then,
//! and changes nothing here unless the server agreed. So they need the server in reach; the
//! race does not. A pilot keeps a copy of who holds it ([`Pilot::server_account`]), and the
//! task that pushes events keeps that copy up with the server.
//!
//! All three are for the director only.

use axum::Json;
use axum::extract::{Path, State};
use gridfpv_events::PilotId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;
use crate::link_client::{Client, PushError};
use crate::pilots::{CreatePilotRequest, Pilot, PilotAccount, PilotError, PilotErrorKind};
use crate::remote::{PushAccount, PushPilot};

/// `GET /server-link/people`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkPeople {
    /// The club's members, by name.
    pub members: Vec<PushAccount>,
    /// The club's pilots as the server has them, each with the account that holds it.
    pub pilots: Vec<PushPilot>,
}

/// `PUT /pilots/{id}/server-account`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PilotAccountChange {
    /// The member's account id. Left out, or `null`, for nobody.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub account: Option<String>,
}

/// `POST /server-link/pull`: what to bring in from the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PullRequest {
    /// Pilots the club has on the server and this Director does not. Each comes in under
    /// the id it has there.
    #[serde(default)]
    pub pilots: Vec<PilotId>,
    /// Members who hold no pilot. A new pilot is made for each, named as the member is,
    /// and tied to them.
    #[serde(default)]
    pub members: Vec<String>,
}

impl From<PushError> for ProtocolError {
    fn from(e: PushError) -> Self {
        // Nothing was changed, and the words say why.
        ProtocolError::new(ErrorCode::Refused, e.to_string())
    }
}

fn pilot_error(error: PilotError) -> ProtocolError {
    let code = match error.kind {
        PilotErrorKind::Invalid => ErrorCode::BadRequest,
        PilotErrorKind::NotFound => ErrorCode::UnknownScope,
        PilotErrorKind::Internal => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// A client for the club the open event goes to, or why there is none to ask.
fn client(registry: &EventRegistry) -> Result<Client, ProtocolError> {
    let club = registry
        .active()
        .and_then(|event| registry.link().club_of(&event.id))
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::Refused,
                "The open event is not sent to a server. Pick a club for it on the Server page.",
            )
        })?;
    Ok(Client::new(&club)?)
}

/// A pilot as the server is told of it: what a visitor may see, and no more.
pub fn push_pilot(pilot: &Pilot) -> PushPilot {
    PushPilot {
        id: pilot.id.clone(),
        callsign: pilot.callsign.clone(),
        color: pilot.color.clone(),
        account: None,
    }
}

fn account(of: &PushAccount) -> PilotAccount {
    PilotAccount {
        id: of.id.clone(),
        name: of.name.clone(),
    }
}

/// `GET /server-link/people`.
pub async fn get_people(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
) -> Result<Json<LinkPeople>, ProtocolError> {
    let client = client(&registry)?;
    Ok(Json(LinkPeople {
        members: client.members().await?,
        pilots: client.pilots().await?,
    }))
}

/// `PUT /pilots/{id}/server-account`.
pub async fn put_account(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(id): Path<PilotId>,
    Json(change): Json<PilotAccountChange>,
) -> Result<Json<Pilot>, ProtocolError> {
    let pilots = registry.pilots();
    let pilot = pilots.get(&id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no pilot with id {:?}", id.0),
        )
    })?;
    let client = client(&registry)?;
    let held = match change.account.filter(|account| !account.is_empty()) {
        Some(wanted) => {
            // The name comes from the server's own list: it is what is shown for them.
            let member = client
                .members()
                .await?
                .into_iter()
                .find(|member| member.id == wanted)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::UnknownScope,
                        "That account is not a member of the club on the server.",
                    )
                })?;
            // The server may not have heard of this pilot yet.
            client.put_pilots(&[push_pilot(&pilot)]).await?;
            client.set_account(&id, Some(&member.id)).await?;
            // The account wins where it says something: the pilot takes its callsign,
            // colour and ids (§13.7).
            return Ok(Json(
                pilots
                    .tie_to_account(&id, account(&member), &member.profile)
                    .map_err(pilot_error)?,
            ));
        }
        None => {
            client.set_account(&id, None).await?;
            None
        }
    };
    Ok(Json(
        pilots.set_server_account(&id, held).map_err(pilot_error)?,
    ))
}

/// `POST /server-link/pull`. Returns the pilots that were added.
///
/// A pilot that is already here is left as it is. Asking for a pilot or a member the
/// server does not have refuses the whole request, before anything is added.
pub async fn post_pull(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(request): Json<PullRequest>,
) -> Result<Json<Vec<Pilot>>, ProtocolError> {
    let client = client(&registry)?;
    let pilots = registry.pilots();
    let theirs = client.pilots().await?;
    let members = client.members().await?;
    let gone = |what: &str| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("The server no longer has {what}. Look again."),
        )
    };
    let wanted_pilots = request
        .pilots
        .iter()
        .map(|id| {
            theirs
                .iter()
                .find(|pilot| &pilot.id == id)
                .ok_or_else(|| gone("one of those pilots"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let wanted_members = request
        .members
        .iter()
        .map(|id| {
            members
                .iter()
                .find(|member| &member.id == id)
                .ok_or_else(|| gone("one of those members"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut added = Vec::new();
    for theirs in wanted_pilots {
        if let Some(pilot) = pilots
            .adopt(
                theirs.id.clone(),
                &theirs.callsign,
                theirs.color.clone(),
                theirs.account.as_ref().map(account),
            )
            .map_err(pilot_error)?
        {
            // Held by an account: its ids come along too.
            let pilot = match &theirs.account {
                Some(holder) => pilots
                    .tie_to_account(&pilot.id, account(holder), &holder.profile)
                    .map_err(pilot_error)?,
                None => pilot,
            };
            added.push(pilot);
        }
    }
    for member in wanted_members {
        // Named as the member names themselves as a pilot, or else as their account.
        let callsign = member
            .profile
            .callsign
            .clone()
            .filter(|callsign| !callsign.trim().is_empty())
            .unwrap_or_else(|| member.name.clone());
        let pilot = pilots
            .create(&CreatePilotRequest {
                callsign,
                ..CreatePilotRequest::default()
            })
            .map_err(pilot_error)?;
        // A pilot is only left here once the server has tied it to the member.
        let tied = async {
            client.put_pilots(&[push_pilot(&pilot)]).await?;
            client.set_account(&pilot.id, Some(&member.id)).await
        };
        if let Err(why) = tied.await {
            pilots.delete(&pilot.id).ok();
            return Err(why.into());
        }
        added.push(
            pilots
                .tie_to_account(&pilot.id, account(member), &member.profile)
                .map_err(pilot_error)?,
        );
    }
    Ok(Json(added))
}
