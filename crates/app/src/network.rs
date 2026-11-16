//! The listener other devices reach the Director on (docs/primitives.md §11).
//!
//! The Director's own listener is for the Grid computer. This one is for the network: it is
//! started when the director switches "other devices" on, stopped when they switch it off,
//! and moved when they change its port. The setting and the status live in
//! [`gridfpv_server::network::NetworkDesk`]; this module only follows it.
//!
//! It serves the very same app as the Director's own listener, gate included. A request
//! that arrives here from another device is a visitor's, and the gate treats it as one.
//!
//! **Off means off.** Stopping the listener also ends every connection it had open, so a
//! phone that was following a race stops getting updates the moment the switch goes off.
//! That is why the connections are served here by hand and not by `axum::serve`, which
//! leaves open connections running.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::network::{Listener, NetworkSettings};
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
/// outlive the task that follows the setting, however that task ends.
struct Up {
    port: u16,
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

/// Follow the network setting for as long as the Director runs.
///
/// `bind` is the address family to listen on; the port comes from the setting.
pub fn spawn(registry: EventRegistry, app: Router) -> JoinHandle<()> {
    spawn_on(registry, app, Ipv4Addr::UNSPECIFIED)
}

/// [`spawn`], listening on one address. Tests listen on loopback only.
pub fn spawn_on(registry: EventRegistry, app: Router, bind: Ipv4Addr) -> JoinHandle<()> {
    tokio::spawn(async move {
        let desk = registry.network();
        let mut wanted = desk.subscribe();
        // The listener that is up, if one is.
        let mut up: Option<Up> = None;
        loop {
            let settings: NetworkSettings = *wanted.borrow_and_update();
            let finished = up.as_ref().is_some_and(|up| up.task.is_finished());
            let right = up
                .as_ref()
                .is_some_and(|up| settings.enabled && up.port == settings.port);
            if (!right || finished)
                && let Some(old) = up.take()
            {
                // Waits until it has let go of the port, so the same port can be taken again.
                old.stop().await;
            }
            let mut retry = false;
            if !settings.enabled {
                desk.report(Listener::Stopped);
            } else if up.is_none() {
                match TcpListener::bind(SocketAddr::from((bind, settings.port))).await {
                    Ok(listener) => {
                        // Port 0 is never set by the console, but a test asks for it: report
                        // the port that was really taken.
                        let port = listener.local_addr().map_or(settings.port, |a| a.port());
                        up = Some(Up {
                            port: settings.port,
                            task: tokio::spawn(serve(listener, app.clone())),
                        });
                        desk.report(Listener::Up(port));
                    }
                    Err(err) => {
                        desk.report(Listener::Failed(bind_problem(settings.port, &err)));
                        retry = true;
                    }
                }
            }
            // Wait for the setting to change. While a listener could not start, also look
            // again every few seconds.
            let changed = if retry {
                match tokio::time::timeout(RETRY, wanted.changed()).await {
                    Ok(changed) => changed,
                    Err(_) => Ok(()),
                }
            } else {
                wanted.changed().await
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
