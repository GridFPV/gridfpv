//! GridFPV Director binary — the Director server entry point.
//!
//! Run it to start the Director: it opens the event registry over the data dir, builds the
//! API, serves the built RD console SPA, and prints the URL, the RD-token state, the data
//! dir and the log file. Configuration is from the environment (`GRIDFPV_ADDR` /
//! `GRIDFPV_DATA_DIR` / `GRIDFPV_ASSETS` / `GRIDFPV_RD_TOKEN`); see
//! [`gridfpv_app::director::Config`].
#![forbid(unsafe_code)]

use gridfpv_app::director::{ASSETS_EMBEDDED, AssetStatus, Config, asset_status, run_director};

/// Mirror this binary's console output into the always-on log file (#380).
///
/// `gridfpv_app`'s `logging` module shadows `eprintln!` for the *library* crate; a binary is
/// its own crate root, so it declares its own shadows. `println!` is shadowed too (not just
/// `eprintln!`) because everything this binary prints is the **startup banner** — bound
/// address, RD-token state, data dir, log path — which is exactly the
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
    // Tauri shell is covered), but a config error from `Config::from_env` must land in the
    // file as well.
    gridfpv_app::logging::init();

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

/// Wire the Director, print the login details, and serve until shutdown.
///
/// The actual Director wiring (registry, token, router, bind, serve) lives in the reusable
/// [`gridfpv_app::director::run_director`] — shared with the Tauri native app. This function
/// only resolves the env config and prints the startup banner via the `on_ready` callback.
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
/// authenticate writes), where events persist, and the asset-dir status. With no token
/// configured (`rd_token` is `None`) writes are **open** (full-trust by default) — print
/// that instead of a token.
fn print_startup(
    config: &Config,
    bound: std::net::SocketAddr,
    rd_token: Option<&str>,
    log_file: Option<&std::path::Path>,
) {
    // For the console URL prefer a loopback-friendly host when bound to all interfaces.
    let url_host = if bound.ip().is_unspecified() {
        format!("127.0.0.1:{}", bound.port())
    } else {
        bound.to_string()
    };

    println!(
        "GridFPV Director {} — serving",
        gridfpv_server::BUILD_VERSION
    );
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
