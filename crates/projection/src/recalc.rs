//! **Threshold recalculate** (RH plugin design D16, Slice 4 / marshaling #3) — re-run crossing
//! detection over a captured dense RSSI trace at marshal-chosen enter/exit levels, and *propose* the
//! resulting laps.
//!
//! RotorHazard exposes no "re-detect at new thresholds" call — the enter/exit hysteresis lives in
//! the node firmware, not the server. So the draggable-threshold recalculate is a **computation over
//! the stored trace** ([`SignalHistory`](gridfpv_events::SignalHistory) — the dense `times`/`rssi`
//! the plugin captured): replay the crossing rule at the marshal's levels and return the crossings.
//! Marshal commits the proposal; RH's truth is never mutated under the RD (Marshaling commit model).
//!
//! ## Fidelity (load-bearing — see the doc's risk #3)
//!
//! This replays the **enter→peak→exit** model: a crossing OPENS when RSSI rises to/above `enter`,
//! the pass time is the RSSI **peak** within the crossing, and the crossing CLOSES when RSSI falls
//! to/below `exit` (hysteresis: `enter > exit`). That matches RH's crossing shape, but the *exact*
//! pass-time RH's firmware reports (peak vs. a centroid/quad-interpolated instant) must be validated
//! against real RH on a captured trace before this drives committed marshaling — it is **not yet
//! wired** into the marshaling UI for that reason.

/// One proposed lap/pass from a recalculation: the crossing's peak instant + peak RSSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposedPass {
    /// Pass time — the RSSI peak within the crossing, in the trace's own time unit (race-relative
    /// microseconds, as carried by [`SignalHistory`](gridfpv_events::SignalHistory)).
    pub at_micros: i64,
    /// The peak RSSI reached during the crossing.
    pub peak_rssi: u16,
}

/// Re-detect crossings over a dense trace at `(enter, exit)` thresholds, returning the proposed
/// passes in time order.
///
/// `times`/`rssi` are the parallel dense-trace arrays (the common prefix is used if they differ in
/// length). A crossing opens at the first sample `>= enter`, tracks the peak, and closes at the first
/// subsequent sample `<= exit`, emitting a pass at the peak. A crossing still open when the trace
/// ends is **not** emitted (no close = no recorded pass — matches RH recording on crossing close).
/// `enter <= exit` is a degenerate calibration (no hysteresis band); the same rule still runs.
pub fn redetect_passes(times: &[i64], rssi: &[u16], enter: u16, exit: u16) -> Vec<ProposedPass> {
    let n = times.len().min(rssi.len());
    let mut out = Vec::new();
    let mut in_crossing = false;
    let mut peak = 0u16;
    let mut peak_t = 0i64;
    for i in 0..n {
        let v = rssi[i];
        let t = times[i];
        if !in_crossing {
            if v >= enter {
                in_crossing = true;
                peak = v;
                peak_t = t;
            }
        } else {
            if v > peak {
                peak = v;
                peak_t = t;
            }
            if v <= exit {
                out.push(ProposedPass {
                    at_micros: peak_t,
                    peak_rssi: peak,
                });
                in_crossing = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trace of `count` bells (baseline→peak→baseline) spaced `gap` µs, at the given peak. Returns
    /// (times, rssi). Each bell is 5 samples: baseline, mid, PEAK, mid, baseline.
    fn bells(count: usize, gap: i64, peak: u16, baseline: u16) -> (Vec<i64>, Vec<u16>) {
        let mid = (peak + baseline) / 2;
        let mut t = Vec::new();
        let mut r = Vec::new();
        let mut clock = 0i64;
        for _ in 0..count {
            for &v in &[baseline, mid, peak, mid, baseline] {
                t.push(clock);
                r.push(v);
                clock += gap;
            }
            clock += gap * 3; // dead air between bells
        }
        (t, r)
    }

    #[test]
    fn detects_one_pass_per_bell_at_the_peak() {
        let (t, r) = bells(3, 100_000, 150, 70);
        let passes = redetect_passes(&t, &r, 90, 80);
        assert_eq!(passes.len(), 3, "three bells -> three passes");
        // Each pass lands at its bell's peak (the 3rd sample of each 5-sample bell + dead air).
        assert!(passes.iter().all(|p| p.peak_rssi == 150));
    }

    #[test]
    fn raising_enter_above_every_peak_yields_no_passes() {
        let (t, r) = bells(3, 100_000, 150, 70);
        assert!(redetect_passes(&t, &r, 200, 80).is_empty());
    }

    #[test]
    fn a_higher_enter_rejects_a_low_false_bump() {
        // Two real bells (peak 150) and one low false bump (peak 100). At enter 120 the false bump
        // never crosses, so the recalc proposes only the two real passes — the marshaling win.
        let (mut t, mut r) = bells(2, 100_000, 150, 70);
        let (ft, fr) = bells(1, 100_000, 100, 70);
        let base = t.last().copied().unwrap_or(0) + 300_000;
        t.extend(ft.iter().map(|x| x + base));
        r.extend(fr);
        assert_eq!(
            redetect_passes(&t, &r, 90, 80).len(),
            3,
            "low enter catches all three"
        );
        assert_eq!(
            redetect_passes(&t, &r, 120, 80).len(),
            2,
            "higher enter drops the false bump"
        );
    }

    #[test]
    fn an_open_crossing_at_trace_end_is_not_a_pass() {
        // Rises into a crossing but never falls back below exit before the trace ends.
        let t = vec![0, 100_000, 200_000];
        let r = vec![70, 150, 150];
        assert!(redetect_passes(&t, &r, 90, 80).is_empty());
    }

    #[test]
    fn mismatched_lengths_use_the_common_prefix() {
        let t = vec![0, 100_000];
        let r = vec![70, 150, 70, 60];
        // Only the first two samples are considered (no close) -> no pass; just must not panic.
        assert!(redetect_passes(&t, &r, 90, 80).is_empty());
    }
}
