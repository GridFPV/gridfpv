//! GridFPV Director binary — the Director server entry point (#13).
//!
//! Run with no arguments to start the Director: it opens the event log, builds the
//! protocol read/realtime + control API, serves the built RD console SPA, mints an RD
//! token, and prints the token + URL + asset dir so the RD can log in. Configuration is
//! from the environment (`GRIDFPV_ADDR` / `GRIDFPV_DB` / `GRIDFPV_ASSETS`); see
//! [`gridfpv_app::director::Config`].
//!
//! `gridfpv demo` runs the original walking-skeleton fold (synthetic session → log →
//! projection → printed lap list), kept from v0.1 for a quick offline smoke.
//!
//! The live-timer pipeline (RH adapter → engine → log append) is a separate later step
//! and is intentionally not wired here — this binary only stands the Director up so a
//! race can be served.
#![forbid(unsafe_code)]

use gridfpv_app::director::{AssetStatus, Config, asset_status, build_app};
use gridfpv_app::source::{SIM_ADAPTER, SourceConfig, spawn_bridge};
use gridfpv_app::{SyntheticPilot, append_and_project, render_lap_list, synthetic_session};
use gridfpv_events::AdapterId;
use gridfpv_server::app::AppState;
use gridfpv_storage::SqliteLog;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Dispatch the offline `demo` subcommand synchronously; the Director server runs on a
    // tokio runtime we build by hand (so `demo` needs no async runtime at all).
    if std::env::args().nth(1).as_deref() == Some("demo") {
        return run_demo();
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve())
}

/// Open the log, wire the Director, print the login details, and serve until shutdown.
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;

    // Open the event log: a file-backed SQLite log when `GRIDFPV_DB` is a path, else an
    // in-memory one (non-durable, fresh each start). Either way it is the one append-only
    // log every protocol path shares.
    let state = match &config.db {
        Some(path) => {
            let log = SqliteLog::open(path)?;
            AppState::new(log)
        }
        None => AppState::new(SqliteLog::open_in_memory()?),
    };

    // Mint the RD's control token up front and print it with the URL so the RD console (or
    // the Tauri app) can log straight in. The store is in-memory, so this is the token for
    // *this* run of the process.
    let rd_token = state.tokens().issue_rd_token();

    // Resolve the built-in lap source (default `sim`) and spawn the control→source bridge
    // alongside the server: it shares this same `AppState`/log, watches for heats driven to
    // `Running` through the control path, and appends synthetic laps for them in real time
    // (see [`gridfpv_app::source`]). It runs until the process exits.
    let source = SourceConfig::from_env();
    let source_desc = source.describe();
    let _bridge = spawn_bridge(state.clone(), source, AdapterId(SIM_ADAPTER.to_string()));

    let app = build_app(state, &config.assets);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    let bound = listener.local_addr()?;
    print_startup(&config, bound, &rd_token, &source_desc);

    // Serve until Ctrl-C; the protocol API + the RD console SPA are live on `bound`.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    println!("gridfpv: shutting down");
    Ok(())
}

/// Print the Director's login details on startup: the URL, the RD token (so the RD can
/// authenticate the control path), the log backing, and the asset-dir status.
fn print_startup(config: &Config, bound: std::net::SocketAddr, rd_token: &str, source_desc: &str) {
    // For the console URL prefer a loopback-friendly host when bound to all interfaces.
    let url_host = if bound.ip().is_unspecified() {
        format!("127.0.0.1:{}", bound.port())
    } else {
        bound.to_string()
    };

    println!("GridFPV Director {} — serving", env!("CARGO_PKG_VERSION"));
    println!("  listening on : http://{bound}");
    println!("  console URL  : http://{url_host}/");
    println!("  RD token     : {rd_token}");
    println!("    (use as `Authorization: Bearer {rd_token}` on the control path)");
    match &config.db {
        Some(path) => println!("  event log    : sqlite file {}", path.display()),
        None => {
            println!("  event log    : in-memory sqlite (non-durable — set GRIDFPV_DB to persist)")
        }
    }
    println!("  lap source   : {source_desc}");
    println!(
        "    (drive a heat to Running via the control path to see synthetic laps; set \
         GRIDFPV_SOURCE / GRIDFPV_SIM_LAPS / GRIDFPV_SIM_LAP_MS to tune)"
    );
    let assets = config.assets.display();
    match asset_status(&config.assets) {
        AssetStatus::Built => println!("  RD console   : serving SPA from {assets}"),
        AssetStatus::Missing => println!(
            "  RD console   : WARNING — assets dir not found at {assets}; serving the API only \
             (run `cd frontend && npm run build`, or set GRIDFPV_ASSETS)"
        ),
        AssetStatus::NoIndex => println!(
            "  RD console   : WARNING — {assets} has no index.html; serving the API only \
             (is this the rd-console dist?)"
        ),
    }
}

/// Resolve when the process is asked to stop (Ctrl-C), so `axum::serve` can shut down
/// gracefully.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// The v0.1 walking-skeleton demo: synthetic session → log → projection → printed laps.
fn run_demo() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "GridFPV {} — walking-skeleton demo\n",
        env!("CARGO_PKG_VERSION")
    );

    let events = synthetic_session(
        "sim",
        &[
            SyntheticPilot {
                name: "Ace",
                lap_micros: &[30_000_000, 31_000_000, 29_500_000],
            },
            SyntheticPilot {
                name: "Bee",
                lap_micros: &[33_000_000, 32_250_000],
            },
        ],
    );

    // Append to a real (in-memory) SQLite log, then derive the read model from it.
    let mut log = SqliteLog::open_in_memory()?;
    let laps = append_and_project(&mut log, &events)?;

    print!("{}", render_lap_list(&laps));
    Ok(())
}
