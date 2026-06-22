//! Canonical event model for GridFPV — the schema of the append-only log.
//!
//! Every live timing source (Velocidrone, RotorHazard, LapRF, manual entry) sits
//! behind one adapter interface and is translated into this small, source-agnostic
//! set of events. This is the vocabulary the whole log speaks; everything else —
//! laps, results, standings — is a *projection* derived downstream.
//!
//! See `docs/timer-adapters.html` (§2 the canonical event model) — this crate is
//! the Rust realisation of that doc.
//!
//! Design notes:
//! - Adapters emit **raw observations** ("a gate was crossed"), never derivations.
//!   A lap is two consecutive [`Pass`]es, computed identically for every source by
//!   the projection engine — not reported here.
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

/// A source-local competitor handle: a node seat, a sim player name, a transponder
/// id. Bound to a GridFPV pilot later by a *registration* action — never by the
/// adapter (see Architecture §9). The adapter only reports the refs it sees.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct CompetitorRef(pub String);

/// A source's own race/heat identifier, where it exposes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct SessionId(pub String);

/// A GridFPV pilot's stable, event-scoped identity — the racer a source-local
/// [`CompetitorRef`] is *bound* to by a registration action (Architecture §9), never by
/// the adapter. For a basic race this is the pilot's callsign / stable id; richer pilot
/// metadata (display name, team, avatar) can layer on later.
///
/// This is the canonical pilot handle the whole stack shares: the event model records
/// the binding ([`Event::CompetitorRegistered`]) and the wire/scope layer addresses a
/// pilot by the same type, so the log and the protocol never disagree on what a pilot id
/// is.
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

/// A gate crossing — the one observation everything else derives from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Pass {
    /// Which timing source produced this pass.
    pub adapter: AdapterId,
    /// The source's own competitor handle.
    pub competitor: CompetitorRef,
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
}

// --- Race-engine vocabulary (#28) -------------------------------------------
// Beyond the adapter's raw observations, the race engine and the RD append events
// too: heat-loop state transitions and marshaling adjudications. These fold into
// the same projections (race-engine.html §2, architecture.html §3); raw
// observations are never mutated.

/// Identifies a heat within the event log.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct HeatId(pub String);

/// Identifies a **class** within an event (protocol.html §4 "Class scope") — one
/// class's phases, schedule, and standings, which may run in parallel with others.
///
/// This is the **canonical** class handle the whole stack shares: the event model tags
/// a scheduled heat with the class it belongs to ([`Event::HeatScheduled`]) and the
/// wire/scope layer addresses a class by the same type
/// ([`ClassId`](../../server/scope/struct.ClassId.html), re-exported from here), so the
/// log and the protocol never disagree on what a class id is. A transparent string
/// newtype like the other event-model ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct ClassId(pub String);

/// Identifies a **round** within a class's schedule — one pass through the phase
/// (qualifying round 1, round 2, …). A transparent string newtype like [`ClassId`];
/// the richer phase/round model lands with the scheduler, this is the stable handle a
/// scheduled heat is tagged with.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct RoundId(pub String);

/// A reference to an already-logged event by its append **offset** — the stable id
/// marshaling adjudications target (e.g. "void *this* pass"). The offset is assigned
/// by the storage layer when the target event was appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
// `#[ts(as = "f64")]` renders the offset as a plain TS `number`: log offsets are
// bounded far below 2^53 in our domain, so `number` is exact and avoids a wide-integer
// wire mismatch.
#[ts(export, export_to = "bindings/", as = "f64")]
pub struct LogRef(pub u64);

/// A transition of the heat-loop state machine (race-engine.html §2). The recorded
/// transition is named for the state it enters on the forward path (Staged → Armed →
/// Running → Finished → Finalized), with the off-ramps (revert/abort/restart/discard)
/// named for the action so they stay distinct even when they land on the same state.
/// The engine validates legality against the current state. Heat *creation* is a
/// separate event ([`Event::HeatScheduled`]) — it carries the lineup, which a
/// transition does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum HeatTransition {
    /// Countdown begins; IRL staging assigns frequencies.
    Staged,
    /// The gate opens to detections.
    Armed,
    /// The race is running; passes are consumed from here (plus the grace window).
    Running,
    /// The race closed — time elapsed or all landed — entering the unofficial phase.
    Finished,
    /// The result is finalized.
    Finalized,
    /// Results are handed to the format generator.
    Advanced,
    /// A finalized result re-opened for correction (Final → Unofficial).
    Reverted,
    /// Abandoned before finalizing (Staged/Armed/Running → Scheduled, so the RD re-Stages).
    Aborted,
    /// A committed heat restarted (Armed/Running/Unofficial → Scheduled, so the RD re-Stages).
    Restarted,
    /// A finalized heat discarded for a re-run.
    Discarded,
}

/// A marshaling penalty applied to a competitor in a heat.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Penalty {
    /// Disqualified from the heat.
    Disqualify,
    /// Time added to the competitor's result, in microseconds.
    TimeAdded {
        #[ts(type = "number")]
        micros: i64,
    },
}

/// A canonical event in the append-only log.
///
/// Adapters append **raw observations** (lifecycle, [`CompetitorSeen`](Event::CompetitorSeen),
/// [`Pass`]); the **race engine** appends heat-loop state transitions and the **RD** appends
/// marshaling adjudications (#28). Everything folds into the same projections — laps, results,
/// standings — and raw observations are never mutated; a correction is a new appended event.
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
    /// A source-local competitor reference appeared (a node seat went active, a sim
    /// player joined). Drives auto-presence and gives the RD the list to bind
    /// against; the binding itself is a registration action, not an adapter event.
    CompetitorSeen {
        adapter: AdapterId,
        competitor: CompetitorRef,
    },
    /// The RD bound a source-local competitor to a GridFPV pilot — the *registration*
    /// action the adapter never performs itself (Architecture §9): "this timer channel
    /// **is** this pilot". This is the logged binding the live and lap projections fold
    /// to surface pilot identity over a bare [`CompetitorRef`]. Last registration for a
    /// given `(adapter, competitor)` wins (a re-bind supersedes the earlier one); the
    /// raw observations it maps over are never mutated.
    CompetitorRegistered {
        adapter: AdapterId,
        competitor: CompetitorRef,
        pilot: PilotId,
    },
    /// A gate crossing — the atom (see [`Pass`]).
    Pass(Pass),

    // --- race-engine events (#28) ---
    /// A heat is created with its lineup and enters the `Scheduled` state — the
    /// `[*] → Scheduled` entry of the heat loop (race-engine.html §2). Carries the
    /// competitors in the heat and, additively, the class/round it belongs to and the
    /// per-pilot frequency assignment.
    ///
    /// The `class`, `round`, and `frequencies` fields are **additive** and
    /// default-absent: a heat scheduled without them (the free-text NewHeat path, a
    /// sim race, a pre-existing log entry) reads back as `None`/empty, so older logs
    /// round-trip unchanged. `frequencies` pairs each competitor with a raw-MHz channel
    /// (e.g. `5800`); empty means none assigned (sim/none).
    HeatScheduled {
        heat: HeatId,
        lineup: Vec<CompetitorRef>,
        /// The class this heat runs in, where the scheduler tagged it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        class: Option<ClassId>,
        /// The round within the class's schedule, where the scheduler tagged it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        round: Option<RoundId>,
        /// Per-pilot frequency assignment in raw MHz (e.g. `5800`). Empty when none is
        /// assigned (a simulator, or the free-text path that does not assign channels).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        frequencies: Vec<(CompetitorRef, u16)>,
    },
    /// A heat-loop state transition appended by the engine (race-engine.html §2).
    HeatStateChanged {
        heat: HeatId,
        transition: HeatTransition,
    },
    /// The **start procedure fired** for a heat that just entered `Armed` (heat-lifecycle
    /// redesign, Slice 2). The Director runtime chooses the randomized start delay **once**, at
    /// the moment the heat is armed, and writes it here as a *fact* — so the console can cue the
    /// start tone, and a replay reads the **same** delay instead of re-randomizing. The
    /// subsequent `Armed → Running` [`HeatStateChanged`](Event::HeatStateChanged) is appended by
    /// the runtime `delay_ms` later; together they make the auto-start deterministic on replay
    /// (race-engine.html §6 — the engine/projection fold never reads a clock or rolls dice; only
    /// the runtime does, at emission time).
    HeatStarting {
        /// The heat whose start procedure fired (it is in `Armed`).
        heat: HeatId,
        /// The chosen randomized start delay, in **milliseconds**, from this event to the
        /// `Armed → Running` transition. Written once by the runtime; deterministic on replay.
        #[ts(type = "number")]
        delay_ms: u32,
    },
    /// Marshaling: void a previously-detected pass, referenced by log offset. The
    /// projection folds it out as if it never happened — the raw [`Pass`] stays in
    /// the log untouched.
    DetectionVoided { target: LogRef },
    /// Marshaling: insert a lap-gate pass the timer missed.
    LapInserted {
        adapter: AdapterId,
        competitor: CompetitorRef,
        at: SourceTime,
    },
    /// Marshaling: re-time a previously-detected pass (referenced by log offset).
    LapAdjusted { target: LogRef, at: SourceTime },
    /// Marshaling: void an entire heat.
    HeatVoided { heat: HeatId },
    /// Marshaling: apply a penalty to a competitor in a heat.
    PenaltyApplied {
        heat: HeatId,
        competitor: CompetitorRef,
        penalty: Penalty,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pass() -> Pass {
        Pass {
            adapter: AdapterId("velocidrone".into()),
            competitor: CompetitorRef("AcroAce".into()),
            at: SourceTime::from_micros(12_500_000),
            sequence: Some(3),
            gate: GateIndex::LAP,
            signal: None,
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
    fn lifecycle_events_round_trip() {
        let events = vec![
            Event::AdapterConnected {
                adapter: AdapterId("rh".into()),
            },
            Event::SessionStarted {
                adapter: AdapterId("rh".into()),
                session: SessionId("heat-1".into()),
            },
            Event::CompetitorSeen {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-2".into()),
            },
            Event::SessionEnded {
                adapter: AdapterId("rh".into()),
                session: SessionId("heat-1".into()),
            },
            Event::AdapterDisconnected {
                adapter: AdapterId("rh".into()),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
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
    fn race_engine_events_round_trip() {
        let events = vec![
            Event::HeatScheduled {
                heat: HeatId("q-1".into()),
                lineup: vec![
                    CompetitorRef("node-0".into()),
                    CompetitorRef("node-1".into()),
                ],
                class: None,
                round: None,
                frequencies: vec![],
            },
            Event::HeatScheduled {
                heat: HeatId("main-a".into()),
                lineup: vec![
                    CompetitorRef("node-0".into()),
                    CompetitorRef("node-1".into()),
                ],
                class: Some(ClassId("open".into())),
                round: Some(RoundId("r1".into())),
                frequencies: vec![
                    (CompetitorRef("node-0".into()), 5658),
                    (CompetitorRef("node-1".into()), 5695),
                ],
            },
            Event::HeatStarting {
                heat: HeatId("q-1".into()),
                delay_ms: 3200,
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Running,
            },
            Event::HeatStateChanged {
                heat: HeatId("q-1".into()),
                transition: HeatTransition::Aborted,
            },
            Event::DetectionVoided { target: LogRef(42) },
            Event::LapInserted {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(5_000_000),
            },
            Event::LapAdjusted {
                target: LogRef(43),
                at: SourceTime::from_micros(5_100_000),
            },
            Event::HeatVoided {
                heat: HeatId("q-1".into()),
            },
            Event::PenaltyApplied {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("AcroAce".into()),
                penalty: Penalty::TimeAdded { micros: 2_000_000 },
            },
            Event::PenaltyApplied {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("Bee".into()),
                penalty: Penalty::Disqualify,
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn all_heat_transitions_round_trip() {
        use HeatTransition::*;
        for t in [
            Staged, Armed, Running, Finished, Finalized, Advanced, Reverted, Aborted, Restarted,
            Discarded,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: HeatTransition = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn competitor_registered_round_trips() {
        // The registration binding: this source competitor *is* this pilot.
        let event = Event::CompetitorRegistered {
            adapter: AdapterId("rh".into()),
            competitor: CompetitorRef("node-2".into()),
            pilot: PilotId("acroace".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
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
        // `LogRef` is transparent — it serialises as the raw offset integer, the
        // stable id adjudications target.
        assert_eq!(serde_json::to_string(&LogRef(42)).unwrap(), "42");
    }

    #[test]
    fn class_and_round_ids_are_transparent_on_the_wire() {
        // Both newtypes serialise as the bare string, like the other event-model ids.
        assert_eq!(
            serde_json::to_string(&ClassId("open".into())).unwrap(),
            "\"open\""
        );
        assert_eq!(
            serde_json::to_string(&RoundId("r1".into())).unwrap(),
            "\"r1\""
        );
    }

    #[test]
    fn heat_scheduled_omits_class_round_and_frequencies_when_default() {
        // A heat with no class/round/frequencies (the free-text NewHeat path, a sim
        // race) serialises *exactly* like the pre-tag shape: the new fields are
        // skipped entirely, so the wire stays byte-compatible with old logs.
        let event = Event::HeatScheduled {
            heat: HeatId("q-1".into()),
            lineup: vec![CompetitorRef("node-0".into())],
            class: None,
            round: None,
            frequencies: vec![],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("class"), "absent class omitted: {json}");
        assert!(!json.contains("round"), "absent round omitted: {json}");
        assert!(
            !json.contains("frequencies"),
            "empty frequencies omitted: {json}"
        );
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn heat_scheduled_round_trips_with_the_new_fields() {
        let event = Event::HeatScheduled {
            heat: HeatId("main-a".into()),
            lineup: vec![
                CompetitorRef("node-0".into()),
                CompetitorRef("node-1".into()),
            ],
            class: Some(ClassId("open".into())),
            round: Some(RoundId("r2".into())),
            frequencies: vec![
                (CompetitorRef("node-0".into()), 5658),
                (CompetitorRef("node-1".into()), 5695),
            ],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("class") && json.contains("round") && json.contains("frequencies"));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn legacy_heat_scheduled_reads_back_with_defaults() {
        // A pre-existing serialized `HeatScheduled` (before the class/round/frequencies
        // tags existed) must still deserialize, with the new fields defaulting to
        // None/empty. This is the exact JSON shape an old log on disk holds.
        let legacy = r#"{"HeatScheduled":{"heat":"q-1","lineup":["node-0","node-1"]}}"#;
        let back: Event = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            back,
            Event::HeatScheduled {
                heat: HeatId("q-1".into()),
                lineup: vec![
                    CompetitorRef("node-0".into()),
                    CompetitorRef("node-1".into()),
                ],
                class: None,
                round: None,
                frequencies: vec![],
            }
        );
    }
}
