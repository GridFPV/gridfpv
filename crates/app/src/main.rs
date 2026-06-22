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
use gridfpv_app::source::{
    SIM_ADAPTER, SourceConfig, spawn_presence_reconciler, spawn_registry_bridge,
};
use gridfpv_app::{SyntheticPilot, append_and_project, render_lap_list, synthetic_session};
use gridfpv_events::AdapterId;
use gridfpv_server::events::EventRegistry;
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

    // Build the event registry — events are first-class containers (#72), each with its own
    // log. The built-in **Practice** event (in-memory, non-persistent) is always present;
    // events created via `POST /events` get a SQLite file under the configured data dir.
    let registry = EventRegistry::new(config.data_dir.clone())?;

    // Control auth is **full-trust (open) by default** (issue #72, Slice 1b): the control
    // path requires no credential unless one is *configured*. So we only register an RD token
    // when `GRIDFPV_RD_TOKEN` is set to a non-blank value — a *known* credential so an
    // automated/remote client (the Tauri app, a token-gated deployment) can log in
    // deterministically; with the env unset the Director registers **no** token and control is
    // open (safe on loopback / a trusted LAN). The proper loopback-trust + remote-passphrase
    // split for production is tracked separately as #80.
    let tokens = registry.tokens();
    let rd_token = match std::env::var("GRIDFPV_RD_TOKEN") {
        Ok(value) if tokens.register_rd_token(&value) => Some(value),
        _ => None,
    };

    // Resolve the built-in lap source (default `sim`) and spawn the **per-event**
    // control→source bridge over the registry: each event (Practice + any created event) gets
    // its own bridge feeding sim passes into ITS own log when a heat goes `Running` there. It
    // runs until the process exits (see [`gridfpv_app::source::spawn_registry_bridge`]).
    let source = SourceConfig::from_env();
    let source_desc = source.describe();
    let _bridge =
        spawn_registry_bridge(registry.clone(), source, AdapterId(SIM_ADAPTER.to_string()));

    // Spawn the **sim auto-presence reconciler** (race redesign Slice 1a): per event it tails the
    // log for the sim adapter's `CompetitorSeen` and auto-adds + binds any seen player whose name
    // matches a directory pilot's callsign — so a sim race needs no manual roster/registration.
    let _presence = spawn_presence_reconciler(registry.clone());

    let app = build_app(registry, &config.assets);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;
    let bound = listener.local_addr()?;
    print_startup(&config, bound, rd_token.as_deref(), &source_desc);

    // Serve until Ctrl-C; the protocol API + the RD console SPA are live on `bound`.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    println!("gridfpv: shutting down");
    Ok(())
}

/// Print the Director's login details on startup: the URL, the RD token (so the RD can
/// authenticate the control path), the log backing, and the asset-dir status. With no token
/// configured (`rd_token` is `None`) the control path is **open** (full-trust by default,
/// #72) — print that instead of a token.
fn print_startup(
    config: &Config,
    bound: std::net::SocketAddr,
    rd_token: Option<&str>,
    source_desc: &str,
) {
    // For the console URL prefer a loopback-friendly host when bound to all interfaces.
    let url_host = if bound.ip().is_unspecified() {
        format!("127.0.0.1:{}", bound.port())
    } else {
        bound.to_string()
    };

    println!("GridFPV Director {} — serving", env!("CARGO_PKG_VERSION"));
    println!("  listening on : http://{bound}");
    println!("  console URL  : http://{url_host}/");
    match rd_token {
        Some(rd_token) => {
            println!("  RD token     : {rd_token}");
            println!("    (use as `Authorization: Bearer {rd_token}` on the control path)");
        }
        None => {
            println!("  RD token     : (none — control is OPEN, full-trust)");
            println!(
                "    (no GRIDFPV_RD_TOKEN configured: the control path requires no credential — \
                 safe on loopback / a trusted LAN; set GRIDFPV_RD_TOKEN to gate control)"
            );
        }
    }
    match &config.data_dir {
        Some(dir) => println!(
            "  events       : Practice (in-memory) + created events persist under {}",
            dir.display()
        ),
        None => println!(
            "  events       : Practice (in-memory); created events in-memory \
             (non-durable — set GRIDFPV_DATA_DIR to persist)"
        ),
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
