//! GridFPV task runner — the single definition of our checks.
//!
//! Local dev runs `cargo xtask ci`; GitHub Actions runs the same stages
//! (see `.github/workflows/ci.yml`). Keeping the logic here means local and remote can
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
    run(
        "cargo",
        &["clippy", "--all-targets", "--", "-D", "warnings"],
    )
}

/// The `live-check` stage: type-check feature-gated test targets that `cargo test` never builds.
///
/// It exists because targets behind a cargo feature (they need a real timer or Docker to run)
/// are invisible to `cargo test --all`: a change to a shared type compiles clean, passes every
/// gate, and then fails minutes into a live run.
///
/// **There is nothing to check right now.** The cut to the skeleton removed every live target
/// along with the RotorHazard adapter, so no crate has a gated feature. The stage stays in `ci`
/// so the slot is not forgotten: when the OFT adapter brings a feature that CI does not enable
/// (`docs/primitives.md` §8), this is where it gets compiled with `--no-run`.
fn live_check() -> bool {
    println!("\n\x1b[1m$ live-check\x1b[0m");
    println!("live-check: nothing to check — no crate has a feature-gated live target yet");
    true
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
/// each derived impl writes its `.ts` file. The wire contract spans two crates — the event
/// model (`gridfpv-events`) and the server's wire types (`gridfpv-server`) — so we run the
/// `export_bindings` filter across the whole workspace. `TS_RS_EXPORT_DIR` pins the base directory to the
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

/// Drift check for `ci`: the **src-tauri lockfile** must be current (#513 follow-up). The
/// desktop app is a standalone workspace whose lock covers the main workspace's crates as path
/// deps — and this dev box never *builds* Tauri (headless), so the lock only ever re-resolved
/// on the CI runners, where cargo rewrote the tracked file mid-build ("Locking 55 packages"),
/// dirtied the tree, and the build stamp truthfully — uselessly — reported every portable as
/// `-dirty`. A stale lock would also demote a *tagged release* build to dev naming. Resolution
/// needs no system deps (`cargo metadata` never compiles), so it runs on every `ci` like the
/// gen/barrel drift twins: refresh, then fail if the tracked lock moved.
fn tauri_lock_check() -> bool {
    let root = workspace_root();
    let root_str = root.to_str().expect("workspace root path is valid UTF-8");
    let src_tauri = root.join("src-tauri");
    let resolved = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(&src_tauri)
        .stdout(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !resolved {
        eprintln!("tauri-lock: `cargo metadata` failed in src-tauri/");
        return false;
    }
    let clean = run(
        "git",
        &[
            "-C",
            root_str,
            "diff",
            "--exit-code",
            "--",
            "src-tauri/Cargo.lock",
        ],
    );
    if !clean {
        eprintln!(
            "\n\x1b[31mtauri-lock drift: src-tauri/Cargo.lock is out of date with the workspace \
             crates.\x1b[0m\nCommit the refreshed lock (this check just rewrote it), or the CI \
             release build re-locks mid-build and stamps the portable -dirty."
        );
    }
    clean
}

/// The frontend barrel that must re-export **every** generated binding (#410).
const BARREL: &str = "frontend/packages/types/src/generated.ts";

/// The one line shape the barrel re-exports a binding with.
const BARREL_PREFIX: &str = "export type * from '@bindings/";

/// The binding name one barrel line re-exports, if it is such a line.
///
/// Presence, not order: the barrel carries one deliberate out-of-alphabetical entry
/// (`VoidReason`, parked beside `LapList`), and the doc comment above the list mentions
/// `@bindings/*` — neither is a re-export, and neither is this check's business.
fn barrel_export(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix(BARREL_PREFIX)?
        .strip_suffix("';")
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
}

/// `(missing, stale)`: generated bindings with no re-export line, and re-export lines
/// naming a binding that no longer exists. Pure, so it is unit-testable with no filesystem.
fn barrel_gaps(bindings: &[String], barrel_src: &str) -> (Vec<String>, Vec<String>) {
    let exported: Vec<&str> = barrel_src.lines().filter_map(barrel_export).collect();
    let missing = bindings
        .iter()
        .filter(|name| !exported.contains(&name.as_str()))
        .cloned()
        .collect();
    let stale = exported
        .iter()
        .filter(|name| !bindings.iter().any(|b| b == *name))
        .map(|name| (*name).to_string())
        .collect();
    (missing, stale)
}

/// Every `bindings/*.ts` type name, sorted.
fn binding_names(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("bindings");
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("ts") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// **Barrel completeness check** for `ci` (#410) — every generated binding is reachable
/// from `@gridfpv/types`.
///
/// The twin of [`gen_check`], one layer out: `gen_check` keeps `bindings/*.ts` honest with
/// the Rust types, and this keeps the frontend's single import seam honest with
/// `bindings/`. A type that is generated but never re-exported is not importable from
/// `@gridfpv/types` at all — which is the hole `TimerSignal` / `NodeSignal` fell through in
/// #410: the Tune page could not import the real wire type even if it wanted to, so it
/// hand-declared one instead, and every gate passed on the fabrication.
///
/// Costs a directory listing and one file read — no cargo, no node, no network.
fn barrel_check() -> bool {
    let root = workspace_root();
    println!("\n\x1b[1m$ barrel check ({BARREL} vs bindings/)\x1b[0m");
    let bindings = match binding_names(&root) {
        Ok(names) => names,
        Err(err) => {
            eprintln!("barrel: cannot read bindings/ — {err}");
            return false;
        }
    };
    let barrel_path = root.join(BARREL);
    let src = match std::fs::read_to_string(&barrel_path) {
        Ok(src) => src,
        Err(err) => {
            eprintln!("barrel: cannot read {} — {err}", barrel_path.display());
            return false;
        }
    };
    let (missing, stale) = barrel_gaps(&bindings, &src);
    if missing.is_empty() && stale.is_empty() {
        println!(
            "barrel: {} generated bindings, all re-exported from {BARREL}",
            bindings.len()
        );
        return true;
    }
    if !missing.is_empty() {
        eprintln!(
            "\n\x1b[31mbarrel gap: {} generated binding(s) are NOT re-exported from \
             {BARREL}.\x1b[0m\n\
             They are not importable from `@gridfpv/types`, which is how a hand-declared (and \
             wrong) copy of a wire type gets written instead — #410.\n\
             Add one line per name:\n{}",
            missing.len(),
            missing
                .iter()
                .map(|name| format!("  {BARREL_PREFIX}{name}';"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    if !stale.is_empty() {
        eprintln!(
            "\n\x1b[31mbarrel gap: {} re-export(s) name a binding that no longer \
             exists.\x1b[0m\n\
             Delete these lines from {BARREL}:\n{}",
            stale.len(),
            stale
                .iter()
                .map(|name| format!("  {BARREL_PREFIX}{name}';"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    false
}

/// `cargo xtask version <x.y.z[-pre.N]>` — set the product version in every file that
/// carries it, keeping them in lock-step (the v0.4.0-alpha.1 scheme): the root workspace
/// `Cargo.toml` (`[workspace.package].version`, which every spine crate inherits), the
/// standalone `src-tauri/Cargo.toml` (excluded from the root workspace, so it cannot
/// inherit), `src-tauri/tauri.conf.json`, and the console `package.json`. With no argument,
/// prints the current version. Refuses a malformed version rather than writing a partial set.
fn version(args: &[String]) -> bool {
    let root = workspace_root_dir();
    let cargo_toml = root.join("Cargo.toml");
    let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap_or_default();
    let current = read(&cargo_toml)
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("version = \"")
                .and_then(|r| r.strip_suffix('"'))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".into());
    let Some(new) = args.first() else {
        println!("{current}");
        return true;
    };
    // A light semver shape check: MAJOR.MINOR.PATCH with an optional -prerelease tail.
    let core = new.split('-').next().unwrap_or("");
    let ok_shape = core.split('.').count() == 3
        && core
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    if !ok_shape {
        eprintln!("version {new:?} is not x.y.z[-pre] shaped");
        return false;
    }
    let targets = [
        (root.join("Cargo.toml"), format!("version = \"{current}\"")),
        (
            root.join("src-tauri/Cargo.toml"),
            format!("version = \"{current}\""),
        ),
        (
            root.join("src-tauri/tauri.conf.json"),
            format!("\"version\": \"{current}\""),
        ),
        (
            root.join("frontend/apps/rd-console/package.json"),
            format!("\"version\": \"{current}\""),
        ),
    ];
    // Verify every site carries the current version BEFORE writing any (no partial bumps).
    for (path, needle) in &targets {
        if !read(path).contains(needle.as_str()) {
            eprintln!(
                "{} does not carry version {current} — refusing a partial bump",
                path.display()
            );
            return false;
        }
    }
    for (path, needle) in &targets {
        let replaced = read(path).replacen(needle.as_str(), &needle.replace(&current, new), 1);
        if std::fs::write(path, replaced).is_err() {
            eprintln!("failed writing {}", path.display());
            return false;
        }
    }
    println!("{current} -> {new}");
    true
}

fn workspace_root_dir() -> std::path::PathBuf {
    // xtask runs from the workspace (cargo sets the manifest dir of xtask itself).
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent")
        .to_path_buf()
}

/// The image tag must match the installed `@playwright/test`: the browsers come from the image
/// while the test runner comes from the mounted `node_modules`, and Playwright refuses a mismatch.
const PLAYWRIGHT_IMAGE: &str = "mcr.microsoft.com/playwright:v1.61.0-noble";

/// `cargo xtask e2e [filter]` — run the browser e2e suite inside Microsoft's Playwright image.
///
/// The suite is already headless; what a build box lacks is the ~300 system libraries Chromium
/// needs. Rather than provision the host, borrow the image that has them.
///
/// **No custom image and no Rust in the container.** The host-built Director binary runs inside it
/// unmodified (verified: image glibc 2.39, host 2.43 — a Rust binary needs the symbols it uses to
/// exist, not an identical libc), so the container needs only node and browsers. That is what keeps
/// this a `docker run` instead of a Dockerfile somebody has to maintain.
///
/// The binary is built on the **host** first because the harness's build-on-demand path shells out
/// to `cargo`, which is not in the image — a missing binary would fail deep inside a spec instead
/// of here, where the message can say what to do.
fn e2e(args: &[String]) -> bool {
    if !run("cargo", &["build", "-p", "gridfpv-app"]) {
        eprintln!("e2e: the Director binary must build before the suite can boot it");
        return false;
    }
    let installed = playwright_version();
    if let Some(v) = &installed {
        if !PLAYWRIGHT_IMAGE.contains(v.as_str()) {
            eprintln!(
                "e2e: @playwright/test is {v} but the image is pinned to {PLAYWRIGHT_IMAGE}.\n\
                 Playwright refuses a runner/browser mismatch — update PLAYWRIGHT_IMAGE in \
                 xtask/src/main.rs to the matching tag."
            );
            return false;
        }
    }
    let root = workspace_root();
    let mount = format!("{}:{}", root.display(), root.display());
    let workdir = root.join("frontend");
    let mut docker: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--init".into(),
        // The specs boot a real Director per worker on an EPHEMERAL port, so nothing here can
        // collide with a Director already running on 8080 for bench testing.
        "-v".into(),
        mount,
        "-w".into(),
        workdir.display().to_string(),
        "-e".into(),
        "CI=1".into(),
        "-e".into(),
        "HOME=/tmp".into(),
        PLAYWRIGHT_IMAGE.into(),
        "npx".into(),
        "playwright".into(),
        "test".into(),
    ];
    // Write artefacts (traces, reports) as the invoking user, not root.
    if let Some(ids) = users_ids() {
        let at = docker.len() - 4; // before the image + npx playwright test
        docker.splice(at..at, ["-u".to_string(), ids]);
    }
    docker.extend(args.iter().cloned());
    let refs: Vec<&str> = docker.iter().map(String::as_str).collect();
    run("docker", &refs)
}

/// The installed `@playwright/test` version, or `None` if it cannot be read (then the tag check is
/// skipped rather than blocking the run on a missing file).
fn playwright_version() -> Option<String> {
    let pkg = workspace_root().join("frontend/node_modules/@playwright/test/package.json");
    let text = std::fs::read_to_string(pkg).ok()?;
    let key = "\"version\"";
    let at = text.find(key)?;
    let rest = &text[at + key.len()..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_string())
}

/// The invoking user's `uid:gid`, so artefacts the container writes (traces, reports) land owned
/// by the RD rather than by root.
///
/// Read from `id -u` / `id -g` rather than libc: this crate forbids `unsafe`, and shelling out once
/// per run is free next to a container start.
fn users_ids() -> Option<String> {
    let uid = Command::new("id").arg("-u").output().ok()?;
    let gid = Command::new("id").arg("-g").output().ok()?;
    let uid = String::from_utf8(uid.stdout).ok()?.trim().to_string();
    let gid = String::from_utf8(gid.stdout).ok()?.trim().to_string();
    if uid.is_empty() || gid.is_empty() {
        return None;
    }
    Some(format!("{uid}:{gid}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().cloned().unwrap_or_else(|| "ci".to_string());
    let ok = match task.as_str() {
        "fmt" => fmt(),
        "lint" | "clippy" => lint(),
        "test" => test(),
        "gen" => generate(),
        // Every `bindings/*.ts` is re-exported from the frontend's `@gridfpv/types` barrel
        // (#410) — the twin of `gen_check`, one layer out. See [`barrel_check`].
        "barrel" => barrel_check(),
        // Type-check feature-gated live targets without running them (see `live_check`).
        "live-check" => live_check(),
        // The src-tauri lock drift twin (#513): a stale lock re-resolves mid-CI-build and
        // stamps every portable -dirty. See [`tauri_lock_check`].
        "tauri-lock" => tauri_lock_check(),
        "ci" => {
            fmt()
                && lint()
                && test()
                && live_check()
                && gen_check()
                && barrel_check()
                && tauri_lock_check()
        }
        // The browser e2e suite, in Microsoft's Playwright image (#426). See `docker/playwright/`.
        "e2e" => e2e(&args[1..]),
        // Bump the ONE product version everywhere it lives (workspace Cargo.toml + the
        // standalone src-tauri crate + tauri.conf.json + the console package.json).
        "version" => version(&args[1..]),
        other => {
            eprintln!("unknown task: {other}");
            eprintln!(
                "usage: cargo xtask [ci|fmt|lint|test|live-check|gen|barrel|tauri-lock|e2e|version]"
            );
            false
        }
    };
    if !ok {
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// The barrel as it really looks: a doc comment that mentions `@bindings/*`, then one
    /// re-export line per generated type.
    const BARREL_SRC: &str = "/**\n * The alias is `@bindings/* → ../../../bindings/*`.\n */\n\
         export type * from '@bindings/LapList';\n\
         export type * from '@bindings/VoidReason';\n\
         export type * from '@bindings/TimerSignal';\n";

    #[test]
    fn barrel_gaps_is_clean_when_every_binding_is_re_exported() {
        let bindings = names(&["LapList", "TimerSignal", "VoidReason"]);
        let (missing, stale) = barrel_gaps(&bindings, BARREL_SRC);
        assert!(missing.is_empty(), "unexpected missing: {missing:?}");
        assert!(stale.is_empty(), "unexpected stale: {stale:?}");
    }

    /// The #410 case itself: a binding was generated and nobody added its export line.
    #[test]
    fn barrel_gaps_reports_a_generated_binding_that_is_never_re_exported() {
        let bindings = names(&["LapList", "NodeSignal", "TimerSignal", "VoidReason"]);
        let (missing, stale) = barrel_gaps(&bindings, BARREL_SRC);
        assert_eq!(missing, names(&["NodeSignal"]));
        assert!(stale.is_empty(), "unexpected stale: {stale:?}");
    }

    /// The other direction: a type was deleted from Rust and its export line lingered.
    #[test]
    fn barrel_gaps_reports_a_re_export_with_no_binding_behind_it() {
        let bindings = names(&["LapList", "VoidReason"]);
        let (missing, stale) = barrel_gaps(&bindings, BARREL_SRC);
        assert!(missing.is_empty(), "unexpected missing: {missing:?}");
        assert_eq!(stale, names(&["TimerSignal"]));
    }

    /// Presence, not order — `VoidReason` sits out of alphabetical order on purpose, and the
    /// `@bindings/*` mention in the doc comment is not a re-export.
    #[test]
    fn barrel_export_reads_lines_not_ordering() {
        assert_eq!(
            barrel_export("export type * from '@bindings/Cursor';"),
            Some("Cursor")
        );
        assert_eq!(
            barrel_export("  export type * from '@bindings/Cursor';  "),
            Some("Cursor")
        );
        assert_eq!(
            barrel_export(" * The alias is `@bindings/* → ../../../bindings/*`."),
            None
        );
        assert_eq!(
            barrel_export("export type { Cursor } from '@bindings/Cursor';"),
            None
        );
        assert_eq!(barrel_export(""), None);
    }

    /// The real repo, checked from the test suite as well as from `ci`: every checked-in
    /// binding is re-exported, and the barrel names nothing that does not exist.
    #[test]
    fn the_checked_in_barrel_covers_every_checked_in_binding() {
        let root = workspace_root();
        let bindings = binding_names(&root).expect("bindings/ is readable");
        assert!(!bindings.is_empty(), "bindings/ should not be empty");
        let src = std::fs::read_to_string(root.join(BARREL)).expect("the barrel is readable");
        let (missing, stale) = barrel_gaps(&bindings, &src);
        assert!(
            missing.is_empty() && stale.is_empty(),
            "barrel gap — missing: {missing:?}, stale: {stale:?}"
        );
    }
}
