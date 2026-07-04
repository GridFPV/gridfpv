//! Clock alignment + sequence handling (#20).
//!
//! Every source stamps in its own clock (game-engine, server, device RTC, wall).
//! At session start an adapter captures an offset mapping its source clock onto the
//! Director's session timeline, so events from different sources share one axis;
//! source time stays authoritative for intervals. Where a source exposes a
//! monotonic sequence counter, the adapter carries it through.
//!
//! This module provides three small, pure pieces, matching `docs/timer-adapters.html`
//! §4 (Clocks & time):
//!
//! - [`SessionTime`] — a microsecond instant on the **Director's session timeline**,
//!   the shared axis onto which every source is aligned. Session time `0` is the
//!   session anchor.
//! - [`ClockAlignment`] — the per-source offset captured at session start that maps a
//!   source [`SourceTime`] onto that shared axis. The direction is **source →
//!   session**: alignment exists to *order and display* events from multiple sources
//!   on one axis, nothing more.
//! - [`pass_order_key`] / [`compare_passes`] — the ordering rule that prefers a
//!   present monotonic `sequence` and falls back to source `at`, mirroring the lap
//!   projection so the whole system orders passes identically.
//!
//! # Intervals stay on source time
//!
//! Alignment is **never** used for timing math. Lap and split durations are computed
//! from the source's own timestamps via [`SourceTime::micros_since`] — internally
//! consistent and immune to clock skew between sources. This module deliberately
//! offers no way to take an interval between two [`SessionTime`]s; the only interval
//! helper here, [`source_interval_micros`], operates on [`SourceTime`]. Aligning two
//! stamps from one source and subtracting is a no-op (the offset cancels), and across
//! *different* sources it would silently mix two clocks — so intervals always use the
//! source clock directly. See [`ClockAlignment`] for the full reasoning.
//!
//! Pure and deterministic: no IO, no time-of-day calls. An adapter captures the
//! anchor from values it already has (the first source timestamp it sees, or a
//! session-start message), so replaying a recorded session aligns identically.
#![forbid(unsafe_code)]

use gridfpv_events::{Pass, SourceTime};

/// An instant on the **Director's session timeline** — the shared axis that every
/// source is aligned onto, in microseconds. Session time `0` is the session anchor
/// (the instant each source's [`ClockAlignment`] is captured against).
///
/// Distinct from [`SourceTime`] on purpose: a [`SourceTime`] is only comparable to
/// other stamps from the *same* source clock, whereas [`SessionTime`]s from
/// different sources are mutually comparable because they have been mapped onto one
/// axis. Keeping them as separate types stops the two from being mixed by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionTime {
    /// Microseconds on the Director's session timeline.
    pub micros: i64,
}

impl SessionTime {
    /// The session anchor — session time `0`.
    pub const ANCHOR: SessionTime = SessionTime { micros: 0 };

    /// Construct from a microsecond count on the session timeline.
    pub const fn from_micros(micros: i64) -> Self {
        Self { micros }
    }
}

/// The per-source mapping from a source's own clock onto the Director's session
/// timeline, captured once at session start.
///
/// # Direction: source → session
///
/// At session start the adapter observes the source-clock reading of the session
/// anchor — call it `anchor` — and records it. Thereafter any source stamp `t` maps
/// to session time `t.micros - anchor.micros`. Equivalently the stored
/// [`offset_micros`](Self::offset_micros) is `-anchor.micros`, and
///
/// ```text
/// session_micros = source_micros + offset_micros
/// ```
///
/// So the source instant equal to the anchor maps to [`SessionTime::ANCHOR`], and
/// later source instants map to positive session times. Two sources whose clocks
/// disagree by some constant each capture their own anchor, so both land the session
/// anchor at session `0` and stay correctly ordered relative to one another
/// afterwards.
///
/// # Why this is for ordering/display only
///
/// Because the map is a pure translation (add a constant), subtracting two *aligned*
/// stamps from the **same source** gives the same number as subtracting the two
/// source stamps directly — the offset cancels. Aligning therefore buys nothing for
/// interval math and, across *different* sources, would silently mix two clocks. So
/// intervals always use the source clock ([`SourceTime::micros_since`] /
/// [`source_interval_micros`]); alignment is reserved for putting multiple sources on
/// one axis to order and display them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClockAlignment {
    /// Added to a source-clock microsecond reading to get session-timeline micros.
    /// Equal to `-anchor.micros`, where `anchor` is the source-clock reading captured
    /// at the session anchor.
    offset_micros: i64,
}

impl ClockAlignment {
    /// Capture the alignment from the source-clock reading of the session anchor.
    ///
    /// `anchor` is whatever source stamp the adapter decides marks session time `0`:
    /// the source's session-start timestamp where it exposes a lifecycle, otherwise
    /// the first source timestamp seen. After this, [`to_session`](Self::to_session)
    /// maps `anchor` to [`SessionTime::ANCHOR`].
    pub const fn capture(anchor: SourceTime) -> Self {
        Self {
            offset_micros: -anchor.micros,
        }
    }

    /// Build directly from a known offset (`session = source + offset`). Mostly for
    /// tests and persistence; prefer [`capture`](Self::capture) at session start.
    pub const fn from_offset_micros(offset_micros: i64) -> Self {
        Self { offset_micros }
    }

    /// The offset added to source-clock micros to reach session-timeline micros.
    /// Equal to `-anchor.micros`.
    pub const fn offset_micros(self) -> i64 {
        self.offset_micros
    }

    /// Map a source stamp onto the Director's session timeline. **For ordering and
    /// display across sources only — never for interval math** (see
    /// [`ClockAlignment`]).
    pub const fn to_session(self, at: SourceTime) -> SessionTime {
        SessionTime {
            micros: at.micros + self.offset_micros,
        }
    }
}

/// Signed microseconds between two stamps **from the same source clock**
/// (`later - earlier`), the only correct way to measure an interval.
///
/// A thin, intention-revealing wrapper over [`SourceTime::micros_since`]: intervals
/// are computed on source time, never on aligned/session time. Provided here so call
/// sites in this module make the "source clock for intervals" rule explicit.
pub const fn source_interval_micros(earlier: SourceTime, later: SourceTime) -> i64 {
    later.micros_since(earlier)
}

/// Ordering key for a pass: sequenced passes first (ascending `sequence`), then
/// unsequenced ones, with `at` (source timestamp) as the final tie-break.
///
/// This is the **same rule the lap projection uses** (`gridfpv_projection::lap_list`),
/// reproduced here so adapters order passes identically before the projection ever
/// sees them. The key is `(sequence.is_none(), sequence, at)`:
///
/// - `sequence.is_none()` is `false` for sequenced passes, so they sort *first*, then
///   by `sequence` ascending — the source's monotonic counter is authoritative when
///   present and survives clock adjustments.
/// - Passes without a `sequence` fall back to `at` ascending.
/// - `at` is the final tie-break in every case, keeping equal keys deterministic.
///
/// Use with a **stable** sort so passes with fully equal keys keep their original log
/// order. Mixing sequenced and unsequenced passes for one competitor is not expected
/// from a real source (a source either numbers its passes or it does not); this rule
/// just keeps ordering total and deterministic if it happens.
pub fn pass_order_key(pass: &Pass) -> (bool, Option<u64>, SourceTime) {
    (pass.sequence.is_none(), pass.sequence, pass.at)
}

/// Compare two passes by the ordering rule in [`pass_order_key`].
///
/// Convenience for `slice::sort_by` / `Ordering`-based callers; produces the same
/// order as sorting by [`pass_order_key`].
pub fn compare_passes(a: &Pass, b: &Pass) -> std::cmp::Ordering {
    pass_order_key(a).cmp(&pass_order_key(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{AdapterId, CompetitorRef, GateIndex};

    fn pass(at: i64, sequence: Option<u64>) -> Pass {
        Pass {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef("A".into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
            heat: None,
        }
    }

    // --- Offset capture + mapping --------------------------------------------

    #[test]
    fn anchor_maps_to_session_zero() {
        // The source stamp captured as the anchor lands on session time 0.
        let anchor = SourceTime::from_micros(5_000_000);
        let align = ClockAlignment::capture(anchor);
        assert_eq!(align.to_session(anchor), SessionTime::ANCHOR);
        assert_eq!(align.offset_micros(), -5_000_000);
    }

    #[test]
    fn later_source_time_maps_to_positive_session_time() {
        let align = ClockAlignment::capture(SourceTime::from_micros(5_000_000));
        // 2.5s after the anchor on the source clock -> +2.5s on the session axis.
        let mapped = align.to_session(SourceTime::from_micros(7_500_000));
        assert_eq!(mapped, SessionTime::from_micros(2_500_000));
    }

    #[test]
    fn before_anchor_maps_to_negative_session_time() {
        let align = ClockAlignment::capture(SourceTime::from_micros(5_000_000));
        let mapped = align.to_session(SourceTime::from_micros(4_000_000));
        assert_eq!(mapped, SessionTime::from_micros(-1_000_000));
    }

    #[test]
    fn from_offset_round_trips_with_offset_accessor() {
        let align = ClockAlignment::from_offset_micros(-1_234);
        assert_eq!(align.offset_micros(), -1_234);
        assert_eq!(
            align.to_session(SourceTime::from_micros(2_000)),
            SessionTime::from_micros(766)
        );
    }

    // --- Two sources, different clocks, one axis -----------------------------

    #[test]
    fn two_sources_with_different_clocks_align_onto_one_axis() {
        // Source A's clock is offset far ahead of source B's, yet both capture their
        // own anchor at session start, so both anchors sit at session 0 and the axis
        // is shared.
        let a = ClockAlignment::capture(SourceTime::from_micros(1_000_000_000));
        let b = ClockAlignment::capture(SourceTime::from_micros(10_000));

        // A crossing 3s into A's run and a crossing 1s into B's run...
        let a_at_3s = a.to_session(SourceTime::from_micros(1_003_000_000));
        let b_at_1s = b.to_session(SourceTime::from_micros(1_010_000));

        assert_eq!(a_at_3s, SessionTime::from_micros(3_000_000));
        assert_eq!(b_at_1s, SessionTime::from_micros(1_000_000));
        // ...order correctly on the shared axis despite wildly different raw clocks.
        assert!(b_at_1s < a_at_3s);
    }

    #[test]
    fn relative_ordering_across_sources_follows_session_time() {
        // B's event happens later in real session time than A's, even though B's raw
        // source clock reads a *smaller* number. Alignment fixes the ordering.
        let a = ClockAlignment::capture(SourceTime::from_micros(1_000_000_000));
        let b = ClockAlignment::capture(SourceTime::from_micros(0));

        let a_event_raw = SourceTime::from_micros(1_002_000_000); // +2s on A
        let b_event_raw = SourceTime::from_micros(5_000_000); // +5s on B

        // Raw source numbers would order A's event after B's (1.002e9 > 5e6)...
        assert!(a_event_raw.micros > b_event_raw.micros);
        // ...but on the session axis B (+5s) is correctly after A (+2s).
        assert!(b.to_session(b_event_raw) > a.to_session(a_event_raw));
    }

    // --- Intervals stay on source time ---------------------------------------

    #[test]
    fn interval_uses_source_clock_and_is_unaffected_by_alignment() {
        let earlier = SourceTime::from_micros(2_000_000);
        let later = SourceTime::from_micros(6_500_000);

        let on_source = source_interval_micros(earlier, later);
        assert_eq!(on_source, 4_500_000);

        // Aligning both stamps first (same source) and subtracting on the session
        // axis yields the identical number: the offset cancels. Proves alignment is
        // a pure shift and contributes nothing to interval math.
        let align = ClockAlignment::capture(SourceTime::from_micros(123_456));
        let aligned_delta = align.to_session(later).micros - align.to_session(earlier).micros;
        assert_eq!(aligned_delta, on_source);
    }

    #[test]
    fn interval_is_independent_of_which_offset_was_captured() {
        // Whatever anchor a source picks, the source-clock interval is the same.
        let earlier = SourceTime::from_micros(10_000_000);
        let later = SourceTime::from_micros(13_000_000);
        for anchor in [0, 5_000_000, -2_000_000, i64::from(i32::MAX)] {
            let align = ClockAlignment::capture(SourceTime::from_micros(anchor));
            let aligned_delta = align.to_session(later).micros - align.to_session(earlier).micros;
            assert_eq!(aligned_delta, source_interval_micros(earlier, later));
            assert_eq!(aligned_delta, 3_000_000);
        }
    }

    // --- Sequence ordering + tie-break ---------------------------------------

    #[test]
    fn sequenced_passes_sort_by_sequence_ascending() {
        let mut passes = [
            pass(6_000_000, Some(3)),
            pass(1_000_000, Some(1)),
            pass(4_000_000, Some(2)),
        ];
        passes.sort_by_key(pass_order_key);
        assert_eq!(
            passes.iter().map(|p| p.sequence).collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn sequence_wins_over_timestamp() {
        // Sequence disagrees with timestamp order; sequence is authoritative.
        let mut passes = [pass(1_000_000, Some(2)), pass(9_000_000, Some(1))];
        passes.sort_by(compare_passes);
        assert_eq!(passes[0].sequence, Some(1));
        assert_eq!(passes[0].at, SourceTime::from_micros(9_000_000));
    }

    #[test]
    fn sequenced_passes_sort_ahead_of_unsequenced() {
        let mut passes = [pass(1_000_000, None), pass(9_000_000, Some(7))];
        passes.sort_by(compare_passes);
        assert_eq!(passes[0].sequence, Some(7));
        assert_eq!(passes[1].sequence, None);
    }

    #[test]
    fn unsequenced_passes_fall_back_to_timestamp() {
        let mut passes = [
            pass(7_000_000, None),
            pass(2_000_000, None),
            pass(5_000_000, None),
        ];
        passes.sort_by(compare_passes);
        assert_eq!(
            passes.iter().map(|p| p.at.micros).collect::<Vec<_>>(),
            vec![2_000_000, 5_000_000, 7_000_000]
        );
    }

    #[test]
    fn equal_sequence_keys_break_ties_on_at() {
        // Pathological equal sequences fall through to `at` ascending.
        let mut passes = [pass(8_000_000, Some(1)), pass(3_000_000, Some(1))];
        passes.sort_by(compare_passes);
        assert_eq!(passes[0].at, SourceTime::from_micros(3_000_000));
        assert_eq!(passes[1].at, SourceTime::from_micros(8_000_000));
    }

    #[test]
    fn compare_passes_matches_order_key() {
        let a = pass(1_000_000, Some(1));
        let b = pass(2_000_000, Some(2));
        assert_eq!(
            compare_passes(&a, &b),
            pass_order_key(&a).cmp(&pass_order_key(&b))
        );
        assert_eq!(compare_passes(&a, &b), std::cmp::Ordering::Less);
    }
}
