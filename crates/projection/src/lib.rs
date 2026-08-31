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

use std::collections::{BTreeMap, BTreeSet};

use gridfpv_events::{AdapterId, CompetitorRef, Event, HeatId, LogRef, Pass, PilotId, SourceTime};
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
    /// The key of the source a pass came from. Public for the engine's grace rule
    /// ([`grace_satisfied`](../gridfpv_engine/scoring/fn.grace_satisfied.html), #505), which
    /// groups still-flying competitors exactly as the folds here do.
    pub fn from_pass(pass: &Pass) -> Self {
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
    /// Gate passes the RD **voided** (`DetectionVoided`, not undone), chronologically. The
    /// record of removals travels WITH the lap list so every consumer shares it: the console
    /// renders them struck-through in place, and threshold re-detection must NOT re-propose a
    /// crossing the RD explicitly removed — the RSSI trace still shows the crossing, so without
    /// this the tuner kept offering a voided lap back as "a lap to add". Additive: absent on
    /// the wire when empty, so older payloads round-trip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub voided: Vec<VoidedPass>,
}

/// One RD-voided gate pass, as the lap list records it (see [`CompetitorLaps::voided`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct VoidedPass {
    /// Source-clock time (µs) of the voided pass — its RAW instant (no re-time applied): the
    /// removal record exists so re-detection can recognise the crossing on the trace, and the
    /// trace knows nothing of a re-time.
    pub at: SourceTime,
    /// The voided pass's own global log offset (a stable row identity for the UI).
    pub pass_ref: LogRef,
    /// The **standing void event's** offset — the target a RESTORE (void-the-void) addresses.
    /// For an AUTO-suppressed pass (see [`VoidReason::UnderMinLap`]) this is the pass's own
    /// offset: there is no void event, and the restore path is a marshal ruling on the pass
    /// itself (an [`Event::LapAdjusted`] re-asserting its raw instant — an explicit ruling
    /// always outranks the floor).
    pub void_ref: LogRef,
    /// WHY the pass is off the lap chain — the console labels the row (and picks the restore
    /// command) by this.
    #[serde(default)]
    pub reason: VoidReason,
}

/// Why a pass sits on the removal record instead of the lap chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum VoidReason {
    /// A marshal explicitly removed it ([`Event::DetectionVoided`]).
    #[default]
    Marshal,
    /// The corrected fold suppressed it: it would close a lap under the round's minimum lap
    /// time (D26 — a gate reflection / double-detection; timers are dumb emitters, GridFPV
    /// owns lap semantics).
    UnderMinLap,
    /// The corrected fold suppressed it under the **grace rule** (#505): the competitor had
    /// already taken their one allowed crossing after the run's `RaceExpired` marker ("finish
    /// the lap you had started; once you cross after the end-of-race tone, no more laps
    /// count"). Marshal-restorable like [`UnderMinLap`](Self::UnderMinLap) — an explicit
    /// ruling outranks the rule.
    AfterRaceEnd,
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

/// Fold the log's heat lineups into the per-source [`CompetitorKey`]s they name (#388).
///
/// A heat's **lineup** ([`Event::HeatScheduled`]) is the authoritative "who was in this heat"
/// set: it is known the moment the heat is built, before a single crossing is detected. The
/// lap projection is keyed on `(adapter, competitor)` though, and the lineup carries only the
/// bare source-local [`CompetitorRef`] — so this fold resolves each lineup ref to the timing
/// source(s) it was actually seen on, over the *same* window the caller folds laps from.
///
/// Resolution, per lineup ref, in order:
///
/// 1. **Every adapter that named the ref** in the window — a [`Pass`], a signal fact
///    ([`Event::SignalChunk`] / [`Event::SignalThresholds`] / [`Event::SignalHistory`]), a
///    [`Event::CompetitorSeen`] / [`Event::CompetitorRegistered`], or a marshal's
///    [`Event::LapInserted`]. A seated node streams RSSI whether or not it ever detects a
///    crossing, so the silent-node case lands here and its key matches the one its
///    [`signal_trace`] entry already carries.
/// 2. Otherwise the window's **only** timing source, when exactly one is in evidence — a
///    single-timer event (the overwhelmingly common case) still seats a ref that produced
///    literally nothing.
/// 3. Otherwise the lexicographically **first** adapter in evidence, so a multi-source window
///    still yields one deterministic seat rather than dropping the competitor.
///
/// A window with no adapter in evidence at all (a bare `HeatScheduled` with nothing else)
/// yields nothing for that ref: there is no source to address a correction to, so inventing
/// an adapter id would only produce an unusable row. Pure and order-independent.
///
/// # A heat's lineup is its LATEST `HeatScheduled`, never the union (#443)
///
/// A seating override (and a round re-materialization, and a re-fill) re-emits `HeatScheduled`
/// for the same heat, and the heat's window keeps **both** entries — `heat_window_offsets` does
/// not filter heat-loop events. Unioning them seeded a pilot the override had seated *out* into
/// the marshaled lap list as a zero-lap row, on the one screen whose purpose is entering laps
/// against a row, while `live_state`'s `lineup_of` / `latest_schedule` (last-wins) showed the
/// correct lineup. So the fold is **last-wins per heat id**, the same rule every other reader
/// folds by; refs are unioned only *across* heats, which is what a multi-heat window means.
pub fn lineup_keys<'a, I>(events: I) -> BTreeSet<CompetitorKey>
where
    I: IntoIterator<Item = &'a Event>,
{
    // Per heat, its most recent lineup — the seats unioned at the end. Keyed by heat so a window
    // holding several heats still contributes every heat's lineup, without one heat's superseded
    // lineup surviving as another's.
    let mut lineup_by_heat: BTreeMap<HeatId, Vec<CompetitorRef>> = BTreeMap::new();
    // Every adapter each ref was named by, and every adapter in evidence at all.
    let mut adapters_by_ref: BTreeMap<CompetitorRef, BTreeSet<AdapterId>> = BTreeMap::new();
    let mut adapters: BTreeSet<AdapterId> = BTreeSet::new();

    for event in events {
        // The `(adapter, competitor)` pair this fact names, where it names one. A lifecycle
        // fact names only its source; a lineup names only refs.
        let named: Option<(&AdapterId, &CompetitorRef)> = match event {
            Event::HeatScheduled {
                heat, lineup: refs, ..
            } => {
                lineup_by_heat.insert(heat.clone(), refs.clone());
                None
            }
            Event::Pass(p) => Some((&p.adapter, &p.competitor)),
            Event::SignalChunk(c) => Some((&c.adapter, &c.competitor)),
            Event::SignalThresholds(t) => Some((&t.adapter, &t.competitor)),
            Event::SignalHistory(h) => Some((&h.adapter, &h.competitor)),
            Event::CompetitorSeen {
                adapter,
                competitor,
            }
            | Event::CompetitorRegistered {
                adapter,
                competitor,
                ..
            }
            | Event::LapInserted {
                adapter,
                competitor,
                ..
            } => Some((adapter, competitor)),
            Event::AdapterConnected { adapter }
            | Event::AdapterDisconnected { adapter }
            | Event::SessionStarted { adapter, .. }
            | Event::SessionEnded { adapter, .. } => {
                adapters.insert(adapter.clone());
                None
            }
            _ => None,
        };
        if let Some((adapter, competitor)) = named {
            adapters.insert(adapter.clone());
            adapters_by_ref
                .entry(competitor.clone())
                .or_default()
                .insert(adapter.clone());
        }
    }

    let lineup: BTreeSet<CompetitorRef> = lineup_by_heat.into_values().flatten().collect();
    lineup
        .into_iter()
        .flat_map(|competitor| {
            let sources: Vec<AdapterId> = match adapters_by_ref.get(&competitor) {
                Some(seen) => seen.iter().cloned().collect(),
                // No fact ever named this ref: fall back to the window's sole (or first)
                // source so a competitor who produced *nothing* is still marshalable.
                None => adapters.iter().next().cloned().into_iter().collect(),
            };
            sources.into_iter().map(move |adapter| CompetitorKey {
                adapter,
                competitor: competitor.clone(),
            })
        })
        .collect()
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
    corrected_and_voided_passes(events).0
}

/// [`corrected_passes`] under a round's **minimum-lap floor** (D26) — the scoring-path
/// sibling of [`lap_list_marshaled_with_floor`], so results and the lap list can never
/// disagree about a suppressed pass.
pub fn corrected_passes_with_floor<'a, I>(
    events: I,
    min_lap_micros: Option<i64>,
    race_expired: Option<u64>,
) -> Vec<(u64, Pass)>
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    corrected_and_voided_passes_with_floor(events, min_lap_micros, race_expired).0
}

/// The offset of `heat`'s standing **`RaceExpired` marker** within `window` — the grace rule's
/// boundary ([`VoidReason::AfterRaceEnd`]), resolved by every fold call site the same way the
/// D26 floor is resolved by `min_lap_micros_of`. **Last one wins**: a Restarted heat re-races
/// with a fresh marker, and the old run's passes all sit below the new marker's offset anyway.
/// `None` when the run never expired (ended on its criterion, ForceEnd, or a zero grace) — the
/// grace rule is then inert.
pub fn race_expired_offset<'a, I>(window: I, heat: &gridfpv_events::HeatId) -> Option<u64>
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    let mut found = None;
    for (offset, event) in window {
        if let Event::RaceExpired { heat: h, .. } = event {
            if h == heat {
                found = Some(offset);
            }
        }
    }
    found
}

/// One removed pass as the fold emits it:
/// `(pass offset, restore-target offset, pass, why)`.
pub type VoidedEmit = (u64, u64, Pass, VoidReason);

/// [`corrected_passes`] plus the passes the RD **voided** (and did not un-void), each resolved
/// to its concrete pass (re-time applied) with its own offset — the shared removal record the
/// lap list carries so re-detection never re-proposes an explicitly-removed crossing.
pub fn corrected_and_voided_passes<'a, I>(events: I) -> (Vec<(u64, Pass)>, Vec<VoidedEmit>)
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
                // The heat tag routes the insertion into the right heat *window* upstream;
                // the synthetic pass carries it through so tag-aware folds agree.
                heat,
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
                        heat: heat.clone(),
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
    // Which VOID EVENT (its own offset) currently holds each base pass voided — the target a
    // restore (void-the-void) addresses; carried onto the removal record for the UI.
    let mut void_source: BTreeMap<u64, u64> = BTreeMap::new();
    for (entry_offset, entry) in entries.iter() {
        match entry {
            // Passes (raw, inserted, or split) carry no ruling of their own; the split is
            // resolved to a concrete pass in the emit loop. A void/adjust *targeting* a split
            // is handled below via `entries.get(target)` falling into the pass arms.
            Entry::RawPass(_) | Entry::Inserted(_) | Entry::Split { .. } => {}
            Entry::Adjusted { target, at } => {
                // Re-time the target raw/inserted pass, and un-void it: an adjust is
                // the newest ruling on that target, so it supersedes an earlier void.
                voided.insert(*target, false);
                void_source.remove(target);
                retime.insert(*target, *at);
            }
            Entry::Voided { target } => {
                // Void the target. If the target is itself a ruling, supersede it:
                // voiding an adjust cancels its re-time (revert to the raw `at`);
                // voiding a void un-voids *that* void's target — and the chain WALKS,
                // so a depth-3 "void the un-void" re-voids the base pass (each link
                // flips the parity; the old two-level special case silently no-opped
                // at depth 3, breaking last-writer-wins).
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
                    Some(Entry::Voided { .. }) => {
                        // Walk the void chain to the base (non-void) target, flipping
                        // the intended state at each link: void(void(P)) restores P,
                        // void(void(void(P))) re-voids it, and so on.
                        let mut cursor = *target;
                        let mut state = true; // what THIS event wants for the base
                        while let Some(Entry::Voided { target: inner }) = entries.get(&cursor) {
                            state = !state;
                            cursor = *inner;
                        }
                        voided.insert(cursor, state);
                        if state {
                            void_source.insert(cursor, *entry_offset);
                        } else {
                            void_source.remove(&cursor);
                        }
                    }
                    // Voiding a raw pass or an inserted pass simply drops it.
                    _ => {
                        voided.insert(*target, true);
                        void_source.insert(*target, *entry_offset);
                    }
                }
            }
        }
    }

    // Emit the passes (raw + inserted + split-synthetic) with any re-time applied, in offset
    // order, each paired with the global offset that addresses it for a future correction —
    // surviving passes into `out`, RD-voided ones into `voided_out` (the shared removal
    // record); callers re-group and re-order them as needed.
    let mut out: Vec<(u64, Pass)> = Vec::new();
    let mut voided_out: Vec<VoidedEmit> = Vec::new();
    let mut scratch: Vec<(u64, Pass)> = Vec::new();
    for (offset, entry) in entries.iter() {
        let is_voided = voided.get(offset).copied().unwrap_or(false);
        scratch.clear();
        let sink: &mut Vec<(u64, Pass)> = if is_voided { &mut scratch } else { &mut out };
        match entry {
            Entry::RawPass(pass) => {
                let mut p = (*pass).clone();
                // A VOIDED pass keeps its RAW instant: the removal record exists so
                // re-detection can recognise the crossing on the trace, and the trace
                // knows nothing of a re-time (an adjusted-then-voided pass would
                // otherwise leave the suppression zone at the wrong instant).
                if !is_voided {
                    if let Some(at) = retime.get(offset) {
                        p.at = *at;
                    }
                }
                sink.push((*offset, p));
            }
            Entry::Inserted(pass) => {
                let mut p = pass.clone();
                if !is_voided {
                    if let Some(at) = retime.get(offset) {
                        p.at = *at;
                    }
                }
                sink.push((*offset, p));
            }
            Entry::Split { target, at } => {
                // Attribute the synthetic mid-lap pass to the competitor whose lap ends at
                // `target` (the target pass's adapter/competitor). A later adjust of *this*
                // split's offset re-times the synthetic pass; a void drops it. If the target
                // pass is unknown (a dangling ref — the command layer rejects these), skip.
                // The synthetic pass is addressable by *this* split's own offset.
                // Resolve the source pass RECURSIVELY: a split may target another split's
                // synthetic pass (splitting the second half of a twice-missed stretch) —
                // the old single-step lookup silently skipped it while the audit showed
                // the split as landed.
                let mut src_offset = *target;
                let src = loop {
                    match entries.get(&src_offset) {
                        Some(Entry::RawPass(p)) => break Some((*p).clone()),
                        Some(Entry::Inserted(p)) => break Some(p.clone()),
                        Some(Entry::Split { target: inner, .. }) => src_offset = *inner,
                        _ => break None,
                    }
                };
                if let Some(src) = src {
                    sink.push((
                        *offset,
                        Pass {
                            adapter: src.adapter,
                            competitor: src.competitor,
                            at: retime.get(offset).copied().unwrap_or(*at),
                            sequence: None,
                            gate: gridfpv_events::GateIndex::LAP,
                            signal: None,
                            heat: src.heat,
                        },
                    ));
                }
            }
            Entry::Adjusted { .. } | Entry::Voided { .. } => {}
        }
        if is_voided {
            let void_ref = void_source.get(offset).copied().unwrap_or(*offset);
            for (o, p) in scratch.drain(..) {
                voided_out.push((o, void_ref, p, VoidReason::Marshal));
            }
        }
    }
    (out, voided_out)
}

/// [`corrected_and_voided_passes`] with the round's **auto-suppression rules** applied: the
/// minimum-lap floor (D26) and the grace rule against the run's `RaceExpired` marker (#505).
///
/// After the marshaling corrections fold, each competitor's surviving chain is walked
/// chronologically:
///
/// - **The floor**: a **raw, unruled** pass that would close a lap shorter than
///   `min_lap_micros` is AUTO-SUPPRESSED — moved onto the removal record with
///   [`VoidReason::UnderMinLap`] (its restore target is itself; a marshal re-time exempts it).
/// - **The grace rule**: past the `race_expired` marker offset (the run's end-of-race tone),
///   each competitor keeps their **first** surviving crossing — the lap they were already
///   flying lands — and every later one drops to the removal record with
///   [`VoidReason::AfterRaceEnd`]. Log-order, not time-order: "one crossing after the tone"
///   is a statement about the tone, and the marker's log position is the tone. The floor runs
///   first, so a reflection burst at the line does not spend the pilot's one allowed crossing.
///
/// Marshal-created passes (inserted, split-synthetic) and re-timed passes are NEVER
/// suppressed by either rule: an explicit ruling outranks both — which is also what keeps
/// post-race marshaling working, since every marshal insert is appended after the marker.
/// `None`/`0` floor and a `None` marker ⇒ identical to the plain fold, so rounds predating
/// the settings keep their results bit-identical.
pub fn corrected_and_voided_passes_with_floor<'a, I>(
    events: I,
    min_lap_micros: Option<i64>,
    race_expired: Option<u64>,
) -> (Vec<(u64, Pass)>, Vec<VoidedEmit>)
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    // The plain fold needs to tell us which surviving passes are raw-and-unruled; re-derive
    // that from the events here so the core fold stays untouched. Collect first (two passes
    // over the data, but windows are per-heat and small).
    let pairs: Vec<(u64, &Event)> = events.into_iter().collect();
    let (surviving, mut voided) = corrected_and_voided_passes(pairs.iter().copied());
    let floor = min_lap_micros.filter(|f| *f > 0);
    if floor.is_none() && race_expired.is_none() {
        return (surviving, voided);
    }

    // A pass is EXEMPT from both rules when a marshal shaped it: inserted or split-synthetic
    // by construction, or re-timed by a standing (un-voided) adjust.
    let mut exempt: BTreeSet<u64> = BTreeSet::new();
    for (offset, event) in &pairs {
        match event {
            Event::LapInserted { .. } | Event::LapSplit { .. } => {
                exempt.insert(*offset);
            }
            Event::LapAdjusted { target, .. } => {
                exempt.insert(target.0);
            }
            _ => {}
        }
    }

    // Walk each competitor's chain in time order, keep-first: a too-close successor that is
    // not marshal-blessed drops to the removal record, and past the marker only the first
    // unruled crossing survives.
    let mut by_competitor: BTreeMap<CompetitorKey, Vec<(u64, Pass)>> = BTreeMap::new();
    for (offset, pass) in surviving {
        by_competitor
            .entry(CompetitorKey::from_pass(&pass))
            .or_default()
            .push((offset, pass));
    }
    let mut out: Vec<(u64, Pass)> = Vec::new();
    for (_, mut chain) in by_competitor {
        chain.sort_by_key(|(offset, p)| (p.at, *offset));
        let mut last_kept: Option<SourceTime> = None;
        let mut post_expiry_taken = false;
        for (offset, pass) in chain {
            let ruled = exempt.contains(&offset);
            let too_close = floor.is_some_and(|floor| {
                last_kept.is_some_and(|prev| pass.at.micros.saturating_sub(prev.micros) < floor)
            });
            if too_close && !ruled {
                voided.push((offset, offset, pass, VoidReason::UnderMinLap));
                continue;
            }
            if race_expired.is_some_and(|marker| offset > marker) && !ruled {
                // The one allowed post-expiry crossing (#505): the first surviving unruled pass
                // after the marker lands (finishing the lap already in the air — or, for a pilot
                // who had not crossed yet, a holeshot that opens nothing scoreable); the rest
                // are void. A pass at or before the marker's offset pre-dates the tone and is
                // untouched by this rule.
                if post_expiry_taken {
                    voided.push((offset, offset, pass, VoidReason::AfterRaceEnd));
                    continue;
                }
                post_expiry_taken = true;
            }
            last_kept = Some(pass.at);
            out.push((offset, pass));
        }
    }
    out.sort_by_key(|(offset, _)| *offset);
    (out, voided_out_sorted(voided))
}

/// Stable ordering for the removal record (offset order, like the surviving stream).
fn voided_out_sorted(mut voided: Vec<VoidedEmit>) -> Vec<VoidedEmit> {
    voided.sort_by_key(|(offset, _, _, _)| *offset);
    voided
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
    lap_list_marshaled_with_floor(events, None, None)
}

/// [`lap_list_marshaled`] under a round's **auto-suppression rules**: the minimum-lap floor
/// (D26) and the grace rule against `race_expired` (#505) — suppressed passes land on each
/// competitor's removal record with [`VoidReason::UnderMinLap`] / [`VoidReason::AfterRaceEnd`].
pub fn lap_list_marshaled_with_floor<'a, I>(
    events: I,
    min_lap_micros: Option<i64>,
    race_expired: Option<u64>,
) -> LapList
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    CorrectedWindow::of(events, min_lap_micros, race_expired).into_lap_list()
}

/// One window's **correction fold, folded once** — the shared input to both views of it: the
/// lap list ([`into_lap_list`](Self::into_lap_list)) and the crossing feed
/// ([`crossings`](Self::crossings)).
///
/// It exists because those two views want the same fold. The live race-state projection derives
/// both from the same run window on every wake, and running
/// [`corrected_and_voided_passes_with_floor`] twice over an identical slice is the single most
/// expensive thing on the 16/s signal-append path (#460 item 2). Folding once and reading the
/// result two ways is also what *guarantees* they agree — a crossing the lap fold counted is
/// `Counted` in the feed with the same lap number, and one the floor suppressed is
/// `RejectedTooShort` in both, because there is only one fold to disagree with.
pub struct CorrectedWindow {
    /// The corrected lap-gate pass stream, ascending by global append offset.
    surviving: Vec<(u64, Pass)>,
    /// The removal record (auto-suppressed + RD-voided), ascending by offset.
    voided: Vec<VoidedEmit>,
    /// The window's lineup seats (#388) — competitors the lap list must show even with no passes.
    seats: BTreeSet<CompetitorKey>,
}

impl CorrectedWindow {
    /// Fold `events` (a window of `(offset, event)` pairs) under the D26 floor and the #505
    /// grace rule (`race_expired` — the run's marker offset, [`race_expired_offset`]).
    pub fn of<'a, I>(events: I, min_lap_micros: Option<i64>, race_expired: Option<u64>) -> Self
    where
        I: IntoIterator<Item = (u64, &'a Event)>,
    {
        // The window is walked twice (once for the lineup seed, once for the corrections fold),
        // so materialise the pairs — they are borrowed `(offset, &Event)` handles and the window
        // is per-heat, so this is a pointer copy, not the log.
        let pairs: Vec<(u64, &Event)> = events.into_iter().collect();
        // #388 — SEED FROM THE LINEUP, not only from what the timer observed. A competitor the
        // timer never detected (mis-tuned gate, dead VTX) must still appear, with zero laps, or
        // the one pilot who most needs marshaling is the one who cannot be marshaled.
        let seats = lineup_keys(pairs.iter().map(|(_, e)| *e));
        let (surviving, voided) = corrected_and_voided_passes_with_floor(
            pairs.iter().copied(),
            min_lap_micros,
            race_expired,
        );
        Self {
            surviving,
            voided,
            seats,
        }
    }

    /// Project the fold into the **lap-list** view: group the corrected pass stream by competitor
    /// and derive laps. Each pass keeps the global offset that addresses it, so the derived laps
    /// carry their `start_ref`/`end_ref` command targets.
    pub fn into_lap_list(self) -> LapList {
        lap_list_of_corrected(self.seats, self.surviving, self.voided)
    }
}

/// The lap-list projection of one folded window — see [`CorrectedWindow::into_lap_list`].
fn lap_list_of_corrected(
    seats: BTreeSet<CompetitorKey>,
    surviving: Vec<(u64, Pass)>,
    voided: Vec<VoidedEmit>,
) -> LapList {
    let mut by_competitor: BTreeMap<CompetitorKey, Vec<(u64, Pass)>> = BTreeMap::new();
    for key in seats {
        by_competitor.entry(key).or_default();
    }
    for (offset, pass) in surviving {
        by_competitor
            .entry(CompetitorKey::from_pass(&pass))
            .or_default()
            .push((offset, pass));
    }
    // The RD-voided passes, grouped the same way — a competitor may have voids but no
    // surviving laps (every crossing removed), so they seed the map too.
    let mut voided_by_competitor: BTreeMap<CompetitorKey, Vec<VoidedPass>> = BTreeMap::new();
    for (offset, void_offset, pass, reason) in voided {
        by_competitor
            .entry(CompetitorKey::from_pass(&pass))
            .or_default();
        voided_by_competitor
            .entry(CompetitorKey::from_pass(&pass))
            .or_default()
            .push(VoidedPass {
                at: pass.at,
                pass_ref: LogRef(offset),
                void_ref: LogRef(void_offset),
                reason,
            });
    }

    let competitors = by_competitor
        .into_iter()
        .map(|(competitor, mut passes)| {
            passes.sort_by_key(|(_, p)| corrected_order_key(p));
            let mut voided = voided_by_competitor.remove(&competitor).unwrap_or_default();
            voided.sort_by_key(|v| v.at);
            CompetitorLaps {
                competitor,
                laps: laps_from_corrected(&passes),
                voided,
            }
        })
        .collect();

    LapList { competitors }
}

/// What became of one gate crossing — the **disposition** the live crossing feed carries (#397).
///
/// A timer emits crossings; GridFPV owns lap semantics, so a crossing is not the same thing as a
/// lap and the interesting ones are mostly *not* laps. There is no "holeshot" concept in the log —
/// the first crossing is an ordinary [`GateIndex::LAP`](gridfpv_events::GateIndex::LAP) pass, and
/// lap derivation is nothing but consecutive pairs of the corrected chain (`passes.windows(2)`).
/// So a disposition is a **position in the corrected pass chain**, or the removal record the fold
/// already keeps — never a new logged fact.
///
/// The two removal-side variants map 1:1 onto the only two [`VoidReason`]s that exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum CrossingDisposition {
    /// The **first** surviving crossing of this competitor's chain — the holeshot. It opens the
    /// first lap and closes none, so it derives no [`Lap`] at all and is invisible to every
    /// lap-derived consumer.
    Holeshot,
    /// A crossing that **closed a lap** (chain position `n >= 1` closes lap `n`).
    Counted,
    /// The corrected fold **auto-suppressed** it under the round's minimum-lap floor (D26 —
    /// [`VoidReason::UnderMinLap`]): a gate reflection / double-detection. It records no lap and
    /// today reaches no live consumer at all, which is the gap #397 exists to close — a
    /// too-sensitive gate is as broken as an insensitive one, and nothing surfaces it live.
    RejectedTooShort,
    /// A marshal explicitly removed it after the fact ([`VoidReason::Marshal`]). It was a real
    /// observed crossing when it happened; the removal is a later ruling over it.
    VoidedByMarshal,
    /// The corrected fold **auto-suppressed** it under the grace rule (#505 —
    /// [`VoidReason::AfterRaceEnd`]): the competitor had already taken their one allowed
    /// crossing after the run's `RaceExpired` marker. Still tones live (#397) — the RD hears
    /// that the gate fired, and the distinct disposition says it no longer scores.
    RejectedAfterRaceEnd,
}

impl CrossingDisposition {
    /// The disposition a removal-record [`VoidReason`] maps to.
    fn of_void(reason: VoidReason) -> Self {
        match reason {
            VoidReason::Marshal => Self::VoidedByMarshal,
            VoidReason::UnderMinLap => Self::RejectedTooShort,
            VoidReason::AfterRaceEnd => Self::RejectedAfterRaceEnd,
        }
    }
}

/// One gate crossing paired with **what became of it** — the unit [`dispositioned_passes`] emits.
///
/// Deliberately *not* a wire type: the live-state projection maps it onto its own additive field
/// (`LiveRaceState::crossings`), resolving the source-local competitor to a pilot on the way.
/// Keeping the derivation here puts it beside [`laps_from_corrected`], which owns the same
/// chain-position rule.
#[derive(Debug, Clone, PartialEq)]
pub struct DispositionedPass {
    /// The crossing's **global append offset** — its stable identity. The same crossing folds to
    /// the same offset on every re-fold of the same log, from any scope, which is what lets a
    /// consumer fire once per crossing instead of once per delivered frame.
    pub offset: LogRef,
    /// The concrete pass: a marshal re-time applied for a surviving crossing; the RAW instant for
    /// one on the removal record (the record exists so the signal trace can be matched against it,
    /// and the trace knows nothing of a re-time).
    pub pass: Pass,
    /// What became of it.
    pub disposition: CrossingDisposition,
    /// The 1-based lap this crossing **closed**, when it closed one — exactly [`Lap::number`] for
    /// the lap whose `end_ref` is this offset. `None` for a holeshot (it closes nothing) and for
    /// anything on the removal record.
    pub lap_number: Option<u32>,
}

/// Fold a window into its **crossings with dispositions** (#397) — every lap-gate crossing the
/// window carries, surviving *and* suppressed, each labelled with what became of it.
///
/// This is the counterpart to [`lap_list_marshaled_with_floor`] for consumers that care about
/// *crossings* rather than *laps*. It is that same fold —
/// [`corrected_and_voided_passes_with_floor`] — read a second way: the surviving chain is walked
/// in corrected order (so chain position 0 is the holeshot and position `n` closed lap `n`,
/// matching [`laps_from_corrected`] exactly), and the removal record supplies the crossings that
/// never made the chain.
///
/// # Ordering — by OFFSET, not by time
///
/// The result is sorted by global append offset, ascending. That is deliberate and load-bearing
/// for idempotency: append offsets only ever grow, so a consumer can hold a single high-water mark
/// and read "offset > watermark" as "a crossing I have not seen". Source *time* is not monotonic
/// across the feed — a marshal-inserted pass carries an old `at` under a brand-new offset — so
/// ordering by `at` would sort a genuinely new entry into the middle of already-seen ones.
///
/// # What is deliberately NOT here
///
/// - **Split gates.** Only lap-gate passes are crossings
///   ([`is_lap_gate`](gridfpv_events::GateIndex::is_lap_gate)); intermediate splits are lap detail,
///   exactly as the lap fold has it.
/// - **Seats with no crossings.** Unlike [`lap_list_marshaled_with_floor`] (which seeds from the
///   lineup so an undetected pilot can still be marshaled, #388), there is nothing to say about a
///   competitor who has not crossed. Conversely a crossing by a competitor who is *not* in the
///   lineup IS reported — a phantom detection on an empty seat is precisely what an RD needs to
///   notice, so this fold must never filter toward "only meaningful laps".
pub fn dispositioned_passes<'a, I>(
    events: I,
    min_lap_micros: Option<i64>,
    race_expired: Option<u64>,
) -> Vec<DispositionedPass>
where
    I: IntoIterator<Item = (u64, &'a Event)>,
{
    CorrectedWindow::of(events, min_lap_micros, race_expired).crossings(None)
}

impl CorrectedWindow {
    /// Project the fold into the **crossing** view — see [`dispositioned_passes`] for the rules.
    ///
    /// `limit` keeps only the most recent `n` crossings by offset (the tail; the head is dropped,
    /// so every surviving `pass_ref` is still above anything a consumer already retired). It is a
    /// parameter rather than the caller's job because the caller's job was to materialise *every*
    /// crossing of a possibly-unbounded open-practice run and then throw all but 64 away (#460
    /// item 2). Here the cutoff is found first — both inputs are offset-sorted, so it costs one
    /// walk of `n` from their tails — and only the crossings at or above it are built.
    ///
    /// Chain positions are still counted over the **whole** chain, so a bounded read reports the
    /// same `lap_number` for a crossing as an unbounded one; only the head is missing.
    pub fn crossings(&self, limit: Option<usize>) -> Vec<DispositionedPass> {
        if limit == Some(0) {
            return Vec::new();
        }
        let cutoff = limit.and_then(|limit| self.nth_newest_offset(limit));
        let keep = |offset: u64| cutoff.is_none_or(|cutoff| offset >= cutoff);

        // Group the surviving stream per competitor so chain POSITION is per-seat: seat 3's first
        // crossing is seat 3's holeshot however many other seats crossed before it.
        let mut by_competitor: BTreeMap<CompetitorKey, Vec<&(u64, Pass)>> = BTreeMap::new();
        for entry in &self.surviving {
            by_competitor
                .entry(CompetitorKey::from_pass(&entry.1))
                .or_default()
                .push(entry);
        }

        let mut out: Vec<DispositionedPass> = Vec::new();
        for (_, mut chain) in by_competitor {
            // The SAME ordering key the lap list uses, so chain position and `Lap::number` cannot
            // drift apart.
            chain.sort_by_key(|(_, p)| corrected_order_key(p));
            for (position, (offset, pass)) in chain.into_iter().enumerate() {
                if !keep(*offset) {
                    continue;
                }
                out.push(DispositionedPass {
                    offset: LogRef(*offset),
                    pass: pass.clone(),
                    // Position 0 opens the first lap and closes none — that IS the holeshot,
                    // derived rather than flagged, because nothing in the log says "holeshot".
                    disposition: if position == 0 {
                        CrossingDisposition::Holeshot
                    } else {
                        CrossingDisposition::Counted
                    },
                    lap_number: (position > 0).then_some(position as u32),
                });
            }
        }
        for (offset, _void_ref, pass, reason) in &self.voided {
            if !keep(*offset) {
                continue;
            }
            out.push(DispositionedPass {
                offset: LogRef(*offset),
                pass: pass.clone(),
                disposition: CrossingDisposition::of_void(*reason),
                lap_number: None,
            });
        }

        out.sort_by_key(|d| d.offset.0);
        out
    }

    /// The offset of the `n`th-newest crossing across both lists, or `None` when there are fewer
    /// than `n` (nothing to trim). Both lists are ascending by offset, so this is a merge walked
    /// backwards `n` steps — `O(n)`, never `O(window)`. `n` is never 0 (the caller short-circuits).
    fn nth_newest_offset(&self, n: usize) -> Option<u64> {
        let mut alive = self.surviving.len();
        let mut voided = self.voided.len();
        if alive + voided < n {
            return None;
        }
        let mut cutoff = None;
        for _ in 0..n {
            let a = (alive > 0).then(|| self.surviving[alive - 1].0);
            let v = (voided > 0).then(|| self.voided[voided - 1].0);
            cutoff = match (a, v) {
                (Some(a), Some(v)) if a >= v => {
                    alive -= 1;
                    Some(a)
                }
                (Some(_), Some(v)) => {
                    voided -= 1;
                    Some(v)
                }
                (Some(a), None) => {
                    alive -= 1;
                    Some(a)
                }
                (None, Some(v)) => {
                    voided -= 1;
                    Some(v)
                }
                (None, None) => break,
            };
        }
        cutoff
    }
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
    /// The **actual** source-clock timestamp (µs) of each sample, when the trace came from a dense
    /// history. RH's marshal history is non-uniformly spaced (bursts of peak/nadir entries around
    /// each crossing), so a uniform `from + i·period_micros` grid badly misplaces samples and
    /// understates the span; a renderer that has these should plot each sample at its real time.
    /// `None` for the coarse streaming path, where `from`/`period_micros` is exact.
    #[serde(default)]
    #[ts(optional, type = "Array<number>")]
    pub times: Option<Vec<i64>>,
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
///   pulled from the request-driven `current_marshal_data` at heat end, or streamed live by the
///   GridFPV plugin. When a competitor has **any** dense history, it **supersedes** the coarse chunk
///   samples entirely: the view carries the dense trace, not the streaming approximation. With no
///   dense history the coarse chunks stand, so a heat that ended before the pull (or a non-RH
///   source) is unchanged.
///
/// # Dense histories are **offset-folded**, not last-writer-wins (#392)
///
/// A dense history is not necessarily a whole trace: the live plugin path streams it in slices, each
/// stamped with the sample offset it starts at ([`SignalHistory::base`]), so the per-tick cost does
/// not grow with heat length. The fold applies each one by that offset:
///
/// - `base ==` the trace's current length — a **contiguous append**: the slice extends the trace.
///   The overwhelmingly common case, and byte-exact: samples are copied verbatim, duplicate
///   timestamps included.
/// - `base == 0` **and** the slice starts no later than what we already hold — a **whole-trace
///   snapshot**: it replaces the competitor's dense trace. The post-race marshal pull sends one, so
///   a re-pull still supersedes what came before (the old behavior).
/// - anything else — **re-synchronise by timestamp**: append the part of the slice that is strictly
///   newer than the last sample held, and drop the overlap.
///
/// ## Why the last rule is not "skip" any more (#448)
///
/// It used to be, and that froze the live trace on any long run. The plugin keeps its own
/// accumulator capped (`SIGNAL_WINDOW`, 20000 samples); past the cap it drops the oldest samples
/// and **rebases** its `sent` cursor by the number dropped. Every subsequent slice's `base` is
/// therefore relative to a *pruned* accumulator, while this fold holds the whole un-pruned trace —
/// so `base` never equalled the trace length again, every live tick was classified out of sync and
/// skipped, and the RSSI trace froze mid-session with no error for the rest of the heat. The
/// end-of-race flush "recovered" it only by replacing a complete trace with the plugin's truncated
/// 20000-sample window, silently discarding the earliest samples of the run.
///
/// Timestamps are what make the recovery safe. A dense sample carries its own source-clock instant,
/// those instants are monotonic within a heat, and the plugin already relies on exactly this to
/// merge RotorHazard's own front-pruned node history. So a slice that cannot be placed by offset is
/// placed by time instead: nothing is duplicated, nothing is invented, and the head this fold has
/// already accumulated is kept rather than thrown away. `base` remains a fast path and a hint, not
/// a contract the fold breaks when the producer's window moves under it.
///
/// The one cost is at a re-sync seam: RotorHazard reports peak/nadir pairs that can share a
/// timestamp, and "strictly newer than the last held" drops a same-instant sample we had not seen.
/// That is bounded to the single slice where a prune happened, and is the right trade against
/// freezing the trace for the rest of the heat.
///
/// An empty history is inert throughout: it carries no evidence, so it must not blank out a trace.
///
/// A pre-#392 log carries no offsets at all; they read back as `base = 0`, i.e. whole snapshots that
/// replace — which is exactly the last-writer-wins rule those logs were written under.
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
///
/// [`SignalHistory::base`]: gridfpv_events::SignalHistory::base
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
        /// The dense trace that supersedes the coarse `samples`/`from`/`period_micros`, as
        /// `(times, rssi)` — assembled from the competitor's [`Event::SignalHistory`] slices by
        /// their `base` offset. `None` until one lands; when present it is resolved into the
        /// emitted trace at the end.
        dense: Option<(Vec<i64>, Vec<u16>)>,
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
                // Prefer-dense: the dense history supersedes the coarse chunks. It arrives as
                // offset-stamped slices (#392), so place each one by its `base` — replace at 0,
                // append at the current length, skip anything else rather than corrupt the trace.
                // An empty history is ignored either way (it carries no evidence, so it must not
                // blank out the coarse trace).
                splice_dense(&mut acc.dense, history.base, &history.times, &history.rssi);
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
                // Prefer-dense: when a dense history is present it replaces the coarse stream. The
                // uniform `from`/`period_micros` grid is kept (compat), but the explicit per-sample
                // `times` are carried too so a renderer can plot each sample at its real instant — RH's
                // history is bursty, so the uniform grid alone badly compresses the trace.
                let (from, period_micros, samples, times) = match acc.dense {
                    Some((dense_times, dense_rssi)) => {
                        let (from, period_micros) = dense_trace_grid(&dense_times);
                        (from, period_micros, dense_rssi, Some(dense_times))
                    }
                    None => (acc.from, acc.period_micros, acc.samples, None),
                };
                CompetitorTrace {
                    competitor,
                    from,
                    period_micros,
                    samples,
                    times,
                    enter: acc.enter,
                    exit: acc.exit,
                }
            })
            .collect(),
    }
}

/// Place one dense [`Event::SignalHistory`] slice onto a competitor's assembled `(times, rssi)`
/// trace — the offset fast path, the whole-trace snapshot, and the timestamp re-sync that keeps a
/// long session's live trace moving after the producer prunes its own window (#448).
///
/// Split out from [`signal_trace`] so the placement rule is a unit test over a simulated prune
/// sequence rather than something only a 20000-sample live run can reach. See `signal_trace`'s
/// "Dense histories are offset-folded" section for why each branch is what it is.
///
/// An empty slice is inert: it carries no evidence, so it never blanks out or replaces a trace.
/// `times` and `rssi` are zipped to the shorter of the two, so a malformed pair can shorten a
/// slice but can never leave the two vectors out of step with each other.
fn splice_dense(dense: &mut Option<(Vec<i64>, Vec<u16>)>, base: u64, times: &[i64], rssi: &[u16]) {
    let n = times.len().min(rssi.len());
    if n == 0 {
        return;
    }
    let (times, rssi) = (&times[..n], &rssi[..n]);

    let Some((held_times, held_rssi)) = dense.as_mut() else {
        // Nothing dense held yet. Only a `base == 0` slice may *establish* the trace: a slice from
        // the middle of a stream whose opening this window never saw is still unplaceable, and the
        // competitor's coarse chunks are better evidence than one orphan fragment. (A prune can
        // never land here — the plugin resets its accumulator at race start, so every heat's
        // stream does begin at 0.)
        if base == 0 {
            *dense = Some((times.to_vec(), rssi.to_vec()));
        }
        return;
    };
    if held_times.is_empty() {
        *dense = Some((times.to_vec(), rssi.to_vec()));
        return;
    }

    // The contiguous append: the producer's cursor still lines up with what we hold. Verbatim,
    // duplicate timestamps and all.
    if base == held_times.len() as u64 {
        held_times.extend_from_slice(times);
        held_rssi.extend_from_slice(rssi);
        return;
    }

    // A whole-trace snapshot — one that starts no later than what we already hold — supersedes it.
    // That is the post-race marshal pull. A `base == 0` flush whose first sample is *newer* than
    // our first is NOT this: it is the plugin's pruned window, and replacing with it would throw
    // away the head of the run that only we still have.
    if base == 0 && times[0] <= held_times[0] {
        *dense = Some((times.to_vec(), rssi.to_vec()));
        return;
    }

    // Otherwise re-synchronise by timestamp: keep everything strictly newer than the last sample
    // held, drop the overlap. This is the branch a producer-side prune lands in, and the branch
    // that used to skip the slice outright and freeze the trace for the rest of the heat.
    let last_held = held_times[held_times.len() - 1];
    let start = times.partition_point(|t| *t <= last_held);
    held_times.extend_from_slice(&times[start..]);
    held_rssi.extend_from_slice(&rssi[start..]);
}

/// Resolve an assembled dense trace's sample `times` into the uniform `(from, period_micros)` grid
/// the [`CompetitorTrace`] carries alongside them. The samples themselves are passed through
/// verbatim by the caller — native ADC counts, no resampling.
///
/// `from` is the first sample's instant; `period_micros` is the first **positive** inter-sample
/// delta (RH samples at a near-fixed rate, so this anchors the grid the renderer draws on). A dense
/// history can legitimately repeat a timestamp — e.g. a peak reported at the same first/last time,
/// so `history_times` reads `[t, t, …]` — so the grid skips zero/negative deltas to avoid a
/// degenerate period of `0`; it falls back to `0` only when every delta is non-positive (a single
/// distinct time, including a single-sample history).
fn dense_trace_grid(times: &[i64]) -> (Option<SourceTime>, u32) {
    let from = times.first().copied().map(SourceTime::from_micros);
    let period_micros = times
        .windows(2)
        .map(|w| w[1] - w[0])
        .find(|&d| d > 0)
        .unwrap_or(0)
        .clamp(0, u32::MAX as i64) as u32;
    (from, period_micros)
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
            heat: None,
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
            heat: None,
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
    use gridfpv_events::{GateIndex, HeatId, LogRef, Penalty, SignalHistory};

    /// Build a lap-gate pass event.
    fn pass(adapter: &str, competitor: &str, at: i64, sequence: Option<u64>) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence,
            gate: GateIndex::LAP,
            signal: None,
            heat: None,
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
            heat: None,
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
    fn a_voided_pass_is_recorded_on_the_lap_list_and_unvoiding_clears_it() {
        // The removal record travels WITH the lap list (the void/re-detection shared-data
        // rule): an RD-voided crossing shows up in `CompetitorLaps::voided` at its recorded
        // instant with its own offset — so the console can render it struck-through and the
        // threshold re-detection can refuse to re-propose it. Un-voiding (void the void)
        // returns the pass to the laps and clears the record.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1 — the not-a-full-lap crossing
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
            voided(1),                           // offset 3 — the RD removes it
        ];
        let result = lap_list_marshaled(tagged(&events));
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        // One surviving lap (0 → 2); the voided crossing is recorded, not forgotten.
        assert_eq!(cl.laps.len(), 1);
        assert_eq!(
            cl.voided,
            vec![VoidedPass {
                at: SourceTime::from_micros(4_000_000),
                pass_ref: LogRef(1),
                void_ref: LogRef(3),
                reason: VoidReason::Marshal,
            }]
        );

        // Void the void: the pass returns to the laps and leaves the removal record.
        let mut events = events;
        events.push(voided(3)); // offset 4
        let result = lap_list_marshaled(tagged(&events));
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(cl.laps.len(), 2);
        assert!(cl.voided.is_empty());
    }

    #[test]
    fn min_lap_floor_suppresses_the_phantom_double_detection() {
        // The live bug (Audit Shakedown): every pilot got TWO passes 4ms apart at race start —
        // the second closed a phantom 0.004s "lap 1" and shifted every real lap's number.
        // Under a 5s floor the echo drops to the removal record; the chain reads holeshot →
        // real laps, exactly as if the timer had never double-fired.
        let events = vec![
            pass("vd", "A", 651_000, Some(1)), // offset 0 — holeshot (kept: first)
            pass("vd", "A", 655_000, Some(2)), // offset 1 — the 4ms echo (suppressed)
            pass("vd", "A", 7_208_000, Some(3)), // offset 2 — real lap 1
            pass("vd", "A", 13_500_000, Some(4)), // offset 3 — real lap 2
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), Some(5_000_000), None);
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        let durations: Vec<i64> = cl.laps.iter().map(|l| l.duration_micros).collect();
        assert_eq!(
            durations,
            vec![6_557_000, 6_292_000],
            "holeshot opens the chain; the echo never closes a lap"
        );
        assert_eq!(
            cl.voided,
            vec![VoidedPass {
                at: SourceTime::from_micros(655_000),
                pass_ref: LogRef(1),
                void_ref: LogRef(1), // restore target = the pass itself (a marshal re-time)
                reason: VoidReason::UnderMinLap,
            }]
        );
        // No floor ⇒ bit-identical to the plain fold (rounds predating the setting).
        let unfloored = lap_list_marshaled_with_floor(tagged(&events), None, None);
        let plain = lap_list_marshaled(tagged(&events));
        assert_eq!(unfloored, plain);
        assert_eq!(plain.competitors[0].laps.len(), 3);
    }

    #[test]
    fn grace_rule_keeps_the_first_post_expiry_crossing_and_voids_the_rest() {
        // The grace rule (#505): past the RaceExpired marker each competitor may finish the lap
        // they were flying — their FIRST post-marker crossing counts — and nothing after it does.
        let heat = HeatId("q-1".into());
        let log = vec![
            pass("rh", "A", 1_000_000, Some(1)),  // offset 0 — holeshot
            pass("rh", "A", 18_000_000, Some(2)), // offset 1 — lap 1
            Event::RaceExpired {
                heat: heat.clone(),
                deadline: None,
            }, // offset 2 — the end-of-race tone
            pass("rh", "A", 35_000_000, Some(3)), // offset 3 — the grace lap: counts
            pass("rh", "A", 52_000_000, Some(4)), // offset 4 — after the allowed crossing: void
        ];
        // The marker is resolved from the window exactly as the server call sites do.
        let marker = race_expired_offset(tagged(&log), &heat);
        assert_eq!(marker, Some(2));
        let result = lap_list_marshaled_with_floor(tagged(&log), None, marker);
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(
            cl.laps.len(),
            2,
            "lap 1 plus the grace lap; the post-grace lap never lands"
        );
        assert_eq!(
            cl.voided,
            vec![VoidedPass {
                at: SourceTime::from_micros(52_000_000),
                pass_ref: LogRef(4),
                void_ref: LogRef(4), // restore target = the pass itself (a marshal re-time)
                reason: VoidReason::AfterRaceEnd,
            }]
        );
        // No marker ⇒ bit-identical to the plain fold (a run that never expired).
        assert_eq!(
            lap_list_marshaled_with_floor(tagged(&log), None, None),
            lap_list_marshaled(tagged(&log))
        );
    }

    #[test]
    fn grace_rule_is_per_competitor_and_a_marshal_ruling_is_exempt() {
        let events = vec![
            pass("rh", "A", 1_000_000, Some(1)),  // offset 0
            pass("rh", "B", 1_100_000, Some(1)),  // offset 1
            pass("rh", "A", 18_000_000, Some(2)), // offset 2
            pass("rh", "B", 19_000_000, Some(2)), // offset 3 — marker sits here at offset 3
            pass("rh", "A", 35_000_000, Some(3)), // offset 4 — A's grace lap: counts
            pass("rh", "B", 36_000_000, Some(3)), // offset 5 — B's grace lap: counts
            pass("rh", "A", 52_000_000, Some(4)), // offset 6 — A again: void
            // offset 7 — a marshal INSERT (post-race by construction): exempt, and it does not
            // spend A's one allowed crossing (that was offset 4).
            inserted("rh", "A", 10_000_000),
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), None, Some(3));
        let a = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        let b = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "B")
            .unwrap();
        // A: holeshot(1s) + inserted(10s) + 18s + grace 35s ⇒ 3 laps; the 52s crossing voided.
        assert_eq!(
            a.laps.len(),
            3,
            "the marshal insert lands mid-chain, exempt"
        );
        assert_eq!(a.voided.len(), 1);
        assert_eq!(a.voided[0].reason, VoidReason::AfterRaceEnd);
        // B took exactly the one allowed crossing: two laps, nothing voided.
        assert_eq!(b.laps.len(), 2);
        assert!(b.voided.is_empty());
    }

    #[test]
    fn floor_runs_before_the_grace_rule_so_a_burst_does_not_spend_the_allowance() {
        // A reflection burst right at the post-buzzer line: the floor strikes the echoes
        // (UnderMinLap) and the pilot's ONE allowed crossing is the real one that closes their
        // lap — the burst must not spend it.
        let events = vec![
            pass("rh", "A", 1_000_000, Some(1)),  // offset 0 — holeshot
            pass("rh", "A", 18_000_000, Some(2)), // offset 1 — lap 1; marker at offset 1
            pass("rh", "A", 35_000_000, Some(3)), // offset 2 — grace lap: counts
            pass("rh", "A", 35_061_000, Some(4)), // offset 3 — reflection: UnderMinLap
            pass("rh", "A", 35_193_000, Some(5)), // offset 4 — reflection: UnderMinLap
            pass("rh", "A", 52_000_000, Some(6)), // offset 5 — real next lap: AfterRaceEnd
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), Some(10_000_000), Some(1));
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(cl.laps.len(), 2, "lap 1 + the grace lap");
        let reasons: Vec<VoidReason> = cl.voided.iter().map(|v| v.reason).collect();
        assert_eq!(
            reasons,
            vec![
                VoidReason::UnderMinLap,
                VoidReason::UnderMinLap,
                VoidReason::AfterRaceEnd
            ],
            "echoes fall to the floor; only the genuine extra lap falls to the grace rule"
        );
    }

    #[test]
    fn race_expired_offset_resolves_the_last_marker_for_the_heat() {
        let h = HeatId("q-1".into());
        let other = HeatId("q-2".into());
        let events = vec![
            pass("rh", "A", 1_000_000, Some(1)), // offset 0
            Event::RaceExpired {
                heat: h.clone(),
                deadline: Some(10),
            }, // offset 1 — an aborted run's stale marker
            Event::RaceExpired {
                heat: other.clone(),
                deadline: None,
            }, // offset 2 — another heat's marker: never ours
            Event::RaceExpired {
                heat: h.clone(),
                deadline: Some(20),
            }, // offset 3 — the standing marker: last one wins
        ];
        assert_eq!(race_expired_offset(tagged(&events), &h), Some(3));
        assert_eq!(race_expired_offset(tagged(&events), &other), Some(2));
        assert_eq!(
            race_expired_offset(tagged(&events), &HeatId("q-3".into())),
            None
        );
    }

    #[test]
    fn a_marshal_re_time_exempts_a_pass_from_the_floor() {
        // The RESTORE path: the floor suppressed a pass the marshal believes is real. An
        // AdjustLap re-asserting its raw instant is an explicit ruling — it outranks the
        // floor and the pass returns to the chain (whiff of a whoop track's 2s laps).
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 3_000_000, Some(2)), // offset 1 — 2s lap, under a 5s floor
            pass("vd", "A", 9_000_000, Some(3)), // offset 2
            adjusted(1, 3_000_000),              // offset 3 — marshal: "that 2s lap is real"
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), Some(5_000_000), None);
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(
            cl.laps
                .iter()
                .map(|l| l.duration_micros)
                .collect::<Vec<_>>(),
            vec![2_000_000, 6_000_000],
            "the blessed pass closes its lap despite the floor"
        );
        assert!(cl.voided.is_empty());
    }

    #[test]
    fn marshal_created_passes_are_never_floor_suppressed() {
        // An inserted pass is a ruling by construction — even one that closes a short lap
        // stands (the marshal typed the time; the floor guards raw detections only).
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),  // offset 0
            pass("vd", "A", 10_000_000, Some(2)), // offset 1
            inserted("vd", "A", 2_500_000),       // offset 2 — a 1.5s lap, by ruling
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), Some(5_000_000), None);
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(cl.laps.len(), 2, "the inserted pass closes its lap");
        assert!(cl.voided.is_empty());
    }

    #[test]
    fn a_burst_of_rapid_echoes_all_suppress_against_the_last_kept_pass() {
        // Three reflections inside the floor window: each compares against the last KEPT
        // pass, so the whole burst drops — not every-other one.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // kept (first)
            pass("vd", "A", 1_004_000, Some(2)), // echo — suppressed
            pass("vd", "A", 1_009_000, Some(3)), // echo — suppressed
            pass("vd", "A", 1_030_000, Some(4)), // echo — suppressed
            pass("vd", "A", 8_000_000, Some(5)), // real — kept (7s from last kept)
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), Some(5_000_000), None);
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(cl.laps.len(), 1);
        assert_eq!(cl.laps[0].duration_micros, 7_000_000);
        assert_eq!(cl.voided.len(), 3);
        assert!(
            cl.voided
                .iter()
                .all(|v| v.reason == VoidReason::UnderMinLap)
        );
    }

    #[test]
    fn floor_suppression_composes_with_marshal_voids() {
        // A marshal void recomputes the chain BEFORE the floor: voiding the first pass makes
        // the echo the new chain opener (kept — nothing precedes it), and both removal
        // reasons render side by side.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0 — marshal-voided below
            pass("vd", "A", 1_004_000, Some(2)), // offset 1 — becomes the opener
            pass("vd", "A", 8_000_000, Some(3)), // offset 2 — real lap
            voided(0),                           // offset 3
        ];
        let result = lap_list_marshaled_with_floor(tagged(&events), Some(5_000_000), None);
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(cl.laps.len(), 1, "opener (echo) -> real pass = one lap");
        assert_eq!(cl.voided.len(), 1);
        assert_eq!(cl.voided[0].reason, VoidReason::Marshal);
    }

    #[test]
    fn a_depth_three_void_chain_re_voids_the_base_pass() {
        // void(void(void(P))) — the RD removed, restored, and re-removed: last writer wins,
        // so P is voided again and back on the removal record. (The old two-level special
        // case silently no-opped here, leaving P alive against the newest ruling.)
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1
            pass("vd", "A", 6_000_000, Some(3)), // offset 2
            voided(1),                           // offset 3 — remove
            voided(3),                           // offset 4 — restore
            voided(4),                           // offset 5 — remove again
        ];
        let result = lap_list_marshaled(tagged(&events));
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(cl.laps.len(), 1, "0 -> 2 is the one surviving lap");
        assert_eq!(
            cl.voided,
            vec![VoidedPass {
                at: SourceTime::from_micros(4_000_000),
                pass_ref: LogRef(1),
                void_ref: LogRef(5),
                reason: VoidReason::Marshal,
            }]
        );
    }

    #[test]
    fn a_retimed_then_voided_pass_records_its_raw_instant() {
        // The removal record exists so RE-DETECTION recognises the crossing on the trace —
        // and the trace knows nothing of a re-time. Adjust 4.0s -> 14.0s, then void: the
        // record must say 4.0s (where the crossing physically is), not 14.0s.
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)), // offset 0
            pass("vd", "A", 4_000_000, Some(2)), // offset 1
            adjusted(1, 14_000_000),             // offset 2
            voided(1),                           // offset 3
        ];
        let result = lap_list_marshaled(tagged(&events));
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        assert_eq!(
            cl.voided,
            vec![VoidedPass {
                at: SourceTime::from_micros(4_000_000),
                pass_ref: LogRef(1),
                void_ref: LogRef(3),
                reason: VoidReason::Marshal,
            }]
        );
    }

    #[test]
    fn splitting_a_split_synthetic_pass_works_recursively() {
        // One 12s lap missed TWO crossings: split at 4s, then split the still-too-long
        // second half (the lap ending at the synthetic pass? no — ending at the raw pass)
        // by targeting the SYNTHETIC pass's own lap. The second split targets the first
        // split's offset and must resolve to a real source recursively (it used to vanish
        // silently while the audit showed it landed).
        let events = vec![
            pass("vd", "A", 1_000_000, Some(1)),  // offset 0 — opens
            pass("vd", "A", 13_000_000, Some(2)), // offset 1 — one 12s lap
            split(1, 5_000_000),                  // offset 2 — synthetic at 5s
            split(2, 9_000_000),                  // offset 3 — split the SYNTHETIC's chain
        ];
        let result = lap_list_marshaled(tagged(&events));
        let cl = result
            .competitors
            .iter()
            .find(|c| c.competitor.competitor.0 == "A")
            .unwrap();
        let durations: Vec<i64> = cl.laps.iter().map(|l| l.duration_micros).collect();
        assert_eq!(
            durations,
            vec![4_000_000, 4_000_000, 4_000_000],
            "three real laps: 1-5, 5-9, 9-13"
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
                    heat: None,
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
                    heat: None,
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

    /// A dense history **snapshot** (`base = 0`) — a whole trace, which replaces.
    fn history(adapter: &str, competitor: &str, times: &[i64], rssi: &[u16]) -> Event {
        history_at(adapter, competitor, 0, times, rssi)
    }

    /// A dense history **slice** starting at sample `base` — the live plugin's incremental shape.
    fn history_at(
        adapter: &str,
        competitor: &str,
        base: u64,
        times: &[i64],
        rssi: &[u16],
    ) -> Event {
        Event::SignalHistory(SignalHistory {
            adapter: AdapterId(adapter.into()),
            competitor: CompetitorRef(competitor.into()),
            times: times.to_vec(),
            rssi: rssi.to_vec(),
            base,
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
        // Two pulls for one competitor: the later dense history replaces the earlier one. Both are
        // snapshots (`base = 0`) — which is also how a pre-#392 log, carrying no offsets at all,
        // reads back, so those logs keep folding under exactly the rule they were written with.
        let log = vec![
            history("rh", "node-0", &[0, 100_000], &[70, 150]),
            history("rh", "node-0", &[0, 50_000, 100_000], &[70, 88, 150]),
        ];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![70, 88, 150]);
        assert_eq!(trace.period_micros, 50_000);
    }

    #[test]
    fn dense_history_slices_append_at_their_base() {
        // #392: the live plugin path streams the dense trace in slices, each stamped with the sample
        // offset it starts at. Contiguous slices APPEND, so the folded trace is the whole run even
        // though no single event ever carried it.
        let log = vec![
            history_at("rh", "node-0", 0, &[0, 50_000], &[70, 88]),
            history_at("rh", "node-0", 2, &[100_000, 150_000], &[150, 149]),
            history_at("rh", "node-0", 4, &[200_000], &[71]),
        ];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![70, 88, 150, 149, 71]);
        assert_eq!(
            trace.times.as_deref(),
            Some([0, 50_000, 100_000, 150_000, 200_000].as_slice())
        );
        assert_eq!(trace.from, Some(SourceTime::from_micros(0)));
        assert_eq!(trace.period_micros, 50_000);
    }

    #[test]
    fn dense_history_out_of_sync_slice_resyncs_on_time_and_never_duplicates() {
        // #448 changed this contract deliberately. A slice that neither restates the trace (base 0)
        // nor continues it (base == len) used to be DROPPED; that is what froze a long session's
        // live trace, because a producer-side prune rebases every subsequent base and none of them
        // ever lines up again. Each sample carries its own instant, so such a slice is now placed
        // by time instead: everything strictly newer than the last sample held is appended.
        let log = vec![
            history_at("rh", "node-0", 0, &[0, 50_000], &[70, 88]),
            // Gap: samples 2..4 never arrived, so this one starts past the end of the trace. The
            // sample is real evidence and is kept — `times` records the discontinuity honestly,
            // which is strictly better than discarding a crossing's signal to hide a gap.
            history_at("rh", "node-0", 4, &[200_000], &[71]),
            // ...and one that would re-apply samples already held. Its samples are not newer than
            // the last held instant, so it contributes nothing: no duplicate, no rewind.
            history_at("rh", "node-0", 1, &[50_000, 100_000], &[88, 150]),
        ];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![70, 88, 71]);
        assert_eq!(
            trace.times.as_deref(),
            Some([0, 50_000, 200_000].as_slice()),
            "the gap is visible in the times rather than papered over by dropping the sample"
        );

        // A slice arriving with no dense trace to place it against is still skipped: a window that
        // lost the opening snapshot cannot site an orphan fragment, and the competitor's coarse
        // chunks are the better evidence. (A prune never lands here — the plugin resets its
        // accumulator at race start, so every heat's own stream does begin at base 0.)
        let orphan = vec![
            chunk("rh", "node-0", 0, 100_000, &[60, 61]),
            history_at("rh", "node-0", 7, &[350_000], &[71]),
        ];
        let trace = &signal_trace(&orphan).competitors[0];
        assert_eq!(trace.samples, vec![60, 61]);
        assert_eq!(trace.times, None);
    }

    #[test]
    fn end_of_race_snapshot_resyncs_a_desynced_trace() {
        // The safety net the skip rule leans on: the plugin's end-of-race flush (and the post-race
        // `current_marshal_data` pull) send `base = 0`, so however the live stream went, the
        // finished heat's marshaling trace is the full-fidelity one.
        let log = vec![
            history_at("rh", "node-0", 0, &[0, 50_000], &[70, 88]),
            history_at("rh", "node-0", 4, &[200_000], &[71]), // out of sync, skipped
            history_at(
                "rh",
                "node-0",
                0,
                &[0, 50_000, 100_000, 150_000, 200_000],
                &[70, 88, 150, 149, 71],
            ),
        ];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.samples, vec![70, 88, 150, 149, 71]);
    }

    /// Emulate the plugin's incremental broadcaster (`plugins/gridfpv/__init__.py`
    /// `reconcile` + `broadcast_signal_once`) exactly, including the `SIGNAL_WINDOW` prune that
    /// rebases `acc['sent']`, and return the slices it would put on the wire plus the true full
    /// sample stream it saw.
    ///
    /// `window` stands in for `SIGNAL_WINDOW` (20000 in the plugin) so a prune is reachable in a
    /// readable test rather than only after a 20-minute open-practice run.
    fn plugin_signal_slices(window: usize, ticks: &[usize]) -> (Vec<Event>, Vec<i64>, Vec<u16>) {
        let (mut acc_t, mut acc_v, mut sent) = (Vec::<i64>::new(), Vec::<u16>::new(), 0usize);
        let (mut all_t, mut all_v) = (Vec::<i64>::new(), Vec::<u16>::new());
        let mut events = Vec::new();
        let mut clock = 0i64;

        for &new_samples in ticks {
            for _ in 0..new_samples {
                clock += 50_000;
                // A value that varies per sample, so a mis-splice shows up as wrong DATA and not
                // merely a wrong length.
                let value = (clock / 50_000) as u16 + 100;
                acc_t.push(clock);
                acc_v.push(value);
                all_t.push(clock);
                all_v.push(value);
            }
            // `reconcile`: drop the oldest past the window and REBASE the sent cursor by the same
            // amount. This is the line that desynchronised the fold (`__init__.py:944`).
            if acc_t.len() > window {
                let drop = acc_t.len() - window;
                acc_t.drain(..drop);
                acc_v.drain(..drop);
                sent = sent.saturating_sub(drop);
            }
            // `broadcast_signal_once`: the slice starts at the (rebased) cursor.
            events.push(history_at(
                "rh",
                "node-0",
                sent as u64,
                &acc_t[sent..],
                &acc_v[sent..],
            ));
            sent = acc_t.len();
        }
        (events, all_t, all_v)
    }

    /// **#448: a long run's live trace must not freeze when the plugin prunes its window.**
    ///
    /// Past `SIGNAL_WINDOW` samples the plugin drops its oldest and rebases `acc['sent']`, so every
    /// following slice's `base` is relative to a pruned accumulator while this fold holds the whole
    /// un-pruned trace. Under the old `base == len` rule nothing ever lined up again: every live
    /// tick was "out of sync", and the RSSI trace froze mid-heat with no error.
    #[test]
    fn a_producer_side_prune_does_not_freeze_the_live_trace() {
        // Ticks of 3 samples against a window of 8: the first two ticks fit, everything after
        // prunes, so most of this run is post-rebase.
        let (events, all_times, all_values) =
            plugin_signal_slices(8, &[3, 3, 3, 3, 3, 3, 3, 3, 3, 3]);
        assert_eq!(all_values.len(), 30, "the run produced 30 dense samples");

        let trace = &signal_trace(&events).competitors[0];
        assert_eq!(
            trace.samples, all_values,
            "every live tick lands: the trace is the whole run, not the two ticks before the \
             first prune"
        );
        assert_eq!(trace.times.as_deref(), Some(all_times.as_slice()));
    }

    /// The other half of #448: the end-of-race flush must not *shrink* the trace.
    ///
    /// That flush sends `base = 0` with the plugin's accumulator — which, after a prune, is only
    /// the last `SIGNAL_WINDOW` samples. Treating every `base == 0` as a wholesale replace threw
    /// away the head of the run that the fold alone still had.
    #[test]
    fn the_end_of_race_flush_never_truncates_a_longer_trace() {
        let (mut events, all_times, all_values) = plugin_signal_slices(8, &[3, 3, 3, 3, 3]);
        assert_eq!(all_values.len(), 15);

        // `broadcast_signal_once(final=True)`: base 0, carrying only the retained window.
        let tail_times = &all_times[all_values.len() - 8..];
        let tail_values = &all_values[all_values.len() - 8..];
        events.push(history_at("rh", "node-0", 0, tail_times, tail_values));

        let trace = &signal_trace(&events).competitors[0];
        assert_eq!(
            trace.samples, all_values,
            "the flush contributes nothing new and takes nothing away — the earliest samples of \
             the run survive it"
        );
        assert_eq!(trace.times.as_deref(), Some(all_times.as_slice()));
    }

    /// A genuine whole-trace snapshot — the post-race `current_marshal_data` pull, which starts at
    /// or before everything held — still supersedes, so a re-pull remains authoritative.
    #[test]
    fn a_whole_trace_snapshot_still_supersedes_the_live_stream() {
        let (mut events, all_times, _) = plugin_signal_slices(8, &[3, 3, 3, 3, 3]);
        // The post-race pull: the full run at full fidelity, re-stated from the start.
        let pulled: Vec<u16> = (0..all_times.len()).map(|i| 900 + i as u16).collect();
        events.push(history_at("rh", "node-0", 0, &all_times, &pulled));

        let trace = &signal_trace(&events).competitors[0];
        assert_eq!(
            trace.samples, pulled,
            "a snapshot that starts no later than what we hold replaces it — the marshal pull is \
             the authority on a finished heat"
        );
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

    #[test]
    fn dense_history_with_repeated_timestamp_derives_a_positive_period() {
        // A live dense history (RH `history_values`) can repeat a timestamp — a peak reported at the
        // same first/last time gives `[t, t, …]`. The grid period must skip the zero delta and use
        // the first POSITIVE one, not collapse to a degenerate 0.
        let log = vec![history(
            "rh",
            "node-0",
            &[1_000, 1_000, 1_100, 1_100],
            &[70, 150, 150, 71],
        )];
        let trace = &signal_trace(&log).competitors[0];
        assert_eq!(trace.from, Some(SourceTime::from_micros(1_000)));
        assert_eq!(
            trace.period_micros, 100,
            "first positive delta (1_100 - 1_000), skipping the leading 0"
        );
        assert_eq!(trace.samples, vec![70, 150, 150, 71]);
    }

    // --- Lineup seeding: the zero-lap competitor (#388) --------------------------------------

    /// Build a heat's `HeatScheduled` with the given lineup refs.
    fn scheduled(heat: &str, lineup: &[&str]) -> Event {
        Event::HeatScheduled {
            heat: gridfpv_events::HeatId(heat.into()),
            lineup: lineup.iter().map(|r| CompetitorRef((*r).into())).collect(),
            class: None,
            round: None,
            frequencies: vec![],
            label: None,
        }
    }

    #[test]
    fn a_lineup_competitor_with_no_passes_is_present_with_zero_laps() {
        // The field failure (#388): node-1's gate never detected a crossing. Before the fix it
        // vanished from the lap list entirely and could not be marshaled — which is exactly when
        // marshaling matters most. Its RSSI still streamed, so it IS in the window.
        let log = vec![
            scheduled("q-1", &["node-0", "node-1"]),
            pass("rh", "node-0", 0, Some(0)),
            pass("rh", "node-0", 2_000_000, Some(1)),
            chunk("rh", "node-1", 0, 100_000, &[70, 71, 70]),
        ];
        let list = lap_list_marshaled(tagged(&log));
        let silent = list
            .competitor(&key("rh", "node-1"))
            .expect("the silent lineup competitor must be present");
        assert!(silent.laps.is_empty(), "no detections => no laps");
        assert!(silent.voided.is_empty());
        // And its trace is right there, keyed identically — so the console can render it.
        assert!(
            signal_trace(&log)
                .competitor(&key("rh", "node-1"))
                .is_some(),
            "the zero-lap competitor's trace must key to the same CompetitorKey"
        );
        // The competitor that DID fly is unchanged.
        assert_eq!(laps_of(&list, "rh", "node-0"), vec![ld(1, 2_000_000)]);
    }

    #[test]
    fn inserting_a_lap_on_a_zero_lap_competitor_builds_its_lap_list() {
        // The recovery path: the RD reconstructs the missed race from the trace. Two inserts
        // make one lap; the seeded entry becomes a real one rather than appearing from nowhere.
        let mut log = vec![
            scheduled("q-1", &["node-0", "node-1"]),
            chunk("rh", "node-1", 0, 100_000, &[70, 150, 70]),
        ];
        assert_eq!(
            lap_list_marshaled(tagged(&log))
                .competitor(&key("rh", "node-1"))
                .map(|c| c.laps.len()),
            Some(0)
        );
        log.push(inserted("rh", "node-1", 1_000_000));
        log.push(inserted("rh", "node-1", 4_000_000));
        assert_eq!(
            laps_of(&lap_list_marshaled(tagged(&log)), "rh", "node-1"),
            vec![ld(1, 3_000_000)],
            "the marshal's two inserted crossings are one recovered lap"
        );
    }

    #[test]
    fn a_lineup_competitor_seats_on_the_adapter_that_saw_it() {
        // Two sources in the window; node-1 is only ever named by `rh-b`, so that is the seat
        // its (empty) entry takes — not `rh-a` merely because it sorts first.
        let log = vec![
            scheduled("q-1", &["node-0", "node-1"]),
            pass("rh-a", "node-0", 0, Some(0)),
            Event::CompetitorSeen {
                adapter: AdapterId("rh-b".into()),
                competitor: CompetitorRef("node-1".into()),
            },
        ];
        let list = lap_list_marshaled(tagged(&log));
        assert!(list.competitor(&key("rh-b", "node-1")).is_some());
        assert!(
            list.competitor(&key("rh-a", "node-1")).is_none(),
            "a competitor must not be invented on a source that never saw it"
        );
    }

    #[test]
    fn a_lineup_competitor_never_named_falls_back_to_the_only_source() {
        // Nothing at all was heard from node-1 — not even RSSI. There is exactly one timer in
        // evidence, so that is unambiguously its seat, and the RD can still insert its laps.
        let log = vec![
            scheduled("q-1", &["node-0", "node-1"]),
            pass("rh", "node-0", 0, Some(0)),
        ];
        assert!(
            lap_list_marshaled(tagged(&log))
                .competitor(&key("rh", "node-1"))
                .is_some_and(|c| c.laps.is_empty())
        );
    }

    #[test]
    fn a_lineup_with_no_source_in_evidence_seeds_nothing() {
        // A bare `HeatScheduled` and nothing else: there is no adapter to address a correction
        // to, so inventing a seat would only produce an unusable row.
        let log = vec![scheduled("q-1", &["node-0"])];
        assert!(lap_list_marshaled(tagged(&log)).competitors.is_empty());
    }

    #[test]
    fn lineup_seeding_is_order_independent_and_idempotent() {
        // The projection must be a pure fold: seeding a competitor that also has passes must
        // not duplicate it, and folding twice must be identical.
        let log = vec![
            pass("rh", "node-0", 0, Some(0)),
            scheduled("q-1", &["node-0", "node-1"]),
            chunk("rh", "node-1", 0, 100_000, &[70]),
            pass("rh", "node-0", 2_000_000, Some(1)),
        ];
        let once = lap_list_marshaled(tagged(&log));
        assert_eq!(once, lap_list_marshaled(tagged(&log)));
        assert_eq!(
            once.competitors.len(),
            2,
            "node-0 is seeded AND has passes — one entry, not two: {once:?}"
        );
    }

    // --- Lineup seeding is LAST-WINS, not a union (#443) ---------------------------------------

    /// A seating override re-emits `HeatScheduled` with the new lineup, and the heat window
    /// keeps **both** entries (`heat_window_offsets` does not filter heat-loop events). The
    /// heat's lineup is its **most recent** `HeatScheduled` — the same last-wins rule
    /// `live_state::lineup_of` / `latest_schedule` fold by — so a pilot the override seated
    /// OUT is not in the heat and must not be seeded into marshaling.
    ///
    /// Unioning the two lineups offers the marshal a zero-lap row for a pilot who never flew
    /// this heat, on a screen whose whole purpose is entering laps against a row — and it puts
    /// the two live surfaces into disagreement about who is in the heat.
    #[test]
    fn a_seating_override_drops_the_reseated_pilot_from_the_marshaled_lap_list() {
        let log = vec![
            // Filled from the round's plan …
            scheduled("q-1", &["node-0", "node-1"]),
            // … then the RD re-seats it: node-1 out, node-2 in.
            scheduled("q-1", &["node-0", "node-2"]),
            pass("rh", "node-0", 0, Some(0)),
            pass("rh", "node-0", 2_000_000, Some(1)),
        ];

        let list = lap_list_marshaled(tagged(&log));
        assert!(
            list.competitor(&key("rh", "node-1")).is_none(),
            "node-1 was seated out before the heat ran — a zero-lap marshaling row invites laps \
             against a pilot who was never in this heat: {list:?}"
        );
        assert!(
            list.competitor(&key("rh", "node-2"))
                .is_some_and(|c| c.laps.is_empty()),
            "node-2 IS in the heat and was never detected — it must still be marshalable: {list:?}"
        );
        assert_eq!(
            laps_of(&list, "rh", "node-0"),
            vec![ld(1, 2_000_000)],
            "the pilot both lineups name is unaffected"
        );

        // And the seat fold the lap list is built on, stated directly — the bug's own site.
        let seats: Vec<String> = lineup_keys(&log)
            .into_iter()
            .map(|k| k.competitor.0)
            .collect();
        assert_eq!(
            seats,
            vec!["node-0".to_string(), "node-2".to_string()],
            "the heat's lineup is the LAST HeatScheduled, not every lineup it ever had"
        );
    }

    // --- Voiding the SOLE detection: the entry survives, emptied ------------------------------

    #[test]
    fn voiding_a_competitors_only_pass_leaves_it_present_with_zero_laps() {
        // The contract the live marshaling e2es assert against: a void never makes a competitor
        // VANISH. It empties the lap chain and leaves the removal record behind, so the RD can
        // still see (and un-void) the ruling they just made. Deliberately NO `HeatScheduled`
        // here, so the lineup seeding (#388) cannot be what keeps the entry alive: the removal
        // record alone does it, and has since voids began carrying one.
        let log = vec![pass("rh", "node-0", 1_000_000, Some(0)), voided(0)];
        let list = lap_list_marshaled(tagged(&log));
        let cl = list
            .competitor(&key("rh", "node-0"))
            .expect("voiding the only pass must not drop the competitor from the lap list");
        assert!(cl.laps.is_empty(), "no surviving passes => no laps");
        assert_eq!(
            cl.voided.len(),
            1,
            "the void it stands on is the removal record: {cl:?}"
        );
        // And the corrected pass stream — the honest detection count — really is empty.
        assert!(corrected_passes(tagged(&log)).is_empty());
    }

    #[test]
    fn voiding_the_only_pass_of_a_lined_up_competitor_keeps_the_seeded_entry() {
        // Same thing with the lineup present (the shape a real heat log has): the entry is
        // doubly anchored — seeded from `HeatScheduled` AND carrying the removal record — and
        // is still ONE entry, with zero laps.
        let log = vec![
            scheduled("q-1", &["node-0"]),
            pass("rh", "node-0", 1_000_000, Some(0)),
            voided(1),
        ];
        let list = lap_list_marshaled(tagged(&log));
        assert_eq!(list.competitors.len(), 1, "one entry, not two: {list:?}");
        let cl = list.competitor(&key("rh", "node-0")).expect("present");
        assert!(cl.laps.is_empty());
        assert_eq!(cl.voided.len(), 1);
    }

    #[test]
    fn a_void_removes_exactly_one_detection_from_the_corrected_stream() {
        // The invariant the live e2es measure, stated on the honest metric: the number of
        // SURVIVING lap-gate passes drops by exactly one per void — at every count, including
        // the 1 -> 0 edge. `laps.len() + 1` cannot express this (it reads 1 for an emptied
        // entry), which is why the live tests must count corrected passes instead.
        for n in 1..=4usize {
            let mut log: Vec<Event> = (0..n)
                .map(|i| pass("rh", "node-0", 1_000_000 * (i as i64 + 1), Some(i as u64)))
                .collect();
            let before = corrected_passes(tagged(&log)).len();
            assert_eq!(before, n);
            log.push(voided(0));
            assert_eq!(
                corrected_passes(tagged(&log)).len(),
                n - 1,
                "voiding one of {n} detections must leave {} ",
                n - 1
            );
        }
    }

    /// **A bounded crossing read is exactly the tail of the unbounded one (#460 item 2).**
    ///
    /// The live projection used to materialise every crossing of a possibly-unbounded run and then
    /// keep the last `MAX_LIVE_CROSSINGS`; the bound now goes into the fold, which finds the cutoff
    /// offset first and builds only the crossings at or above it. That is a pure efficiency change
    /// only if it is byte-identical to the slice it replaces — including `lap_number`, which is a
    /// position in the *whole* chain and must not be renumbered from the truncated head.
    ///
    /// The fixture interleaves seats, a marshal void and a floor rejection so the tail spans all
    /// four dispositions and both input lists (surviving and the removal record).
    #[test]
    fn a_bounded_crossing_read_is_exactly_the_tail_of_the_unbounded_one() {
        let mut log = vec![scheduled("q-1", &["node-0", "node-1"])];
        for lap in 0..12i64 {
            log.push(pass("rh", "node-0", lap * 30_000_000, None));
            log.push(pass("rh", "node-1", lap * 30_000_000 + 1_000_000, None));
            // A sub-floor echo right behind node-1's crossing — auto-suppressed under the floor.
            log.push(pass("rh", "node-1", lap * 30_000_000 + 1_100_000, None));
        }
        // And one crossing the marshal removed outright, mid-run.
        log.push(voided(10));

        let floor = Some(5_000_000);
        let window = CorrectedWindow::of(tagged(&log), floor, None);
        let full = window.crossings(None);
        assert!(
            full.len() > 8,
            "the fixture must overflow every bound under test ({} crossings)",
            full.len()
        );
        assert!(
            full.iter()
                .any(|d| d.disposition == CrossingDisposition::RejectedTooShort)
                && full
                    .iter()
                    .any(|d| d.disposition == CrossingDisposition::VoidedByMarshal)
                && full
                    .iter()
                    .any(|d| d.disposition == CrossingDisposition::Holeshot),
            "the fixture must exercise the removal record as well as the surviving chain"
        );

        for limit in [1usize, 2, 8, full.len() - 1, full.len(), full.len() + 5] {
            let bounded = window.crossings(Some(limit));
            let expected = &full[full.len().saturating_sub(limit)..];
            assert_eq!(
                bounded, expected,
                "crossings(Some({limit})) must equal the last {limit} of the unbounded read"
            );
        }

        // Degenerate, but it must not panic or quietly return everything.
        assert!(window.crossings(Some(0)).is_empty());

        // And the free function still answers the whole run.
        assert_eq!(dispositioned_passes(tagged(&log), floor, None), full);
    }

    /// **One fold, two views.** The lap list and the crossing feed are now read off a single
    /// [`CorrectedWindow`], so this pins that each still equals what its own standalone entry point
    /// produced when they each ran the correction fold themselves.
    #[test]
    fn the_shared_fold_agrees_with_both_standalone_entry_points() {
        let log = vec![
            scheduled("q-1", &["node-0", "node-1", "node-2"]),
            pass("rh", "node-0", 0, None),
            pass("rh", "node-1", 1_000_000, None),
            pass("rh", "node-0", 30_000_000, None),
            pass("rh", "node-0", 30_100_000, None), // sub-floor echo
            pass("rh", "node-1", 31_000_000, None),
            voided(3),
            pass("rh", "node-0", 60_000_000, None),
        ];
        let floor = Some(5_000_000);
        let window = CorrectedWindow::of(tagged(&log), floor, None);
        assert_eq!(
            window.crossings(None),
            dispositioned_passes(tagged(&log), floor, None)
        );
        assert_eq!(
            window.into_lap_list(),
            lap_list_marshaled_with_floor(tagged(&log), floor, None)
        );
    }

    #[test]
    fn an_un_lined_up_competitor_still_appears_from_its_passes() {
        // Seeding is additive: a competitor the timer saw but the lineup never named (a
        // mis-seated node, a late re-seat) must not be dropped.
        let log = vec![
            scheduled("q-1", &["node-0"]),
            pass("rh", "node-3", 0, Some(0)),
            pass("rh", "node-3", 1_000_000, Some(1)),
        ];
        assert_eq!(
            laps_of(&lap_list_marshaled(tagged(&log)), "rh", "node-3"),
            vec![ld(1, 1_000_000)]
        );
    }
}
