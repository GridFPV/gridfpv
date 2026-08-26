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

use gridfpv_app::director::{ASSETS_EMBEDDED, AssetStatus, Config, asset_status, run_director};
use gridfpv_app::{SyntheticPilot, append_and_project, render_lap_list, synthetic_session};
use gridfpv_storage::SqliteLog;

/// Mirror this binary's console output into the always-on log file (#380).
///
/// `gridfpv_app`'s `logging` module shadows `eprintln!` for the *library* crate; a binary is
/// its own crate root, so it declares its own shadows. `println!` is shadowed too (not just
/// `eprintln!`) because everything this binary prints is the **startup banner** — bound
/// address, RD-token state, data dir, active lap source, log path — which is exactly the
/// context a field log needs sitting above the errors. `macro_rules!` scope is textual, so
/// these must stay above every use below.
macro_rules! println {
    () => { gridfpv_app::logging::record_stdout(::std::format_args!("")) };
    ($($arg:tt)*) => { gridfpv_app::logging::record_stdout(::std::format_args!($($arg)*)) };
}

macro_rules! eprintln {
    ($($arg:tt)*) => { gridfpv_app::logging::record(::std::format_args!($($arg)*)) };
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Open the log file before anything else can fail. `run_director` does this too (so the
    // Tauri shell is covered), but the `demo` path below returns before ever reaching it, and
    // a config error from `Config::from_env` must land in the file as well.
    gridfpv_app::logging::init();

    // Dispatch the offline `demo` subcommand synchronously; the Director server runs on a
    // tokio runtime we build by hand (so `demo` needs no async runtime at all).
    if std::env::args().nth(1).as_deref() == Some("demo") {
        return run_demo();
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(serve());
    if let Err(err) = &result {
        // A fatal Director error otherwise only reaches the process-exit message on stderr —
        // invisible on a GUI-subsystem build, and the single most useful line in the log.
        eprintln!("gridfpv: FATAL — the Director exited with an error: {err}");
    }
    result
}

/// Open the log, wire the Director, print the login details, and serve until shutdown.
///
/// The actual Director wiring (registry, token, source bridge, presence reconciler, router,
/// bind, serve) lives in the reusable [`gridfpv_app::director::run_director`] — shared with
/// the Tauri native app — so this binary's behavior is identical to when the wiring was
/// inline here. This function only resolves the env config and prints the startup banner via
/// the `on_ready` callback.
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let banner_config = config.clone();

    run_director(
        config.addr,
        config.data_dir,
        config.assets,
        // Print the same startup banner once the listener is bound, before serving.
        move |ready| {
            print_startup(
                &banner_config,
                ready.bound,
                ready.rd_token.as_deref(),
                &ready.source_desc,
                ready.log_file.as_deref(),
            );
        },
        // Serve until Ctrl-C.
        shutdown_signal(),
    )
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
    log_file: Option<&std::path::Path>,
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
    // #414 removed the built-in in-memory Practice event: a fresh Director now starts with NO
    // events, and the RD creates one (with a practice round in it). Saying otherwise here sent
    // an operator looking for an event that is not there.
    match &config.data_dir {
        Some(dir) => println!("  events       : persist under {}", dir.display()),
        None => println!(
            "  events       : in-memory \
             (non-durable — set GRIDFPV_DATA_DIR to persist)"
        ),
    }
    // Where the log is (#380) — printed in the banner AND served at `GET /diagnostics`, so
    // "send me the log" has an answer whether the operator has a console or only the console
    // UI. This very line is itself in the file.
    match log_file {
        Some(path) => println!("  log file     : {}", path.display()),
        None => {
            println!("  log file     : (unavailable — no writable log dir; set GRIDFPV_LOG_DIR)")
        }
    }
    println!("  lap source   : {source_desc}");
    println!(
        "    (drive a heat to Running via the control path to see synthetic laps; set \
         GRIDFPV_SOURCE / GRIDFPV_SIM_LAPS / GRIDFPV_SIM_LAP_MS to tune)"
    );
    // When built with `embed-assets`, the SPA is baked into the binary — the on-disk
    // `assets` dir / `GRIDFPV_ASSETS` are ignored, so report the embedded source rather than
    // inspecting (and possibly warning about) a filesystem dist that isn't used.
    if ASSETS_EMBEDDED {
        println!("  RD console   : serving SPA from assets embedded in the binary");
    } else {
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
