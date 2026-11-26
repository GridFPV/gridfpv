//! The listeners other devices reach the Director on (docs/primitives.md §11.10).
//!
//! The Director's own listener is for the Grid computer. These two are for the network: the
//! **Public** door and the **Remote RD** door. Each is started when the director switches
//! it on, stopped when they switch it off, and moved when they change its port. The
//! settings and the status live in [`gridfpv_server::network::NetworkDesk`]; this module
//! only follows it.
//!
//! Each serves the very same app as the Director's own listener, gate included, and marks
//! every request with the [`Door`] it came in by. The gate decides from that what the
//! caller may do.
//!
//! **Off means off.** Stopping a listener also ends every connection it had open, so a
//! phone that was following a race stops getting updates the moment the switch goes off.
//! That is why the connections are served here by hand and not by `axum::serve`, which
//! leaves open connections running. Changing the access token starts the Remote RD
//! listener again for the same reason: nothing opened with the old token stays open.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::network::{Door, DoorSettings, Listener, NetworkSettings};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};

/// How long to wait before trying again to start a listener that could not start: the port
/// may have been freed.
const RETRY: Duration = Duration::from_secs(5);

/// Why a listener could not start, in words for the director.
pub fn bind_problem(port: u16, err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::AddrInUse => {
            format!("Port {port} is in use by another program on this computer. Pick another port.")
        }
        std::io::ErrorKind::PermissionDenied if port < 1024 => format!(
            "This computer does not let Grid use port {port}. Ports under 1024 need special rights here. Pick a port from 1024 up."
        ),
        std::io::ErrorKind::PermissionDenied => format!(
            "This computer did not let Grid listen on port {port}. Pick another port, or check the firewall."
        ),
        _ => format!("Grid could not listen on port {port}: {err}"),
    }
}

/// Accept connections and serve `app` on each, until this future is dropped. Dropping it
/// ends every connection it has open.
async fn serve(listener: TcpListener, app: Router) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    // A failed accept (too many open files, a reset before accept) is about
                    // that one connection. Give the system a moment and carry on.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                // The gate reads the peer's address from here. Without it, it would have to
                // treat the caller as unknown.
                let service = TowerToHyperService::new(
                    app.clone().layer(axum::Extension(ConnectInfo(peer))),
                );
                connections.spawn(async move {
                    let _ = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(TokioIo::new(stream), service)
                        .await;
                });
            }
            // Forget connections that have ended, so the set does not grow all day.
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// A listener that is up. Dropping it stops it and ends its connections, so it can never
/// outlive the task that follows the settings, however that task ends.
struct Up {
    /// The setting it was started for.
    wanted: (DoorSettings, u32),
    task: JoinHandle<()>,
}

impl Drop for Up {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Up {
    /// Stop it, and wait until it has let go of its port.
    async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

/// What makes one door's listener the right one: its setting, and for the Remote RD door
/// the turn of the access token.
fn wanted_of(door: Door, settings: &NetworkSettings) -> (DoorSettings, u32) {
    let turn = match door {
        Door::Public => 0,
        Door::Remote => settings.token_turn,
    };
    (settings.door(door), turn)
}

/// Listen on the first of `ports` that can be had. On none: why the last could not.
async fn bind_first(bind: Ipv4Addr, ports: &[u16]) -> Result<TcpListener, String> {
    let mut problem = String::from("Grid has no port to listen on.");
    for port in ports {
        match TcpListener::bind(SocketAddr::from((bind, *port))).await {
            Ok(listener) => return Ok(listener),
            Err(err) => problem = bind_problem(*port, &err),
        }
    }
    Err(problem)
}

/// Follow one door's setting for as long as the Director runs.
pub fn spawn(registry: EventRegistry, app: Router, door: Door) -> JoinHandle<()> {
    spawn_on(registry, app, door, Ipv4Addr::UNSPECIFIED)
}

/// [`spawn`], listening on one address. Tests listen on loopback only.
pub fn spawn_on(
    registry: EventRegistry,
    app: Router,
    door: Door,
    bind: Ipv4Addr,
) -> JoinHandle<()> {
    // Every request taken here says which door it came in by.
    let app = app.layer(axum::Extension(door));
    tokio::spawn(async move {
        let desk = registry.network();
        let mut heard = desk.subscribe();
        // The listener that is up, if one is.
        let mut up: Option<Up> = None;
        loop {
            let wanted = wanted_of(door, &heard.borrow_and_update());
            let settings = wanted.0;
            let finished = up.as_ref().is_some_and(|up| up.task.is_finished());
            let right = up
                .as_ref()
                .is_some_and(|up| settings.enabled && up.wanted == wanted);
            if (!right || finished)
                && let Some(old) = up.take()
            {
                // Waits until it has let go of the port, so the same port can be taken again.
                old.stop().await;
            }
            let mut retry = false;
            if !settings.enabled {
                desk.report(door, Listener::Stopped);
            } else if up.is_none() {
                match bind_first(bind, &settings.ports(door)).await {
                    Ok(listener) => {
                        let port = listener.local_addr().map_or(0, |a| a.port());
                        up = Some(Up {
                            wanted,
                            task: tokio::spawn(serve(listener, app.clone())),
                        });
                        desk.report(door, Listener::Up(port));
                    }
                    Err(problem) => {
                        desk.report(door, Listener::Failed(problem));
                        retry = true;
                    }
                }
            }
            // Wait for the setting to change. While a listener could not start, also look
            // again every few seconds.
            let changed = if retry {
                match tokio::time::timeout(RETRY, heard.changed()).await {
                    Ok(changed) => changed,
                    Err(_) => Ok(()),
                }
            } else {
                heard.changed().await
            };
            if changed.is_err() {
                // The desk is gone: the Director is closing. Dropping `up` stops the listener.
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_in_use_is_said_in_plain_words() {
        let taken = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert_eq!(
            bind_problem(8081, &taken),
            "Port 8081 is in use by another program on this computer. Pick another port."
        );
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(bind_problem(8081, &denied).contains("did not let Grid listen on port 8081"));
        let other = std::io::Error::other("boom");
        assert_eq!(
            bind_problem(9000, &other),
            "Grid could not listen on port 9000: boom"
        );
    }
}
