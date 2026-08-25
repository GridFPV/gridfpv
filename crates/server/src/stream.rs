//! The realtime change stream (protocol.html §3): the per-stream [`Cursor`] and the
//! [`ChangeEnvelope`]s that advance a client's projections after the snapshot.
//!
//! After fetching a [`Snapshot`](crate::snapshot::Snapshot), the client opens a
//! WebSocket and receives an ordered run of change envelopes. "Snapshot + incremental
//! changes" (§3): the stream sends **deltas, not repeated full state** — a new lap, a
//! heat-state transition, a re-scored result — and may collapse a burst into a single
//! fresh-value envelope when a delta would be larger or ambiguous (e.g. after a
//! marshaling re-fold of a whole lap list).
//!
//! Each envelope names the projection it touches (via the
//! [`ProjectionKind`](crate::snapshot::ProjectionKind) for a delta, or the
//! [`ProjectionBody`](crate::snapshot::ProjectionBody) itself for a fresh value) and
//! carries the monotonic [`Cursor`] the client applies in order.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ProtocolError;
use crate::snapshot::{ProjectionBody, ProjectionKind};

/// The public **projection sequence number** (protocol.html §3, §9.5): a single
/// monotonic integer per stream, increasing by one per envelope.
///
/// It is *not* the raw log offset — one log append can fan out into several projection
/// changes or none a client subscribes to. The protocol commits to this projection
/// sequence as its public ordering; the log offset stays a private Director/Cloud
/// detail. The snapshot hands the client a starting cursor (§2) and every envelope
/// advances it; on reconnect the client presents its last-applied cursor to resume
/// (§3).
///
/// Transparent `u64` newtype; `#[ts(as = "f64")]` so it renders as a plain TS
/// `number`. Our cursors/sequences are bounded well below 2^53, so `number` is exact
/// and avoids the wire/type mismatch a `u64` → wide-integer TS mapping would introduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/", as = "f64")]
pub struct Cursor {
    /// The monotonic per-stream sequence value.
    pub seq: u64,
}

impl Cursor {
    /// Construct a cursor from a raw sequence value.
    pub const fn new(seq: u64) -> Self {
        Self { seq }
    }

    /// The next cursor in sequence (`seq + 1`) — the envelope that follows this one.
    pub const fn next(self) -> Self {
        Self { seq: self.seq + 1 }
    }
}

/// How an envelope conveys a projection change (protocol.html §3, §9.2): a **delta**
/// for append-heavy projections (lap lists, live state) or a **fresh value** for cheap
/// or re-folded ones (a heat result, a ranking after a marshaling correction).
///
/// Externally tagged, mapping to a TS discriminated union.
///
/// > **Deferred (#43):** the exact per-projection delta *encodings* are a placeholder.
/// > [`Change::Delta`] carries a `serde_json::Value` rather than a typed delta for each
/// > [`ProjectionKind`]; the precise delta shapes (a single appended lap, a heat-state
/// > transition) are pinned when the change stream lands. The *structure* — the
/// > per-projection delta-vs-fresh-value distinction protocol.html §9.2 fixes — is
/// > what matters now and is captured here. A fresh value, by contrast, is already a
/// > fully-typed [`ProjectionBody`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Change {
    /// An incremental delta against the projection named by the envelope's
    /// [`ChangeEnvelope::projection`]. Placeholder encoding (`serde_json::Value`) until
    /// #43 pins the typed per-projection delta shapes.
    Delta(#[ts(type = "unknown")] serde_json::Value),
    /// The projection's complete new value — used when a delta would be larger or
    /// ambiguous (a marshaling re-fold, a cheap projection). Fully typed already.
    FreshValue(ProjectionBody),
}

/// A change envelope (protocol.html §3): one ordered, sequenced update to a single
/// projection.
///
/// Each envelope carries its monotonic `sequence` [`Cursor`], names the `projection` it
/// touches (as a [`ProjectionKind`] — for a [`Change::FreshValue`] the body also
/// carries the kind, but naming it here keeps deltas and fresh values uniform), and a
/// `change` that is a delta or a fresh value. The client applies envelopes in strictly
/// increasing `sequence` order; re-delivering one already applied is a no-op keyed by
/// sequence (§3 "idempotent application", "at-least-once, deduplicated").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ChangeEnvelope {
    /// This envelope's position in the per-stream sequence.
    pub sequence: Cursor,
    /// Which projection this change advances.
    pub projection: ProjectionKind,
    /// The delta or fresh value to apply.
    pub change: Change,
}

/// A frame the server sends down the change-stream WebSocket (protocol.html §3).
///
/// Almost every frame is a [`ChangeEnvelope`]; the one exception is the distinguished
/// **re-snapshot-required** signal the server sends when a client resumes from a cursor
/// older than the bounded retained window (§3 "fall back to re-snapshot"). Modelling
/// both as one externally-tagged enum means a client reads a single message type off the
/// socket and branches on the tag, rather than guessing whether a frame is data or a
/// control signal.
///
/// Externally tagged, mapping to a TS discriminated union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum StreamMessage {
    /// One ordered, sequenced projection change to apply (the common case).
    ///
    /// **Boxed** so the enum is a pointer rather than a whole projection body: an envelope
    /// carries a `LiveRaceState`, which is a wide struct that only ever grows as the live view
    /// gains fields (the #397 crossing feed being the latest), and every non-`Change` frame would
    /// otherwise pay that width too. Serde is transparent through the `Box` and ts-rs renders it as
    /// the inner type, so the wire shape and the TS binding are unchanged.
    Change(Box<ChangeEnvelope>),
    /// The resume cursor the client presented is older than the server's retained
    /// window: the gap cannot be replayed, so the client must fetch a fresh snapshot and
    /// resubscribe from its new cursor (protocol.html §3). The carried
    /// [`ProtocolError`] always has [`ErrorCode::StaleCursor`](crate::error::ErrorCode::StaleCursor).
    /// This is the terminal frame of the stream — no envelopes follow it.
    ReSnapshotRequired(ProtocolError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{HeatPhase, LiveRaceState};
    use gridfpv_events::HeatId;

    #[test]
    fn cursor_is_a_bare_integer_on_the_wire() {
        // Transparent newtype: serialises as the raw sequence integer.
        assert_eq!(serde_json::to_string(&Cursor::new(42)).unwrap(), "42");
    }

    #[test]
    fn cursor_next_increments() {
        assert_eq!(Cursor::new(5).next(), Cursor::new(6));
    }

    #[test]
    fn fresh_value_envelope_round_trips() {
        let env = ChangeEnvelope {
            sequence: Cursor::new(101),
            projection: ProjectionKind::LiveRaceState,
            change: Change::FreshValue(ProjectionBody::LiveRaceState(LiveRaceState {
                current_heat: Some(HeatId("q-1".into())),
                phase: HeatPhase::Running,
                ..Default::default()
            })),
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: ChangeEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn delta_envelope_round_trips_with_placeholder_payload() {
        // The delta is an opaque JSON value until #43 pins typed per-projection deltas.
        let env = ChangeEnvelope {
            sequence: Cursor::new(102),
            projection: ProjectionKind::LapList,
            change: Change::Delta(serde_json::json!({ "appended_lap": { "number": 3 } })),
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: ChangeEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn stream_message_variants_round_trip() {
        use crate::error::{ErrorCode, ProtocolError};

        let change = StreamMessage::Change(Box::new(ChangeEnvelope {
            sequence: Cursor::new(1),
            projection: ProjectionKind::LiveRaceState,
            change: Change::FreshValue(ProjectionBody::LiveRaceState(LiveRaceState::default())),
        }));
        let json = serde_json::to_string(&change).unwrap();
        assert_eq!(
            serde_json::from_str::<StreamMessage>(&json).unwrap(),
            change
        );

        let resnap = StreamMessage::ReSnapshotRequired(ProtocolError::new(
            ErrorCode::StaleCursor,
            "cursor 3 is below the retained window (oldest replayable offset 8)",
        ));
        let json = serde_json::to_string(&resnap).unwrap();
        assert_eq!(
            serde_json::from_str::<StreamMessage>(&json).unwrap(),
            resnap
        );
    }
}
