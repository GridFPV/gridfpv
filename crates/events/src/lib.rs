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

/// A contiguous run of per-tick RSSI samples captured from a signal-capable timer
/// (RotorHazard first).
///
/// The trace is the **evidence** a marshal later reviews a call against. It is captured
/// **chunked and append-incrementally** rather than one fat per-heat blob: a hardware timer
/// streams its node RSSI continuously, so chunks append as the run proceeds and interleave
/// deterministically with the [`Pass`]es in the same log — a single buffered-until-heat-end
/// event would be lost on an abort. Each chunk is a window of `rssi` samples beginning at
/// `from` on the source clock, one every `period_micros`; concatenating a seat's chunks
/// in append order reconstructs the whole trace (a fold's job).
///
/// `rssi` is kept as `u16` — RotorHazard's filtered ADC counts are integers, so this is
/// compact, exact, and free of the float-equality hazards a `f32` trace would carry into the
/// deterministic fold/tests (matching the events crate's integer-time convention).
///
/// # Fidelity bound
///
/// RotorHazard's streaming `node_data` socket emit carries only the **latest** per-node
/// sample (`pass_peak_rssi`/`node_peak_rssi`), not a backfilled per-tick history array (that
/// lives in the request-driven `current_marshal_data`, which a live translator does not
/// subscribe to). So a chunk captured from the live stream samples at the emit cadence — one
/// `rssi` value per `node_data` tick — which is faithful to *what RH exposes live*, but is
/// coarser than the detector's internal sampling. This is the load-bearing fidelity bound
/// to confirm on real hardware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalChunk {
    /// The timing source that produced this trace.
    pub adapter: AdapterId,
    /// The source-local seat the trace belongs to.
    pub seat: SeatRef,
    /// The source-clock timestamp of the **first** sample in `rssi`.
    pub from: SourceTime,
    /// Microseconds between consecutive samples (the capture cadence).
    #[ts(type = "number")]
    pub period_micros: u32,
    /// The RSSI samples (filtered ADC counts), oldest first, one every `period_micros`.
    pub rssi: Vec<u16>,
}

/// The enter/exit detection thresholds a signal-capable timer is configured with for a node —
/// the levels the RSSI crosses to open/close a pass (RotorHazard `enter_at_level` /
/// `exit_at_level`). Captured **once** alongside the [`SignalChunk`] trace so a marshal can
/// see *why* the timer called (or missed) a lap against the visible signal. One-shot per
/// `(adapter, seat)`; a later one supersedes (a fold keeps the last).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalThresholds {
    /// The timing source these thresholds belong to.
    pub adapter: AdapterId,
    /// The source-local seat the thresholds apply to.
    pub seat: SeatRef,
    /// The **enter** threshold: RSSI rising above this opens a pass.
    pub enter: u16,
    /// The **exit** threshold: RSSI falling below this closes the pass.
    pub exit: u16,
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

/// Identifies one **heat**: a lineup that is raced one or more times — the leaf phase
/// (`docs/primitives.md` §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct HeatId(pub String);

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
    pub heat: HeatId,
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

/// A canonical event in the append-only log.
///
/// Adapters append **raw observations** (lifecycle, [`SeatSeen`](Event::SeatSeen),
/// [`Pass`], signal). The Director appends the **race facts** of `docs/primitives.md` §5.1.
/// The rest of the competition vocabulary (phases, rulings) is added as those primitives are
/// built. Raw observations are never mutated; a correction is a
/// new appended event.
///
/// Externally tagged (the default serde representation): each variant serialises as
/// `{ "VariantName": { ..fields } }`, which maps cleanly to a discriminated union in
/// the generated TypeScript (#4).
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
                rssi: vec![70, 72, 150, 148, 71, 70],
            }),
            Event::SignalThresholds(SignalThresholds {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-0".into()),
                enter: 90,
                exit: 80,
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
        let chunk = r#"{"SignalChunk":{"adapter":"rotorhazard","seat":"node-1","from":5000000,"period_micros":100000,"rssi":[60,120,61]}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(chunk).unwrap(),
            Event::SignalChunk(SignalChunk {
                adapter: AdapterId("rotorhazard".into()),
                seat: SeatRef("node-1".into()),
                from: SourceTime::from_micros(5_000_000),
                period_micros: 100_000,
                rssi: vec![60, 120, 61],
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
            heat: HeatId("heat-a".into()),
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
}
