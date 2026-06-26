//! Projection engine — folds the append-only log into derived read models.
//!
//! Projections are recomputable from the log with no hidden state. The first
//! projection (a lap list) lands in #7; this crate is where it and later
//! projections (standings, brackets, stats) live.
//!
//! # Lap projection (#7)
//!
//! A **lap is two consecutive lap-gate passes** for the same competitor,
//! computed identically for every source. [`lap_list`] folds a sequence of
//! [`Event`]s into a [`LapList`] with no hidden state: folding the same events
//! always yields the same result, so the read model can be rebuilt from the log
//! at any time.
//!
//! # Marshaling (#31)
//!
//! Corrections are never mutations (architecture.html §3): the raw [`Pass`]es
//! stay byte-identical in the log forever, and a marshal's ruling is a *new*
//! appended event that the projection **folds in** over them.
//! [`corrected_passes`] is the **single home** of that fold (#39): it takes each
//! event paired with its append **offset** and folds the adjudications
//! ([`Event::DetectionVoided`], [`Event::LapInserted`], [`Event::LapAdjusted`])
//! into a *corrected view* of the lap-gate passes. [`lap_list_marshaled`] is the
//! marshaling-aware lap projection — a thin consumer of [`corrected_passes`] that
//! groups that corrected view by competitor and derives laps from it exactly as
//! [`lap_list`] does — and the engine's marshaling-aware scorer
//! (`gridfpv_engine::event::score_marshaled`) consumes the *same* [`corrected_passes`]
//! output, so the void/insert/adjust logic exists once. [`lap_list`] is the no-adjudications case:
//! it is a thin wrapper that assigns positional offsets and folds the same way,
//! so a log with no rulings projects identically through either entry point.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, HeatId, LogRef, Pass, PilotId, SignalHistory, SourceTime,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifies a competitor *within a single timing source*.
///
/// A source-local [`CompetitorRef`] is only meaningful relative to the adapter
/// that emitted it (node 2 on RotorHazard is unrelated to node 2 on a second
/// timer), so laps are grouped on the `(AdapterId, CompetitorRef)` pair. Binding
/// these per-source competitors to a single GridFPV pilot is a later registration
/// concern (Architecture §9) and deliberately out of scope here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CompetitorKey {
    /// The timing source the competitor belongs to.
    pub adapter: AdapterId,
    /// The source-local competitor handle.
    pub competitor: CompetitorRef,
}

impl CompetitorKey {
    /// Build a key from the `(adapter, competitor)` pair of a [`Pass`].
    fn from_pass(pass: &Pass) -> Self {
        Self {
            adapter: pass.adapter.clone(),
            competitor: pass.competitor.clone(),
        }
    }
}

/// A single completed lap: the interval between two consecutive lap-gate passes.
///
/// The lap also carries the **global append offsets** ([`LogRef`]) of the two passes that
/// bound it — `start_ref` (the opening pass) and `end_ref` (the closing pass). These are the
/// *stable* log offsets a marshaling command targets (`VoidDetection`/`AdjustLap`/`SplitLap`
/// all key on a single pass's offset), so a UI that selects a lap can address the correct pass
/// without the operator hand-typing an offset (#55). They are real global offsets even when the
/// lap list is folded from a heat window — the fold is fed `(global_offset, &Event)` pairs, so
/// `end_ref`/`start_ref` are valid command targets across a multi-heat log (the heat-window
/// re-enumeration bug this fixes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Lap {
    /// 1-based lap number within the competitor's run.
    pub number: u32,
    /// Lap duration in microseconds on the source clock
    /// (`pass[n + 1].at - pass[n].at`). Always `>= 0` for in-order passes.
    /// Renders as a plain TS `number` (bounded far below 2^53).
    #[ts(type = "number")]
    pub duration_micros: i64,
    /// Source-clock timestamp (µs) of the pass that **closes** this lap — the gate-pass instant.
    /// On the same clock as the signal trace's sample times (`from + i·period_micros`), so the
    /// Marshaling RSSI graph can place a vertical lap marker at exactly this lap's gate pass
    /// without re-deriving it from durations (Slice 4 — signal-as-evidence).
    pub at: SourceTime,
    /// Global append offset of the pass that **opens** this lap (the lap's start gate).
    pub start_ref: LogRef,
    /// Global append offset of the pass that **closes** this lap (the lap's end gate). This is
    /// the natural correction target: voiding/adjusting it edits this lap's boundary, and a
    /// `SplitLap` splits the over-long lap *ending* here.
    pub end_ref: LogRef,
}

/// Every lap a single competitor completed, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CompetitorLaps {
    /// Which source-local competitor these laps belong to.
    pub competitor: CompetitorKey,
    /// Completed laps, ordered by lap number (1-based, ascending).
    pub laps: Vec<Lap>,
}

impl CompetitorLaps {
    /// Number of completed laps (`K - 1` for `K` lap-gate passes).
    pub fn lap_count(&self) -> usize {
        self.laps.len()
    }

    /// Sum of all lap durations in microseconds; `0` when there are no laps.
    pub fn total_micros(&self) -> i64 {
        self.laps.iter().map(|lap| lap.duration_micros).sum()
    }

    /// The fastest lap, or `None` when no laps were completed.
    pub fn best(&self) -> Option<&Lap> {
        self.laps.iter().min_by_key(|lap| lap.duration_micros)
    }
}

/// The lap-list read model: per-competitor lap lists derived from the log.
///
/// Competitors are ordered deterministically by [`CompetitorKey`] so the
/// projection is stable across runs regardless of event arrival order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LapList {
    /// Per-competitor laps, ordered by competitor key.
    pub competitors: Vec<CompetitorLaps>,
}

impl LapList {
    /// Look up a single competitor's laps by key, if present.
    pub fn competitor(&self, key: &CompetitorKey) -> Option<&CompetitorLaps> {
        self.competitors.iter().find(|c| &c.competitor == key)
    }
}

/// Fold the log's registration bindings into a `(adapter, competitor) -> pilot` map (#60).
///
/// Each [`Event::CompetitorRegistered`] records that a source-local competitor *is* a
/// GridFPV pilot (Architecture §9). A competitor is keyed by its per-source
/// [`CompetitorKey`] — a bare [`CompetitorRef`] is only meaningful relative to its adapter
/// — and **last registration wins**: a later re-bind of the same `(adapter, competitor)`
/// supersedes the earlier one. The fold is pure and order-preserving, so replaying the
/// same log yields the same mapping. Competitors with no registration are simply absent —
/// they still appear by their bare [`CompetitorRef`] in the projections that consume this.
pub fn registrations<'a, I>(events: I) -> BTreeMap<CompetitorKey, PilotId>
where
    I: IntoIterator<Item = &'a Event>,
{
    let mut bindings = BTreeMap::new();
    for event in events {
        if let Event::CompetitorRegistered {
            adapter,
            competitor,
            pilot,
        } = event
        {
            // Insert (overwriting any earlier binding) — log order is append order, so the
            // last writer for a given competitor wins.
            bindings.insert(
                CompetitorKey {
                    adapter: adapter.clone(),
                    competitor: competitor.clone(),
                },
                pilot.clone(),
            );
        }
    }
    bindings
}

/// Fold a sequence of events into the lap-list read model.
///
/// Only [`Event::Pass`]es over the **lap gate** ([`is_lap_gate`]) contribute;
/// lifecycle events and split passes are ignored. Passes are grouped by
/// `(adapter, competitor)` and ordered within each group, then consecutive pairs
/// become laps.
///
/// # Ordering and tie-breaks
///
/// Within a competitor, passes are ordered by `at` (source timestamp), with
/// `sequence` as the tie-break for passes that share an instant (sequenced ahead
/// of unsequenced, then by `sequence` ascending). A real source either numbers its
/// passes monotonically in step with its clock or carries no sequence at all, so
/// ordering by `at` reproduces the source's timeline either way; the `sequence`
/// tie-break just keeps coincident passes deterministic. (This is the same key the
/// marshaling fold uses — see [`lap_list_marshaled`] — so the un-marshaled and
/// marshaled projections agree on ordering.)
///
/// The sort is *stable*, so passes with fully equal keys keep their original log
/// order.
///
/// Accepts anything iterable over `&Event` (e.g. `&[Event]`), so it is decoupled
/// from storage and trivially testable. This is the no-adjudications wrapper over
/// [`lap_list_marshaled`]; see it for the marshaling-aware fold.
///
/// [`is_lap_gate`]: gridfpv_events::GateIndex::is_lap_gate
pub fn lap_list<'a, I>(events: I) -> LapList
where
    I: IntoIterator<Item = &'a Event>,
{
    // The un-marshaled case is just the marshaling fold over a log that happens
    // to carry no adjudications: tag each event with its positional offset and
    // defer to `lap_list_marshaled`. With no `DetectionVoided`/`LapInserted`/
    // `LapAdjusted` present the corrected view is the raw view, so this projects
    // byte-for-byte identically to the original lap fold.
    lap_list_marshaled(events.into_iter().enumerate().map(|(i, e)| (i as u64, e)))
}

/// Fold a sequence of `(offset, event)` pairs into the **corrected lap-gate pass
/// stream**, applying every marshaling adjudication keyed on its target's append
/// **offset** (#31).
///
/// This is the *single source of truth* for the void/insert/adjust marshaling fold.
/// Both [`lap_list_marshaled`] (which groups these passes by competitor and derives
/// laps) and the engine's marshaling-aware scorer
/// (`gridfpv_engine::event::score_marshaled`) consume this one function — the fold
/// is implemented here, once, and nowhere else (#39).
///
/// Each event is paired with its append [`LogRef`](gridfpv_events::LogRef) offset;
/// rulings reference the raw event they correct by that offset. The result is a fresh
/// `Vec<(u64, Pass)>` of the surviving lap-gate passes (synthetic inserts included, re-timed
/// passes moved to their new instant), in **offset order**, each paired with the **global
/// append offset** that addresses it for a future correction — a raw/inserted/split pass's
/// own offset (the split's synthetic pass is addressable by the split event's offset, exactly
/// as "void the void" already relies on). The raw [`Pass`]es in the input are never mutated
/// (architecture.html §3); callers re-group/re-order as needed and may carry the offset onto
/// the projection (e.g. [`Lap::end_ref`]) so a UI can target the right pass.
///
/// # Adjudications folded
///
/// - [`Event::DetectionVoided { target }`](Event::DetectionVoided) — drop the
///   correction at `target` offset, as if it was never detected.
/// - [`Event::LapInserted { adapter, competitor, at }`](Event::LapInserted) — add
///   a synthetic lap-gate pass for that competitor at `at` (a lap the timer missed).
///   The insert's own offset becomes a valid `target` for a later ruling.
/// - [`Event::LapAdjusted { target, at }`](Event::LapAdjusted) — re-time the pass
///   at `target` offset to `at`. Because a lap is two consecutive passes, this shifts
///   *both* adjacent lap durations sharing the moved pass (the duration recompute is
///   structural — no extra event needed).
/// - [`Event::LapSplit { target, at }`](Event::LapSplit) — split the over-long lap
///   *ending* at the `target` pass by adding a synthetic mid-lap pass at `at`, attributed
///   to the target pass's competitor. Like an insert, the split's own offset is a valid
///   `target` for a later ruling (so it is reversible via "void the void").
///
/// # Offsets and last-writer-wins
///
/// Every fold-relevant entry — a raw lap-gate [`Pass`] *or* an adjudication — owns
/// the offset it was appended at, and a ruling addresses its target by that offset.
/// Rulings are applied **in log (offset) order**, so the *last writer to a given
/// target wins*: a [`Event::LapAdjusted`] then a later [`Event::DetectionVoided`]
/// of the same target leaves the pass voided; a later adjust of an already-adjusted
/// pass re-times from the original raw pass to the newest `at` (adjusts are not
/// cumulative — each re-times the *target's* raw value).
///
/// # "Void the void"
///
/// Because an adjudication is itself addressable by its offset, a
/// [`Event::DetectionVoided`] may target *another adjudication* rather than a raw
/// pass — the architecture.html §3 "void the void". A marshal who ruled wrongly
/// appends a higher-offset ruling that supersedes the earlier one:
///
/// - voiding a [`Event::LapInserted`] removes that synthetic pass again (the
///   inserted lap never makes it into the view);
/// - voiding a [`Event::LapAdjusted`] cancels the re-time, so its target raw pass
///   reverts to its original timestamp (the void supersedes the adjust);
/// - voiding a [`Event::DetectionVoided`] un-voids that earlier void's target — the
///   originally-voided raw pass comes back.
///
/// Resolution is purely last-writer-wins by offset and nothing is ever lost, so the
/// fold stays deterministic and recomputable.
///
/// # Heat / result-level rulings
///
/// [`Event::HeatVoided`], [`Event::PenaltyApplied`], and [`Event::RulingReversed`] are
/// *not* lap-level — they reshape the heat result, not the per-competitor lap list — so
/// this fold ignores them. They are consumed by scoring/results (#30, #33+), which fold
/// the same log alongside this corrected view.
pub fn corrected_passes<'a, I>(events: I) -> Vec<(u64, Pass)>
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    // First pass: record, by offset, every entry a later ruling could target —
    // raw lap-gate passes and the adjudications themselves — plus the rulings to
    // apply. We resolve targets against this map so "void the void" (a ruling whose
    // target is another ruling) and last-writer-wins both fall out of offset order.
    enum Entry<'a> {
        /// A raw lap-gate pass observed by an adapter (never mutated).
        RawPass(&'a Pass),
        /// A synthetic lap-gate pass inserted by marshaling.
        Inserted(Pass),
        /// A re-time ruling: the target pass's `at` is overridden to this value.
        Adjusted { target: u64, at: SourceTime },
        /// A split ruling: insert a synthetic mid-lap pass at `at`, attributed to the
        /// competitor whose lap *ends* at the `target` pass. Resolved to a concrete
        /// [`Pass`] in the second loop, where the target's adapter/competitor are known.
        Split { target: u64, at: SourceTime },
        /// A void ruling: the target entry is dropped from the corrected view.
        Voided { target: u64 },
    }

    let mut entries: BTreeMap<u64, Entry<'a>> = BTreeMap::new();
    for (offset, event) in events {
        match event {
            Event::Pass(pass) if pass.gate.is_lap_gate() => {
                entries.insert(offset, Entry::RawPass(pass));
            }
            Event::LapInserted {
                adapter,
                competitor,
                at,
            } => {
                entries.insert(
                    offset,
                    Entry::Inserted(Pass {
                        adapter: adapter.clone(),
                        competitor: competitor.clone(),
                        at: *at,
                        // A synthetic pass carries no source sequence; ordered by `at`.
                        sequence: None,
                        gate: gridfpv_events::GateIndex::LAP,
                        signal: None,
                    }),
                );
            }
            Event::LapAdjusted { target, at } => {
                entries.insert(
                    offset,
                    Entry::Adjusted {
                        target: target.0,
                        at: *at,
                    },
                );
            }
            Event::DetectionVoided { target } => {
                entries.insert(offset, Entry::Voided { target: target.0 });
            }
            Event::LapSplit { target, at } => {
                // A split inserts a synthetic mid-lap pass at `at` for the competitor whose
                // lap (the one *ending* at `target`) is being split — so the synthetic pass
                // carries the target pass's adapter/competitor. It is addressable by *this*
                // event's offset (just like an insert), so "void the void" reverses it.
                entries.insert(
                    offset,
                    Entry::Split {
                        target: target.0,
                        at: *at,
                    },
                );
            }
            // Lifecycle, heat transitions, and the heat/result-level rulings
            // (`HeatVoided`, `PenaltyApplied`, `RulingReversed`) never touch the lap view.
            _ => {}
        }
    }

    // Resolve each entry to its effective state by walking the chain of rulings in
    // offset order (BTreeMap iterates ascending). `voided[off]` marks an offset as
    // dropped from the view; `retime[off]` overrides a raw pass's timestamp. A
    // ruling targeting another ruling is the "void the void" / re-rule case — we
    // apply it against the target's *kind*, last writer winning by construction
    // (we process offsets ascending, so a later ruling overwrites an earlier one).
    let mut voided: BTreeMap<u64, bool> = BTreeMap::new();
    let mut retime: BTreeMap<u64, SourceTime> = BTreeMap::new();
    for entry in entries.values() {
        match entry {
            // Passes (raw, inserted, or split) carry no ruling of their own; the split is
            // resolved to a concrete pass in the emit loop. A void/adjust *targeting* a split
            // is handled below via `entries.get(target)` falling into the pass arms.
            Entry::RawPass(_) | Entry::Inserted(_) | Entry::Split { .. } => {}
            Entry::Adjusted { target, at } => {
                // Re-time the target raw/inserted pass, and un-void it: an adjust is
                // the newest ruling on that target, so it supersedes an earlier void.
                voided.insert(*target, false);
                retime.insert(*target, *at);
            }
            Entry::Voided { target } => {
                // Void the target. If the target is itself a ruling, supersede it:
                // voiding an adjust cancels its re-time (revert to the raw `at`);
                // voiding a void un-voids *that* void's target.
                match entries.get(target) {
                    Some(Entry::Adjusted {
                        target: inner_target,
                        ..
                    }) => {
                        // Cancel the adjust: drop its re-time so the inner target
                        // reverts to its original timestamp, and leave the inner
                        // target present (the adjust, not the pass, was voided).
                        retime.remove(inner_target);
                    }
                    Some(Entry::Voided {
                        target: inner_target,
                    }) => {
                        // Void the void: resurrect the originally-voided target.
                        voided.insert(*inner_target, false);
                    }
                    // Voiding a raw pass or an inserted pass simply drops it.
                    _ => {
                        voided.insert(*target, true);
                    }
                }
            }
        }
    }

    // Emit the surviving passes (raw + inserted) with any re-time applied, in offset
    // order, each paired with the global offset that addresses it for a future correction;
    // callers re-group and re-order them as needed.
    let mut out: Vec<(u64, Pass)> = Vec::new();
    for (offset, entry) in entries.iter() {
        if voided.get(offset).copied().unwrap_or(false) {
            continue;
        }
        match entry {
            Entry::RawPass(pass) => {
                let mut p = (*pass).clone();
                if let Some(at) = retime.get(offset) {
                    p.at = *at;
                }
                out.push((*offset, p));
            }
            Entry::Inserted(pass) => {
                let mut p = pass.clone();
                if let Some(at) = retime.get(offset) {
                    p.at = *at;
                }
                out.push((*offset, p));
            }
            Entry::Split { target, at } => {
                // Attribute the synthetic mid-lap pass to the competitor whose lap ends at
                // `target` (the target pass's adapter/competitor). A later adjust of *this*
                // split's offset re-times the synthetic pass; a void drops it. If the target
                // pass is unknown (a dangling ref — the command layer rejects these), skip.
                // The synthetic pass is addressable by *this* split's own offset.
                let src = match entries.get(target) {
                    Some(Entry::RawPass(p)) => Some((*p).clone()),
                    Some(Entry::Inserted(p)) => Some(p.clone()),
                    _ => None,
                };
                if let Some(src) = src {
                    out.push((
                        *offset,
                        Pass {
                            adapter: src.adapter,
                            competitor: src.competitor,
                            at: retime.get(offset).copied().unwrap_or(*at),
                            sequence: None,
                            gate: gridfpv_events::GateIndex::LAP,
                            signal: None,
                        },
                    ));
                }
            }
            Entry::Adjusted { .. } | Entry::Voided { .. } => {}
        }
    }
    out
}

/// Fold a sequence of `(offset, event)` pairs into the lap-list read model,
/// applying marshaling adjudications keyed on the target's append **offset** (#31).
///
/// This is the marshaling-aware sibling of [`lap_list`]. It is a thin consumer of
/// [`corrected_passes`] — the single home of the void/insert/adjust fold (#39): it
/// takes that corrected lap-gate pass stream, groups it by `(adapter, competitor)`,
/// orders each group, and derives laps exactly like [`lap_list`]. The raw [`Pass`]es
/// in the input are never mutated (architecture.html §3).
///
/// See [`corrected_passes`] for the adjudications folded, the offset/last-writer-wins
/// semantics, and the "void the void" cases.
pub fn lap_list_marshaled<'a, I>(events: I) -> LapList
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    // Group the corrected pass stream by competitor and derive laps. The fold itself
    // lives in `corrected_passes`; here we only project it into the lap-list view. Each
    // pass keeps the global offset that addresses it, so the derived laps carry their
    // `start_ref`/`end_ref` command targets.
    let mut by_competitor: BTreeMap<CompetitorKey, Vec<(u64, Pass)>> = BTreeMap::new();
    for (offset, pass) in corrected_passes(events) {
        by_competitor
            .entry(CompetitorKey::from_pass(&pass))
            .or_default()
            .push((offset, pass));
    }

    let competitors = by_competitor
        .into_iter()
        .map(|(competitor, mut passes)| {
            passes.sort_by_key(|(_, p)| corrected_order_key(p));
            CompetitorLaps {
                competitor,
                laps: laps_from_corrected(&passes),
            }
        })
        .collect();

    LapList { competitors }
}

/// Ordering key for a corrected pass.
///
/// A *corrected view* is a single coherent timeline: a re-timed pass moves to its
/// new instant and a synthetic inserted pass slots in chronologically, so the
/// view is ordered by `at` first. `sequence` is only a tie-break for passes that
/// share a timestamp (sequenced passes ahead of unsequenced, then by sequence),
/// keeping the fold deterministic. This subsumes the un-marshaled rule from
/// [`lap_list`]: when there are no rulings, a source either numbers its passes
/// monotonically in step with `at` or carries no sequence at all, so ordering by
/// `at` yields the same lap list.
fn corrected_order_key(pass: &Pass) -> (SourceTime, bool, Option<u64>) {
    (pass.at, pass.sequence.is_none(), pass.sequence)
}

/// Turn an ordered run of corrected lap-gate passes (each carrying its global offset) into
/// laps: `K` passes ⇒ `K - 1` laps, each spanning a consecutive pair. The lap's
/// `start_ref`/`end_ref` are the global offsets of the opening/closing pass — the stable
/// command targets a UI uses to address this lap (the end pass is the natural target for
/// void/adjust/split).
fn laps_from_corrected(passes: &[(u64, Pass)]) -> Vec<Lap> {
    passes
        .windows(2)
        .enumerate()
        .map(|(idx, pair)| {
            let (start_off, start_pass) = &pair[0];
            let (end_off, end_pass) = &pair[1];
            Lap {
                number: (idx + 1) as u32,
                duration_micros: end_pass.at.micros_since(start_pass.at),
                at: end_pass.at,
                start_ref: LogRef(*start_off),
                end_ref: LogRef(*end_off),
            }
        })
        .collect()
}

/// The kind of a marshaling audit entry — *what sort of action* a logged fact was (#55).
///
/// Derived purely from the event **type**, this is the "defensible results" surface: every
/// marshaling correction is a recorded, attributable, reversible fact (marshaling.html §3.3).
/// The automatic timer pass is included as [`AuditKind::Pass`] so the panel can distinguish a
/// *human ruling* from an *automatic detection* — but note `marshaling_log` only folds the
/// human rulings into entries (a heat has far too many passes to list); `Pass` exists so a
/// future "show automatic detections too" toggle is an additive consumer, not a model change.
///
/// There is deliberately **no actor / who**: per the no-login decision every change is implicitly
/// the RD, so naming an actor would be a false precision (marshaling.html §3.3, the audit records
/// what-changed-when, and the single-RD trust model supplies the who).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum AuditKind {
    /// A detection was voided (a phantom lap removed, or a "void the void" undo).
    Voided,
    /// A missed lap was inserted.
    Inserted,
    /// A lap's time was adjusted (re-timed).
    Adjusted,
    /// An over-long lap was split into two.
    Split,
    /// A penalty (DQ, added time, or points) was applied to a competitor.
    PenaltyApplied,
    /// A valid lap was thrown out of a competitor's scored count (not a void — the lap stays real).
    LapThrownOut,
    /// A protest was filed against a heat result.
    ProtestFiled,
    /// A filed protest was resolved (upheld / denied / withdrawn).
    ProtestResolved,
    /// A prior ruling (a penalty, throw-out, protest resolution, or heat-void) was reversed.
    RulingReversed,
    /// The whole heat was voided.
    HeatVoided,
    /// An automatic timer detection (NOT a marshal action). Folded only when a consumer asks for
    /// the raw stream; `marshaling_log` omits these so the audit reads as a ruling history.
    Pass,
}

/// A single reverse-chronological marshaling audit entry (#55): *what changed, when, what kind*.
///
/// A thin, render-ready fact derived from one logged marshaling event. `at` is the event's
/// `recorded_at` (the server wall-clock instant the log received it), so the panel can show
/// "when"; `summary` is a short human string ("Lap split", "DQ applied"); `kind` drives the visual
/// treatment. There is no actor field by design (see [`AuditKind`]).
///
/// `competitor` carries the **structured** competitor ref the action targeted (when it targets one),
/// kept *out* of `summary` on purpose: the ref is a source-local handle (a pilot id, a node seat),
/// not a friendly name, so the client resolves it to the pilot's **callsign** and composes the final
/// line (e.g. prepends "Ace · " to "DQ applied"). A server-baked `summary` cannot be re-resolved, so
/// the name lives in this field, not in the string (the Marshaling raw-id bug, #214 follow-up). It is
/// `None` for the lap-/heat-addressed actions that name no competitor (void, split, heat-void, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct AuditEntry {
    /// What kind of marshaling action this was — derived from the event type.
    pub kind: AuditKind,
    /// When the log received this fact (microseconds since the Unix epoch), if recorded.
    /// `None` when the append carried no arrival timestamp (e.g. a replay with none supplied).
    #[ts(type = "number | null")]
    pub at: Option<i64>,
    /// The global append offset of this fact — a stable identity for the entry (and what a
    /// later "reverse this" would target). Lets the UI key the list deterministically.
    pub at_ref: LogRef,
    /// The competitor this action targeted, as a **structured ref** the client resolves to a
    /// callsign and composes into the displayed line. `None` for actions that name no competitor.
    pub competitor: Option<CompetitorRef>,
    /// A short human-readable description of the change — **without** the raw competitor ref (that
    /// is carried structured in `competitor` so the client can show the resolved callsign instead).
    pub summary: String,
}

/// Fold a **heat-scoped** sequence of `(recorded_at, offset, &Event)` into the marshaling
/// audit trail (#55), newest first.
///
/// This is the "defensible results" panel's projection: it walks the heat's events and emits one
/// [`AuditEntry`] per **marshaling action** (the human rulings — void/insert/adjust/split, penalty,
/// reversal, heat-void), in **reverse-chronological** (offset-descending) order. Automatic passes
/// are *not* emitted (a heat has too many to list, and they are not rulings); they fold into the
/// lap list instead. The fold is pure and deterministic — folding the same heat window twice yields
/// the same trail — so it recomputes from the log like every other projection.
///
/// `events` must already be scoped to the heat (e.g. via the server's heat window) so the audit
/// reflects only this heat's rulings; `heat` is carried so the heat-addressed rulings
/// ([`Event::PenaltyApplied`], [`Event::HeatVoided`]) that name a *different* heat are excluded
/// even if they happen to fall inside the window.
pub fn marshaling_log<'a, I>(events: I, heat: &HeatId) -> Vec<AuditEntry>
where
    I: IntoIterator<Item = (Option<i64>, u64, &'a Event)>,
{
    let mut entries: Vec<AuditEntry> = Vec::new();
    for (at, offset, event) in events {
        // `competitor` carries the structured ref the client resolves to a callsign; it is kept OUT
        // of `summary` (the server can't resolve a friendly name; the client composes the final line).
        let (kind, competitor, summary) = match event {
            Event::DetectionVoided { target } => (
                AuditKind::Voided,
                None,
                format!("Detection voided (ref {})", target.0),
            ),
            Event::LapInserted {
                competitor, at: t, ..
            } => (
                AuditKind::Inserted,
                Some(competitor.clone()),
                format!("Lap inserted at {}", fmt_secs(*t)),
            ),
            Event::LapAdjusted { target, at: t } => (
                AuditKind::Adjusted,
                None,
                format!("Lap re-timed (ref {}) to {}", target.0, fmt_secs(*t)),
            ),
            Event::LapSplit { target, at: t } => (
                AuditKind::Split,
                None,
                format!("Lap split (ref {}) at {}", target.0, fmt_secs(*t)),
            ),
            Event::PenaltyApplied {
                heat: h,
                competitor,
                penalty,
            } if h == heat => (
                AuditKind::PenaltyApplied,
                Some(competitor.clone()),
                fmt_penalty(penalty),
            ),
            Event::LapThrownOut { target } => (
                AuditKind::LapThrownOut,
                None,
                format!("Lap thrown out (ref {})", target.0),
            ),
            Event::ProtestFiled {
                heat: h,
                competitor,
                note,
            } if h == heat => (
                AuditKind::ProtestFiled,
                Some(competitor.clone()),
                format!("Protest filed: {note}"),
            ),
            Event::ProtestResolved { target, outcome } => (
                AuditKind::ProtestResolved,
                None,
                format!("Protest {} (ref {})", fmt_outcome(*outcome), target.0),
            ),
            Event::RulingReversed { target } => (
                AuditKind::RulingReversed,
                None,
                format!("Ruling reversed (ref {})", target.0),
            ),
            Event::HeatVoided { heat: h } if h == heat => {
                (AuditKind::HeatVoided, None, "Heat voided".to_string())
            }
            // Passes and lifecycle/heat-loop events are not marshaling actions — skip them.
            _ => continue,
        };
        entries.push(AuditEntry {
            kind,
            at,
            at_ref: LogRef(offset),
            competitor,
            summary,
        });
    }
    // Reverse-chronological: newest action first. Offset is append order, so descending offset
    // is descending time.
    entries.reverse();
    entries
}

/// Format a `SourceTime` as whole seconds for an audit summary ("4.000s").
fn fmt_secs(t: SourceTime) -> String {
    let micros = t.micros_since(SourceTime::from_micros(0));
    format!("{:.3}s", micros as f64 / 1_000_000.0)
}

/// Format a [`Penalty`](gridfpv_events::Penalty) for an audit summary.
fn fmt_penalty(penalty: &gridfpv_events::Penalty) -> String {
    match penalty {
        gridfpv_events::Penalty::Disqualify { reason } => match reason {
            Some(r) => format!("DQ applied ({r})"),
            None => "DQ applied".to_string(),
        },
        gridfpv_events::Penalty::TimeAdded { micros } => {
            format!("+{:.3}s penalty", *micros as f64 / 1_000_000.0)
        }
        gridfpv_events::Penalty::PointsDeducted { points } => {
            format!("-{points} points")
        }
        gridfpv_events::Penalty::PointsAdded { points } => {
            format!("+{points} points")
        }
    }
}

/// Format a [`ProtestOutcome`](gridfpv_events::ProtestOutcome) for an audit summary.
fn fmt_outcome(outcome: gridfpv_events::ProtestOutcome) -> &'static str {
    match outcome {
        gridfpv_events::ProtestOutcome::Upheld => "upheld",
        gridfpv_events::ProtestOutcome::Denied => "denied",
        gridfpv_events::ProtestOutcome::Withdrawn => "withdrawn",
    }
}

// --- Signal trace (marshaling Slice 1 — signal-as-evidence) -----------------------------------

/// One competitor's reconstructed RSSI trace within a heat: the concatenated samples plus the
/// enter/exit thresholds the timer detected against (marshaling.html §3.2).
///
/// The `samples` are the per-tick RSSI values in capture order; `from`/`period_micros` carry the
/// time base of the **first** chunk so a UI can place each sample on the source clock (chunks are
/// captured back-to-back at a fixed cadence, so the first chunk's base plus the running index
/// reconstructs every sample's time — see [`signal_trace`] for the contiguity it assumes).
/// `enter`/`exit` are the last thresholds seen for this competitor, `None` until one is captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CompetitorTrace {
    /// Which source-local competitor this trace belongs to.
    pub competitor: CompetitorKey,
    /// The source-clock timestamp of the first captured sample, if any.
    #[ts(optional)]
    pub from: Option<SourceTime>,
    /// Microseconds between consecutive samples (the capture cadence of the first chunk).
    #[ts(type = "number")]
    pub period_micros: u32,
    /// The concatenated per-tick RSSI samples (filtered ADC counts), oldest first.
    pub samples: Vec<u16>,
    /// The enter detection threshold, where captured.
    #[ts(optional)]
    pub enter: Option<u16>,
    /// The exit detection threshold, where captured.
    #[ts(optional)]
    pub exit: Option<u16>,
}

/// The signal-trace read model for a heat: one [`CompetitorTrace`] per competitor that produced
/// any signal facts, ordered deterministically by [`CompetitorKey`] (marshaling Slice 1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalTraceView {
    /// Per-competitor traces, ordered by competitor key.
    pub competitors: Vec<CompetitorTrace>,
}

impl SignalTraceView {
    /// Look up a single competitor's trace by key, if present.
    pub fn competitor(&self, key: &CompetitorKey) -> Option<&CompetitorTrace> {
        self.competitors.iter().find(|c| &c.competitor == key)
    }
}

/// Fold a sequence of events into the per-heat [`SignalTraceView`] (marshaling Slice 1; dense
/// upgrade — RH `current_marshal_data`).
///
/// Folds [`Event::SignalChunk`], [`Event::SignalThresholds`], and [`Event::SignalHistory`].
/// Thresholds are **last-writer-wins** per competitor. The two trace sources are handled with a
/// **prefer-dense** rule:
///
/// - [`Event::SignalChunk`] — the *coarse* live-streamed samples (one per `node_data` heartbeat).
///   They **append** to the competitor's running buffer in log order, reconstructing the stream
///   exactly.
/// - [`Event::SignalHistory`] — the *dense, full-fidelity* per-tick history RotorHazard records,
///   pulled from the request-driven `current_marshal_data` at heat end. When a competitor has **any**
///   dense history, it **supersedes** the coarse chunk samples entirely: the view carries the dense
///   trace, not the streaming approximation. Multiple histories for one competitor are last-writer-
///   wins (a re-pull replaces the earlier one). With no dense history the coarse chunks stand, so a
///   heat that ended before the pull (or a non-RH source) is unchanged.
///
/// Because the dense history carries an **explicit per-sample time** (not a uniform cadence), the
/// view's uniform `from`/`period_micros` grid is derived from those times: `from` is the first
/// sample's instant and `period_micros` is the **first inter-sample delta** (RotorHazard samples at
/// a near-fixed rate). The samples themselves are stored verbatim — native integer ADC counts, no
/// resampling (the Slice 1 fidelity caution) — so the rendered grid is faithful to the real sample
/// count even where the cadence drifts slightly. A single-sample history keeps a `period_micros` of
/// `0` (degenerate, but the grid still places it at `from`).
///
/// Pure and deterministic — no clock, no hidden state — so folding the same events twice yields the
/// identical view (the determinism-on-replay guarantee Slice 1 is built around): the dense/coarse
/// choice is a pure function of which facts are present, independent of fold order.
///
/// `events` is the heat's window (the server scopes it before folding, exactly as the lap/audit
/// projections do); the fold itself is window-agnostic, so it is equally correct over the full log.
pub fn signal_trace<'a, I>(events: I) -> SignalTraceView
where
    I: IntoIterator<Item = &'a Event>,
{
    // Per competitor, in first-seen order; sorted by key at the end for a stable view.
    struct Acc {
        from: Option<SourceTime>,
        period_micros: u32,
        samples: Vec<u16>,
        enter: Option<u16>,
        exit: Option<u16>,
        /// The dense history that supersedes the coarse `samples`/`from`/`period_micros`, if any has
        /// been seen (last writer wins). When present it is resolved into the trace at emit time.
        dense: Option<SignalHistory>,
    }
    impl Acc {
        fn empty() -> Self {
            Acc {
                from: None,
                period_micros: 0,
                samples: Vec::new(),
                enter: None,
                exit: None,
                dense: None,
            }
        }
    }
    let mut by_competitor: BTreeMap<CompetitorKey, Acc> = BTreeMap::new();

    for event in events {
        match event {
            Event::SignalChunk(chunk) => {
                let key = CompetitorKey {
                    adapter: chunk.adapter.clone(),
                    competitor: chunk.competitor.clone(),
                };
                let acc = by_competitor.entry(key).or_insert_with(Acc::empty);
                // The first chunk anchors the time base; later chunks append onto it.
                if acc.from.is_none() {
                    acc.from = Some(chunk.from);
                    acc.period_micros = chunk.period_micros;
                }
                acc.samples.extend_from_slice(&chunk.rssi);
            }
            Event::SignalHistory(history) => {
                let key = CompetitorKey {
                    adapter: history.adapter.clone(),
                    competitor: history.competitor.clone(),
                };
                let acc = by_competitor.entry(key).or_insert_with(Acc::empty);
                // Prefer-dense: the dense history supersedes the coarse chunks. Last writer wins, so
                // a re-pull replaces the earlier history. An empty history is ignored (it carries no
                // evidence, so it must not blank out the coarse trace).
                if !history.rssi.is_empty() {
                    acc.dense = Some(history.clone());
                }
            }
            Event::SignalThresholds(t) => {
                let key = CompetitorKey {
                    adapter: t.adapter.clone(),
                    competitor: t.competitor.clone(),
                };
                let acc = by_competitor.entry(key).or_insert_with(Acc::empty);
                // Last writer wins.
                acc.enter = Some(t.enter);
                acc.exit = Some(t.exit);
            }
            _ => {}
        }
    }

    SignalTraceView {
        competitors: by_competitor
            .into_iter()
            .map(|(competitor, acc)| {
                // Prefer-dense: when a dense history is present it replaces the coarse stream; the
                // uniform `from`/`period_micros` grid is derived from the history's explicit times.
                let (from, period_micros, samples) = match acc.dense {
                    Some(history) => dense_trace_grid(&history),
                    None => (acc.from, acc.period_micros, acc.samples),
                };
                CompetitorTrace {
                    competitor,
                    from,
                    period_micros,
                    samples,
                    enter: acc.enter,
                    exit: acc.exit,
                }
            })
            .collect(),
    }
}

/// Resolve a dense [`SignalHistory`] into the `(from, period_micros, samples)` the uniform-grid
/// [`CompetitorTrace`] carries, preserving the native samples verbatim (no resampling).
///
/// `from` is the first sample's instant; `period_micros` is the first inter-sample delta (RH samples
/// at a near-fixed rate, so this anchors the grid the renderer draws on). The `samples` are the
/// dense RSSI vector unchanged. A single-sample history yields `period_micros = 0`. Times that are
/// non-monotonic or out of range are clamped to a non-negative `u32` delta so the grid stays valid.
fn dense_trace_grid(history: &SignalHistory) -> (Option<SourceTime>, u32, Vec<u16>) {
    let from = history.times.first().copied().map(SourceTime::from_micros);
    let period_micros = match history.times.get(..2) {
        Some([a, b]) => (b - a).clamp(0, u32::MAX as i64) as u32,
        _ => 0,
    };
    (from, period_micros, history.rssi.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{GateIndex, SessionId, SignalContext};

    /// Build a lap-gate pass with the given competitor, timestamp and sequence.
    fn pass(adapter: &str, competitor: &str, at: i64, sequence: Option<u64>) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    /// Build a split (non-lap-gate) pass.
    fn split(adapter: &str, competitor: &str, at: i64, gate: u32) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: None,
            gate: GateIndex(gate),
            signal: Some(SignalContext { rssi_peak: None }),
        })
    }

    fn key(adapter: &str, competitor: &str) -> CompetitorKey {
        CompetitorKey {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
        }
    }

    /// Expected `(lap number, duration)` pair — the marshaling-irrelevant lap shape these
    /// fold tests assert. Laps now also carry `start_ref`/`end_ref` global offsets (#55);
    /// those are exercised by dedicated offset-targeting tests, so the duration folds compare
    /// on `(number, duration)` via [`bare`] to stay focused on the fold arithmetic.
    fn ld(number: u32, duration_micros: i64) -> (u32, i64) {
        (number, duration_micros)
    }

    /// Project laps to their `(number, duration)` pairs, dropping the ref offsets.
    fn bare(laps: &[Lap]) -> Vec<(u32, i64)> {
        laps.iter().map(|l| (l.number, l.duration_micros)).collect()
    }

    #[test]
    fn clean_multi_lap_run_yields_k_minus_one_laps() {
        // 4 lap-gate passes ⇒ 3 laps with exact integer-microsecond durations.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "A", 6_500_000, Some(3)),
            pass("vd", "A", 11_000_000, Some(4)),
        ];
        let result = lap_list(&events);
        let laps = &result.competitor(&key("vd", "A")).unwrap().laps;
        assert_eq!(
            bare(laps),
            vec![ld(1, 3_000_000), ld(2, 2_500_000), ld(3, 4_500_000),]
        );
        let cl = result.competitor(&key("vd", "A")).unwrap();
        assert_eq!(cl.lap_count(), 3);
        assert_eq!(cl.total_micros(), 10_000_000);
        assert_eq!(cl.best(), Some(&laps[1]));
    }

    #[test]
    fn single_pass_yields_zero_laps() {
        let events = vec![pass("vd", "A", 1_000_000, Some(1))];
        let result = lap_list(&events);
        let cl = result.competitor(&key("vd", "A")).unwrap();
        assert_eq!(bare(&cl.laps), vec![]);
        assert_eq!(cl.lap_count(), 0);
        assert_eq!(cl.total_micros(), 0);
        assert_eq!(cl.best(), None);
    }

    #[test]
    fn empty_log_yields_empty_lap_list() {
        let events: Vec<Event> = vec![];
        assert_eq!(lap_list(&events), LapList::default());
    }

    #[test]
    fn multiple_competitors_interleaved_are_grouped_independently() {
        // Two competitors on the same adapter, passes interleaved in the log.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "B", 1_500_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "B", 5_500_000, Some(2)),
            pass("vd", "A", 6_000_000, Some(3)),
        ];
        let result = lap_list(&events);

        let a = result.competitor(&key("vd", "A")).unwrap();
        assert_eq!(bare(&a.laps), vec![ld(1, 3_000_000), ld(2, 2_000_000),]);

        let b = result.competitor(&key("vd", "B")).unwrap();
        assert_eq!(bare(&b.laps), vec![ld(1, 4_000_000)]);
    }

    #[test]
    fn same_ref_on_different_adapters_is_two_competitors() {
        // CompetitorRef is per-source: "node-2" on two adapters never merges.
        let events = vec![
            pass("rh-a", "node-2", 0, Some(1)),
            pass("rh-a", "node-2", 2_000_000, Some(2)),
            pass("rh-b", "node-2", 0, Some(1)),
            pass("rh-b", "node-2", 3_000_000, Some(2)),
        ];
        let result = lap_list(&events);
        assert_eq!(result.competitors.len(), 2);
        assert_eq!(
            bare(&result.competitor(&key("rh-a", "node-2")).unwrap().laps),
            vec![ld(1, 2_000_000)]
        );
        assert_eq!(
            bare(&result.competitor(&key("rh-b", "node-2")).unwrap().laps),
            vec![ld(1, 3_000_000)]
        );
    }

    #[test]
    fn split_passes_are_ignored() {
        // Splits between lap-gate passes must not become laps or shift durations.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            split("vd", "A", 2_000_000, 1),
            split("vd", "A", 3_000_000, 2),
            pass("vd", "A", 5_000_000, Some(2)),
        ];
        let result = lap_list(&events);
        assert_eq!(
            bare(&result.competitor(&key("vd", "A")).unwrap().laps),
            vec![ld(1, 4_000_000)]
        );
    }

    #[test]
    fn lifecycle_events_are_ignored() {
        let events = vec![
            Event::AdapterConnected {
                adapter: AdapterId("vd".into()),
            },
            Event::SessionStarted {
                adapter: AdapterId("vd".into()),
                session: SessionId("heat-1".into()),
            },
            Event::CompetitorSeen {
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
            },
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 3_000_000, Some(2)),
            Event::SessionEnded {
                adapter: AdapterId("vd".into()),
                session: SessionId("heat-1".into()),
            },
        ];
        let result = lap_list(&events);
        assert_eq!(
            bare(&result.competitor(&key("vd", "A")).unwrap().laps),
            vec![ld(1, 2_000_000)]
        );
    }

    #[test]
    fn passes_are_ordered_by_sequence_not_log_order() {
        // Out-of-order arrival, but sequence is authoritative: 1 -> 2 -> 3.
        // Timestamps deliberately disagree with log order to prove sequence wins.
        let events = vec![
            pass("vd", "A", 6_000_000, Some(3)),
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
        ];
        let result = lap_list(&events);
        assert_eq!(
            bare(&result.competitor(&key("vd", "A")).unwrap().laps),
            vec![ld(1, 3_000_000), ld(2, 2_000_000),]
        );
    }

    #[test]
    fn passes_without_sequence_are_ordered_by_timestamp() {
        // No sequence anywhere: fall back to `at` ascending despite log order.
        let events = vec![
            pass("vd", "A", 7_000_000, None),
            pass("vd", "A", 2_000_000, None),
            pass("vd", "A", 5_000_000, None),
        ];
        let result = lap_list(&events);
        assert_eq!(
            bare(&result.competitor(&key("vd", "A")).unwrap().laps),
            vec![ld(1, 3_000_000), ld(2, 2_000_000),]
        );
    }

    #[test]
    fn registrations_map_bindings_last_writer_wins() {
        use gridfpv_events::PilotId;
        let events = vec![
            Event::CompetitorRegistered {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: PilotId("acroace".into()),
            },
            // A different competitor binds to a different pilot.
            Event::CompetitorRegistered {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-3".into()),
                pilot: PilotId("bee".into()),
            },
            // node-2 is re-bound: the later registration wins.
            Event::CompetitorRegistered {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: PilotId("zoomer".into()),
            },
        ];
        let map = registrations(&events);
        assert_eq!(
            map.get(&key("rh", "node-2")),
            Some(&PilotId("zoomer".into()))
        );
        assert_eq!(map.get(&key("rh", "node-3")), Some(&PilotId("bee".into())));
        // An unregistered competitor is simply absent.
        assert_eq!(map.get(&key("rh", "node-9")), None);
    }

    #[test]
    fn registrations_are_per_source() {
        use gridfpv_events::PilotId;
        // The same ref on two adapters is two distinct bindings.
        let events = vec![
            Event::CompetitorRegistered {
                adapter: AdapterId("rh-a".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: PilotId("acroace".into()),
            },
            Event::CompetitorRegistered {
                adapter: AdapterId("rh-b".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: PilotId("bee".into()),
            },
        ];
        let map = registrations(&events);
        assert_eq!(
            map.get(&key("rh-a", "node-2")),
            Some(&PilotId("acroace".into()))
        );
        assert_eq!(
            map.get(&key("rh-b", "node-2")),
            Some(&PilotId("bee".into()))
        );
    }

    #[test]
    fn lap_list_serde_round_trips() {
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 3_500_000, Some(2)),
        ];
        let result = lap_list(&events);
        let json = serde_json::to_string(&result).unwrap();
        let back: LapList = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
    }
}

/// Marshaling fold golden cases (#31): hand-authored `(offset, Event)` logs with
/// explicit offsets, one per adjudication, asserting the corrected [`LapList`].
#[cfg(test)]
mod marshaling_tests {
    use super::*;
    use gridfpv_events::{GateIndex, HeatId, LogRef, Penalty};

    /// Build a lap-gate pass event.
    fn pass(adapter: &str, competitor: &str, at: i64, sequence: Option<u64>) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    fn voided(target: u64) -> Event {
        Event::DetectionVoided {
            target: LogRef(target),
        }
    }

    fn inserted(adapter: &str, competitor: &str, at: i64) -> Event {
        Event::LapInserted {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
        }
    }

    fn adjusted(target: u64, at: i64) -> Event {
        Event::LapAdjusted {
            target: LogRef(target),
            at: SourceTime::from_micros(at),
        }
    }

    fn split(target: u64, at: i64) -> Event {
        Event::LapSplit {
            target: LogRef(target),
            at: SourceTime::from_micros(at),
        }
    }

    fn key(adapter: &str, competitor: &str) -> CompetitorKey {
        CompetitorKey {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
        }
    }

    /// Tag a log with positional offsets (0, 1, 2, …) — the storage layer assigns
    /// the same dense append offsets, so this mirrors a real on-disk log.
    fn tagged(events: &[Event]) -> Vec<(u64, &Event)> {
        events
            .iter()
            .enumerate()
            .map(|(i, e)| (i as u64, e))
            .collect()
    }

    /// Expected `(lap number, duration)` pair. Laps also carry `start_ref`/`end_ref` offsets
    /// (#55); these fold goldens assert the duration arithmetic via [`laps_of`], and the
    /// offset-targeting behaviour is checked by the dedicated `*_ref*` tests below.
    fn ld(number: u32, duration_micros: i64) -> (u32, i64) {
        (number, duration_micros)
    }

    /// A competitor's laps as `(number, duration)` pairs, dropping the ref offsets — the fold
    /// goldens compare on lap arithmetic only.
    fn laps_of(list: &LapList, adapter: &str, competitor: &str) -> Vec<(u32, i64)> {
        list.competitor(&key(adapter, competitor))
            .map(|c| {
                c.laps
                    .iter()
                    .map(|l| (l.number, l.duration_micros))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A competitor's raw laps (with refs intact), for the offset-targeting tests.
    fn raw_laps_of(list: &LapList, adapter: &str, competitor: &str) -> Vec<Lap> {
        list.competitor(&key(adapter, competitor))
            .map(|c| c.laps.clone())
            .unwrap_or_default()
    }

    #[test]
    fn no_adjudications_matches_lap_list() {
        // With no rulings, `lap_list_marshaled` projects identically to `lap_list`.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "A", 6_500_000, Some(3)),
        ];
        assert_eq!(lap_list_marshaled(tagged(&events)), lap_list(&events));
    }

    #[test]
    fn detection_voided_drops_the_targeted_pass() {
        // Three raw passes; the middle one (offset 1) is a phantom and is voided.
        // The corrected view is just passes 0 and 2 ⇒ a single lap spanning them.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1 — phantom
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
            voided(1),                           // offset 3 — voids the phantom
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 5_000_000)]);
    }

    #[test]
    fn lap_inserted_adds_a_synthetic_pass() {
        // Two raw passes; a missed lap is recovered by inserting a pass between them.
        // The synthetic pass at 4.0s splits the 6.0s span into two laps.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1
            inserted("vd", "A", 4_000_000),      // offset 2 — recovered lap
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![ld(1, 3_000_000), ld(2, 3_000_000),]
        );
    }

    #[test]
    fn lap_adjusted_retimes_the_targeted_pass() {
        // The middle pass was detected late; re-time offset 1 from 5.0s to 4.0s.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1 — detected late
            pass("vd", "A", 7_000_000, Some(3)), // offset 2
            adjusted(1, 4_000_000),              // offset 3 — re-time to 4.0s
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![ld(1, 3_000_000), ld(2, 3_000_000),]
        );
    }

    #[test]
    fn last_writer_wins_void_supersedes_adjust() {
        // offset 1 is adjusted (offset 3) and then voided (offset 4): the later void
        // wins, so the pass is gone entirely — not merely re-timed.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1
            pass("vd", "A", 8_000_000, Some(3)), // offset 2
            adjusted(1, 4_000_000),              // offset 3 — re-time...
            voided(1),                           // offset 4 — ...then void (wins)
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 7_000_000)]);
    }

    #[test]
    fn void_the_void_resurrects_the_original_pass() {
        // architecture.html §3 "void the void": offset 1 is voided (offset 3), then a
        // marshal realises that was wrong and voids *the void* (offset 4) — the
        // originally-voided pass comes back, so all three laps' passes are present.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
            voided(1),                           // offset 3 — void the pass
            voided(3),                           // offset 4 — void the void
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![ld(1, 3_000_000), ld(2, 2_000_000),]
        );
    }

    #[test]
    fn voiding_an_insert_removes_the_synthetic_pass() {
        // A void may target a `LapInserted` (an adjudication) — voiding offset 2
        // removes the synthetic lap again, leaving only the two raw passes.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1
            inserted("vd", "A", 4_000_000),      // offset 2 — synthetic
            voided(2),                           // offset 3 — void the insert
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 6_000_000)]);
    }

    #[test]
    fn voiding_an_adjust_reverts_to_the_raw_timestamp() {
        // A void targeting a `LapAdjusted` cancels the re-time: the target raw pass
        // reverts to its original timestamp rather than being dropped.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1 — original 5.0s
            adjusted(1, 4_000_000),              // offset 2 — re-time to 4.0s...
            voided(2),                           // offset 3 — ...cancel the re-time
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 4_000_000)]);
    }

    #[test]
    fn heat_and_result_level_rulings_are_ignored_by_the_lap_view() {
        // `HeatVoided` / `PenaltyApplied` are result-level — scoring consumes them,
        // not the lap list. They must not perturb the lap projection.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            Event::HeatVoided {
                heat: HeatId("q-1".into()),
            },
            Event::PenaltyApplied {
                heat: HeatId("q-1".into()),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::TimeAdded { micros: 2_000_000 },
            },
        ];
        assert_eq!(lap_list_marshaled(tagged(&events)), lap_list(&events));
        assert_eq!(
            laps_of(&lap_list_marshaled(tagged(&events)), "vd", "A"),
            vec![ld(1, 3_000_000)]
        );
    }

    #[test]
    fn fold_is_idempotent_recompute_equivalence() {
        // Folding the same log twice yields the same result — the projection is a
        // pure function of the log with no hidden state (recompute-equivalence).
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 5_000_000, Some(2)),
            pass("vd", "A", 8_000_000, Some(3)),
            adjusted(1, 4_000_000),
            voided(2),
            inserted("vd", "A", 9_000_000),
        ];
        let first = lap_list_marshaled(tagged(&events));
        let second = lap_list_marshaled(tagged(&events));
        assert_eq!(first, second);
    }

    #[test]
    fn raw_passes_are_byte_identical_before_and_after_folding() {
        // The fold builds a *corrected view*; it must never mutate the raw log. We
        // snapshot the raw `Pass`es' serialized bytes, fold (with adjudications), and
        // assert every raw pass round-trips byte-for-byte unchanged.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 5_000_000, Some(2)),
            pass("vd", "A", 8_000_000, Some(3)),
            adjusted(1, 4_000_000), // would "change" a pass if we mutated
            voided(2),              // would "drop" a pass if we mutated
        ];
        let before: Vec<String> = events
            .iter()
            .filter(|e| matches!(e, Event::Pass(_)))
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();

        // Fold — and use the result so the corrected view genuinely differs.
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 3_000_000)]);

        let after: Vec<String> = events
            .iter()
            .filter(|e| matches!(e, Event::Pass(_)))
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        assert_eq!(
            before, after,
            "raw passes must be byte-identical after folding"
        );
    }

    // --- LapSplit (Slice 2) ----------------------------------------------------

    #[test]
    fn lap_split_makes_two_laps_from_one() {
        // One over-long lap (1.0s → 7.0s) ending at the offset-1 pass; the timer missed a
        // mid-lap detection. Splitting it at 4.0s — attributed to the target's competitor —
        // turns the single 6.0s lap into two 3.0s laps.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1 — ends the over-long lap
            split(1, 4_000_000),                 // offset 2 — split at 4.0s
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![ld(1, 3_000_000), ld(2, 3_000_000),]
        );
    }

    #[test]
    fn lap_split_attributes_to_the_target_competitor_only() {
        // Two competitors interleaved; splitting B's lap must add a pass for B, never A.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "B", 2_000_000, Some(1)), // offset 1
            pass("vd", "A", 4_000_000, Some(2)), // offset 2
            pass("vd", "B", 8_000_000, Some(2)), // offset 3 — ends B's over-long lap
            split(3, 5_000_000),                 // offset 4 — split B's lap at 5.0s
        ];
        let result = lap_list_marshaled(tagged(&events));
        // A is untouched: one 3.0s lap.
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 3_000_000)]);
        // B's single 6.0s lap becomes two (2.0→5.0, 5.0→8.0).
        assert_eq!(
            laps_of(&result, "vd", "B"),
            vec![ld(1, 3_000_000), ld(2, 3_000_000),]
        );
    }

    #[test]
    fn void_the_split_removes_the_synthetic_pass() {
        // The split's synthetic pass is addressable by the split's own offset, so a later
        // void of that offset removes it — the single over-long lap is restored.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1
            split(1, 4_000_000),                 // offset 2 — synthetic mid-lap pass
            voided(2),                           // offset 3 — void the split
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(laps_of(&result, "vd", "A"), vec![ld(1, 6_000_000)]);
    }

    #[test]
    fn void_the_void_of_a_split_restores_it() {
        // "Void the void" works on a split: void the split (offset 3), then void *that*
        // void (offset 4) — the synthetic pass comes back and the lap is two again.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 7_000_000, Some(2)), // offset 1
            split(1, 4_000_000),                 // offset 2 — synthetic
            voided(2),                           // offset 3 — void the split
            voided(3),                           // offset 4 — void the void
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![ld(1, 3_000_000), ld(2, 3_000_000),]
        );
    }

    #[test]
    fn folding_the_same_events_twice_is_identical_with_a_split() {
        // Determinism-on-replay (mirrors `fold_is_idempotent_recompute_equivalence`): a log
        // mixing a split with the other rulings folds to the identical result twice over.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 7_000_000, Some(2)),
            pass("vd", "A", 10_000_000, Some(3)),
            split(1, 4_000_000),
            adjusted(2, 9_500_000),
            inserted("vd", "A", 12_000_000),
        ];
        let first = lap_list_marshaled(tagged(&events));
        let second = lap_list_marshaled(tagged(&events));
        assert_eq!(first, second);
    }

    #[test]
    fn edit_time_shifts_both_neighbour_lap_durations() {
        // Slice-2 "edit-time": re-timing a *middle* pass shifts BOTH adjacent lap durations
        // that share it — no new event, the duration recompute is structural in
        // `corrected_passes`. Verify the prior-lap and next-lap durations both change.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 5_000_000, Some(2)), // offset 1 — the shared middle pass
            pass("vd", "A", 9_000_000, Some(3)), // offset 2
        ];
        // Before the edit: laps are 4.0s (1→5) and 4.0s (5→9).
        let before = laps_of(&lap_list_marshaled(tagged(&events)), "vd", "A");
        assert_eq!(before[0].1, 4_000_000);
        assert_eq!(before[1].1, 4_000_000);

        // Re-time the middle pass from 5.0s to 6.0s — the prior lap lengthens and the next
        // lap shortens by the same 1.0s, both neighbours moving off one edit.
        let mut edited = events.clone();
        edited.push(adjusted(1, 6_000_000)); // offset 3
        let after = laps_of(&lap_list_marshaled(tagged(&edited)), "vd", "A");
        assert_eq!(after[0].1, 5_000_000, "prior lap lengthened 4→5s");
        assert_eq!(after[1].1, 3_000_000, "next lap shortened 4→3s");
    }

    #[test]
    fn adjust_targeting_a_synthetic_inserted_pass_retimes_it() {
        // An inserted lap is itself addressable; a later adjust re-times it.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 9_000_000, Some(2)), // offset 1
            inserted("vd", "A", 4_000_000),      // offset 2 — synthetic at 4.0s
            adjusted(2, 5_000_000),              // offset 3 — re-time insert to 5.0s
        ];
        let result = lap_list_marshaled(tagged(&events));
        assert_eq!(
            laps_of(&result, "vd", "A"),
            vec![ld(1, 4_000_000), ld(2, 4_000_000),]
        );
    }

    // --- Lap end_ref/start_ref: the load-bearing UI-targeting offsets (#55) ----------

    #[test]
    fn lap_refs_carry_the_global_pass_offsets() {
        // Each lap's start_ref/end_ref are the GLOBAL append offsets of its bounding passes —
        // the stable command target a UI selects. 3 passes at offsets 0,1,2 ⇒ laps
        // (start=0,end=1) and (start=1,end=2).
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
        ];
        let result = lap_list_marshaled(tagged(&events));
        let laps = raw_laps_of(&result, "vd", "A");
        assert_eq!(laps[0].start_ref, LogRef(0));
        assert_eq!(laps[0].end_ref, LogRef(1));
        assert_eq!(laps[1].start_ref, LogRef(1));
        assert_eq!(laps[1].end_ref, LogRef(2));
    }

    #[test]
    fn lap_refs_are_global_offsets_not_window_relative() {
        // THE BUG THIS FIXES: when the fold is fed real global offsets (a heat starting at
        // offset 100, e.g. the heat-window path), the lap refs are those global offsets — NOT
        // re-enumerated 0,1,2. A UI selecting lap 2's end targets global offset 102, and a void
        // of that offset removes the right pass.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),
            pass("vd", "A", 4_000_000, Some(2)),
            pass("vd", "A", 6_000_000, Some(3)),
        ];
        // Feed the fold global offsets 100,101,102 (as the heat window now does).
        let tagged_global: Vec<(u64, &Event)> = events
            .iter()
            .enumerate()
            .map(|(i, e)| (100 + i as u64, e))
            .collect();
        let result = lap_list_marshaled(tagged_global);
        let laps = raw_laps_of(&result, "vd", "A");
        assert_eq!(
            laps[1].end_ref,
            LogRef(102),
            "must be the global offset, not 2"
        );

        // And that ref is a valid void target: void lap 2's end pass (offset 102) and the lap is
        // gone — proving the UI-selected ref hits the RIGHT pass.
        let end_ref = laps[1].end_ref;
        let mut log: Vec<Event> = events.clone();
        // Append the void at its own real global offset; re-fold the same global window.
        let mut full: Vec<(u64, Event)> = log
            .drain(..)
            .enumerate()
            .map(|(i, e)| (100 + i as u64, e))
            .collect();
        full.push((103, Event::DetectionVoided { target: end_ref }));
        let refolded = lap_list_marshaled(full.iter().map(|(o, e)| (*o, e)));
        // Voiding the offset-102 pass leaves laps (100→101) only.
        assert_eq!(laps_of(&refolded, "vd", "A"), vec![ld(1, 3_000_000)]);
    }

    #[test]
    fn selecting_a_lap_to_split_targets_the_right_pass() {
        // A UI selects an over-long lap and splits it: the split must target that lap's END pass.
        // Two competitors interleaved with non-zero global offsets so a window-relative bug would
        // target the wrong pass.
        let events = [
            pass("vd", "A", 1_000_000, Some(1)), // global 50
            pass("vd", "B", 2_000_000, Some(1)), // global 51
            pass("vd", "A", 4_000_000, Some(2)), // global 52
            pass("vd", "B", 8_000_000, Some(2)), // global 53 — ends B's over-long lap
        ];
        let tagged_global: Vec<(u64, &Event)> = events
            .iter()
            .enumerate()
            .map(|(i, e)| (50 + i as u64, e))
            .collect();
        let list = lap_list_marshaled(tagged_global);
        // The UI selects B's only lap and reads its end_ref to target.
        let b_lap = raw_laps_of(&list, "vd", "B")[0].clone();
        assert_eq!(b_lap.end_ref, LogRef(53));

        // Split that lap at 5.0s, targeting end_ref — B's lap becomes two, A untouched.
        let mut full: Vec<(u64, Event)> = events
            .iter()
            .enumerate()
            .map(|(i, e)| (50 + i as u64, e.clone()))
            .collect();
        full.push((
            54,
            Event::LapSplit {
                target: b_lap.end_ref,
                at: SourceTime::from_micros(5_000_000),
            },
        ));
        let refolded = lap_list_marshaled(full.iter().map(|(o, e)| (*o, e)));
        assert_eq!(laps_of(&refolded, "vd", "A"), vec![ld(1, 3_000_000)]);
        assert_eq!(
            laps_of(&refolded, "vd", "B"),
            vec![ld(1, 3_000_000), ld(2, 3_000_000)]
        );
    }

    // --- marshaling_log: the audit projection (#55) ---------------------------------

    fn audit(events: &[(Option<i64>, Event)], heat: &str) -> Vec<AuditEntry> {
        let heat = HeatId(heat.into());
        let tagged: Vec<(Option<i64>, u64, &Event)> = events
            .iter()
            .enumerate()
            .map(|(i, (at, e))| (*at, i as u64, e))
            .collect();
        marshaling_log(tagged, &heat)
    }

    #[test]
    fn marshaling_log_lists_rulings_newest_first_no_passes() {
        let heat = "q-1";
        let log = vec![
            (Some(10), pass("vd", "A", 1_000_000, Some(1))), // offset 0 — automatic, excluded
            (Some(20), pass("vd", "A", 4_000_000, Some(2))), // offset 1 — automatic, excluded
            (Some(30), Event::DetectionVoided { target: LogRef(1) }), // offset 2
            (
                Some(40),
                Event::LapSplit {
                    target: LogRef(1),
                    at: SourceTime::from_micros(2_500_000),
                },
            ), // offset 3
        ];
        let entries = audit(&log, heat);
        // Only the two rulings appear, newest (split, offset 3) first.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, AuditKind::Split);
        assert_eq!(entries[0].at_ref, LogRef(3));
        assert_eq!(entries[0].at, Some(40));
        assert_eq!(entries[1].kind, AuditKind::Voided);
        assert_eq!(entries[1].at_ref, LogRef(2));
    }

    #[test]
    fn marshaling_log_covers_every_ruling_kind() {
        use gridfpv_events::Penalty;
        let heat = "q-1";
        let log = vec![
            (
                Some(1),
                Event::LapInserted {
                    adapter: AdapterId("vd".into()),
                    competitor: CompetitorRef("A".into()),
                    at: SourceTime::from_micros(3_000_000),
                },
            ),
            (
                Some(2),
                Event::LapAdjusted {
                    target: LogRef(0),
                    at: SourceTime::from_micros(4_000_000),
                },
            ),
            (
                Some(3),
                Event::PenaltyApplied {
                    heat: HeatId(heat.into()),
                    competitor: CompetitorRef("B".into()),
                    penalty: Penalty::Disqualify { reason: None },
                },
            ),
            (Some(4), Event::RulingReversed { target: LogRef(2) }),
            (
                Some(5),
                Event::HeatVoided {
                    heat: HeatId(heat.into()),
                },
            ),
        ];
        let kinds: Vec<AuditKind> = audit(&log, heat).into_iter().map(|e| e.kind).collect();
        // Newest first.
        assert_eq!(
            kinds,
            vec![
                AuditKind::HeatVoided,
                AuditKind::RulingReversed,
                AuditKind::PenaltyApplied,
                AuditKind::Adjusted,
                AuditKind::Inserted,
            ]
        );
    }

    #[test]
    fn marshaling_log_covers_slice6_ruling_kinds() {
        use gridfpv_events::{Penalty, ProtestOutcome};
        let heat = "q-1";
        let log = vec![
            (Some(1), Event::LapThrownOut { target: LogRef(0) }),
            (
                Some(2),
                Event::PenaltyApplied {
                    heat: HeatId(heat.into()),
                    competitor: CompetitorRef("A".into()),
                    penalty: Penalty::PointsDeducted { points: 5 },
                },
            ),
            (
                Some(3),
                Event::ProtestFiled {
                    heat: HeatId(heat.into()),
                    competitor: CompetitorRef("B".into()),
                    note: "cut the course".into(),
                },
            ),
            (
                Some(4),
                Event::ProtestResolved {
                    target: LogRef(2),
                    outcome: ProtestOutcome::Upheld,
                },
            ),
        ];
        let entries = audit(&log, heat);
        // Newest first.
        let kinds: Vec<AuditKind> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                AuditKind::ProtestResolved,
                AuditKind::ProtestFiled,
                AuditKind::PenaltyApplied,
                AuditKind::LapThrownOut,
            ]
        );
        // The summaries read cleanly: the points penalty, the protest text, the outcome.
        assert!(
            entries
                .iter()
                .any(|e| e.kind == AuditKind::PenaltyApplied && e.summary.contains("-5 points"))
        );
        assert!(
            entries
                .iter()
                .any(|e| e.kind == AuditKind::ProtestFiled && e.summary.contains("cut the course"))
        );
        assert!(
            entries
                .iter()
                .any(|e| e.kind == AuditKind::ProtestResolved && e.summary.contains("upheld"))
        );
        // Competitor-addressed actions carry the STRUCTURED ref (for client callsign resolution) and
        // keep it out of the summary string; the client composes the resolved name into the line.
        let penalty = entries
            .iter()
            .find(|e| e.kind == AuditKind::PenaltyApplied)
            .unwrap();
        assert_eq!(penalty.competitor, Some(CompetitorRef("A".into())));
        let protest = entries
            .iter()
            .find(|e| e.kind == AuditKind::ProtestFiled)
            .unwrap();
        assert_eq!(protest.competitor, Some(CompetitorRef("B".into())));
        assert!(!protest.summary.contains('B'));
        // Lap-/heat-addressed actions name no competitor.
        let resolved = entries
            .iter()
            .find(|e| e.kind == AuditKind::ProtestResolved)
            .unwrap();
        assert_eq!(resolved.competitor, None);
    }

    #[test]
    fn marshaling_log_excludes_other_heats_penalties_and_voids() {
        use gridfpv_events::Penalty;
        let heat = "q-1";
        let log = vec![
            (
                Some(1),
                Event::PenaltyApplied {
                    heat: HeatId("q-2".into()), // a DIFFERENT heat — excluded
                    competitor: CompetitorRef("B".into()),
                    penalty: Penalty::Disqualify { reason: None },
                },
            ),
            (
                Some(2),
                Event::HeatVoided {
                    heat: HeatId("q-2".into()),
                },
            ), // different heat — excluded
            (
                Some(3),
                Event::PenaltyApplied {
                    heat: HeatId(heat.into()),
                    competitor: CompetitorRef("A".into()),
                    penalty: Penalty::TimeAdded { micros: 2_000_000 },
                },
            ),
        ];
        let entries = audit(&log, heat);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, AuditKind::PenaltyApplied);
        assert!(entries[0].summary.contains("+2.000s"));
        // The competitor ref is carried STRUCTURED (for client callsign resolution), not baked into
        // the summary string — the summary holds only the penalty description.
        assert!(!entries[0].summary.contains('A'));
        assert_eq!(entries[0].competitor, Some(CompetitorRef("A".into())));
    }

    #[test]
    fn marshaling_log_is_deterministic_fold_twice() {
        use gridfpv_events::Penalty;
        let heat = "q-1";
        let log = vec![
            (
                Some(1),
                Event::LapInserted {
                    adapter: AdapterId("vd".into()),
                    competitor: CompetitorRef("A".into()),
                    at: SourceTime::from_micros(3_000_000),
                },
            ),
            (
                Some(2),
                Event::PenaltyApplied {
                    heat: HeatId(heat.into()),
                    competitor: CompetitorRef("B".into()),
                    penalty: Penalty::Disqualify { reason: None },
                },
            ),
            (Some(3), Event::RulingReversed { target: LogRef(1) }),
        ];
        assert_eq!(audit(&log, heat), audit(&log, heat));
    }

    // --- Signal trace (marshaling Slice 1) ----------------------------------------------------

    use gridfpv_events::{SignalChunk, SignalThresholds};

    fn chunk(adapter: &str, competitor: &str, from: i64, period: u32, rssi: &[u16]) -> Event {
        Event::SignalChunk(SignalChunk {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            from: SourceTime::from_micros(from),
            period_micros: period,
            rssi: rssi.to_vec(),
        })
    }

    fn thresholds(adapter: &str, competitor: &str, enter: u16, exit: u16) -> Event {
        Event::SignalThresholds(SignalThresholds {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            enter,
            exit,
        })
    }

    #[test]
    fn signal_trace_concatenates_chunks_in_log_order() {
        // Two appended chunks for one node reconstruct into one contiguous sample buffer, anchored
        // at the FIRST chunk's time base; thresholds fold in alongside.
        let log = vec![
            thresholds("rh", "node-0", 90, 80),
            chunk("rh", "node-0", 1_000_000, 100_000, &[70, 72, 150]),
            chunk("rh", "node-0", 1_300_000, 100_000, &[148, 71, 70]),
        ];
        let view = signal_trace(&log);
        let key = CompetitorKey {
            adapter: AdapterId("rh".into()),
            competitor: CompetitorRef("node-0".into()),
        };
        let trace = view.competitor(&key).expect("node-0 trace present");
        assert_eq!(trace.samples, vec![70, 72, 150, 148, 71, 70]);
        assert_eq!(trace.from, Some(SourceTime::from_micros(1_000_000)));
        assert_eq!(trace.period_micros, 100_000);
        assert_eq!(trace.enter, Some(90));
        assert_eq!(trace.exit, Some(80));
    }

    #[test]
    fn signal_trace_thresholds_are_last_writer_wins() {
        let log = vec![
            thresholds("rh", "node-0", 90, 80),
            thresholds("rh", "node-0", 95, 85),
        ];
        let view = signal_trace(&log);
        let trace = &view.competitors[0];
        assert_eq!((trace.enter, trace.exit), (Some(95), Some(85)));
    }

    #[test]
    fn signal_trace_ignores_passes_and_other_events() {
        // A heat full of passes/lifecycle/marshaling with no signal facts projects empty.
        let log = vec![
            pass("rh", "node-0", 1_000_000, Some(0)),
            pass("rh", "node-0", 4_000_000, Some(1)),
            voided(0),
        ];
        assert!(signal_trace(&log).competitors.is_empty());
    }

    #[test]
    fn signal_trace_is_per_competitor_and_key_ordered() {
        let log = vec![
            chunk("rh", "node-1", 0, 100_000, &[60, 120]),
            chunk("rh", "node-0", 0, 100_000, &[70, 150]),
        ];
        let view = signal_trace(&log);
        // Ordered by CompetitorKey (node-0 before node-1) regardless of arrival order.
        let keys: Vec<&str> = view
            .competitors
            .iter()
            .map(|c| c.competitor.competitor.0.as_str())
            .collect();
        assert_eq!(keys, vec!["node-0", "node-1"]);
    }

    #[test]
    fn signal_trace_fold_twice_identical() {
        // Determinism-on-replay: the same log always yields the same view.
        let log = vec![
            chunk("rh", "node-0", 1_000_000, 100_000, &[70, 150, 71]),
            thresholds("rh", "node-0", 90, 80),
            chunk("rh", "node-1", 1_000_000, 100_000, &[60, 120, 61]),
            chunk("rh", "node-0", 1_300_000, 100_000, &[70]),
        ];
        assert_eq!(signal_trace(&log), signal_trace(&log));
    }

    fn history(adapter: &str, competitor: &str, times: &[i64], rssi: &[u16]) -> Event {
        Event::SignalHistory(SignalHistory {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            times: times.to_vec(),
            rssi: rssi.to_vec(),
        })
    }

    #[test]
    fn dense_history_supersedes_coarse_chunks() {
        // A competitor with both coarse chunks AND a dense history: the view carries the DENSE trace
        // (far more samples), not the streamed approximation — the prefer-dense rule.
        let log = vec![
            thresholds("rh", "node-0", 90, 80),
            // Coarse: two streamed samples.
            chunk("rh", "node-0", 1_000_000, 100_000, &[70, 150]),
            // Dense: the full per-tick history pulled at heat end (6 samples, finer grid).
            history(
                "rh",
                "node-0",
                &[0, 50_000, 100_000, 150_000, 200_000, 250_000],
                &[70, 88, 150, 149, 90, 71],
            ),
        ];
        let view = signal_trace(&log);
        let key = CompetitorKey {
            adapter: AdapterId("rh".into()),
            competitor: CompetitorRef("node-0".into()),
        };
        let trace = view.competitor(&key).expect("node-0 trace");
        // The DENSE samples win, not the 2 coarse ones.
        assert_eq!(trace.samples, vec![70, 88, 150, 149, 90, 71]);
        // Grid derived from the dense times: from = first time, period = first inter-sample delta.
        assert_eq!(trace.from, Some(SourceTime::from_micros(0)));
        assert_eq!(trace.period_micros, 50_000);
        // Thresholds still fold in alongside (independent of the trace source).
        assert_eq!((trace.enter, trace.exit), (Some(90), Some(80)));
    }

    #[test]
    fn coarse_chunks_stand_without_a_dense_history() {
        // No SignalHistory: the coarse chunks are the trace (a heat that ended before the pull).
        let log = vec![chunk("rh", "node-0", 1_000_000, 100_000, &[70, 150, 71])];
        let view = signal_trace(&log);
        let trace = &view.competitors[0];
        assert_eq!(trace.samples, vec![70, 150, 71]);
        assert_eq!(trace.period_micros, 100_000);
    }

    #[test]
    fn empty_dense_history_does_not_blank_the_coarse_trace() {
        // A pull that returned an empty history must NOT erase the coarse evidence already captured.
        let log = vec![
            chunk("rh", "node-0", 0, 100_000, &[70, 150, 71]),
            history("rh", "node-0", &[], &[]),
        ];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![70, 150, 71]);
    }

    #[test]
    fn dense_history_last_writer_wins() {
        // Two pulls for one competitor: the later dense history replaces the earlier one.
        let log = vec![
            history("rh", "node-0", &[0, 100_000], &[70, 150]),
            history("rh", "node-0", &[0, 50_000, 100_000], &[70, 88, 150]),
        ];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![70, 88, 150]);
        assert_eq!(trace.period_micros, 50_000);
    }

    #[test]
    fn dense_history_fold_is_order_independent() {
        // Determinism: the dense/coarse choice is a pure function of which facts are present, not of
        // fold order — the history before or after the chunk yields the same (dense) view.
        let chunk_first = vec![
            chunk("rh", "node-0", 0, 100_000, &[70, 150]),
            history("rh", "node-0", &[0, 50_000, 100_000], &[70, 88, 150]),
        ];
        let history_first = vec![
            history("rh", "node-0", &[0, 50_000, 100_000], &[70, 88, 150]),
            chunk("rh", "node-0", 0, 100_000, &[70, 150]),
        ];
        assert_eq!(signal_trace(&chunk_first), signal_trace(&history_first));
    }

    #[test]
    fn single_sample_dense_history_has_zero_period() {
        let log = vec![history("rh", "node-0", &[42_000], &[150])];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![150]);
        assert_eq!(trace.from, Some(SourceTime::from_micros(42_000)));
        assert_eq!(trace.period_micros, 0);
    }
}
