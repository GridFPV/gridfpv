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
    AdapterId, ClassId, CompetitorRef, HeatId, LogRef, Penalty, ProtestOutcome, RoundId, SourceTime,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ProtocolError;

/// A privileged RD control command (protocol.html §5). Externally tagged like the
/// event model, so it maps to a TS discriminated union.
///
/// The variants fall into four groups:
///
/// - **Heat-loop transitions** — [`Stage`](Command::Stage), [`Start`](Command::Start),
///   [`Finalize`](Command::Finalize), [`Advance`](Command::Advance), the off-ramps
///   [`Revert`](Command::Revert), [`Abort`](Command::Abort),
///   [`Restart`](Command::Restart), [`Discard`](Command::Discard), and the runtime-clock
///   **overrides** [`SkipCountdown`](Command::SkipCountdown) / [`ForceEnd`](Command::ForceEnd).
///   Each requests the matching [`HeatTransition`](gridfpv_events::HeatTransition); the engine
///   validates it against the heat's current state (race-engine.html §2). The ordinary
///   `Armed → Running` and `Running → Unofficial` transitions are appended by the Director's
///   **runtime clock** (heat-lifecycle Slice 2), not by a command — `SkipCountdown`/`ForceEnd`
///   are the manual overrides for when the clock must be bypassed.
/// - **Scheduling** — [`ScheduleHeat`](Command::ScheduleHeat) creates a heat with its
///   lineup ([`Event::HeatScheduled`](gridfpv_events::Event::HeatScheduled)).
/// - **Registration** — [`Register`](Command::Register) binds a source-local
///   competitor to a pilot (the binding the adapter never does itself; Architecture §9).
/// - **Marshaling adjudications** — the corrections
///   ([`VoidDetection`](Command::VoidDetection), [`InsertLap`](Command::InsertLap),
///   [`AdjustLap`](Command::AdjustLap), [`SplitLap`](Command::SplitLap),
///   [`VoidHeat`](Command::VoidHeat), [`ApplyPenalty`](Command::ApplyPenalty),
///   [`ReverseRuling`](Command::ReverseRuling)), each requesting the corresponding
///   marshaling event the projection/scorer folds in (never a mutation; architecture.html §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Command {
    // --- Heat-loop transitions (race-engine.html §2) ---
    /// Begin staging the heat — countdown starts, frequencies are assigned.
    Stage {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Start the heat — arm it (open the gate to detections) and run the start procedure. The
    /// Director's runtime clock then auto-advances the heat to `Running` after the logged start
    /// delay. (Renamed from the former `Arm` in the heat-lifecycle command collapse, Slice 2.)
    Start {
        /// The heat to transition.
        heat: HeatId,
    },
    /// **Override:** force the heat `Armed → Running` immediately, skipping the start countdown —
    /// the race-day escape hatch when the runtime's auto-start can't be trusted. Records the same
    /// `Running` transition the auto-start would (Slice 2).
    SkipCountdown {
        /// The heat to transition.
        heat: HeatId,
    },
    /// **Override:** force the heat `Running → Unofficial` now — call the race when the runtime's
    /// completion clock must be bypassed. Records the same `Finished` transition the auto-complete
    /// would (Slice 2).
    ForceEnd {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Finalize the heat — lock in the result (Unofficial → Final).
    Finalize {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Advance the finalized heat — hand its result to the format generator.
    Advance {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Revert a finalized heat — re-open its result for correction (Final → Unofficial).
    Revert {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Abort the heat — abandon it before finalizing (an off-ramp).
    Abort {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Restart a committed heat — reset to `Scheduled` for a re-run (the RD re-Stages).
    Restart {
        /// The heat to transition.
        heat: HeatId,
    },
    /// Discard a finalized heat — drop its result for a re-run.
    Discard {
        /// The heat to transition.
        heat: HeatId,
    },

    // --- Live-control selection ---
    /// **Manually select the current heat** in Live control (the RD's explicit "show/control
    /// *this* heat"). Validates the heat exists in the event and appends an
    /// [`Event::CurrentHeatSelected`](gridfpv_events::Event::CurrentHeatSelected) — the live
    /// `current_heat` derivation then follows the RD's choice. Not a heat-loop transition: it
    /// only moves Live control's focus (the sheet/clock/leaderboard + the transition buttons
    /// target the chosen heat); it does not change the heat's state.
    SetCurrentHeat {
        /// The heat to bring into focus — one already scheduled in the event.
        heat: HeatId,
    },

    // --- Scheduling ---
    /// Create a heat with its lineup (`Event::HeatScheduled`). Additively carries the
    /// class/round the heat runs in, the per-pilot frequency assignment, and an optional
    /// human `label`; all are optional and default-absent, so the free-text NewHeat path
    /// (which assigns none of them) is unchanged on the wire.
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
        /// An **optional human label** for a manually-built heat. When set it becomes the
        /// heat's display name everywhere (overriding the derived "‹Round› Heat N" / tier
        /// convention); `None` (the default / the generator path) keeps the auto-name.
        /// Threaded straight into the emitted [`Event::HeatScheduled`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        label: Option<String>,
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
        /// How much of the round to fill in this one command (#216):
        ///
        /// - [`FillMode::Next`] (the default — wire-compatible with the original `{ round }`
        ///   shape) schedules the **single** next heat the generator emits.
        /// - [`FillMode::All`] loops the generator — append a heat, re-fold the round's state,
        ///   draw the next — until the round reports **complete** (no more heats producible now),
        ///   filling a whole deterministic round in one round-trip.
        ///
        /// Either way an already-complete round (or one whose outstanding heat must be scored
        /// first) appends nothing and acks a typed ok — `All` is just `Next` iterated to that
        /// terminal state, so it stays idempotent on re-run.
        #[serde(default)]
        mode: FillMode,
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
        /// The heat the inserted lap belongs to, so the scorer routes it by tag even when a
        /// different heat is live (marshaling a finished heat mid-event). `None` only from a
        /// legacy client — that insertion attributes positionally, the old behavior.
        #[serde(default)]
        #[ts(optional)]
        heat: Option<HeatId>,
    },
    /// Re-time a previously-detected pass (`Event::LapAdjusted`).
    AdjustLap {
        /// The log offset of the pass to re-time.
        target: LogRef,
        /// The corrected crossing time, on the source clock.
        at: SourceTime,
    },
    /// **Split** one over-long lap (the lap *ending* at `target`) into two by inserting a
    /// synthetic mid-lap pass at `at` (`Event::LapSplit`) — the FPVTrackside split action for
    /// a missed mid-lap detection. A distinct command from `InsertLap` so the audit names it.
    SplitLap {
        /// The log offset of the pass that ends the over-long lap to split.
        target: LogRef,
        /// When the inserted mid-lap crossing happened, on the source clock.
        at: SourceTime,
    },
    /// Void an entire heat (`Event::HeatVoided`).
    VoidHeat {
        /// The heat to void.
        heat: HeatId,
    },
    /// Apply a penalty to a competitor in a heat (`Event::PenaltyApplied`). Covers the full
    /// penalty set: a (reversible) DQ with an optional reason, added time, and the standings-only
    /// points adjustments (`PointsDeducted` / `PointsAdded`). `DeductPoints` is sugar over this for
    /// the points case; either path appends the same `PenaltyApplied`.
    ApplyPenalty {
        /// The heat the penalty applies in.
        heat: HeatId,
        /// The competitor penalized.
        competitor: CompetitorRef,
        /// The penalty applied.
        penalty: Penalty,
    },
    /// **Deduct standings points** from a competitor in a heat (marshaling Slice 6) — sugar over
    /// [`ApplyPenalty`](Command::ApplyPenalty) with a
    /// [`Penalty::PointsDeducted`](gridfpv_events::Penalty::PointsDeducted), so the console can offer
    /// a dedicated points control. Points affect the **season / event standings**, not the per-heat
    /// lap result.
    DeductPoints {
        /// The heat the deduction is recorded against.
        heat: HeatId,
        /// The competitor losing points.
        competitor: CompetitorRef,
        /// How many standings points to deduct.
        points: u32,
    },
    /// **Throw out a single valid lap** from a competitor's scored count (`Event::LapThrownOut`),
    /// referenced by the lap's **end-pass** log offset. The lap stays real in the lap list/audit;
    /// it is only excluded from scoring. Distinct from `VoidDetection` (which removes the pass).
    ThrowOutLap {
        /// The log offset of the pass that *ends* the lap to throw out.
        target: LogRef,
    },
    /// **File a protest** against a heat result (`Event::ProtestFiled`) — the append-only filing
    /// half of the protest pair (resolved later by `ResolveProtest`). No actor (no-login; filed at
    /// the RD console on a pilot's behalf).
    FileProtest {
        /// The heat the protest concerns.
        heat: HeatId,
        /// The competitor the protest is about.
        competitor: CompetitorRef,
        /// A free-text note describing the protest.
        note: String,
    },
    /// **Resolve a filed protest** (`Event::ProtestResolved`), referenced by the
    /// `ProtestFiled`'s log offset, recording the `outcome`.
    ResolveProtest {
        /// The log offset of the `ProtestFiled` this resolves.
        target: LogRef,
        /// How the protest was resolved.
        outcome: ProtestOutcome,
    },
    /// **Reverse a prior ruling** (`Event::RulingReversed`), referenced by its log offset.
    /// Generalized (Slice 6) to undo **any** ruling — a [`PenaltyApplied`](gridfpv_events::Event::PenaltyApplied)
    /// (DQ / time / points), a [`LapThrownOut`](gridfpv_events::Event::LapThrownOut), a
    /// [`ProtestResolved`](gridfpv_events::Event::ProtestResolved), or a
    /// [`HeatVoided`](gridfpv_events::Event::HeatVoided). A distinct command from `VoidDetection` so
    /// the audit reads "DQ reversed" / "throw-out reversed".
    ReverseRuling {
        /// The log offset of the ruling to reverse.
        target: LogRef,
    },
}

/// How much of a round a [`Command::FillRound`] fills (#216). Externally tagged like the rest
/// of the control vocabulary, so it maps to a TS string-union (`"Next" | "All"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum FillMode {
    /// Schedule the **single** next heat the round's generator emits — the original
    /// (interactive) fill, and the building block `All` iterates. The default, so an older
    /// `FillRound { round }` payload (no `mode`) still deserializes to it.
    #[default]
    Next,
    /// Fill the **whole** round: loop the generator (append a heat → re-fold the round's
    /// state → draw the next) until it reports complete — a deterministic round drawn in one
    /// command. Idempotent on a round already at its terminal state (appends nothing).
    All,
}

/// Why a [`Command::FillRound`] stopped drawing heats (#395).
///
/// Every value here is a **success**; they differ in what the RD has to do next, which is
/// exactly what `ok: true` alone could not say. Externally tagged, so it maps to a TS
/// string-union (`"SingleStep" | "Complete" | "AwaitingResult" | "Blocked"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum FillStop {
    /// [`FillMode::Next`] drew its **one** heat and stopped there — the mode's contract, not a
    /// limit reached. Always paired with exactly one scheduled heat, and the round's own state
    /// (finished? waiting?) is simply not known: asking would have meant drawing again.
    SingleStep,
    /// The round is **finished**: every heat its format wants exists. Nothing more to do.
    Complete,
    /// The next heat is **already scheduled** and awaiting its result — the RD drives the
    /// outstanding heat before another can be drawn. Not finished; come back after scoring.
    AwaitingResult,
    /// The round's format **refuses this field** and cannot draw a heat for it at all (#394) —
    /// Head-to-Head with a single pilot. Nothing has raced and nothing can until the RD changes
    /// the round: [`detail`](FillRoundOutcome::detail) says what to change.
    Blocked,
}

/// A heat a command **created**, identified for the caller (#395).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ScheduledHeat {
    /// The heat's id — a **wire handle**, for addressing it in follow-up commands. Never shown
    /// to a user; [`name`](Self::name) is what a message prints (repo display rule).
    pub heat: HeatId,
    /// The heat's **friendly display name** ("Test Round Heat 1", "A-Main", "Practice Heat"),
    /// resolved server-side by the same convention the console uses.
    pub name: String,
    /// Who is racing in it, in the generator's seeding order.
    pub lineup: Vec<CompetitorRef>,
    /// The per-pilot channel assignment in raw MHz, as logged on the heat. Empty when the round
    /// assigns none (open practice, or no timer channels to hand out).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frequencies: Vec<(CompetitorRef, u16)>,
}

/// What a [`Command::FillRound`] actually **did** (#395).
///
/// The scheduling commands' useful answer is their *effect*, not their acceptance: a fill that
/// appended nothing — round finished, outstanding heat unscored, field too small for the format
/// — was reported byte-identically to one that scheduled a heat. That is not merely unhelpful,
/// it actively misleads: `ok: true` reads as "the thing you asked for happened", so a caller
/// debugging an empty log searches downstream through the projection and the read routes, all of
/// which are working correctly. Both halves are here, so nobody has to diff the event log to
/// find out which happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct FillRoundOutcome {
    /// The heats this fill scheduled, in append order — one for
    /// [`FillMode::Next`](FillMode::Next), zero or more for [`All`](FillMode::All). **Empty
    /// means the fill appended nothing**, and [`stopped`](Self::stopped) says why.
    ///
    /// Always serialized, empty included: "this fill scheduled nothing" is the very signal this
    /// type exists to carry, and omitting the field would leave a caller inferring it from an
    /// absence again — the exact shape of the bug (#395).
    #[serde(default)]
    pub scheduled: Vec<ScheduledHeat>,
    /// Why the fill stopped drawing — the machine-readable discriminator.
    pub stopped: FillStop,
    /// The same thing in one RD-facing sentence, naming the round by its **label** and any heat
    /// by its friendly name (never an id). Safe to show verbatim.
    pub detail: String,
}

/// What a command **did**, for the commands whose useful result is the effect rather than the
/// acceptance (#395).
///
/// Externally tagged and keyed by command, so each command's outcome is its own shape and a new
/// one is an additive variant. Rides along in [`CommandAck::outcome`], which is optional — a
/// command with nothing interesting to report (a transition either happened or was rejected)
/// omits it entirely and its ack is byte-identical to before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum CommandOutcome {
    /// The result of a [`Command::FillRound`].
    FillRound(FillRoundOutcome),
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
///
/// `ok` answers *"was the command accepted?"* — which for some commands is not the question the
/// caller is actually asking. `FillRound`'s "did nothing" is a routine, expected result, not an
/// error, so it acks ok; a caller then has no way to tell it from a fill that scheduled a heat
/// (#395). [`outcome`](Self::outcome) carries **what the command did** for those commands, and
/// is absent for the ones where acceptance *is* the whole answer — so it is purely additive:
/// every existing client keeps parsing every ack unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CommandAck {
    /// Whether the command was accepted and applied.
    pub ok: bool,
    /// The failure detail when `ok` is `false`; `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<ProtocolError>,
    /// **What the command did**, for the commands whose effect is the useful answer — currently
    /// [`FillRound`](Command::FillRound). `None` for every other command (and for a failure,
    /// where `error` is the answer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome: Option<CommandOutcome>,
}

impl CommandAck {
    /// A successful acknowledgement, reporting acceptance only.
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            outcome: None,
        }
    }

    /// A successful acknowledgement that also reports **what the command did**.
    pub fn ok_with(outcome: CommandOutcome) -> Self {
        Self {
            ok: true,
            error: None,
            outcome: Some(outcome),
        }
    }

    /// A failed acknowledgement carrying the error detail.
    pub fn failed(error: ProtocolError) -> Self {
        Self {
            ok: false,
            error: Some(error),
            outcome: None,
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
            Command::Start {
                heat: HeatId("q-1".into()),
            },
            Command::SkipCountdown {
                heat: HeatId("q-1".into()),
            },
            Command::ForceEnd {
                heat: HeatId("q-1".into()),
            },
            Command::Finalize {
                heat: HeatId("q-1".into()),
            },
            Command::Advance {
                heat: HeatId("q-1".into()),
            },
            Command::Revert {
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
            Command::SetCurrentHeat {
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
                label: None,
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
                label: Some("Featured Heat".into()),
            },
            Command::FillRound {
                round: RoundId("qualifying-r1-abc".into()),
                mode: FillMode::Next,
            },
            Command::FillRound {
                round: RoundId("qualifying-r1-abc".into()),
                mode: FillMode::All,
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
    fn all_marshaling_adjudications_round_trip() {
        let commands = vec![
            Command::VoidDetection { target: LogRef(42) },
            Command::InsertLap {
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(5_000_000),
                heat: Some(HeatId("main-a".into())),
            },
            Command::AdjustLap {
                target: LogRef(43),
                at: SourceTime::from_micros(5_100_000),
            },
            Command::SplitLap {
                target: LogRef(44),
                at: SourceTime::from_micros(5_050_000),
            },
            Command::VoidHeat {
                heat: HeatId("q-1".into()),
            },
            Command::ApplyPenalty {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::TimeAdded { micros: 2_000_000 },
            },
            Command::ApplyPenalty {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::Disqualify {
                    reason: Some("unsafe".into()),
                },
            },
            Command::DeductPoints {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("A".into()),
                points: 5,
            },
            Command::ThrowOutLap { target: LogRef(46) },
            Command::FileProtest {
                heat: HeatId("main-a".into()),
                competitor: CompetitorRef("B".into()),
                note: "contact on lap 2".into(),
            },
            Command::ResolveProtest {
                target: LogRef(47),
                outcome: ProtestOutcome::Denied,
            },
            Command::ReverseRuling { target: LogRef(45) },
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
