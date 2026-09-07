//! Reconnect + deduplication (#21).
//!
//! A source that drops mid-race reconnects and resumes appending. To stay robust,
//! a reconnecting source is expected to **replay** an overlapping tail of recent
//! passes rather than trust that the consumer saw exactly up to the disconnect.
//! Deduplication is what makes that replay safe: it keys each pass on a stable
//! identity so a pass already recorded is suppressed instead of double-counted.
//!
//! See `docs/timer-adapters.html` §5:
//! > A source that drops mid-race reconnects and resumes appending; deduplication
//! > relies on source sequence numbers / timestamps so a reconnect can't
//! > double-count a pass.
//!
//! # Dedup key
//!
//! A [`Pass`] is identified by a [`PassKey`]:
//!
//! - When the pass carries a `sequence`, the key is
//!   `(adapter, competitor, Seq(sequence))`. The source's own monotonic counter is
//!   authoritative — it survives clock adjustments and disambiguates passes that
//!   share a timestamp.
//! - When `sequence` is `None`, the key falls back to
//!   `(adapter, competitor, Time(at))`, using the source timestamp.
//!
//! `adapter` and `competitor` are always part of the key, so two different
//! competitors — or two different sources — that happen to emit the same sequence
//! number or the same timestamp are **never** collapsed into one.
//!
//! ## When the sequence is not an identity
//!
//! That default rests on a premise — "the source's own monotonic counter is
//! authoritative" — that is **false for some sources**, and silently so. A source
//! whose `sequence` is a *display ordinal* recomputed over the whole list can hand
//! two different crossings the same number over the life of a race, and the default
//! keying then swallows the second one as a replay of the first.
//!
//! RotorHazard is exactly that source (#434): `RHRace.delete_lap` marks the deleted
//! crossing and then **renumbers every surviving lap of that seat sequentially from
//! 0**, so the pilot's next genuine crossing arrives carrying a `lap_number` already
//! accepted. `restore_deleted_lap` and `replace_laps` renumber the same way. All
//! three leave `lap_time_stamp` — and therefore the pass's `at` — untouched.
//! Verified against RH 4.3.0 and 4.4.0.
//!
//! Such a source constructs its deduplicator with
//! [`PassIdentity::CrossingTime`](PassIdentity::CrossingTime), which ignores
//! `sequence` and keys on the timestamp. The pass still *carries* its `sequence` —
//! it is the right lap number to display and to order by, it is just not an
//! identity.
//!
//! ## Limitation
//!
//! The fallback is only as good as the timestamps. A source that provides **no**
//! stable sequence counter **and** can emit two genuinely distinct passes for the
//! same `(adapter, competitor, at)` (identical source timestamps) cannot be
//! perfectly deduplicated: the second such pass is indistinguishable from a replay
//! of the first and will be dropped. Sources whose timestamps are unique per
//! competitor (the common case), or that supply a sequence counter at all, are not
//! affected. The fix where it matters is upstream — have the adapter carry a real
//! `sequence` (see [`clock`](crate::clock), #20).
//!
//! # Usage
//!
//! [`Deduplicator`] is stateful and remembers every key it has accepted.
//!
//! - [`Deduplicator::observe`] — feed one [`Pass`]; returns `true` if it is new
//!   (keep it) and `false` if it is a duplicate (drop it).
//! - [`Deduplicator::retain_new`] — filter a `Vec<Event>` in place, keeping only
//!   first-seen [`Pass`]es. Non-`Pass` events are connection/session/liveness
//!   signals with no pass identity, so they always pass through untouched.
//!
//! Both are pure and deterministic: same inputs, same decisions, no IO. A future
//! live adapter (#22/#23) owns one `Deduplicator` for the lifetime of a source and
//! runs every batch it translates — including the overlapping tail a reconnect
//! replays — through [`retain_new`](Deduplicator::retain_new) before appending. The
//! replayed prefix is recognised and dropped; only the genuinely new suffix is
//! emitted.

use std::collections::HashSet;

use gridfpv_events::{AdapterId, CompetitorRef, Event, Pass, SourceTime};

/// The discriminator within a `(adapter, competitor)` pair: the authoritative
/// source sequence number when present, otherwise the source timestamp.
///
/// Kept private to the key — callers never construct it directly; it is derived
/// from a [`Pass`] by [`PassKey::of`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Discriminator {
    /// The source's monotonic sequence counter (authoritative when present).
    Seq(u64),
    /// Fallback: the source timestamp, used when no sequence is available.
    Time(SourceTime),
}

/// The deduplication identity of a [`Pass`].
///
/// Two passes are "the same pass" — and the second is a reconnect duplicate — iff
/// their `PassKey`s are equal. See the [module docs](self) for the keying rule and
/// its limitation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PassKey {
    adapter: AdapterId,
    competitor: CompetitorRef,
    discriminator: Discriminator,
}

/// What a source's passes are identified **by** — see the [module docs](self).
///
/// A property of the *source*, fixed for the life of its [`Deduplicator`], not of an
/// individual pass: whether a `sequence` names a crossing is a fact about how the
/// source numbers things, and answering it per-pass would be guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PassIdentity {
    /// The source's `sequence` where it carries one, falling back to the timestamp.
    /// The right choice for a source whose counter is genuinely per-crossing and
    /// monotonic, because it survives clock adjustments and separates two passes that
    /// share a timestamp.
    #[default]
    SourceSequence,
    /// The source **timestamp**, always — `sequence` is ignored for identity.
    ///
    /// For a source whose `sequence` is a display ordinal that can be recomputed, so
    /// the same number can name different crossings over a race. See the [module
    /// docs](self) for RotorHazard's renumbering, which is the case this exists for
    /// (#434).
    CrossingTime,
}

impl PassKey {
    /// Derive the dedup key for a pass under the default
    /// [`PassIdentity::SourceSequence`] rule: sequence-keyed when the pass carries a
    /// `sequence`, timestamp-keyed otherwise.
    pub fn of(pass: &Pass) -> Self {
        Self::of_with(PassIdentity::SourceSequence, pass)
    }

    /// Derive the dedup key for a pass under an explicit [`PassIdentity`].
    pub fn of_with(identity: PassIdentity, pass: &Pass) -> Self {
        let discriminator = match (identity, pass.sequence) {
            (PassIdentity::SourceSequence, Some(seq)) => Discriminator::Seq(seq),
            (PassIdentity::SourceSequence, None) | (PassIdentity::CrossingTime, _) => {
                Discriminator::Time(pass.at)
            }
        };
        Self {
            adapter: pass.adapter.clone(),
            competitor: pass.competitor.clone(),
            discriminator,
        }
    }
}

/// Suppresses passes already seen, so a reconnect that re-sends an overlapping tail
/// yields no duplicate events.
///
/// Stateful: it accumulates the [`PassKey`] of every pass it accepts and rejects any
/// pass whose key it has already accepted. Construct one per source and keep it for
/// that source's lifetime (across reconnects). See the [module docs](self).
#[derive(Debug, Clone, Default)]
pub struct Deduplicator {
    seen: HashSet<PassKey>,
    /// What this source's passes are identified by. Fixed for the deduplicator's life —
    /// changing it mid-race would leave the keys already accepted un-matchable.
    identity: PassIdentity,
}

impl Deduplicator {
    /// A fresh deduplicator that has seen nothing, keyed the default way
    /// ([`PassIdentity::SourceSequence`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh deduplicator that identifies passes the given way — for a source whose
    /// `sequence` is not a per-crossing identity. See [`PassIdentity`].
    pub fn keyed_on(identity: PassIdentity) -> Self {
        Self {
            seen: HashSet::new(),
            identity,
        }
    }

    /// What this deduplicator identifies passes by.
    pub fn identity(&self) -> PassIdentity {
        self.identity
    }

    /// Observe one pass. Returns `true` if it is new — its key had not been seen, so
    /// it is now recorded and should be kept — and `false` if it is a duplicate of a
    /// pass already accepted (drop it).
    pub fn observe(&mut self, pass: &Pass) -> bool {
        // `HashSet::insert` returns `true` when the value was newly inserted, which
        // is exactly "this pass is new". A duplicate leaves the set unchanged.
        self.seen.insert(self.key(pass))
    }

    /// This deduplicator's key for `pass`, under the identity it was constructed with.
    fn key(&self, pass: &Pass) -> PassKey {
        PassKey::of_with(self.identity, pass)
    }

    /// Whether `pass` would be treated as a duplicate, **without** recording it.
    /// Useful for inspection/metrics; [`observe`](Self::observe) is the one that
    /// updates state.
    pub fn is_duplicate(&self, pass: &Pass) -> bool {
        self.seen.contains(&self.key(pass))
    }

    /// Filter a batch of events in place, keeping only first-seen passes.
    ///
    /// Each [`Event::Pass`] is run through [`observe`](Self::observe); duplicates are
    /// removed. Every non-`Pass` event (connect/disconnect, session lifecycle,
    /// competitor-seen) has no pass identity and is always retained, in order. This
    /// is the entry point an adapter runs each translated batch through — including
    /// the overlapping tail a reconnect replays.
    pub fn retain_new(&mut self, events: &mut Vec<Event>) {
        events.retain_mut(|event| match event {
            Event::Pass(pass) => self.observe(pass),
            _ => true,
        });
    }

    /// Number of distinct pass keys accepted so far.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether no pass has been accepted yet.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{GateIndex, SessionId};

    /// Build a pass with the fields dedup actually keys on; gate/signal are
    /// irrelevant to identity here.
    fn pass(adapter: &str, competitor: &str, at: i64, sequence: Option<u64>) -> Pass {
        Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
            heat: None,
        }
    }

    /// Run a slice of events through a deduplicator and collect what survives.
    fn filter(dedup: &mut Deduplicator, events: Vec<Event>) -> Vec<Event> {
        let mut events = events;
        dedup.retain_new(&mut events);
        events
    }

    #[test]
    fn clean_stream_passes_through_unchanged() {
        let mut dedup = Deduplicator::new();
        let stream: Vec<Event> = (0..5)
            .map(|i| Event::Pass(pass("velo", "Ace", i * 1_000_000, Some(i as u64))))
            .collect();

        let out = filter(&mut dedup, stream.clone());

        assert_eq!(out, stream, "no duplicates should be removed");
        assert_eq!(dedup.len(), 5);
    }

    #[test]
    fn observe_reports_new_then_duplicate() {
        let mut dedup = Deduplicator::new();
        let p = pass("velo", "Ace", 1_000_000, Some(7));
        assert!(dedup.observe(&p), "first sighting is new");
        assert!(!dedup.observe(&p), "second sighting is a duplicate");
        assert!(dedup.is_duplicate(&p));
    }

    #[test]
    fn reconnect_replaying_last_n_injects_zero_duplicates() {
        let mut dedup = Deduplicator::new();

        // Initial run: sequences 0..6 accepted.
        let initial: Vec<Event> = (0..7)
            .map(|i| Event::Pass(pass("velo", "Ace", i * 1_000_000, Some(i as u64))))
            .collect();
        let first = filter(&mut dedup, initial);
        assert_eq!(first.len(), 7);

        // Reconnect: source replays the last 3 it had (4,5,6) then appends 7,8.
        let replayed: Vec<Event> = (4..9)
            .map(|i| Event::Pass(pass("velo", "Ace", i * 1_000_000, Some(i as u64))))
            .collect();
        let after = filter(&mut dedup, replayed);

        // Only the genuinely new suffix (7, 8) survives — zero duplicates.
        let seqs: Vec<Option<u64>> = after
            .iter()
            .map(|e| match e {
                Event::Pass(p) => p.sequence,
                _ => None,
            })
            .collect();
        assert_eq!(seqs, vec![Some(7), Some(8)]);
        assert_eq!(dedup.len(), 9, "9 distinct passes total");
    }

    #[test]
    fn sequence_keyed_ignores_timestamp_differences() {
        // Same sequence re-sent with a *different* (e.g. re-stamped) timestamp is
        // still the same pass — the sequence is authoritative.
        let mut dedup = Deduplicator::new();
        assert!(dedup.observe(&pass("velo", "Ace", 1_000_000, Some(3))));
        assert!(
            !dedup.observe(&pass("velo", "Ace", 9_999_999, Some(3))),
            "sequence match wins over timestamp difference"
        );
    }

    #[test]
    fn timestamp_keyed_when_no_sequence() {
        let mut dedup = Deduplicator::new();
        // No sequence: identity falls back to the timestamp.
        assert!(dedup.observe(&pass("manual", "Ace", 5_000_000, None)));
        assert!(
            !dedup.observe(&pass("manual", "Ace", 5_000_000, None)),
            "same timestamp + no sequence is a duplicate"
        );
        assert!(
            dedup.observe(&pass("manual", "Ace", 6_000_000, None)),
            "different timestamp is a new pass"
        );
    }

    #[test]
    fn sequence_and_timestamp_keys_do_not_alias() {
        // A pass with sequence Some(5) and a different pass with no sequence but
        // timestamp 5 must not collide just because the inner number is 5.
        let mut dedup = Deduplicator::new();
        assert!(dedup.observe(&pass("velo", "Ace", 42, Some(5))));
        assert!(
            dedup.observe(&pass("velo", "Ace", 5, None)),
            "Seq(5) and Time(5) are distinct discriminators"
        );
    }

    #[test]
    fn distinct_competitors_with_equal_sequence_not_collapsed() {
        let mut dedup = Deduplicator::new();
        assert!(dedup.observe(&pass("velo", "Ace", 1_000_000, Some(1))));
        assert!(
            dedup.observe(&pass("velo", "Bee", 1_000_000, Some(1))),
            "same sequence, different competitor is a different pass"
        );
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn distinct_adapters_with_equal_sequence_not_collapsed() {
        let mut dedup = Deduplicator::new();
        assert!(dedup.observe(&pass("velo", "Ace", 1_000_000, Some(1))));
        assert!(
            dedup.observe(&pass("rh", "Ace", 1_000_000, Some(1))),
            "same sequence, different adapter is a different pass"
        );
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn distinct_competitors_with_equal_timestamp_not_collapsed() {
        // The fallback path must also keep competitors apart.
        let mut dedup = Deduplicator::new();
        assert!(dedup.observe(&pass("manual", "Ace", 7_000_000, None)));
        assert!(
            dedup.observe(&pass("manual", "Bee", 7_000_000, None)),
            "same timestamp, different competitor is a different pass"
        );
    }

    // ── PassIdentity::CrossingTime — for a source whose sequence is a display ordinal ────────

    /// **A renumbered source must not lose its next real crossing** (#434).
    ///
    /// RotorHazard's shape exactly: three crossings numbered 0,1,2; the RD deletes the middle one,
    /// so RH drops it from the payload and renumbers the survivors — and the *next* genuine
    /// crossing arrives as `2`, a number already accepted. Under the default keying that pass is
    /// indistinguishable from a replay; keyed on the crossing time it is what it is.
    #[test]
    fn crossing_time_keying_survives_a_renumbering() {
        let mut dedup = Deduplicator::keyed_on(PassIdentity::CrossingTime);
        for (seq, at) in [(0, 2_000_000), (1, 7_000_000), (2, 12_000_000)] {
            assert!(dedup.observe(&pass("rh", "node-0", at, Some(seq))));
        }
        // The RD deletes the 7 s crossing: 12 s comes back as lap 1, and the new 17 s crossing
        // arrives as lap 2.
        assert!(
            !dedup.observe(&pass("rh", "node-0", 12_000_000, Some(1))),
            "the same crossing under a new number is still the same crossing"
        );
        assert!(
            dedup.observe(&pass("rh", "node-0", 17_000_000, Some(2))),
            "a genuinely new crossing must not be swallowed by the number a deletion freed up"
        );

        // The default keying is what the bug was: it loses that lap.
        let mut sequenced = Deduplicator::new();
        for (seq, at) in [(0, 2_000_000), (1, 7_000_000), (2, 12_000_000)] {
            assert!(sequenced.observe(&pass("rh", "node-0", at, Some(seq))));
        }
        assert!(
            !sequenced.observe(&pass("rh", "node-0", 17_000_000, Some(2))),
            "…which is precisely why RotorHazard does not use it"
        );
    }

    /// Crossing-time keying still keeps competitors and adapters apart, and still suppresses the
    /// replay it exists for: a re-sent snapshot repeats both the stamp and the number.
    #[test]
    fn crossing_time_keying_still_dedups_replays_and_separates_seats() {
        let mut dedup = Deduplicator::keyed_on(PassIdentity::CrossingTime);
        assert!(dedup.observe(&pass("rh", "node-0", 5_000_000, Some(0))));
        assert!(
            !dedup.observe(&pass("rh", "node-0", 5_000_000, Some(0))),
            "a re-sent snapshot is still a duplicate"
        );
        assert!(
            dedup.observe(&pass("rh", "node-1", 5_000_000, Some(0))),
            "two seats crossing at the same stamp are two passes"
        );
        assert!(
            dedup.observe(&pass("other", "node-0", 5_000_000, Some(0))),
            "two sources are never collapsed"
        );
        assert_eq!(dedup.identity(), PassIdentity::CrossingTime);
    }

    #[test]
    fn non_pass_events_pass_through_untouched() {
        let mut dedup = Deduplicator::new();
        let adapter = AdapterId("rh".into());
        let session = SessionId("heat-1".into());
        let competitor = CompetitorRef("node-2".into());

        let events = vec![
            Event::AdapterConnected {
                adapter: adapter.clone(),
            },
            Event::SessionStarted {
                adapter: adapter.clone(),
                session: session.clone(),
            },
            Event::CompetitorSeen {
                adapter: adapter.clone(),
                competitor: competitor.clone(),
            },
            Event::Pass(pass("rh", "node-2", 1_000_000, Some(0))),
            Event::SessionEnded {
                adapter: adapter.clone(),
                session,
            },
            Event::AdapterDisconnected { adapter },
        ];

        // Even if the whole batch is replayed, only the one Pass is deduped; the
        // liveness/lifecycle events are not pass observations and always survive.
        let first = filter(&mut dedup, events.clone());
        assert_eq!(first, events, "first sighting keeps everything");

        let second = filter(&mut dedup, events.clone());
        assert_eq!(second.len(), events.len() - 1, "only the Pass is dropped");
        assert!(
            !second.iter().any(|e| matches!(e, Event::Pass(_))),
            "the duplicate Pass is gone; non-Pass events remain"
        );
    }

    #[test]
    fn is_empty_and_len_track_state() {
        let mut dedup = Deduplicator::new();
        assert!(dedup.is_empty());
        dedup.observe(&pass("velo", "Ace", 0, Some(0)));
        assert!(!dedup.is_empty());
        assert_eq!(dedup.len(), 1);
    }
}
