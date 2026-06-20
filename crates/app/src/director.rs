//! The GridFPV **Director server** wiring (#13, v0.4 Director wiring).
//!
//! This is the app-level glue that turns the protocol [`server`] crate into a runnable
//! Director: it opens the one append-only event log, builds the protocol router over it,
//! serves the built RD console as a static SPA, and applies a permissive CORS layer so a
//! different-origin client (the Windows Tauri RD app) can call the API and open the WS.
//!
//! The **live-timer pipeline is deliberately not here** — this commit only wires the app
//! so a race *can* be served; feeding real timer passes into the log (the RH adapter →
//! engine → `AppState::append`) is the next step (see the crate roadmap / #13 follow-up).
//!
//! # Layout (why a builder, not just `main`)
//!
//! [`build_app`] assembles the full [`axum::Router`] — protocol routes + static SPA +
//! CORS — from an [`AppState`] and a resolved [`Config`]. Keeping it a pure function (no
//! binding, no process exit) lets the Director integration test (`tests/director.rs`)
//! drive the exact same router over an in-memory log via `tower::ServiceExt::oneshot`,
//! with no real socket and no dependency on a built frontend `dist/`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::Router;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use gridfpv_server::app::{AppState, router, smart_fallback};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

/// The default listen address: every interface on port 8080, so the RD console and the
/// LAN spectators reach the Director without extra config.
pub const DEFAULT_ADDR: &str = "0.0.0.0:8080";

/// Resolved Director configuration, read from the environment with sane defaults.
///
/// See [`Config::from_env`] for the env vars; the defaults make `gridfpv` runnable with
/// no configuration at all (in-memory log, bundled console).
#[derive(Debug, Clone)]
pub struct Config {
    /// The socket address to bind (`GRIDFPV_ADDR`, default [`DEFAULT_ADDR`]).
    pub addr: SocketAddr,
    /// Where the durable event log lives (`GRIDFPV_DB`): `None` ⇒ an in-memory SQLite log
    /// (nothing persisted; fresh every start), `Some(path)` ⇒ open/create a file log there.
    pub db: Option<PathBuf>,
    /// The directory of the built RD console SPA to serve (`GRIDFPV_ASSETS`); defaults to
    /// the repo's `frontend/apps/rd-console/dist`. May not exist — the server still serves
    /// the API and logs a warning (see [`build_app`]).
    pub assets: PathBuf,
}

impl Config {
    /// Read the Director config from the environment, applying defaults.
    ///
    /// - `GRIDFPV_ADDR` — listen address (default `0.0.0.0:8080`).
    /// - `GRIDFPV_DB` — SQLite log path; unset ⇒ in-memory (non-durable).
    /// - `GRIDFPV_ASSETS` — RD console `dist/` directory; unset ⇒ the repo's
    ///   `frontend/apps/rd-console/dist`, resolved relative to the workspace root.
    pub fn from_env() -> Result<Self, String> {
        let addr = match std::env::var("GRIDFPV_ADDR") {
            Ok(value) => value.parse().map_err(|e| {
                format!("GRIDFPV_ADDR ({value:?}) is not a valid socket address: {e}")
            })?,
            Err(_) => DEFAULT_ADDR
                .parse()
                .expect("DEFAULT_ADDR is a valid address"),
        };

        let db = match std::env::var("GRIDFPV_DB") {
            Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value)),
            _ => None,
        };

        let assets = match std::env::var("GRIDFPV_ASSETS") {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
            _ => default_assets_dir(),
        };

        Ok(Self { addr, db, assets })
    }
}

/// The default RD console assets directory: `frontend/apps/rd-console/dist` under the
/// workspace root.
///
/// The workspace root is two levels above this crate's manifest dir
/// (`<root>/crates/app`), pinned at compile time via `CARGO_MANIFEST_DIR` so the default
/// resolves regardless of the process's working directory.
pub fn default_assets_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent() // <root>/crates
        .and_then(Path::parent) // <root>
        .unwrap_or(manifest_dir);
    workspace_root.join("frontend/apps/rd-console/dist")
}

/// Build the full Director [`Router`]: the protocol API, the static RD-console SPA, and a
/// permissive CORS layer — all over the shared [`AppState`].
///
/// The protocol router ([`server::app::router`]) is mounted first so its routes (`/health`,
/// `/snapshot/...`, `/stream`, the control surface) take precedence. A
/// [`ServeDir`](tower_http::services::ServeDir) then serves `assets` as the SPA root with a
/// **fallback to `index.html`** so client-side routes (deep links into the console) resolve
/// to the SPA shell instead of 404ing — wrapped in
/// [`smart_fallback`](gridfpv_server::app::smart_fallback) (#64) so a *mistyped API* path
/// (a wrong `/snapshot/...`, `/control/...`, `/auth/...`) returns a typed `ProtocolError`
/// 404 instead of the SPA shell, while genuine client-side routes still resolve to it.
/// Finally [`CorsLayer::permissive`] is applied so a different-origin client (the Tauri RD
/// app) can call the API and upgrade the WS.
///
/// If `assets` does not exist the static service is *still* mounted (so the binary runs
/// without a prior `npm run build`); requests for `/` then yield a 404 from `ServeDir`
/// while the API stays fully functional. Callers should warn when the dir is missing
/// ([`build_app`] does not log — [`crate::director::asset_status`] reports it for `main`).
pub fn build_app(state: AppState, assets: &Path) -> Router {
    // SPA serving: serve files out of `assets`. Any path that does not match a real file
    // (a client-side route like `/heats/q-1/live`) falls back to the SPA shell
    // `index.html` so deep links resolve to the app, not a 404. The fallback is an axum
    // handler (rather than `ServeDir::not_found_service`) so it reliably returns the shell
    // for *any* unmatched path, including nested ones, and a clear 404 when the console
    // has not been built yet.
    let index_html = assets.join("index.html");
    let serve_dir = ServeDir::new(assets).fallback(spa_fallback(index_html));

    router(state)
        // Anything the protocol router does not handle falls through to `smart_fallback`:
        // a mistyped API path → a typed `ProtocolError` 404 (#64), any other path → the SPA.
        .fallback_service(smart_fallback(serve_dir))
        // Permissive CORS so the cross-origin Tauri RD app can reach the API + WS.
        .layer(CorsLayer::permissive())
}

/// Build the SPA-shell fallback service: a handler that returns the contents of
/// `index_html` (the client-side router takes over from there), or a 404 if the console
/// has not been built. Read per-request so a `npm run build` mid-run is picked up.
fn spa_fallback(index_html: PathBuf) -> axum::routing::MethodRouter {
    axum::routing::get(move || {
        let index_html = index_html.clone();
        async move {
            match tokio::fs::read_to_string(&index_html).await {
                Ok(body) => Html(body).into_response(),
                Err(_) => spa_unbuilt_response(),
            }
        }
    })
}

/// The response served when a client route is requested but the RD console has not been
/// built (no `index.html`): the [`UNBUILT_FALLBACK_STATUS`] with a short explanation.
fn spa_unbuilt_response() -> Response {
    (
        UNBUILT_FALLBACK_STATUS,
        "the RD console has not been built — run `cd frontend && npm run build` (the protocol API is still available)",
    )
        .into_response()
}

/// Whether the configured assets directory looks like a built SPA (an `index.html` is
/// present). `main` uses this to print a clear warning when the console has not been built
/// yet, while still serving the API.
pub fn asset_status(assets: &Path) -> AssetStatus {
    if !assets.exists() {
        AssetStatus::Missing
    } else if assets.join("index.html").is_file() {
        AssetStatus::Built
    } else {
        AssetStatus::NoIndex
    }
}

/// The result of inspecting the configured assets directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetStatus {
    /// `index.html` is present — the SPA will be served.
    Built,
    /// The directory does not exist (no `npm run build` yet).
    Missing,
    /// The directory exists but has no `index.html`.
    NoIndex,
}

/// A tiny convenience used by the integration test and conceivable health checks: the
/// HTTP status the SPA fallback yields for an unbuilt assets dir (a 404 from `ServeDir`).
pub const UNBUILT_FALLBACK_STATUS: StatusCode = StatusCode::NOT_FOUND;
