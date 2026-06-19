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

/// Identifies the timing source / adapter that produced an event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdapterId(pub String);

/// A source-local competitor handle: a node seat, a sim player name, a transponder
/// id. Bound to a GridFPV pilot later by a *registration* action — never by the
/// adapter (see Architecture §9). The adapter only reports the refs it sees.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompetitorRef(pub String);

/// A source's own race/heat identifier, where it exposes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

/// A timestamp in the **source's own clock**, stored as microseconds.
///
/// Lap and split durations are computed from these values, which are internally
/// consistent and immune to network jitter. Game-engine time (Velocidrone), server
/// time (RotorHazard), device RTC (LapRF) and wall clock (manual) all reduce to a
/// microsecond count on their own timeline; the adapter maps that timeline onto the
/// Director's session axis separately. Integer microseconds keep interval math exact
/// and comparisons stable (no float-equality hazards in tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalContext {
    /// Peak RSSI at the crossing, if reported.
    pub rssi_peak: Option<f32>,
}

/// A gate crossing — the one observation everything else derives from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    /// The gate crossed; defaults to the lap gate.
    #[serde(default, skip_serializing_if = "GateIndex::is_lap_gate")]
    pub gate: GateIndex,
    /// Optional signal context (hardware only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<SignalContext>,
}

/// A canonical event appended to the log by an adapter.
///
/// Externally tagged (the default serde representation): each variant serialises as
/// `{ "VariantName": { ..fields } }`, which maps cleanly to a discriminated union in
/// the generated TypeScript (#4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// A gate crossing — the atom (see [`Pass`]).
    Pass(Pass),
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
}
