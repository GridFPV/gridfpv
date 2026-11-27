//! The Director's links to remote servers (docs/primitives.md §13): the clubs it can send
//! to, which club each event goes to, and how the sending is going.
//!
//! This is the desk: what is set, kept in `server-link.json`, and what is reported. The
//! pushing itself is `gridfpv_app::link`, which reads the settings here and writes the
//! status here. The console reads and sets both through the routes below.
//!
//! - **A saved club** is a server's address and a link token for one club on it. The
//!   director keeps a list of them.
//! - **An event goes to one club, or to none.** A new event goes to none.
//! - **Only the open event's club is connected.** While it is, every event that goes to
//!   that club is kept up to date. An event of another club waits until one of that club's
//!   events is open (Ryan, 2026-11-27).
//!
//! | route | what |
//! |-------|------|
//! | `GET /server-link` | the saved clubs, each event's club, and how the sending stands |
//! | `POST /server-link/clubs` | save a club |
//! | `PUT /server-link/clubs/{club}` | change a saved club |
//! | `DELETE /server-link/clubs/{club}` | forget a saved club |
//! | `PUT /server-link/events/{id}/club` | say which club an event goes to, or none |
//! | `POST /server-link/events/{id}/replace` | replace the server's copy of an event |
//!
//! None of these is open to a visitor (`access`: an API route is closed until it is listed).
//!
//! # What is checked
//!
//! - **HTTPS only** (§13.5). The one exception is a server on this same computer
//!   (`http://localhost` or a loopback address): nothing leaves the machine, and it is how a
//!   server is tried out before it has a name.
//! - **A token is never given back.** The console is told only that one is set.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventId, EventMeta, EventRegistry};
use crate::remote::PushClub;

/// The file the links are kept in, under the data dir.
pub const LINK_FILE: &str = "server-link.json";

/// What every link token starts with (`gridfpv-serverd` makes them).
const TOKEN_PREFIX: &str = "gfs_";

/// The longest name a director may give a saved club.
const NAME_MAX: usize = 60;

/// Which certificates the Director takes as proof that a server is who it says (§13.5).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum LinkTrust {
    /// The ones this computer trusts already. Right for a server with a normal
    /// certificate, such as one from Let's Encrypt.
    #[default]
    System,
    /// Those, and the certificate authorities given here as PEM text. For a club that
    /// signs its own.
    ExtraAuthorities { pem: String },
}

/// One club the Director can send to, as it is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedClub {
    /// This Director's own handle for it. Not the club's id on the server.
    pub id: String,
    /// What the director calls it. Empty when they gave no name: then the club's own name
    /// on the server is shown.
    #[serde(default)]
    pub name: String,
    /// The server's address, such as `https://grid.example.org`.
    pub url: String,
    /// The link token.
    pub token: String,
    #[serde(default)]
    pub trust: LinkTrust,
    /// The club the token sends into, as the server last said. Kept so the club has its
    /// name, and an event its address there, while the server is out of reach.
    #[serde(default)]
    pub club: Option<PushClub>,
}

impl SavedClub {
    /// Whether `other` reaches the same server the same way: a connection made for one
    /// serves the other.
    pub fn same_wire(&self, other: &SavedClub) -> bool {
        self.id == other.id
            && self.url == other.url
            && self.token == other.token
            && self.trust == other.trust
    }
}

/// The links as they are kept.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinkStored {
    #[serde(default)]
    pub clubs: Vec<SavedClub>,
    /// Which saved club each event goes to. An event not here goes to none.
    #[serde(default)]
    pub events: BTreeMap<EventId, String>,
}

/// `POST /server-link/clubs` and `PUT /server-link/clubs/{club}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClubSettings {
    /// What the director calls it. May be empty.
    #[serde(default)]
    pub name: String,
    pub url: String,
    /// The link token. On a change it may be left out, which keeps the one already set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token: Option<String>,
    #[serde(default)]
    pub trust: LinkTrust,
}

/// `PUT /server-link/events/{id}/club`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventClub {
    /// The saved club's id. Left out, or `null`, for none: the event is not sent.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub club: Option<String>,
}

/// How the connection for the open event stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum LinkState {
    /// No event is open, or the open event goes to no club. Nothing is connected.
    #[default]
    NotSet,
    /// Trying to reach the server.
    Connecting,
    /// The server answered and took the token.
    Connected,
    /// The server could not be reached, or did not take the token. It is tried again.
    Failed { message: String },
}

/// How one event stands on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum LinkEventState {
    /// The event goes to no club. None of it is sent.
    NoClub,
    /// The event's club is not the one connected now. It is sent when an event of its
    /// club is open.
    Waiting,
    /// The server holds all of it.
    Sent,
    /// Some of it is still to go.
    Sending,
    /// The server holds a different event under this id. Nothing is sent until the
    /// director says to replace the server's copy.
    Different,
    /// The event was removed on the server by someone who runs the club. Nothing is sent
    /// until the director says to send it again.
    Removed,
    /// The last try failed. It is tried again.
    Failed { message: String },
}

/// One event, where it goes, and how much of it the server holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkEvent {
    pub id: EventId,
    pub name: String,
    /// The saved club it goes to, by [`LinkClub::id`]. Left out when it goes to none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub club: Option<String>,
    /// How many entries the event's log holds here.
    #[ts(type = "number")]
    pub len: u64,
    /// How many of them the server holds.
    #[ts(type = "number")]
    pub sent: u64,
    pub state: LinkEventState,
}

/// One saved club, for the console. The token is never in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkClub {
    pub id: String,
    /// What the director calls it. Empty when they gave no name.
    pub name: String,
    pub url: String,
    pub trust: LinkTrust,
    /// The club on the server, as it last said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub club: Option<PushClub>,
}

/// `GET /server-link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkView {
    pub clubs: Vec<LinkClub>,
    /// The open event, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub open: Option<EventId>,
    /// The saved club the open event goes to, by [`LinkClub::id`]. It is the one club the
    /// Director connects to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub club: Option<String>,
    pub state: LinkState,
    /// The connected server's build, once it has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server_version: Option<String>,
    /// Every event the Director has, in the order it lists them.
    pub events: Vec<LinkEvent>,
}

/// Why settings were not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("That is not a web address. It should look like https://grid.example.org")]
    NotAnAddress,
    #[error(
        "The address must start with https://. Only a server on this same computer may use http://"
    )]
    NotHttps,
    #[error("Give the server's address only, with no path, question mark or sign-in in it.")]
    MoreThanAnAddress,
    #[error("That is not a link token. One starts with gfs_ and is made on the server.")]
    NotAToken,
    #[error("That is not a certificate. It should hold a -----BEGIN CERTIFICATE----- block.")]
    NotACertificate,
    #[error("Give the server's address.")]
    NoAddress,
    #[error("Give the link token. It is made on the server, for your club.")]
    NoToken,
    #[error("That name is too long. Keep it to 60 letters.")]
    NameTooLong,
    #[error("That club is not saved here any more. Look again.")]
    NoSuchClub,
    #[error("The settings could not be saved: {0}")]
    Io(String),
}

#[derive(Default)]
struct Status {
    /// The saved club this status is about: the one last connected to.
    for_club: Option<String>,
    state: LinkState,
    server_version: Option<String>,
    events: BTreeMap<EventId, LinkEvent>,
    /// Events whose copy on the server the director said to replace.
    replace: Vec<EventId>,
}

struct Inner {
    stored: LinkStored,
    status: Status,
    data_dir: Option<PathBuf>,
}

/// The links' settings and status. Cloning shares the one desk.
#[derive(Clone)]
pub struct LinkDesk {
    inner: Arc<RwLock<Inner>>,
    /// Counts up each time the settings change or a replace is asked for, so the pusher
    /// knows to look again at once.
    changed: watch::Sender<u64>,
}

/// The address a server is reached at, tidied: scheme and host (and port), no slash after.
pub(crate) fn clean_url(text: &str) -> Result<String, LinkError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(String::new());
    }
    let url = reqwest::Url::parse(text).map_err(|_| LinkError::NotAnAddress)?;
    let host = url.host_str().ok_or(LinkError::NotAnAddress)?;
    match url.scheme() {
        "https" => {}
        "http" if crate::access::is_loopback_host(host) => {}
        "http" => return Err(LinkError::NotHttps),
        _ => return Err(LinkError::NotAnAddress),
    }
    if !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(LinkError::MoreThanAnAddress);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub(crate) fn clean_trust(trust: LinkTrust) -> Result<LinkTrust, LinkError> {
    match trust {
        LinkTrust::System => Ok(LinkTrust::System),
        LinkTrust::ExtraAuthorities { pem } => {
            let pem = pem.trim().to_owned();
            if pem.contains("-----BEGIN CERTIFICATE-----")
                && pem.contains("-----END CERTIFICATE-----")
            {
                Ok(LinkTrust::ExtraAuthorities { pem })
            } else {
                Err(LinkError::NotACertificate)
            }
        }
    }
}

/// A token as typed, tidied. Empty stays empty.
fn clean_token(token: &str) -> Result<String, LinkError> {
    let token = token.trim().to_owned();
    if !token.is_empty()
        && (!token.starts_with(TOKEN_PREFIX)
            || token.len() <= TOKEN_PREFIX.len()
            || token.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err(LinkError::NotAToken);
    }
    Ok(token)
}

/// A handle for a new saved club: one no club in `taken` has.
fn new_id(taken: &[SavedClub]) -> String {
    loop {
        let mut bytes = [0u8; 4];
        // The OS always has random bytes. A handle only has to differ from the others.
        getrandom::fill(&mut bytes).ok();
        let id = format!("k{:08x}", u32::from_le_bytes(bytes));
        if taken.iter().all(|club| club.id != id) {
            return id;
        }
    }
}

impl LinkDesk {
    /// The desk kept under `data_dir`. A file that cannot be read gives no saved clubs,
    /// which sends nothing.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let stored: LinkStored = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(LINK_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                stored,
                status: Status::default(),
                data_dir,
            })),
            changed: watch::Sender::new(0),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The links as kept, tokens and all. For the pusher, not for the wire.
    pub fn stored(&self) -> LinkStored {
        self.read().stored.clone()
    }

    /// Told each time the settings change or a replace is asked for.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Keep `next`, on disk first. Nothing is changed when it cannot be written.
    fn keep(inner: &mut Inner, next: LinkStored) -> Result<(), LinkError> {
        if let Some(dir) = &inner.data_dir {
            write_private(&dir.join(LINK_FILE), &next).map_err(LinkError::Io)?;
        }
        inner.stored = next;
        Ok(())
    }

    /// Save a club: a new one when `id` is `None`, else a change to that one. Nothing is
    /// kept unless all of it is right. Returns the club's id.
    pub fn save_club(&self, id: Option<&str>, settings: ClubSettings) -> Result<String, LinkError> {
        let url = clean_url(&settings.url)?;
        if url.is_empty() {
            return Err(LinkError::NoAddress);
        }
        let trust = clean_trust(settings.trust)?;
        let name = settings.name.trim().to_owned();
        if name.chars().count() > NAME_MAX {
            return Err(LinkError::NameTooLong);
        }
        let token = settings.token.as_deref().map(clean_token).transpose()?;
        let id = {
            let mut inner = self.write();
            let mut next = inner.stored.clone();
            let mut forget = false;
            let id = match id {
                None => {
                    let token = token.filter(|t| !t.is_empty()).ok_or(LinkError::NoToken)?;
                    let id = new_id(&next.clubs);
                    next.clubs.push(SavedClub {
                        id: id.clone(),
                        name,
                        url,
                        token,
                        trust,
                        club: None,
                    });
                    id
                }
                Some(id) => {
                    let club = next
                        .clubs
                        .iter_mut()
                        .find(|club| club.id == id)
                        .ok_or(LinkError::NoSuchClub)?;
                    // Another server is another token: the old one is not sent to a new
                    // address.
                    let token = match token.filter(|t| !t.is_empty()) {
                        Some(token) => token,
                        None if club.url == url => club.token.clone(),
                        None => return Err(LinkError::NoToken),
                    };
                    if club.url != url || club.token != token {
                        // What the server said was about another server, or another club.
                        club.club = None;
                        forget = true;
                    }
                    club.name = name;
                    club.url = url;
                    club.token = token;
                    club.trust = trust;
                    id.to_owned()
                }
            };
            Self::keep(&mut inner, next)?;
            if forget && inner.status.for_club.as_deref() == Some(id.as_str()) {
                let replace = std::mem::take(&mut inner.status.replace);
                inner.status = Status {
                    replace,
                    ..Status::default()
                };
            }
            id
        };
        self.changed.send_modify(|n| *n += 1);
        Ok(id)
    }

    /// Forget a saved club. Its events go to none. What the server holds stays there.
    pub fn remove_club(&self, id: &str) -> Result<(), LinkError> {
        {
            let mut inner = self.write();
            let mut next = inner.stored.clone();
            let before = next.clubs.len();
            next.clubs.retain(|club| club.id != id);
            if next.clubs.len() == before {
                return Err(LinkError::NoSuchClub);
            }
            next.events.retain(|_, club| club != id);
            Self::keep(&mut inner, next)?;
        }
        self.changed.send_modify(|n| *n += 1);
        Ok(())
    }

    /// Say which saved club an event goes to, or none.
    pub fn set_event_club(&self, event: &EventId, club: Option<&str>) -> Result<(), LinkError> {
        {
            let mut inner = self.write();
            let mut next = inner.stored.clone();
            match club {
                Some(club) => {
                    if next.clubs.iter().all(|saved| saved.id != club) {
                        return Err(LinkError::NoSuchClub);
                    }
                    next.events.insert(event.clone(), club.to_owned());
                }
                None => {
                    next.events.remove(event);
                }
            }
            if next != inner.stored {
                Self::keep(&mut inner, next)?;
            }
        }
        self.changed.send_modify(|n| *n += 1);
        Ok(())
    }

    /// An event is gone: it goes nowhere any more.
    pub fn forget_event(&self, event: &EventId) {
        let mut inner = self.write();
        if inner.stored.events.contains_key(event) {
            let mut next = inner.stored.clone();
            next.events.remove(event);
            // The file is tidied when it can be. A name left in it sends nothing.
            Self::keep(&mut inner, next).ok();
        }
        inner.status.events.remove(event);
        inner.status.replace.retain(|id| id != event);
    }

    /// The id of the saved club an event goes to.
    pub fn club_id_of(&self, event: &EventId) -> Option<String> {
        let inner = self.read();
        let id = inner.stored.events.get(event)?;
        inner
            .stored
            .clubs
            .iter()
            .any(|club| &club.id == id)
            .then(|| id.clone())
    }

    /// The saved club an event goes to, token and all.
    pub fn club_of(&self, event: &EventId) -> Option<SavedClub> {
        let inner = self.read();
        let id = inner.stored.events.get(event)?;
        inner
            .stored
            .clubs
            .iter()
            .find(|club| &club.id == id)
            .cloned()
    }

    /// The links and status, for the console. `events` is every event the Director has,
    /// and `open` the one that is open.
    pub fn view(&self, events: &[EventMeta], open: Option<&EventId>) -> LinkView {
        let inner = self.read();
        let stored = &inner.stored;
        let club_of = |event: &EventId| {
            stored
                .events
                .get(event)
                .filter(|id| stored.clubs.iter().any(|club| &club.id == *id))
                .cloned()
        };
        let club = open.and_then(club_of);
        // What the pusher said is about the club it last connected to.
        let live = club.is_some() && inner.status.for_club == club;
        let state = match &club {
            None => LinkState::NotSet,
            Some(_) if !live => LinkState::Connecting,
            Some(_) => match &inner.status.state {
                // Picked a moment ago, and the pusher has not said yet.
                LinkState::NotSet => LinkState::Connecting,
                other => other.clone(),
            },
        };
        LinkView {
            clubs: stored
                .clubs
                .iter()
                .map(|saved| LinkClub {
                    id: saved.id.clone(),
                    name: saved.name.clone(),
                    url: saved.url.clone(),
                    trust: saved.trust.clone(),
                    club: saved.club.clone(),
                })
                .collect(),
            open: open.cloned(),
            server_version: inner.status.server_version.clone().filter(|_| live),
            events: events
                .iter()
                .map(|meta| {
                    let goes_to = club_of(&meta.id);
                    // What was last heard of the club the pusher last reached. It stays
                    // true of that club's events until another club is reached.
                    let known = (goes_to.is_some() && inner.status.for_club == goes_to)
                        .then(|| inner.status.events.get(&meta.id))
                        .flatten();
                    match known {
                        Some(known) => LinkEvent {
                            name: meta.name.clone(),
                            club: goes_to,
                            ..known.clone()
                        },
                        None => LinkEvent {
                            id: meta.id.clone(),
                            name: meta.name.clone(),
                            state: if goes_to.is_some() {
                                LinkEventState::Waiting
                            } else {
                                LinkEventState::NoClub
                            },
                            club: goes_to,
                            len: 0,
                            sent: 0,
                        },
                    }
                })
                .collect(),
            club,
            state,
        }
    }

    // ── What the pusher reports ─────────────────────────────────────────────

    /// The pusher is about to reach for `club`. What was known of another club, or of this
    /// one reached another way, is dropped.
    pub fn begin(&self, club: &SavedClub) {
        let mut inner = self.write();
        if inner.status.for_club.as_deref() != Some(&club.id) {
            let replace = std::mem::take(&mut inner.status.replace);
            inner.status = Status {
                for_club: Some(club.id.clone()),
                replace,
                ..Status::default()
            };
        }
        inner.status.state = LinkState::Connecting;
    }

    /// How the connection stands.
    pub fn report(&self, state: LinkState) {
        self.write().status.state = state;
    }

    /// What the server said of itself. The club is remembered with the saved club it was
    /// reached through.
    pub fn report_server(&self, club: PushClub, version: String) {
        let mut inner = self.write();
        inner.status.server_version = Some(version);
        let Some(id) = inner.status.for_club.clone() else {
            return;
        };
        let mut next = inner.stored.clone();
        if let Some(saved) = next.clubs.iter_mut().find(|saved| saved.id == id)
            && saved.club.as_ref() != Some(&club)
        {
            saved.club = Some(club);
            // Kept when it can be. It is said again at the next connection anyway.
            Self::keep(&mut inner, next).ok();
        }
    }

    /// How one event stands.
    pub fn report_event(&self, event: LinkEvent) {
        self.write().status.events.insert(event.id.clone(), event);
    }

    /// Keep status only for these events: the ones that still exist and go to the
    /// connected club.
    pub fn keep_events(&self, existing: &[EventId]) {
        self.write()
            .status
            .events
            .retain(|id, _| existing.contains(id));
    }

    /// The director says to replace the server's copy of an event.
    pub fn ask_replace(&self, event: EventId) {
        {
            let mut inner = self.write();
            if !inner.status.replace.contains(&event) {
                inner.status.replace.push(event);
            }
        }
        self.changed.send_modify(|n| *n += 1);
    }

    /// Whether a replace was asked for. It is asked once: this takes it.
    pub fn take_replace(&self, event: &EventId) -> bool {
        let mut inner = self.write();
        let before = inner.status.replace.len();
        inner.status.replace.retain(|id| id != event);
        inner.status.replace.len() != before
    }
}

/// Write the settings where only this user can read them: the file holds the token.
fn write_private(path: &std::path::Path, stored: &LinkStored) -> Result<(), String> {
    let text = serde_json::to_string_pretty(stored).map_err(|e| e.to_string())?;
    let part = path.with_extension("json.part");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&part).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&part, path).map_err(|e| e.to_string())
}

impl From<LinkError> for ProtocolError {
    fn from(e: LinkError) -> Self {
        let code = match e {
            LinkError::Io(_) => ErrorCode::Internal,
            LinkError::NoSuchClub => ErrorCode::UnknownScope,
            _ => ErrorCode::BadRequest,
        };
        ProtocolError::new(code, e.to_string())
    }
}

/// An event's header as it is sent to a server: its name and date, and the pilots, classes,
/// timers and channels its log refers to.
///
/// **The lists hold only what a visitor at the race may see.** They are the same lists the
/// Public door gives (`spectate`): a callsign and a colour, not a real name; a timer's name
/// and seats, not its address. The event's own details all go: its name, date, place,
/// description and organizer. The log itself is sent whole (§13.3).
pub async fn header(
    registry: &EventRegistry,
    id: &EventId,
) -> Result<crate::remote::PushHeader, ProtocolError> {
    use crate::spectate;
    let meta = registry.meta_of(id).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", id.0),
        )
    })?;
    // Every field is named, so one added later is not sent until someone decides here.
    // All of an event's own details go up: what a server shows an event by (Ryan,
    // 2026-11-27).
    let crate::events::EventMeta {
        id: meta_id,
        name,
        created_at,
        persistent,
        date,
        location,
        description,
        organizer,
    } = meta;
    let meta = crate::events::EventMeta {
        id: meta_id,
        name,
        created_at,
        persistent,
        date,
        location,
        description,
        organizer,
    };
    let view = crate::control::with_event(registry, id, |session| {
        Ok(crate::view::EventView::build(session.state(), session.now))
    })?;
    let wanted = spectate::mentioned(&view);
    let mut channels = Vec::new();
    for timer in &wanted.timers {
        channels.extend(crate::timers::seat_channels(registry, timer).await);
    }
    Ok(crate::remote::PushHeader {
        meta,
        pilots: registry
            .pilots()
            .list()
            .into_iter()
            .filter(|pilot| wanted.pilots.contains(&pilot.id))
            .map(spectate::pilot_for_visitors)
            .collect(),
        classes: registry
            .classes()
            .list()
            .into_iter()
            .filter(|class| wanted.classes.contains(&class.id))
            .map(spectate::class_for_visitors)
            .collect(),
        timers: registry
            .timers()
            .list()
            .into_iter()
            .filter(|view| wanted.timers.contains(&view.timer.id))
            .map(spectate::timer_for_visitors)
            .collect(),
        channels,
    })
}

/// The links as the console sees them now.
fn view_of(registry: &EventRegistry) -> LinkView {
    let open = registry.active().map(|event| event.id);
    registry.link().view(&registry.list(), open.as_ref())
}

fn unknown_event(id: &EventId) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::UnknownScope,
        format!("no event with id {:?}", id.0),
    )
}

/// `GET /server-link`.
pub async fn get_link(State(registry): State<EventRegistry>) -> Json<LinkView> {
    Json(view_of(&registry))
}

/// `POST /server-link/clubs`.
pub async fn post_club(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(settings): Json<ClubSettings>,
) -> Result<Json<LinkView>, ProtocolError> {
    registry.link().save_club(None, settings)?;
    Ok(Json(view_of(&registry)))
}

/// `PUT /server-link/clubs/{club}`.
pub async fn put_club(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(club): Path<String>,
    Json(settings): Json<ClubSettings>,
) -> Result<Json<LinkView>, ProtocolError> {
    registry.link().save_club(Some(&club), settings)?;
    Ok(Json(view_of(&registry)))
}

/// `DELETE /server-link/clubs/{club}`.
pub async fn delete_club(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(club): Path<String>,
) -> Result<Json<LinkView>, ProtocolError> {
    registry.link().remove_club(&club)?;
    Ok(Json(view_of(&registry)))
}

/// `PUT /server-link/events/{id}/club`.
pub async fn put_event_club(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(id): Path<EventId>,
    Json(change): Json<EventClub>,
) -> Result<Json<LinkView>, ProtocolError> {
    if registry.meta_of(&id).is_none() {
        return Err(unknown_event(&id));
    }
    let club = change.club.filter(|club| !club.is_empty());
    registry.link().set_event_club(&id, club.as_deref())?;
    Ok(Json(view_of(&registry)))
}

/// `POST /server-link/events/{id}/replace`.
pub async fn post_replace(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(id): Path<EventId>,
) -> Result<Json<LinkView>, ProtocolError> {
    if registry.meta_of(&id).is_none() {
        return Err(unknown_event(&id));
    }
    registry.link().ask_replace(id);
    Ok(Json(view_of(&registry)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "gfs_abcDEF123-_";

    fn settings(url: &str, token: Option<&str>) -> ClubSettings {
        ClubSettings {
            name: String::new(),
            url: url.to_owned(),
            token: token.map(str::to_owned),
            trust: LinkTrust::System,
        }
    }

    fn meta(id: &str) -> EventMeta {
        EventMeta {
            id: EventId(id.into()),
            name: format!("Event {id}"),
            created_at: 0,
            persistent: true,
            date: None,
            location: None,
            description: None,
            organizer: None,
        }
    }

    fn id(event: &str) -> EventId {
        EventId(event.into())
    }

    fn temp() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-link-{nanos}-{:?}",
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A desk with one saved club.
    fn desk_with_a_club() -> (LinkDesk, String) {
        let desk = LinkDesk::new(None);
        let club = desk
            .save_club(None, settings("https://a.example", Some(TOKEN)))
            .unwrap();
        (desk, club)
    }

    fn sent(event: &str, club: &str) -> LinkEvent {
        LinkEvent {
            id: id(event),
            name: event.into(),
            club: Some(club.into()),
            len: 10,
            sent: 10,
            state: LinkEventState::Sent,
        }
    }

    #[test]
    fn a_new_desk_has_no_clubs_and_sends_nothing() {
        let desk = LinkDesk::new(None);
        assert_eq!(desk.stored(), LinkStored::default());
        let view = desk.view(&[meta("e")], Some(&id("e")));
        assert_eq!(view.state, LinkState::NotSet);
        assert!(view.clubs.is_empty() && view.club.is_none());
        assert_eq!(view.open, Some(id("e")));
        assert_eq!(view.events[0].state, LinkEventState::NoClub);
        assert!(desk.club_of(&id("e")).is_none());
    }

    #[test]
    fn addresses_are_tidied_and_only_https_leaves_the_computer() {
        let (desk, club) = desk_with_a_club();
        for (given, kept) in [
            ("https://grid.example.org", "https://grid.example.org"),
            ("  https://grid.example.org/  ", "https://grid.example.org"),
            ("HTTPS://Grid.Example.org", "https://grid.example.org"),
            (
                "https://grid.example.org:8443",
                "https://grid.example.org:8443",
            ),
            ("http://localhost:8470", "http://localhost:8470"),
            ("http://127.0.0.1:8470/", "http://127.0.0.1:8470"),
            ("http://[::1]:8470", "http://[::1]:8470"),
        ] {
            desk.save_club(Some(&club), settings(given, Some(TOKEN)))
                .unwrap();
            assert_eq!(desk.stored().clubs[0].url, kept, "{given:?}");
        }
        let before = desk.stored();
        for (given, why) in [
            ("", LinkError::NoAddress),
            ("   ", LinkError::NoAddress),
            ("http://grid.example.org", LinkError::NotHttps),
            ("http://192.168.1.5:8470", LinkError::NotHttps),
            ("http://localhost.example.org", LinkError::NotHttps),
            ("grid.example.org", LinkError::NotAnAddress),
            ("ftp://grid.example.org", LinkError::NotAnAddress),
            ("https://", LinkError::NotAnAddress),
            ("not an address", LinkError::NotAnAddress),
            ("https://grid.example.org/api", LinkError::MoreThanAnAddress),
            (
                "https://grid.example.org/?a=1",
                LinkError::MoreThanAnAddress,
            ),
            ("https://grid.example.org/#x", LinkError::MoreThanAnAddress),
            (
                "https://user:pw@grid.example.org",
                LinkError::MoreThanAnAddress,
            ),
            (
                "https://user@grid.example.org",
                LinkError::MoreThanAnAddress,
            ),
        ] {
            for which in [None, Some(club.as_str())] {
                assert_eq!(
                    desk.save_club(which, settings(given, Some(TOKEN))),
                    Err(why.clone()),
                    "{given:?}"
                );
            }
        }
        // A refused one changed nothing.
        assert_eq!(desk.stored(), before);
    }

    #[test]
    fn a_new_club_needs_a_token_and_a_changed_one_keeps_its_own() {
        let desk = LinkDesk::new(None);
        for none in [None, Some(""), Some("   ")] {
            assert_eq!(
                desk.save_club(None, settings("https://a.example", none)),
                Err(LinkError::NoToken)
            );
        }
        assert!(desk.stored().clubs.is_empty());

        let club = desk
            .save_club(
                None,
                settings("https://a.example", Some(&format!("  {TOKEN}\n"))),
            )
            .unwrap();
        assert_eq!(desk.stored().clubs[0].token, TOKEN);

        // Left out, or empty, on a change: the one already set is kept.
        for kept in [None, Some("")] {
            desk.save_club(Some(&club), settings("https://a.example", kept))
                .unwrap();
            assert_eq!(desk.stored().clubs[0].token, TOKEN);
        }
        // A new one replaces it.
        desk.save_club(
            Some(&club),
            settings("https://a.example", Some("gfs_another")),
        )
        .unwrap();
        assert_eq!(desk.stored().clubs[0].token, "gfs_another");

        // It is never shown.
        let shown = serde_json::to_string(&desk.view(&[], None)).unwrap();
        assert!(
            !shown.contains("gfs_") && !shown.contains("another"),
            "{shown}"
        );
    }

    #[test]
    fn a_token_is_not_carried_to_another_server() {
        let (desk, club) = desk_with_a_club();
        assert_eq!(
            desk.save_club(Some(&club), settings("https://b.example", None)),
            Err(LinkError::NoToken),
            "the old server's token is not sent to the new one"
        );
        assert_eq!(desk.stored().clubs[0].url, "https://a.example");
        // Given with the new address, it is kept.
        desk.save_club(Some(&club), settings("https://c.example", Some(TOKEN)))
            .unwrap();
        assert_eq!(desk.stored().clubs[0].url, "https://c.example");
    }

    #[test]
    fn what_is_not_a_token_is_refused() {
        let desk = LinkDesk::new(None);
        for bad in [
            "hunter2",
            "gfs_",
            "GFS_abc",
            "gfs_has space",
            "gfs_two\nlines",
            "K7QM-2XWD-9HRA",
        ] {
            assert_eq!(
                desk.save_club(None, settings("https://a.example", Some(bad))),
                Err(LinkError::NotAToken),
                "{bad:?}"
            );
        }
        assert_eq!(desk.stored(), LinkStored::default());
    }

    #[test]
    fn extra_authorities_must_be_certificates() {
        let (desk, club) = desk_with_a_club();
        let pem = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        let mut s = settings("https://a.example", None);
        s.trust = LinkTrust::ExtraAuthorities {
            pem: format!("\n{pem}\n\n"),
        };
        desk.save_club(Some(&club), s.clone()).unwrap();
        assert_eq!(
            desk.stored().clubs[0].trust,
            LinkTrust::ExtraAuthorities {
                pem: pem.to_owned()
            }
        );
        for bad in [
            "",
            "hello",
            "-----BEGIN CERTIFICATE-----",
            "-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----",
        ] {
            s.trust = LinkTrust::ExtraAuthorities {
                pem: bad.to_owned(),
            };
            assert_eq!(
                desk.save_club(Some(&club), s.clone()),
                Err(LinkError::NotACertificate),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_name_is_tidied_and_may_be_left_out() {
        let (desk, club) = desk_with_a_club();
        assert_eq!(desk.stored().clubs[0].name, "");
        let mut s = settings("https://a.example", None);
        s.name = "  Recon FPV ".into();
        desk.save_club(Some(&club), s.clone()).unwrap();
        assert_eq!(desk.view(&[], None).clubs[0].name, "Recon FPV");
        s.name = "x".repeat(NAME_MAX + 1);
        assert_eq!(
            desk.save_club(Some(&club), s.clone()),
            Err(LinkError::NameTooLong)
        );
        s.name = "é".repeat(NAME_MAX);
        desk.save_club(Some(&club), s).unwrap();
    }

    #[test]
    fn many_clubs_are_kept_each_under_its_own_id() {
        let desk = LinkDesk::new(None);
        let mut ids = Vec::new();
        for n in 0..20 {
            ids.push(
                desk.save_club(
                    None,
                    settings(&format!("https://s{n}.example"), Some(TOKEN)),
                )
                .unwrap(),
            );
        }
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 20);
        // In the order they were saved.
        let kept: Vec<String> = desk.stored().clubs.into_iter().map(|c| c.id).collect();
        assert_eq!(kept, ids);
        // Two clubs on one server are two saved clubs: same address, another token.
        let twin = desk
            .save_club(None, settings("https://s0.example", Some("gfs_other")))
            .unwrap();
        assert!(!ids.contains(&twin));
        assert_eq!(
            desk.save_club(Some("nope"), settings("https://a.example", Some(TOKEN))),
            Err(LinkError::NoSuchClub)
        );
    }

    #[test]
    fn an_event_goes_to_one_club_or_to_none() {
        let (desk, a) = desk_with_a_club();
        let b = desk
            .save_club(None, settings("https://b.example", Some(TOKEN)))
            .unwrap();
        let events = [meta("x"), meta("y"), meta("z")];

        assert_eq!(
            desk.set_event_club(&id("x"), Some("nope")),
            Err(LinkError::NoSuchClub)
        );
        desk.set_event_club(&id("x"), Some(&a)).unwrap();
        desk.set_event_club(&id("y"), Some(&b)).unwrap();
        assert_eq!(desk.club_of(&id("x")).unwrap().url, "https://a.example");
        assert_eq!(desk.club_id_of(&id("y")), Some(b.clone()));
        assert_eq!(desk.club_id_of(&id("z")), None);

        // With x open: its club is the one connected to. y waits, z goes nowhere.
        let view = desk.view(&events, Some(&id("x")));
        assert_eq!(view.club.as_deref(), Some(a.as_str()));
        assert_eq!(view.state, LinkState::Connecting);
        let states: Vec<_> = view
            .events
            .iter()
            .map(|e| (e.club.clone(), e.state.clone()))
            .collect();
        assert_eq!(
            states,
            [
                (Some(a.clone()), LinkEventState::Waiting),
                (Some(b.clone()), LinkEventState::Waiting),
                (None, LinkEventState::NoClub),
            ]
        );
        // With z open, or nothing open, nothing is connected.
        assert_eq!(desk.view(&events, Some(&id("z"))).state, LinkState::NotSet);
        assert_eq!(desk.view(&events, None).state, LinkState::NotSet);

        // Moved to the other club: one club at a time.
        desk.set_event_club(&id("x"), Some(&b)).unwrap();
        assert_eq!(desk.club_id_of(&id("x")), Some(b.clone()));
        // And to none.
        desk.set_event_club(&id("x"), None).unwrap();
        assert_eq!(desk.club_id_of(&id("x")), None);
        desk.set_event_club(&id("x"), None).unwrap();
    }

    #[test]
    fn forgetting_a_club_leaves_its_events_going_nowhere() {
        let (desk, a) = desk_with_a_club();
        let b = desk
            .save_club(None, settings("https://b.example", Some(TOKEN)))
            .unwrap();
        desk.set_event_club(&id("x"), Some(&a)).unwrap();
        desk.set_event_club(&id("y"), Some(&b)).unwrap();

        desk.remove_club(&a).unwrap();
        assert_eq!(desk.remove_club(&a), Err(LinkError::NoSuchClub));
        assert_eq!(desk.club_id_of(&id("x")), None);
        assert_eq!(desk.club_id_of(&id("y")), Some(b.clone()));
        let stored = desk.stored();
        assert_eq!(stored.clubs.len(), 1);
        assert_eq!(stored.events.len(), 1);
    }

    #[test]
    fn an_event_that_is_gone_goes_nowhere() {
        let (desk, a) = desk_with_a_club();
        desk.set_event_club(&id("x"), Some(&a)).unwrap();
        desk.begin(&desk.club_of(&id("x")).unwrap());
        desk.report_event(sent("x", &a));
        desk.ask_replace(id("x"));
        desk.forget_event(&id("x"));
        assert!(desk.stored().events.is_empty());
        assert!(!desk.take_replace(&id("x")));
        // An event made later under the same id starts clean.
        assert_eq!(
            desk.view(&[meta("x")], Some(&id("x"))).events[0].state,
            LinkEventState::NoClub
        );
        // Forgetting one that goes nowhere is nothing.
        desk.forget_event(&id("never"));
    }

    #[test]
    fn the_state_shown_follows_the_open_event_and_then_the_pusher() {
        let (desk, a) = desk_with_a_club();
        let b = desk
            .save_club(None, settings("https://b.example", Some(TOKEN)))
            .unwrap();
        let events = [meta("x"), meta("x2"), meta("y")];
        desk.set_event_club(&id("x"), Some(&a)).unwrap();
        desk.set_event_club(&id("x2"), Some(&a)).unwrap();
        desk.set_event_club(&id("y"), Some(&b)).unwrap();
        let open_x = || desk.view(&events, Some(&id("x")));
        assert_eq!(open_x().state, LinkState::Connecting);

        // The pusher reaches club a.
        desk.begin(&desk.club_of(&id("x")).unwrap());
        assert_eq!(open_x().state, LinkState::Connecting);
        desk.report(LinkState::Connected);
        desk.report_server(
            PushClub {
                id: "club".into(),
                name: "Recon".into(),
            },
            "0.6.0".into(),
        );
        desk.report_event(sent("x", &a));
        let view = open_x();
        assert_eq!(view.state, LinkState::Connected);
        assert_eq!(view.server_version.as_deref(), Some("0.6.0"));
        // The server's name for the club is kept with the saved club.
        assert_eq!(view.clubs[0].club.as_ref().unwrap().name, "Recon");
        assert!(view.clubs[1].club.is_none());
        assert_eq!(view.events[0].state, LinkEventState::Sent);
        assert_eq!((view.events[0].len, view.events[0].sent), (10, 10));
        // The name shown is the event's own, as it is now.
        assert_eq!(view.events[0].name, "Event x");
        // Another event of the same club, not looked at yet. And one of another club.
        assert_eq!(view.events[1].state, LinkEventState::Waiting);
        assert_eq!(view.events[2].state, LinkEventState::Waiting);

        // The director opens y, of club b. Until the pusher gets there it is connecting,
        // and what is shown of a's events is what was last heard.
        let view = desk.view(&events, Some(&id("y")));
        assert_eq!(view.state, LinkState::Connecting);
        assert_eq!(view.club.as_deref(), Some(b.as_str()));
        assert!(view.server_version.is_none());
        assert_eq!(view.events[0].state, LinkEventState::Sent);

        // The pusher reaches club b: what was known of a is dropped.
        desk.begin(&desk.club_of(&id("y")).unwrap());
        desk.report(LinkState::Failed {
            message: "no".into(),
        });
        let view = desk.view(&events, Some(&id("y")));
        assert_eq!(
            view.state,
            LinkState::Failed {
                message: "no".into()
            }
        );
        assert_eq!(view.events[0].state, LinkEventState::Waiting);
        // x open again: b's failure is not a's.
        assert_eq!(open_x().state, LinkState::Connecting);

        // x goes to no club: nothing is connected, whatever the pusher last said.
        desk.set_event_club(&id("x"), None).unwrap();
        assert_eq!(open_x().state, LinkState::NotSet);
        assert_eq!(open_x().events[0].state, LinkEventState::NoClub);
    }

    #[test]
    fn a_club_reached_another_way_is_not_the_club_that_was_known() {
        let (desk, a) = desk_with_a_club();
        desk.set_event_club(&id("x"), Some(&a)).unwrap();
        desk.begin(&desk.club_of(&id("x")).unwrap());
        desk.report(LinkState::Connected);
        desk.report_server(
            PushClub {
                id: "club".into(),
                name: "Recon".into(),
            },
            "0.6.0".into(),
        );
        desk.report_event(sent("x", &a));

        // A new name alone changes nothing that is known.
        let mut named = settings("https://a.example", None);
        named.name = "Mine".into();
        desk.save_club(Some(&a), named).unwrap();
        let view = desk.view(&[meta("x")], Some(&id("x")));
        assert_eq!(view.state, LinkState::Connected);
        assert_eq!(view.events[0].state, LinkEventState::Sent);
        assert!(view.clubs[0].club.is_some());

        // Another token may be another club on that server: all of it is forgotten.
        desk.save_club(Some(&a), settings("https://a.example", Some("gfs_other")))
            .unwrap();
        let view = desk.view(&[meta("x")], Some(&id("x")));
        assert_eq!(view.state, LinkState::Connecting);
        assert_eq!(view.events[0].state, LinkEventState::Waiting);
        assert!(view.clubs[0].club.is_none());
        // The event still goes to it.
        assert_eq!(view.events[0].club.as_deref(), Some(a.as_str()));
    }

    #[test]
    fn status_is_kept_only_for_the_events_named() {
        let (desk, a) = desk_with_a_club();
        let events: Vec<EventMeta> = ["a", "b", "c"].into_iter().map(meta).collect();
        for event in &events {
            desk.set_event_club(&event.id, Some(&a)).unwrap();
        }
        desk.begin(&desk.club_of(&id("a")).unwrap());
        for event in ["a", "b", "c"] {
            desk.report_event(sent(event, &a));
        }
        desk.keep_events(&[id("a"), id("c")]);
        let states: Vec<_> = desk
            .view(&events, Some(&id("a")))
            .events
            .into_iter()
            .map(|e| e.state)
            .collect();
        assert_eq!(
            states,
            [
                LinkEventState::Sent,
                LinkEventState::Waiting,
                LinkEventState::Sent
            ]
        );
    }

    #[test]
    fn a_replace_is_asked_once_and_wakes_the_pusher() {
        let desk = LinkDesk::new(None);
        let mut watch = desk.watch();
        watch.borrow_and_update();
        let event = id("e");
        assert!(!desk.take_replace(&event));
        desk.ask_replace(event.clone());
        desk.ask_replace(event.clone());
        assert!(watch.has_changed().unwrap());
        assert!(desk.take_replace(&event));
        assert!(!desk.take_replace(&event));
    }

    #[test]
    fn a_replace_asked_for_is_still_wanted_when_the_club_is_reached() {
        let (desk, a) = desk_with_a_club();
        desk.set_event_club(&id("x"), Some(&a)).unwrap();
        desk.ask_replace(id("x"));
        desk.begin(&desk.club_of(&id("x")).unwrap());
        assert!(desk.take_replace(&id("x")));
    }

    #[test]
    fn every_change_wakes_the_pusher_and_a_refused_one_does_not() {
        let desk = LinkDesk::new(None);
        let mut watch = desk.watch();
        let mut woke = |what: &str, expected: bool| {
            assert_eq!(watch.has_changed().unwrap(), expected, "{what}");
            watch.borrow_and_update();
        };
        assert!(desk.save_club(None, settings("nope", Some(TOKEN))).is_err());
        woke("a refused club", false);
        let club = desk
            .save_club(None, settings("https://a.example", Some(TOKEN)))
            .unwrap();
        woke("a saved club", true);
        assert!(desk.set_event_club(&id("x"), Some("nope")).is_err());
        woke("a club that is not saved", false);
        desk.set_event_club(&id("x"), Some(&club)).unwrap();
        woke("an event given a club", true);
        desk.save_club(Some(&club), settings("https://b.example", Some(TOKEN)))
            .unwrap();
        woke("a changed club", true);
        assert!(desk.remove_club("nope").is_err());
        woke("removing a club that is not saved", false);
        desk.remove_club(&club).unwrap();
        woke("a removed club", true);
    }

    #[test]
    fn it_is_all_there_after_a_restart_and_the_file_is_private() {
        let dir = temp();
        let club = {
            let desk = LinkDesk::new(Some(dir.clone()));
            let club = desk
                .save_club(None, settings("https://a.example", Some(TOKEN)))
                .unwrap();
            desk.set_event_club(&id("x"), Some(&club)).unwrap();
            desk.begin(&desk.club_of(&id("x")).unwrap());
            desk.report_server(
                PushClub {
                    id: "c1".into(),
                    name: "Recon".into(),
                },
                "0.6.0".into(),
            );
            club
        };
        let desk = LinkDesk::new(Some(dir.clone()));
        assert_eq!(
            desk.club_of(&id("x")),
            Some(SavedClub {
                id: club,
                name: String::new(),
                url: "https://a.example".into(),
                token: TOKEN.into(),
                trust: LinkTrust::System,
                // The club's name on the server is known before the server is reached.
                club: Some(PushClub {
                    id: "c1".into(),
                    name: "Recon".into()
                }),
            })
        );
        // Nothing was heard since the restart.
        assert_eq!(
            desk.view(&[meta("x")], Some(&id("x"))).state,
            LinkState::Connecting
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(LINK_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_that_cannot_be_read_or_is_from_an_older_build_gives_no_clubs() {
        let dir = temp();
        std::fs::write(dir.join(LINK_FILE), "{ half").unwrap();
        assert_eq!(
            LinkDesk::new(Some(dir.clone())).stored(),
            LinkStored::default()
        );
        // The file of a build that kept one link: it reads, and holds no club.
        std::fs::write(
            dir.join(LINK_FILE),
            r#"{"url":"https://a.example","token":"gfs_old","on":true,"trust":"System"}"#,
        )
        .unwrap();
        let desk = LinkDesk::new(Some(dir.clone()));
        assert_eq!(desk.stored(), LinkStored::default());
        // And the desk works from there.
        desk.save_club(None, settings("https://a.example", Some(TOKEN)))
            .unwrap();
        assert_eq!(LinkDesk::new(Some(dir.clone())).stored().clubs.len(), 1);
        // An event that goes to a club the file no longer has goes nowhere.
        std::fs::write(
            dir.join(LINK_FILE),
            r#"{"clubs":[],"events":{"x":"kgone"},"later":1}"#,
        )
        .unwrap();
        let desk = LinkDesk::new(Some(dir.clone()));
        assert!(desk.club_of(&id("x")).is_none() && desk.club_id_of(&id("x")).is_none());
        assert_eq!(
            desk.view(&[meta("x")], Some(&id("x"))).events[0].state,
            LinkEventState::NoClub
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_that_cannot_be_written_changes_nothing() {
        // The data folder is a file: nothing can be written under it.
        let dir = temp();
        let not_a_dir = dir.join("data");
        std::fs::write(&not_a_dir, "x").unwrap();
        let desk = LinkDesk::new(Some(not_a_dir));
        assert!(matches!(
            desk.save_club(None, settings("https://a.example", Some(TOKEN))),
            Err(LinkError::Io(_))
        ));
        assert_eq!(desk.stored(), LinkStored::default());
        std::fs::remove_dir_all(dir).ok();
    }
}
