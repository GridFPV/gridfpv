//! Whether the Director can be reached from other devices (docs/primitives.md §11).
//!
//! The desktop app listens on its own computer only. This setting adds a second listener
//! that other devices on the network can reach: phones at the field, a tablet, another
//! computer in the house. It is **off** until the director switches it on.
//!
//! What a device on the network may do is decided in [`crate::access`], not here. Switching
//! this on never gives a visitor more than the public pages.
//!
//! This module holds the setting and what is known about the listener. The listener itself
//! is started and stopped by the app (`gridfpv_app::network`), which watches
//! [`NetworkDesk::subscribe`] and reports back with [`NetworkDesk::report`].
//!
//! The setting is kept in `network.json` in the data folder. It is about this computer, so
//! it is not part of a copy made on the Data page.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// The file the setting is kept in.
pub const NETWORK_FILE: &str = "network.json";
/// The port other devices use, until the director picks another.
pub const DEFAULT_PORT: u16 = 8081;
/// The lowest port that can be picked. Ports under this need special rights on most systems.
pub const LOWEST_PORT: u16 = 1024;

/// The setting as it is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSettings {
    /// Whether other devices may reach the Director.
    #[serde(default)]
    pub enabled: bool,
    /// The port they reach it on.
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
        }
    }
}

/// Where the listener for other devices stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum NetworkState {
    /// Switched off. Only this computer can reach the Director.
    Off,
    /// Switched on, and not listening yet.
    Starting,
    /// Other devices can reach the Director.
    On,
    /// Switched on, but the listener could not be started. `problem` says why.
    Failed,
    /// The Director was started listening on the network (`GRIDFPV_ADDR`). It is on, and
    /// the switch here does nothing.
    Always,
}

/// What the console shows about the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NetworkView {
    /// The switch as it is set.
    pub enabled: bool,
    /// The port other devices use.
    pub port: u16,
    pub state: NetworkState,
    /// The addresses to open on another device, such as `http://192.168.1.20:8081/`. Empty
    /// when the listener is not up, or this computer has no network address.
    pub addresses: Vec<String>,
    /// Why the listener is not up, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem: Option<String>,
}

/// `PUT /network`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NetworkRequest {
    pub enabled: bool,
    /// Left out: the port stays as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub port: Option<u16>,
}

/// What the app reports about its listener.
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
    settings: NetworkSettings,
    listener: Listener,
    /// The address the Director's own listener is on.
    main: Option<SocketAddr>,
}

/// The network setting and status, shared by the API and the app's listener.
#[derive(Clone)]
pub struct NetworkDesk {
    inner: Arc<RwLock<Inner>>,
    wanted: watch::Sender<NetworkSettings>,
}

impl NetworkDesk {
    /// The desk kept under `data_dir`. A file that is missing or cannot be read means off.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let file = data_dir.map(|dir| dir.join(NETWORK_FILE));
        let mut settings: NetworkSettings = file
            .as_ref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // A file edited by hand to a port that cannot be used: fall back, do not fail.
        if settings.port < LOWEST_PORT {
            settings.port = DEFAULT_PORT;
        }
        let (wanted, _) = watch::channel(settings);
        Self {
            inner: Arc::new(RwLock::new(Inner {
                file,
                settings,
                listener: Listener::Stopped,
                main: None,
            })),
            wanted,
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The setting as it is now.
    pub fn settings(&self) -> NetworkSettings {
        self.read().settings
    }

    /// Hear each change to the setting. The app's listener follows this.
    pub fn subscribe(&self) -> watch::Receiver<NetworkSettings> {
        self.wanted.subscribe()
    }

    /// The app says where the Director's own listener is. One that is already on the
    /// network makes the state [`NetworkState::Always`].
    pub fn set_main(&self, bound: SocketAddr) {
        self.write().main = Some(bound);
    }

    /// The app says where its network listener stands.
    pub fn report(&self, listener: Listener) {
        self.write().listener = listener;
    }

    /// Whether the Director's own listener is already reachable from the network.
    fn always(inner: &Inner) -> Option<u16> {
        inner
            .main
            .filter(|main| !main.ip().to_canonical().is_loopback())
            .map(|main| main.port())
    }

    /// Change the setting. The port must be one a program may listen on, and not the one
    /// the Director itself is on.
    pub fn set(&self, request: NetworkRequest) -> Result<NetworkView, ProtocolError> {
        let settings = {
            let mut inner = self.write();
            let port = request.port.unwrap_or(inner.settings.port);
            if port < LOWEST_PORT {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("Pick a port from {LOWEST_PORT} to 65535."),
                ));
            }
            if Self::always(&inner).is_none() && inner.main.is_some_and(|main| main.port() == port)
            {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!(
                        "Port {port} is the one Grid itself uses on this computer. Pick another."
                    ),
                ));
            }
            let settings = NetworkSettings {
                enabled: request.enabled,
                port,
            };
            if let Some(file) = &inner.file {
                let bytes = serde_json::to_vec_pretty(&settings).unwrap_or_default();
                std::fs::write(file, bytes).map_err(|err| {
                    ProtocolError::new(
                        ErrorCode::Internal,
                        format!("The network setting could not be saved: {err}"),
                    )
                })?;
            }
            inner.settings = settings;
            settings
        };
        self.wanted.send_replace(settings);
        Ok(self.view())
    }

    /// What the console shows.
    pub fn view(&self) -> NetworkView {
        self.view_with(&local_addresses())
    }

    /// [`view`](Self::view), with this computer's addresses given. Tests use it.
    pub fn view_with(&self, addresses: &[Ipv4Addr]) -> NetworkView {
        let inner = self.read();
        let settings = inner.settings;
        let urls = |port: u16| -> Vec<String> {
            addresses
                .iter()
                .map(|ip| format!("http://{ip}:{port}/"))
                .collect()
        };
        if let Some(port) = Self::always(&inner) {
            // Listening on one address only: that is the one to give out.
            let addresses = match inner.main.map(|main| main.ip().to_canonical()) {
                Some(IpAddr::V4(ip)) if !ip.is_unspecified() => {
                    vec![format!("http://{ip}:{port}/")]
                }
                Some(IpAddr::V6(ip)) if !ip.is_unspecified() => {
                    vec![format!("http://[{ip}]:{port}/")]
                }
                _ => urls(port),
            };
            return NetworkView {
                enabled: settings.enabled,
                port,
                state: NetworkState::Always,
                addresses,
                problem: None,
            };
        }
        let (state, addresses, problem) = match (&inner.listener, settings.enabled) {
            (_, false) => (NetworkState::Off, Vec::new(), None),
            // Up, but on the port of a setting that has just been changed: not yet.
            (Listener::Up(port), true) if *port == settings.port => {
                let found = urls(*port);
                let problem = found.is_empty().then(|| {
                    "This computer is not on a network, so there is no address to give out."
                        .to_string()
                });
                (NetworkState::On, found, problem)
            }
            (Listener::Failed(why), true) => (NetworkState::Failed, Vec::new(), Some(why.clone())),
            (_, true) => (NetworkState::Starting, Vec::new(), None),
        };
        NetworkView {
            enabled: settings.enabled,
            port: settings.port,
            state,
            addresses,
            problem,
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

/// `GET /network` — the setting and where the listener stands.
pub async fn get_network(State(registry): State<EventRegistry>) -> Json<NetworkView> {
    Json(registry.network().view())
}

/// `PUT /network` — switch it on or off, or change the port.
pub async fn put_network(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<NetworkRequest>,
) -> Result<Json<NetworkView>, ProtocolError> {
    Ok(Json(registry.network().set(body)?))
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
    fn loopback_main(desk: &NetworkDesk) {
        desk.set_main("127.0.0.1:8080".parse().unwrap());
    }
    fn on(port: Option<u16>) -> NetworkRequest {
        NetworkRequest {
            enabled: true,
            port,
        }
    }
    const HOME: &[Ipv4Addr] = &[Ipv4Addr::new(192, 168, 1, 20)];

    #[test]
    fn it_is_off_until_switched_on() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        let view = desk.view_with(HOME);
        assert!(!view.enabled);
        assert_eq!(view.port, DEFAULT_PORT);
        assert_eq!(view.state, NetworkState::Off);
        assert!(view.addresses.is_empty());
        assert_eq!(view.problem, None);
    }

    #[test]
    fn switched_on_it_is_starting_until_the_listener_reports() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(None)).unwrap();
        assert_eq!(desk.view_with(HOME).state, NetworkState::Starting);
        desk.report(Listener::Up(DEFAULT_PORT));
        let view = desk.view_with(HOME);
        assert_eq!(view.state, NetworkState::On);
        assert_eq!(view.addresses, ["http://192.168.1.20:8081/"]);
        assert_eq!(view.problem, None);
    }

    #[test]
    fn a_listener_still_on_the_old_port_is_not_shown_as_on() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(None)).unwrap();
        desk.report(Listener::Up(DEFAULT_PORT));
        desk.set(on(Some(9000))).unwrap();
        let view = desk.view_with(HOME);
        assert_eq!(view.port, 9000);
        assert_eq!(view.state, NetworkState::Starting);
        assert!(view.addresses.is_empty());
        desk.report(Listener::Up(9000));
        assert_eq!(
            desk.view_with(HOME).addresses,
            ["http://192.168.1.20:9000/"]
        );
    }

    #[test]
    fn a_listener_that_could_not_start_says_why_and_gives_no_address() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(None)).unwrap();
        desk.report(Listener::Failed("Port 8081 is in use.".into()));
        let view = desk.view_with(HOME);
        assert_eq!(view.state, NetworkState::Failed);
        assert!(view.addresses.is_empty());
        assert_eq!(view.problem.as_deref(), Some("Port 8081 is in use."));
    }

    #[test]
    fn switched_off_it_is_off_whatever_the_listener_last_said() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(None)).unwrap();
        for last in [
            Listener::Up(DEFAULT_PORT),
            Listener::Failed("x".into()),
            Listener::Stopped,
        ] {
            desk.report(last);
            desk.set(NetworkRequest {
                enabled: false,
                port: None,
            })
            .unwrap();
            let view = desk.view_with(HOME);
            assert_eq!(view.state, NetworkState::Off);
            assert!(view.addresses.is_empty());
            assert_eq!(view.problem, None);
            desk.set(on(None)).unwrap();
        }
    }

    #[test]
    fn on_with_no_network_address_says_so() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(None)).unwrap();
        desk.report(Listener::Up(DEFAULT_PORT));
        let view = desk.view_with(&[]);
        assert_eq!(view.state, NetworkState::On);
        assert!(view.addresses.is_empty());
        assert!(view.problem.unwrap().contains("not on a network"));
    }

    #[test]
    fn every_address_of_the_computer_is_given() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(Some(9000))).unwrap();
        desk.report(Listener::Up(9000));
        let view = desk.view_with(&[v4("192.168.1.20"), v4("10.42.0.1")]);
        assert_eq!(
            view.addresses,
            ["http://192.168.1.20:9000/", "http://10.42.0.1:9000/"]
        );
    }

    #[test]
    fn a_port_that_cannot_be_used_is_refused_and_nothing_changes() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        for port in [0, 1, 80, 443, 1023] {
            let err = desk.set(on(Some(port))).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "{port}");
        }
        // The Director's own port.
        let err = desk.set(on(Some(8080))).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(err.message.contains("8080"));
        assert_eq!(desk.settings(), NetworkSettings::default());
        // The edges that are fine.
        desk.set(on(Some(1024))).unwrap();
        desk.set(on(Some(65535))).unwrap();
        assert_eq!(desk.settings().port, 65535);
    }

    #[test]
    fn leaving_the_port_out_keeps_it() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        desk.set(on(Some(9000))).unwrap();
        desk.set(NetworkRequest {
            enabled: false,
            port: None,
        })
        .unwrap();
        assert_eq!(
            desk.settings(),
            NetworkSettings {
                enabled: false,
                port: 9000
            }
        );
    }

    #[test]
    fn a_change_is_heard_by_the_listener() {
        let desk = NetworkDesk::new(None);
        loopback_main(&desk);
        let mut heard = desk.subscribe();
        assert_eq!(*heard.borrow_and_update(), NetworkSettings::default());
        desk.set(on(Some(9000))).unwrap();
        assert!(heard.has_changed().unwrap());
        assert_eq!(
            *heard.borrow_and_update(),
            NetworkSettings {
                enabled: true,
                port: 9000
            }
        );
        // A refused change is not heard.
        desk.set(on(Some(80))).unwrap_err();
        assert!(!heard.has_changed().unwrap());
    }

    #[test]
    fn the_setting_is_kept_in_the_data_folder_and_read_back() {
        let dir = tempdir("kept");
        let desk = NetworkDesk::new(Some(dir.clone()));
        assert_eq!(desk.settings(), NetworkSettings::default());
        assert!(
            !dir.join(NETWORK_FILE).exists(),
            "nothing is written until it is set"
        );
        desk.set(on(Some(9000))).unwrap();
        let again = NetworkDesk::new(Some(dir.clone()));
        assert_eq!(
            again.settings(),
            NetworkSettings {
                enabled: true,
                port: 9000
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_file_that_is_broken_or_odd_means_off_and_never_an_error() {
        let dir = tempdir("broken");
        for text in [
            "",
            "not json",
            "[]",
            "null",
            "{\"enabled\":\"yes\"}",
            "{\"enabled\":true,\"port\":\"x\"}",
            "{\"enabled\":true,\"port\":99999999}",
            "{\"enabled\":true,\"port\":-1}",
        ] {
            std::fs::write(dir.join(NETWORK_FILE), text).unwrap();
            assert_eq!(
                NetworkDesk::new(Some(dir.clone())).settings(),
                NetworkSettings::default(),
                "{text}"
            );
        }
        // Fields left out take their usual values; an unknown one is ignored.
        std::fs::write(dir.join(NETWORK_FILE), "{\"enabled\":true,\"later\":1}").unwrap();
        assert_eq!(
            NetworkDesk::new(Some(dir.clone())).settings(),
            NetworkSettings {
                enabled: true,
                port: DEFAULT_PORT
            }
        );
        std::fs::write(dir.join(NETWORK_FILE), "{}").unwrap();
        assert_eq!(
            NetworkDesk::new(Some(dir.clone())).settings(),
            NetworkSettings::default()
        );
        // A port edited by hand to one that cannot be used falls back.
        std::fs::write(dir.join(NETWORK_FILE), "{\"enabled\":true,\"port\":80}").unwrap();
        assert_eq!(
            NetworkDesk::new(Some(dir.clone())).settings(),
            NetworkSettings {
                enabled: true,
                port: DEFAULT_PORT
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_folder_that_cannot_be_written_refuses_the_change_and_keeps_the_old_setting() {
        let dir = tempdir("gone");
        let desk = NetworkDesk::new(Some(dir.join("no-such-folder")));
        let mut heard = desk.subscribe();
        heard.borrow_and_update();
        let err = desk.set(on(None)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(desk.settings(), NetworkSettings::default());
        assert!(!heard.has_changed().unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_director_started_on_the_network_is_always_on() {
        let desk = NetworkDesk::new(None);
        desk.set_main("0.0.0.0:8090".parse().unwrap());
        let view = desk.view_with(HOME);
        assert_eq!(view.state, NetworkState::Always);
        assert_eq!(view.port, 8090);
        assert_eq!(view.addresses, ["http://192.168.1.20:8090/"]);
        // Its own port is not a clash here: there is no second listener.
        desk.set(on(Some(8090))).unwrap();
        assert_eq!(desk.view_with(HOME).state, NetworkState::Always);
        // Started on one address: that one is given, not the others.
        let one = NetworkDesk::new(None);
        one.set_main("192.168.1.20:8090".parse().unwrap());
        assert_eq!(
            one.view_with(&[v4("192.168.1.20"), v4("10.0.0.2")])
                .addresses,
            ["http://192.168.1.20:8090/"]
        );
        let six = NetworkDesk::new(None);
        six.set_main("[2001:db8::7]:8090".parse().unwrap());
        assert_eq!(
            six.view_with(HOME).addresses,
            ["http://[2001:db8::7]:8090/"]
        );
        let any6 = NetworkDesk::new(None);
        any6.set_main("[::]:8090".parse().unwrap());
        assert_eq!(
            any6.view_with(HOME).addresses,
            ["http://192.168.1.20:8090/"]
        );
    }

    #[test]
    fn before_the_app_says_where_it_listens_nothing_is_assumed() {
        // No main address known: not "always", and no port is refused as the Director's own.
        let desk = NetworkDesk::new(None);
        assert_eq!(desk.view_with(HOME).state, NetworkState::Off);
        desk.set(on(Some(8080))).unwrap();
        assert_eq!(desk.view_with(HOME).state, NetworkState::Starting);
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
