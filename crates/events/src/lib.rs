//! Canonical event model for GridFPV — the schema of the append-only log.
//!
//! Every live timing source sits behind one adapter interface and is translated into
//! this small, source-agnostic set of events. This is the vocabulary the whole log
//! speaks; everything else — laps, results, standings — is derived downstream by
//! pure folds.
//!
//! This is the **skeleton vocabulary**: the raw observations a timer reports. The
//! competition facts of `docs/primitives.md` §5 (race, phase, heat, ruling) are added
//! here as those primitives are built.
//!
//! Design notes:
//! - Adapters emit **raw observations** ("a gate was crossed"), never derivations.
//!   A lap is two consecutive [`Pass`]es, computed identically for every source by
//!   a fold over the log — not reported here.
//! - The [`Pass`] is the universal atom; richer detail (signal context, splits) is
//!   *optional*, so a simulator with nothing under the crossing and hardware with
//!   full RSSI both fit the same model without special-casing.
//! - Source timestamps are authoritative for intervals; the source's own clock is
//!   kept straight rather than flattened on arrival (see [`SourceTime`]).
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use ts_rs::TS;

// Every public type derives `ts_rs::TS` and exports into a repo-root `bindings/`
// directory. ts-rs resolves each `export_to` path against `TS_RS_EXPORT_DIR`, which
// `cargo xtask gen` pins to the workspace root — so the files always land in
// `<repo>/bindings/`. Regenerated and drift-checked by `cargo xtask gen` (#4).

/// Identifies the timing source / adapter that produced an event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct AdapterId(pub String);

/// A **seat** on a timer, by the timer's own handle for it: a receiver slot, a sim player
/// name, a transponder id (`docs/primitives.md` §3.2). A seat is linked to a pilot only by a
/// race's lineup ([`LineupEntry`]) — never by the adapter, which just reports what it sees.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct SeatRef(pub String);

/// A source's own race/heat identifier, where it exposes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct SessionId(pub String);

/// A GridFPV pilot's stable identity — the person a source-local [`SeatRef`] is
/// *bound* to by the Director, never by the adapter.
///
/// This is the canonical pilot handle the whole stack shares: the log and the wire
/// layer address a pilot by the same type, so they never disagree on what a pilot id is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct PilotId(pub String);

/// A timestamp in the **source's own clock**, stored as microseconds.
///
/// Lap and split durations are computed from these values, which are internally
/// consistent and immune to network jitter. Game-engine time (Velocidrone), server
/// time (RotorHazard), device RTC (LapRF) and wall clock (manual) all reduce to a
/// microsecond count on their own timeline; the adapter maps that timeline onto the
/// Director's session axis separately. Integer microseconds keep interval math exact
/// and comparisons stable (no float-equality hazards in tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
// serde flattens this to the bare `micros` integer (transparent). `#[ts(as = "f64")]`
// renders it as a plain TS `number`: our source times are microsecond counts bounded
// far below 2^53, so `number` is exact and avoids the wire/type mismatch a wide-integer
// TS mapping would introduce.
#[ts(export, export_to = "bindings/", as = "f64")]
pub struct SourceTime {
    /// Microseconds on the source's clock.
    pub micros: i64,
}

impl SourceTime {
    /// Construct from a microsecond count.
    pub const fn from_micros(micros: i64) -> Self {
        Self { micros }
    }

    /// Signed microseconds between two source times (`self - earlier`). Only
    /// meaningful for times from the *same* source clock.
    pub const fn micros_since(self, earlier: SourceTime) -> i64 {
        self.micros - earlier.micros
    }
}

/// Which gate a [`Pass`] crossed. The lap gate is index `0`; higher indices are
/// splits for sources that report multiple gates (Velocidrone). Lap derivation
/// counts lap-gate passes; splits are intermediate detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct GateIndex(pub u32);

impl GateIndex {
    /// The lap gate (start/finish line).
    pub const LAP: GateIndex = GateIndex(0);

    /// Whether this is the lap gate (vs. an intermediate split). Takes `&self` so
    /// it doubles as the serde `skip_serializing_if` predicate for [`Pass::gate`].
    pub const fn is_lap_gate(&self) -> bool {
        self.0 == Self::LAP.0
    }
}

impl Default for GateIndex {
    fn default() -> Self {
        Self::LAP
    }
}

/// Optional signal detail beneath a crossing, present only where the source has it
/// (RotorHazard/LapRF). A simulator reports an exact crossing with nothing
/// underneath, so this is `None` there — and signal-based lap recovery is simply
/// unavailable for such adapters. Kept minimal for now; hardware adapters (v0.2)
/// extend it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalContext {
    /// Peak RSSI at the crossing, if reported.
    pub rssi_peak: Option<f32>,
}

/// What a signal level's number means. A timer reports levels in its own unit, and Grid
/// keeps them exactly as reported: it never converts one unit to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum SignalUnit {
    /// The timer's own counts, with no physical unit (RotorHazard's filtered ADC counts).
    Counts,
    /// Tenths of a dB, relative: only differences mean anything (an uncalibrated OFT Timer).
    TenthsDb,
    /// Tenths of a dBm (a calibrated OFT Timer).
    TenthsDbm,
}

/// A run of signal samples for one seat, captured from a timer that reports signal.
///
/// The trace is the **evidence** a marshal later reviews a call against. It is logged in
/// chunks as the race runs, so it interleaves with the [`Pass`]es in the same log and
/// survives an aborted race. Each chunk is a window of `levels` beginning at `from` on the
/// timer's clock, one every `period_micros`. A fold joins a seat's chunks back into one
/// trace.
///
/// Levels are integers in the timer's own [`SignalUnit`], exactly as reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalChunk {
    /// The timing source that produced this trace.
    pub adapter: AdapterId,
    /// The source-local seat the trace belongs to.
    pub seat: SeatRef,
    /// The source-clock timestamp of the **first** sample in `levels`.
    pub from: SourceTime,
    /// Microseconds between consecutive samples.
    #[ts(type = "number")]
    pub period_micros: u32,
    /// The samples, oldest first, one every `period_micros`.
    pub levels: Vec<i32>,
    pub unit: SignalUnit,
    /// The timer's noise floor at the end of this chunk, in the same unit, where it
    /// reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub noise_floor: Option<i32>,
}

/// The enter and exit levels a timer detects crossings against for one seat. Logged
/// alongside the trace so a marshal can see *why* the timer called (or missed) a lap. The
/// last one per `(adapter, seat)` wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalThresholds {
    /// The timing source these thresholds belong to.
    pub adapter: AdapterId,
    /// The source-local seat the thresholds apply to.
    pub seat: SeatRef,
    /// The signal rising above this opens a crossing.
    pub enter: i32,
    /// The signal falling below this closes it.
    pub exit: i32,
    pub unit: SignalUnit,
}

/// The **dense, full-fidelity** per-node RSSI history for a heat — the detector's own internal
/// sampling, pulled from RotorHazard's request-driven `current_marshal_data` at heat end.
///
/// Unlike the live-streamed [`SignalChunk`] (one aggregate sample per `node_data` heartbeat emit,
/// the *coarse* trace), this is the trace RotorHazard's own marshal page reviews against: every
/// per-tick sample the node recorded, with its **own** sample time. A signal-capable adapter pulls
/// it once when a heat reaches `DONE` and appends one `SignalHistory` per node. A fold over the
/// trace should **prefer** this dense history over the coarse [`SignalChunk`] samples for a
/// seat when both are present (the dense trace supersedes the streaming approximation).
///
/// # Whole trace or slice — [`base`](SignalHistory::base) says which (#392)
///
/// The GridFPV plugin streams this trace **live**, every 0.5 s, so a `SignalHistory` is not always a
/// whole trace: [`base`](SignalHistory::base) is the sample offset its `times`/`rssi` start at, and
/// the fold replaces at `0`, appends at the trace's current length, and skips anything else. Sending
/// the accumulated whole on every tick made the per-tick cost grow with heat length (O(n) per tick,
/// O(n²) per heat, per seat): it flooded the heat's log and re-woke every projection subscriber
/// twice a second with nothing new in hand (#392).
///
/// # Why explicit per-sample times (not a uniform cadence)
///
/// [`SignalChunk`] assumes a fixed `period_micros` because the live stream emits on a regular
/// heartbeat. The detector's internal history is **not** guaranteed uniform — RotorHazard reports a
/// parallel `history_times`/`history_values` pair — so this carries the per-sample times directly
/// (`times[i]` is the source-clock instant of `rssi[i]`), preserving native fidelity with **no
/// resampling** (the load-bearing fidelity caution). Times are race-relative
/// microseconds on the same clock as [`Pass::at`] and the chunk time base, so lap markers and the
/// dense trace align.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalHistory {
    /// The timing source that produced this history.
    pub adapter: AdapterId,
    /// The source-local seat the history belongs to.
    pub seat: SeatRef,
    /// The source-clock timestamp (race-relative µs) of each sample, parallel to `rssi`. Same
    /// length as `rssi`; `times[i]` is when `rssi[i]` was recorded. Renders as TS `number[]`
    /// (bounded far below 2^53).
    #[ts(type = "number[]")]
    pub times: Vec<i64>,
    /// The dense per-tick RSSI samples (filtered ADC counts), parallel to `times`.
    pub rssi: Vec<u16>,
    /// The index of this event's **first** sample within the seat's full trace — the offset
    /// the fold applies `times`/`rssi` at (#392). Named for the plugin's own `gridfpv_signal` field,
    /// whose semantics it carries onto the canonical wire unchanged.
    ///
    /// - `base == 0` — a **full snapshot**: it *replaces* the seat's trace. The post-race
    ///   `current_marshal_data` pull, the `race_details` fold and the plugin's end-of-race flush
    ///   each send one, so a finished heat's marshaling trace is always whole.
    /// - `base ==` the trace's current length — a **contiguous append**: `times`/`rssi` extend it.
    ///   This is the live path, and it is what keeps the per-tick cost independent of heat length.
    /// - anything else — **out of sync**: the fold skips the event rather than splice a gap or a
    ///   duplicate into the trace. The next full snapshot resynchronises it.
    ///
    /// Additive on the wire: `#[serde(default)]` reads a pre-#392 log — which only ever carried
    /// whole traces — back as `base = 0`, i.e. exactly the replace semantics it was written with.
    /// Renders as TS `number` (a sample count, bounded far below 2^53), not a `bigint`.
    #[serde(default)]
    #[ts(type = "number")]
    pub base: u64,
}

/// A gate crossing — the one observation everything else derives from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Pass {
    /// Which timing source produced this pass.
    pub adapter: AdapterId,
    /// The source's own seat handle.
    pub seat: SeatRef,
    /// When the crossing happened, in the source's clock.
    pub at: SourceTime,
    /// A source-provided monotonic sequence number, where one exists. Disambiguates
    /// passes that share a timestamp and survives clock adjustments; also the basis
    /// for reconnect deduplication.
    // serde skips this when `None`; `#[ts(optional)]` mirrors that as `sequence?:`.
    // `#[ts(type = "number")]` renders the sequence as a plain TS `number` (it is
    // bounded far below 2^53), not a `bigint`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub sequence: Option<u64>,
    /// The gate crossed; defaults to the lap gate.
    #[serde(default, skip_serializing_if = "GateIndex::is_lap_gate")]
    pub gate: GateIndex,
    /// Optional signal context (hardware only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub signal: Option<SignalContext>,
    /// The race this pass was recorded **for** — stamped by the Director at append time
    /// (the adapter doesn't know the race; the Director does). `None` for a pass logged
    /// while no race was running. See `docs/primitives.md` §5.10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub race: Option<RaceId>,
}

// --- Ids the competition model shares ----------------------------------------
// The competition vocabulary itself (race, phase, heat, ruling facts) is defined by
// `docs/primitives.md` and is added as those primitives are built.

/// Identifies one **race**: a single start-to-stop flight (`docs/primitives.md` §5.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct RaceId(pub String);

/// Identifies a **class** (`docs/primitives.md` §2): a named racing category.
///
/// This is the **canonical** class handle the whole stack shares: the log and the wire
/// layer address a class by the same type, so they never disagree on what a class id
/// is. A transparent string newtype like the other event-model ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct ClassId(pub String);

/// A reference to an already-logged event by its append **offset** — the stable id a
/// later fact uses to point at an earlier one (e.g. a ruling on *this* pass). The offset
/// is assigned by the storage layer when the target event was appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
// `#[ts(as = "f64")]` renders the offset as a plain TS `number`: log offsets are
// bounded far below 2^53 in our domain, so `number` is exact and avoids a wide-integer
// wire mismatch.
#[ts(export, export_to = "bindings/", as = "f64")]
pub struct LogRef(pub u64);

/// Identifies one **phase**: field in, ranking out (`docs/primitives.md` §5.2). A heat is the
/// leaf phase, so a heat's id is a `PhaseId` too.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct PhaseId(pub String);

/// One place in a race's lineup: a seat on a timer, and the pilot flying on it.
///
/// The lineup **is** the link between a pilot and a seat; there is no separate registration
/// step (`docs/primitives.md` §3.2). `pilot` is `None` for an open seat, which only an
/// unranked phase such as practice allows (§5.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LineupEntry {
    /// The timer the seat is on.
    pub adapter: AdapterId,
    /// The seat, by the timer's own handle.
    pub seat: SeatRef,
    /// The pilot on the seat, or `None` for an open seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pilot: Option<PilotId>,
}

/// Who owned the start of a race (`docs/primitives.md` §3.1). Timers differ: Grid starts a
/// race on a timer that always listens, Grid starts it and tells a timer that needs telling,
/// or the source starts it and Grid follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum StartOwner {
    /// Grid started the race and told the timer nothing (OFT).
    Grid,
    /// Grid started the race and told the timer to start (RotorHazard).
    GridCommandsTimer,
    /// The source started the race and Grid followed (Velocidrone).
    Source,
}

/// Everything fixed about a race when it starts: which heat it is a race of, who is on which
/// seat, and who owned the start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RaceSetup {
    /// The race's own id. Every race has one, including a re-run of the same heat.
    pub race: RaceId,
    /// The heat this is a race of.
    pub heat: PhaseId,
    /// Who is on which seat.
    pub lineup: Vec<LineupEntry>,
    /// Who owned the start.
    pub owner: StartOwner,
}

/// Why a race was discarded. Optional, and one tap to pick (`docs/primitives.md` §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum DiscardReason {
    /// A pilot left before the start tone. The pilot is named by a separate false start
    /// ruling, so it can be counted.
    FalseStart,
    /// A crash or an obstruction on the track.
    CrashOrObstruction,
    /// The timer misbehaved.
    TimerProblem,
    /// Anything else.
    Other,
}

/// Why a lap was removed. Optional, one tap to pick, and **a label only**: it never changes
/// what the removal does (`docs/primitives.md` §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum RemovalReason {
    /// The timer saw a crossing that did not happen.
    FalseDetection,
    /// A real crossing, but the lap it ended should not stand.
    IncorrectLap,
    /// Anything else.
    Other,
}

// --- Rules as data (docs/primitives.md §5.5, §5.6) ----------------------------
// A format, its race rules, its completion rule and its ranking rule are plain data. The
// named rules Grid ships are values of these types, not special code, so a rule editor can
// come later with no engine change.

/// How one race ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum EndCondition {
    /// The race is won by the first pilot to finish this many laps.
    FirstToLaps(u32),
    /// The race runs for a fixed time, then a grace window to finish the lap in progress.
    Timed {
        #[ts(type = "number")]
        micros: i64,
        #[ts(type = "number")]
        grace_micros: i64,
    },
    /// The race runs until the director stops it.
    Open,
}

/// The settings for one race. They are never set on a race: they come from the format in
/// force for its heat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RaceRules {
    /// How the race ends.
    pub end: EndCondition,
    /// The length of the staging countdown before the start tone, in microseconds.
    #[ts(type = "number")]
    pub staging_micros: i64,
    /// The shortest time a lap may take, in microseconds. A faster crossing is filtered out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub min_lap_micros: Option<i64>,
    /// What starts a pilot's first lap. The default is the hole shot.
    #[serde(default)]
    pub first_lap: FirstLap,
    /// For a first-to-N race: stop the race by itself this many microseconds after every
    /// pilot in it has finished. `None`, the default, leaves the stop to the director. The
    /// wait is there for a track where extra crossings are likely and laps may need
    /// sorting out before the race is called over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub stop_after_all_finish_micros: Option<i64>,
    /// For a race that is not timed (first-to-N or open): the longest it may run, in
    /// microseconds from the start. The race stops by itself when it gets there, whoever
    /// has or has not finished. `None`, the default, sets no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub max_micros: Option<i64>,
}

/// What starts a pilot's first lap in a race.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum FirstLap {
    /// The pilot's first crossing (the hole shot) starts lap 1. That crossing has no lap
    /// time of its own.
    #[default]
    HoleShot,
    /// The start tone starts lap 1, so the first crossing ends it. This needs the tone's
    /// time on the timer's clock; a timer that cannot give it falls back to the hole shot.
    FromTone,
}

/// One number Grid folds for a pilot from the saved races of a heat. Rules are built from
/// these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Measure {
    /// Races won. More is better.
    Wins,
    /// Points scored across the races, from the format's points table. More is better.
    Points,
    /// The most laps flown in one race. More is better.
    Laps,
    /// The fastest single lap in any race. Less is better.
    BestLap,
    /// The fastest run of this many laps in a row in any race. Less is better.
    BestConsecutive(u32),
    /// The best finishing position in any race. Less is better.
    BestFinish,
    /// The finishing position in the latest race. Less is better.
    LastFinish,
}

/// How a heat's results become an order: the measures to sort by, first one first. Every
/// ranking then ends the same way — pilots who never flew go below those who did, and the
/// last tie-break is seed — so a ranking is always a strict order of the whole field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RankingRule {
    pub sort: Vec<Measure>,
}

/// When a heat is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum CompletionRule {
    /// After this many saved races.
    SavedRaces(u32),
    /// When any pilot's value for a measure reaches a level ("any pilot's wins ≥ 2").
    AnyPilotReaches {
        measure: Measure,
        #[ts(type = "number")]
        at_least: i64,
    },
    /// When the director says so.
    DirectorDecides,
}

/// A named, reusable bundle of rules. It is set on a phase and applies to every heat
/// beneath it; a phase lower down can set its own, and the nearest one wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Format {
    /// The director's name for it ("2 minute qualifying", "Chase the Ace").
    pub name: String,
    /// The settings for each race.
    pub race: RaceRules,
    /// When a heat under this format is done.
    pub completion: CompletionRule,
    /// How a heat under this format is ranked. `None` for an unranked heat (practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ranking: Option<RankingRule>,
    /// Points by finishing position, first place first. A position past the end scores 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<u32>,
}

// --- The phase tree (docs/primitives.md §5.2 – §5.4) ---------------------------

/// One part of a phase's field. A field is built from one or more slices, in order, and
/// that order is the seed order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum FieldSlice {
    /// Every pilot on a class's roster, in roster order.
    Class(ClassId),
    /// One named pilot.
    Pilot(PilotId),
    /// Positions `from` to `to` (1-based, both included) of another phase's ranking. It
    /// resolves when that phase is complete.
    FromPhase { phase: PhaseId, from: u32, to: u32 },
    /// A list brought in from outside (a Velocidrone leaderboard, a MultiGP list), already
    /// in seed order.
    Imported(Vec<PilotId>),
}

/// Who a seat in a heat's lineup is for: the **seat reference**.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum SeatSource {
    /// A named pilot.
    Pilot(PilotId),
    /// Whoever finishes at `position` (1-based) in another phase. Until that phase is
    /// complete the seat shows as its description ("Winner of Heat 3").
    FromPhase { phase: PhaseId, position: u32 },
    /// Nobody named. Only an unranked heat (practice) may leave a seat open.
    Open,
}

/// One seat in a heat's lineup, before it is resolved to a pilot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct HeatSeat {
    pub adapter: AdapterId,
    pub seat: SeatRef,
    pub source: SeatSource,
}

/// How a phase that holds phases turns its children's results into its own ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum CombineRule {
    /// Stack the children in order: a pilot is placed by the first child that holds them,
    /// at their position in it. Children are listed best first (A-Main before B-Main; the
    /// final before the semi-finals).
    Stack,
    /// For a round of side-by-side heats: order pilots by their position in their own heat
    /// (all the winners, then all the seconds, …), then by seed.
    ByHeatPosition,
    /// Pool every race the pilot flew beneath this phase and sort by these measures (sum
    /// of points for a round robin; best lap or best N consecutive across time-trial runs).
    Measures(RankingRule),
}

/// What a phase is: a heat, or a holder of phases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum PhaseKind {
    /// The leaf: a lineup that is raced one or more times.
    Heat { lineup: Vec<HeatSeat> },
    /// A phase that holds phases. An empty `field` means "whoever is in my children".
    Group {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        field: Vec<FieldSlice>,
        combine: CombineRule,
    },
}

/// One phase in an event's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PhaseDef {
    pub id: PhaseId,
    /// The director's name for it ("Qualifying", "C-Main", "Winners Round 2").
    pub label: String,
    /// The phase that holds this one, or `None` at the top of the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<PhaseId>,
    pub kind: PhaseKind,
    /// The format set on this phase. It applies to every heat beneath it unless a phase
    /// lower down sets its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub format: Option<Format>,
    /// How many false starts a pilot may make across every race beneath this phase. On
    /// reaching it Grid proposes a disqualification, which the director confirms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub false_start_limit: Option<u32>,
    /// A layout name for views and overlays ("double-elimination", "mains-ladder"). The
    /// engine never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display_hint: Option<String>,
}

/// Identifies one protest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct ProtestId(pub String);

/// How a protest ended. A protest is resolved once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ProtestOutcome {
    Upheld,
    Denied,
    Withdrawn,
}

/// A canonical event in the append-only log.
///
/// Adapters append **raw observations** (lifecycle, [`SeatSeen`](Event::SeatSeen),
/// [`Pass`], signal). The Director appends the **race facts** of `docs/primitives.md` §5.1.
/// the **lap rulings** of §5.7, and the phase tree with its rulings (false starts,
/// disqualification, locking, protests). Raw observations are never mutated; a correction is a
/// new appended event.
///
/// Externally tagged (the default serde representation): each variant serialises as
/// `{ "VariantName": { ..fields } }`, which maps cleanly to a discriminated union in
/// the generated TypeScript (#4).
// A phase definition is much bigger than a pass. Boxing it would save memory only where
// events are held in bulk, and nothing holds them in bulk: they are read from the log,
// folded, and dropped. So the plain shape is kept.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Event {
    /// The source became available. Liveness only; never affects past truth.
    AdapterConnected { adapter: AdapterId },
    /// The source went away. Liveness only.
    AdapterDisconnected { adapter: AdapterId },
    /// The source started a race/heat, where it exposes a lifecycle. Optional —
    /// declared by adapter capability.
    SessionStarted {
        adapter: AdapterId,
        session: SessionId,
    },
    /// The source ended a race/heat.
    SessionEnded {
        adapter: AdapterId,
        session: SessionId,
    },
    /// A source-local seat reference appeared (a node seat went active, a sim
    /// player joined). Gives the Director the list to bind pilots against; the binding
    /// itself is never an adapter event.
    SeatSeen { adapter: AdapterId, seat: SeatRef },
    /// A gate crossing — the atom (see [`Pass`]).
    Pass(Pass),
    /// How a timer's clock lines up with the Director's: `offset_micros` is the timer's
    /// clock minus the Director's, measured to within `error_micros`. Logged by the
    /// Director when it measures a timer (and again when the measurement moves). It is
    /// what lets the start tone, which the Director decides, be placed on the timer's
    /// clock. A timer that never reports this has laps timed from the hole shot only.
    ClockOffset {
        adapter: AdapterId,
        #[ts(type = "number")]
        offset_micros: i64,
        #[ts(type = "number")]
        error_micros: i64,
    },

    /// A chunk of captured per-node RSSI trace. An **immutable raw observation** like
    /// [`Pass`]: a signal-capable adapter appends these incrementally as a race runs, and a
    /// fold joins a seat's chunks back into a contiguous trace. A timer with no signal
    /// (a simulator) never emits this, so the signal layer is simply absent there.
    SignalChunk(SignalChunk),
    /// The one-shot enter/exit detection thresholds for a node (see [`SignalThresholds`]).
    /// Captured alongside the trace so the evidence carries the levels the timer detected
    /// against; the last one per `(adapter, seat)` wins.
    SignalThresholds(SignalThresholds),
    /// The **dense, full-fidelity** per-node RSSI history for a heat, pulled from RotorHazard's
    /// request-driven `current_marshal_data` at heat end (see [`SignalHistory`]). Supersedes the
    /// coarse streaming [`SignalChunk`] samples for its seat.
    SignalHistory(SignalHistory),

    // --- race facts (docs/primitives.md §5.1) ---
    /// Staging began for a race: the countdown every console and overlay shares. `tone_at`
    /// is the planned start tone, in microseconds on the server clock. Optional — a race the
    /// source starts has no staging and begins at [`Event::RaceStarted`].
    RaceStarting {
        setup: RaceSetup,
        #[ts(type = "number")]
        tone_at: i64,
    },
    /// A race started (logged at the tone). Carries the full setup, so it stands alone
    /// whether or not a [`Event::RaceStarting`] came first.
    RaceStarted { setup: RaceSetup },
    /// A race stopped. It is now undecided until the director saves or discards it.
    RaceStopped { race: RaceId },
    /// The director saved a stopped race: it counts. A saved race is provisional until a
    /// phase that contains it is locked.
    RaceSaved {
        race: RaceId,
        /// Flags the race for the marshal's to-do list.
        #[serde(default)]
        needs_marshal: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// The director discarded a race: it stays in the log and counts toward nothing. A
    /// restart is a discard followed by the next race of the same heat.
    RaceDiscarded {
        race: RaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<DiscardReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },

    /// The marshal's to-do flag on a race was set or cleared after the save: `true` puts
    /// the race on the list of races that need a look, `false` takes it off once someone
    /// has looked.
    MarshalFlagSet { race: RaceId, needs_marshal: bool },

    // --- lap rulings (docs/primitives.md §5.7) ---
    // A ruling is a human decision. Each names its race, and targets a **crossing** by the
    // log offset of the fact that made it: a `Pass`, or a `LapInserted`.
    /// Remove the lap that `crossing` ended. The crossing is ignored, so the lap before it
    /// stays as it was and the lap after it starts where that one ended.
    LapRemoved {
        race: RaceId,
        crossing: LogRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<RemovalReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// Undo a removal: the crossing counts again.
    LapRestored { race: RaceId, crossing: LogRef },
    /// Add a crossing the timer missed. Inside an existing lap this splits it in two.
    LapInserted {
        race: RaceId,
        adapter: AdapterId,
        seat: SeatRef,
        at: SourceTime,
    },
    /// Move a crossing to a different time on the timer's clock.
    LapAdjusted {
        race: RaceId,
        crossing: LogRef,
        at: SourceTime,
    },
    /// Mark the lap that `crossing` ended as a **restart**: it has no lap time, the laps
    /// either side keep theirs, and a run of consecutive laps may not span it.
    RestartMarked { race: RaceId, crossing: LogRef },
    /// Undo a restart mark.
    RestartUnmarked { race: RaceId, crossing: LogRef },

    // --- the phase tree and its rulings (docs/primitives.md §5.2, §5.7, §5.8) ---
    /// A phase was added to the event's tree, or changed.
    PhaseDefined { def: PhaseDef },
    /// A phase was taken out of the tree.
    PhaseRemoved { phase: PhaseId },
    /// The director said a heat is done. Only this completes a "director decides" heat.
    HeatDone { heat: PhaseId },
    /// The director took that back.
    HeatReopened { heat: PhaseId },
    /// A pilot false started in a race. Recorded against the pilot so it can be counted;
    /// the race itself is discarded and re-run.
    FalseStart { race: RaceId, pilot: PilotId },
    /// A false start was taken back.
    FalseStartCleared { race: RaceId, pilot: PilotId },
    /// The director disqualified a pilot from a phase: they are dropped from its later
    /// lineups and ranked below everyone who kept racing.
    Disqualified {
        phase: PhaseId,
        pilot: PilotId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<String>,
        /// What happens to a place the pilot had already earned that feeds another heat (a
        /// bump-up spot). `false`, the default: the next pilot moves up and takes it.
        /// `true`: the place stays empty.
        #[serde(default)]
        keep_place_empty: bool,
    },
    /// The director set a pilot's finishing position in a heat by hand. It holds until it
    /// is cleared, and everything that reads the heat's ranking sees it.
    PositionSet {
        heat: PhaseId,
        pilot: PilotId,
        /// 1 for the top.
        position: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// A hand-set position was cleared: the pilot is ranked by the rules again.
    PositionCleared { heat: PhaseId, pilot: PilotId },
    /// A disqualification was lifted.
    Reinstated { phase: PhaseId, pilot: PilotId },
    /// The director locked a phase: its results, and everything beneath it, are official.
    PhaseLocked { phase: PhaseId },
    /// The director unlocked a phase.
    PhaseUnlocked { phase: PhaseId },
    /// The event set a class's roster: the pilots racing in that class at this event, in
    /// roster order. A class is in the event once it has a roster.
    RosterSet {
        class: ClassId,
        pilots: Vec<PilotId>,
    },
    /// The event dropped a class.
    RosterRemoved { class: ClassId },
    /// The event set which phases count toward a class's standings, in order. The first
    /// decides. A later one in the list is only reached back to for pilots the ones before
    /// it leave unplaced (`docs/primitives.md` §5.3).
    StandingsSet {
        class: ClassId,
        phases: Vec<PhaseId>,
    },
    /// The director set the run queue: the order they mean to fly heats in. Heats not
    /// listed follow after, in the default order. A suggestion only: any ready heat can
    /// be started at any time.
    QueueSet { heats: Vec<PhaseId> },
    /// A protest was filed against a race.
    ProtestFiled {
        protest: ProtestId,
        race: RaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        by: Option<PilotId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// A protest was resolved.
    ProtestResolved {
        protest: ProtestId,
        outcome: ProtestOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
}

/// What a console asks the Director to do in an event (`POST /events/{id}/commands`).
///
/// The Director checks a command against the event's state. If it is allowed, the facts it
/// stands for are appended to the log; if not, it is refused with the reason and nothing is
/// appended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Command {
    /// Start the next race of a heat. The lineup is the heat's resolved lineup, and the
    /// race rules come from the format in force. A format with a staging time begins the
    /// countdown; the Director logs the start at the tone.
    StartRace { heat: PhaseId },
    /// Stop the running race.
    StopRace { race: RaceId },
    /// Save a stopped race, so it counts.
    SaveRace {
        race: RaceId,
        #[serde(default)]
        needs_marshal: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// Discard a race: staging, running, stopped or already saved.
    DiscardRace {
        race: RaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<DiscardReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// File a protest against a race. The Director gives it an id.
    FileProtest {
        race: RaceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        by: Option<PilotId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        note: Option<String>,
    },
    /// Record a setup fact or a ruling, sent as the fact itself: a phase defined or removed,
    /// a roster, a lap ruling, a heat called done, a false start, a disqualification, a
    /// position set by hand, a lock, a protest resolved. Facts that only a timer or the
    /// Director itself may log (passes, race starts and stops) are refused.
    Record(Box<Event>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pass() -> Pass {
        Pass {
            adapter: AdapterId("velocidrone".into()),
            seat: SeatRef("AcroAce".into()),
            at: SourceTime::from_micros(12_500_000),
            sequence: Some(3),
            gate: GateIndex::LAP,
            signal: None,
            race: None,
        }
    }

    #[test]
    fn pass_round_trips_through_json() {
        let event = Event::Pass(sample_pass());
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn lap_gate_and_sequence_are_omitted_when_default() {
        // A plain lap-gate pass with no sequence should not serialise `gate` or
        // `signal`, keeping the common case compact.
        let pass = Pass {
            sequence: None,
            ..sample_pass()
        };
        let json = serde_json::to_string(&Event::Pass(pass)).unwrap();
        assert!(!json.contains("gate"), "lap gate should be omitted: {json}");
        assert!(!json.contains("signal"), "absent signal omitted: {json}");
    }

    #[test]
    fn splits_carry_a_gate_index() {
        let split = Pass {
            gate: GateIndex(2),
            ..sample_pass()
        };
        assert!(!split.gate.is_lap_gate());
        let json = serde_json::to_string(&Event::Pass(split)).unwrap();
        assert!(json.contains("gate"));
    }

    #[test]
    fn interval_math_uses_source_clock() {
        let a = SourceTime::from_micros(10_000_000);
        let b = SourceTime::from_micros(22_500_000);
        assert_eq!(b.micros_since(a), 12_500_000);
    }

    #[test]
    fn pilot_id_is_transparent_on_the_wire() {
        // `PilotId` is a transparent newtype — it serialises as the bare callsign string.
        assert_eq!(
            serde_json::to_string(&PilotId("acroace".into())).unwrap(),
            "\"acroace\""
        );
    }

    #[test]
    fn log_ref_is_a_bare_offset_on_the_wire() {
        // `LogRef` is transparent — it serialises as the raw offset integer.
        assert_eq!(serde_json::to_string(&LogRef(42)).unwrap(), "42");
    }

    #[test]
    fn signal_trace_events_round_trip() {
        // The Slice-1 signal-as-evidence facts round-trip through externally-tagged JSON.
        let events = vec![
            Event::SignalChunk(SignalChunk {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-0".into()),
                from: SourceTime::from_micros(2_215_296),
                period_micros: 100_000,
                levels: vec![70, 72, 150, 148, 71, 70],
                unit: SignalUnit::Counts,
                noise_floor: None,
            }),
            Event::SignalThresholds(SignalThresholds {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-0".into()),
                enter: 90,
                exit: 80,
                unit: SignalUnit::Counts,
            }),
            Event::SignalHistory(SignalHistory {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-0".into()),
                times: vec![0, 50_000, 100_000, 150_000],
                rssi: vec![70, 88, 150, 71],
                base: 0,
            }),
            // The same fact as a live slice: `base` is the sample offset it starts at (#392).
            Event::SignalHistory(SignalHistory {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-0".into()),
                times: vec![200_000, 250_000],
                rssi: vec![90, 71],
                base: 4,
            }),
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn legacy_log_without_signal_trace_reads_back_with_defaults() {
        // A log written before `SignalChunk`/`SignalThresholds` existed carries none of the new
        // variants. They are a purely additive extension of the externally-tagged `Event` enum, so
        // every pre-existing serialized event still deserializes unchanged — and a Velocidrone/sim
        // log (which never emits the signal facts) is byte-identical to a pre-Slice-1 log.
        let legacy_pass = r#"{"Pass":{"adapter":"velocidrone","seat":"AcroAce","at":12500000}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy_pass).unwrap(),
            Event::Pass(Pass {
                adapter: AdapterId("velocidrone".into()),
                seat: SeatRef("AcroAce".into()),
                at: SourceTime::from_micros(12_500_000),
                sequence: None,
                gate: GateIndex::LAP,
                signal: None,
                race: None,
            })
        );
        // And the new facts themselves deserialize from their on-the-wire shape.
        let chunk = r#"{"SignalChunk":{"adapter":"rotorhazard","seat":"node-1","from":5000000,"period_micros":100000,"levels":[-912,-640,-910],"unit":"TenthsDb","noise_floor":-920}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(chunk).unwrap(),
            Event::SignalChunk(SignalChunk {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-1".into()),
                from: SourceTime::from_micros(5_000_000),
                period_micros: 100_000,
                levels: vec![-912, -640, -910],
                unit: SignalUnit::TenthsDb,
                noise_floor: Some(-920),
            })
        );
    }

    #[test]
    fn legacy_signal_history_without_base_reads_back_as_a_full_snapshot() {
        // `base` is additive (#392). A history written before it existed was always the seat's
        // whole trace, folded last-writer-wins — and `base = 0` is exactly that rule, so an old log
        // replays unchanged rather than being mistaken for a slice.
        let legacy = r#"{"SignalHistory":{"adapter":"rotorhazard","seat":"node-0","times":[0,50000],"rssi":[70,88]}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy).unwrap(),
            Event::SignalHistory(SignalHistory {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-0".into()),
                times: vec![0, 50_000],
                rssi: vec![70, 88],
                base: 0,
            })
        );
        // And the field goes out on the wire as a plain number, so an older consumer that ignores
        // unknown keys still parses a slice-shaped history.
        let json = serde_json::to_string(&Event::SignalHistory(SignalHistory {
            adapter: AdapterId("rotorhazard".into()),
            seat: SeatRef("node-0".into()),
            times: vec![100_000],
            rssi: vec![150],
            base: 2,
        }))
        .unwrap();
        assert!(json.contains(r#""base":2"#), "{json}");
    }

    fn sample_setup() -> RaceSetup {
        RaceSetup {
            race: RaceId("race-1".into()),
            heat: PhaseId("heat-a".into()),
            lineup: vec![
                LineupEntry {
                    adapter: AdapterId("oft-3fa2".into()),
                    seat: SeatRef("seat-0".into()),
                    pilot: Some(PilotId("ryan".into())),
                },
                LineupEntry {
                    adapter: AdapterId("oft-3fa2".into()),
                    seat: SeatRef("seat-1".into()),
                    pilot: None,
                },
            ],
            owner: StartOwner::Grid,
        }
    }

    #[test]
    fn race_facts_round_trip() {
        let race = RaceId("race-1".into());
        let events = vec![
            Event::RaceStarting {
                setup: sample_setup(),
                tone_at: 1_700_000_000_000_000,
            },
            Event::RaceStarted {
                setup: sample_setup(),
            },
            Event::RaceStopped { race: race.clone() },
            Event::RaceSaved {
                race: race.clone(),
                needs_marshal: true,
                note: Some("seat 1 looked late".into()),
            },
            Event::RaceDiscarded {
                race,
                reason: Some(DiscardReason::FalseStart),
                note: None,
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn a_plain_save_and_a_plain_discard_carry_no_optional_fields() {
        let saved = Event::RaceSaved {
            race: RaceId("r".into()),
            needs_marshal: false,
            note: None,
        };
        assert_eq!(
            serde_json::to_string(&saved).unwrap(),
            r#"{"RaceSaved":{"race":"r","needs_marshal":false}}"#
        );
        // The reason is optional: a discard with nothing picked is still a whole fact.
        let discarded: Event = serde_json::from_str(r#"{"RaceDiscarded":{"race":"r"}}"#).unwrap();
        assert_eq!(
            discarded,
            Event::RaceDiscarded {
                race: RaceId("r".into()),
                reason: None,
                note: None,
            }
        );
    }

    #[test]
    fn an_open_seat_omits_the_pilot_on_the_wire() {
        let open = LineupEntry {
            adapter: AdapterId("oft".into()),
            seat: SeatRef("seat-2".into()),
            pilot: None,
        };
        assert_eq!(
            serde_json::to_string(&open).unwrap(),
            r#"{"adapter":"oft","seat":"seat-2"}"#
        );
    }

    #[test]
    fn lap_rulings_round_trip() {
        let race = RaceId("race-1".into());
        let events = vec![
            Event::LapRemoved {
                race: race.clone(),
                crossing: LogRef(12),
                reason: Some(RemovalReason::FalseDetection),
                note: None,
            },
            Event::LapRestored {
                race: race.clone(),
                crossing: LogRef(12),
            },
            Event::LapInserted {
                race: race.clone(),
                adapter: AdapterId("oft".into()),
                seat: SeatRef("seat-0".into()),
                at: SourceTime::from_micros(9_000_000),
            },
            Event::LapAdjusted {
                race: race.clone(),
                crossing: LogRef(14),
                at: SourceTime::from_micros(9_100_000),
            },
            Event::RestartMarked {
                race: race.clone(),
                crossing: LogRef(15),
            },
            Event::RestartUnmarked {
                race,
                crossing: LogRef(15),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn a_removal_needs_no_reason() {
        let removed: Event =
            serde_json::from_str(r#"{"LapRemoved":{"race":"r","crossing":7}}"#).unwrap();
        assert_eq!(
            removed,
            Event::LapRemoved {
                race: RaceId("r".into()),
                crossing: LogRef(7),
                reason: None,
                note: None,
            }
        );
    }

    #[test]
    fn a_format_round_trips_and_is_plain_data() {
        let chase_the_ace = Format {
            name: "Chase the Ace".into(),
            race: RaceRules {
                end: EndCondition::FirstToLaps(3),
                staging_micros: 5_000_000,
                min_lap_micros: Some(2_000_000),
                first_lap: Default::default(),
                stop_after_all_finish_micros: None,
                max_micros: None,
            },
            completion: CompletionRule::AnyPilotReaches {
                measure: Measure::Wins,
                at_least: 2,
            },
            ranking: Some(RankingRule {
                sort: vec![Measure::Wins, Measure::LastFinish],
            }),
            points: vec![],
        };
        let json = serde_json::to_string(&chase_the_ace).unwrap();
        let back: Format = serde_json::from_str(&json).unwrap();
        assert_eq!(chase_the_ace, back);

        // Practice: no ranking, no points, done when the director says so.
        let practice: Format = serde_json::from_str(
            r#"{"name":"Practice","race":{"end":"Open","staging_micros":0},"completion":"DirectorDecides"}"#,
        )
        .unwrap();
        assert_eq!(practice.ranking, None);
        assert!(practice.points.is_empty());
        assert_eq!(practice.race.min_lap_micros, None);
    }

    #[test]
    fn a_phase_definition_round_trips() {
        let heat = PhaseDef {
            id: PhaseId("a-final".into()),
            label: "A-Main Final".into(),
            parent: Some(PhaseId("a-main".into())),
            kind: PhaseKind::Heat {
                lineup: vec![
                    HeatSeat {
                        adapter: AdapterId("oft".into()),
                        seat: SeatRef("seat-0".into()),
                        source: SeatSource::FromPhase {
                            phase: PhaseId("a-semi-1".into()),
                            position: 1,
                        },
                    },
                    HeatSeat {
                        adapter: AdapterId("oft".into()),
                        seat: SeatRef("seat-1".into()),
                        source: SeatSource::Open,
                    },
                ],
            },
            format: None,
            false_start_limit: None,
            display_hint: None,
        };
        let group = PhaseDef {
            id: PhaseId("c-main".into()),
            label: "C-Main".into(),
            parent: Some(PhaseId("mains".into())),
            kind: PhaseKind::Group {
                field: vec![
                    FieldSlice::FromPhase {
                        phase: PhaseId("qualifying".into()),
                        from: 17,
                        to: 22,
                    },
                    FieldSlice::FromPhase {
                        phase: PhaseId("d-main".into()),
                        from: 1,
                        to: 2,
                    },
                ],
                combine: CombineRule::Stack,
            },
            format: None,
            false_start_limit: Some(2),
            display_hint: Some("double-elimination".into()),
        };
        for def in [heat, group] {
            let json = serde_json::to_string(&def).unwrap();
            let back: PhaseDef = serde_json::from_str(&json).unwrap();
            assert_eq!(def, back);
        }
    }
}
