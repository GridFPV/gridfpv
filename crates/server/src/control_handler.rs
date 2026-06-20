//! The RD control **write path** (protocol.html §5) — issue #45.
//!
//! [`control`](crate::control) defines the command *vocabulary*; this module is the
//! handler that turns a [`Command`] into validated, appended [`Event`]s and the axum
//! routes that carry it. Control is the one **bidirectional** protocol surface (§5):
//! commands up, [`CommandAck`]s down, on a distinct privileged endpoint — while the
//! *resulting state* flows back down the ordinary read stream (#43), because a command's
//! whole job is **validate → append → ack**. The append goes through the very same
//! [`AppState::append`] the change stream observes, so the moment a command is accepted
//! every subscribed `/stream` re-folds and pushes the new value (see "the resulting state
//! reaches the stream" below).
//!
//! # Command → Event mapping
//!
//! | command group | validation | appended event |
//! |---------------|------------|----------------|
//! | heat-loop (`Stage`/`Arm`/`Start`/`Finish`/`Score`/`Advance`/`Abort`/`Restart`/`Discard`) | [`heat::heat_state`] folds the heat's current state; [`heat::apply`] checks the transition is legal | [`Event::HeatStateChanged`] with the engine-returned [`HeatTransition`](gridfpv_events::HeatTransition) |
//! | [`Command::ScheduleHeat`] | none (it creates the heat) | [`Event::HeatScheduled`] |
//! | [`Command::Register`] | — | **deferred** — see below |
//! | [`Command::VoidDetection`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) | [`Event::DetectionVoided`] |
//! | [`Command::AdjustLap`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) | [`Event::LapAdjusted`] |
//! | [`Command::InsertLap`] | none (it adds a pass) | [`Event::LapInserted`] |
//! | [`Command::VoidHeat`] | the heat exists in the log | [`Event::HeatVoided`] |
//! | [`Command::ApplyPenalty`] | the heat exists in the log | [`Event::PenaltyApplied`] |
//!
//! ## Legality lives in the engine (reused, not re-implemented)
//!
//! A heat-loop command does **not** re-derive the FSM here: it folds the heat's current
//! [`HeatState`](gridfpv_engine::heat::HeatState) with [`heat::heat_state`] over the log,
//! then calls [`heat::apply`] — the single source of FSM legality (race-engine.html §2).
//! A legal command yields the [`HeatTransition`](gridfpv_events::HeatTransition) to record;
//! an [`IllegalTransition`](gridfpv_engine::heat::IllegalTransition) maps to a
//! [`ProtocolError`] of [`ErrorCode::BadRequest`]. A command on a heat that was never
//! scheduled (no `HeatScheduled` in the log, so `heat_state` is `None`) is rejected with
//! [`ErrorCode::UnknownScope`] — nothing is appended.
//!
//! ## Register is deferred (a model gap, not a protocol gap)
//!
//! The event log (`gridfpv-events`) carries **no pilot-binding event** — there is a
//! [`CompetitorSeen`](gridfpv_events::Event::CompetitorSeen) *adapter observation*, but no
//! event that records "this source competitor *is* this event-scoped pilot" (Architecture
//! §9; the same gap the snapshot path notes for pilot scope). Rather than append a
//! lossy stand-in, [`Command::Register`] is acknowledged as **not yet modelled** with a
//! [`ProtocolError`] of [`ErrorCode::BadRequest`] that names the deferral, and **nothing
//! is appended**. When the registration event lands in the log model this becomes a
//! one-line append like the others; the command vocabulary and endpoint already carry it.
//!
//! # Endpoints — the privileged control channel (protocol.html §5)
//!
//! Both shapes drive the *same* [`apply_command`] handler:
//!
//! - **`GET /control`** — the bidirectional control WebSocket §5 calls for ("another
//!   reason the RD wants WebSocket"): the RD sends a stream of JSON [`Command`] frames and
//!   receives a JSON [`CommandAck`] per command on the same socket. This is the primary
//!   surface.
//! - **`POST /control`** — a one-shot `Command` → `CommandAck` for a simple request/reply
//!   caller (a script, a test) that does not want a long-lived socket.
//!
//! # Auth hook for #44
//!
//! Control is authenticated and Director-local (§5); **this issue does not implement
//! auth**. The seam is the [`ControlAuth`] marker extractor: every control route lists it
//! as its first extractor, so #44 replaces its permissive stub with a real RD-role check
//! (a token/role extractor that rejects an unprivileged caller with
//! [`ErrorCode::Unauthorized`]) **without touching the handlers** — the routes already
//! demand it, the wiring already threads it. Until then it admits every caller (the read
//! paths are equally unauthenticated pre-#44).
//!
//! # How the resulting state reaches the stream (protocol.html §3, §5)
//!
//! The ack carries only success/failure (the [`CommandAck`] shape, §5): the *state* a
//! command produces is **not** echoed in the ack. Instead [`apply_command`] appends through
//! [`AppState::append`], which appends to the one log **and** `notify_waiters()` wakes every
//! parked change stream (#43). A subscriber to the affected scope therefore re-folds the new
//! log tail and pushes the resulting [`ChangeEnvelope`](crate::stream::ChangeEnvelope) — the
//! RD sees the consequence of its own command arrive on the read stream it already holds, in
//! the same total order as every other client (§3). The control test below asserts exactly
//! this: after a control append, a `/stream` subscriber observes the change.

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::get;
use axum::{Router, routing::MethodRouter};
use gridfpv_engine::heat::{self, HeatCommand};
use gridfpv_events::{Event, HeatId, LogRef};

use crate::app::AppState;
use crate::control::{Command, CommandAck};
use crate::error::{ErrorCode, ProtocolError};

/// The **auth seam** for the privileged control path (protocol.html §5), left for #44.
///
/// An axum extractor every control route demands *before* its handler runs. Today its
/// extraction is infallible — it admits every caller, exactly as the read paths do
/// pre-#44 — so the control path is reachable for this issue's write-path work without
/// auth being implemented here.
///
/// #44 makes this the single chokepoint: it replaces the permissive
/// [`from_request_parts`](FromRequestParts::from_request_parts) below with a real RD-role
/// check (read the bearer token / session, verify the control role, reject an unprivileged
/// caller with [`ErrorCode::Unauthorized`]). Because every control route already lists
/// `ControlAuth` as its first extractor, that change gates `POST /control` and the
/// `GET /control` upgrade at once **without editing a single handler body**.
#[derive(Debug, Clone, Copy)]
pub struct ControlAuth {
    // A private field so `ControlAuth` can only be minted by the extractor (the auth
    // chokepoint), never constructed ad hoc by a handler that wants to skip the check.
    _private: (),
}

impl<S> FromRequestParts<S> for ControlAuth
where
    S: Send + Sync,
{
    type Rejection = ProtocolError;

    async fn from_request_parts(_parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // #44: read the RD credential from `_parts` (Authorization header / session) and
        // return `Err(ProtocolError::new(ErrorCode::Unauthorized, …))` for an unprivileged
        // caller. For now control is open, matching the unauthenticated read paths.
        Ok(ControlAuth { _private: () })
    }
}

/// Mount the privileged control routes (protocol.html §5) onto an existing [`Router`].
///
/// Adds `GET /control` (the bidirectional control WebSocket) and `POST /control` (the
/// one-shot request/reply). Kept separate from [`crate::app::router`] so the control
/// surface is composed explicitly and #44 can wrap *just* these routes in its auth layer.
pub fn control_routes(router: Router<AppState>) -> Router<AppState> {
    router.route("/control", control_method_router())
}

/// `GET /control` (WS upgrade) + `POST /control` (one-shot) on the one path.
fn control_method_router() -> MethodRouter<AppState> {
    get(control_ws).post(control_post)
}

/// `POST /control` — a single [`Command`] in the body, one [`CommandAck`] back
/// (protocol.html §5). The simple request/reply control surface.
///
/// [`ControlAuth`] runs first (the #44 seam); on success the command is dispatched through
/// [`apply_command`] against the shared log. The ack is always `200 OK` with the
/// `ok`/`error` body — a *rejected* command (illegal transition, unknown heat) is a
/// well-formed `CommandAck { ok: false, .. }`, not an HTTP error, so a client reads one
/// uniform shape (the transport-level errors — a poisoned lock — still surface as the
/// shared [`ProtocolError`]).
async fn control_post(
    _auth: ControlAuth,
    State(state): State<AppState>,
    Json(command): Json<Command>,
) -> Json<CommandAck> {
    Json(apply_command(&state, command))
}

/// `GET /control` — upgrade to the bidirectional control WebSocket (protocol.html §5).
///
/// [`ControlAuth`] gates the upgrade (the #44 seam); the upgraded socket is driven by
/// [`run_control`].
async fn control_ws(
    _auth: ControlAuth,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| run_control(socket, state))
}

/// Drive one control socket: read [`Command`] frames, write a [`CommandAck`] per command
/// (protocol.html §5).
///
/// The RD sends a stream of JSON command frames; for each, [`apply_command`] validates and
/// (on success) appends, and the ack goes straight back on the same socket. A malformed
/// frame is answered with a `CommandAck::failed(BadRequest)` rather than closing the
/// socket, so one bad command does not drop the RD's control session. The loop ends when
/// the client closes or the socket errors.
async fn run_control(mut socket: WebSocket, state: AppState) {
    while let Some(frame) = socket.recv().await {
        let ack = match frame {
            Ok(Message::Text(text)) => match serde_json::from_str::<Command>(&text) {
                Ok(command) => apply_command(&state, command),
                Err(e) => CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("malformed command: {e}"),
                )),
            },
            Ok(Message::Binary(bytes)) => match serde_json::from_slice::<Command>(&bytes) {
                Ok(command) => apply_command(&state, command),
                Err(e) => CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("malformed command: {e}"),
                )),
            },
            // Ping/Pong are handled by axum; a Close (or a transport error) ends the session.
            Ok(Message::Close(_)) | Err(_) => return,
            Ok(_) => continue,
        };
        let json = match serde_json::to_string(&ack) {
            Ok(json) => json,
            Err(_) => return,
        };
        if socket.send(Message::Text(json.into())).await.is_err() {
            return; // client gone
        }
    }
}

/// Validate a [`Command`] against the current log and, on success, append the event(s) it
/// records (protocol.html §5) — the one control write path, shared by both endpoints.
///
/// Reads the log once to fold current state for validation, dispatches per the
/// command→event table (see the module docs), and appends through [`AppState::append`]
/// (which wakes the change streams). Returns [`CommandAck::ok`] on a successful append, or
/// [`CommandAck::failed`] carrying the shared [`ProtocolError`] — and appends **nothing**
/// — on any rejection.
pub fn apply_command(state: &AppState, command: Command) -> CommandAck {
    match command_to_event(state, command) {
        Ok(event) => match state.append(event, None) {
            Ok(_offset) => CommandAck::ok(),
            Err(err) => CommandAck::failed(err),
        },
        Err(err) => CommandAck::failed(err),
    }
}

/// Validate `command` against the current log and produce the [`Event`] to append, or the
/// [`ProtocolError`] explaining the rejection. Pure with respect to the log: it reads but
/// never writes — the append is [`apply_command`]'s job — so a rejected command leaves the
/// log untouched.
fn command_to_event(state: &AppState, command: Command) -> Result<Event, ProtocolError> {
    match command {
        // --- Heat-loop transitions: fold current state, reuse the engine's legality. ---
        Command::Stage { heat } => heat_transition(state, heat, HeatCommand::Stage),
        Command::Arm { heat } => heat_transition(state, heat, HeatCommand::Arm),
        Command::Start { heat } => heat_transition(state, heat, HeatCommand::Start),
        Command::Finish { heat } => heat_transition(state, heat, HeatCommand::Finish),
        Command::Score { heat } => heat_transition(state, heat, HeatCommand::Score),
        Command::Advance { heat } => heat_transition(state, heat, HeatCommand::Advance),
        Command::Abort { heat } => heat_transition(state, heat, HeatCommand::Abort),
        Command::Restart { heat } => heat_transition(state, heat, HeatCommand::Restart),
        Command::Discard { heat } => heat_transition(state, heat, HeatCommand::Discard),

        // --- Scheduling: creates the heat, so no prior-state check. ---
        Command::ScheduleHeat { heat, lineup } => Ok(Event::HeatScheduled { heat, lineup }),

        // --- Registration: no binding event in the log model yet (see module docs). ---
        Command::Register { .. } => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "registration binding is not yet modelled in the event log \
             (Architecture §9; deferred — no pilot-binding event exists to append)",
        )),

        // --- Marshaling adjudications: validate targets where cheap, then append. ---
        Command::VoidDetection { target } => {
            require_pass_target(state, target)?;
            Ok(Event::DetectionVoided { target })
        }
        Command::AdjustLap { target, at } => {
            require_pass_target(state, target)?;
            Ok(Event::LapAdjusted { target, at })
        }
        Command::InsertLap {
            adapter,
            competitor,
            at,
        } => Ok(Event::LapInserted {
            adapter,
            competitor,
            at,
        }),
        Command::VoidHeat { heat } => {
            require_scheduled_heat(state, &heat)?;
            Ok(Event::HeatVoided { heat })
        }
        Command::ApplyPenalty {
            heat,
            competitor,
            penalty,
        } => {
            require_scheduled_heat(state, &heat)?;
            Ok(Event::PenaltyApplied {
                heat,
                competitor,
                penalty,
            })
        }
    }
}

/// Fold the heat's current state from the log, validate `command` against it with the
/// engine's [`heat::apply`], and return the [`Event::HeatStateChanged`] it records.
///
/// - The heat must have been scheduled (`heat_state` is `Some`), else
///   [`ErrorCode::UnknownScope`].
/// - The transition must be legal in the current state, else [`ErrorCode::BadRequest`]
///   (the [`IllegalTransition`](gridfpv_engine::heat::IllegalTransition) message).
fn heat_transition(
    state: &AppState,
    heat: HeatId,
    command: HeatCommand,
) -> Result<Event, ProtocolError> {
    let (events, _cursor) = state.read()?;
    let current = heat::heat_state(&events, &heat).ok_or_else(|| {
        ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no heat scheduled with id {:?}", heat.0),
        )
    })?;
    let transition = heat::apply(current, command)
        .map_err(|illegal| ProtocolError::new(ErrorCode::BadRequest, illegal.to_string()))?;
    Ok(Event::HeatStateChanged { heat, transition })
}

/// Require that `heat` was scheduled in the log (a `HeatScheduled` for it), else
/// [`ErrorCode::UnknownScope`]. The cheap existence check the marshaling heat commands run.
fn require_scheduled_heat(state: &AppState, heat: &HeatId) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let scheduled = events
        .iter()
        .any(|e| matches!(e, Event::HeatScheduled { heat: h, .. } if h == heat));
    if scheduled {
        Ok(())
    } else {
        Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no heat scheduled with id {:?}", heat.0),
        ))
    }
}

/// Require that `target` names a real [`Pass`](gridfpv_events::Pass) in the log — the cheap
/// target check for the offset-addressed marshaling commands (`VoidDetection`,
/// `AdjustLap`). An out-of-range or non-pass offset is [`ErrorCode::BadRequest`]; nothing
/// is appended.
fn require_pass_target(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match events.get(target.0 as usize) {
        Some(Event::Pass(_)) => Ok(()),
        Some(_) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("log offset {} is not a detected pass", target.0),
        )),
        None => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("log offset {} is out of range", target.0),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{
        AdapterId, CompetitorRef, GateIndex, HeatTransition, Pass, Penalty, SourceTime,
    };
    use gridfpv_storage::{EventLog, InMemoryLog};

    fn heat() -> HeatId {
        HeatId("q-1".into())
    }

    /// A state whose log already has `q-1` scheduled.
    fn scheduled_state() -> AppState {
        let mut log = InMemoryLog::default();
        EventLog::append(
            &mut log,
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
            },
            None,
        )
        .unwrap();
        AppState::new(log)
    }

    fn pass(competitor: &str, at: i64, seq: u64) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef(competitor.into()),
            at: SourceTime::from_micros(at),
            sequence: Some(seq),
            gate: GateIndex::LAP,
            signal: None,
        })
    }

    /// (a) A legal Stage→Arm→Start→Finish→Score sequence acks ok and appends the matching
    /// `HeatStateChanged` events in order.
    #[test]
    fn legal_heat_loop_sequence_appends_transitions_and_acks_ok() {
        let state = scheduled_state();
        let steps = [
            (Command::Stage { heat: heat() }, HeatTransition::Staged),
            (Command::Arm { heat: heat() }, HeatTransition::Armed),
            (Command::Start { heat: heat() }, HeatTransition::Running),
            (Command::Finish { heat: heat() }, HeatTransition::Finished),
            (Command::Score { heat: heat() }, HeatTransition::Scored),
        ];
        for (command, _expected) in steps.iter().cloned() {
            let ack = apply_command(&state, command);
            assert!(ack.ok, "expected ok ack, got {ack:?}");
            assert!(ack.error.is_none());
        }

        // The log now holds the scheduling plus one HeatStateChanged per step, in order.
        let (events, _) = state.read().unwrap();
        let transitions: Vec<HeatTransition> = events
            .iter()
            .filter_map(|e| match e {
                Event::HeatStateChanged { transition, .. } => Some(*transition),
                _ => None,
            })
            .collect();
        assert_eq!(
            transitions,
            steps.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
        );
    }

    /// (b) An illegal transition (Start before Arm) is rejected with the shared error shape
    /// and appends nothing.
    #[test]
    fn illegal_transition_is_rejected_and_appends_nothing() {
        let state = scheduled_state();
        let (before, _) = state.read().unwrap();

        let ack = apply_command(&state, Command::Start { heat: heat() });
        assert!(!ack.ok);
        let err = ack.error.expect("a failed ack carries the error");
        assert_eq!(err.code, ErrorCode::BadRequest);

        // Nothing was appended — the log is unchanged.
        let (after, _) = state.read().unwrap();
        assert_eq!(
            before.len(),
            after.len(),
            "illegal command appended nothing"
        );
    }

    /// A command on a heat that was never scheduled is an UnknownScope rejection.
    #[test]
    fn command_on_unknown_heat_is_rejected() {
        let state = scheduled_state();
        let ack = apply_command(
            &state,
            Command::Stage {
                heat: HeatId("does-not-exist".into()),
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::UnknownScope);
    }

    /// `ScheduleHeat` creates the heat with its lineup.
    #[test]
    fn schedule_heat_appends_heat_scheduled() {
        let state = AppState::new(InMemoryLog::default());
        let lineup = vec![CompetitorRef("A".into()), CompetitorRef("B".into())];
        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: lineup.clone(),
            },
        );
        assert!(ack.ok);
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::HeatScheduled { heat: h, lineup: l } if *h == heat() && *l == lineup
        )));
    }

    /// (c) A marshaling command appends the right adjudication event.
    #[test]
    fn apply_penalty_appends_penalty_event() {
        let state = scheduled_state();
        let penalty = Penalty::TimeAdded { micros: 2_000_000 };
        let ack = apply_command(
            &state,
            Command::ApplyPenalty {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                penalty,
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::PenaltyApplied { heat: h, competitor: c, penalty: p }
                if *h == heat() && *c == CompetitorRef("A".into()) && *p == penalty
        )));
    }

    /// `VoidDetection` validates the target is a real pass, then appends the adjudication.
    #[test]
    fn void_detection_validates_target_and_appends() {
        let mut log = InMemoryLog::default();
        // offset 0: a pass; offset 1: a non-pass.
        EventLog::append(&mut log, pass("A", 1_000_000, 1), None).unwrap();
        EventLog::append(
            &mut log,
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![],
            },
            None,
        )
        .unwrap();
        let state = AppState::new(log);

        // Voiding the pass at offset 0 succeeds and appends.
        let ack = apply_command(&state, Command::VoidDetection { target: LogRef(0) });
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::DetectionVoided { target } if *target == LogRef(0)))
        );

        // A non-pass target is rejected and appends nothing.
        let (before, _) = state.read().unwrap();
        let ack = apply_command(&state, Command::VoidDetection { target: LogRef(1) });
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len());

        // An out-of-range target is rejected too.
        let ack = apply_command(
            &state,
            Command::VoidDetection {
                target: LogRef(999),
            },
        );
        assert!(!ack.ok);
    }

    /// `Register` is acknowledged as not-yet-modelled and appends nothing (the model gap).
    #[test]
    fn register_is_deferred_and_appends_nothing() {
        let state = scheduled_state();
        let (before, _) = state.read().unwrap();
        let ack = apply_command(
            &state,
            Command::Register {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: crate::scope::PilotId("acroace".into()),
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len());
    }
}
