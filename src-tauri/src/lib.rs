//! GridFPV native desktop app (Tauri 2) — the **release form** of the Director.
//!
//! This is purely additive packaging: it does **not** replace the hosted development
//! workflow. The team still develops against the hosted Director
//! (`cargo build -p gridfpv-app --features live`, serving `:8080`). This app is how the
//! maintainer runs a *native* build.
//!
//! # What it does on launch
//!
//! 1. Builds a multi-thread tokio runtime and spawns the **same** Director the hosted
//!    `gridfpv` binary runs — via the shared [`gridfpv_app::director::run_director`] — as a
//!    background task. It binds **loopback on an ephemeral free port** (`127.0.0.1:0`), uses
//!    a **true-portable data dir** (`gridfpv-data/` beside the running executable, with a
//!    per-user app-data fallback when the exe dir isn't writable) for created events, and
//!    serves the `rd-console` SPA **embedded into the binary** (`gridfpv-app`'s `embed-assets`
//!    feature) — so this executable is self-contained, with no external dist folder beside it.
//!    Loopback ⇒ the control path is open (no auth), per the GridFPV auth model (loopback =
//!    trusted).
//! 2. Waits (briefly) for the Director to report the OS-assigned port, then opens the main
//!    window pointed at `http://127.0.0.1:<port>/`.
//!
//! Pointing the webview at the Director's HTTP origin (rather than loading the bundled
//! assets over the `tauri://` protocol) is deliberate: the SPA makes **same-origin**
//! relative calls to the protocol API (`/snapshot/...`, `/control/...`) and opens the
//! realtime WS — those only work if the page and the API share an origin, which they do
//! when the window loads the Director directly.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

/// The Tauri application entry point, invoked by `main.rs` (and re-exported for the mobile
/// `tauri::mobile_entry_point` shape, should a mobile shell ever be added).
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log_shim())
        .setup(|app| {
            let handle = app.handle().clone();

            // Resolve the portable data dir (created events' SQLite files live here): a
            // `gridfpv-data/` folder beside the running executable, with a per-user app-data
            // fallback when the exe's directory isn't writable. The RD-console SPA is
            // **embedded in the binary** (`gridfpv-app`'s `embed-assets` feature), so there is
            // no on-disk dist to resolve — the Director serves it from memory. We still pass an
            // `assets` path to `run_director` (it's part of the shared signature), but with
            // embedded assets the path is ignored. See `gridfpv_app::director::ASSETS_EMBEDDED`.
            let data_dir = resolve_data_dir(&handle);
            let assets_dir = PathBuf::new();

            // A dedicated multi-thread tokio runtime hosts the in-process Director. We keep
            // the runtime alive for the whole app lifetime by leaking its handle into app
            // state (the `JoinHandle` of the serve task keeps the server running).
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to build the Director tokio runtime");

            // `oneshot` carries the OS-assigned ephemeral port from the Director's
            // `on_ready` (inside the runtime) back out to the setup thread so we can open the
            // window at the right URL.
            let (port_tx, port_rx) = oneshot::channel::<u16>();

            // Spawn the Director on the runtime. Bind `127.0.0.1:0` ⇒ loopback + ephemeral
            // free port (no auth, per the auth model). It runs until the process exits; the
            // app quitting tears the runtime (and the server) down.
            runtime.spawn(async move {
                let mut port_tx = Some(port_tx);
                let result = gridfpv_app::director::run_director(
                    SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                    Some(data_dir),
                    assets_dir,
                    move |ready| {
                        // Hand the bound ephemeral port back to the setup thread.
                        if let Some(tx) = port_tx.take() {
                            let _ = tx.send(ready.bound.port());
                        }
                    },
                    // No graceful-shutdown trigger: the server lives for the app's lifetime
                    // and is torn down when the process exits.
                    std::future::pending::<()>(),
                )
                .await;
                if let Err(err) = result {
                    eprintln!("gridfpv-desktop: Director exited with error: {err}");
                }
            });

            // Block (briefly) for the Director to bind and report its port. This runs on the
            // setup thread, not the tokio runtime, so it cannot deadlock the server task.
            let port = port_rx
                .blocking_recv()
                .expect("the embedded Director failed to report its bound port");

            let url = format!("http://{}:{port}/", Ipv4Addr::LOCALHOST);
            eprintln!("gridfpv-desktop: embedded Director ready — opening {url}");

            // Open the main window pointed at the Director's HTTP origin so the SPA's
            // same-origin API/WS calls work.
            WebviewWindowBuilder::new(
                &handle,
                "main",
                WebviewUrl::External(url.parse().expect("Director URL is valid")),
            )
            .title("GridFPV")
            .inner_size(1280.0, 800.0)
            .min_inner_size(900.0, 600.0)
            .build()?;

            // Keep the runtime alive for the whole app lifetime.
            app.manage(DirectorRuntime(runtime));

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the GridFPV desktop app");
}

/// Owns the tokio runtime hosting the in-process Director; held in Tauri-managed state so it
/// (and the serve task) live for the whole app lifetime.
struct DirectorRuntime(#[allow(dead_code)] tokio::runtime::Runtime);

/// The bundled-log plugin is optional; this returns a no-op shim plugin so the builder chain
/// reads cleanly and a real logging plugin can slot in later without restructuring `setup`.
fn tauri_plugin_log_shim() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("gridfpv-noop").build()
}

/// Resolve the data directory for created events' SQLite files (created if needed).
///
/// **True-portable first:** prefer a `gridfpv-data/` folder *beside the running executable*
/// (`current_exe()`'s parent), so a portable copy carries its own data. If that directory
/// can't be resolved or isn't writable (e.g. the exe lives on a read-only mount, or
/// `current_exe()` fails), fall back to the **per-user app-data** dir. Either way the chosen
/// location is logged so it's obvious where data lives. The final fallback is a temp dir, so
/// this is never fatal.
fn resolve_data_dir(handle: &tauri::AppHandle) -> PathBuf {
    if let Some(dir) = portable_data_dir() {
        eprintln!(
            "gridfpv-desktop: using PORTABLE data dir (next to executable): {}",
            dir.display()
        );
        return dir;
    }

    let dir = handle
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("gridfpv"));
    let _ = std::fs::create_dir_all(&dir);
    eprintln!(
        "gridfpv-desktop: exe dir not writable — using per-user app-data dir: {}",
        dir.display()
    );
    dir
}

/// Try to resolve (and create) the portable `gridfpv-data/` dir beside the executable,
/// returning `Some(path)` only if it exists and is **writable**. Returns `None` when
/// `current_exe()` fails, has no parent, or the directory can't be created / written to —
/// signalling the caller to fall back to the per-user app-data dir.
fn portable_data_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("gridfpv-data");
    std::fs::create_dir_all(&dir).ok()?;
    if is_writable(&dir) {
        Some(dir)
    } else {
        None
    }
}

/// Probe whether `dir` is writable by creating (and immediately removing) a marker file.
/// Cheap and reliable across platforms — `std::fs::Permissions` alone can't tell us whether
/// the *filesystem* (vs. the inode) is read-only.
fn is_writable(dir: &std::path::Path) -> bool {
    let marker = dir.join(".gridfpv-write-test");
    match std::fs::File::create(&marker) {
        Ok(_) => {
            let _ = std::fs::remove_file(&marker);
            true
        }
        Err(_) => false,
    }
}
