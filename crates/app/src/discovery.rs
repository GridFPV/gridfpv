//! Finding OFT timers on the network.
//!
//! An OFT timer advertises itself over mDNS as `_oft._tcp` (spec §6.1): its name, its port
//! and a few facts about itself. [`spawn`] listens for those for as long as the Director
//! runs and keeps the timer directory's "found on the network" list up to date.
//!
//! Finding a timer adds nothing. A person adds it, from that list, and then it is a timer
//! like any other.
//!
//! mDNS needs the network to allow multicast. Where it does not (some sandboxes, some
//! locked-down networks) nothing is found, a line goes in the log, and everything else
//! works: a timer can always be added by typing its address.

use std::net::Ipv4Addr;

use gridfpv_server::events::EventRegistry;
use gridfpv_server::timers::DiscoveredTimer;
use mdns_sd::{ServiceDaemon, ServiceEvent};

/// The service an OFT timer advertises.
const SERVICE: &str = "_oft._tcp.local.";

/// What a timer said about itself, turned into a list entry. `None` when it gave no usable
/// address.
///
/// A timer on a machine with several addresses advertises all of them: loopback, and
/// sometimes ones only that machine can use (a container bridge, say). So the first one
/// that `reachable` says answers is taken. They are tried in a fixed order, one that is not
/// loopback first, so the entry does not flip between two that both answer.
fn describe(
    name: &str,
    addresses: impl IntoIterator<Item = Ipv4Addr>,
    port: u16,
    seats: Option<&str>,
    reachable: impl Fn(Ipv4Addr, u16) -> bool,
) -> Option<DiscoveredTimer> {
    let mut addresses: Vec<Ipv4Addr> = addresses.into_iter().collect();
    addresses.sort_by_key(|ip| (ip.is_loopback(), *ip));
    let ip = addresses.iter().find(|ip| reachable(**ip, port))?;
    Some(DiscoveredTimer {
        name: name.to_owned(),
        address: format!("{ip}:{port}"),
        seats: seats.and_then(|seats| seats.parse().ok()),
        added: false,
    })
}

/// Whether something answers at an address, tried for a fifth of a second. That is plenty
/// on a race network, and short enough that an address that goes nowhere costs little.
fn answers(ip: Ipv4Addr, port: u16) -> bool {
    let address = std::net::SocketAddr::from((ip, port));
    std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(200)).is_ok()
}

/// The name a timer advertises under, from its full mDNS name
/// ("oft-dev._oft._tcp.local." → "oft-dev").
fn instance_name(fullname: &str) -> &str {
    fullname
        .strip_suffix(SERVICE)
        .map_or(fullname, |name| name.trim_end_matches('.'))
}

/// Start listening for timers. The listener runs on its own thread until the returned
/// handle is dropped.
pub fn spawn(registry: EventRegistry) -> Option<Discovery> {
    let daemon = match ServiceDaemon::new() {
        Ok(daemon) => daemon,
        Err(e) => {
            eprintln!("WARNING: cannot look for timers on the network (mDNS): {e}");
            return None;
        }
    };
    let found = match daemon.browse(SERVICE) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("WARNING: cannot look for timers on the network (mDNS): {e}");
            let _ = daemon.shutdown();
            return None;
        }
    };
    std::thread::spawn(move || {
        // Ends when the daemon is shut down: the channel closes.
        while let Ok(event) = found.recv() {
            match event {
                ServiceEvent::ServiceResolved(service) => {
                    let name = service
                        .txt_properties
                        .get_property_val_str("id")
                        .unwrap_or_else(|| instance_name(&service.fullname))
                        .to_owned();
                    let seats = service.txt_properties.get_property_val_str("seats");
                    let described = describe(
                        &name,
                        service.get_addresses_v4(),
                        service.port,
                        seats,
                        answers,
                    );
                    match described {
                        Some(timer) => registry.timers().found(&service.fullname, timer),
                        None => registry.timers().lost(&service.fullname),
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => registry.timers().lost(&fullname),
                _ => {}
            }
        }
    });
    Some(Discovery(daemon))
}

/// The running listener. Dropping it stops the listening.
pub struct Discovery(ServiceDaemon);

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timer_is_listed_at_the_first_address_that_answers() {
        let loopback = Ipv4Addr::new(127, 0, 0, 1);
        let lan = Ipv4Addr::new(192, 168, 1, 50);
        let bridge = Ipv4Addr::new(172, 17, 0, 1);
        let all = |_: Ipv4Addr, _: u16| true;
        let timer = describe("oft-dev", [loopback, lan, bridge], 7270, Some("2"), all).unwrap();
        assert_eq!(timer.name, "oft-dev");
        // With every address answering: not loopback, and the lowest so it does not flip.
        assert_eq!(timer.address, "172.17.0.1:7270");
        assert_eq!(timer.seats, Some(2));
        assert!(!timer.added);
        // An address only the timer's own machine can use does not answer from here: the
        // one that does is taken.
        let only_lan = |ip: Ipv4Addr, _: u16| ip == lan;
        assert_eq!(
            describe("oft-dev", [loopback, lan, bridge], 7270, None, only_lan)
                .unwrap()
                .address,
            "192.168.1.50:7270"
        );
        // Only loopback answers: still usable from this machine.
        let only_loopback = |ip: Ipv4Addr, _: u16| ip.is_loopback();
        assert_eq!(
            describe("oft-dev", [loopback, lan], 7270, None, only_loopback)
                .unwrap()
                .address,
            "127.0.0.1:7270"
        );
        // A seat count that is not a number is left out, not guessed.
        assert_eq!(
            describe("x", [lan], 7270, Some("many"), all).unwrap().seats,
            None
        );
        // Nothing answers, or no address at all: no entry.
        assert_eq!(describe("x", [lan], 7270, None, |_, _| false), None);
        assert_eq!(describe("x", [], 7270, None, all), None);
    }

    #[test]
    fn the_name_comes_from_the_advertised_name() {
        assert_eq!(instance_name("oft-dev._oft._tcp.local."), "oft-dev");
        assert_eq!(instance_name("odd"), "odd");
    }
}
