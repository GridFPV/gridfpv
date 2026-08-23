//! `cargo xtask race-day` — the **mock race-day autopilot harness**.
//!
//! Stands up a RotorHazard with the GridFPV plugin **and** the test-only `gridfpv_mock` plugin, then
//! runs the [`race_day.py`](../../docker/rotorhazard/race_day.py) autopilot inside it. The autopilot
//! watches the race state and, whenever you stage + start a heat in the GridFPV Director, emulates
//! that heat's race over the wire (realistic per-pilot laps + RSSI bells via `gridfpv_mock_pass`).
//! You drive the application; the test plugin drives the races.
//!
//! - **`cargo xtask race-day [scenario] [--port P] [--container NAME]`** — ensure the race RH is up
//!   (build the image + `docker run` it if needed; no `mock_data` CSV — the autopilot is the only
//!   signal source), then exec the autopilot. Point the Director's active-event timer at the printed
//!   URL. Ctrl-C stops the autopilot (the RH container is left running for the next scenario).
//! - **`cargo xtask race-day list`** — the scenario menu.
//!
//! Scenarios (race-day "personalities"): `clean`, `varied`, `messy` (marshaling cases), `pack`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The persistent race-day RH container (distinct from the test harness's `gridfpv-rh-sig-*` and the
/// maintainer's `gridfpv-demo-rh`). Reused across runs; not auto-removed.
const RACE_CONTAINER: &str = "gridfpv-race-rh";
/// Default host port — matches the timer URL the deploy guide configures in the Director.
const DEFAULT_PORT: u16 = 5055;
/// The RH harness image (built from `docker/rotorhazard/`) — resolved through the testkit so
/// this can't drift from the tag the live harness builds. Race-day drives the Director by hand,
/// so it always uses the default RotorHazard version; the version × plugin sweep is
/// `cargo xtask live`'s job.
fn rh_image() -> String {
    gridfpv_testkit::rh_image_for(gridfpv_testkit::DEFAULT_RH_VERSION)
}

const SCENARIOS: &[(&str, &str)] = &[
    (
        "clean",
        "smooth, steady-pace laps, strong signal (the happy path)",
    ),
    (
        "varied",
        "per-pilot pace spread + lap-to-lap jitter (a realistic leaderboard)",
    ),
    (
        "messy",
        "marshaling practice: a missed lap, a false/extra pass, and a DNF",
    ),
    (
        "pack",
        "a tight field crossing close together (close finishes)",
    ),
];

pub fn run(args: &[String]) -> bool {
    let mut scenario = "clean".to_string();
    let mut port = DEFAULT_PORT;
    let mut container = RACE_CONTAINER.to_string();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "list" => {
                print_menu();
                return true;
            }
            "--port" => match it.next().and_then(|v| v.parse::<u16>().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("--port needs a number");
                    return false;
                }
            },
            "--container" => match it.next() {
                Some(c) => container = c.clone(),
                None => {
                    eprintln!("--container needs a name");
                    return false;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("unknown flag: {other}");
                return false;
            }
            name => scenario = name.to_string(),
        }
    }

    if !SCENARIOS.iter().any(|(n, _)| *n == scenario) {
        eprintln!("unknown scenario: {scenario}");
        print_menu();
        return false;
    }
    if !docker_present() {
        eprintln!("\x1b[31mdocker not found on PATH.\x1b[0m race-day drives a real container.");
        return false;
    }

    let root = workspace_root();
    if !ensure_image(&root) {
        return false;
    }
    if !ensure_container(&root, &container, port) {
        return false;
    }

    // Copy the autopilot in fresh (so edits land without rebuilding the container).
    let script = root.join("docker/rotorhazard/race_day.py");
    if !run_ok(
        "docker",
        &[
            "cp",
            script.to_str().unwrap(),
            &format!("{container}:/tmp/race_day.py"),
        ],
    ) {
        eprintln!("failed to copy race_day.py into {container}");
        return false;
    }

    println!(
        "\n\x1b[1mMock race day\x1b[0m — scenario \x1b[1m{scenario}\x1b[0m on RotorHazard \
         \x1b[1mhttp://localhost:{port}\x1b[0m (container `{container}`).\n"
    );
    println!("\x1b[1mNext steps:\x1b[0m");
    println!(
        "  1. In the GridFPV Director, point your active event's RotorHazard timer at \
         \x1b[1mhttp://localhost:{port}\x1b[0m and make the event active (timer reads Connected)."
    );
    println!(
        "  2. Build an event with pilots + heats, seat pilots on nodes, then \x1b[1mStage → Start\x1b[0m a heat."
    );
    println!(
        "  3. Watch it race — the autopilot below emulates the seated nodes per the scenario."
    );
    println!(
        "  4. Marshal / advance as you like; run the next heat. Ctrl-C here ends the autopilot.\n"
    );

    // Exec the autopilot in the foreground (streams its log; runs until Ctrl-C).
    run_ok(
        "docker",
        &["exec", &container, "python3", "/tmp/race_day.py", &scenario],
    )
}

fn print_menu() {
    println!("\n\x1b[1mMock race-day scenarios\x1b[0m (cargo xtask race-day <name>):\n");
    for (name, blurb) in SCENARIOS {
        println!("  \x1b[1m{name:<8}\x1b[0m {blurb}");
    }
    println!("\nEach watches the race state and emulates every heat you Start in the Director.");
}

/// Build the RH harness image if it isn't present locally — the testkit's own auto-build, so
/// there is exactly one place that knows how to build it (tag, build-arg, and context alike).
fn ensure_image(_root: &Path) -> bool {
    gridfpv_testkit::ensure_rh_image(gridfpv_testkit::DEFAULT_RH_VERSION);
    true
}

/// Ensure the race RH container is running with BOTH plugins mounted (and no `mock_data` CSV, so the
/// autopilot is the only signal source). Reuses an already-running container; starts one otherwise.
fn ensure_container(root: &Path, container: &str, port: u16) -> bool {
    let running = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", container])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);
    if running {
        println!("Reusing running RotorHazard container `{container}`.");
        return true;
    }
    // Remove any stopped leftover of the same name, then start fresh.
    let _ = Command::new("docker")
        .args(["rm", "-f", container])
        .output();

    let gridfpv = root.join("plugins/gridfpv");
    let mock = root.join("plugins/gridfpv_mock");
    let mount = |dir: &Path, name: &str| {
        format!(
            "{}:/opt/RotorHazard/src/server/plugins/{name}:ro",
            dir.display()
        )
    };
    println!("Starting RotorHazard `{container}` on :{port} (gridfpv + gridfpv_mock plugins)…");
    let ok = run_ok(
        "docker",
        &[
            "run",
            "-d",
            "--name",
            container,
            "-p",
            &format!("{port}:5000"),
            "-v",
            &mount(&gridfpv, "gridfpv"),
            "-v",
            &mount(&mock, "gridfpv_mock"),
            &rh_image(),
        ],
    );
    if !ok {
        eprintln!("failed to start {container}");
        return false;
    }
    // Wait for the HTTP port to accept connections.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            std::thread::sleep(Duration::from_secs(3)); // settle the socket API
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!("RotorHazard did not open port {port} in time");
    false
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest dir has a parent (workspace root)")
        .to_path_buf()
}

fn docker_present() -> bool {
    Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_ok(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
