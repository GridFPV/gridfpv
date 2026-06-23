# GridFPV native desktop app (Tauri 2)

`src-tauri` is the **release form** of GridFPV: a native desktop application that embeds the
Director and opens the RD console in a native window. It is **purely additive** — it does
**not** replace the hosted development workflow.

> **Develop against the hosted Director, not this app.** The team's fast dev loop is
> unchanged:
>
> ```sh
> cargo build -p gridfpv-app --features live   # builds the `gridfpv` Director binary
> ./target/debug/gridfpv                        # serves API + RD console on :8080
> ```
>
> The native app is how the **maintainer runs a native build** (e.g. over RDP, which has a
> display). Day-to-day work stays on the hosted Director.

## How it works

On launch the app:

1. Builds a multi-thread tokio runtime and spawns the **same** Director the hosted `gridfpv`
   binary runs — via the shared `gridfpv_app::director::run_director` — as a background task.
   It binds **loopback on an ephemeral free port** (`127.0.0.1:0`), uses a **per-user
   app-data dir** for created events' SQLite files, and serves the **bundled** `rd-console`
   dist as the SPA.
2. Waits for the Director to report its OS-assigned port, then opens the main window pointed
   at `http://127.0.0.1:<port>/`.

Because the window loads the Director's HTTP origin directly (rather than the bundled assets
over `tauri://`), the SPA's **same-origin** API calls (`/snapshot/...`, `/control/...`) and
the realtime WebSocket work without any cross-origin shims.

**Loopback ⇒ no auth.** Per the GridFPV auth model (loopback = trusted), the control path is
open in the native app — there is no passphrase prompt. (Remote/passphrase gating applies
only to non-loopback deployments.)

### Why the Director is shared, not duplicated

The Director's serve entry point was extracted into one reusable function,
`gridfpv_app::director::run_director(addr, data_dir, assets, on_ready, shutdown)`, called by
**both**:

- the hosted `gridfpv` binary (`crates/app/src/main.rs`) — env-resolved address, Ctrl-C
  shutdown, prints the startup banner; behavior unchanged; and
- this native app (`src-tauri/src/lib.rs`) — `127.0.0.1:0`, per-user data dir, bundled
  assets, and an `on_ready` that reads the ephemeral port.

So the two share identical Director wiring (event registry, optional RD token, sim
lap-source bridge, presence reconciler, router, CORS).

## Data-dir location

Created events persist as SQLite files under the OS per-user **app-data** directory (Tauri's
`app_data_dir`):

| OS      | Path                                                          |
| ------- | ------------------------------------------------------------ |
| Linux   | `~/.local/share/org.gridfpv.desktop/`                        |
| Windows | `%APPDATA%\org.gridfpv.desktop\` (deferred — not yet built)  |
| macOS   | `~/Library/Application Support/org.gridfpv.desktop/`          |

The built-in **Practice** event is always in-memory (non-persistent), matching the hosted
Director.

## Building

Prerequisites (Linux / Ubuntu 24.04):

```sh
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev build-essential curl wget file libssl-dev \
  libayatana-appindicator3-dev
cargo install tauri-cli --version '^2' --locked   # provides `cargo tauri`
```

Build the bundles (the `beforeBuildCommand` builds the frontend dist first):

```sh
cd src-tauri
cargo tauri build
```

Artifacts land under `src-tauri/target/release/bundle/`:

- `appimage/GridFPV_<version>_amd64.AppImage`
- `deb/GridFPV_<version>_amd64.deb`

> **CI note:** `src-tauri` is **excluded** from the root Cargo workspace, so
> `cargo xtask ci` (which runs `cargo {clippy,test} --all` / `--workspace`) never tries to
> compile it. CI runners have no webkit2gtk/GTK system libs — the Tauri build is a separate,
> deliberate command. The hosted Director (`gridfpv-app`) stays in the workspace and is
> unaffected.

## Running the produced artifact

```sh
# AppImage (self-contained):
chmod +x src-tauri/target/release/bundle/appimage/GridFPV_*_amd64.AppImage
./src-tauri/target/release/bundle/appimage/GridFPV_*_amd64.AppImage

# or install the .deb:
sudo dpkg -i src-tauri/target/release/bundle/deb/GridFPV_*_amd64.deb
gridfpv-desktop
```

A native window opens with the RD console, backed by the in-process Director on a private
loopback port. The maintainer runs this over RDP (which provides a display).

> **Headless note:** the GUI window needs a display, so on a headless VM the window itself
> can't render. The **server half** (the embedded Director) needs no display and is covered
> by an integration test — `crates/app/tests/director.rs ::
> run_director_serves_health_on_loopback_ephemeral_port` — which drives the exact
> `run_director` entry point the app uses, binds a loopback ephemeral port, and asserts
> `/health` answers over a real socket. The full app can also be smoke-launched headless with
> `xvfb-run` (set `WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1` to avoid the
> WebKit GPU path hanging under Xvfb).

## Windows

Windows packaging is **deferred**. The crate is Windows-ready in principle (the
`windows_subsystem = "windows"` attribute is set for release builds), but no Windows artifact
is produced here and **no cross-compile is attempted** — it needs a Windows build host with
the WebView2 runtime and the MSVC toolchain.
