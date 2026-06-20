//! Shared test support (#38): the dockerized-RotorHazard harness.
//!
//! This is the §5.1 ("mock-RH e2e") harness from `docs/testing-strategy.html`,
//! promoted out of `gridfpv-adapters`' internal `tests/common/` so that any crate's
//! `live` end-to-end tests can reuse it (the race engine's e2e tests, #29+, in
//! particular). It has no third-party dependencies — `node_csv` is pure string
//! generation and [`RhContainer`] drives Docker through `std::process` / `std::net`.
//!
//! Two pieces:
//! - [`node_csv`] generates a RotorHazard `mock_data_{N}.csv` — an **emulated
//!   node-output stream** (the post-detection signal a real timer node sends the
//!   server: per-tick RSSI, peak, crossing flag, and an incrementing `lap_id` that
//!   makes the server record a lap). This drives RH's *real* lap recording, unlike
//!   the `simulate_lap` injector.
//! - [`RhContainer`] runs a disposable dockerized RotorHazard with those CSVs
//!   mounted, on its own port, and removes it on drop (RAII).
//!
//! Timing note: the mock interface reads its CSV continuously (decoupled from race
//! start), so exact lap *timing* is not controllable — tests assert structure
//! (lap counts, multi-node independence, signal context, dedup), not exact µs.
//!
//! # Usage
//!
//! Consumers gate their own tests behind whatever feature they use for the live
//! class (e.g. `#![cfg(feature = "live")]` + `#[ignore]`); this crate itself is
//! plain helper code that always compiles. The container code compiles without
//! Docker present (it only *runs* Docker), so the crate builds in normal CI.
//!
//! ```no_run
//! use gridfpv_testkit::{NodeCsv, RhContainer, node_csv};
//!
//! // Emulated node 0: fast laps, strong signal.
//! let csvs = vec![(0usize, node_csv(&NodeCsv::default()))];
//! let rh = RhContainer::start(5031, "0.1", &csvs);
//! // ... connect a transport to `rh.url()`, drive a race, assert on the events ...
//! // The container is torn down when `rh` is dropped.
//! ```

use std::fs;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// How to synthesise one node's emulated output stream.
pub struct NodeCsv {
    /// Ticks (CSV lines) between lap increments — roughly `lap_duration /
    /// RH_UPDATE_INTERVAL`. Smaller = faster laps.
    pub ticks_per_lap: usize,
    /// The node's peak RSSI, reported every tick so the node's `pass_peak_rssi`
    /// (and thus the adapter's `SignalContext`) is a stable, assertable value.
    pub peak_rssi: i32,
    /// Baseline RSSI between crossings (kept valid: 1..=999).
    pub baseline_rssi: i32,
}

impl Default for NodeCsv {
    fn default() -> Self {
        Self {
            ticks_per_lap: 6,
            peak_rssi: 150,
            baseline_rssi: 70,
        }
    }
}

/// Render one node's `mock_data_{N}.csv` content.
///
/// Columns (RotorHazard `MockInterface`): `idx, lap_id, ms, rssi, node_peak,
/// pass_peak, loop_time, cross(T/F), pass_nadir, node_nadir, peakRssi, pkFirst,
/// pkLast, nadirRssi, ndFirst, ndLast`. A new lap is recorded each time `lap_id`
/// increments.
///
/// `lap_id` increments **continuously** (every `ticks_per_lap` lines) for the whole
/// file. The mock interface reads the file continuously from container start
/// (decoupled from race start), so laps must keep coming *throughout* the race
/// rather than being baked in up front — capping `lap_id` would stop producing laps
/// before the race even begins. At EOF the file loops, which simply keeps laps
/// coming (each increment still differs from the last seen id).
pub fn node_csv(opts: &NodeCsv) -> String {
    const TOTAL_TICKS: usize = 600;
    let node_peak = opts.peak_rssi + 30;
    let mut lines = Vec::with_capacity(TOTAL_TICKS);
    for i in 0..TOTAL_TICKS {
        let lap_id = i / opts.ticks_per_lap;
        let on_lap = i > 0 && i % opts.ticks_per_lap == 0;
        let rssi = if on_lap {
            opts.peak_rssi
        } else {
            opts.baseline_rssi
        };
        let cross = if on_lap { 'T' } else { 'F' };
        // pass_peak is reported on EVERY tick so the node's signal level is a stable
        // value the adapter can cache and assert (not just on lap ticks).
        lines.push(format!(
            "0,{lap_id},0,{rssi},{node_peak},{peak},10,{cross},20,30,{peak},0,0,20,0,0",
            peak = opts.peak_rssi,
        ));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A disposable dockerized RotorHazard for a single test, with emulated node CSVs
/// mounted. Removed on drop.
pub struct RhContainer {
    name: String,
    url: String,
    _tmp: PathBuf,
}

impl RhContainer {
    /// Start RotorHazard on `port` with `csvs` (one per node, 0-based node index)
    /// mounted as `mock_data_{index+1}.csv`, ticking every `update_interval`
    /// seconds. Blocks until the HTTP port accepts connections.
    pub fn start(port: u16, update_interval: &str, csvs: &[(usize, String)]) -> Self {
        let name = format!("gridfpv-rh-sig-{port}");
        // Clean any leftover from a previous aborted run.
        let _ = Command::new("docker").args(["rm", "-f", &name]).output();

        let tmp = std::env::temp_dir().join(&name);
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).expect("create temp mount dir");

        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            name.clone(),
            "-p".into(),
            format!("{port}:5000"),
            "-e".into(),
            format!("RH_UPDATE_INTERVAL={update_interval}"),
        ];
        for (node_index, content) in csvs {
            let file = tmp.join(format!("mock_data_{}.csv", node_index + 1));
            fs::write(&file, content).expect("write mock_data csv");
            args.push("-v".into());
            args.push(format!(
                "{}:/root/RotorHazard/src/server/mock_data_{}.csv",
                file.display(),
                node_index + 1
            ));
        }
        args.push("cruwaller/rotorhazard:latest".into());

        let out = Command::new("docker")
            .args(&args)
            .output()
            .expect("docker run RotorHazard");
        assert!(
            out.status.success(),
            "docker run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let url = format!("http://localhost:{port}");
        wait_for_port(port, Duration::from_secs(60));
        // Give the server a moment past TCP-accept to finish booting its socket API.
        std::thread::sleep(Duration::from_secs(3));
        Self {
            name,
            url,
            _tmp: tmp,
        }
    }

    /// Base URL (`http://localhost:{port}`) a transport connects to.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Drop for RhContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
        let _ = fs::remove_dir_all(&self._tmp);
    }
}

/// Block until `port` accepts a TCP connection, or panic after `timeout`.
fn wait_for_port(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("RotorHazard container did not open port {port} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
