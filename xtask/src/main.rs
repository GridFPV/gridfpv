//! GridFPV task runner — the single definition of our checks.
//!
//! Local dev runs `cargo xtask ci`; GitLab CI runs the identical `cargo xtask ci`
//! (see `.gitlab-ci.yml`). Keeping the logic here means local and remote can
//! never drift. Pure std + cargo, so it works the same on Windows/Linux/macOS.
#![forbid(unsafe_code)]

mod race_day;
mod rh_mock;

use std::path::{Path, PathBuf};
use std::process::{Command, exit};

/// Run a command, echoing it first; returns whether it succeeded.
fn run(program: &str, args: &[&str]) -> bool {
    run_env(program, args, &[])
}

/// Like [`run`], but with extra environment variables set for the child process.
fn run_env(program: &str, args: &[&str], env: &[(&str, &Path)]) -> bool {
    let vars: Vec<(&str, Option<String>)> = env
        .iter()
        .map(|(k, v)| (*k, Some(v.display().to_string())))
        .collect();
    run_vars(program, args, &vars)
}

/// Like [`run_env`], but each variable may be `None` to **unset** it for the child.
/// Unsetting matters for the live matrix: "no plugin" means the child `cargo test` must
/// not see `GRIDFPV_RH_PLUGIN` at all, even if the invoking shell exported it.
fn run_vars(program: &str, args: &[&str], env: &[(&str, Option<String>)]) -> bool {
    println!("\n\x1b[1m$ {program} {}\x1b[0m", args.join(" "));
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (key, value) in env {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
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

/// A live target: `(package, test binary, is `#[ignore]`d)`.
type LiveTarget = (&'static str, &'static str, bool);

/// The **full** live suite — every live target we have. Run against the primary
/// configuration (current-stable RH *with* the plugin).
const FULL_TARGETS: &[LiveTarget] = &[
    // No container needed (in-process mock WS server).
    ("gridfpv-adapters", "velocidrone_ws", false),
    // Each spins up + tears down its own disposable RotorHazard.
    ("gridfpv-adapters", "rh_live", true),
    ("gridfpv-adapters", "rh_signal", true),
    // The engine's mock-RH e2e tests: each drives a full heat through the shared
    // harness on its own port (#29 heat loop, #30 scoring, #31 marshaling).
    ("gridfpv-engine", "heat_live", true),
    ("gridfpv-engine", "scoring_live", true),
    ("gridfpv-engine", "marshaling_live", true),
    // #388 — a seated node the timer never detected must still be marshalable.
    ("gridfpv-engine", "zero_lap_marshaling_live", true),
    ("gridfpv-engine", "format_live", true),
    ("gridfpv-engine", "timed_qual_live", true),
    ("gridfpv-engine", "zippyq_live", true),
    ("gridfpv-engine", "multiclass_live", true),
    // The protocol server's mock-RH e2e: full event → server log → protocol client (#47).
    ("gridfpv-server", "full_event_live", true),
    // The Director's RH-connect e2e (#65, #73): the per-event bridge connects dockerized RH,
    // drives status to Connected, and feeds real passes into the event log.
    ("gridfpv-app", "rh_connect_live", true),
];

/// The **targeted** subset the secondary matrix legs run: the lap-ingestion-critical
/// targets. Each drives a real dockerized RotorHazard through a real race and asserts on
/// the passes that came back out — so "RH recorded laps but Grid ingested none" (#389)
/// fails them, in whichever version × plugin combination it happens in:
///
/// - `heat_live` — adapter → engine: a heat is only `Final` with crossings collected
///   while it was live; the harness asserts at least one `Pass` came through.
/// - `scoring_live` — the ingested passes must fold into laps and a ranked result, so a
///   *partial* ingest (some laps dropped) fails too, not just a total blackout.
/// - `gridfpv-server`'s `full_event_live` — the whole spine: RH → adapter → engine →
///   event log → protocol client, over a full multi-heat event.
///
/// Running the full suite four times would cost ~4× the wall clock for near-duplicate
/// coverage; these three are what actually discriminate between the plugin's pass path
/// and stock RH's `current_laps` snapshot path.
const INGEST_TARGETS: &[LiveTarget] = &[
    ("gridfpv-engine", "heat_live", true),
    ("gridfpv-engine", "scoring_live", true),
    ("gridfpv-server", "full_event_live", true),
];

/// How much of the live suite a matrix leg runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Coverage {
    /// Every target in [`FULL_TARGETS`].
    Full,
    /// The lap-ingestion-critical [`INGEST_TARGETS`].
    Targeted,
}

impl Coverage {
    fn targets(self) -> &'static [LiveTarget] {
        match self {
            Coverage::Full => FULL_TARGETS,
            Coverage::Targeted => INGEST_TARGETS,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Coverage::Full => "full",
            Coverage::Targeted => "targeted",
        }
    }
}

/// One leg of the RotorHazard version × plugin matrix.
struct LiveConfig {
    /// RotorHazard version the containers run (image `gridfpv-rotorhazard:<rh>`).
    rh: String,
    /// Whether the GridFPV plugin is mounted into the container.
    plugin: bool,
    coverage: Coverage,
}

impl LiveConfig {
    fn label(&self) -> String {
        format!(
            "RH {} {} plugin ({})",
            self.rh,
            if self.plugin { "+" } else { "—" },
            self.coverage.label()
        )
    }

    /// The command that reproduces exactly this leg on its own.
    fn command(&self) -> String {
        format!(
            "cargo xtask live --rh {}{} --{}",
            self.rh,
            if self.plugin { "" } else { " --no-plugin" },
            self.coverage.label()
        )
    }
}

/// The default matrix `cargo xtask live` runs with no arguments: **targeted × 4, full on
/// one**. The current stable with the plugin keeps the full suite (today's behaviour,
/// unchanged); the other three legs run the ingestion-critical subset.
///
/// Why these four: before #389 the suite only ever ran one configuration — current-stable
/// RH *with* the plugin — so the stock `current_laps` path had **no** live coverage at all
/// and the two ingest paths were never contrasted. A plugin-only lap-ingestion regression
/// therefore reached the field green. The floor (RHAPI 1.3 / RH v4.3.0+, D16) is what the
/// field timer runs, so it is covered on both paths too.
fn default_matrix() -> Vec<LiveConfig> {
    let stable = gridfpv_testkit::DEFAULT_RH_VERSION.to_string();
    let floor = gridfpv_testkit::FLOOR_RH_VERSION.to_string();
    vec![
        LiveConfig {
            rh: stable.clone(),
            plugin: true,
            coverage: Coverage::Full,
        },
        LiveConfig {
            rh: stable,
            plugin: false,
            coverage: Coverage::Targeted,
        },
        LiveConfig {
            rh: floor.clone(),
            plugin: true,
            coverage: Coverage::Targeted,
        },
        LiveConfig {
            rh: floor,
            plugin: false,
            coverage: Coverage::Targeted,
        },
    ]
}

/// `cargo xtask live` — the local-only integration test class, run as a **RotorHazard
/// version × plugin matrix**.
///
/// Container-dependent tests don't belong in the shared CI pipeline (flaky,
/// resource-limited). These run the `live` feature: the WebSocket mock-server test
/// (no container) and the dockerized-RotorHazard tests (`rh_live` via `simulate_lap`
/// and `rh_signal` via emulated `mock_data` RSSI streams). Each container test spins
/// up and tears down its own disposable RotorHazard, so no external state is needed —
/// just Docker. `--ignored` runs the `#[ignore]`d container tests too.
///
/// Bare `cargo xtask live` runs the whole [`default_matrix`] — still one command. Flags
/// pin a single configuration instead (e.g. `cargo xtask live --rh 4.3.0 --no-plugin`),
/// which is how you reproduce one leg while debugging:
///
/// - `--rh <version>` — RotorHazard version (repeatable); default the current stable.
/// - `--plugin` / `--no-plugin` — mount the GridFPV plugin, or run stock RH; default on.
/// - `--full` / `--targeted` — override how much of the suite that leg runs; the default
///   matches what the matrix gives that configuration.
fn live(args: &[String]) -> bool {
    let mut versions: Vec<String> = Vec::new();
    let mut plugin: Option<bool> = None;
    let mut coverage: Option<Coverage> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--rh" => match iter.next() {
                Some(v) => versions.push(v.trim_start_matches('v').to_string()),
                None => {
                    eprintln!("--rh needs a version, e.g. --rh 4.3.0");
                    return false;
                }
            },
            "--plugin" => plugin = Some(true),
            "--no-plugin" => plugin = Some(false),
            "--full" => coverage = Some(Coverage::Full),
            "--targeted" | "--subset" => coverage = Some(Coverage::Targeted),
            other => {
                eprintln!("unknown `live` flag: {other}");
                eprintln!(
                    "usage: cargo xtask live [--rh <version>]… [--plugin|--no-plugin] [--full|--targeted]"
                );
                return false;
            }
        }
    }

    // No selector at all = the full matrix (the one-command default).
    let configs: Vec<LiveConfig> = if versions.is_empty() && plugin.is_none() && coverage.is_none()
    {
        default_matrix()
    } else {
        if versions.is_empty() {
            versions.push(gridfpv_testkit::DEFAULT_RH_VERSION.to_string());
        }
        let plugin = plugin.unwrap_or(true);
        versions
            .into_iter()
            .map(|rh| {
                // Default coverage mirrors the matrix: the primary config gets the full
                // suite, every other configuration the ingestion-critical subset.
                let default = if plugin && rh == gridfpv_testkit::DEFAULT_RH_VERSION {
                    Coverage::Full
                } else {
                    Coverage::Targeted
                };
                LiveConfig {
                    rh,
                    plugin,
                    coverage: coverage.unwrap_or(default),
                }
            })
            .collect()
    };

    // Build every image the run needs up front, so a first-run image build (a few minutes:
    // it clones RH and pip-installs) fails fast and doesn't land in the middle of a leg.
    let mut built: Vec<&str> = Vec::new();
    for config in &configs {
        if !built.contains(&config.rh.as_str()) {
            gridfpv_testkit::ensure_rh_image(&config.rh);
            built.push(&config.rh);
        }
    }

    let started = std::time::Instant::now();
    let mut results: Vec<(String, bool, std::time::Duration)> = Vec::new();
    for config in &configs {
        println!(
            "\n\x1b[1m═══ live matrix: {} ═══\x1b[0m\n    {}",
            config.label(),
            config.command()
        );
        let leg = std::time::Instant::now();
        results.push((config.label(), run_config(config), leg.elapsed()));
    }

    println!("\n\x1b[1m═══ live matrix summary ═══\x1b[0m");
    for (label, ok, elapsed) in &results {
        println!(
            "  {} {label} ({:.0}s)",
            if *ok {
                "\x1b[32mPASS\x1b[0m"
            } else {
                "\x1b[31mFAIL\x1b[0m"
            },
            elapsed.as_secs_f64()
        );
    }
    println!("  total {:.0}s", started.elapsed().as_secs_f64());
    results.iter().all(|(_, ok, _)| *ok)
}

/// Run one matrix leg: every target of its coverage, sequentially, against the leg's
/// RotorHazard version and plugin setting.
///
/// The whole configuration reaches the containers through the child `cargo test`'s
/// environment (env on the child, not a process-global `set_var`, which is unsafe under
/// this crate's `#![forbid(unsafe_code)]`):
///
/// - `GRIDFPV_RH_VERSION` picks the harness image (`gridfpv-rotorhazard:<version>`).
/// - `GRIDFPV_RH_PLUGIN` names the host plugin dir the testkit's `RhContainer` mounts into
///   the container's user `plugins/gridfpv`. **Unset** on the no-plugin legs — that is
///   exactly "stock RH", the `current_laps` snapshot path.
///
/// Every target runs even after one fails, so a leg reports its whole picture rather than
/// stopping at the first red. Targets run one at a time so at most one RotorHazard
/// container exists at any moment (cargo would otherwise run test binaries in parallel).
fn run_config(config: &LiveConfig) -> bool {
    let plugin_dir = config.plugin.then(|| {
        workspace_root()
            .join("plugins/gridfpv")
            .display()
            .to_string()
    });
    let env: Vec<(&str, Option<String>)> = vec![
        (gridfpv_testkit::RH_VERSION_ENV, Some(config.rh.clone())),
        (gridfpv_testkit::PLUGIN_ENV, plugin_dir),
    ];

    let mut ok = true;
    for (package, name, ignored) in config.coverage.targets() {
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
        if *ignored {
            args.push("--ignored");
        }
        ok &= run_vars("cargo", &args, &env);
    }
    ok
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().cloned().unwrap_or_else(|| "ci".to_string());
    let ok = match task.as_str() {
        "fmt" => fmt(),
        "lint" | "clippy" => lint(),
        "test" => test(),
        "gen" => generate(),
        "ci" => fmt() && lint() && test() && gen_check(),
        // The RotorHazard version × plugin live matrix; bare `live` runs all four legs.
        "live" => live(&args[1..]),
        // The interactive RotorHazard mock-signal harness (marshaling testing). Needs Docker to
        // `feed`; `dump`/`list` are plain HTTP/std. See `rh_mock.rs`.
        "rh-mock" => rh_mock::run(&args[1..]),
        // The mock race-day autopilot: emulate races via the gridfpv_mock plugin while you drive
        // the Director. See `race_day.rs`.
        "race-day" => race_day::run(&args[1..]),
        // Bump the ONE product version everywhere it lives (workspace Cargo.toml + the
        // standalone src-tauri crate + tauri.conf.json + the console package.json).
        "version" => version(&args[1..]),
        other => {
            eprintln!("unknown task: {other}");
            eprintln!("usage: cargo xtask [ci|fmt|lint|test|gen|live|rh-mock|race-day|version]");
            false
        }
    };
    if !ok {
        exit(1);
    }
}
