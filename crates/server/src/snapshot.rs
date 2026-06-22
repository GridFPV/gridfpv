//! The snapshot read model (protocol.html §2) and the served projection bodies (§1).
//!
//! A client begins by fetching a [`Snapshot`]: the current materialized state of the
//! projection it scoped to, plus the [`Cursor`](crate::stream::Cursor) the change
//! stream resumes from. "Snapshot first, then subscribe" — the snapshot is
//! self-contained and immediately renderable, and its cursor lets the stream start
//! exactly where the snapshot ended so nothing is missed or double-applied (§2, §3).
//!
//! # What the protocol serves — projections, not the log (§1)
//!
//! The wire never carries the raw event log; it carries **projections** — the answers
//! derived by folding the log. [`ProjectionBody`] is the closed set of served
//! projection shapes, each reusing the *existing* projection/engine output type rather
//! than redefining it (protocol.html §1: this doc "does not redefine the projections
//! themselves"):
//!
//! - [`LiveRaceState`] — the latency-sensitive live core (a #41 placeholder here);
//! - [`LapList`](gridfpv_projection::LapList) — per-pilot laps, marshaling already
//!   folded in;
//! - [`HeatResult`](gridfpv_engine::scoring::HeatResult) — the scored heat outcome;
//! - ranking — a qualifying / bracket [`RankEntry`](gridfpv_engine::format::RankEntry)
//!   list (the provisional-or-final ordering a format exposes);
//! - [`EventOutcome`](gridfpv_engine::event::EventOutcome) — the full-event standings.
//!
//! [`ProjectionKind`] is the bare discriminant (which projection, no body) — what a
//! [`ChangeEnvelope`](crate::stream::ChangeEnvelope) names when it carries a *delta*
//! rather than a fresh value.

use gridfpv_engine::event::EventOutcome;
use gridfpv_engine::format::RankEntry;
use gridfpv_engine::scoring::HeatResult;
use gridfpv_projection::LapList;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::stream::Cursor;

// The live race-state projection (#41) lives in [`crate::live_state`]; it is re-exported
// here so `ProjectionBody::LiveRaceState` keeps naming a snapshot-module type.
pub use crate::live_state::{LiveRaceState, PilotProgress};

/// The heat-loop phase the live state reports (protocol.html §1: `Scheduled → Staged →
/// Armed → Running → Finished → Final`).
///
/// This is the *projected* view of the heat loop — the folded current phase a client
/// renders — not the raw [`HeatTransition`](gridfpv_events::HeatTransition) event the
/// engine appends. The off-ramp transitions (abort/restart/discard) resolve back onto
/// one of these phases, so the live view stays a simple linear status. A #41-era detail
/// the placeholder pins minimally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum HeatPhase {
    /// The heat exists with a lineup but has not begun.
    Scheduled,
    /// Countdown begun; frequencies assigned.
    Staged,
    /// The gate is open to detections.
    Armed,
    /// The race is running; passes are being consumed.
    Running,
    /// The race has closed — time elapsed or all finished — but is not yet scored.
    /// Mirrors the canonical engine state `HeatState::Finished` (code-conventions §4).
    Finished,
    /// The result is finalized.
    Final,
}

/// Which projection a snapshot body or change envelope is *about* — the bare
/// discriminant with no payload (protocol.html §3).
///
/// A fresh-value envelope carries the whole [`ProjectionBody`] (kind *and* value); a
/// delta envelope carries this `ProjectionKind` plus an opaque delta, naming the
/// projection it advances without re-sending it. Keeping the kind separate from the
/// body lets a delta name its target cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ProjectionKind {
    /// The live race-state ([`LiveRaceState`]).
    LiveRaceState,
    /// A lap list ([`LapList`](gridfpv_projection::LapList)).
    LapList,
    /// A scored heat result ([`HeatResult`](gridfpv_engine::scoring::HeatResult)).
    HeatResult,
    /// A qualifying / bracket ranking (a [`RankEntry`](gridfpv_engine::format::RankEntry) list).
    Ranking,
    /// The full-event outcome / standings ([`EventOutcome`](gridfpv_engine::event::EventOutcome)).
    EventOutcome,
}

/// A served projection **with its value** (protocol.html §1) — the closed set of
/// projection shapes the contract carries, each reusing the existing projection/engine
/// output type. Externally tagged, so it maps to a TS discriminated union keyed by
/// projection kind.
///
/// This is the body of a [`Snapshot`] and of a fresh-value
/// [`ChangeEnvelope`](crate::stream::ChangeEnvelope). Adding a new served projection is
/// an additive variant (§7); an older client ignores a kind it does not understand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ProjectionBody {
    /// The live race-state (protocol.html §1, the latency-sensitive core).
    LiveRaceState(LiveRaceState),
    /// A per-pilot lap list, with marshaling adjudications already folded in.
    LapList(LapList),
    /// A scored heat result — finishing order, lap counts, deciding metrics.
    HeatResult(HeatResult),
    /// A qualifying or bracket ranking (best first; provisional mid-format, final once
    /// the format completes).
    Ranking(Vec<RankEntry>),
    /// The full-event outcome: the qualifying ranking, bracket standings, and winner.
    EventOutcome(EventOutcome),
}

impl ProjectionBody {
    /// The [`ProjectionKind`] discriminant for this body — what a delta envelope names.
    pub fn kind(&self) -> ProjectionKind {
        match self {
            ProjectionBody::LiveRaceState(_) => ProjectionKind::LiveRaceState,
            ProjectionBody::LapList(_) => ProjectionKind::LapList,
            ProjectionBody::HeatResult(_) => ProjectionKind::HeatResult,
            ProjectionBody::Ranking(_) => ProjectionKind::Ranking,
            ProjectionBody::EventOutcome(_) => ProjectionKind::EventOutcome,
        }
    }
}

/// A snapshot of a scoped projection (protocol.html §2): the current materialized
/// [`ProjectionBody`] plus the [`Cursor`] the change stream resumes from.
///
/// "Snapshot first, then subscribe" (§2): the client renders the `body` immediately and
/// opens the stream `from` this `cursor`, so the first envelope it applies is exactly
/// the one after the snapshot — nothing missed, nothing double-applied. Re-snapshotting
/// is always correct because projections are recomputable; the stream is an
/// optimization, never the source of truth (§3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Snapshot {
    /// The stream cursor this snapshot was taken at — where the subscription resumes.
    pub cursor: Cursor,
    /// The materialized projection at that cursor.
    pub body: ProjectionBody,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_live() -> ProjectionBody {
        ProjectionBody::LiveRaceState(LiveRaceState {
            current_heat: Some(gridfpv_events::HeatId("q-1".into())),
            phase: HeatPhase::Running,
            ..Default::default()
        })
    }

    #[test]
    fn snapshot_round_trips() {
        let snap = Snapshot {
            cursor: Cursor::new(7),
            body: sample_live(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn empty_lap_list_body_round_trips() {
        let snap = Snapshot {
            cursor: Cursor::new(0),
            body: ProjectionBody::LapList(LapList::default()),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    #[test]
    fn body_kind_matches_variant() {
        assert_eq!(sample_live().kind(), ProjectionKind::LiveRaceState);
        assert_eq!(
            ProjectionBody::Ranking(vec![]).kind(),
            ProjectionKind::Ranking
        );
        assert_eq!(
            ProjectionBody::LapList(LapList::default()).kind(),
            ProjectionKind::LapList
        );
    }

    #[test]
    fn projection_kind_round_trips() {
        use ProjectionKind::*;
        for kind in [LiveRaceState, LapList, HeatResult, Ranking, EventOutcome] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: ProjectionKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }
}
