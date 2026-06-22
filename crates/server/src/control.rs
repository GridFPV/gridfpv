//! The RD control path (protocol.html §5): the privileged commands the Race Director
//! issues, and the acknowledgements they get back.
//!
//! Control is **authenticated and Director-local** (§5): race control, marshaling, and
//! scheduling require the RD's authenticated role and run only against the Director —
//! the Cloud exposes no control path at all. This module is the *command vocabulary*;
//! the auth that gates it (#44) and the axum endpoints that carry it (#45) are later
//! issues.
//!
//! Every [`Command`] maps to an action the engine/marshaling layer already models — a
//! heat-loop transition ([`HeatTransition`](gridfpv_events::HeatTransition)), a
//! schedule ([`Event::HeatScheduled`](gridfpv_events::Event::HeatScheduled)), a
//! registration binding, or one of the five marshaling adjudications. A command is a
//! *request* to append the corresponding event(s); the engine validates legality
//! against current state and answers with a [`CommandAck`].
//!
//! > **Note (scope/addressing deferred):** commands address heats and competitors by
//! > the ids the event model already uses ([`HeatId`](gridfpv_events::HeatId),
//! > [`CompetitorRef`](gridfpv_events::CompetitorRef),
//! > [`LogRef`](gridfpv_events::LogRef)). The richer scope grammar and any
//! > event/class addressing on commands (protocol.html §9.6) are refined alongside the
//! > control endpoints (#45) and the doc-reconciliation pass.

use gridfpv_events::{
    AdapterId, ClassId, CompetitorRef, HeatId, LogRef, Penalty, RoundId, SourceTime,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ProtocolError;

/// A privileged RD control command (protocol.html §5). Externally tagged like the
/// event model, so it maps to a TS discriminated union.
///
/// The variants fall into four groups:
///
/// - **Heat-loop transitions** — [`Stage`](Command::Stage), [`Arm`](Command::Arm),
///   [`Start`](Command::Start), [`Finish`](Command::Finish), [`Score`](Command::Score),
///   [`Advance`](Command::Advance), and the off-ramps [`Abort`](Command::Abort),
///   [`Restart`](Command::Restart), [`Discard`](Command::Discard). Each requests the
///   matching [`HeatTransition`](gridfpv_events::HeatTransition); the engine validates
///   it against the heat's current state (race-engine.html §2).
/// - **Scheduling** — [`ScheduleHeat`](Command::ScheduleHeat) creates a heat with its
///   lineup ([`Event::HeatScheduled`](gridfpv_events::Event::HeatScheduled)).
/// - **Registration** — [`Register`](Command::Register) binds a source-local
///   competitor to a pilot (the binding the adapter never does itself; Architecture §9).
/// - **Marshaling adjudications** — the five corrections
///   ([`VoidDetection`](Command::VoidDetection), [`InsertLap`](Command::InsertLap),
///   [`AdjustLap`](Command::AdjustLap), [`VoidHeat`](Command::VoidHeat),
///   [`ApplyPenalty`](Command::ApplyPenalty)), each requesting the corresponding
///   marshaling event the projection folds in (never a mutation; architecture.html §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Command {
    // --- Heat-loop transitions (race-engine.html §2) ---
    /// Begin staging the heat — countdown starts, frequencies are assigned.
    Stage {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Arm the heat — open the gate to detections.
    Arm {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Start the heat running — passes are consumed from here.
    Start {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Finish the heat — close the race (time elapsed or all landed).
    Finish {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Score the heat — finalize the result.
    Score {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Advance the scored heat — hand its result to the format generator.
    Advance {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Abort the heat — abandon it before scoring (an off-ramp).
    Abort {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Restart a running heat — back to staging for a re-run.
    Restart {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Discard a scored heat — drop its result for a re-run.
    Discard {
        /// The heat to transition.
        heat: HeatId,
    },

    // --- Scheduling ---
    /// Create a heat with its lineup (`Event::HeatScheduled`). Additively carries the
    /// class/round the heat runs in and the per-pilot frequency assignment; all three
    /// are optional and default-absent, so the free-text NewHeat path (which assigns
    /// none of them) is unchanged on the wire.
    ScheduleHeat {
        /// The id the new heat will carry.
        heat: HeatId,
        /// The competitors in the heat, in lineup order.
        lineup: Vec<CompetitorRef>,
        /// The class this heat runs in, when the scheduler assigns one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        class: Option<ClassId>,
        /// The round within the class's schedule, when the scheduler assigns one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        round: Option<RoundId>,
        /// Per-pilot frequency assignment in raw MHz (e.g. `5800`); empty when none is
        /// assigned (a sim race, or the free-text path).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        frequencies: Vec<(CompetitorRef, u16)>,
    },

    /// **Fill a round** (race redesign Slice 3a) — the round-driven engine's one command.
    /// Build the round's format generator from the eligible classes' membership (the field)
    /// and the round's completed heats read back from the log, then schedule the **next**
    /// heat the generator emits — a [`Event::HeatScheduled`] tagged with `round` (and
    /// `class` when the round is single-class), lineup from the generator's plan.
    ///
    /// The advance closes through the log: once the scheduled heat is driven to `Score`, the
    /// next `FillRound` rebuilds the generator, sees the new completed heat, and emits the
    /// following one — including the qualifying→bracket carry when the round seeds
    /// [`FromRanking`](crate::events::SeedingRule::FromRanking). A round whose generator has
    /// no more heats is **complete** — a successful ack, not an error.
    FillRound {
        /// The round to fill — one of the event's [`rounds`](crate::events::EventMeta::rounds).
        round: RoundId,
    },

    // --- Registration ---
    /// Bind a source-local competitor to a GridFPV pilot (Architecture §9) — the
    /// registration the adapter never performs. The pilot handle is the event-scoped
    /// [`PilotId`](crate::scope::PilotId).
    Register {
        /// The timing source the competitor belongs to.
        adapter: AdapterId,
        /// The source-local competitor handle being bound.
        competitor: CompetitorRef,
        /// The event-scoped pilot the competitor is bound to.
        pilot: crate::scope::PilotId,
    },

    // --- Marshaling adjudications (architecture.html §3, the five corrections) ---
    /// Void a previously-detected pass, referenced by its log offset
    /// (`Event::DetectionVoided`).
    VoidDetection {
        /// The log offset of the pass (or ruling) to void.
        target: LogRef,
    },
    /// Insert a lap-gate pass the timer missed (`Event::LapInserted`).
    InsertLap {
        /// The timing source to attribute the inserted pass to.
        adapter: AdapterId,
        /// The competitor the inserted lap belongs to.
        competitor: CompetitorRef,
        /// When the inserted crossing happened, on the source clock.
        at: SourceTime,
    },
    /// Re-time a previously-detected pass (`Event::LapAdjusted`).
    AdjustLap {
        /// The log offset of the pass to re-time.
        target: LogRef,
        /// The corrected crossing time, on the source clock.
        at: SourceTime,
    },
    /// Void an entire heat (`Event::HeatVoided`).
    VoidHeat {
        /// The heat to void.
        heat: HeatId,
    },
    /// Apply a penalty to a competitor in a heat (`Event::PenaltyApplied`).
    ApplyPenalty {
        /// The heat the penalty applies in.
        heat: HeatId,
        /// The competitor penalized.
        competitor: CompetitorRef,
        /// The penalty applied.
        penalty: Penalty,
    },
}

/// The acknowledgement of a [`Command`] (protocol.html §5): commands up,
/// acknowledgements down.
///
/// `ok` is the success flag; on failure `error` carries the single shared
/// [`ProtocolError`](crate::error::ProtocolError) (§9.8) — an illegal transition for
/// the heat's state, an unauthorized caller, an unknown heat. On success `error` is
/// `None`. (The resulting projection state flows back separately as
/// [`ChangeEnvelope`](crate::stream::ChangeEnvelope)s on the read stream, not in the
/// ack.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CommandAck {
    /// Whether the command was accepted and applied.
    pub ok: bool,
    /// The failure detail when `ok` is `false`; `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<ProtocolError>,
}

impl CommandAck {
    /// A successful acknowledgement.
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
        }
    }

    /// A failed acknowledgement carrying the error detail.
    pub fn failed(error: ProtocolError) -> Self {
        Self {
            ok: false,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::scope::PilotId;

    #[test]
    fn heat_transition_commands_round_trip() {
        let commands = vec![
            Command::Stage {
                heat: HeatId("q-1".into()),
            },
            Command::Arm {
                heat: HeatId("q-1".into()),
            },
            Command::Start {
                heat: HeatId("q-1".into()),
            },
            Command::Finish {
                heat: HeatId("q-1".into()),
            },
            Command::Score {
                heat: HeatId("q-1".into()),
            },
            Command::Advance {
                heat: HeatId("q-1".into()),
            },
            Command::Abort {
                heat: HeatId("q-1".into()),
            },
            Command::Restart {
                heat: HeatId("q-1".into()),
            },
            Command::Discard {
                heat: HeatId("q-1".into()),
            },
        ];
        for command in commands {
            let json = serde_json::to_string(&command).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(command, back);
        }
    }

    #[test]
    fn scheduling_and_registration_round_trip() {
        let commands = vec![
            Command::ScheduleHeat {
                heat: HeatId("q-1".into()),
                lineup: vec![
                    CompetitorRef("node-0".into()),
                    CompetitorRef("node-1".into()),
                ],
                class: None,
                round: None,
                frequencies: vec![],
            },
            Command::ScheduleHeat {
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
            Command::FillRound {
                round: RoundId("qualifying-r1-abc".into()),
            },
            Command::Register {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: PilotId("acroace".into()),
            },
        ];
        for command in commands {
            let json = serde_json::to_string(&command).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(command, back);
        }
    }

    #[test]
    fn all_five_marshaling_adjudications_round_trip() {
        let commands = vec![
            Command::VoidDetection { target: LogRef(42) },
            Command::InsertLap {
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(5_000_000),
            },
            Command::AdjustLap {
                target: LogRef(43),
                at: SourceTime::from_micros(5_100_000),
            },
            Command::VoidHeat {
                heat: HeatId("q-1".into()),
            },
            Command::ApplyPenalty {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::TimeAdded { micros: 2_000_000 },
            },
        ];
        for command in commands {
            let json = serde_json::to_string(&command).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(command, back);
        }
    }

    #[test]
    fn command_ack_ok_omits_error() {
        let ack = CommandAck::ok();
        let json = serde_json::to_string(&ack).unwrap();
        assert!(!json.contains("error"), "ok ack omits error: {json}");
        let back: CommandAck = serde_json::from_str(&json).unwrap();
        assert_eq!(ack, back);
    }

    #[test]
    fn command_ack_failed_carries_the_error() {
        let ack = CommandAck::failed(ProtocolError::new(
            ErrorCode::BadRequest,
            "cannot Arm a heat that is not Staged",
        ));
        let json = serde_json::to_string(&ack).unwrap();
        let back: CommandAck = serde_json::from_str(&json).unwrap();
        assert_eq!(ack, back);
        assert!(!ack.ok);
    }
}
