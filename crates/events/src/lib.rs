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

/// A contiguous run of per-tick RSSI samples captured from a signal-capable timer
/// (RotorHazard first — marshaling.html §3.2, the signal-as-evidence layer).
///
/// The trace is the **evidence** a marshal later reviews a call against. It is captured
/// **chunked and append-incrementally** rather than one fat per-heat blob: a hardware timer
/// streams its node RSSI continuously, so chunks append as the run proceeds and interleave
/// deterministically with the [`Pass`]es in the same log — a single buffered-until-heat-end
/// event would be lost on an abort. Each chunk is a window of `rssi` samples beginning at
/// `from` on the source clock, one every `period_micros`; concatenating a competitor's chunks
/// in append order reconstructs the whole trace (the projection's job, see
/// `gridfpv_projection::signal_trace`).
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
/// (marshaling.html §4) to confirm on real hardware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalChunk {
    /// The timing source that produced this trace.
    pub adapter: AdapterId,
    /// The source-local competitor (node seat) the trace belongs to.
    pub competitor: CompetitorRef,
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
/// see *why* the timer called (or missed) a lap against the visible signal (marshaling.html
/// §3.2). One-shot per `(adapter, competitor)`; a later one supersedes (the projection keeps
/// the last).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalThresholds {
    /// The timing source these thresholds belong to.
    pub adapter: AdapterId,
    /// The source-local competitor (node seat) the thresholds apply to.
    pub competitor: CompetitorRef,
    /// The **enter** threshold: RSSI rising above this opens a pass.
    pub enter: u16,
    /// The **exit** threshold: RSSI falling below this closes the pass.
    pub exit: u16,
}

/// The **dense, full-fidelity** per-node RSSI history for a heat — the detector's own internal
/// sampling, pulled from RotorHazard's request-driven `current_marshal_data` at heat end
/// (marshaling.html §3.2, the "RSSI fidelity" risk in §4).
///
/// Unlike the live-streamed [`SignalChunk`] (one aggregate sample per `node_data` heartbeat emit,
/// the *coarse* trace), this is the trace RotorHazard's own marshal page reviews against: every
/// per-tick sample the node recorded, with its **own** sample time. A signal-capable adapter pulls
/// it once when a heat reaches `DONE` and appends one `SignalHistory` per node. The `signal_trace`
/// projection **prefers** this dense history over the coarse [`SignalChunk`] samples for a
/// competitor when both are present (the dense trace supersedes the streaming approximation).
///
/// # Why explicit per-sample times (not a uniform cadence)
///
/// [`SignalChunk`] assumes a fixed `period_micros` because the live stream emits on a regular
/// heartbeat. The detector's internal history is **not** guaranteed uniform — RotorHazard reports a
/// parallel `history_times`/`history_values` pair — so this carries the per-sample times directly
/// (`times[i]` is the source-clock instant of `rssi[i]`), preserving native fidelity with **no
/// resampling** (the load-bearing fidelity caution, marshaling.html §4). Times are race-relative
/// microseconds on the same clock as [`Pass::at`] and the chunk time base, so lap markers and the
/// dense trace align.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignalHistory {
    /// The timing source that produced this history.
    pub adapter: AdapterId,
    /// The source-local competitor (node seat) the history belongs to.
    pub competitor: CompetitorRef,
    /// The source-clock timestamp (race-relative µs) of each sample, parallel to `rssi`. Same
    /// length as `rssi`; `times[i]` is when `rssi[i]` was recorded. Renders as TS `number[]`
    /// (bounded far below 2^53).
    #[ts(type = "number[]")]
    pub times: Vec<i64>,
    /// The dense per-tick RSSI samples (filtered ADC counts), parallel to `times`.
    pub rssi: Vec<u16>,
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

/// A marshaling penalty applied to a competitor in a heat (marshaling.html §3.3 — the
/// adjudication framework: DQ, penalty time *and* points, richer than RotorHazard's
/// `+time`/note model).
///
/// The variants split by **where they land**:
/// - [`Disqualify`](Penalty::Disqualify) and [`TimeAdded`](Penalty::TimeAdded) reshape the
///   **per-heat** lap/time result (the heat scorer, `gridfpv_engine::scoring`): a DQ sinks a
///   competitor below the field, a time penalty worsens their deciding time.
/// - [`PointsDeducted`](Penalty::PointsDeducted) / [`PointsAdded`](Penalty::PointsAdded) do
///   **not** touch the per-heat lap result — they adjust the competitor's **season / event
///   standings** points, folded by the standings projection
///   (`gridfpv_server::round_engine::class_standings`), leaving the heat's lap/time/DQ intact.
///
/// All variants are reversible via [`RulingReversed`](Event::RulingReversed) and read back on a
/// legacy log (the enum is externally tagged, so the new variants are purely additive).
///
/// # Legacy `Disqualify` compatibility
///
/// Adding the optional `reason` made [`Disqualify`](Penalty::Disqualify) a *struct* variant,
/// which serde would otherwise serialise as `{"Disqualify":{}}` and refuse to read from the
/// legacy bare string `"Disqualify"`. A hand-written [`Deserialize`] (see the impl below) accepts
/// **both** the legacy bare `"Disqualify"` and the struct form, and [`Serialize`] keeps emitting
/// the compact bare `"Disqualify"` when there is no reason — so old logs round-trip byte-for-byte
/// and a reason-less DQ stays on the wire exactly as before.
#[derive(Debug, Clone, PartialEq, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Penalty {
    /// Disqualified from the heat. A first-class, reversible competitor **status** (not just a
    /// time effect): the scorer sinks a DQ'd competitor below every non-disqualified one and
    /// flags the placement, and a [`RulingReversed`](Event::RulingReversed) of the
    /// [`PenaltyApplied`](Event::PenaltyApplied) cleanly restores them. An optional `reason`
    /// carries *why* — surfaced in the result + audit; default-absent so a bare `Disqualify`
    /// (and every legacy DQ) reads back unchanged.
    Disqualify {
        /// Why the competitor was disqualified (e.g. "cut the course", "unsafe flying"). `None`
        /// when no reason was recorded — the common quick-DQ, and the legacy shape. (`Penalty`
        /// hand-rolls serde — see the impls below — so the optional/skip behaviour lives there;
        /// `#[ts(optional)]` keeps the generated TS field `reason?:`.)
        #[ts(optional)]
        reason: Option<String>,
    },
    /// Time added to the competitor's result, in microseconds.
    TimeAdded {
        #[ts(type = "number")]
        micros: i64,
    },
    /// **Points deducted** from the competitor's season / event **standings** (marshaling.html
    /// §3.3). Distinct from a time penalty: it does *not* change the per-heat lap result — it
    /// subtracts from the points the competitor accrued across the class's rounds, folded by the
    /// standings projection (`class_standings`). Reversible like any ruling.
    PointsDeducted {
        /// How many standings points to subtract (saturating at zero).
        points: u32,
    },
    /// **Points added** to the competitor's season / event **standings** — the symmetric
    /// counterpart of [`PointsDeducted`](Penalty::PointsDeducted) (e.g. a goodwill / appeal
    /// award). Also standings-only; does not touch the per-heat lap result.
    PointsAdded {
        /// How many standings points to add.
        points: u32,
    },
}

impl Penalty {
    /// Whether this penalty is a disqualification (regardless of any reason). The first-class
    /// DQ-status predicate the scorer / UI use without matching the inner `reason`.
    pub const fn is_disqualify(&self) -> bool {
        matches!(self, Penalty::Disqualify { .. })
    }
}

// `Penalty` hand-rolls serde so the legacy bare `"Disqualify"` string and a reason-carrying
// `{"Disqualify":{"reason":...}}` both round-trip, while a reason-less DQ still serialises as the
// compact bare string (see the `Penalty` doc, "Legacy `Disqualify` compatibility"). The other
// variants serialise/deserialise exactly as the derived externally-tagged form would, so adding
// `PointsDeducted` / `PointsAdded` stays purely additive on the wire.
impl Serialize for Penalty {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStructVariant;
        match self {
            // A reason-less DQ stays the bare `"Disqualify"` string (legacy-identical); a DQ with a
            // reason serialises as the struct form so the reason rides along.
            Penalty::Disqualify { reason: None } => {
                serializer.serialize_unit_variant("Penalty", 0, "Disqualify")
            }
            Penalty::Disqualify {
                reason: Some(reason),
            } => {
                let mut sv = serializer.serialize_struct_variant("Penalty", 0, "Disqualify", 1)?;
                sv.serialize_field("reason", reason)?;
                sv.end()
            }
            Penalty::TimeAdded { micros } => serializer.serialize_newtype_variant(
                "Penalty",
                1,
                "TimeAdded",
                &TimeAddedRepr { micros: *micros },
            ),
            Penalty::PointsDeducted { points } => serializer.serialize_newtype_variant(
                "Penalty",
                2,
                "PointsDeducted",
                &PointsRepr { points: *points },
            ),
            Penalty::PointsAdded { points } => serializer.serialize_newtype_variant(
                "Penalty",
                3,
                "PointsAdded",
                &PointsRepr { points: *points },
            ),
        }
    }
}

/// The body of a serialized `TimeAdded` — its single `micros` field, so the externally-tagged
/// wire shape stays `{"TimeAdded":{"micros":N}}` exactly as the derived form produced.
#[derive(Serialize, Deserialize)]
struct TimeAddedRepr {
    micros: i64,
}

/// The body of a serialized `PointsDeducted` / `PointsAdded` — its single `points` field.
#[derive(Serialize, Deserialize)]
struct PointsRepr {
    points: u32,
}

impl<'de> Deserialize<'de> for Penalty {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct PenaltyVisitor;

        impl<'de> Visitor<'de> for PenaltyVisitor {
            type Value = Penalty;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Penalty: the string \"Disqualify\" or a tagged variant object")
            }

            // The legacy bare string `"Disqualify"` (a unit variant on the wire).
            fn visit_str<E: de::Error>(self, value: &str) -> Result<Penalty, E> {
                match value {
                    "Disqualify" => Ok(Penalty::Disqualify { reason: None }),
                    other => Err(de::Error::unknown_variant(other, &["Disqualify"])),
                }
            }

            // The externally-tagged object form `{"Variant": <body>}` for every variant
            // (including a reason-carrying `Disqualify`).
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Penalty, A::Error> {
                let tag: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty Penalty object"))?;
                let penalty = match tag.as_str() {
                    "Disqualify" => Penalty::Disqualify {
                        reason: map.next_value::<DisqualifyRepr>()?.reason,
                    },
                    "TimeAdded" => Penalty::TimeAdded {
                        micros: map.next_value::<TimeAddedRepr>()?.micros,
                    },
                    "PointsDeducted" => Penalty::PointsDeducted {
                        points: map.next_value::<PointsRepr>()?.points,
                    },
                    "PointsAdded" => Penalty::PointsAdded {
                        points: map.next_value::<PointsRepr>()?.points,
                    },
                    other => {
                        return Err(de::Error::unknown_variant(
                            other,
                            &["Disqualify", "TimeAdded", "PointsDeducted", "PointsAdded"],
                        ));
                    }
                };
                // Reject a trailing second key — an externally-tagged variant is a single entry.
                if map.next_key::<String>()?.is_some() {
                    return Err(de::Error::custom(
                        "Penalty object has more than one variant",
                    ));
                }
                Ok(penalty)
            }
        }

        deserializer.deserialize_any(PenaltyVisitor)
    }
}

/// The body of a serialized `Disqualify { reason }` — its optional `reason` field, defaulting to
/// `None` so `{"Disqualify":{}}` (an empty body) reads back as a reason-less DQ.
#[derive(Deserialize)]
struct DisqualifyRepr {
    #[serde(default)]
    reason: Option<String>,
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

    /// A chunk of captured per-node RSSI trace (marshaling Slice 1 — the signal-as-evidence
    /// plumbing, marshaling.html §3.2). An **immutable raw observation** like [`Pass`]: a
    /// signal-capable adapter appends these incrementally as a heat runs, and the
    /// `gridfpv_projection::signal_trace` projection folds a competitor's chunks back into a
    /// contiguous trace. A timer with no signal (sim/Velocidrone) never emits this, so the
    /// signal layer is simply absent there.
    SignalChunk(SignalChunk),
    /// The one-shot enter/exit detection thresholds for a node (see [`SignalThresholds`]).
    /// Captured alongside the trace so the evidence carries the levels the timer detected
    /// against; the last one per `(adapter, competitor)` wins.
    SignalThresholds(SignalThresholds),
    /// The **dense, full-fidelity** per-node RSSI history for a heat, pulled from RotorHazard's
    /// request-driven `current_marshal_data` at heat end (see [`SignalHistory`]). Supersedes the
    /// coarse streaming [`SignalChunk`] samples for its competitor in the `signal_trace` projection.
    SignalHistory(SignalHistory),

    // --- race-engine events (#28) ---
    /// A round's **seeded field is drawn** — the one-time freeze of a carry seeding's
    /// resolution (issue #334; decision D18's "one grouping decision" extended to the field).
    ///
    /// A round seeded **from another round's outcome** (`FromRanking` / `FromRankingRange` /
    /// `FromHeatWinners` / `Combine`) records its resolved field here at **first fill**, and
    /// every later read (fills, ranking, standings, dependent seeding) uses the recorded
    /// draw. Without this, the seeding re-resolved live on every read — so adjudicating the
    /// *source* round after this round had already raced silently rewrote who this round's
    /// field "was", vanishing raced results from its ranking. Roster-derived seedings
    /// (`FromRoster` / `AllChannels`) never record a draw: they stay live so late entrants
    /// keep working.
    RoundFieldDrawn {
        /// The round whose field this freezes.
        round: RoundId,
        /// The resolved field, in seed order — the draw every later read replays.
        field: Vec<CompetitorRef>,
    },
    /// A heat is created with its lineup and enters the `Scheduled` state — the
    /// `[*] → Scheduled` entry of the heat loop (race-engine.html §2). Carries the
    /// competitors in the heat and, additively, the class/round it belongs to and the
    /// per-pilot frequency assignment.
    ///
    /// The `class`, `round`, `frequencies`, and `label` fields are **additive** and
    /// default-absent: a heat scheduled without them (the free-text NewHeat path, a
    /// sim race, a pre-existing log entry) reads back as `None`/empty, so older logs
    /// round-trip unchanged. `frequencies` pairs each competitor with a raw-MHz channel
    /// (e.g. `5800`); empty means none assigned (sim/none). `label` is the optional
    /// human name a manual build-heat carries (see the field doc).
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
        /// An **optional human label** the RD typed when building this heat by hand. When
        /// present it is the heat's display name everywhere (overriding the derived
        /// "‹Round› Heat N" / tier convention); `None` for a generator-filled heat, which
        /// keeps the auto-name. Additive and default-absent, so a pre-existing log (or a
        /// generator heat) reads back as `None` and round-trips unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        label: Option<String>,
    },
    /// A heat-loop state transition appended by the engine (race-engine.html §2).
    HeatStateChanged {
        heat: HeatId,
        transition: HeatTransition,
    },
    /// The RD **manually selected the current heat** in Live control — the explicit
    /// "show/control *this* heat" the console appends so the live projection follows the
    /// RD's choice rather than auto-following a freshly-scheduled heat.
    ///
    /// Event-sourced like every other RD action: the live `current_heat` derivation folds
    /// the last of these (alongside `HeatStateChanged`) to decide which heat is on the
    /// timer, so a replay is deterministic. Filling a new heat only adds it to the list /
    /// on-deck; focus moves on a real transition or on this explicit selection.
    CurrentHeatSelected { heat: HeatId },
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
    /// The **auto-official timer armed** for a heat that just entered `Unofficial` (marshaling
    /// Slice 5 — the provisional → official lifecycle, marshaling.html §3.3). When the heat's round
    /// configures a [`ProtestWindow::After`](gridfpv_engine::heat::ProtestWindow::After), the
    /// Director runtime writes the **deadline** here as a *fact* — the server-clock instant at which
    /// it will auto-append the `Unofficial → Final` `Finalize` — so the console can render a live
    /// "auto-official in M:SS" countdown and a replay reads the **same** deadline instead of
    /// recomputing it from a clock.
    ///
    /// Mirrors [`HeatStarting`](Event::HeatStarting): the runtime logs the chosen timing once, at
    /// emission time, and the subsequent `Finalize` lands at the deadline; the engine/projection
    /// fold never reads a clock (race-engine.html §6). Additive and default-absent — a heat whose
    /// round has no protest window (the default) never emits this, and an older log round-trips
    /// unchanged.
    HeatFinalizing {
        /// The heat whose protest window is open (it is in `Unofficial`).
        heat: HeatId,
        /// The **auto-official deadline**: the server wall-clock instant (microseconds since the
        /// Unix epoch) at which the runtime appends the auto `Finalize`. The countdown the console
        /// shows is `at − now`.
        #[ts(type = "number")]
        at: i64,
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
        /// The heat the inserted lap belongs to. Unlike a raw [`Pass`] (an untagged wire
        /// observation attributed positionally), an insertion is an RD statement **about a
        /// specific heat** — often a finished one being marshaled while a later heat runs —
        /// so it carries the tag and the heat window routes it by tag, never by position.
        /// `None` only on legacy logs / commands from before the tag existed (positional
        /// attribution, the old behavior). Additive: serde defaults it, `#[ts(optional)]`
        /// keeps the generated TS field `heat?:`.
        #[serde(default)]
        #[ts(optional)]
        heat: Option<HeatId>,
    },
    /// Marshaling: re-time a previously-detected pass (referenced by log offset).
    ///
    /// Because a lap is two consecutive passes, re-timing a pass shifts the *two*
    /// adjacent lap durations that share it — the projection's `corrected_passes`
    /// recomputes those durations naturally from the moved pass (no extra event).
    ///
    /// NOTE: FPVTrackside's alternative keeps *total race time* constant by also
    /// shifting the neighbour's far boundary; that is a deferred product nuance — the
    /// default here is the natural per-edit duration shift (marshaling.html §3.1).
    LapAdjusted { target: LogRef, at: SourceTime },
    /// Marshaling: **split** one over-long lap (the lap *ending* at `target`) into two by
    /// inserting a synthetic mid-lap pass at `at` — the FPVTrackside "split" action for a
    /// missed mid-lap detection (marshaling.html §3.1).
    ///
    /// A distinct event from [`LapInserted`](Event::LapInserted) (not sugar over it) so the
    /// Slice 3 audit trail can name the action — "lap split" reads cleaner than a bare
    /// insert. The projection folds it by emitting the synthetic pass into the corrected
    /// stream **addressable by this event's own offset**, so it is fully reversible: a later
    /// [`DetectionVoided`](Event::DetectionVoided) of this offset removes the split again
    /// (and "void the void" restores it).
    LapSplit {
        /// The log offset of the pass that *ends* the over-long lap being split.
        target: LogRef,
        /// When the inserted mid-lap crossing happened, on the source clock — between the
        /// `target` lap's start and `target` itself.
        at: SourceTime,
    },
    /// Marshaling: **throw out a single valid lap** from a competitor's *scored* count
    /// (marshaling.html §3.3, the adjudication framework). The lap *ending* at `target` is a
    /// **real** lap — it stays in the lap list and the audit — but it is **excluded from the
    /// scored result** (the counted-lap set / best-lap / consecutive window).
    ///
    /// This is **distinct from [`DetectionVoided`](Event::DetectionVoided)**: a void says "this
    /// detection was never real" and *removes the pass* (merging the two adjacent laps); a
    /// throw-out says "this lap really happened but does not count for this competitor" and leaves
    /// the lap intact, only dropping it from scoring. (It is also distinct from a future season
    /// "drop-worst-round" rule — that is round-level; this is **lap-level**, within one heat.)
    ///
    /// The scorer excludes the lap whose **end pass** is `target` **deterministically and
    /// order-independently** (a pure set membership, not an evaluation-order effect — the
    /// throw-out determinism risk, marshaling-plan.html §4). Reversible like any ruling via
    /// [`RulingReversed`](Event::RulingReversed) of *this* event's offset.
    LapThrownOut {
        /// The log offset of the pass that *ends* the lap to exclude from the scored count.
        target: LogRef,
    },
    /// Marshaling: void an entire heat.
    HeatVoided { heat: HeatId },
    /// Marshaling: apply a penalty to a competitor in a heat.
    PenaltyApplied {
        heat: HeatId,
        competitor: CompetitorRef,
        penalty: Penalty,
    },
    /// Marshaling: a **protest was filed** against a heat result (marshaling.html §3.3 — the
    /// adjudication framework's protest workflow). An **append-only fact**: filing records that a
    /// protest exists; resolving it is a *separate* [`ProtestResolved`](Event::ProtestResolved)
    /// fact targeting this one. There is **no `by` / actor** — per the no-login model every action
    /// is the RD at the console (filed on a pilot's behalf), so naming an actor would be false
    /// precision (marshaling-plan.html §2). Reversible via [`RulingReversed`](Event::RulingReversed)
    /// and rendered in the audit.
    ProtestFiled {
        /// The heat the protest concerns.
        heat: HeatId,
        /// The competitor the protest is about (whose result is contested).
        competitor: CompetitorRef,
        /// A free-text note describing the protest.
        note: String,
    },
    /// Marshaling: a **protest was resolved** — the second half of the append-only protest pair,
    /// targeting the [`ProtestFiled`](Event::ProtestFiled) it closes by its log offset. The
    /// `outcome` records the ruling ([`Upheld`](ProtestOutcome::Upheld) /
    /// [`Denied`](ProtestOutcome::Denied) / [`Withdrawn`](ProtestOutcome::Withdrawn)). Like the
    /// filing it carries no actor, is reversible, and renders in the audit.
    ProtestResolved {
        /// The log offset of the [`ProtestFiled`](Event::ProtestFiled) this resolves.
        target: LogRef,
        /// How the protest was resolved.
        outcome: ProtestOutcome,
    },
    /// Marshaling: **reverse a prior ruling**, referenced by its log offset — the generalized,
    /// structural undo for **any** adjudication (marshaling.html §3.3 "everything reversible").
    ///
    /// Originally scoped to penalties (Slice 2); now generalized (Slice 6) so a reversal can target
    /// **any** ruling — a [`PenaltyApplied`](Event::PenaltyApplied) (DQ / time / points), a
    /// [`LapThrownOut`](Event::LapThrownOut), a [`ProtestResolved`](Event::ProtestResolved), or a
    /// [`HeatVoided`](Event::HeatVoided). The reversal is **structural** ("void the void"): the fold
    /// drops the ruling at `target` from the result, and a reversal can itself be reversed.
    ///
    /// A distinct event from [`DetectionVoided`](Event::DetectionVoided) so the audit reads cleanly
    /// — "DQ reversed" / "throw-out reversed" rather than overloading the lap-level void.
    RulingReversed {
        /// The log offset of the ruling to reverse (a penalty, throw-out, protest resolution, or
        /// heat-void).
        target: LogRef,
    },
}

/// How a [`ProtestFiled`](Event::ProtestFiled) was resolved (marshaling.html §3.3). A small,
/// closed enum recorded by [`ProtestResolved`](Event::ProtestResolved); externally tagged on the
/// wire like the rest of the event vocabulary, so it maps to a TS string union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ProtestOutcome {
    /// The protest was **upheld** — the contesting party's claim was accepted.
    Upheld,
    /// The protest was **denied** — the result stands as adjudicated.
    Denied,
    /// The protest was **withdrawn** before a ruling.
    Withdrawn,
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
                label: None,
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
                label: None,
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
            Event::CurrentHeatSelected {
                heat: HeatId("q-2".into()),
            },
            Event::DetectionVoided { target: LogRef(42) },
            Event::LapInserted {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-0".into()),
                at: SourceTime::from_micros(5_000_000),
                heat: None,
            },
            Event::LapAdjusted {
                target: LogRef(43),
                at: SourceTime::from_micros(5_100_000),
            },
            Event::LapSplit {
                target: LogRef(44),
                at: SourceTime::from_micros(5_050_000),
            },
            Event::RulingReversed { target: LogRef(45) },
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
                penalty: Penalty::Disqualify { reason: None },
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
        // A heat with no class/round/frequencies/label (the free-text NewHeat path, a
        // sim race) serialises *exactly* like the pre-tag shape: the new fields are
        // skipped entirely, so the wire stays byte-compatible with old logs.
        let event = Event::HeatScheduled {
            heat: HeatId("q-1".into()),
            lineup: vec![CompetitorRef("node-0".into())],
            class: None,
            round: None,
            frequencies: vec![],
            label: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("class"), "absent class omitted: {json}");
        assert!(!json.contains("round"), "absent round omitted: {json}");
        assert!(
            !json.contains("frequencies"),
            "empty frequencies omitted: {json}"
        );
        assert!(!json.contains("label"), "absent label omitted: {json}");
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
            label: Some("Featured Heat".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("class") && json.contains("round") && json.contains("frequencies"));
        assert!(json.contains("label") && json.contains("Featured Heat"));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn lap_split_and_ruling_reversed_round_trip() {
        // The two new Slice-2 marshaling facts round-trip through the externally-tagged JSON.
        let events = vec![
            Event::LapSplit {
                target: LogRef(7),
                at: SourceTime::from_micros(3_500_000),
            },
            Event::RulingReversed { target: LogRef(9) },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn slice6_adjudication_events_round_trip() {
        // Every new Slice-6 fact round-trips through the externally-tagged JSON.
        let events = vec![
            Event::LapThrownOut { target: LogRef(11) },
            Event::ProtestFiled {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("AcroAce".into()),
                note: "cut the chicane on lap 3".into(),
            },
            Event::ProtestResolved {
                target: LogRef(12),
                outcome: ProtestOutcome::Upheld,
            },
            Event::PenaltyApplied {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("Bee".into()),
                penalty: Penalty::PointsDeducted { points: 5 },
            },
            Event::PenaltyApplied {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("Bee".into()),
                penalty: Penalty::PointsAdded { points: 2 },
            },
            Event::PenaltyApplied {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("Cee".into()),
                penalty: Penalty::Disqualify {
                    reason: Some("unsafe flying".into()),
                },
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back);
        }
    }

    #[test]
    fn protest_outcomes_round_trip() {
        for outcome in [
            ProtestOutcome::Upheld,
            ProtestOutcome::Denied,
            ProtestOutcome::Withdrawn,
        ] {
            let json = serde_json::to_string(&outcome).unwrap();
            let back: ProtestOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(outcome, back);
        }
    }

    #[test]
    fn disqualify_without_reason_serializes_as_the_legacy_bare_string() {
        // A reason-less DQ stays byte-compatible with the legacy `"Disqualify"` wire form, so old
        // consumers and old logs round-trip unchanged. A DQ *with* a reason takes the struct form.
        let bare = Penalty::Disqualify { reason: None };
        assert_eq!(serde_json::to_string(&bare).unwrap(), r#""Disqualify""#);
        assert_eq!(
            serde_json::from_str::<Penalty>(r#""Disqualify""#).unwrap(),
            bare
        );

        let with_reason = Penalty::Disqualify {
            reason: Some("cut the course".into()),
        };
        let json = serde_json::to_string(&with_reason).unwrap();
        assert_eq!(json, r#"{"Disqualify":{"reason":"cut the course"}}"#);
        assert_eq!(serde_json::from_str::<Penalty>(&json).unwrap(), with_reason);
        // An empty struct body also reads back as a reason-less DQ.
        assert_eq!(
            serde_json::from_str::<Penalty>(r#"{"Disqualify":{}}"#).unwrap(),
            bare
        );
    }

    #[test]
    fn points_penalties_round_trip_on_the_wire() {
        // The two standings-only penalties carry their `points` as a bare integer.
        let deducted = Penalty::PointsDeducted { points: 7 };
        assert_eq!(
            serde_json::to_string(&deducted).unwrap(),
            r#"{"PointsDeducted":{"points":7}}"#
        );
        assert_eq!(
            serde_json::from_str::<Penalty>(r#"{"PointsDeducted":{"points":7}}"#).unwrap(),
            deducted
        );
        let added = Penalty::PointsAdded { points: 3 };
        assert_eq!(
            serde_json::from_str::<Penalty>(r#"{"PointsAdded":{"points":3}}"#).unwrap(),
            added
        );
    }

    #[test]
    fn time_added_wire_shape_is_unchanged() {
        // Hand-rolling serde for `Penalty` must not change `TimeAdded`'s legacy wire shape.
        let p = Penalty::TimeAdded { micros: 2_000_000 };
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"TimeAdded":{"micros":2000000}}"#
        );
        assert_eq!(
            serde_json::from_str::<Penalty>(r#"{"TimeAdded":{"micros":2000000}}"#).unwrap(),
            p
        );
    }

    #[test]
    fn legacy_log_without_slice6_variants_reads_back() {
        // A log written before the Slice-6 facts existed carries none of them; the additive
        // variants leave every pre-existing serialized event deserializing unchanged.
        let legacy_penalty = r#"{"PenaltyApplied":{"heat":"m","competitor":"B","penalty":{"TimeAdded":{"micros":500000}}}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy_penalty).unwrap(),
            Event::PenaltyApplied {
                heat: HeatId("m".into()),
                competitor: CompetitorRef("B".into()),
                penalty: Penalty::TimeAdded { micros: 500_000 },
            }
        );
    }

    #[test]
    fn legacy_log_without_split_or_reversal_reads_back() {
        // An old log written before `LapSplit`/`RulingReversed` existed carries only the
        // pre-Slice-2 marshaling variants. Adding the new variants is purely additive on the
        // externally-tagged `Event` enum, so every pre-existing serialized event still
        // deserializes unchanged — mirrors `legacy_heat_scheduled_reads_back_with_defaults`.
        let legacy_void = r#"{"DetectionVoided":{"target":42}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy_void).unwrap(),
            Event::DetectionVoided { target: LogRef(42) }
        );
        let legacy_adjust = r#"{"LapAdjusted":{"target":43,"at":5100000}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy_adjust).unwrap(),
            Event::LapAdjusted {
                target: LogRef(43),
                at: SourceTime::from_micros(5_100_000),
            }
        );
        let legacy_penalty =
            r#"{"PenaltyApplied":{"heat":"main-a","competitor":"Bee","penalty":"Disqualify"}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy_penalty).unwrap(),
            Event::PenaltyApplied {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("Bee".into()),
                penalty: Penalty::Disqualify { reason: None },
            }
        );
    }

    #[test]
    fn signal_trace_events_round_trip() {
        // The Slice-1 signal-as-evidence facts round-trip through externally-tagged JSON.
        let events = vec![
            Event::SignalChunk(SignalChunk {
                adapter: AdapterId("rotorhazard".into()),
                competitor: CompetitorRef("node-0".into()),
                from: SourceTime::from_micros(2_215_296),
                period_micros: 100_000,
                rssi: vec![70, 72, 150, 148, 71, 70],
            }),
            Event::SignalThresholds(SignalThresholds {
                adapter: AdapterId("rotorhazard".into()),
                competitor: CompetitorRef("node-0".into()),
                enter: 90,
                exit: 80,
            }),
            Event::SignalHistory(SignalHistory {
                adapter: AdapterId("rotorhazard".into()),
                competitor: CompetitorRef("node-0".into()),
                times: vec![0, 50_000, 100_000, 150_000],
                rssi: vec![70, 88, 150, 71],
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
        let legacy_pass =
            r#"{"Pass":{"adapter":"velocidrone","competitor":"AcroAce","at":12500000}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(legacy_pass).unwrap(),
            Event::Pass(Pass {
                adapter: AdapterId("velocidrone".into()),
                competitor: CompetitorRef("AcroAce".into()),
                at: SourceTime::from_micros(12_500_000),
                sequence: None,
                gate: GateIndex::LAP,
                signal: None,
            })
        );
        // And the new facts themselves deserialize from their on-the-wire shape.
        let chunk = r#"{"SignalChunk":{"adapter":"rotorhazard","competitor":"node-1","from":5000000,"period_micros":100000,"rssi":[60,120,61]}}"#;
        assert_eq!(
            serde_json::from_str::<Event>(chunk).unwrap(),
            Event::SignalChunk(SignalChunk {
                adapter: AdapterId("rotorhazard".into()),
                competitor: CompetitorRef("node-1".into()),
                from: SourceTime::from_micros(5_000_000),
                period_micros: 100_000,
                rssi: vec![60, 120, 61],
            })
        );
    }

    #[test]
    fn legacy_heat_scheduled_reads_back_with_defaults() {
        // A pre-existing serialized `HeatScheduled` (before the class/round/frequencies/
        // label tags existed) must still deserialize, with the new fields defaulting to
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
                label: None,
            }
        );
    }
}
