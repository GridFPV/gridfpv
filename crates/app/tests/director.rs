//! Director-server wiring tests (#13, v0.4 Director wiring).
//!
//! These drive the *exact* router `main` serves — [`gridfpv_app::director::build_app`]
//! over an [`AppState`] — with no real socket, via `tower::ServiceExt::oneshot`. They
//! assert the protocol API is reachable (a `GET /health` 200) and that the static SPA is
//! served with an `index.html` fallback when `GRIDFPV_ASSETS` points at a built dir. The
//! committed tests never depend on the real frontend `dist/`: the SPA case writes its own
//! tiny `index.html` into a temp dir.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gridfpv_app::director::{
    AssetStatus, asset_status, build_app, default_assets_dir, run_director,
};
use gridfpv_server::events::EventRegistry;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A unique temp directory under the OS temp dir, created fresh for one test.
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gridfpv-director-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp assets dir");
    dir
}

/// `GET <uri>` against the Director router over an empty in-memory log.
async fn get(assets: &Path, uri: &str) -> (StatusCode, String) {
    let registry = EventRegistry::new(None).unwrap();
    let app = build_app(registry, assets);
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn health_endpoint_is_served() {
    // A non-existent assets dir must not break the API.
    let assets = std::env::temp_dir().join("gridfpv-director-does-not-exist");
    let (status, body) = get(&assets, "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn snapshot_endpoint_returns_ok() {
    let assets = std::env::temp_dir().join("gridfpv-director-no-assets");
    // The event scope folds the whole (empty) log into idle live state — a 200 either way.
    let (status, _body) = get(&assets, "/events/practice/snapshot/event/spring-cup").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn root_serves_index_html_when_assets_present() {
    let dir = temp_dir("root");
    let marker = "<!doctype html><title>RD Console</title><div id=app></div>";
    std::fs::write(dir.join("index.html"), marker).unwrap();

    let (status, body) = get(&dir, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("RD Console"), "served the SPA shell: {body}");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn unknown_client_route_falls_back_to_index_html() {
    let dir = temp_dir("spa-fallback");
    let marker = "<!doctype html><title>RD Console</title>";
    std::fs::write(dir.join("index.html"), marker).unwrap();

    // A deep client-side route (no such file) must resolve to the SPA shell, not 404.
    let (status, body) = get(&dir, "/heats/q-1/live").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("RD Console"),
        "fell back to index.html: {body}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn static_assets_are_served_alongside_index() {
    let dir = temp_dir("assets");
    std::fs::write(dir.join("index.html"), "<title>shell</title>").unwrap();
    std::fs::write(dir.join("app.js"), "console.log('rd')").unwrap();

    let (status, body) = get(&dir, "/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("console.log"), "served the JS asset: {body}");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn cors_preflight_is_permissive() {
    let dir = temp_dir("cors");
    std::fs::write(dir.join("index.html"), "<title>shell</title>").unwrap();

    let registry = EventRegistry::new(None).unwrap();
    let app = build_app(registry, &dir);
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/events/practice/snapshot/event/spring-cup")
                .header("Origin", "http://tauri.localhost")
                .header("Access-Control-Request-Method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // A permissive CORS layer answers the preflight with an allow-origin header so the
    // cross-origin Tauri RD app may call the API + open the WS.
    assert!(
        response
            .headers()
            .contains_key("access-control-allow-origin"),
        "permissive CORS echoes an allow-origin header"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The **embedded-Director path** the Tauri native app relies on: [`run_director`] binds a
/// loopback **ephemeral** port (`127.0.0.1:0`), reports the OS-assigned port via `on_ready`,
/// and answers `/health` over a real socket — all with **no display**, proving the server
/// half of the desktop app is sound on a headless VM (the GUI window is the only piece that
/// needs a display). A `oneshot`-driven graceful shutdown then stops it cleanly.
#[tokio::test]
async fn run_director_serves_health_on_loopback_ephemeral_port() {
    let dir = temp_dir("embedded");
    std::fs::write(dir.join("index.html"), "<title>RD Console</title>").unwrap();

    // Capture the bound address `run_director` reports — this is exactly what the Tauri app
    // reads (`ready.bound.port()`) to point its window at `http://127.0.0.1:<port>`.
    let bound: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let bound_for_cb = bound.clone();

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    // Run the SAME entry point the desktop app uses, on loopback + an ephemeral port, with no
    // configured token (⇒ open control, the loopback-trust model) and an in-memory registry.
    let server = tokio::spawn(async move {
        // `Box<dyn Error>` isn't `Send`, so flatten to a `String` for the join across tasks.
        run_director(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            None,
            dir,
            move |ready| {
                *bound_for_cb.lock().unwrap() = Some(ready.bound);
            },
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
        .map_err(|e| e.to_string())
    });

    // Poll briefly for `on_ready` to record the bound (ephemeral) address.
    let addr = {
        let mut found = None;
        for _ in 0..100 {
            if let Some(addr) = *bound.lock().unwrap() {
                found = Some(addr);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        found.expect("run_director reported its bound address via on_ready")
    };

    assert!(addr.ip().is_loopback(), "bound to loopback: {addr}");
    assert_ne!(addr.port(), 0, "an ephemeral port was assigned: {addr}");

    // Hit the real socket: the embedded Director answers /health with a 200 "ok".
    let body = reqwest_get(&format!("http://{addr}/health")).await;
    assert_eq!(
        body, "ok",
        "embedded Director answers /health over loopback"
    );

    // Graceful shutdown via the oneshot trigger, mirroring the app's lifetime model.
    let _ = shutdown_tx.send(());
    server
        .await
        .expect("server task joins")
        .expect("run_director returns Ok after graceful shutdown");
}

/// A tiny dependency-free HTTP GET over a TcpStream — enough to read a short `/health` body
/// without pulling an HTTP client into the app crate's dev-deps.
async fn reqwest_get(url: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = url.strip_prefix("http://").unwrap();
    let (host_port, path) = match addr.find('/') {
        Some(i) => (&addr[..i], &addr[i..]),
        None => (addr, "/"),
    };
    let mut stream = tokio::net::TcpStream::connect(host_port).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    // The body follows the blank line after the headers.
    text.split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string()
}

#[test]
fn asset_status_classifies_the_dir() {
    let missing = std::env::temp_dir().join("gridfpv-director-absent-xyz");
    assert_eq!(asset_status(&missing), AssetStatus::Missing);

    let dir = temp_dir("status");
    assert_eq!(asset_status(&dir), AssetStatus::NoIndex);
    std::fs::write(dir.join("index.html"), "x").unwrap();
    assert_eq!(asset_status(&dir), AssetStatus::Built);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn default_assets_dir_points_at_rd_console_dist() {
    let dir = default_assets_dir();
    assert!(
        dir.ends_with("frontend/apps/rd-console/dist"),
        "default assets resolve under the workspace frontend: {}",
        dir.display()
    );
}
