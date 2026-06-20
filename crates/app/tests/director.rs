//! Director-server wiring tests (#13, v0.4 Director wiring).
//!
//! These drive the *exact* router `main` serves — [`gridfpv_app::director::build_app`]
//! over an [`AppState`] — with no real socket, via `tower::ServiceExt::oneshot`. They
//! assert the protocol API is reachable (a `GET /health` 200) and that the static SPA is
//! served with an `index.html` fallback when `GRIDFPV_ASSETS` points at a built dir. The
//! committed tests never depend on the real frontend `dist/`: the SPA case writes its own
//! tiny `index.html` into a temp dir.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use gridfpv_app::director::{AssetStatus, asset_status, build_app, default_assets_dir};
use gridfpv_server::app::AppState;
use gridfpv_storage::InMemoryLog;
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
    let state = AppState::new(InMemoryLog::new());
    let app = build_app(state, assets);
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
    let (status, _body) = get(&assets, "/snapshot/event/spring-cup").await;
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

    let state = AppState::new(InMemoryLog::new());
    let app = build_app(state, &dir);
    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/snapshot/event/spring-cup")
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
