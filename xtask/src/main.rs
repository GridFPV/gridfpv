//! GridFPV task runner — the single definition of our checks.
//!
//! Local dev runs `cargo xtask ci`; GitLab CI runs the identical `cargo xtask ci`
//! (see `.gitlab-ci.yml`). Keeping the logic here means local and remote can
//! never drift. Pure std + cargo, so it works the same on Windows/Linux/macOS.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, exit};

/// Run a command, echoing it first; returns whether it succeeded.
fn run(program: &str, args: &[&str]) -> bool {
    run_env(program, args, &[])
}

/// Like [`run`], but with extra environment variables set for the child process.
fn run_env(program: &str, args: &[&str], env: &[(&str, &Path)]) -> bool {
    println!("\n\x1b[1m$ {program} {}\x1b[0m", args.join(" "));
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    match cmd.status() {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("failed to launch `{program}`: {err}");
            false
        }
    }
}

/// The workspace root — the parent of this crate's manifest dir (`<root>/xtask`).
/// Used to pin `TS_RS_EXPORT_DIR` and to run git from a known location so `bindings/`
/// resolves to `<root>/bindings/` regardless of the invoking shell's cwd.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest dir always has a parent (the workspace root)")
        .to_path_buf()
}

fn fmt() -> bool {
    run("cargo", &["fmt", "--all", "--", "--check"])
}

fn lint() -> bool {
    // Default features only: the optional `live` feature pulls a network stack
    // (rust_socketio -> openssl) and needs a running RotorHazard, so it is linted
    // + tested in the dedicated `rh-live` CI job, not the core check suite.
    run(
        "cargo",
        &["clippy", "--all-targets", "--", "-D", "warnings"],
    )
}

fn test() -> bool {
    // `cargo test --all` also runs ts-rs's generated export tests, which write the
    // `.ts` files. Pin `TS_RS_EXPORT_DIR` to the workspace root so they land in the
    // canonical `<root>/bindings/` (ts-rs's default base is the *crate's* dir, which
    // would otherwise scatter a stray copy under `crates/events/`).
    run_env(
        "cargo",
        &["test", "--all"],
        &[("TS_RS_EXPORT_DIR", &workspace_root())],
    )
}

/// Regenerate the Rust→TypeScript bindings (#4, #40).
///
/// ts-rs exports its types from a generated `#[test]` per `#[ts(export)]` type, so
/// "generation" is just running the export tests of every crate that derives `TS`:
/// each derived impl writes its `.ts` file. The wire contract now spans four crates —
/// the event model (`gridfpv-events`), the served lap projection (`gridfpv-projection`),
/// the heat results / rankings / event outcome (`gridfpv-engine`), and the protocol
/// wire types themselves (`gridfpv-server`) — so we run the `export_bindings` filter
/// across the whole workspace. `TS_RS_EXPORT_DIR` pins the base directory to the
/// workspace root, so every `export_to = "bindings/"` lands in `<root>/bindings/`
/// regardless of which crate the type lives in.
fn gen_bindings(root: &Path) -> bool {
    run_env(
        "cargo",
        &["test", "--workspace", "--quiet", "export_bindings"],
        &[("TS_RS_EXPORT_DIR", root)],
    )
}

/// `cargo xtask gen` — regenerate the bindings in place.
fn generate() -> bool {
    let root = workspace_root();
    if !gen_bindings(&root) {
        eprintln!("gen: failed to export TypeScript bindings");
        return false;
    }
    println!("gen: TypeScript bindings written to bindings/");
    true
}

/// Drift check for `ci`: regenerate, then fail if the checked-in `bindings/` differ
/// from what the current Rust types produce. Keeps `bindings/*.ts` honest without a
/// human remembering to rerun the generator.
///
/// `git add --intent-to-add` first so a *newly added* binding (an untracked file)
/// shows up in `git diff` rather than being silently ignored; the check then catches
/// added, modified, and deleted bindings alike.
fn gen_check() -> bool {
    let root = workspace_root();
    if !gen_bindings(&root) {
        eprintln!("gen: failed to export TypeScript bindings");
        return false;
    }
    let root_str = root.to_str().expect("workspace root path is valid UTF-8");
    run(
        "git",
        &["-C", root_str, "add", "--intent-to-add", "bindings/"],
    );
    let clean = run(
        "git",
        &["-C", root_str, "diff", "--exit-code", "--", "bindings/"],
    );
    if !clean {
        eprintln!(
            "\n\x1b[31mgen drift: bindings/ is out of date with the Rust types.\x1b[0m\n\
             Run `cargo xtask gen` and commit the updated bindings/*.ts files."
        );
    }
    clean
}

/// `cargo xtask live` — the local-only integration test class.
///
/// Container-dependent tests don't belong in the shared CI pipeline (flaky,
/// resource-limited). These run the `live` feature: the WebSocket mock-server test
/// (no container) and the dockerized-RotorHazard tests (`rh_live` via `simulate_lap`
/// and `rh_signal` via emulated `mock_data` RSSI streams). Each container test spins
/// up and tears down its own disposable RotorHazard, so no external state is needed —
/// just Docker. `--include-ignored` runs the `#[ignore]`d container tests too.
fn live() -> bool {
    // Run each target sequentially so at most one RotorHazard container exists at a
    // time (cargo runs separate test binaries in parallel otherwise).
    let target = |package: &str, name: &str, ignored: bool| {
        let mut args = vec![
            "test",
            "-p",
            package,
            "--features",
            "live",
            "--test",
            name,
            "--",
            "--nocapture",
        ];
        if ignored {
            args.push("--ignored");
        }
        run("cargo", &args)
    };
    // No container needed (in-process mock WS server).
    let ws = target("gridfpv-adapters", "velocidrone_ws", false);
    // Each spins up + tears down its own disposable RotorHazard.
    let live_rh = target("gridfpv-adapters", "rh_live", true);
    let signal = target("gridfpv-adapters", "rh_signal", true);
    // The engine's mock-RH e2e tests: each drives a full heat through the shared
    // harness on its own port (#29 heat loop, #30 scoring, #31 marshaling).
    let heat_live = target("gridfpv-engine", "heat_live", true);
    let scoring_live = target("gridfpv-engine", "scoring_live", true);
    let marshaling_live = target("gridfpv-engine", "marshaling_live", true);
    let format_live = target("gridfpv-engine", "format_live", true);
    let timed_qual_live = target("gridfpv-engine", "timed_qual_live", true);
    let single_elim_live = target("gridfpv-engine", "single_elim_live", true);
    let zippyq_live = target("gridfpv-engine", "zippyq_live", true);
    let multiclass_live = target("gridfpv-engine", "multiclass_live", true);
    let full_event_live = target("gridfpv-engine", "full_event_live", true);
    // Additional format generators (#68 double-elim, #69 round-robin, #70 multi-main).
    let double_elim_live = target("gridfpv-engine", "double_elim_live", true);
    let round_robin_live = target("gridfpv-engine", "round_robin_live", true);
    let multi_main_live = target("gridfpv-engine", "multi_main_live", true);
    // The protocol server's mock-RH e2e: full event → server log → protocol client (#47).
    let server_e2e = target("gridfpv-server", "full_event_live", true);
    // The Director's RH-connect e2e (#65, #73): the per-event bridge connects dockerized RH,
    // drives status to Connected, and feeds real passes into the event log.
    let rh_connect = target("gridfpv-app", "rh_connect_live", true);
    ws && live_rh
        && signal
        && heat_live
        && scoring_live
        && marshaling_live
        && format_live
        && timed_qual_live
        && single_elim_live
        && zippyq_live
        && multiclass_live
        && full_event_live
        && double_elim_live
        && round_robin_live
        && multi_main_live
        && server_e2e
        && rh_connect
}

fn main() {
    let task = std::env::args().nth(1).unwrap_or_else(|| "ci".to_string());
    let ok = match task.as_str() {
        "fmt" => fmt(),
        "lint" | "clippy" => lint(),
        "test" => test(),
        "gen" => generate(),
        "ci" => fmt() && lint() && test() && gen_check(),
        "live" => live(),
        other => {
            eprintln!("unknown task: {other}");
            eprintln!("usage: cargo xtask [ci|fmt|lint|test|gen|live]");
            false
        }
    };
    if !ok {
        exit(1);
    }
}
