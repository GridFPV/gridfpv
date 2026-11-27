//! The Director's link to a remote server (docs/primitives.md §13): where the server is,
//! the token to push with, and how the pushing is going.
//!
//! This is the desk: what is set, kept in `server-link.json`, and what is reported. The
//! pushing itself is `gridfpv_app::link`, which reads the settings here and writes the
//! status here. The console reads and sets both through the routes below.
//!
//! | route | what |
//! |-------|------|
//! | `GET /server-link` | the settings and how the pushing stands |
//! | `PUT /server-link` | set them |
//! | `POST /server-link/events/{id}/replace` | replace the server's copy of an event |
//!
//! None of these is open to a visitor (`access`: an API route is closed until it is listed).
//!
//! # What is checked
//!
//! - **HTTPS only** (§13.5). The one exception is a server on this same computer
//!   (`http://localhost` or a loopback address): nothing leaves the machine, and it is how a
//!   server is tried out before it has a name.
//! - **The token is never given back.** The console is told only that one is set.

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
use crate::events::{EventId, EventRegistry};
use crate::remote::PushClub;

/// The file the link's settings are kept in, under the data dir.
pub const LINK_FILE: &str = "server-link.json";

/// What every link token starts with (`gridfpv-serverd` makes them).
const TOKEN_PREFIX: &str = "gfs_";

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

/// The settings as they are kept.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinkStored {
    /// The server's address, such as `https://grid.example.org`. Empty when none is set.
    #[serde(default)]
    pub url: String,
    /// The link token. Empty when none is set.
    #[serde(default)]
    pub token: String,
    /// Whether the Director pushes.
    #[serde(default)]
    pub on: bool,
    #[serde(default)]
    pub trust: LinkTrust,
}

impl LinkStored {
    /// Whether there is enough here to push with.
    pub fn ready(&self) -> bool {
        self.on && !self.url.is_empty() && !self.token.is_empty()
    }
}

/// `PUT /server-link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkSettings {
    pub url: String,
    /// A new token. Left out, the one already set is kept. Empty, it is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token: Option<String>,
    pub on: bool,
    #[serde(default)]
    pub trust: LinkTrust,
}

/// How the link as a whole stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum LinkState {
    /// No address or no token is set.
    #[default]
    NotSet,
    /// Set, and switched off.
    Off,
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

/// One event, and how much of it the server holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkEvent {
    pub id: EventId,
    pub name: String,
    /// How many entries the event's log holds here.
    #[ts(type = "number")]
    pub len: u64,
    /// How many of them the server holds.
    #[ts(type = "number")]
    pub sent: u64,
    pub state: LinkEventState,
}

/// `GET /server-link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LinkView {
    pub url: String,
    /// Whether a token is set. The token itself is never given out.
    pub has_token: bool,
    pub on: bool,
    pub trust: LinkTrust,
    pub state: LinkState,
    /// The club the token pushes into, once the server has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub club: Option<PushClub>,
    /// The server's build, once it has said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub server_version: Option<String>,
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
    #[error("The settings could not be saved: {0}")]
    Io(String),
}

#[derive(Default)]
struct Status {
    state: LinkState,
    club: Option<PushClub>,
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

/// The link's settings and status. Cloning shares the one desk.
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

impl LinkDesk {
    /// The desk kept under `data_dir`. A file that cannot be read gives a link that is not
    /// set, which pushes nothing.
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

    /// The settings as kept, token and all. For the pusher, not for the wire.
    pub fn stored(&self) -> LinkStored {
        self.read().stored.clone()
    }

    /// Told each time the settings change or a replace is asked for.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Set the link. Nothing is kept unless all of it is right.
    pub fn save(&self, settings: LinkSettings) -> Result<(), LinkError> {
        let url = clean_url(&settings.url)?;
        let trust = clean_trust(settings.trust)?;
        let token = match settings.token {
            None => None,
            Some(token) => {
                let token = token.trim().to_owned();
                if !token.is_empty()
                    && (!token.starts_with(TOKEN_PREFIX)
                        || token.len() <= TOKEN_PREFIX.len()
                        || token.chars().any(|c| c.is_whitespace() || c.is_control()))
                {
                    return Err(LinkError::NotAToken);
                }
                Some(token)
            }
        };
        {
            let mut inner = self.write();
            let mut next = inner.stored.clone();
            // Another server is another token: the old one is not sent to a new address.
            if next.url != url && token.is_none() {
                next.token = String::new();
            }
            next.url = url;
            next.on = settings.on;
            next.trust = trust;
            if let Some(token) = token {
                next.token = token;
            }
            if let Some(dir) = &inner.data_dir {
                write_private(&dir.join(LINK_FILE), &next).map_err(LinkError::Io)?;
            }
            let same_server = inner.stored.url == next.url && inner.stored.token == next.token;
            inner.stored = next;
            if !same_server {
                // What was known was about another server, or another club on it.
                inner.status = Status::default();
            }
        }
        self.changed.send_modify(|n| *n += 1);
        Ok(())
    }

    /// The settings and status, for the console.
    pub fn view(&self) -> LinkView {
        let inner = self.read();
        let state = if inner.stored.url.is_empty() || inner.stored.token.is_empty() {
            LinkState::NotSet
        } else if !inner.stored.on {
            LinkState::Off
        } else {
            match &inner.status.state {
                // Switched on a moment ago, and the pusher has not said yet.
                LinkState::NotSet | LinkState::Off => LinkState::Connecting,
                other => other.clone(),
            }
        };
        let pushing = inner.stored.ready();
        LinkView {
            url: inner.stored.url.clone(),
            has_token: !inner.stored.token.is_empty(),
            on: inner.stored.on,
            trust: inner.stored.trust.clone(),
            state,
            club: inner.status.club.clone().filter(|_| pushing),
            server_version: inner.status.server_version.clone().filter(|_| pushing),
            events: if pushing {
                inner.status.events.values().cloned().collect()
            } else {
                Vec::new()
            },
        }
    }

    // ── What the pusher reports ─────────────────────────────────────────────

    /// How the link as a whole stands.
    pub fn report(&self, state: LinkState) {
        self.write().status.state = state;
    }

    /// What the server said of itself.
    pub fn report_server(&self, club: PushClub, version: String) {
        let mut inner = self.write();
        inner.status.club = Some(club);
        inner.status.server_version = Some(version);
    }

    /// How one event stands.
    pub fn report_event(&self, event: LinkEvent) {
        self.write().status.events.insert(event.id.clone(), event);
    }

    /// Keep status only for the events that still exist.
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

/// `GET /server-link`.
pub async fn get_link(State(registry): State<EventRegistry>) -> Json<LinkView> {
    Json(registry.link().view())
}

/// `PUT /server-link`.
pub async fn put_link(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(settings): Json<LinkSettings>,
) -> Result<Json<LinkView>, ProtocolError> {
    registry.link().save(settings)?;
    Ok(Json(registry.link().view()))
}

/// `POST /server-link/events/{id}/replace`.
pub async fn post_replace(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(id): Path<EventId>,
) -> Result<Json<LinkView>, ProtocolError> {
    if registry.meta_of(&id).is_none() {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", id.0),
        ));
    }
    registry.link().ask_replace(id);
    Ok(Json(registry.link().view()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "gfs_abcDEF123-_";

    fn settings(url: &str, token: Option<&str>, on: bool) -> LinkSettings {
        LinkSettings {
            url: url.to_owned(),
            token: token.map(str::to_owned),
            on,
            trust: LinkTrust::System,
        }
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

    #[test]
    fn a_new_desk_is_not_set_and_pushes_nothing() {
        let desk = LinkDesk::new(None);
        assert!(!desk.stored().ready());
        let view = desk.view();
        assert_eq!(view.state, LinkState::NotSet);
        assert!(!view.has_token && !view.on && view.url.is_empty());
    }

    #[test]
    fn addresses_are_tidied_and_only_https_leaves_the_computer() {
        let desk = LinkDesk::new(None);
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
            ("", ""),
        ] {
            desk.save(settings(given, None, false)).unwrap();
            assert_eq!(desk.stored().url, kept, "{given:?}");
        }
        for (given, why) in [
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
            assert_eq!(
                desk.save(settings(given, None, false)),
                Err(why),
                "{given:?}"
            );
        }
        // A refused one changed nothing.
        assert_eq!(desk.stored().url, "");
    }

    #[test]
    fn a_token_is_kept_when_left_out_removed_when_empty_and_never_shown() {
        let desk = LinkDesk::new(None);
        desk.save(settings(
            "https://a.example",
            Some(&format!("  {TOKEN}\n")),
            true,
        ))
        .unwrap();
        assert_eq!(desk.stored().token, TOKEN);
        assert!(desk.stored().ready());

        desk.save(settings("https://a.example", None, true))
            .unwrap();
        assert_eq!(desk.stored().token, TOKEN, "left out: kept");

        let shown = serde_json::to_string(&desk.view()).unwrap();
        assert!(
            !shown.contains(TOKEN) && !shown.contains("abcDEF"),
            "{shown}"
        );
        assert!(desk.view().has_token);

        desk.save(settings("https://a.example", Some(""), true))
            .unwrap();
        assert_eq!(desk.stored().token, "");
        assert_eq!(desk.view().state, LinkState::NotSet);
        assert!(!desk.stored().ready());
    }

    #[test]
    fn a_token_is_not_carried_to_another_server() {
        let desk = LinkDesk::new(None);
        desk.save(settings("https://a.example", Some(TOKEN), true))
            .unwrap();
        desk.save(settings("https://b.example", None, true))
            .unwrap();
        assert_eq!(
            desk.stored().token,
            "",
            "the old server's token is not sent to the new one"
        );
        // Given with the new address, it is kept.
        desk.save(settings("https://c.example", Some(TOKEN), true))
            .unwrap();
        assert_eq!(desk.stored().token, TOKEN);
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
                desk.save(settings("https://a.example", Some(bad), true)),
                Err(LinkError::NotAToken),
                "{bad:?}"
            );
        }
        assert_eq!(desk.stored(), LinkStored::default());
    }

    #[test]
    fn extra_authorities_must_be_certificates() {
        let desk = LinkDesk::new(None);
        let pem = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        let mut s = settings("https://a.example", Some(TOKEN), true);
        s.trust = LinkTrust::ExtraAuthorities {
            pem: format!("\n{pem}\n\n"),
        };
        desk.save(s.clone()).unwrap();
        assert_eq!(
            desk.stored().trust,
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
                desk.save(s.clone()),
                Err(LinkError::NotACertificate),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_state_shown_follows_the_settings_and_then_the_pusher() {
        let desk = LinkDesk::new(None);
        desk.save(settings("https://a.example", Some(TOKEN), false))
            .unwrap();
        assert_eq!(desk.view().state, LinkState::Off);
        desk.save(settings("https://a.example", None, true))
            .unwrap();
        assert_eq!(desk.view().state, LinkState::Connecting);

        desk.report(LinkState::Connected);
        desk.report_server(
            PushClub {
                id: "club".into(),
                name: "Recon".into(),
            },
            "0.6.0".into(),
        );
        desk.report_event(LinkEvent {
            id: EventId("e".into()),
            name: "E".into(),
            len: 10,
            sent: 4,
            state: LinkEventState::Sending,
        });
        let view = desk.view();
        assert_eq!(view.state, LinkState::Connected);
        assert_eq!(view.club.unwrap().name, "Recon");
        assert_eq!(view.events.len(), 1);

        // Switched off: nothing of the server is shown, and it is all there again when on.
        desk.save(settings("https://a.example", None, false))
            .unwrap();
        let off = desk.view();
        assert_eq!(off.state, LinkState::Off);
        assert!(off.club.is_none() && off.events.is_empty());
        desk.save(settings("https://a.example", None, true))
            .unwrap();
        assert_eq!(desk.view().events.len(), 1);

        // Another server: what was known is forgotten.
        desk.save(settings("https://b.example", Some(TOKEN), true))
            .unwrap();
        let moved = desk.view();
        assert_eq!(moved.state, LinkState::Connecting);
        assert!(moved.club.is_none() && moved.events.is_empty());
    }

    #[test]
    fn status_is_kept_only_for_events_that_exist() {
        let desk = LinkDesk::new(None);
        desk.save(settings("https://a.example", Some(TOKEN), true))
            .unwrap();
        for id in ["a", "b", "c"] {
            desk.report_event(LinkEvent {
                id: EventId(id.into()),
                name: id.into(),
                len: 1,
                sent: 1,
                state: LinkEventState::Sent,
            });
        }
        desk.keep_events(&[EventId("a".into()), EventId("c".into())]);
        let ids: Vec<String> = desk.view().events.into_iter().map(|e| e.id.0).collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[test]
    fn a_replace_is_asked_once_and_wakes_the_pusher() {
        let desk = LinkDesk::new(None);
        let mut watch = desk.watch();
        watch.borrow_and_update();
        let event = EventId("e".into());
        assert!(!desk.take_replace(&event));
        desk.ask_replace(event.clone());
        desk.ask_replace(event.clone());
        assert!(watch.has_changed().unwrap());
        assert!(desk.take_replace(&event));
        assert!(!desk.take_replace(&event));
    }

    #[test]
    fn saving_wakes_the_pusher_and_a_refused_save_does_not() {
        let desk = LinkDesk::new(None);
        let mut watch = desk.watch();
        watch.borrow_and_update();
        assert!(desk.save(settings("nope", None, true)).is_err());
        assert!(!watch.has_changed().unwrap());
        desk.save(settings("https://a.example", None, true))
            .unwrap();
        assert!(watch.has_changed().unwrap());
    }

    #[test]
    fn the_settings_are_there_after_a_restart_and_the_file_is_private() {
        let dir = temp();
        {
            let desk = LinkDesk::new(Some(dir.clone()));
            desk.save(settings("https://a.example", Some(TOKEN), true))
                .unwrap();
        }
        let desk = LinkDesk::new(Some(dir.clone()));
        assert_eq!(
            desk.stored(),
            LinkStored {
                url: "https://a.example".into(),
                token: TOKEN.into(),
                on: true,
                trust: LinkTrust::System
            }
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
    fn a_file_that_cannot_be_read_gives_a_link_that_is_not_set() {
        let dir = temp();
        std::fs::write(dir.join(LINK_FILE), "{ half").unwrap();
        let desk = LinkDesk::new(Some(dir.clone()));
        assert_eq!(desk.stored(), LinkStored::default());
        // A file from a build that kept less, or more, still reads.
        std::fs::write(
            dir.join(LINK_FILE),
            r#"{"url":"https://a.example","later":1}"#,
        )
        .unwrap();
        assert_eq!(
            LinkDesk::new(Some(dir.clone())).stored().url,
            "https://a.example"
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
