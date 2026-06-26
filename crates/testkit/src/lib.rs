//! Shared test support (#38): the dockerized-RotorHazard harness.
//!
//! This is the §5.1 ("mock-RH e2e") harness from `docs/testing-strategy.html`,
//! promoted out of `gridfpv-adapters`' internal `tests/common/` so that any crate's
//! `live` end-to-end tests can reuse it (the race engine's e2e tests, #29+, in
//! particular). It has no third-party dependencies — `node_csv` is pure string
//! generation and [`RhContainer`] drives Docker through `std::process` / `std::net`.
//!
//! Three pieces:
//! - [`node_csv`] generates a RotorHazard `mock_data_{N}.csv` — an **emulated
//!   node-output stream** (the post-detection signal a real timer node sends the
//!   server: per-tick RSSI, peak, crossing flag, and an incrementing `lap_id` that
//!   makes the server record a lap). This drives RH's *real* lap recording, unlike
//!   the `simulate_lap` injector. This is the *simple* uniform case.
//! - [`plan_csv`] + [`NodePlan`]/[`LapSpec`] are the **scenario library**: a
//!   composable, per-node lap schedule where every lap carries its own gap-in-ticks
//!   (pace), so pace can vary, laps can be skipped (missed crossings), a node can
//!   drop out (DNF), and signal magnitude is per-node. [`race`] / [`simultaneous`]
//!   compose nodes into the multi-node `(node_index, csv)` list [`RhContainer::start`]
//!   takes. See the [`scenarios`] module for the ready-made menu.
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

/// Render one CSV row (one mock tick) in RotorHazard `MockInterface` column order.
///
/// Columns: `idx, lap_id, ms, rssi, node_peak, pass_peak, loop_time, cross(T/F),
/// pass_nadir, node_nadir, peakRssi, pkFirst, pkLast, nadirRssi, ndFirst, ndLast`.
///
/// Two distinct signal fidelities ride in one row, and tests assert on both:
/// - **Coarse** — `node_peak` (col 4) is what `node_data` streams and the adapter's coarse
///   [`SignalChunk`] trace samples once per heartbeat. It is held **stable** at `node_peak` so the
///   coarse-fidelity test can assert an exact value.
/// - **Dense** — the `peakRssi`/`nadirRssi` history columns (10/13) with their first/last times
///   (11/12, 14/15) feed RotorHazard's per-tick `history_values`/`history_times` accumulator
///   (`BaseHardwareInterface.PeakNadirHistory.addTo`), the dense trace its marshal page reviews and
///   our path-2 (`current_marshal_data`/`get_pilotrace`) pulls at heat end. Passing `pk_hi/pk_lo`
///   distinct **first/last** times (`pkFirst > pkLast`) makes RH log *two* history entries per tick,
///   and varying `peak_rssi_hist`/`nadir_rssi_hist` per tick (rather than a flat square wave) gives
///   the captured trace real texture at a much higher sample density than the coarse stream — the
///   resolution the marshaling graph is judged on.
#[allow(clippy::too_many_arguments)]
fn csv_row(
    lap_id: usize,
    rssi: i32,
    node_peak: i32,
    pass_peak: i32,
    cross: char,
    peak_rssi_hist: i32,
    nadir_rssi_hist: i32,
) -> String {
    // Distinct first/last times so RH's `addTo` records two entries per peak and per nadir each
    // tick (`pkFirst > pkLast` / `ndFirst > ndLast`), raising the dense-history density. Small,
    // fixed, positive values keep them well clear of the "corrupted history times" guard.
    format!(
        "0,{lap_id},0,{rssi},{node_peak},{pass_peak},10,{cross},20,30,{peak},2,1,{nadir},2,1",
        peak = peak_rssi_hist,
        nadir = nadir_rssi_hist,
    )
}

/// A per-tick dense **peak/nadir** RSSI envelope for the history columns, given the lap's peak and
/// the node's baseline and where this tick sits within the lap (`0.0` just after the previous
/// crossing → `1.0` at the next). The envelope ramps the peak up toward the crossing and the nadir
/// down between crossings, so the captured dense trace has a realistic, textured shape (not a flat
/// square wave) at full per-tick density — what the marshaling graph is judged on. Returns
/// `(peak_rssi_hist, nadir_rssi_hist)`, both kept valid (`1..=999`).
fn dense_envelope(peak: i32, baseline: i32, phase: f64) -> (i32, i32) {
    // Peak rises from baseline toward `peak` as the craft approaches the gate (phase -> 1.0).
    let span = (peak - baseline).max(1) as f64;
    let rise = baseline as f64 + span * phase;
    // A small per-tick wobble so consecutive ticks differ (defeats RH's run-length dedup, keeping
    // the trace dense) without crossing the enter threshold off-gate.
    let wobble = if phase < 0.95 {
        ((phase * 12.0).sin() * 4.0).round()
    } else {
        0.0
    };
    let peak_rssi = (rise + wobble).round().clamp(1.0, 999.0) as i32;
    // Nadir sits a little below the running baseline, dipping deepest mid-lap (phase ~0.5).
    let dip = (baseline as f64 * 0.4) * (1.0 - (phase - 0.5).abs() * 2.0).max(0.0);
    let nadir_rssi = ((baseline as f64) - dip).round().clamp(1.0, 999.0) as i32;
    (peak_rssi, nadir_rssi)
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
///
/// The coarse `node_peak` (col 4) stays stable at `peak + 30` (the value the coarse [`SignalChunk`]
/// trace samples and the fidelity test asserts), while the dense history columns carry a textured
/// per-tick peak/nadir envelope (see [`csv_row`]/[`dense_envelope`]) so the path-2 trace pulled at
/// heat end is high-resolution.
pub fn node_csv(opts: &NodeCsv) -> String {
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
        // Phase within the current lap window (0 just after the last crossing -> ~1 at the next).
        let phase = (i % opts.ticks_per_lap) as f64 / opts.ticks_per_lap.max(1) as f64;
        let (peak_hist, nadir_hist) = dense_envelope(opts.peak_rssi, opts.baseline_rssi, phase);
        // pass_peak is reported on EVERY tick so the node's signal level is a stable
        // value the adapter can cache and assert (not just on lap ticks).
        lines.push(csv_row(
            lap_id,
            rssi,
            node_peak,
            opts.peak_rssi,
            cross,
            peak_hist,
            nadir_hist,
        ));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Scenario library: per-node lap schedules (`NodePlan` / `LapSpec` / `plan_csv`)
// ---------------------------------------------------------------------------

/// Total CSV lines (ticks) every generated `mock_data` file contains. A node's
/// schedule is rendered into this fixed-length window; ticks past the last
/// scheduled event are flat baseline (no further laps for that node).
pub const TOTAL_TICKS: usize = 600;

/// RotorHazard's default *enter* threshold (`pass_peak`): a crossing is only
/// detected when the peak rises above roughly this RSSI. The scenario helpers use
/// it to distinguish a *marginal* signal (just above) from a comfortably-detected
/// one. This mirrors RH's `DEFAULT_ENTER_AT_LEVEL`; treat it as approximate.
pub const ENTER_THRESHOLD: i32 = 90;

/// One scheduled lap (crossing) in a [`NodePlan`].
///
/// `gap` is the number of ticks since the previous lap's crossing — i.e. this
/// lap's *pace*. Smaller = faster. The first lap's `gap` is measured from tick 0.
/// `peak_rssi` is the crossing's reported peak RSSI; it controls whether (and how
/// strongly) RH detects the pass, and is the value the adapter caches as the
/// lap's signal context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LapSpec {
    /// Ticks since the previous crossing (this lap's pace).
    pub gap: usize,
    /// Peak RSSI reported at this crossing.
    pub peak_rssi: i32,
}

impl LapSpec {
    /// A lap at the given pace, reusing the plan's default peak (see
    /// [`NodePlan::peak_rssi`]). The renderer substitutes the plan peak when
    /// `peak_rssi` is `0`, so `LapSpec::paced(g)` means "normal lap, `g` ticks".
    pub const fn paced(gap: usize) -> Self {
        Self { gap, peak_rssi: 0 }
    }

    /// A lap at the given pace with an explicit peak RSSI (overrides the plan
    /// default) — e.g. one marginal lap among strong ones.
    pub const fn with_peak(gap: usize, peak_rssi: i32) -> Self {
        Self { gap, peak_rssi }
    }
}

/// A single node's emulated-output schedule.
///
/// Unlike [`NodeCsv`] (uniform pace forever), a plan is an explicit list of laps,
/// each with its own gap/pace, so a CSV can encode varied pace, skipped laps
/// (long gaps), an early stop (DNF), and per-lap signal levels. Render with
/// [`plan_csv`].
///
/// The rendered `lap_id` increments once per scheduled lap and then **holds flat**
/// after the last lap (the file does not loop more laps in for that node) — that
/// is what makes "DNF" and "missed crossing" expressible: a held `lap_id` records
/// no further laps. A node with `laps: []` produces a flat, lap-less stream
/// (present but never crossing); a node omitted from [`race`] entirely is *silent*.
#[derive(Clone, Debug)]
pub struct NodePlan {
    /// The laps this node flies, in order. Empty = present but never crosses.
    pub laps: Vec<LapSpec>,
    /// Default peak RSSI for laps that don't set their own (see [`LapSpec::paced`]),
    /// and the value the adapter caches as this node's signal context.
    pub peak_rssi: i32,
    /// Baseline RSSI reported between crossings (kept valid: 1..=999).
    pub baseline_rssi: i32,
}

impl Default for NodePlan {
    fn default() -> Self {
        Self {
            laps: Vec::new(),
            peak_rssi: 150,
            baseline_rssi: 70,
        }
    }
}

impl NodePlan {
    /// The absolute tick index of each scheduled crossing (cumulative gaps).
    /// Crossings whose tick falls outside the [`TOTAL_TICKS`] window are dropped
    /// (they would never be rendered), so this is the count of laps that will
    /// actually be recorded.
    pub fn crossing_ticks(&self) -> Vec<usize> {
        let mut ticks = Vec::with_capacity(self.laps.len());
        let mut t = 0usize;
        for lap in &self.laps {
            t += lap.gap;
            if t < TOTAL_TICKS {
                ticks.push(t);
            }
        }
        ticks
    }
}

/// Render one node's `mock_data_{N}.csv` from a [`NodePlan`].
///
/// Columns are identical to [`node_csv`]. `lap_id` starts at 0 and increments by
/// one at each scheduled crossing tick (so `lap_id == number of laps recorded so
/// far`), then holds at its final value for the rest of the file. On a crossing
/// tick the row reports that lap's peak RSSI and `cross = T`; every other row
/// reports `baseline_rssi` and `cross = F`. `pass_peak` carries the lap's peak on
/// *every* row so the node's signal level stays a stable, assertable value (it
/// reflects the most recent crossing, like the real node's `pass_peak_rssi`).
///
/// As in [`node_csv`], the coarse `node_peak` (col 4) stays stable at `last_peak + 30` while the
/// dense history columns carry a textured per-tick peak/nadir envelope (see
/// [`csv_row`]/[`dense_envelope`]), so the path-2 trace pulled at heat end is high-resolution.
pub fn plan_csv(plan: &NodePlan) -> String {
    let crossings = plan.crossing_ticks();
    // Map tick -> peak for fast lookup, and precompute the running lap_id.
    let mut next = 0usize; // index into crossings
    let mut lap_id = 0usize;
    // Most recent crossing peak, for the per-row pass_peak (signal context).
    let mut last_peak = plan.peak_rssi;
    // The tick window of the current lap, for the dense envelope's phase: the previous crossing
    // (or 0) and the next scheduled crossing (or the window end).
    let mut prev_cross = 0usize;
    let mut lines = Vec::with_capacity(TOTAL_TICKS);
    for i in 0..TOTAL_TICKS {
        let on_lap = next < crossings.len() && crossings[next] == i;
        if on_lap {
            lap_id += 1;
            let spec = &plan.laps[lap_id - 1];
            last_peak = if spec.peak_rssi != 0 {
                spec.peak_rssi
            } else {
                plan.peak_rssi
            };
            prev_cross = i;
            next += 1;
        }
        let node_peak = last_peak + 30;
        let rssi = if on_lap {
            last_peak
        } else {
            plan.baseline_rssi
        };
        let cross = if on_lap { 'T' } else { 'F' };
        // Phase within the current lap window: rises from the previous crossing toward the next.
        let next_cross = crossings.get(next).copied().unwrap_or(TOTAL_TICKS);
        let window = next_cross.saturating_sub(prev_cross).max(1);
        let phase = (i.saturating_sub(prev_cross)) as f64 / window as f64;
        let (peak_hist, nadir_hist) = dense_envelope(last_peak, plan.baseline_rssi, phase);
        lines.push(csv_row(
            lap_id, rssi, node_peak, last_peak, cross, peak_hist, nadir_hist,
        ));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The ready-made scenario menu: high-level constructors that each describe a
/// real-world race situation, returning a [`NodePlan`] (single node) or a full
/// multi-node `(node_index, csv)` list.
///
/// Each generator's doc states **what it simulates** and **what an e2e test should
/// assert** — always structural (lap counts, relative ordering, signal magnitude),
/// never exact µs (mock timing is approximate; see crate docs / testing-strategy
/// §5.1).
pub mod scenarios {
    use super::{ENTER_THRESHOLD, LapSpec, NodePlan};

    /// Default strong peak used by the helpers (well above [`ENTER_THRESHOLD`]).
    pub const STRONG_PEAK: i32 = 200;
    /// A clearly-detected-but-weak peak (above threshold, below [`STRONG_PEAK`]).
    pub const WEAK_PEAK: i32 = 120;
    /// A *marginal* peak: just above the enter threshold — a pass that barely
    /// registers (dirty signal, gate at the edge of range).
    pub const MARGINAL_PEAK: i32 = ENTER_THRESHOLD + 5;

    /// **Uniform pace** — `laps` crossings, every `gap` ticks, strong signal.
    ///
    /// Simulates: a clean, metronomic pilot. The [`super::node_csv`] base case,
    /// but as a finite plan (laps stop after `laps`, where `node_csv` loops
    /// forever).
    ///
    /// Assert: exactly `laps` laps recorded for the node, roughly evenly spaced
    /// (ordering, not µs), stable strong signal context.
    pub fn uniform(laps: usize, gap: usize) -> NodePlan {
        NodePlan {
            laps: (0..laps).map(|_| LapSpec::paced(gap)).collect(),
            peak_rssi: STRONG_PEAK,
            ..NodePlan::default()
        }
    }

    /// **Varied pace** — one lap per supplied gap, so per-lap pace differs.
    ///
    /// Simulates: a real pilot whose lap times wander (a bobble, a clean line,
    /// traffic). `gaps` is read in order as each lap's tick spacing.
    ///
    /// Assert: `gaps.len()` laps recorded; relative ordering of lap *durations*
    /// follows the gap ordering (slower gap => longer lap), tolerance-based — never
    /// exact times.
    pub fn varied_pace(gaps: &[usize]) -> NodePlan {
        NodePlan {
            laps: gaps.iter().map(|&g| LapSpec::paced(g)).collect(),
            peak_rssi: STRONG_PEAK,
            ..NodePlan::default()
        }
    }

    /// **Missed crossing** — a normal cadence with one oversized gap where a lap
    /// is skipped (the `lap_id` holds across the dead stretch).
    ///
    /// Simulates: the timing gate failing to register one pass (craft too high, RF
    /// null, momentary dropout) — the pilot kept flying but one lap never recorded.
    /// `before`/`after` laps fly at `gap`; between them is a single gap of
    /// `gap * miss_factor` with no crossing.
    ///
    /// Assert: `before + after` laps recorded (NOT `before + after + 1`), and a
    /// detectable time gap (one lap roughly `miss_factor`× longer) where the missed
    /// lap should have been.
    pub fn missed_crossing(
        before: usize,
        after: usize,
        gap: usize,
        miss_factor: usize,
    ) -> NodePlan {
        let mut laps: Vec<LapSpec> = (0..before).map(|_| LapSpec::paced(gap)).collect();
        // The first lap after the dead stretch is reached only after the long gap.
        laps.push(LapSpec::paced(gap * miss_factor));
        laps.extend((1..after).map(|_| LapSpec::paced(gap)));
        NodePlan {
            laps,
            peak_rssi: STRONG_PEAK,
            ..NodePlan::default()
        }
    }

    /// **DNF / drops out** — `flown` normal laps, then nothing for the rest of the
    /// file (`lap_id` stops incrementing and stays flat).
    ///
    /// Simulates: a crash / dead battery / pull-out — the pilot completes some laps
    /// then never crosses again while the race continues for others.
    ///
    /// Assert: exactly `flown` laps recorded for this node, and that it records no
    /// further laps even though peers keep lapping (per-node independence).
    pub fn dnf(flown: usize, gap: usize) -> NodePlan {
        NodePlan {
            laps: (0..flown).map(|_| LapSpec::paced(gap)).collect(),
            peak_rssi: STRONG_PEAK,
            ..NodePlan::default()
        }
    }

    /// **Marginal RSSI** — `laps` laps whose peak sits just above the enter
    /// threshold ([`MARGINAL_PEAK`]).
    ///
    /// Simulates: a craft at the edge of range / a dirty channel — passes that
    /// only barely clear detection.
    ///
    /// Assert: laps still record (peak is above threshold), and the cached signal
    /// context is *low* (near [`ENTER_THRESHOLD`]) — distinguishable from a strong
    /// node for any signal-quality logic.
    pub fn marginal(laps: usize, gap: usize) -> NodePlan {
        NodePlan {
            laps: (0..laps)
                .map(|_| LapSpec::with_peak(gap, MARGINAL_PEAK))
                .collect(),
            peak_rssi: MARGINAL_PEAK,
            ..NodePlan::default()
        }
    }

    /// **Strong signal** — `laps` laps at a high, comfortably-detected peak
    /// ([`STRONG_PEAK`]). Pair with [`weak`] for signal-context assertions.
    ///
    /// Simulates: a close, clean pass. Assert: high cached signal context,
    /// distinctly greater than a [`weak`]/[`marginal`] node.
    pub fn strong(laps: usize, gap: usize) -> NodePlan {
        NodePlan {
            laps: (0..laps).map(|_| LapSpec::paced(gap)).collect(),
            peak_rssi: STRONG_PEAK,
            ..NodePlan::default()
        }
    }

    /// **Weak signal** — `laps` laps at a low-but-above-threshold peak
    /// ([`WEAK_PEAK`]).
    ///
    /// Simulates: a distant-but-detected pass. Assert: cached signal context is
    /// lower than [`strong`] yet still records every lap — magnitude ordering
    /// (`strong > weak > marginal`) is the assertable property.
    pub fn weak(laps: usize, gap: usize) -> NodePlan {
        NodePlan {
            laps: (0..laps).map(|_| LapSpec::paced(gap)).collect(),
            peak_rssi: WEAK_PEAK,
            ..NodePlan::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-node composition (`race` / `simultaneous`)
// ---------------------------------------------------------------------------

/// Compose per-node plans into the `(node_index, csv)` list [`RhContainer::start`]
/// takes. Node indices are assigned in slice order (0, 1, 2, …).
///
/// A **silent node** is simply one that is *not* in this list: omit a node and RH
/// sees no `mock_data` file for it, so it never crosses. (A node that is present
/// but never crosses — e.g. armed-but-grounded — is instead an empty-`laps`
/// [`NodePlan`], which still emits a flat CSV.) To leave a gap in the index space
/// for an intentionally-silent middle node, build the `Vec` yourself.
pub fn race(plans: &[NodePlan]) -> Vec<(usize, String)> {
    plans
        .iter()
        .enumerate()
        .map(|(i, plan)| (i, plan_csv(plan)))
        .collect()
}

/// **Simultaneous passes** — `nodes` nodes that all cross on the *same* ticks, so
/// their `lap_id` increments line up tick-for-tick.
///
/// Simulates: a pack crossing the gate together — the hardest dedup/ordering case,
/// where the server must record one lap per node for the same instant.
///
/// Produces `nodes` plans, each flying `laps` laps every `gap` ticks starting from
/// the same offset, returned as the `(node_index, csv)` race list. Because timing
/// is only approximate once RH reads the files, this guarantees *aligned schedules*
/// (same crossing ticks), not byte-identical wall-clock crossings.
///
/// Assert: each node records `laps` laps; no lap is dropped or double-counted
/// despite the alignment; per-node independence holds under contention.
pub fn simultaneous(nodes: usize, laps: usize, gap: usize) -> Vec<(usize, String)> {
    let plans: Vec<NodePlan> = (0..nodes).map(|_| scenarios::uniform(laps, gap)).collect();
    race(&plans)
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

    /// The container's docker name — so a test can drive its lifecycle directly (e.g. `docker stop`
    /// it to simulate a RotorHazard drop-off). Final cleanup still happens via the RAII `Drop`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stop the container (a real RotorHazard drop-off) without removing it — the link the app holds
    /// is severed, so the driver's liveness monitor should observe the drop. `Drop` still removes
    /// the (now-stopped) container at end of test. Used by the drop-detection live test (#105).
    pub fn stop(&self) {
        let _ = Command::new("docker")
            .args(["stop", "-t", "0", &self.name])
            .output();
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

// ---------------------------------------------------------------------------
// Tests — on the *generated CSV shape* only (no Docker). Deterministic + fast;
// these run in the core `cargo xtask ci` suite.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::scenarios::*;
    use super::*;

    /// Parse a rendered CSV into per-row fields, asserting the column count.
    fn rows(csv: &str) -> Vec<Vec<String>> {
        let rows: Vec<Vec<String>> = csv
            .lines()
            .map(|l| l.split(',').map(|s| s.to_string()).collect::<Vec<_>>())
            .collect();
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.len(), 16, "row {i} has {} columns, want 16", r.len());
        }
        rows
    }

    /// The `lap_id` (column 1) sequence for each row.
    fn lap_ids(csv: &str) -> Vec<usize> {
        rows(csv)
            .iter()
            .map(|r| r[1].parse::<usize>().expect("lap_id is a number"))
            .collect()
    }

    /// Count of crossing rows (column 7 == "T").
    fn crossings(csv: &str) -> usize {
        rows(csv).iter().filter(|r| r[7] == "T").count()
    }

    /// `lap_id` must be monotonic non-decreasing for any plan.
    fn assert_monotonic(ids: &[usize]) {
        for w in ids.windows(2) {
            assert!(w[1] >= w[0], "lap_id went backwards: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn fixed_length_and_columns() {
        let csv = plan_csv(&uniform(5, 20));
        let r = rows(&csv);
        assert_eq!(r.len(), TOTAL_TICKS);
        // node_csv keeps its shape too.
        assert_eq!(rows(&node_csv(&NodeCsv::default())).len(), TOTAL_TICKS);
    }

    #[test]
    fn uniform_records_expected_laps() {
        let csv = plan_csv(&uniform(8, 30));
        let ids = lap_ids(&csv);
        assert_monotonic(&ids);
        // lap_id increments exactly 8 times, ending at 8.
        assert_eq!(*ids.last().unwrap(), 8);
        assert_eq!(crossings(&csv), 8);
        // Each crossing row aligns with a lap_id increment.
        let inc = ids.windows(2).filter(|w| w[1] == w[0] + 1).count();
        assert_eq!(inc, 8);
    }

    #[test]
    fn varied_pace_spacing_follows_gaps() {
        let gaps = [10usize, 40, 15, 60];
        let plan = varied_pace(&gaps);
        assert_eq!(plan.crossing_ticks(), vec![10, 50, 65, 125]);
        let csv = plan_csv(&plan);
        assert_eq!(crossings(&csv), gaps.len());
        assert_eq!(*lap_ids(&csv).last().unwrap(), gaps.len());
    }

    #[test]
    fn missed_crossing_leaves_a_gap_and_drops_one_lap() {
        let gap = 20;
        let plan = missed_crossing(3, 3, gap, 3); // 3 before, 3 after, one 3x gap
        let ticks = plan.crossing_ticks();
        // 6 laps recorded, not 7 (the missed one never crosses).
        assert_eq!(ticks.len(), 6);
        let csv = plan_csv(&plan);
        assert_eq!(crossings(&csv), 6);
        assert_eq!(*lap_ids(&csv).last().unwrap(), 6);
        // A detectable gap: the longest inter-crossing interval is the missed one.
        let max_gap = ticks.windows(2).map(|w| w[1] - w[0]).max().unwrap();
        assert_eq!(max_gap, gap * 3);
        assert!(max_gap > gap * 2, "missed gap should dwarf a normal lap");
    }

    #[test]
    fn dnf_stops_incrementing() {
        let csv = plan_csv(&dnf(4, 25));
        let ids = lap_ids(&csv);
        assert_monotonic(&ids);
        assert_eq!(crossings(&csv), 4);
        assert_eq!(*ids.last().unwrap(), 4);
        // After the 4th crossing the tail is flat at 4 for the rest of the file.
        let last_cross_tick = dnf(4, 25).crossing_ticks().pop().unwrap();
        assert!(last_cross_tick < TOTAL_TICKS - 1);
        assert_eq!(ids[TOTAL_TICKS - 1], 4);
        // No crossings after the DNF point.
        let tail_crossings = rows(&csv)[last_cross_tick + 1..]
            .iter()
            .filter(|r| r[7] == "T")
            .count();
        assert_eq!(tail_crossings, 0);
    }

    #[test]
    fn empty_plan_has_no_laps() {
        let csv = plan_csv(&NodePlan::default());
        assert_eq!(crossings(&csv), 0);
        assert!(lap_ids(&csv).iter().all(|&id| id == 0));
        assert_eq!(rows(&csv).len(), TOTAL_TICKS);
    }

    #[test]
    fn marginal_vs_strong_peaks_differ() {
        let strong_csv = plan_csv(&strong(3, 20));
        let weak_csv = plan_csv(&weak(3, 20));
        let marginal_csv = plan_csv(&marginal(3, 20));

        // Peak RSSI is reported in pass_peak (column 5) on every row.
        let peak = |csv: &str| rows(csv)[0][5].parse::<i32>().unwrap();
        let s = peak(&strong_csv);
        let w = peak(&weak_csv);
        let m = peak(&marginal_csv);
        assert!(s > w && w > m, "magnitude ordering: {s} > {w} > {m}");
        assert!(m > ENTER_THRESHOLD, "marginal still clears the threshold");
        assert!(
            m < ENTER_THRESHOLD + 20,
            "marginal sits *just* above the threshold"
        );
        // All three still record their laps.
        assert_eq!(crossings(&strong_csv), 3);
        assert_eq!(crossings(&marginal_csv), 3);
    }

    #[test]
    fn per_lap_peak_overrides_plan_default() {
        let plan = NodePlan {
            laps: vec![
                LapSpec::paced(20),
                LapSpec::with_peak(20, MARGINAL_PEAK),
                LapSpec::paced(20),
            ],
            peak_rssi: STRONG_PEAK,
            ..NodePlan::default()
        };
        let csv = plan_csv(&plan);
        let r = rows(&csv);
        // pass_peak (col 5) reflects the most recent crossing's peak.
        assert_eq!(r[20][5].parse::<i32>().unwrap(), STRONG_PEAK);
        assert_eq!(r[40][5].parse::<i32>().unwrap(), MARGINAL_PEAK);
        assert_eq!(r[60][5].parse::<i32>().unwrap(), STRONG_PEAK);
    }

    #[test]
    fn race_assigns_sequential_node_indices() {
        let plans = vec![uniform(3, 20), dnf(1, 20), weak(2, 20)];
        let out = race(&plans);
        assert_eq!(
            out.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(crossings(&out[0].1), 3);
        assert_eq!(crossings(&out[1].1), 1);
        assert_eq!(crossings(&out[2].1), 2);
    }

    #[test]
    fn simultaneous_increments_line_up() {
        let nodes = 3;
        let out = simultaneous(nodes, 5, 30);
        assert_eq!(out.len(), nodes);
        // Every node crosses on the same ticks => identical lap_id sequences.
        let baseline = lap_ids(&out[0].1);
        assert_monotonic(&baseline);
        assert_eq!(*baseline.last().unwrap(), 5);
        for (_, csv) in &out[1..] {
            assert_eq!(lap_ids(csv), baseline, "nodes' lap_id curves must align");
            assert_eq!(crossings(csv), 5);
        }
    }

    #[test]
    fn crossings_past_window_are_dropped() {
        // 50 laps * 20 ticks = 1000 ticks, but the window is only 600.
        let plan = uniform(50, 20);
        let ticks = plan.crossing_ticks();
        assert!(ticks.iter().all(|&t| t < TOTAL_TICKS));
        assert_eq!(ticks.len(), (TOTAL_TICKS - 1) / 20); // 29
        let csv = plan_csv(&plan);
        assert_eq!(crossings(&csv), ticks.len());
    }
}
