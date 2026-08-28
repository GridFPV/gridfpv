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
   It binds **loopback on an ephemeral free port** (`127.0.0.1:0`), uses a **true-portable
   data dir** (`gridfpv-data/` next to the executable, with a per-user app-data fallback) for
   created events' SQLite files, and serves the **bundled** `rd-console` dist as the SPA.
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

This is a **true-portable** build: created events persist as SQLite files in a
**`gridfpv-data/` folder next to the running executable**. Copy the executable to a USB stick
or any folder and its data travels with it — nothing is written to per-user locations as long
as the executable's directory is writable.

**Fallback.** If the executable's directory isn't writable (e.g. it's run from a read-only
mount, or `current_exe()` can't be resolved), the app falls back to the OS per-user
**app-data** directory (Tauri's `app_data_dir`) and logs which location is in use:

| OS      | Fallback path                                                |
| ------- | ------------------------------------------------------------ |
| Linux   | `~/.local/share/org.gridfpv.desktop/`                        |
| Windows | `%APPDATA%\org.gridfpv.desktop\` (deferred — not yet built)  |
| macOS   | `~/Library/Application Support/org.gridfpv.desktop/`          |

On startup the app prints one of:

```
gridfpv-desktop: using PORTABLE data dir (next to executable): <path>/gridfpv-data
gridfpv-desktop: exe dir not writable — using per-user app-data dir: <path>
```

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

Build the **portable** binary (the `beforeBuildCommand` builds the frontend dist first). This
is a **portable-only** build — no installers (msi/nsis/appimage/deb) are produced — so pass
`--no-bundle`:

```sh
cd src-tauri
cargo tauri build --no-bundle
```

The single self-contained executable lands at:

- `src-tauri/target/release/gridfpv-desktop` (Linux)
- `src-tauri/target/release/gridfpv-desktop.exe` (Windows)

Because `gridfpv-app` is built with the `embed-assets` feature, the frontend dist is baked
into the binary — so it self-serves the RD console with no external assets folder, and writes
its data to `gridfpv-data/` beside itself (see **Data-dir location** above).

> **CI note:** `src-tauri` is **excluded** from the root Cargo workspace, so
> `cargo xtask ci` (which runs `cargo {clippy,test} --all` / `--workspace`) never tries to
> compile it. CI runners have no webkit2gtk/GTK system libs — the Tauri build is a separate,
> deliberate command. The hosted Director (`gridfpv-app`) stays in the workspace and is
> unaffected.

## Running the produced artifact

```sh
# Portable single-file binary (self-contained):
chmod +x src-tauri/target/release/gridfpv-desktop
./src-tauri/target/release/gridfpv-desktop
```

A native window opens with the RD console, backed by the in-process Director on a private
loopback port. The maintainer runs this over RDP (which provides a display). A `gridfpv-data/`
folder appears next to the binary, holding created events' SQLite files.

> **Headless note:** the GUI window needs a display, so on a headless VM the window itself
> can't render. The **server half** (the embedded Director) needs no display and is covered
> by an integration test — `crates/app/tests/director.rs ::
> run_director_serves_health_on_loopback_ephemeral_port` — which drives the exact
> `run_director` entry point the app uses, binds a loopback ephemeral port, and asserts
> `/health` answers over a real socket. The full app can also be smoke-launched headless with
> `xvfb-run` (set `WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1` to avoid the
> WebKit GPU path hanging under Xvfb).

## Linux rendering glitches (flicker, blank window)

GridFPV renders through **WebKitGTK** on Linux, whose accelerated compositing misbehaves on
virtualized and NVIDIA GPUs — colour flicker in large filled boxes, or a blank window. If you
hit that, opt into a mitigation with `GRIDFPV_LINUX_GPU_WORKAROUND` (#476):

```sh
# Try this FIRST — the targeted fix, and the cheaper of the two.
GRIDFPV_LINUX_GPU_WORKAROUND=dmabuf ./gridfpv-desktop

# The heavier hammer: disables accelerated compositing outright.
GRIDFPV_LINUX_GPU_WORKAROUND=compositing ./gridfpv-desktop

GRIDFPV_LINUX_GPU_WORKAROUND=all ./gridfpv-desktop
```

They map to `WEBKIT_DISABLE_DMABUF_RENDERER=1` and `WEBKIT_DISABLE_COMPOSITING_MODE=1`, and the
app logs which it applied.

**Nothing is on by default, deliberately.** Both variables give up a faster rendering path for
*everyone*, and Tauri's Linux graphics guidance is explicit that an unconditional override should
only ship once the app is verified to be affected. The flicker has so far only been seen in a VM,
never reproduced on bare metal, and the primary RD machines are unaffected — so this stays an
opt-in switch you can A/B without a rebuild. A variable you have already exported yourself always
wins; this never overwrites it.

## Windows

Windows packaging is **deferred**. The crate is Windows-ready in principle (the
`windows_subsystem = "windows"` attribute is set for release builds), but no Windows artifact
is produced here and **no cross-compile is attempted** — it needs a Windows build host with
the WebView2 runtime and the MSVC toolchain.
