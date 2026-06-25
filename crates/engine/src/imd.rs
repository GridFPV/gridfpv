//! IMD-aware channel-set scoring + selection (#209 auto-pick).
//!
//! **IMD** (inter­modulation distortion) is the analog-video failure mode that decides which
//! *set* of channels flies cleanly together in a heat. When several VTX transmit at once their
//! signals mix in every receiver's front end and produce **third-order intermodulation
//! products** — new frequencies at `2·f_i − f_j` (two-tone) and `f_i + f_j − f_k` (three-tone).
//! When such a product lands on (or near) a frequency another pilot in the same heat is flying,
//! that pilot sees video breakup. IMD is therefore a property of a heat's **simultaneous**
//! lineup, not of the roster: it only matters for the channels flying *at the same time*.
//!
//! This module is the pure, deterministic core that the heat-build channel assignment uses to
//! pick the cleanest set:
//!
//! - [`imd_score`] scores a candidate channel set — higher is cleaner (the worst third-order
//!   product is *farther* from every used channel).
//! - [`pick_best_imd_set`] chooses the size-`n` subset of the available channels that maximises
//!   that score, with a deterministic tie-break so heat fill stays replay-deterministic.
//!
//! Frequencies are raw **MHz** (`u16`), the timer's `available_channels`. Products are computed
//! in `i32` because `2·f_i − f_j` can exceed `u16::MAX` or go negative.
#![forbid(unsafe_code)]

/// Score a channel set by its **third-order IMD cleanliness** — higher is cleaner (#209).
///
/// Computes every third-order intermodulation product among `freqs`:
///
/// - **two-tone**, for every ordered pair `i ≠ j`: `2·f_i − f_j`;
/// - **three-tone**, for every distinct triple `(i, j, k)`: `f_i + f_j − f_k`.
///
/// The score is the **minimum absolute gap (MHz)** between any such product and any frequency in
/// the set: the closest a spurious product comes to landing on a channel a pilot is actually
/// flying. A large minimum gap means even the worst product is comfortably away from every used
/// channel (clean); a small gap means a product sits on or beside a used channel (video
/// breakup). **Higher = cleaner.**
///
/// Edge cases: a set of fewer than two frequencies produces no two-tone products and a set of
/// fewer than three produces no three-tone products; when there are no products at all the score
/// is [`i32::MAX`] (nothing can interfere). Math is `i32` throughout — a product can exceed
/// `u16::MAX` or go negative — and the gap to each (`u16`) used frequency is taken in `i32`.
pub fn imd_score(freqs: &[u16]) -> i32 {
    let f: Vec<i32> = freqs.iter().map(|&x| i32::from(x)).collect();
    let n = f.len();

    // The smallest absolute gap seen between any product and any used frequency. Starts at the
    // "no interference possible" sentinel and only ever shrinks.
    let mut min_gap = i32::MAX;

    // Smallest |product − used| over every used frequency, folded into `min_gap`.
    let consider = |product: i32, min_gap: &mut i32| {
        for &used in &f {
            let gap = (product - used).abs();
            if gap < *min_gap {
                *min_gap = gap;
            }
        }
    };

    // Two-tone third-order products: 2·f_i − f_j for every ordered pair i ≠ j.
    for i in 0..n {
        for j in 0..n {
            if i != j {
                consider(2 * f[i] - f[j], &mut min_gap);
            }
        }
    }

    // Three-tone third-order products: f_i + f_j − f_k for every distinct triple (i, j, k).
    for i in 0..n {
        for j in 0..n {
            for k in 0..n {
                if i != j && j != k && i != k {
                    consider(f[i] + f[j] - f[k], &mut min_gap);
                }
            }
        }
    }

    min_gap
}

/// The total spread (max − min MHz) of a frequency set — the tie-break preference: wider is
/// better (channels spread across more of the band). Empty/singleton sets spread `0`.
fn spread(freqs: &[u16]) -> i32 {
    match (freqs.iter().min(), freqs.iter().max()) {
        (Some(&lo), Some(&hi)) => i32::from(hi) - i32::from(lo),
        _ => 0,
    }
}

/// Choose the size-`n` subset of `available` with the best (highest) [`imd_score`] (#209 auto-pick).
///
/// Returns the cleanest `n` channels to fly simultaneously, sorted ascending (lowest channel
/// first) so the caller can pair them with a seed-ordered lineup deterministically. When
/// `n == 0` the result is empty; when `n >= available.len()` the whole (de-duplicated) pool is
/// returned (there is no choice to make).
///
/// # Tractability
///
/// FPV channel pools are small — a band is ≤ 8 channels and a timer rarely advertises more than
/// ~12 — so for `available.len() <= 12` this **enumerates every subset** of size `n` and keeps
/// the best, which is exact. For a larger pool the exhaustive count `C(len, n)` can blow up, so
/// it falls back to a **greedy** heuristic: start from the widest-spread seed pair and add, one
/// at a time, the channel that keeps the running set's score highest. The greedy result is not
/// guaranteed optimal but is good and fast; the cap is generous enough that the real fill always
/// takes the exact path.
///
/// # Determinism (replay-safe)
///
/// No clock, no RNG — pure over its inputs. Ties in `imd_score` are broken **deterministically**:
/// prefer the **widest total spread**, then the **lexicographically lowest** sorted channel set.
/// So the same `available` + `n` always yields the same subset, which is what keeps heat fill
/// replay-deterministic.
pub fn pick_best_imd_set(available: &[u16], n: usize) -> Vec<u16> {
    // De-duplicate while preserving the first-seen order (a pool should not offer a channel
    // twice; if it does, a subset never gets the same channel twice).
    let mut pool: Vec<u16> = Vec::new();
    for &ch in available {
        if !pool.contains(&ch) {
            pool.push(ch);
        }
    }

    if n == 0 {
        return Vec::new();
    }
    if n >= pool.len() {
        let mut all = pool;
        all.sort_unstable();
        return all;
    }

    /// Beyond this pool size the exhaustive enumeration is skipped for a greedy heuristic. 12 is
    /// comfortably above the FPV norm (one band ≤ 8 channels; a flexible timer rarely lists more).
    const EXHAUSTIVE_POOL_CAP: usize = 12;

    if pool.len() <= EXHAUSTIVE_POOL_CAP {
        best_subset_exhaustive(&pool, n)
    } else {
        best_subset_greedy(&pool, n)
    }
}

/// Whether candidate set `a` is strictly **better** than the current best `b` under the full
/// ordering: higher [`imd_score`], then wider [`spread`], then lexicographically lower sorted set.
/// Both slices must already be sorted ascending.
fn is_better(a: &[u16], a_score: i32, b: &[u16], b_score: i32) -> bool {
    a_score > b_score
        || (a_score == b_score && (spread(a) > spread(b) || (spread(a) == spread(b) && a < b)))
}

/// Exhaustively enumerate every size-`n` subset of `pool` and return the best under the IMD
/// score + deterministic tie-break. `pool` is de-duplicated; `0 < n < pool.len()`.
fn best_subset_exhaustive(pool: &[u16], n: usize) -> Vec<u16> {
    let mut best: Option<(Vec<u16>, i32)> = None;
    let mut indices: Vec<usize> = (0..n).collect();
    let len = pool.len();

    loop {
        // The current subset, sorted ascending (pool order is arbitrary; indices ascend so the
        // gathered set is already sorted by pool order — we sort defensively for the tie-break).
        let mut subset: Vec<u16> = indices.iter().map(|&i| pool[i]).collect();
        subset.sort_unstable();
        let score = imd_score(&subset);

        let take = match &best {
            None => true,
            Some((b, b_score)) => is_better(&subset, score, b, *b_score),
        };
        if take {
            best = Some((subset, score));
        }

        // Advance the combination indices (standard lexicographic next-combination).
        let mut i = n;
        loop {
            if i == 0 {
                // Exhausted every combination.
                return best.map(|(s, _)| s).unwrap_or_default();
            }
            i -= 1;
            if indices[i] != i + len - n {
                indices[i] += 1;
                for j in (i + 1)..n {
                    indices[j] = indices[j - 1] + 1;
                }
                break;
            }
        }
    }
}

/// A greedy fallback for a pool larger than the exhaustive cap: seed with the widest-spread pair,
/// then add the channel that keeps the running [`imd_score`] highest (tie-break: widest spread,
/// then lowest channel). Not guaranteed optimal — only used for pools too large to enumerate.
fn best_subset_greedy(pool: &[u16], n: usize) -> Vec<u16> {
    let mut sorted = pool.to_vec();
    sorted.sort_unstable();

    if n == 1 {
        // A single channel has no products and so no IMD distinction; the lowest is the
        // deterministic pick.
        return vec![sorted[0]];
    }

    // Seed with the widest-spread pair: the lowest and highest channels.
    let mut chosen: Vec<u16> = vec![sorted[0], sorted[sorted.len() - 1]];

    while chosen.len() < n {
        let mut best: Option<(u16, i32)> = None;
        for &cand in &sorted {
            if chosen.contains(&cand) {
                continue;
            }
            let mut trial = chosen.clone();
            trial.push(cand);
            trial.sort_unstable();
            let score = imd_score(&trial);

            // Compare the candidate addition by the resulting set's score, then spread, then by
            // preferring the lower candidate channel.
            let take = match best {
                None => true,
                Some((best_cand, best_score)) => {
                    score > best_score || (score == best_score && cand < best_cand)
                }
            };
            if take {
                best = Some((cand, score));
            }
        }
        match best {
            Some((cand, _)) => chosen.push(cand),
            None => break,
        }
    }

    chosen.sort_unstable();
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard 5.8 GHz Raceband R1–R8, in channel order — an evenly-spaced (37 MHz step)
    /// set, the IMD-clean reference.
    const RACEBAND: [u16; 8] = [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917];

    #[test]
    fn evenly_spaced_set_scores_above_a_tightly_packed_set() {
        // A worked clean-vs-tight comparison (#209). Take three Raceband channels spread across
        // the band vs three tightly-packed adjacent channels.
        //
        // CLEAN — R1, R4, R8 = [5658, 5769, 5917]:
        //   two-tone 2·f_i − f_j and three-tone f_i+f_j−f_k land far from every used channel.
        //   e.g. 2·5769 − 5658 = 5880 (gap 37 to 5917/none-used), 5658+5917−5769 = 5806, etc.
        // TIGHT — adjacent 5658, 5695, 5732 (37 MHz apart, consecutive):
        //   2·5695 − 5658 = 5732 — *exactly* a used channel → gap 0. Catastrophic IMD.
        let clean = [5658u16, 5769, 5917];
        let tight = [5658u16, 5695, 5732];

        let clean_score = imd_score(&clean);
        let tight_score = imd_score(&tight);

        // The tight, consecutive set has a product landing exactly on a used channel.
        assert_eq!(
            tight_score, 0,
            "tight adjacent set: a product hits a channel"
        );
        // The spread set keeps every product well away from every channel.
        assert!(
            clean_score > tight_score,
            "evenly-spaced {clean:?} (score {clean_score}) must beat tight {tight:?} (score {tight_score})"
        );
        assert!(clean_score > 0, "clean set has no product on a channel");
    }

    #[test]
    fn raceband_full_set_score_is_zero_consecutive() {
        // The full evenly-spaced Raceband is itself NOT IMD-clean as a simultaneous 8-set: with a
        // constant 37 MHz step, 2·R2 − R1 = R3 exactly. This is why real 8-pilot events use a
        // hand-tuned IMD set, and why picking a clean *subset* matters.
        assert_eq!(imd_score(&RACEBAND), 0);
    }

    #[test]
    fn fewer_than_two_frequencies_have_no_products() {
        // No pairs/triples ⇒ no products ⇒ the "no interference" sentinel.
        assert_eq!(imd_score(&[]), i32::MAX);
        assert_eq!(imd_score(&[5800]), i32::MAX);
    }

    #[test]
    fn score_is_symmetric_and_uses_i32_math() {
        // A product can go below u16 range (negative) without panicking: 2·5658 − 5917 = 5399,
        // still positive here, but the gap math is i32 so a low product never wraps. Two widely
        // separated channels: 2·5658 − 5917 = 5399 (gap 259 to 5658); 2·5917 − 5658 = 6176 (gap
        // 259 to 5917). Min gap 259.
        let score = imd_score(&[5658, 5917]);
        assert_eq!(score, 259);
        // Order does not matter.
        assert_eq!(imd_score(&[5917, 5658]), score);
    }

    #[test]
    fn pick_returns_the_max_scoring_subset_of_the_requested_size() {
        // From the full Raceband pool, the best 3-channel subset must out-score the naive
        // first-fit R1,R2,R3 (which scores 0 — a product lands on R3).
        let picked = pick_best_imd_set(&RACEBAND, 3);
        assert_eq!(picked.len(), 3, "exactly the requested size");

        let first_fit = [5658u16, 5695, 5732];
        assert!(
            imd_score(&picked) > imd_score(&first_fit),
            "picked {picked:?} (score {}) must beat first-fit {first_fit:?} (score {})",
            imd_score(&picked),
            imd_score(&first_fit),
        );
        // It is a genuine subset of the pool.
        for ch in &picked {
            assert!(RACEBAND.contains(ch), "{ch} is from the pool");
        }
    }

    #[test]
    fn pick_never_exceeds_available() {
        // n larger than the pool returns the whole (sorted) pool, never invents channels.
        let pool = [5658u16, 5917, 5732];
        let picked = pick_best_imd_set(&pool, 10);
        assert_eq!(picked, vec![5658, 5732, 5917]);
    }

    #[test]
    fn pick_zero_is_empty() {
        assert!(pick_best_imd_set(&RACEBAND, 0).is_empty());
    }

    #[test]
    fn pick_is_deterministic_and_sorted() {
        // Same inputs ⇒ same output, every time, sorted ascending.
        let a = pick_best_imd_set(&RACEBAND, 4);
        let b = pick_best_imd_set(&RACEBAND, 4);
        assert_eq!(a, b, "deterministic");
        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(a, sorted, "result is sorted ascending");
    }

    #[test]
    fn pick_dedupes_a_repeated_channel() {
        // A pool listing a channel twice never yields it twice in the subset.
        let pool = [5658u16, 5658, 5769, 5917, 5806];
        let picked = pick_best_imd_set(&pool, 3);
        let mut deduped = picked.clone();
        deduped.sort_unstable();
        deduped.dedup();
        assert_eq!(picked.len(), deduped.len(), "no duplicate channel");
    }

    #[test]
    fn tie_break_prefers_wider_spread_then_lower_channels() {
        // Construct a pool where two size-2 subsets score identically on IMD (every 2-channel set
        // has the same single min-gap pattern only when the gaps match). Use a symmetric pool so
        // the picker must fall to the spread tie-break: pick the widest pair.
        // Pool of 4 evenly spaced channels; the best 2-subset by spread is the outermost pair.
        let pool = [5700u16, 5750, 5800, 5850];
        let picked = pick_best_imd_set(&pool, 2);
        // The two widest-apart channels maximise spread; with equal IMD that is the tie-break.
        assert_eq!(picked, vec![5700, 5850]);
    }

    #[test]
    fn greedy_path_runs_for_a_large_pool() {
        // A pool above the exhaustive cap (13 channels) takes the greedy path and still returns a
        // valid, sorted, in-pool subset of the requested size.
        let pool: Vec<u16> = (0..13).map(|i| 5600 + i * 30).collect();
        let picked = pick_best_imd_set(&pool, 5);
        assert_eq!(picked.len(), 5);
        let mut sorted = picked.clone();
        sorted.sort_unstable();
        assert_eq!(picked, sorted, "sorted");
        for ch in &picked {
            assert!(pool.contains(ch), "in pool");
        }
    }
}
