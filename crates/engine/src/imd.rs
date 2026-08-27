//! IMD-aware channel-set rating + selection (#209 auto-pick, #430 IMDTabler port).
//!
//! **IMD** (inter­modulation distortion) is the analog-video failure mode that decides which
//! *set* of channels flies cleanly together in a heat. When several VTX transmit at once their
//! signals mix in every receiver's front end and produce **third-order intermodulation
//! products** — new frequencies at `2·f_i − f_j`. When such a product lands on (or near) a
//! frequency another pilot in the same heat is flying, that pilot sees video breakup. IMD is
//! therefore a property of a heat's **simultaneous** lineup, not of the roster: it only matters
//! for the channels flying *at the same time*.
//!
//! # This is a port of IMDTabler, deliberately
//!
//! [`imd_rating`] is a faithful port of **IMDTabler** (`ethomas997/IMDTabler`), the hobby's
//! standard IMD calculator — the one MultiGP's published channel sets are rated with, and the one
//! **RotorHazard itself shells out to** for its own IMD readout. That is the whole point: an RD
//! who types a channel set into RotorHazard, or into etheli.com, must read back *the same
//! number* we show them. A different number for the same channels is worse than no number.
//!
//! Three properties of the rating are load-bearing, and none of them is an accident:
//!
//! 1. **Two-tone products only** (`2·f_i − f_j`). Three-tone products (`f_i + f_j − f_k`) are
//!    physically real and about 6 dB stronger at equal drive, but on a course the nearest pilot
//!    dominates by tens of dB, so the near-field two-tone term is what actually breaks video —
//!    and triple-beat-free sets barely exist inside 5.8 GHz at racing separations. Empirically
//!    decisive: two-tone-only tracks IMDTabler monotonically across every canonical set, and
//!    including the three-tone term does not (the measurements are in #430).
//! 2. **Accumulate, do not minimise.** Every product that falls within
//!    [`RATING_DIFF_LIMIT_MHZ`] of a used channel contributes `(35 − gap)²`. Many near-misses
//!    hurt more than one — a failure a single worst-case minimum cannot express. Squaring makes
//!    small gaps hurt disproportionately.
//! 3. **Products outside 5100–6099 MHz are ignored.** A mixing product far outside the band is
//!    not in any 5.8 GHz receiver's passband, so it cannot break anybody's video.
//!
//! # Why the old "minimum gap" score was replaced (#430)
//!
//! The previous `imd_score` returned the minimum gap between *any* third-order product
//! (two-tone **and** three-tone) and any used channel. Because `f_i + f_j − f_k` lands *exactly*
//! on a used channel whenever the set contains two pairs with the same sum, and a balanced,
//! well-designed set is exactly the kind of set that has two pairs with the same sum, it scored
//! **0** for MultiGP's official 6-pilot set, for RotorHazard's default IMD6C profile, for the
//! perfect-100 Racebnd4 set, *and* for all eight of Raceband (the worst set in FPV) alike. It
//! punished good sets for being symmetric, and [`pick_best_imd_set`] maximised it.
//!
//! # What this module provides
//!
//! - [`imd_rating`] rates a candidate channel set — **higher is cleaner**, `100` is the ceiling,
//!   and a genuinely bad set goes **negative** (all of Raceband rates −635).
//! - [`min_channel_separation`] is the raw adjacent-channel spacing of a set, the other half of
//!   "can this fly": a good IMD rating does not rescue two channels 15 MHz apart, which bleed
//!   into each other directly.
//! - [`pick_best_imd_set`] chooses the size-`n` subset of the available channels that maximises
//!   the rating **subject to** the separation floor, with a deterministic tie-break so heat fill
//!   stays replay-deterministic.
//!
//! Frequencies are raw **MHz** (`u16`), the timer's `available_channels`. Products are computed
//! in `i32` because `2·f_i − f_j` can exceed `u16::MAX` or go negative.
//!
//! **No thresholds here.** Deciding what rating counts as "clean" for an RD is presentation
//! (#117 S4) and is deliberately not this module's business: the achievable ceiling collapses
//! with pilot count (4 pilots → 100, 6 pilots → 67, 8 pilots → −203 from the full catalog), so
//! any flat clean/marginal/poor band would tell every RD running a big heat that their spectrum
//! is dirty. This module produces the number; something else decides what to say about it.
#![forbid(unsafe_code)]

/// The top of the rating scale — a set with no product within [`RATING_DIFF_LIMIT_MHZ`] of any
/// used channel (`RATING_MAX_VALUE` in `IMDTabler.java`).
pub const RATING_MAX: i32 = 100;

/// The gap (MHz) at or above which a third-order product is considered harmless
/// (`RATING_DIFF_LIMIT` in `IMDTabler.java`). Roughly one analog channel width plus guard: a
/// product 5 MHz out is inside the passband, 10–15 MHz is in the skirt of a cheap VRX.
pub const RATING_DIFF_LIMIT_MHZ: i32 = 35;

/// Lowest product frequency (MHz) that can matter — below this it is outside any 5.8 GHz
/// receiver's passband (`MIN_DISP_FREQ` in `IMDTabler.java`).
const MIN_PRODUCT_MHZ: i32 = 5100;

/// Highest product frequency (MHz) that can matter (`MAX_DISP_FREQ` in `IMDTabler.java`).
const MAX_PRODUCT_MHZ: i32 = 6099;

/// The minimum spacing (MHz) between two channels flown at once that [`pick_best_imd_set`] will
/// accept — IMDTabler's own `MIN_FREQ_SEP`, the line below which it prints a separation warning.
///
/// This is a *separate* failure mode from IMD and the rating cannot see it: two channels 15 MHz
/// apart bleed into each other's passband directly, and no IMD rating rescues that. The picker
/// used to have no such floor at all, and its best 6-set from the full catalog put two channels
/// **15 MHz apart** (#430).
///
/// **Why 35 and not 40.** 40 MHz is tempting — cleaner guard, and the best 6-set from the full
/// catalog satisfies it anyway. But Raceband's own native step is **37 MHz**, so a 40 MHz floor
/// declares every adjacent Raceband pair unflyable, and a Raceband-only timer (the FPV default)
/// could not seat 5 pilots without falling through to the unconstrained fallback. It also costs
/// real quality: the best 5-set from the full catalog at a 35 MHz floor rates **100** and has a
/// 37 MHz minimum spacing; forcing 40 drops the best available 5-set to **98**. 35 is the
/// standard's own line, and holding to the standard's line is the point of this module.
pub const MIN_CHANNEL_SEPARATION_MHZ: u16 = 35;

/// Rate a channel set by its **third-order IMD cleanliness**, as IMDTabler rates it (#430).
///
/// **Higher is cleaner. [`RATING_MAX`] (100) is the ceiling; a bad set goes negative.**
///
/// For every ordered pair of distinct channels `(i, j)` in `freqs` this computes the two-tone
/// third-order product `2·f_i − f_j`, finds the channel in the set *nearest* that product, and —
/// when the product lies inside the receivable band `5100..=6099` MHz and that gap is under
/// [`RATING_DIFF_LIMIT_MHZ`] — charges `(35 − gap)²` against the set. The squared term is what
/// makes a product landing *on* a used channel (gap 0, cost 1225) cost far more than one grazing
/// it (gap 34, cost 1). The accumulated cost is then scaled down and subtracted from 100:
///
/// ```text
/// rating = 100 − total / 5 / freqs.len()
/// ```
///
/// with truncating integer division at each step, exactly as `IMDTabler.java` does it — the
/// division order is preserved so our integer matches the one an RD reads off RotorHazard or
/// etheli.com for the same channels.
///
/// # Edge cases
///
/// A set of fewer than two frequencies produces no products and rates [`RATING_MAX`] — nothing
/// can interfere with nothing. (IMDTabler divides by the set length and would fault on an empty
/// set; returning the ceiling is the only sensible reading.) The rating does not depend on the
/// order of `freqs`. Math is `i32` throughout: a product can exceed `u16::MAX` or go negative.
///
/// # Validation
///
/// The port reproduces IMDTabler's published ratings exactly — Racebnd4 = 100, IMD6C = 29,
/// MultiGP's ETBest6 = 67, all of Raceband = −635. See `canonical_sets_rate_as_imdtabler_does`.
pub fn imd_rating(freqs: &[u16]) -> i32 {
    let n = freqs.len();
    if n < 2 {
        // No pairs ⇒ no products ⇒ nothing to interfere with anything.
        return RATING_MAX;
    }
    let f: Vec<i32> = freqs.iter().map(|&x| i32::from(x)).collect();

    // The accumulated penalty across every two-tone product (`trTtotal` in IMDTabler).
    let mut total: i32 = 0;

    for (i, &fi) in f.iter().enumerate() {
        for (j, &fj) in f.iter().enumerate() {
            if i == j {
                continue;
            }
            // The two-tone third-order product. Only products inside the receivable band can
            // land in anybody's front end; IMDTabler skips the rest outright.
            let product = 2 * fi - fj;
            if !(MIN_PRODUCT_MHZ..=MAX_PRODUCT_MHZ).contains(&product) {
                continue;
            }
            // How close the product comes to *any* channel in the set — including f_i and f_j
            // themselves, which is deliberate: a product landing back on one of its own parents
            // still breaks that pilot's video.
            let gap = f
                .iter()
                .map(|&used| (product - used).abs())
                .min()
                .expect("the set is non-empty");
            if gap < RATING_DIFF_LIMIT_MHZ {
                // Squared, so low gaps hurt disproportionately.
                let val = RATING_DIFF_LIMIT_MHZ - gap;
                total += val * val;
            }
        }
    }

    // Scale down by the frequency count "and a bit more", then subtract from the max. The two
    // successive truncating divisions are IMDTabler's, kept in its order.
    RATING_MAX - total / 5 / (n as i32)
}

/// The smallest spacing (MHz) between any two channels in the set — the adjacent-channel-bleed
/// check that the IMD rating cannot see.
///
/// `None` for a set of fewer than two channels (there is no pair to space). Order-independent:
/// the set is sorted internally.
pub fn min_channel_separation(freqs: &[u16]) -> Option<u16> {
    if freqs.len() < 2 {
        return None;
    }
    let mut sorted = freqs.to_vec();
    sorted.sort_unstable();
    sorted.windows(2).map(|w| w[1] - w[0]).min()
}

/// Whether every pair in the set is at least `floor` MHz apart. A set of fewer than two channels
/// is trivially separated.
fn meets_separation(freqs: &[u16], floor: u16) -> bool {
    min_channel_separation(freqs).is_none_or(|sep| sep >= floor)
}

/// The total spread (max − min MHz) of a frequency set — the tie-break preference: wider is
/// better (channels spread across more of the band). Empty/singleton sets spread `0`.
fn spread(freqs: &[u16]) -> i32 {
    match (freqs.iter().min(), freqs.iter().max()) {
        (Some(&lo), Some(&hi)) => i32::from(hi) - i32::from(lo),
        _ => 0,
    }
}

/// Choose the size-`n` subset of `available` that flies cleanest together (#209 auto-pick, #430).
///
/// Returns the `n` channels with the best [`imd_rating`] **among those whose channels are all at
/// least [`MIN_CHANNEL_SEPARATION_MHZ`] apart**, sorted ascending (lowest channel first) so the
/// caller can pair them with a seed-ordered lineup deterministically. When `n == 0` the result is
/// empty; when `n >= available.len()` the whole (de-duplicated) pool is returned — there is no
/// choice to make, and refusing would be worse than handing back a set the RD explicitly
/// configured.
///
/// # The separation floor
///
/// Rating alone is not enough. Adjacent-channel bleed is a different failure mode with a
/// different cause, and before #430 nothing stopped the picker choosing a set with two channels
/// 15 MHz apart because they happened to rate well. Sets under the floor are considered only as a
/// **last resort**: if the pool admits no size-`n` subset that clears
/// [`MIN_CHANNEL_SEPARATION_MHZ`], the search reruns with no floor rather than returning fewer
/// channels than asked for (a short set would surface to the RD as "too few channels", which is
/// a lie — the channels exist, they are just tight).
///
/// # Tractability
///
/// The separation floor is also what makes the search cheap: the candidates are enumerated
/// depth-first over the *sorted* pool, and a branch is abandoned the moment the next channel is
/// closer than the floor. For the full 39-channel catalog that leaves ~34k feasible 6-subsets out
/// of C(39,6) ≈ 3.3M — an exact answer for a fraction of the work. A pool pathological enough to
/// blow [`SEARCH_BUDGET`] anyway (which needs the unconstrained rerun on a large, very tight
/// pool) falls back to a greedy walk: seed with the widest-spread pair and add, one at a time,
/// the channel that keeps the running set best. Greedy is not guaranteed optimal, but the budget
/// is generous enough that a real channel pool never reaches it.
///
/// # Determinism (replay-safe)
///
/// No clock, no RNG — pure over its inputs. Ties are broken **deterministically** and in the same
/// order as before #430: prefer the **highest rating**, then the **widest total spread**, then
/// the **lexicographically lowest** sorted channel set. So the same `available` + `n` always
/// yields the same subset, which is what keeps heat fill replay-deterministic.
pub fn pick_best_imd_set(available: &[u16], n: usize) -> Vec<u16> {
    // De-duplicate (a pool should not offer a channel twice; if it does, a subset never gets the
    // same channel twice) and sort — the depth-first search needs ascending order to prune on
    // separation, and the result is returned sorted anyway.
    let mut pool: Vec<u16> = Vec::new();
    for &ch in available {
        if !pool.contains(&ch) {
            pool.push(ch);
        }
    }
    pool.sort_unstable();

    if n == 0 {
        return Vec::new();
    }
    if n >= pool.len() {
        return pool;
    }

    // First choice: the best set that clears the separation floor.
    match best_subset(&pool, n, MIN_CHANNEL_SEPARATION_MHZ) {
        Search::Best(best) => return best,
        // No subset of this pool can clear the floor (a tight pool — every candidate has an
        // adjacent-bleed pair). Rate them all and take the least bad rather than refuse.
        Search::NoneFeasible => {}
        // Too many candidates to enumerate exactly; the greedy walk below is the fallback.
        Search::OverBudget => return best_subset_greedy(&pool, n),
    }

    match best_subset(&pool, n, 0) {
        Search::Best(best) => best,
        // Unreachable in practice (with no floor every size-`n` subset is a candidate, and
        // `n < pool.len()` guarantees at least one), but greedy always produces *something*.
        Search::NoneFeasible | Search::OverBudget => best_subset_greedy(&pool, n),
    }
}

/// How many nodes the depth-first search may visit before giving up and handing over to the
/// greedy fallback. The separation floor keeps a real pool far under this: the full 39-channel
/// catalog tops out around 35k feasible subsets for any heat size, so the budget carries roughly
/// 5× headroom over the worst case that can actually occur.
const SEARCH_BUDGET: u64 = 300_000;

/// The outcome of a bounded exact search.
enum Search {
    /// The search completed and this is the best subset under the full ordering.
    Best(Vec<u16>),
    /// The search completed but no subset met the separation floor.
    NoneFeasible,
    /// The search would have exceeded [`SEARCH_BUDGET`]; the caller falls back to greedy.
    OverBudget,
}

/// The running state of one depth-first search over the sorted pool.
struct SearchState<'a> {
    /// The de-duplicated pool, sorted ascending.
    pool: &'a [u16],
    /// The subset size being sought.
    n: usize,
    /// The minimum spacing (MHz) two chosen channels may have; `0` disables the constraint.
    floor: u16,
    /// Nodes left to visit before the search abandons exactness.
    budget: u64,
    /// The best complete subset seen so far, with its rating.
    best: Option<(Vec<u16>, i32)>,
}

/// Enumerate every size-`n` subset of the sorted `pool` whose channels are at least `floor` MHz
/// apart and return the best under the rating + deterministic tie-break, or why it could not.
///
/// `pool` must be de-duplicated and sorted ascending; `0 < n < pool.len()`.
fn best_subset(pool: &[u16], n: usize, floor: u16) -> Search {
    let mut state = SearchState {
        pool,
        n,
        floor,
        budget: SEARCH_BUDGET,
        best: None,
    };
    let mut chosen: Vec<u16> = Vec::with_capacity(n);
    if !search(&mut state, 0, &mut chosen) {
        return Search::OverBudget;
    }
    match state.best {
        Some((best, _)) => Search::Best(best),
        None => Search::NoneFeasible,
    }
}

/// One node of the depth-first search: extend `chosen` with channels from `pool[start..]` that
/// keep it `floor`-separated, folding every complete subset into `state.best`.
///
/// Returns `false` once the budget is exhausted, which unwinds the whole search.
fn search(state: &mut SearchState<'_>, start: usize, chosen: &mut Vec<u16>) -> bool {
    if state.budget == 0 {
        return false;
    }
    state.budget -= 1;

    if chosen.len() == state.n {
        // A complete candidate. `chosen` is built in ascending pool order, so it is already
        // sorted — which both the rating (order-independent) and the lexicographic tie-break
        // rely on.
        let rating = imd_rating(chosen);
        let take = match &state.best {
            None => true,
            Some((best, best_rating)) => is_better(chosen, rating, best, *best_rating),
        };
        if take {
            state.best = Some((chosen.clone(), rating));
        }
        return true;
    }

    // Not enough channels left in the pool to finish this subset.
    let remaining = state.n - chosen.len();
    if state.pool.len() - start < remaining {
        return true;
    }

    for i in start..state.pool.len() {
        let cand = state.pool[i];
        // The pool is sorted, so the previous pick is the nearest channel below `cand`: if the
        // gap to it is under the floor this candidate is out, and — since every later candidate
        // is further away, not closer — skipping only this one is right (not the whole branch).
        if let Some(&prev) = chosen.last() {
            if state.floor > 0 && cand - prev < state.floor {
                continue;
            }
        }
        chosen.push(cand);
        let ok = search(state, i + 1, chosen);
        chosen.pop();
        if !ok {
            return false;
        }
    }
    true
}

/// Whether candidate set `a` is strictly **better** than the current best `b` under the full
/// ordering: higher [`imd_rating`], then wider [`spread`], then lexicographically lower sorted
/// set. Both slices must already be sorted ascending.
fn is_better(a: &[u16], a_rating: i32, b: &[u16], b_rating: i32) -> bool {
    a_rating > b_rating
        || (a_rating == b_rating && (spread(a) > spread(b) || (spread(a) == spread(b) && a < b)))
}

/// A greedy fallback for a pool too large to enumerate exactly: seed with the widest-spread pair,
/// then add the channel that leaves the running set best — preferring, at every step, an addition
/// that keeps the set clear of [`MIN_CHANNEL_SEPARATION_MHZ`], then the best [`imd_rating`], then
/// the widest spread, then the lowest channel.
///
/// Not guaranteed optimal — only reached when the exact search blows [`SEARCH_BUDGET`].
fn best_subset_greedy(pool: &[u16], n: usize) -> Vec<u16> {
    // `pool` arrives sorted and de-duplicated from `pick_best_imd_set`.
    if n == 1 {
        // A single channel has no products and so no IMD distinction; the lowest is the
        // deterministic pick.
        return vec![pool[0]];
    }

    // Seed with the widest-spread pair: the lowest and highest channels.
    let mut chosen: Vec<u16> = vec![pool[0], pool[pool.len() - 1]];

    while chosen.len() < n {
        let mut best: Option<(u16, Vec<u16>, i32)> = None;
        for &cand in pool {
            if chosen.contains(&cand) {
                continue;
            }
            let mut trial = chosen.clone();
            trial.push(cand);
            trial.sort_unstable();
            let rating = imd_rating(&trial);

            // Separation first (an adjacent-bleed pair is disqualifying, not a rating penalty),
            // then the resulting set's rating, spread and lexicographic order — which, since two
            // distinct candidates always yield two distinct sorted sets, is a total order.
            let take = match &best {
                None => true,
                Some((_, best_trial, best_rating)) => {
                    let cand_ok = meets_separation(&trial, MIN_CHANNEL_SEPARATION_MHZ);
                    let best_ok = meets_separation(best_trial, MIN_CHANNEL_SEPARATION_MHZ);
                    match (cand_ok, best_ok) {
                        (true, false) => true,
                        (false, true) => false,
                        _ => is_better(&trial, rating, best_trial, *best_rating),
                    }
                }
            };
            if take {
                best = Some((cand, trial, rating));
            }
        }
        match best {
            Some((cand, _, _)) => chosen.push(cand),
            None => break,
        }
        chosen.sort_unstable();
    }

    chosen.sort_unstable();
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard 5.8 GHz Raceband R1–R8, in channel order (a constant 37 MHz step).
    const RACEBAND: [u16; 8] = [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917];

    /// The full standard FPV channel catalog — Raceband, Fatshark/IRC, Boscam A/B/E — flattened
    /// and de-duplicated (5880 is both Raceband R7 and Fatshark F8), 39 distinct channels. This
    /// is the real pool an RD who enables everything hands the picker.
    fn full_catalog() -> Vec<u16> {
        let bands: [[u16; 8]; 5] = [
            RACEBAND,
            [5740, 5760, 5780, 5800, 5820, 5840, 5860, 5880],
            [5865, 5845, 5825, 5805, 5785, 5765, 5745, 5725],
            [5733, 5752, 5771, 5790, 5809, 5828, 5847, 5866],
            [5705, 5685, 5665, 5645, 5885, 5905, 5925, 5945],
        ];
        let mut out: Vec<u16> = Vec::new();
        for band in bands {
            for ch in band {
                if !out.contains(&ch) {
                    out.push(ch);
                }
            }
        }
        out
    }

    #[test]
    fn canonical_sets_rate_as_imdtabler_does() {
        // THE validation table for the #430 port. Every expected value here is IMDTabler's own
        // published rating for the set — reproduce these integers or the port is wrong. Do not
        // adjust an expectation to match the code.
        //
        // These sets are the hobby's reference points: Racebnd4 is the canonical perfect
        // 4-pilot set, ETBest6 is MultiGP's official 6-pilot set, IMD6C is RotorHazard's default
        // 6-channel profile, and all eight of Raceband is the worst set in common use.
        let cases: [(&str, &[u16], i32); 5] = [
            ("Racebnd4", &[5658, 5732, 5843, 5917], 100),
            ("ET6minus1", &[5645, 5685, 5760, 5905, 5945], 98),
            (
                "ETBest6 (MultiGP)",
                &[5645, 5685, 5760, 5805, 5905, 5945],
                67,
            ),
            (
                "IMD6C (RotorHazard)",
                &[5658, 5695, 5760, 5800, 5880, 5917],
                29,
            ),
            ("Raceband R1–R8", &RACEBAND, -635),
        ];
        for (name, freqs, expected) in cases {
            assert_eq!(
                imd_rating(freqs),
                expected,
                "{name} {freqs:?} must rate exactly as IMDTabler rates it"
            );
        }
    }

    #[test]
    fn the_rating_orders_the_canonical_sets_the_way_the_hobby_does() {
        // The property the old min-gap score could not deliver: these five sets are *ordered*,
        // best to worst, and the metric must agree. Before #430 the first four all scored 0 and
        // the fifth scored 0 too — MultiGP's official set, RotorHazard's default, a perfect-100
        // set and all-eight-Raceband in one indistinguishable bucket.
        let racebnd4 = imd_rating(&[5658, 5732, 5843, 5917]);
        let et6minus1 = imd_rating(&[5645, 5685, 5760, 5905, 5945]);
        let etbest6 = imd_rating(&[5645, 5685, 5760, 5805, 5905, 5945]);
        let imd6c = imd_rating(&[5658, 5695, 5760, 5800, 5880, 5917]);
        let raceband8 = imd_rating(&RACEBAND);

        assert!(
            racebnd4 > et6minus1 && et6minus1 > etbest6 && etbest6 > imd6c && imd6c > raceband8,
            "canonical order broken: {racebnd4} > {et6minus1} > {etbest6} > {imd6c} > {raceband8}"
        );
    }

    #[test]
    fn a_clean_spread_set_rates_at_the_ceiling_and_a_tight_one_goes_negative() {
        // A worked clean-vs-tight comparison. CLEAN — Racebnd4: no two-tone product comes within
        // 35 MHz of a used channel, so nothing is charged and the rating sits at the ceiling.
        // TIGHT — three consecutive Raceband channels: 2·5695 − 5658 = 5732, *exactly* a used
        // channel (gap 0, the maximum 1225 charge), and 2·5695 − 5732 = 5658 likewise.
        let clean = [5658u16, 5732, 5843, 5917];
        let tight = [5658u16, 5695, 5732];

        assert_eq!(imd_rating(&clean), RATING_MAX, "nothing to charge");
        assert!(
            imd_rating(&tight) < 0,
            "a product landing on a used channel must be punished hard, got {}",
            imd_rating(&tight)
        );
        assert!(imd_rating(&clean) > imd_rating(&tight));
    }

    #[test]
    fn a_near_miss_costs_less_than_a_direct_hit() {
        // The squared term is the point of accumulating rather than minimising: a product 34 MHz
        // off costs 1, one landing on the channel costs 1225.
        let grazing = imd_rating(&[5658u16, 5695, 5733]); // 2·5695 − 5658 = 5732, 1 MHz off 5733
        let direct = imd_rating(&[5658u16, 5695, 5732]); // 2·5695 − 5658 = 5732 exactly
        assert!(
            grazing > direct,
            "grazing {grazing} must beat a direct hit {direct}"
        );
    }

    #[test]
    fn products_outside_the_receivable_band_are_ignored() {
        // 2·5658 − 5917 = 5399 and 2·5917 − 5658 = 6176. The first is inside 5100..=6099 but
        // 259 MHz from any used channel; the second is outside the band entirely. Neither is
        // charged, so a widely-separated pair sits at the ceiling.
        assert_eq!(imd_rating(&[5658, 5917]), RATING_MAX);
        // Order does not matter.
        assert_eq!(imd_rating(&[5917, 5658]), imd_rating(&[5658, 5917]));
    }

    #[test]
    fn fewer_than_two_frequencies_have_no_products() {
        // No pairs ⇒ no products ⇒ the ceiling. (IMDTabler would divide by zero on the empty
        // set; the ceiling is the only sensible reading.)
        assert_eq!(imd_rating(&[]), RATING_MAX);
        assert_eq!(imd_rating(&[5800]), RATING_MAX);
    }

    #[test]
    fn min_channel_separation_reports_the_tightest_pair() {
        assert_eq!(min_channel_separation(&RACEBAND), Some(37));
        assert_eq!(min_channel_separation(&[5917, 5658, 5673]), Some(15));
        assert_eq!(min_channel_separation(&[5800]), None);
        assert_eq!(min_channel_separation(&[]), None);
    }

    #[test]
    fn pick_never_returns_two_channels_under_the_separation_floor() {
        // #430's live bug: the picker's best 6-set from the full catalog used to contain two
        // channels 15 MHz apart — unflyable on adjacent-channel bleed whatever its IMD rating.
        // The floor now binds for every heat size the catalog can seat.
        let pool = full_catalog();
        for n in 2..=8 {
            let picked = pick_best_imd_set(&pool, n);
            assert_eq!(picked.len(), n, "n={n}: exactly the requested size");
            assert!(
                min_channel_separation(&picked).unwrap() >= MIN_CHANNEL_SEPARATION_MHZ,
                "n={n}: {picked:?} has a pair under the {MIN_CHANNEL_SEPARATION_MHZ} MHz floor"
            );
        }
    }

    #[test]
    fn pick_finds_multigps_official_six_from_the_full_catalog() {
        // The headline #430 check: given every channel in the standard catalog and six pilots,
        // the picker independently arrives at ETBest6 — the set MultiGP publishes for 6-pilot
        // heats — and rates it 67, the number IMDTabler gives it. Before #430 it returned a set
        // with two channels 15 MHz apart.
        let picked = pick_best_imd_set(&full_catalog(), 6);
        assert_eq!(picked, vec![5645, 5685, 5760, 5805, 5905, 5945]);
        assert_eq!(imd_rating(&picked), 67);
    }

    #[test]
    fn pick_beats_the_naive_first_fit() {
        // From the full Raceband pool the best 3-subset must out-rate the first-fit R1,R2,R3
        // (which has a product landing exactly on R3).
        let picked = pick_best_imd_set(&RACEBAND, 3);
        assert_eq!(picked.len(), 3, "exactly the requested size");

        let first_fit = [5658u16, 5695, 5732];
        assert!(
            imd_rating(&picked) > imd_rating(&first_fit),
            "picked {picked:?} (rating {}) must beat first-fit {first_fit:?} (rating {})",
            imd_rating(&picked),
            imd_rating(&first_fit),
        );
        for ch in &picked {
            assert!(RACEBAND.contains(ch), "{ch} is from the pool");
        }
    }

    #[test]
    fn pick_never_exceeds_available() {
        // n larger than the pool returns the whole (sorted) pool, never invents channels — even
        // though that set may be under the separation floor. Refusing a set the RD explicitly
        // configured would be worse than handing it back.
        let pool = [5658u16, 5917, 5732];
        assert_eq!(pick_best_imd_set(&pool, 10), vec![5658, 5732, 5917]);
    }

    #[test]
    fn pick_zero_is_empty() {
        assert!(pick_best_imd_set(&RACEBAND, 0).is_empty());
    }

    #[test]
    fn pick_falls_back_when_no_set_can_clear_the_floor() {
        // A pool packed tighter than the floor everywhere (5 MHz steps). No 3-subset can clear
        // 35 MHz, so rather than returning fewer channels than asked for — which the caller
        // would report to the RD as "too few channels", a lie — the search reruns unconstrained
        // and returns the least-bad set of the requested size.
        let pool: Vec<u16> = (0..8).map(|i| 5800 + i * 5).collect();
        let picked = pick_best_imd_set(&pool, 3);
        assert_eq!(picked.len(), 3);
        for ch in &picked {
            assert!(pool.contains(ch), "{ch} is from the pool");
        }
        // Still deterministic on the fallback path.
        assert_eq!(picked, pick_best_imd_set(&pool, 3));
    }

    #[test]
    fn pick_is_deterministic_and_sorted() {
        // Same inputs ⇒ same output, every time, sorted ascending — on the small-pool path and
        // on the full catalog, which is what keeps heat fill replay-deterministic.
        for pool in [RACEBAND.to_vec(), full_catalog()] {
            for n in 1..=6 {
                let a = pick_best_imd_set(&pool, n);
                let b = pick_best_imd_set(&pool, n);
                assert_eq!(a, b, "deterministic for n={n}");
                let mut sorted = a.clone();
                sorted.sort_unstable();
                assert_eq!(a, sorted, "sorted ascending for n={n}");
            }
        }
    }

    #[test]
    fn pick_ignores_the_order_the_pool_is_offered_in() {
        // The pool is sorted before the search, so an RD's preference order cannot change which
        // set comes out — only which channels are on the table.
        let mut shuffled = full_catalog();
        shuffled.reverse();
        assert_eq!(
            pick_best_imd_set(&shuffled, 5),
            pick_best_imd_set(&full_catalog(), 5)
        );
    }

    #[test]
    fn pick_dedupes_a_repeated_channel() {
        // A pool listing a channel twice never yields it twice in the subset.
        let pool = [5658u16, 5658, 5769, 5917, 5806];
        let picked = pick_best_imd_set(&pool, 3);
        let mut deduped = picked.clone();
        deduped.dedup();
        assert_eq!(picked.len(), deduped.len(), "no duplicate channel");
    }

    #[test]
    fn tie_break_prefers_wider_spread_then_lower_channels() {
        // Four channels 50 MHz apart: every 2-subset rates 100 (no product lands within 35 MHz
        // of a used channel) and every one clears the floor, so the picker must fall through to
        // the spread tie-break and take the outermost pair.
        let pool = [5700u16, 5750, 5800, 5850];
        assert_eq!(pick_best_imd_set(&pool, 2), vec![5700, 5850]);
    }

    #[test]
    fn a_wider_pool_never_produces_a_worse_set() {
        // Monotonicity in the pool: adding channels can only ever help, which is the premise
        // #117 S3 removed the `take(nodes)` truncation on.
        for n in 2..=5 {
            let narrow = pick_best_imd_set(&RACEBAND, n);
            let wide = pick_best_imd_set(&full_catalog(), n);
            assert!(
                imd_rating(&wide) >= imd_rating(&narrow),
                "n={n}: the full catalog ({wide:?}, {}) must not lose to Raceband alone ({narrow:?}, {})",
                imd_rating(&wide),
                imd_rating(&narrow),
            );
        }
    }

    #[test]
    fn the_greedy_fallback_returns_a_valid_set() {
        // Reached only when the exact search blows its budget; exercised directly so the path
        // stays honest. It must still return a sorted, in-pool subset of the requested size.
        let pool: Vec<u16> = (0..13).map(|i| 5600 + i * 30).collect();
        let picked = best_subset_greedy(&pool, 5);
        assert_eq!(picked.len(), 5);
        let mut sorted = picked.clone();
        sorted.sort_unstable();
        assert_eq!(picked, sorted, "sorted");
        for ch in &picked {
            assert!(pool.contains(ch), "in pool");
        }
        assert_eq!(picked, best_subset_greedy(&pool, 5), "deterministic");
    }
}
