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

use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
#[cfg(not(feature = "embed-assets"))]
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use gridfpv_events::AdapterId;
use gridfpv_server::app::{router, smart_fallback};
use gridfpv_server::events::EventRegistry;
use tower_http::cors::CorsLayer;
#[cfg(not(feature = "embed-assets"))]
use tower_http::services::ServeDir;

use crate::source::{SIM_ADAPTER, SourceConfig, spawn_presence_reconciler, spawn_registry_bridge};

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
    /// Where **persistent events' SQLite files** live (`GRIDFPV_DATA_DIR`) — one file per
    /// created event (issue #72). `None` ⇒ no data dir configured: the built-in Practice
    /// event is always in-memory, and any *created* event falls back to an in-memory log
    /// (non-durable, fresh every start). `Some(dir)` ⇒ created events persist there.
    pub data_dir: Option<PathBuf>,
    /// The directory of the built RD console SPA to serve (`GRIDFPV_ASSETS`); defaults to
    /// the repo's `frontend/apps/rd-console/dist`. May not exist — the server still serves
    /// the API and logs a warning (see [`build_app`]).
    pub assets: PathBuf,
}

impl Config {
    /// Read the Director config from the environment, applying defaults.
    ///
    /// - `GRIDFPV_ADDR` — listen address (default `0.0.0.0:8080`).
    /// - `GRIDFPV_DATA_DIR` — directory for persistent events' SQLite files (one per created
    ///   event, #72); unset ⇒ created events are in-memory (non-durable). The deprecated
    ///   `GRIDFPV_DB` (a single flat log path) is read as a *fallback* — its parent directory
    ///   is used as the data dir — so an old config still persists *somewhere*, though the
    ///   single-flat-log model it named is gone.
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

        let data_dir = match std::env::var("GRIDFPV_DATA_DIR") {
            Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value)),
            _ => match std::env::var("GRIDFPV_DB") {
                // Back-compat: a `GRIDFPV_DB=<file>` resolves its parent dir as the data dir.
                Ok(value) if !value.trim().is_empty() => PathBuf::from(value)
                    .parent()
                    .map(Path::to_path_buf)
                    .filter(|p| !p.as_os_str().is_empty()),
                _ => None,
            },
        };

        let assets = match std::env::var("GRIDFPV_ASSETS") {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
            _ => default_assets_dir(),
        };

        Ok(Self {
            addr,
            data_dir,
            assets,
        })
    }
}

/// What [`run_director`] reports back to its caller once the listener is bound, before
/// it starts serving: the resolved bind address and whether the control path is gated.
///
/// The Director binary uses this to print its startup banner; the Tauri app uses
/// `bound.port()` to learn the ephemeral port it should point the window at.
#[derive(Debug, Clone)]
pub struct DirectorReady {
    /// The address the listener actually bound to. With a `:0` request this carries the
    /// OS-assigned ephemeral port — the only way to learn it.
    pub bound: SocketAddr,
    /// The registered RD token, if `GRIDFPV_RD_TOKEN` configured one; `None` ⇒ control is
    /// open (full-trust, safe on loopback).
    pub rd_token: Option<String>,
    /// A one-line description of the active lap source (for the startup banner).
    pub source_desc: String,
    /// The always-on diagnostics log file this process is writing (#380), or `None` if no
    /// writable log directory could be resolved. Reported here so **every** caller — the
    /// `gridfpv` banner and the Tauri shell alike — can tell the operator where it is without
    /// re-deriving the path.
    pub log_file: Option<PathBuf>,
}

/// Run the GridFPV Director server to completion: open the event registry, register an
/// optional RD token, spawn the built-in lap-source bridge + presence reconciler, build the
/// router, bind `addr`, hand the resolved [`DirectorReady`] to `on_ready`, then serve until
/// `shutdown` resolves.
///
/// This is the **single reusable Director entry point**, shared by:
/// - the `gridfpv` binary (`main.rs`), which passes the env-resolved [`Config`] address, the
///   process Ctrl-C signal as `shutdown`, and an `on_ready` that prints the startup banner —
///   so the binary's behavior is unchanged; and
/// - the Tauri native app, which passes a per-user app-data dir, a `127.0.0.1:0` address
///   (ephemeral free port on loopback ⇒ no auth per the auth model), and an `on_ready` that
///   records `bound.port()` so it can point the window at `http://127.0.0.1:<port>`.
///
/// The token / bridge / presence wiring matches what `serve()` did inline before this was
/// extracted, so both callers get identical Director behavior.
///
/// `addr` is the requested bind address (use a `:0` port for an OS-assigned ephemeral one).
/// `data_dir` is where created events' SQLite files live (`None` ⇒ in-memory, non-durable).
/// `assets` is the built RD-console `dist/` to serve as the SPA. `on_ready` is invoked once,
/// after binding, with the resolved [`DirectorReady`]. `shutdown` is awaited to trigger a
/// graceful stop.
pub async fn run_director<R, S>(
    addr: SocketAddr,
    data_dir: Option<PathBuf>,
    assets: PathBuf,
    on_ready: R,
    shutdown: S,
) -> Result<(), Box<dyn std::error::Error>>
where
    R: FnOnce(&DirectorReady),
    S: Future<Output = ()> + Send + 'static,
{
    // Open the diagnostics log FIRST, before anything can fail (#380). Every entry point goes
    // through here, so this one line is what guarantees "the Director always writes a log
    // file, every platform, no flags" — including the Windows GUI-subsystem build, where
    // stderr goes nowhere and this file is the only surviving record.
    let log_file = crate::logging::init().map(Path::to_path_buf);

    // Events are first-class containers (#72): the built-in Practice event is always present
    // (in-memory); created events persist as a SQLite file under `data_dir` when set.
    let registry = EventRegistry::new(data_dir)?;

    // Control auth is full-trust (open) by default (#72, Slice 1b): register an RD token only
    // when `GRIDFPV_RD_TOKEN` is set to a non-blank value. On loopback (the Tauri app) this is
    // unset, so control is open — matching the loopback-no-auth model.
    let tokens = registry.tokens();
    let rd_token = match std::env::var("GRIDFPV_RD_TOKEN") {
        Ok(value) if tokens.register_rd_token(&value) => Some(value),
        _ => None,
    };

    // The built-in lap source (default `sim`) + the per-event control→source bridge: each
    // event gets a bridge feeding sim passes into ITS log when a heat goes `Running`. Plus the
    // sim auto-presence reconciler (race redesign Slice 1a). Both run until the process exits.
    let source = SourceConfig::from_env();
    let source_desc = source.describe();
    let _bridge =
        spawn_registry_bridge(registry.clone(), source, AdapterId(SIM_ADAPTER.to_string()));
    let _presence = spawn_presence_reconciler(registry.clone());

    let app = build_app(registry, &assets);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    on_ready(&DirectorReady {
        bound,
        rd_token,
        source_desc,
        log_file,
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
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
///
/// # Embedded assets (`--features embed-assets`)
///
/// When the `embed-assets` feature is enabled the SPA is served from **assets baked into
/// the binary** at compile time (see [`embedded`]) rather than from the filesystem — the
/// `assets` path is then ignored, so the produced binary is self-contained and needs no
/// external dist folder beside it. This is how the native desktop (`gridfpv-desktop`) build
/// becomes a single portable file. The default build (feature off) serves from `assets` on
/// disk exactly as before, keeping the hosted dev loop's rebuild-dist-only workflow intact.
pub fn build_app(registry: EventRegistry, assets: &Path) -> Router {
    router(registry)
        // Where the diagnostics log is (#380). Mounted HERE rather than in the protocol
        // router because the log file is an *app*-level concern (`crate::logging` owns it)
        // and `gridfpv-server` knows nothing about it.
        .route("/diagnostics", get(diagnostics))
        // Anything the protocol router does not handle falls through to `smart_fallback`:
        // a mistyped API path → a typed `ProtocolError` 404 (#64), any other path → the SPA.
        .fallback_service(smart_fallback(spa_service(assets)))
        // Permissive CORS so the cross-origin Tauri RD app can reach the API + WS.
        .layer(CorsLayer::permissive())
}

/// `GET /diagnostics` — **where the log file is** (#380).
///
/// A log nobody can find is not a fix. On the shipped desktop build the RD has no console,
/// no terminal and no idea what `%LOCALAPPDATA%` means, so the console has to be able to tell
/// them: this is the endpoint it reads to render the path (in About / on a timer's `Error`
/// state) and the string they paste into a bug report.
///
/// ```json
/// { "log_file": "…/GridFPV/logs/gridfpv.log", "log_dir": "…/GridFPV/logs" }
/// ```
///
/// Both fields are `null` if no writable log directory could be resolved at all — which the
/// temp-dir fallback in [`crate::logging`] makes effectively unreachable, but the console
/// should render "unavailable" rather than an empty path if it ever happens.
///
/// The response is a `BTreeMap` rather than a `serde_json::json!` literal so this endpoint
/// adds **no dependency** to `gridfpv-app` (`serde_json` is only a dev-dependency here);
/// `BTreeMap` serializes as a JSON object with stable key order.
async fn diagnostics() -> Json<BTreeMap<&'static str, Option<String>>> {
    let path = |p: Option<&Path>| p.map(|p| p.display().to_string());
    Json(BTreeMap::from([
        ("log_file", path(crate::logging::log_file())),
        ("log_dir", path(crate::logging::log_dir())),
    ]))
}

/// Build the SPA-serving service the Director composes into its `smart_fallback`.
///
/// Default build: a [`ServeDir`] over the filesystem `assets`, with an `index.html` fallback
/// for client-side routes. The `assets` dir is read per-request, so a `npm run build` mid-run
/// is picked up without rebuilding the binary — the hosted dev loop relies on this.
#[cfg(not(feature = "embed-assets"))]
fn spa_service(assets: &Path) -> ServeDir<axum::routing::MethodRouter> {
    // SPA serving: serve files out of `assets`. Any path that does not match a real file
    // (a client-side route like `/heats/q-1/live`) falls back to the SPA shell
    // `index.html` so deep links resolve to the app, not a 404. The fallback is an axum
    // handler (rather than `ServeDir::not_found_service`) so it reliably returns the shell
    // for *any* unmatched path, including nested ones, and a clear 404 when the console
    // has not been built yet.
    let index_html = assets.join("index.html");
    ServeDir::new(assets).fallback(spa_fallback(index_html))
}

/// Build the SPA-serving service from **embedded assets** (`--features embed-assets`).
///
/// The `assets` filesystem path is ignored — every file is baked into the binary
/// (see [`embedded`]), so the produced binary self-serves the console with no external dist.
#[cfg(feature = "embed-assets")]
fn spa_service(_assets: &Path) -> Router {
    embedded::router()
}

/// Build the SPA-shell fallback service: a handler that returns the contents of
/// `index_html` (the client-side router takes over from there), or a 404 if the console
/// has not been built. Read per-request so a `npm run build` mid-run is picked up.
#[cfg(not(feature = "embed-assets"))]
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
#[cfg(not(feature = "embed-assets"))]
fn spa_unbuilt_response() -> Response {
    (
        UNBUILT_FALLBACK_STATUS,
        "the RD console has not been built — run `cd frontend && npm run build` (the protocol API is still available)",
    )
        .into_response()
}

/// Serve the RD console SPA from assets **embedded into the binary** at compile time
/// (`--features embed-assets`).
///
/// `rust-embed` bakes the entire `frontend/apps/rd-console/dist` tree into the executable, so
/// the Director self-serves the console with no external dist folder beside it — the basis of
/// the single-file portable / native-desktop build. A request for an embedded file is served
/// with its guessed Content-Type; any other path falls back to the embedded `index.html` so
/// client-side deep links resolve to the SPA shell (mirroring the filesystem `ServeDir`
/// behavior). The embed path is resolved relative to this crate's manifest dir, so the dist
/// must exist at compile time **only** when this feature is on (the Tauri / release-builds
/// workflow builds the frontend first).
#[cfg(feature = "embed-assets")]
mod embedded {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{StatusCode, Uri, header};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use rust_embed::RustEmbed;

    /// The built RD console `dist/` baked into the binary at compile time. The path is
    /// relative to this crate's manifest dir (`crates/app`), reaching the workspace's
    /// `frontend/apps/rd-console/dist`.
    #[derive(RustEmbed)]
    #[folder = "../../frontend/apps/rd-console/dist"]
    struct Assets;

    /// The SPA router over the embedded assets: a path that names an embedded file serves it
    /// (with a guessed Content-Type); anything else serves the embedded `index.html` so
    /// client-side routes resolve to the SPA shell.
    pub fn router() -> Router {
        Router::new()
            .route("/", get(|| async { serve_path("index.html") }))
            .fallback(get(|uri: Uri| async move {
                serve_path(uri.path().trim_start_matches('/'))
            }))
    }

    /// Serve the embedded file at `path`, or fall back to the embedded `index.html` (the SPA
    /// shell) when no such file is embedded — so deep links into the console resolve.
    fn serve_path(path: &str) -> Response {
        if let Some(file) = Assets::get(path) {
            return file_response(path, file.data.into_owned());
        }
        match Assets::get("index.html") {
            Some(index) => file_response("index.html", index.data.into_owned()),
            None => (
                StatusCode::NOT_FOUND,
                "the RD console was not embedded — the binary was built without the frontend dist",
            )
                .into_response(),
        }
    }

    /// Build a response for an embedded file: its bytes with a Content-Type guessed from the
    /// path's extension (defaulting to `application/octet-stream`).
    fn file_response(path: &str, bytes: Vec<u8>) -> Response {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        (
            [(header::CONTENT_TYPE, mime.as_ref().to_string())],
            Body::from(bytes),
        )
            .into_response()
    }
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

/// Whether this binary was built with `--features embed-assets`, i.e. the RD console SPA is
/// baked into the executable and served from memory (the filesystem `assets` dir / the
/// `GRIDFPV_ASSETS` env var are ignored). The Director binary uses this to print an accurate
/// startup banner — when `true`, an absent on-disk dist is **not** a problem.
pub const ASSETS_EMBEDDED: bool = cfg!(feature = "embed-assets");
