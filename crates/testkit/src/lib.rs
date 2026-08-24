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
//!
//! ## Which RotorHazard, with or without the plugin
//!
//! A test doesn't choose either: both come from the environment, so one test body runs in
//! every configuration of `cargo xtask live`'s matrix. [`RH_VERSION_ENV`] picks the version
//! (image `gridfpv-rotorhazard:<version>`, built on demand by [`ensure_rh_image`]);
//! [`PLUGIN_ENV`] names a plugin dir to mount, and unset means stock RotorHazard.

use std::fs;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How to synthesise one node's emulated output stream.
///
/// The stream is **not** a square peak/nadir envelope: each lap is modelled as a low
/// [`NodeCsv::baseline_rssi`] floor plus a smooth Gaussian-ish bell centred on the gate crossing
/// (rise → peak → fall over the approach/depart window), with a few RSSI units of deterministic
/// per-sample noise on baseline *and* peak, and slight lap-to-lap variation in peak height. See
/// [`pass_profile`] for the model.
pub struct NodeCsv {
    /// Ticks (CSV lines) between lap increments — roughly `lap_duration /
    /// RH_UPDATE_INTERVAL`. At a realistic ~50–100 Hz sample rate (10–20 ms/tick) a lap is many
    /// dozens of ticks, so the default is far denser than the old square-wave `6`. Smaller =
    /// faster laps. See [`DEFAULT_TICKS_PER_LAP`].
    pub ticks_per_lap: usize,
    /// The lap's **peak** RSSI: the height of the bell at the gate crossing (kept valid: 1..=999).
    /// Lap-to-lap variation nudges this a few units per lap; the noise floor jitters it per sample.
    pub peak_rssi: i32,
    /// Baseline RSSI between crossings, while the craft is away from the gate (kept valid: 1..=999).
    pub baseline_rssi: i32,
    /// Deterministic-RNG seed for this node's noise and lap-to-lap variation, so two nodes in the
    /// same race get distinct (but reproducible) traces. [`race`] sets this to the node index;
    /// single-node callers can leave it at the default `0`.
    pub seed: u64,
}

impl Default for NodeCsv {
    fn default() -> Self {
        Self {
            ticks_per_lap: DEFAULT_TICKS_PER_LAP,
            peak_rssi: 150,
            baseline_rssi: 70,
            seed: 0,
        }
    }
}

/// Realistic default sample density. At a ~50–100 Hz mock tick (10–20 ms/sample) a real lap spans
/// many dozens of samples, so the bell profile is resolved smoothly rather than as a 6-point square.
/// The `cargo xtask rh-mock feed --tick T` knob sets the *wall-clock* seconds per tick (RH's
/// `RH_UPDATE_INTERVAL`); this constant sets how many ticks make up one lap in the generated CSV.
pub const DEFAULT_TICKS_PER_LAP: usize = 48;

/// Half-width of the gate-pass bell, as a fraction of the lap window. The Gaussian peak occupies
/// roughly the final ~30% of the approach and the first ~30% of the depart around the crossing; the
/// rest of the lap sits at the noisy baseline floor while the craft is away from the gate.
const BELL_SIGMA_FRAC: f64 = 0.16;

/// A tiny deterministic xorshift64* PRNG, seeded per `(node, sample)` so the noise is reproducible
/// across runs (the repo bans `Math.random`/wall-clock in generators). Returns a value in `0.0..1.0`.
fn det_unit(node_seed: u64, sample: u64, salt: u64) -> f64 {
    // SplitMix-style avalanche of the keyed input, then one xorshift* step.
    let mut x = node_seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(sample.wrapping_mul(0xD1B5_4A32_D192_ED03))
        .wrapping_add(salt.wrapping_mul(0xCA5A_8267_6F49_5C2D))
        .wrapping_add(0x2545_F491_4F6C_DD1D);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    // Top 53 bits -> [0,1).
    (r >> 11) as f64 / (1u64 << 53) as f64
}

/// Deterministic symmetric noise in `±amp` RSSI units for one sample, keyed by node+sample.
fn det_noise(node_seed: u64, sample: u64, salt: u64, amp: f64) -> f64 {
    (det_unit(node_seed, sample, salt) - 0.5) * 2.0 * amp
}

/// The realistic per-tick RSSI **value** for one node at sample `i`, given the **crossing tick** it
/// is nearest to (`cross_tick`), the lap window (`tpl` ticks), the lap's peak, the node baseline,
/// and the node seed.
///
/// Model (what real per-gate-pass RSSI looks like):
/// - a low **baseline floor** while the craft is away from the gate,
/// - a smooth **Gaussian bell** centred on `cross_tick` (rise → peak → fall over the approach and
///   departure window) — not a square step,
/// - small **deterministic noise** (a few RSSI units) so the trace is never perfectly smooth, yet
///   stays reproducible (seeded xorshift, no wall-clock). The noise is *shaped* (see the body): full
///   ± jitter on the off-gate baseline, but rectified **upward** at the crest so the reported peak
///   never dips below the lap's true peak (`node_peak`/`pass_peak` are running maxima). That keeps
///   the crossing **reliably detectable** — its captured peak always clears RH's calibrated `enter`
///   by the lap's full peak-to-baseline margin — while the baseline stays clear of `exit`.
///
/// `noise_amp` is the per-sample jitter amplitude (scenarios raise it for a "noisy" feed). The bell
/// peaks **at** `cross_tick`, where RH's crossing detection fires (`cross=T`), so the signal maximum
/// and the recorded crossing align. Returned value is clamped valid (`1..=999`).
fn pass_value(
    node_seed: u64,
    sample: usize,
    cross_tick: usize,
    tpl: usize,
    peak: i32,
    baseline: i32,
    noise_amp: f64,
) -> i32 {
    let window = tpl.max(1) as f64;
    // Signed distance (in lap-fractions) from this sample to the nearest crossing.
    let dist = (cross_tick as f64 - sample as f64) / window;
    // Gaussian bell: 1.0 at the crossing, decaying to ~baseline as the craft moves off-gate.
    let sigma = BELL_SIGMA_FRAC.max(1e-3);
    let bell = (-(dist * dist) / (2.0 * sigma * sigma)).exp();
    let span = (peak - baseline) as f64;
    let smooth = baseline as f64 + span * bell;
    // Noise model, shaped so the trace stays realistic *and* the crossing stays detectable:
    //
    // - **Off-gate (baseline)** carries full bidirectional jitter (`±noise_amp`) so the floor has
    //   real texture — but its amplitude is bounded (see [`BASE_NOISE`]) to stay clear of RH's exit
    //   threshold, so the detector cleanly drops below `exit` between passes and re-arms.
    // - **Near the crest** the jitter is *rectified upward* (`bell`-weighted): `node_peak`/`pass_peak`
    //   are running **maxima** on a real timer, so sensor noise around a pass crest only ever pushes
    //   the reported peak *up*, never carves a notch below the true peak. This keeps the crest the
    //   unambiguous maximum it physically is, guaranteeing the captured peak clears RH's calibrated
    //   `enter` with the lap's full peak-to-baseline margin (no noise-induced dip below threshold).
    //
    // The two blend by `bell`: pure ± noise at the floor, pure additive crest noise at the peak.
    let floor_noise = det_noise(node_seed, sample as u64, 0xA1, noise_amp) * (1.0 - bell);
    let crest_noise = det_unit(node_seed, sample as u64, 0xB2) * noise_amp * 0.4 * bell;
    (smooth + floor_noise + crest_noise)
        .round()
        .clamp(1.0, 999.0) as i32
}

/// Lap-to-lap variation: nudge a lap's peak height a few RSSI units (deterministically, keyed by
/// node+lap) so no two passes are identical. Returns the varied peak, clamped valid.
fn varied_peak(node_seed: u64, lap_index: usize, peak: i32) -> i32 {
    // ±~4% of the peak, at least ±2 units, so even low peaks vary visibly.
    let amp = (peak as f64 * 0.04).max(2.0);
    (peak as f64 + det_noise(node_seed, lap_index as u64, 0xC3, amp))
        .round()
        .clamp(1.0, 999.0) as i32
}

/// Render one CSV row (one mock tick) in RotorHazard `MockInterface` column order.
///
/// Columns: `idx, lap_id, ms, rssi, node_peak, pass_peak, loop_time, cross(T/F),
/// pass_nadir, node_nadir, peakRssi, pkFirst, pkLast, nadirRssi, ndFirst, ndLast`.
///
/// Two signal fidelities ride in one row, and both now carry the realistic gate-pass shape:
/// - **Coarse** — `node_peak` (col 4) is what `node_data` streams and the adapter's coarse
///   [`SignalChunk`] trace samples once per heartbeat. It carries the per-tick **bell + noise**
///   value so the *live* trace already looks like real RSSI (rise → peak → fall, not a square step).
/// - **Dense** — the `peakRssi`/`nadirRssi` history columns (10/13) with their first/last times
///   (11/12, 14/15) feed RotorHazard's per-tick `history_values`/`history_times` accumulator
///   (`BaseHardwareInterface.PeakNadirHistory.addTo`), the dense trace its marshal page reviews and
///   our path-2 (`current_marshal_data`/`get_pilotrace`) pulls at heat end. Distinct **first/last**
///   times (`pkFirst > pkLast`) make RH log *two* history entries per tick, raising density; the
///   per-tick `peak_rssi_hist`/`nadir_rssi_hist` carry the same bell-shaped, noisy envelope so the
///   captured dense trace has real texture at full per-tick resolution — what the graph is judged on.
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
/// Both the coarse `node_peak` (col 4) and the dense history columns (10/13) now carry the
/// realistic gate-pass value — a baseline floor plus a Gaussian bell centred on each crossing, with
/// deterministic per-sample noise (see [`pass_value`]) and slight lap-to-lap peak variation (see
/// [`varied_peak`]). The peak lands exactly on the crossing tick, so RH's crossing detection still
/// fires there and laps still record.
pub fn node_csv(opts: &NodeCsv) -> String {
    let tpl = opts.ticks_per_lap.max(1);
    let mut lines = Vec::with_capacity(TOTAL_TICKS);
    for i in 0..TOTAL_TICKS {
        let lap_id = i / tpl;
        let on_lap = i > 0 && i % tpl == 0;
        // The crossing tick this sample is nearest to frames the bell (crossings are at multiples
        // of `tpl`); the peak lands on it so detection and signal maximum align.
        let cross_idx = (i + tpl / 2) / tpl;
        let cross_tick = cross_idx * tpl;
        // That crossing's lap-varied peak (so each pass differs slightly).
        let peak = varied_peak(opts.seed, cross_idx.max(1), opts.peak_rssi);
        let value = pass_value(
            opts.seed,
            i,
            cross_tick,
            tpl,
            peak,
            opts.baseline_rssi,
            BASE_NOISE,
        );
        let cross = if on_lap { 'T' } else { 'F' };
        // pass_peak reports the lap's (varied) peak on EVERY tick so the node's signal level is a
        // stable, cacheable context value the adapter asserts (not the noisy per-tick value).
        lines.push(csv_row(
            lap_id,
            value,
            value,
            peak,
            cross,
            value,
            opts.baseline_rssi,
        ));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Default per-sample baseline/peak noise amplitude (RSSI units, ±). A few units of jitter on
/// everything so the trace is never perfectly smooth. Scenarios raise this for a "noisy" feed.
pub const BASE_NOISE: f64 = 3.0;

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
    /// Per-sample noise amplitude (RSSI units, ±) for this node's trace. Defaults to [`BASE_NOISE`];
    /// the `noisy` scenario raises it for a dirtier baseline.
    pub noise_amp: f64,
    /// Deterministic-RNG seed for this node's noise and lap-to-lap variation (see [`NodeCsv::seed`]).
    /// [`race`] sets it to the node index so co-racing nodes get distinct, reproducible traces.
    pub seed: u64,
}

impl Default for NodePlan {
    fn default() -> Self {
        Self {
            laps: Vec::new(),
            peak_rssi: 150,
            baseline_rssi: 70,
            noise_amp: BASE_NOISE,
            seed: 0,
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
/// As in [`node_csv`], both the coarse `node_peak` (col 4) and the dense history columns (10/13)
/// carry the realistic gate-pass value: a baseline floor plus a Gaussian bell centred on each
/// scheduled crossing (the bell peaks *on* the `cross=T` tick so detection still fires), with
/// deterministic per-sample noise (see [`pass_value`]) and slight lap-to-lap peak variation (see
/// [`varied_peak`]). `pass_peak` (col 5) holds the most-recent crossing's nominal peak as the stable
/// signal-context value the adapter caches.
pub fn plan_csv(plan: &NodePlan) -> String {
    let crossings = plan.crossing_ticks();
    // Resolve each crossing's nominal peak (plan default unless the lap overrides it), then apply
    // lap-to-lap variation so no two passes are identical.
    let peaks: Vec<i32> = crossings
        .iter()
        .enumerate()
        .map(|(li, _)| {
            let nominal = match plan.laps.get(li) {
                Some(s) if s.peak_rssi != 0 => s.peak_rssi,
                _ => plan.peak_rssi,
            };
            varied_peak(plan.seed, li + 1, nominal)
        })
        .collect();

    let mut lap_id = 0usize;
    // Most recent crossing's NOMINAL peak (unvaried), for the per-row pass_peak signal context —
    // kept stable so signal-magnitude assertions read a clean value.
    let mut last_nominal = plan.peak_rssi;
    let mut next = 0usize; // index into crossings
    let mut lines = Vec::with_capacity(TOTAL_TICKS);
    for i in 0..TOTAL_TICKS {
        let on_lap = next < crossings.len() && crossings[next] == i;
        if on_lap {
            lap_id += 1;
            last_nominal = match plan.laps.get(lap_id - 1) {
                Some(s) if s.peak_rssi != 0 => s.peak_rssi,
                _ => plan.peak_rssi,
            };
            next += 1;
        }
        let cross = if on_lap { 'T' } else { 'F' };

        // The realistic per-tick value: the nearest scheduled crossing's bell over the baseline.
        let value = if let Some(ci) = nearest_crossing(&crossings, i) {
            let cross_tick = crossings[ci];
            // Local window = distance to the nearer neighbouring crossing (sets the bell width to
            // this lap's pace). Falls back to a sane default with only one crossing.
            let win = local_window(&crossings, ci);
            pass_value(
                plan.seed,
                i,
                cross_tick,
                win,
                peaks[ci],
                plan.baseline_rssi,
                plan.noise_amp,
            )
        } else {
            // No crossings (empty plan): a flat, noisy baseline floor.
            (plan.baseline_rssi as f64 + det_noise(plan.seed, i as u64, 0xA1, plan.noise_amp))
                .round()
                .clamp(1.0, 999.0) as i32
        };

        lines.push(csv_row(
            lap_id,
            value,
            value,
            last_nominal,
            cross,
            value,
            plan.baseline_rssi,
        ));
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Index into `crossings` of the crossing nearest to sample `i`, or `None` if there are none.
fn nearest_crossing(crossings: &[usize], i: usize) -> Option<usize> {
    crossings
        .iter()
        .enumerate()
        .min_by_key(|&(_, &t)| t.abs_diff(i))
        .map(|(ci, _)| ci)
}

/// The bell-width window for crossing `ci`: the gap to its nearer neighbour (so a fast lap gets a
/// narrow bell and a slow one a wide bell). With a single crossing, defaults to [`DEFAULT_TICKS_PER_LAP`].
fn local_window(crossings: &[usize], ci: usize) -> usize {
    let here = crossings[ci];
    let prev = ci.checked_sub(1).map(|p| here - crossings[p]);
    let next = crossings.get(ci + 1).map(|&n| n - here);
    match (prev, next) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => DEFAULT_TICKS_PER_LAP,
    }
    .max(1)
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
    /// A *marginal* peak: above the enter threshold but with only a slim margin — a pass that
    /// registers, but is the weakest the model still reliably detects (dirty signal, gate at the edge
    /// of range). The margin (`+15`) is sized so the bell crest clears RH's calibrated `enter` even
    /// after lap-to-lap peak variation (`varied_peak` can shave a few percent off a low peak), so the
    /// pass *always* records — "marginal" in magnitude, not in reliability. It stays clearly below
    /// [`WEAK_PEAK`] so the `strong > weak > marginal` ordering the signal-context tests assert holds.
    pub const MARGINAL_PEAK: i32 = ENTER_THRESHOLD + 15;

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

    /// **Noisy / dirty channel** — `laps` marginal-peak laps with a much louder per-sample noise
    /// floor (`noise_amp` ~3× the [`super::BASE_NOISE`] default).
    ///
    /// Simulates: a dirty RF environment — a jittery baseline and a marginal peak that only just
    /// clears the enter threshold. Distinct from [`marginal`] (clean baseline) in the *texture* of
    /// the trace: the marshaling graph should look genuinely grainy.
    ///
    /// Assert: laps still record (the peak clears threshold despite the noise); the captured trace
    /// has a visibly higher baseline variance than a clean node.
    pub fn noisy(laps: usize, gap: usize) -> NodePlan {
        NodePlan {
            laps: (0..laps)
                .map(|_| LapSpec::with_peak(gap, MARGINAL_PEAK + 10))
                .collect(),
            peak_rssi: MARGINAL_PEAK + 10,
            noise_amp: super::BASE_NOISE * 3.0,
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
        .map(|(i, plan)| {
            // Key each node's deterministic noise/variation to its index so co-racing nodes get
            // distinct (but reproducible) traces, even from identical plans (e.g. a `pack`).
            let seeded = NodePlan {
                seed: i as u64,
                ..plan.clone()
            };
            (i, plan_csv(&seeded))
        })
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

/// The RotorHazard version the harness runs unless told otherwise: the current stable
/// (RHAPI 1.4). One locally-built image per version, tagged `gridfpv-rotorhazard:<version>`.
pub const DEFAULT_RH_VERSION: &str = "4.4.0";

/// The **declared floor** (D16): RHAPI 1.3 / RotorHazard v4.3.0+ — the version the field
/// timers actually run. The version × plugin matrix (`cargo xtask live`) covers this leg as
/// well as [`DEFAULT_RH_VERSION`], so a regression that only bites on the floor — or only
/// with the plugin installed (#389) — cannot hide behind a single green configuration.
pub const FLOOR_RH_VERSION: &str = "4.3.0";

/// Env var selecting which RotorHazard version the container runs — e.g. `4.3.0`
/// (a leading `v` is accepted). Unset = [`DEFAULT_RH_VERSION`]. `cargo xtask live` sets it
/// per matrix configuration, the same way [`PLUGIN_ENV`] selects with/without the plugin.
pub const RH_VERSION_ENV: &str = "GRIDFPV_RH_VERSION";

/// Normalize a version string to the bare `x.y.z` form used in image tags (`v4.3.0` →
/// `4.3.0`); blank falls back to [`DEFAULT_RH_VERSION`].
fn normalize_version(version: &str) -> String {
    let v = version.trim().trim_start_matches(['v', 'V']);
    if v.is_empty() {
        DEFAULT_RH_VERSION.to_string()
    } else {
        v.to_string()
    }
}

/// The RotorHazard version this process's containers run: [`RH_VERSION_ENV`] if set,
/// else [`DEFAULT_RH_VERSION`].
pub fn rh_version() -> String {
    normalize_version(&std::env::var(RH_VERSION_ENV).unwrap_or_default())
}

/// The locally-built RotorHazard harness image for `version` (mock nodes,
/// `MOCK_NODE_SIGNAL=1` so `mock_data_{N}.csv` drives the signal). Built from
/// `docker/rotorhazard/Dockerfile` with `--build-arg RH_VERSION=v<version>`;
/// [`RhContainer::start`] builds it on demand if it is missing, so `cargo xtask live`
/// stays a one-command run.
///
/// This **replaces** the old `cruwaller/rotorhazard:latest` pull, which is stale at
/// 4.0.0-beta.4 — below the GridFPV plugin floor (RHAPI 1.3 / RH v4.3.0+, D16).
pub fn rh_image_for(version: &str) -> String {
    format!("gridfpv-rotorhazard:{}", normalize_version(version))
}

/// The image this process's containers run — [`rh_image_for`] at [`rh_version`].
pub fn rh_image() -> String {
    rh_image_for(&rh_version())
}

/// The server dir inside the image: `DATA_DIR` (= `PROGRAM_DIR`), so `mock_data_{N}.csv`
/// and the user `plugins/` dir both resolve here (see `docker/rotorhazard/Dockerfile`).
const RH_SERVER_DIR: &str = "/opt/RotorHazard/src/server";

/// Env var pointing at a host plugin directory to mount into the container's user
/// `plugins/` dir as `plugins/gridfpv` (read-only). Set by `cargo xtask live` /
/// `rh-mock` so live runs boot against the GridFPV plugin; unset = stock RH (the
/// socket-fallback path). Mounting an empty/placeholder plugin is harmless.
///
/// Together with [`RH_VERSION_ENV`] this is the axis of `cargo xtask live`'s matrix: the
/// no-plugin legs simply run with this **unset**, which is the only way the stock
/// `current_laps` snapshot path gets live coverage at all (#389).
pub const PLUGIN_ENV: &str = "GRIDFPV_RH_PLUGIN";

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
    ///
    /// The GridFPV plugin is mounted when the [`PLUGIN_ENV`] env var points at a plugin
    /// directory — the path `cargo xtask live` uses to boot every live RH against the
    /// plugin. In-process callers that want to mount a specific plugin explicitly use
    /// [`RhContainer::start_with_plugin`].
    pub fn start(port: u16, update_interval: &str, csvs: &[(usize, String)]) -> Self {
        Self::start_with_plugin(port, update_interval, csvs, None)
    }

    /// Like [`RhContainer::start`], but with an explicit GridFPV `plugin_dir` to mount into the
    /// container's user `plugins/gridfpv` (read-only). `Some(dir)` takes precedence; with `None`
    /// the [`PLUGIN_ENV`] env var is consulted as a fallback (the path `cargo xtask live` uses).
    /// For mounting additional plugins (e.g. the test-only `gridfpv_mock`) use
    /// [`start_with_plugins`](Self::start_with_plugins).
    pub fn start_with_plugin(
        port: u16,
        update_interval: &str,
        csvs: &[(usize, String)],
        plugin_dir: Option<PathBuf>,
    ) -> Self {
        let plugin_dir = plugin_dir.or_else(|| std::env::var_os(PLUGIN_ENV).map(PathBuf::from));
        let plugins: Vec<(&str, PathBuf)> =
            plugin_dir.into_iter().map(|dir| ("gridfpv", dir)).collect();
        let refs: Vec<(&str, &Path)> = plugins.iter().map(|(n, d)| (*n, d.as_path())).collect();
        Self::start_with_plugins(port, update_interval, csvs, &refs)
    }

    /// Start RotorHazard with **any number of plugins** mounted (read-only) into the container's
    /// user `plugins/<folder>` dirs — each `(folder, host_dir)`. Used by `cargo xtask rh-mock` to
    /// boot the real `gridfpv` plugin and/or the test-only `gridfpv_mock` control plugin together
    /// (xtask can't set process env under `#![forbid(unsafe_code)]`, so it passes dirs explicitly).
    pub fn start_with_plugins(
        port: u16,
        update_interval: &str,
        csvs: &[(usize, String)],
        plugins: &[(&str, &Path)],
    ) -> Self {
        ensure_image();

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
                "{}:{RH_SERVER_DIR}/mock_data_{}.csv",
                file.display(),
                node_index + 1
            ));
        }
        // Mount each requested plugin into the container's user `plugins/<folder>` (read-only).
        // Plugins are additive — a placeholder loads cleanly and stock-RH behavior is unchanged.
        for (folder, dir) in plugins {
            if dir.is_dir() {
                args.push("-v".into());
                args.push(format!(
                    "{}:{RH_SERVER_DIR}/plugins/{folder}:ro",
                    dir.display()
                ));
            } else {
                eprintln!(
                    "plugin dir {} for `{folder}` is not a directory; skipping that mount",
                    dir.display()
                );
            }
        }
        args.push(rh_image());

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
        // …then wait for RH to actually answer HTTP (docker binds the port long before the
        // server serves), and give it a moment more to finish booting its socket API.
        wait_for_http(port, Duration::from_secs(120));
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

    /// The container's combined stdout+stderr so far (`docker logs`). Used by
    /// `cargo xtask rh-mock plugin-check` to assert the GridFPV plugin loaded.
    pub fn logs(&self) -> String {
        Command::new("docker")
            .args(["logs", &self.name])
            .output()
            .map(|o| {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                s.push_str(&String::from_utf8_lossy(&o.stderr));
                s
            })
            .unwrap_or_default()
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

/// Ensure the image for the version this process runs ([`rh_version`]) exists locally.
fn ensure_image() {
    ensure_rh_image(&rh_version());
}

/// Ensure `gridfpv-rotorhazard:<version>` exists locally, building it from
/// `docker/rotorhazard/Dockerfile` (`--build-arg RH_VERSION=v<version>`) if not. Keeps
/// `cargo xtask live`/`rh-mock` a single command now that the harness uses locally-built
/// images instead of a Docker Hub pull. The first build of a given version takes a few
/// minutes (it clones RH and pip-installs); subsequent runs are a no-op (cached). Public so
/// `cargo xtask live` can pre-build every version in its matrix up front instead of paying
/// for it mid-suite. Panics if the build fails — a live test cannot proceed without it.
pub fn ensure_rh_image(version: &str) {
    let version = normalize_version(version);
    let image = rh_image_for(&version);
    let present = Command::new("docker")
        .args(["image", "inspect", &image])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if present {
        return;
    }

    // `<crate>/../../docker/rotorhazard` — the build context (Dockerfile + mock CSV +
    // config.json + entrypoint).
    let context = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docker/rotorhazard")
        .canonicalize()
        .expect("locate docker/rotorhazard build context");

    eprintln!(
        "\x1b[1mBuilding {image}\x1b[0m from {} (first run only; ~2–4 min)…",
        context.display()
    );
    let mut cmd = Command::new("docker");
    cmd.args(["build", "-t", &image]);
    // `:latest` follows the default version only, so it never silently points at a
    // floor-version image built by one leg of the matrix.
    if version == DEFAULT_RH_VERSION {
        cmd.args(["-t", "gridfpv-rotorhazard:latest"]);
    }
    cmd.args(["--build-arg", &format!("RH_VERSION=v{version}")]);
    let status = cmd
        .arg(&context)
        .status()
        .expect("run docker build for the RH harness image");
    assert!(
        status.success(),
        "failed to build {image} from {} — see the docker build output above",
        context.display()
    );
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

/// Block until RotorHazard actually **serves HTTP** on `port`, or panic after `timeout`.
///
/// A TCP connect is not readiness: docker's port proxy binds the host port the instant the
/// container starts, so [`wait_for_port`] returns while the Python server is still booting.
/// A connect therefore succeeds and the first socket.io handshake gets a connection reset —
/// which surfaces as `IncompleteResponseFromEngineIo` seconds into a test rather than as a
/// clear "not up yet". Rare on an idle box; routine when several containers boot under load
/// (the version × plugin matrix does exactly that). So we speak just enough HTTP/1.0 to
/// require a real status line back from RH before handing the container to a test.
fn wait_for_http(port: u16, timeout: Duration) {
    use std::io::{Read, Write};

    let deadline = Instant::now() + timeout;
    let mut last = String::from("no response");
    loop {
        let served = (|| -> std::io::Result<bool> {
            let mut sock = TcpStream::connect(("127.0.0.1", port))?;
            sock.set_read_timeout(Some(Duration::from_secs(5)))?;
            sock.set_write_timeout(Some(Duration::from_secs(5)))?;
            sock.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
            let mut head = [0u8; 64];
            let read = sock.read(&mut head)?;
            let status = String::from_utf8_lossy(&head[..read]).to_string();
            let ok = status.starts_with("HTTP/") && status.contains(" 200");
            if !ok {
                last = status
                    .lines()
                    .next()
                    .unwrap_or("empty response")
                    .to_string();
            }
            Ok(ok)
        })();
        match served {
            Ok(true) => return,
            Ok(false) => {}
            Err(err) => last = err.to_string(),
        }
        if Instant::now() >= deadline {
            panic!("RotorHazard on port {port} did not serve HTTP within {timeout:?} ({last})");
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

    // ---------------------------------------------------------------------------
    // Realistic-signal model: the per-tick RSSI VALUE (col 4 node_peak / col 10
    // dense peak history) is a baseline floor + a Gaussian bell on each crossing,
    // with deterministic noise and lap-to-lap variation — not a square wave.
    // ---------------------------------------------------------------------------

    /// The per-tick streamed value (col 4 `node_peak`) for every row — what the coarse trace
    /// samples and the live sparkline draws.
    fn values(csv: &str) -> Vec<i32> {
        rows(csv)
            .iter()
            .map(|r| r[4].parse::<i32>().expect("node_peak is a number"))
            .collect()
    }

    /// A compact sparkline (used by the visual sanity test so a human can eyeball the shape).
    fn sparkline(vals: &[i32]) -> String {
        const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let min = *vals.iter().min().unwrap();
        let max = *vals.iter().max().unwrap();
        let span = (max - min).max(1);
        vals.iter()
            .map(|&v| BARS[((v - min) as i64 * 7 / span as i64) as usize])
            .collect()
    }

    #[test]
    fn value_is_bell_not_square() {
        // A single node, default density: the value should PEAK at each crossing and sit near the
        // baseline midway between crossings — a smooth rise/fall, not a two-level square wave.
        let opts = NodeCsv {
            ticks_per_lap: 48,
            peak_rssi: 200,
            baseline_rssi: 60,
            seed: 0,
        };
        let vals = values(&node_csv(&opts));
        // At a crossing tick (i = 48) the value is near the peak; halfway between (i = 72) it has
        // fallen close to the baseline.
        let at_cross = vals[48];
        let mid = vals[72];
        assert!(
            at_cross > 180,
            "value at the crossing should be near the peak (~200), got {at_cross}"
        );
        assert!(
            mid < 90,
            "value midway between crossings should fall near the baseline (~60), got {mid}"
        );
        // It is genuinely many-valued (a curve), not just two levels (peak/baseline).
        let mut distinct: Vec<i32> = vals[24..72].to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() > 10,
            "the rise→peak→fall should take many distinct values, got {}",
            distinct.len()
        );
    }

    #[test]
    fn baseline_carries_noise() {
        // Between crossings the value jitters by a few units (constant small noise) rather than
        // being a perfectly flat baseline.
        let opts = NodeCsv {
            ticks_per_lap: 48,
            peak_rssi: 200,
            baseline_rssi: 60,
            seed: 0,
        };
        let vals = values(&node_csv(&opts));
        // Sample the off-gate stretch around mid-lap (i ~ 70..78), away from the bell.
        let win = &vals[68..78];
        let min = *win.iter().min().unwrap();
        let max = *win.iter().max().unwrap();
        assert!(
            max > min,
            "off-gate baseline must jitter (noise), got a flat run: {win:?}"
        );
        assert!(
            (max - min) <= 12,
            "baseline noise should be small (a few units), got spread {} in {win:?}",
            max - min
        );
    }

    #[test]
    fn generation_is_deterministic() {
        // Same input -> byte-identical output (seeded RNG, no wall-clock).
        let opts = NodeCsv::default();
        assert_eq!(node_csv(&opts), node_csv(&opts));
        let plan = varied_pace(&[40, 60, 50]);
        assert_eq!(plan_csv(&plan), plan_csv(&plan));
    }

    #[test]
    fn lap_to_lap_peaks_vary() {
        // No two passes are identical: the peak height drifts a little lap to lap.
        let plan = uniform(6, 60);
        let vals = values(&plan_csv(&plan));
        let ticks = plan.crossing_ticks();
        let peaks: Vec<i32> = ticks.iter().map(|&t| vals[t]).collect();
        let mut uniq = peaks.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert!(
            uniq.len() > 1,
            "lap peaks should vary lap-to-lap, all equal: {peaks:?}"
        );
        // But the variation is modest — every pass is still clearly a strong peak.
        let min = *peaks.iter().min().unwrap();
        let max = *peaks.iter().max().unwrap();
        assert!(
            (max - min) < STRONG_PEAK / 5,
            "lap variation should be modest, got {min}..{max}"
        );
    }

    #[test]
    fn seeded_nodes_differ_in_a_race() {
        // Two identical pack plans get distinct traces once `race` keys them by node index.
        let out = simultaneous(2, 6, 60);
        assert_ne!(
            values(&out[0].1),
            values(&out[1].1),
            "co-racing nodes must have distinct (seeded) noise"
        );
    }

    #[test]
    fn fidelity_within_model_peak_clears_threshold() {
        // The model's fidelity invariant (mirrors the adapter e2e gate): the streamed value (what
        // the coarse trace captures) reaches the peak band at each crossing, so RH's enter
        // threshold is cleared and laps still record — and every value is a plausible RSSI in
        // [baseline-noise, peak+noise].
        const PEAK: i32 = 150;
        const BASE: i32 = 70;
        let opts = NodeCsv {
            ticks_per_lap: 48,
            peak_rssi: PEAK,
            baseline_rssi: BASE,
            seed: 0,
        };
        let vals = values(&node_csv(&opts));
        for (i, &v) in vals.iter().enumerate() {
            assert!(
                (1..=PEAK + 10).contains(&v),
                "sample {i}={v} outside the model band [1, peak+noise]"
            );
        }
        // Each crossing tick clears the enter threshold (so detection fires on a real peak).
        for cross in (48..TOTAL_TICKS).step_by(48) {
            assert!(
                vals[cross] > ENTER_THRESHOLD,
                "crossing at {cross} (value {}) must clear the enter threshold {ENTER_THRESHOLD}",
                vals[cross]
            );
        }
    }

    #[test]
    fn crest_is_rectified_to_the_nominal_peak() {
        // The detection guarantee: at the crossing tick the reported value never dips BELOW the
        // lap's (varied) peak — `node_peak`/`pass_peak` are running maxima, so crest noise only adds.
        // This is what makes the peak-above-enter margin equal to `peak - enter` with no noise dip
        // that could drop a marginal pass under the calibrated threshold.
        const PEAK: i32 = 96; // a deliberately low peak so any downward dip would breach the margin
        const BASE: i32 = 70;
        let opts = NodeCsv {
            ticks_per_lap: 48,
            peak_rssi: PEAK,
            baseline_rssi: BASE,
            seed: 7,
        };
        let vals = values(&node_csv(&opts));
        for cross in (48..TOTAL_TICKS).step_by(48) {
            let nominal = varied_peak(opts.seed, cross / 48, PEAK);
            assert!(
                vals[cross] >= nominal,
                "crest at {cross} ({}) dipped below the lap's nominal peak {nominal} — noise must \
                 rectify upward at the crest so detection never loses the margin",
                vals[cross]
            );
        }
    }

    #[test]
    fn thin_margin_scenarios_clear_enter_with_headroom() {
        // The marginal/noisy/weak scenarios must clear RH's enter threshold at EVERY crossing with a
        // real margin (after lap-to-lap variation), so passes record reliably — "marginal" in
        // magnitude, never in reliability. (The old +5 marginal could dip to the threshold and miss.)
        const MIN_MARGIN: i32 = 8;
        for (name, plan) in [
            ("marginal", marginal(8, 48)),
            ("noisy", noisy(8, 48)),
            ("weak", weak(8, 48)),
        ] {
            let csv = plan_csv(&plan);
            let rows = rows(&csv);
            for (i, r) in rows.iter().enumerate() {
                if r[7] == "T" {
                    let v: i32 = r[4].parse().unwrap();
                    assert!(
                        v >= ENTER_THRESHOLD + MIN_MARGIN,
                        "{name} crossing at {i} (value {v}) must clear enter {ENTER_THRESHOLD} by at \
                         least {MIN_MARGIN} so it reliably detects"
                    );
                }
            }
            // And the off-gate baseline (sampled midway between crossings, well clear of any bell)
            // must stay below the exit threshold (80) so RH's two-state detector cleanly drops out
            // between passes and re-arms for the next crossing. Crossings are every 48 ticks; tick
            // `n*48 + 24` is the trough between them.
            for mid in (24..TOTAL_TICKS).step_by(48) {
                let v: i32 = rows[mid][4].parse().unwrap();
                assert!(
                    v < 80,
                    "{name} off-gate baseline at {mid} ({v}) must stay below the exit threshold 80"
                );
            }
        }
    }

    #[test]
    fn visual_sparkline() {
        // Not an assertion gate — run with `--nocapture` to eyeball the shape:
        //   cargo test -p gridfpv-testkit visual_sparkline -- --nocapture
        let clean = node_csv(&NodeCsv {
            ticks_per_lap: 48,
            peak_rssi: 200,
            baseline_rssi: 60,
            seed: 0,
        });
        println!(
            "\nclean (first 144 ticks):\n{}",
            sparkline(&values(&clean)[..144])
        );
        let noisy = plan_csv(&NodePlan {
            laps: (0..3)
                .map(|_| LapSpec::with_peak(48, MARGINAL_PEAK))
                .collect(),
            peak_rssi: MARGINAL_PEAK,
            baseline_rssi: 60,
            noise_amp: BASE_NOISE * 3.0,
            seed: 1,
        });
        println!(
            "noisy/marginal (first 144 ticks):\n{}",
            sparkline(&values(&noisy)[..144])
        );
    }
}
