//! How other devices reach the Director (docs/primitives.md §11.10).
//!
//! The Director's own listener answers the Grid computer only, and nothing here changes
//! that. This module adds two more **doors**, each a listener on its own port, each off
//! until the director switches it on:
//!
//! - **Public**: the spectator, pilot and kiosk pages. Nobody is the director there,
//!   whatever they send.
//! - **Remote RD**: the whole console, for a director on another device. It asks for the
//!   **access token**, which is made here and can be read on the Grid computer.
//!
//! What a caller at each door may do is decided in [`crate::access`], not here.
//!
//! This module holds the settings and what is known about the two listeners. The listeners
//! are started and stopped by the app (`gridfpv_app::network`), which watches
//! [`NetworkDesk::subscribe`] and reports back with [`NetworkDesk::report`].
//!
//! The settings are kept in `network.json` in the data folder. They are about this
//! computer, and they hold the access token, so they are not part of a copy made on the
//! Data page.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;

use crate::access::{Access, Origin};
use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// The file the settings are kept in.
pub const NETWORK_FILE: &str = "network.json";
/// The ports the Public door tries, in order, until the director picks one. Port 80 is the
/// one a browser uses when no port is typed, so the address is short. Some computers do
/// not let a program use it; then the next is used.
pub const PUBLIC_PORTS: [u16; 2] = [80, 8081];
/// The port the Remote RD door uses, until the director picks another.
pub const REMOTE_PORT: u16 = 8082;
/// The fewest characters an access token the director types may have.
pub const TOKEN_MIN: usize = 8;
/// The most.
pub const TOKEN_MAX: usize = 64;

/// One of the two doors other devices come in by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Door {
    /// The spectator, pilot and kiosk pages.
    Public,
    /// The whole console, with the access token.
    Remote,
}

impl Door {
    /// The ports this door tries, in order, when the director has picked none.
    pub fn usual_ports(self) -> &'static [u16] {
        match self {
            Door::Public => &PUBLIC_PORTS,
            Door::Remote => &[REMOTE_PORT],
        }
    }

    /// What the door is called on the Network page.
    pub fn name(self) -> &'static str {
        match self {
            Door::Public => "Public",
            Door::Remote => "Remote RD",
        }
    }

    fn other(self) -> Door {
        match self {
            Door::Public => Door::Remote,
            Door::Remote => Door::Public,
        }
    }
}

/// One door's setting as it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DoorSettings {
    /// Whether the door is open.
    #[serde(default)]
    pub enabled: bool,
    /// The port the director picked. `None`: the door's usual ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl DoorSettings {
    /// The ports to try, in order.
    pub fn ports(self, door: Door) -> Vec<u16> {
        match self.port {
            Some(port) => vec![port],
            None => door.usual_ports().to_vec(),
        }
    }
}

/// What the listeners follow: both doors' settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetworkSettings {
    pub public: DoorSettings,
    pub remote: DoorSettings,
    /// Goes up each time the access token changes. The Remote RD listener starts again
    /// when it does, so no connection made with the old token stays open.
    pub token_turn: u32,
}

impl NetworkSettings {
    pub fn door(&self, door: Door) -> DoorSettings {
        match door {
            Door::Public => self.public,
            Door::Remote => self.remote,
        }
    }
}

/// The file's contents.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    public: DoorSettings,
    #[serde(default)]
    remote: DoorSettings,
    /// The access token for the Remote RD door.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

/// Where a door's listener stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum DoorState {
    /// Switched off.
    Off,
    /// Switched on, and not listening yet.
    Starting,
    /// Other devices can come in by it.
    On,
    /// Switched on, but the listener could not be started. `problem` says why.
    Failed,
}

/// What the console shows about one door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DoorView {
    /// The switch as it is set.
    pub enabled: bool,
    /// The port in use, or the one that will be tried first.
    pub port: u16,
    /// Whether the director picked the port. `false`: Grid picks.
    pub port_picked: bool,
    pub state: DoorState,
    /// The addresses to open on another device, such as `http://192.168.1.20/`. Empty when
    /// the listener is not up, or this computer has no network address.
    pub addresses: Vec<String>,
    /// Why the listener is not up, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem: Option<String>,
    /// Something the director should know that is not a problem: Grid is on its second
    /// choice of port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// `GET /network`: what the console shows about the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NetworkView {
    pub public: DoorView,
    pub remote: DoorView,
    /// The access token for the Remote RD door. Given to the Grid computer only: a device
    /// that signed in with it is never told it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token: Option<String>,
}

/// `PUT /network/{door}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct DoorRequest {
    pub enabled: bool,
    /// Left out: the port stays as it is. `0`: Grid picks the port again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub port: Option<u16>,
}

/// `PUT /network/token`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TokenRequest {
    /// The token to use. Left out: Grid makes a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token: Option<String>,
}

/// What the app reports about one listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listener {
    /// Not running.
    Stopped,
    /// Running on this port.
    Up(u16),
    /// Could not start. The sentence is shown to the director.
    Failed(String),
}

struct Inner {
    file: Option<PathBuf>,
    stored: Stored,
    token_turn: u32,
    public: Listener,
    remote: Listener,
    /// The address the Director's own listener is on.
    main: Option<SocketAddr>,
}

impl Inner {
    fn settings(&self) -> NetworkSettings {
        NetworkSettings {
            public: self.stored.public,
            remote: self.stored.remote,
            token_turn: self.token_turn,
        }
    }
    fn listener(&self, door: Door) -> &Listener {
        match door {
            Door::Public => &self.public,
            Door::Remote => &self.remote,
        }
    }
}

/// The network settings and status, shared by the API and the app's listeners.
#[derive(Clone)]
pub struct NetworkDesk {
    inner: Arc<RwLock<Inner>>,
    wanted: watch::Sender<NetworkSettings>,
}

/// A new access token: twelve letters and digits in three groups, such as
/// `K7QM-2XWD-9HRA`. The letters and digits that look alike (`0 O 1 I`) are left out.
pub fn new_token() -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    let mut token = String::with_capacity(14);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && index % 4 == 0 {
            token.push('-');
        }
        // 256 is a multiple of 32, so every character is as likely as the next.
        token.push(ALPHABET[usize::from(*byte) % ALPHABET.len()] as char);
    }
    token
}

/// Whether two tokens are the same, taking as long whatever the answer.
fn same_token(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut differ = a.len() ^ b.len();
    for index in 0..a.len().max(b.len()) {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        differ |= usize::from(left ^ right);
    }
    differ == 0
}

/// What is wrong with a token the director typed, in words.
fn token_problem(token: &str) -> Option<String> {
    let count = token.chars().count();
    if count < TOKEN_MIN {
        return Some(format!(
            "An access token needs at least {TOKEN_MIN} characters."
        ));
    }
    if count > TOKEN_MAX {
        return Some(format!(
            "An access token can have {TOKEN_MAX} characters at most."
        ));
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("An access token cannot have spaces in it.".to_string());
    }
    None
}

/// The address to open for `ip` on `port`. Port 80 is the one a browser uses by itself.
fn url(ip: Ipv4Addr, port: u16) -> String {
    if port == 80 {
        format!("http://{ip}/")
    } else {
        format!("http://{ip}:{port}/")
    }
}

impl NetworkDesk {
    /// The desk kept under `data_dir`. A file that is missing or cannot be read means both
    /// doors are off.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let file = data_dir.map(|dir| dir.join(NETWORK_FILE));
        let mut stored: Stored = file
            .as_ref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // A file edited by hand: mend what cannot be used, do not fail.
        for door in [&mut stored.public, &mut stored.remote] {
            if door.port == Some(0) {
                door.port = None;
            }
        }
        if stored.public.port.is_some() && stored.public.port == stored.remote.port {
            stored.remote.port = None;
        }
        if stored
            .token
            .as_deref()
            .is_some_and(|token| token_problem(token).is_some())
        {
            stored.token = None;
        }
        // The Remote RD door is never open with no token to ask for.
        if stored.remote.enabled && stored.token.is_none() {
            stored.remote.enabled = false;
        }
        let inner = Inner {
            file,
            stored,
            token_turn: 0,
            public: Listener::Stopped,
            remote: Listener::Stopped,
            main: None,
        };
        let (wanted, _) = watch::channel(inner.settings());
        Self {
            inner: Arc::new(RwLock::new(inner)),
            wanted,
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The settings as they are now.
    pub fn settings(&self) -> NetworkSettings {
        self.read().settings()
    }

    /// Hear each change to the settings. The app's listeners follow this.
    pub fn subscribe(&self) -> watch::Receiver<NetworkSettings> {
        self.wanted.subscribe()
    }

    /// The app says where the Director's own listener is, so a door is not put on its port.
    pub fn set_main(&self, bound: SocketAddr) {
        self.write().main = Some(bound);
    }

    /// The app says where one door's listener stands.
    pub fn report(&self, door: Door, listener: Listener) {
        let mut inner = self.write();
        match door {
            Door::Public => inner.public = listener,
            Door::Remote => inner.remote = listener,
        }
    }

    /// Whether `token` is the access token, and the Remote RD door is open to take it.
    pub fn is_token(&self, token: &str) -> bool {
        let inner = self.read();
        inner.stored.remote.enabled
            && inner
                .stored
                .token
                .as_deref()
                .is_some_and(|kept| same_token(kept, token))
    }

    /// Whether a director can sign in at the Remote RD door.
    pub fn remote_open(&self) -> bool {
        let inner = self.read();
        inner.stored.remote.enabled && inner.stored.token.is_some()
    }

    /// Write `stored` to the file, then take it. On a failure nothing changes.
    fn keep(inner: &mut Inner, stored: Stored) -> Result<(), ProtocolError> {
        if let Some(file) = &inner.file {
            let bytes = serde_json::to_vec_pretty(&stored).unwrap_or_default();
            std::fs::write(file, bytes).map_err(|err| {
                ProtocolError::new(
                    ErrorCode::Internal,
                    format!("The network setting could not be saved: {err}"),
                )
            })?;
        }
        inner.stored = stored;
        Ok(())
    }

    /// Open or shut one door, or change its port.
    ///
    /// A picked port must not be the Director's own, nor the other door's. Opening the
    /// Remote RD door with no access token makes one.
    ///
    /// Answers whether directors signed in from other devices must be signed out: the
    /// Remote RD door was shut.
    pub fn set(&self, door: Door, request: DoorRequest) -> Result<bool, ProtocolError> {
        let (settings, shut) = {
            let mut inner = self.write();
            let before = inner.settings().door(door);
            let port = match request.port {
                None => before.port,
                Some(0) => None,
                Some(port) => Some(port),
            };
            if let Some(port) = port {
                let refuse = |why: String| Err(ProtocolError::new(ErrorCode::BadRequest, why));
                if inner.main.is_some_and(|main| main.port() == port) {
                    return refuse(format!(
                        "Port {port} is the one Grid itself uses on this computer. Pick another."
                    ));
                }
                let other = inner.settings().door(door.other());
                let others_port = match inner.listener(door.other()) {
                    Listener::Up(up) if other.enabled => Some(*up),
                    _ => other.port,
                };
                if other.port == Some(port) || others_port == Some(port) {
                    return refuse(format!(
                        "Port {port} is the one {} uses. Pick another.",
                        door.other().name()
                    ));
                }
            }
            let mut stored = inner.stored.clone();
            let next = DoorSettings {
                enabled: request.enabled,
                port,
            };
            match door {
                Door::Public => stored.public = next,
                Door::Remote => {
                    stored.remote = next;
                    if next.enabled && stored.token.is_none() {
                        stored.token = Some(new_token());
                    }
                }
            }
            Self::keep(&mut inner, stored)?;
            let shut = door == Door::Remote && before.enabled && !next.enabled;
            (inner.settings(), shut)
        };
        self.wanted.send_replace(settings);
        Ok(shut)
    }

    /// Change the access token: to `token`, or to a new one Grid makes. Every director
    /// signed in from another device must be signed out after this; the caller does that.
    pub fn set_token(&self, token: Option<&str>) -> Result<(), ProtocolError> {
        let settings = {
            let mut inner = self.write();
            let token = match token.map(str::trim) {
                Some(typed) => {
                    if let Some(problem) = token_problem(typed) {
                        return Err(ProtocolError::new(ErrorCode::BadRequest, problem));
                    }
                    typed.to_string()
                }
                None => new_token(),
            };
            let mut stored = inner.stored.clone();
            stored.token = Some(token);
            Self::keep(&mut inner, stored)?;
            inner.token_turn = inner.token_turn.wrapping_add(1);
            inner.settings()
        };
        self.wanted.send_replace(settings);
        Ok(())
    }

    /// The first address other devices open the public pages at, if the Public door is up.
    pub fn public_address(&self) -> Option<String> {
        self.view(false).public.addresses.into_iter().next()
    }

    /// What the console shows. `with_token`: the caller is on the Grid computer, and may
    /// read the access token.
    pub fn view(&self, with_token: bool) -> NetworkView {
        self.view_with(&local_addresses(), with_token)
    }

    /// [`view`](Self::view), with this computer's addresses given. Tests use it.
    pub fn view_with(&self, addresses: &[Ipv4Addr], with_token: bool) -> NetworkView {
        let inner = self.read();
        let door = |door: Door| -> DoorView {
            let settings = inner.settings().door(door);
            let ports = settings.ports(door);
            let first = ports[0];
            let mut view = DoorView {
                enabled: settings.enabled,
                port: first,
                port_picked: settings.port.is_some(),
                state: DoorState::Off,
                addresses: Vec::new(),
                problem: None,
                note: None,
            };
            match (inner.listener(door), settings.enabled) {
                (_, false) => {}
                // Up, but on the port of a setting that has just been changed: not yet.
                (Listener::Up(port), true) if ports.contains(port) => {
                    view.state = DoorState::On;
                    view.port = *port;
                    view.addresses = addresses.iter().map(|ip| url(*ip, *port)).collect();
                    if view.addresses.is_empty() {
                        view.problem = Some(
                            "This computer is not on a network, so there is no address to give out."
                                .to_string(),
                        );
                    }
                    if *port != first {
                        view.note = Some(format!(
                            "This computer did not let Grid use port {first}, so it is using port {port}."
                        ));
                    }
                }
                (Listener::Failed(why), true) => {
                    view.state = DoorState::Failed;
                    view.problem = Some(why.clone());
                }
                (_, true) => view.state = DoorState::Starting,
            }
            view
        };
        NetworkView {
            public: door(Door::Public),
            remote: door(Door::Remote),
            token: with_token.then(|| inner.stored.token.clone()).flatten(),
        }
    }
}

/// The order addresses are given out in: a home or field network first, then anything else.
fn rank(ip: &Ipv4Addr) -> u8 {
    if ip.is_private() { 0 } else { 1 }
}

/// Whether a network interface, by its name, is one a program made for itself and no phone
/// is on: a container bridge, a virtual machine's switch. Its address would only confuse.
pub fn is_virtual_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    const STARTS: [&str; 7] = ["docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "lxc"];
    const HOLDS: [&str; 6] = [
        "vethernet",
        "wsl",
        "hyper-v",
        "virtualbox",
        "vmware",
        "loopback",
    ];
    STARTS.iter().any(|start| name.starts_with(start))
        || HOLDS.iter().any(|part| name.contains(part))
}

/// Which of a computer's addresses another device could use, best first.
///
/// Kept: IPv4 only; not this computer's own loopback; not the self-assigned kind a computer
/// gives itself with no network (169.254.x.x); not an address on a virtual interface
/// ([`is_virtual_interface`]).
///
/// Order: `primary` first, when it is one of them. That is the address this computer uses
/// to reach the rest of the network, so it is nearly always the one a phone needs. Then
/// home and field networks, then the rest. No repeats.
pub fn usable_addresses(
    found: impl IntoIterator<Item = (String, IpAddr)>,
    primary: Option<Ipv4Addr>,
) -> Vec<Ipv4Addr> {
    let plain = |ip: &Ipv4Addr| {
        !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_broadcast()
    };
    let mut usable: Vec<Ipv4Addr> = found
        .into_iter()
        .filter(|(name, _)| !is_virtual_interface(name))
        .filter_map(|(_, ip)| match ip.to_canonical() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .filter(plain)
        .collect();
    usable.sort_by_key(|ip| (rank(ip), *ip));
    usable.dedup();
    if let Some(primary) = primary.filter(plain) {
        usable.retain(|ip| *ip != primary);
        usable.insert(0, primary);
    }
    usable
}

/// The address this computer uses to reach the rest of its network, if it has a way out.
/// Nothing is sent: asking the system which address it *would* use is enough. A computer
/// that is only on a field network with no way out has none, and that is fine.
pub fn primary_address() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // An address kept for documentation: it is never reached, and never needs to be.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

/// This computer's addresses on its networks, right now, best first.
pub fn local_addresses() -> Vec<Ipv4Addr> {
    let found: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| (interface.name.clone(), interface.ip()))
        .collect();
    // The way out counts only if it is over a real interface: on a computer whose only
    // route is through a virtual switch, that address is no use to a phone.
    let primary = primary_address().filter(|primary| {
        found
            .iter()
            .any(|(name, ip)| *ip == IpAddr::V4(*primary) && !is_virtual_interface(name))
    });
    usable_addresses(found, primary)
}

/// Whether the caller is on the Grid computer. A router with no gate around it (a test) is.
fn on_grid_computer(access: Option<Extension<Access>>) -> bool {
    access.is_none_or(|Extension(access)| access.origin == Origin::Local)
}

/// `GET /network` — both doors, and where their listeners stand. The access token is in
/// the answer only for the Grid computer.
pub async fn get_network(
    State(registry): State<EventRegistry>,
    access: Option<Extension<Access>>,
) -> Json<NetworkView> {
    Json(registry.network().view(on_grid_computer(access)))
}

/// `PUT /network/{door}` — open or shut a door, or change its port. `door` is `public` or
/// `remote`.
pub async fn put_door(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    access: Option<Extension<Access>>,
    Path(door): Path<String>,
    Json(body): Json<DoorRequest>,
) -> Result<Json<NetworkView>, ProtocolError> {
    let door = match door.as_str() {
        "public" => Door::Public,
        "remote" => Door::Remote,
        _ => {
            return Err(ProtocolError::new(
                ErrorCode::UnknownScope,
                "There is no such door. There are two: public and remote.",
            ));
        }
    };
    let network = registry.network();
    if network.set(door, body)? {
        registry.tokens().end_browser_sessions();
    }
    Ok(Json(network.view(on_grid_computer(access))))
}

/// `PUT /network/token` — change the access token. Only the Grid computer may: a device
/// that came in with the token cannot change it for everyone else.
pub async fn put_token(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    access: Option<Extension<Access>>,
    Json(body): Json<TokenRequest>,
) -> Result<Json<NetworkView>, ProtocolError> {
    if !on_grid_computer(access) {
        return Err(ProtocolError::new(
            ErrorCode::Forbidden,
            "The access token can only be changed on the Grid computer.",
        ));
    }
    let network = registry.network();
    network.set_token(body.token.as_deref())?;
    registry.tokens().end_browser_sessions();
    Ok(Json(network.view(true)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }
    fn v4(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    fn desk() -> NetworkDesk {
        let desk = NetworkDesk::new(None);
        desk.set_main("127.0.0.1:8080".parse().unwrap());
        desk
    }
    fn on(port: Option<u16>) -> DoorRequest {
        DoorRequest {
            enabled: true,
            port,
        }
    }
    fn off() -> DoorRequest {
        DoorRequest {
            enabled: false,
            port: None,
        }
    }
    const HOME: &[Ipv4Addr] = &[Ipv4Addr::new(192, 168, 1, 20)];
    const BOTH: [Door; 2] = [Door::Public, Door::Remote];

    fn view(desk: &NetworkDesk, door: Door) -> DoorView {
        let view = desk.view_with(HOME, true);
        match door {
            Door::Public => view.public,
            Door::Remote => view.remote,
        }
    }

    #[test]
    fn both_doors_are_shut_until_switched_on() {
        let desk = desk();
        for door in BOTH {
            let view = view(&desk, door);
            assert!(!view.enabled);
            assert!(!view.port_picked);
            assert_eq!(view.state, DoorState::Off);
            assert!(view.addresses.is_empty());
            assert_eq!(view.problem, None);
        }
        assert_eq!(view(&desk, Door::Public).port, 80);
        assert_eq!(view(&desk, Door::Remote).port, REMOTE_PORT);
        assert_eq!(desk.view_with(HOME, true).token, None);
        assert!(!desk.remote_open());
    }

    #[test]
    fn one_door_is_switched_without_the_other() {
        let desk = desk();
        desk.set(Door::Public, on(None)).unwrap();
        assert!(view(&desk, Door::Public).enabled);
        assert!(!view(&desk, Door::Remote).enabled);
        desk.set(Door::Remote, on(None)).unwrap();
        desk.set(Door::Public, off()).unwrap();
        assert!(!view(&desk, Door::Public).enabled);
        assert!(view(&desk, Door::Remote).enabled);
    }

    #[test]
    fn switched_on_a_door_is_starting_until_its_listener_reports() {
        let desk = desk();
        desk.set(Door::Remote, on(None)).unwrap();
        assert_eq!(view(&desk, Door::Remote).state, DoorState::Starting);
        desk.report(Door::Remote, Listener::Up(REMOTE_PORT));
        let view = view(&desk, Door::Remote);
        assert_eq!(view.state, DoorState::On);
        assert_eq!(view.addresses, ["http://192.168.1.20:8082/"]);
        assert_eq!(view.problem, None);
        assert_eq!(view.note, None);
    }

    #[test]
    fn the_public_door_on_port_80_gives_an_address_with_no_port() {
        let desk = desk();
        desk.set(Door::Public, on(None)).unwrap();
        desk.report(Door::Public, Listener::Up(80));
        let view = view(&desk, Door::Public);
        assert_eq!(view.port, 80);
        assert_eq!(view.addresses, ["http://192.168.1.20/"]);
        assert_eq!(view.note, None);
    }

    #[test]
    fn the_public_door_on_its_second_port_says_so() {
        let desk = desk();
        desk.set(Door::Public, on(None)).unwrap();
        desk.report(Door::Public, Listener::Up(8081));
        let view = view(&desk, Door::Public);
        assert_eq!(view.state, DoorState::On);
        assert_eq!(view.port, 8081);
        assert!(!view.port_picked);
        assert_eq!(view.addresses, ["http://192.168.1.20:8081/"]);
        let note = view.note.unwrap();
        assert!(
            note.contains("port 80") && note.contains("port 8081"),
            "{note}"
        );
    }

    #[test]
    fn a_picked_port_is_used_and_zero_gives_the_choice_back_to_grid() {
        let desk = desk();
        desk.set(Door::Public, on(Some(9000))).unwrap();
        let picked = view(&desk, Door::Public);
        assert_eq!(picked.port, 9000);
        assert!(picked.port_picked);
        assert_eq!(desk.settings().public.ports(Door::Public), [9000]);
        // Left out: kept.
        desk.set(Door::Public, off()).unwrap();
        assert_eq!(desk.settings().public.port, Some(9000));
        // Zero: Grid picks again.
        desk.set(Door::Public, on(Some(0))).unwrap();
        assert_eq!(desk.settings().public.port, None);
        assert_eq!(desk.settings().public.ports(Door::Public), PUBLIC_PORTS);
    }

    #[test]
    fn a_listener_still_on_the_old_port_is_not_shown_as_on() {
        let desk = desk();
        desk.set(Door::Remote, on(None)).unwrap();
        desk.report(Door::Remote, Listener::Up(REMOTE_PORT));
        desk.set(Door::Remote, on(Some(9000))).unwrap();
        let moved = view(&desk, Door::Remote);
        assert_eq!(moved.port, 9000);
        assert_eq!(moved.state, DoorState::Starting);
        assert!(moved.addresses.is_empty());
        desk.report(Door::Remote, Listener::Up(9000));
        assert_eq!(
            view(&desk, Door::Remote).addresses,
            ["http://192.168.1.20:9000/"]
        );
    }

    #[test]
    fn a_listener_that_could_not_start_says_why_and_gives_no_address() {
        let desk = desk();
        desk.set(Door::Public, on(None)).unwrap();
        desk.report(
            Door::Public,
            Listener::Failed("Port 8081 is in use.".into()),
        );
        let view = view(&desk, Door::Public);
        assert_eq!(view.state, DoorState::Failed);
        assert!(view.addresses.is_empty());
        assert_eq!(view.problem.as_deref(), Some("Port 8081 is in use."));
    }

    #[test]
    fn a_shut_door_is_off_whatever_its_listener_last_said() {
        let desk = desk();
        for door in BOTH {
            for last in [
                Listener::Up(door.usual_ports()[0]),
                Listener::Failed("x".into()),
                Listener::Stopped,
            ] {
                desk.set(door, on(None)).unwrap();
                desk.report(door, last);
                desk.set(door, off()).unwrap();
                let view = view(&desk, door);
                assert_eq!(view.state, DoorState::Off);
                assert!(view.addresses.is_empty());
                assert_eq!(view.problem, None);
            }
        }
    }

    #[test]
    fn on_with_no_network_address_says_so() {
        let desk = desk();
        desk.set(Door::Public, on(None)).unwrap();
        desk.report(Door::Public, Listener::Up(80));
        let view = desk.view_with(&[], true).public;
        assert_eq!(view.state, DoorState::On);
        assert!(view.addresses.is_empty());
        assert!(view.problem.unwrap().contains("not on a network"));
    }

    #[test]
    fn every_address_of_the_computer_is_given() {
        let desk = desk();
        desk.set(Door::Public, on(Some(9000))).unwrap();
        desk.report(Door::Public, Listener::Up(9000));
        let view = desk.view_with(&[v4("192.168.1.20"), v4("10.42.0.1")], true);
        assert_eq!(
            view.public.addresses,
            ["http://192.168.1.20:9000/", "http://10.42.0.1:9000/"]
        );
        assert_eq!(
            desk.public_address(),
            desk.view(false).public.addresses.first().cloned()
        );
    }

    #[test]
    fn no_two_listeners_may_be_given_one_port() {
        let desk = desk();
        // The Director's own port.
        for door in BOTH {
            let err = desk.set(door, on(Some(8080))).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest);
            assert!(err.message.contains("8080"), "{}", err.message);
        }
        assert_eq!(desk.settings(), NetworkSettings::default());
        // The port the other door was given.
        desk.set(Door::Public, on(Some(9000))).unwrap();
        let err = desk.set(Door::Remote, on(Some(9000))).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("Public"), "{}", err.message);
        assert!(!desk.settings().remote.enabled);
        // The port the other door is really on, when Grid picked it.
        desk.set(Door::Public, on(Some(0))).unwrap();
        desk.report(Door::Public, Listener::Up(8081));
        let err = desk.set(Door::Remote, on(Some(8081))).unwrap_err();
        assert!(err.message.contains("Public"), "{}", err.message);
        desk.set(Door::Remote, on(Some(8083))).unwrap();
        let err = desk.set(Door::Public, on(Some(8083))).unwrap_err();
        assert!(err.message.contains("Remote RD"), "{}", err.message);
        // Any other port from 1 up is fine, low ones too.
        for port in [1, 80, 443, 1024, 65535] {
            desk.set(Door::Public, on(Some(port))).unwrap();
            assert_eq!(desk.settings().public.port, Some(port));
        }
    }

    #[test]
    fn opening_the_remote_door_makes_a_token_and_keeps_it_after() {
        let desk = desk();
        desk.set(Door::Public, on(None)).unwrap();
        assert_eq!(desk.view_with(HOME, true).token, None, "public needs none");
        desk.set(Door::Remote, on(None)).unwrap();
        let token = desk.view_with(HOME, true).token.unwrap();
        assert_eq!(token.len(), 14);
        assert!(desk.remote_open());
        assert!(desk.is_token(&token));
        // Shut and opened again: the same token.
        assert!(desk.set(Door::Remote, off()).unwrap(), "shutting signs out");
        assert!(!desk.is_token(&token), "a shut door takes no token");
        assert!(!desk.remote_open());
        assert!(!desk.set(Door::Remote, on(None)).unwrap());
        assert_eq!(desk.view_with(HOME, true).token.as_deref(), Some(&*token));
        assert!(desk.is_token(&token));
        // Shutting the public door signs nobody out.
        assert!(!desk.set(Door::Public, off()).unwrap());
    }

    #[test]
    fn the_token_is_shown_to_the_grid_computer_only() {
        let desk = desk();
        desk.set(Door::Remote, on(None)).unwrap();
        assert!(desk.view_with(HOME, true).token.is_some());
        assert_eq!(desk.view_with(HOME, false).token, None);
        assert_eq!(desk.view(false).token, None);
    }

    #[test]
    fn only_the_exact_token_is_the_token() {
        let desk = desk();
        desk.set(Door::Remote, on(None)).unwrap();
        desk.set_token(Some("field-day-2026")).unwrap();
        assert!(desk.is_token("field-day-2026"));
        for wrong in [
            "",
            "field-day-202",
            "field-day-20266",
            "Field-day-2026",
            "field-day-2026 ",
            "xield-day-2026",
        ] {
            assert!(!desk.is_token(wrong), "{wrong:?}");
        }
    }

    #[test]
    fn a_new_token_replaces_the_old_one_and_is_heard_by_the_listeners() {
        let desk = desk();
        desk.set(Door::Remote, on(None)).unwrap();
        let first = desk.view_with(HOME, true).token.unwrap();
        let mut heard = desk.subscribe();
        heard.borrow_and_update();
        desk.set_token(None).unwrap();
        let second = desk.view_with(HOME, true).token.unwrap();
        assert_ne!(first, second);
        assert!(!desk.is_token(&first));
        assert!(desk.is_token(&second));
        assert!(heard.has_changed().unwrap(), "the listener starts again");
        assert_eq!(heard.borrow_and_update().token_turn, 1);
        // Typed tokens are tidied of the spaces around them.
        desk.set_token(Some("  my-own-token  ")).unwrap();
        assert!(desk.is_token("my-own-token"));
    }

    #[test]
    fn a_token_that_is_too_short_too_long_or_has_spaces_is_refused() {
        let desk = desk();
        desk.set(Door::Remote, on(None)).unwrap();
        let kept = desk.view_with(HOME, true).token.unwrap();
        let long = "x".repeat(TOKEN_MAX + 1);
        for bad in [
            "",
            "short",
            "1234567",
            "has a space in",
            "tab\tinside1",
            &long,
        ] {
            let err = desk.set_token(Some(bad)).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "{bad:?}");
        }
        assert!(desk.is_token(&kept), "a refused token changes nothing");
        desk.set_token(Some("12345678")).unwrap();
        desk.set_token(Some(&"x".repeat(TOKEN_MAX))).unwrap();
    }

    #[test]
    fn made_tokens_are_twelve_plain_characters_and_never_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let token = new_token();
            let groups: Vec<&str> = token.split('-').collect();
            assert_eq!(groups.len(), 3, "{token}");
            for group in groups {
                assert_eq!(group.len(), 4, "{token}");
                assert!(
                    group
                        .chars()
                        .all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c)),
                    "{token}"
                );
            }
            assert!(seen.insert(token));
        }
    }

    #[test]
    fn a_change_is_heard_by_the_listeners_and_a_refused_one_is_not() {
        let desk = desk();
        let mut heard = desk.subscribe();
        assert_eq!(*heard.borrow_and_update(), NetworkSettings::default());
        desk.set(Door::Public, on(Some(9000))).unwrap();
        assert!(heard.has_changed().unwrap());
        assert_eq!(
            heard.borrow_and_update().public,
            DoorSettings {
                enabled: true,
                port: Some(9000)
            }
        );
        desk.set(Door::Remote, on(Some(8080))).unwrap_err();
        assert!(!heard.has_changed().unwrap());
    }

    #[test]
    fn the_settings_and_the_token_are_kept_in_the_data_folder_and_read_back() {
        let dir = tempdir("kept");
        let desk = NetworkDesk::new(Some(dir.clone()));
        assert_eq!(desk.settings(), NetworkSettings::default());
        assert!(
            !dir.join(NETWORK_FILE).exists(),
            "nothing is written until it is set"
        );
        desk.set(Door::Public, on(Some(9000))).unwrap();
        desk.set(Door::Remote, on(None)).unwrap();
        desk.set_token(Some("field-day-2026")).unwrap();
        let again = NetworkDesk::new(Some(dir.clone()));
        assert_eq!(
            again.settings().public,
            DoorSettings {
                enabled: true,
                port: Some(9000)
            }
        );
        assert_eq!(
            again.settings().remote,
            DoorSettings {
                enabled: true,
                port: None
            }
        );
        assert!(again.is_token("field-day-2026"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_that_is_broken_odd_or_from_an_older_grid_never_fails_to_load() {
        let dir = tempdir("broken");
        for text in [
            "",
            "not json",
            "[]",
            "null",
            "{}",
            "{\"public\":\"yes\"}",
            "{\"public\":{\"enabled\":true,\"port\":\"x\"}}",
            "{\"public\":{\"enabled\":true,\"port\":99999999}}",
            "{\"remote\":{\"enabled\":true,\"port\":-1}}",
            // The shape an older Grid wrote: one switch and one port. Both doors are shut.
            "{\"enabled\":true,\"port\":8081}",
        ] {
            std::fs::write(dir.join(NETWORK_FILE), text).unwrap();
            assert_eq!(
                NetworkDesk::new(Some(dir.clone())).settings(),
                NetworkSettings::default(),
                "{text}"
            );
        }
        // Fields left out take their usual values; an unknown one is ignored.
        std::fs::write(
            dir.join(NETWORK_FILE),
            "{\"public\":{\"enabled\":true},\"later\":1}",
        )
        .unwrap();
        let desk = NetworkDesk::new(Some(dir.clone()));
        assert!(desk.settings().public.enabled);
        assert_eq!(desk.settings().public.port, None);
        // Edited by hand: the remote door open with no token is shut.
        std::fs::write(dir.join(NETWORK_FILE), "{\"remote\":{\"enabled\":true}}").unwrap();
        assert!(
            !NetworkDesk::new(Some(dir.clone()))
                .settings()
                .remote
                .enabled
        );
        // A token that could not have been set is dropped, and the door with it.
        std::fs::write(
            dir.join(NETWORK_FILE),
            "{\"remote\":{\"enabled\":true},\"token\":\"x\"}",
        )
        .unwrap();
        let desk = NetworkDesk::new(Some(dir.clone()));
        assert!(!desk.settings().remote.enabled);
        assert!(!desk.is_token("x"));
        // Both doors on one port: the remote door goes back to its own.
        std::fs::write(
            dir.join(NETWORK_FILE),
            "{\"public\":{\"enabled\":true,\"port\":9000},\"remote\":{\"port\":9000}}",
        )
        .unwrap();
        let desk = NetworkDesk::new(Some(dir.clone()));
        assert_eq!(desk.settings().public.port, Some(9000));
        assert_eq!(desk.settings().remote.port, None);
        // Port zero means Grid picks.
        std::fs::write(
            dir.join(NETWORK_FILE),
            "{\"public\":{\"enabled\":true,\"port\":0}}",
        )
        .unwrap();
        assert_eq!(
            NetworkDesk::new(Some(dir.clone())).settings().public.port,
            None
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_folder_that_cannot_be_written_refuses_the_change_and_keeps_the_old_setting() {
        let dir = tempdir("gone");
        let desk = NetworkDesk::new(Some(dir.join("no-such-folder")));
        let mut heard = desk.subscribe();
        heard.borrow_and_update();
        let err = desk.set(Door::Public, on(None)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        let err = desk.set_token(None).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(desk.settings(), NetworkSettings::default());
        assert_eq!(desk.view_with(HOME, true).token, None);
        assert!(!heard.has_changed().unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn before_the_app_says_where_it_listens_no_port_is_refused_as_its_own() {
        let desk = NetworkDesk::new(None);
        desk.set(Door::Public, on(Some(8080))).unwrap();
        assert_eq!(view(&desk, Door::Public).state, DoorState::Starting);
    }

    fn at(name: &str, address: &str) -> (String, IpAddr) {
        (name.to_string(), ip(address))
    }

    #[test]
    fn only_addresses_another_device_can_use_are_given() {
        assert_eq!(
            usable_addresses(
                [
                    at("lo", "127.0.0.1"),
                    at("lo", "::1"),
                    at("eth1", "169.254.10.20"),
                    at("eth0", "fe80::1"),
                    at("eth0", "2001:db8::1"),
                    at("eth2", "0.0.0.0"),
                    at("eth2", "255.255.255.255"),
                    at("tun0", "100.64.0.5"),
                    at("wlan0", "192.168.1.20"),
                    at("wlan0", "192.168.1.20"),
                    at("eth0", "10.42.0.1"),
                    at("eth3", "::ffff:172.16.5.5"),
                ],
                None
            ),
            [
                v4("10.42.0.1"),
                v4("172.16.5.5"),
                v4("192.168.1.20"),
                v4("100.64.0.5")
            ]
        );
        assert!(usable_addresses([], None).is_empty());
    }

    #[test]
    fn addresses_of_virtual_interfaces_are_left_out() {
        // A laptop with Docker and a virtual machine: only the Wi-Fi address is of any use.
        assert_eq!(
            usable_addresses(
                [
                    at("docker0", "172.17.0.1"),
                    at("br-3f2a9c1d77aa", "172.18.0.1"),
                    at("veth1a2b3c", "172.19.0.1"),
                    at("virbr0", "192.168.122.1"),
                    at("vmnet8", "172.16.99.1"),
                    at("vboxnet0", "192.168.56.1"),
                    at("lxcbr0", "10.0.3.1"),
                    at("vEthernet (WSL)", "172.25.160.1"),
                    at("vEthernet (Default Switch)", "172.30.0.1"),
                    at("VirtualBox Host-Only Network", "192.168.56.2"),
                    at("VMware Network Adapter VMnet1", "192.168.80.1"),
                    at("Loopback Pseudo-Interface 1", "10.9.9.9"),
                    at("wlp3s0", "192.168.1.20"),
                ],
                None
            ),
            [v4("192.168.1.20")]
        );
        for real in [
            "eth0",
            "enp3s0",
            "wlan0",
            "wlp3s0",
            "en0",
            "Wi-Fi",
            "Ethernet",
            "Ethernet 2",
        ] {
            assert!(!is_virtual_interface(real), "{real}");
        }
        for made in [
            "docker0",
            "DOCKER0",
            "br-abc",
            "vEthernet (WSL)",
            "vethernet (x)",
        ] {
            assert!(is_virtual_interface(made), "{made}");
        }
        // A bridge a person made by hand ("br0", "bridge0") is a real network.
        assert!(!is_virtual_interface("br0"));
        assert!(!is_virtual_interface("bridge0"));
    }

    #[test]
    fn the_address_the_computer_uses_to_get_out_comes_first() {
        let found = || {
            [
                at("eth0", "10.0.0.5"),
                at("wlan0", "192.168.1.20"),
                at("tun0", "100.64.0.5"),
            ]
        };
        assert_eq!(
            usable_addresses(found(), Some(v4("192.168.1.20"))),
            [v4("192.168.1.20"), v4("10.0.0.5"), v4("100.64.0.5")]
        );
        assert_eq!(
            usable_addresses(found(), Some(v4("100.64.0.5"))),
            [v4("100.64.0.5"), v4("10.0.0.5"), v4("192.168.1.20")]
        );
        // With no way out, the plain order.
        assert_eq!(
            usable_addresses(found(), None),
            [v4("10.0.0.5"), v4("192.168.1.20"), v4("100.64.0.5")]
        );
        // A "way out" that is no use to anyone is not put first, or in at all.
        for useless in ["127.0.0.1", "169.254.1.1", "0.0.0.0"] {
            assert_eq!(
                usable_addresses(found(), Some(v4(useless))),
                [v4("10.0.0.5"), v4("192.168.1.20"), v4("100.64.0.5")],
                "{useless}"
            );
        }
        // It is listed once, even when the interface list has it too.
        assert_eq!(
            usable_addresses([at("wlan0", "192.168.1.20")], Some(v4("192.168.1.20"))),
            [v4("192.168.1.20")]
        );
    }

    #[test]
    fn this_computers_addresses_can_be_read_and_none_is_unusable() {
        for ip in local_addresses() {
            assert!(!ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified());
        }
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-network-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
