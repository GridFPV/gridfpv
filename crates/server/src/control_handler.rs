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
//! | heat-loop (`Stage`/`Arm`/`Start`/`Finish`/`Finalize`/`Advance`/`Revert`/`Abort`/`Restart`/`Discard`) | [`heat::heat_state`] folds the heat's current state; [`heat::apply`] checks the transition is legal | [`Event::HeatStateChanged`] with the engine-returned [`HeatTransition`](gridfpv_events::HeatTransition) |
//! | [`Command::ScheduleHeat`] | none (it creates the heat) | [`Event::HeatScheduled`] |
//! | [`Command::Register`] | none (the binding is always recordable; last-registration-wins folds downstream) | [`Event::CompetitorRegistered`] |
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
//! ## Register binds a competitor to a pilot (#60)
//!
//! [`Command::Register`] appends [`Event::CompetitorRegistered`] — the logged binding
//! "this source competitor *is* this event-scoped pilot" (Architecture §9), the action the
//! adapter never performs itself. There is nothing to validate against current state: a
//! binding is always recordable, and a re-bind of the same `(adapter, competitor)` is a
//! fresh append that supersedes the earlier one (last-registration-wins is folded
//! downstream by the registrations projection, not enforced here). The live and lap
//! projections fold these bindings to surface the pilot identity over a bare
//! [`CompetitorRef`](gridfpv_events::CompetitorRef).
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
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, FromRequestParts, Path, Request, State};
use axum::http::request::Parts;
use axum::response::Response;
use axum::routing::get;
use axum::{Router, routing::MethodRouter};
use gridfpv_engine::heat::{self, HeatCommand};
use gridfpv_events::{Event, HeatId, LogRef};

use crate::app::{AppState, resolve_event};
use crate::control::{Command, CommandAck};
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;
use crate::scope::EventId;

/// A JSON body extractor that fails with the shared [`ProtocolError`] shape instead of axum's
/// bare-text rejection — the papercut fix for `POST /events/{id}/control`.
///
/// Wrapping [`axum::Json`] inherits its parsing, but its default [`JsonRejection`] renders as a
/// plain-text 4xx (e.g. `Expected request with Content-Type: application/json`) that a client
/// cannot parse as the uniform [`ProtocolError`] every other API surface returns. This newtype
/// maps **every** rejection cause — a missing/wrong `Content-Type`, malformed JSON, a schema
/// mismatch, an oversized/unreadable body — to a typed [`ProtocolError`] of
/// [`ErrorCode::BadRequest`] (HTTP 400) carrying the cause as its message, so the control endpoint
/// answers the same JSON error shape as the rest of the API. Generic over the body type so any
/// JSON handler can opt in.
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
{
    type Rejection = ProtocolError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(JsonBody(value)),
            // Every `JsonRejection` cause (missing/wrong Content-Type, malformed/unreadable body,
            // schema mismatch) is a malformed request → a typed `BadRequest` JSON error, not a
            // bare-text 4xx. The rejection's own message is the human-readable detail.
            Err(rejection) => Err(ProtocolError::new(
                ErrorCode::BadRequest,
                rejection.body_text(),
            )),
        }
    }
}

/// The **auth chokepoint** for the privileged control path (protocol.html §5, §9.4) — #44.
///
/// An axum extractor every control route demands *before* its handler runs: it reads the
/// caller's `Authorization: Bearer <token>` and requires a token resolving to a
/// **control-authorized** ([`Role::Rd`](crate::auth::Role::Rd)) session in the shared
/// [`TokenStore`](crate::auth::TokenStore). A valid RD token yields the marker; anything
/// else — no header, a read-only/join token, an unknown or revoked token — is rejected
/// with [`ErrorCode::Unauthorized`] (HTTP 401 / a failed ack). The whole policy lives in
/// [`TokenStore::authenticate_control`](crate::auth::TokenStore::authenticate_control);
/// this extractor is just where it is applied.
///
/// Because every control route lists `ControlAuth` as its first extractor, this gates
/// `POST /control` and the `GET /control` upgrade at once **without touching a single
/// handler body** — the seam #45 left. Reads, by contrast, stay open on the LAN (§5; see
/// [`crate::auth`]).
#[derive(Debug, Clone, Copy)]
pub struct ControlAuth {
    // A private field so `ControlAuth` can only be minted by the extractor (the auth
    // chokepoint), never constructed ad hoc by a handler that wants to skip the check.
    _private: (),
}

impl FromRequestParts<EventRegistry> for ControlAuth {
    type Rejection = ProtocolError;

    async fn from_request_parts(
        parts: &mut Parts,
        registry: &EventRegistry,
    ) -> Result<Self, Self::Rejection> {
        // Read the bearer token (if any) and require a control-authorized RD session; every
        // non-RD case maps to `ErrorCode::Unauthorized` inside the store. The token store is
        // Director-wide (shared across events via the registry), so one RD token authorizes
        // control on every event — control is gated by *role*, the event is resolved per
        // handler from the path.
        let token = crate::auth::bearer_token(parts);
        registry.tokens().authenticate_control(token.as_deref())?;
        Ok(ControlAuth { _private: () })
    }
}

/// Mount the privileged control routes (protocol.html §5) onto an existing [`Router`].
///
/// Adds `GET /control` (the bidirectional control WebSocket) and `POST /control` (the
/// one-shot request/reply). Kept separate from [`crate::app::router`] so the control
/// surface is composed explicitly and #44 can wrap *just* these routes in its auth layer.
pub fn control_routes(router: Router<EventRegistry>) -> Router<EventRegistry> {
    router.route("/events/{event_id}/control", control_method_router())
}

/// `GET /events/{event_id}/control` (WS upgrade) + `POST …/control` (one-shot) on the one path.
fn control_method_router() -> MethodRouter<EventRegistry> {
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
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    JsonBody(command): JsonBody<Command>,
) -> Result<Json<CommandAck>, ProtocolError> {
    // Resolve the event first (an unknown id → typed 404) so the command applies to THAT
    // event's log only — commands never cross event boundaries.
    let state = resolve_event(&registry, &event_id)?;
    Ok(Json(apply_command_in_event(
        &registry, &event_id, &state, command,
    )))
}

/// `GET /control` — upgrade to the bidirectional control WebSocket (protocol.html §5).
///
/// [`ControlAuth`] gates the upgrade (the #44 seam); the upgraded socket is driven by
/// [`run_control`].
async fn control_ws(
    _auth: ControlAuth,
    ws: WebSocketUpgrade,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Response, ProtocolError> {
    // Resolve the event before the upgrade so every command on this socket drives THAT
    // event's log (an unknown id → typed 404 before upgrading).
    let state = resolve_event(&registry, &event_id)?;
    Ok(ws.on_upgrade(move |socket| run_control(socket, registry, event_id, state)))
}

/// Drive one control socket: read [`Command`] frames, write a [`CommandAck`] per command
/// (protocol.html §5).
///
/// The RD sends a stream of JSON command frames; for each, [`apply_command`] validates and
/// (on success) appends, and the ack goes straight back on the same socket. A malformed
/// frame is answered with a `CommandAck::failed(BadRequest)` rather than closing the
/// socket, so one bad command does not drop the RD's control session. The loop ends when
/// the client closes or the socket errors.
async fn run_control(
    mut socket: WebSocket,
    registry: EventRegistry,
    event_id: EventId,
    state: AppState,
) {
    while let Some(frame) = socket.recv().await {
        let ack = match frame {
            Ok(Message::Text(text)) => match serde_json::from_str::<Command>(&text) {
                Ok(command) => apply_command_in_event(&registry, &event_id, &state, command),
                Err(e) => CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("malformed command: {e}"),
                )),
            },
            Ok(Message::Binary(bytes)) => match serde_json::from_slice::<Command>(&bytes) {
                Ok(command) => apply_command_in_event(&registry, &event_id, &state, command),
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

/// Apply a [`Command`] in the context of a known event (race redesign Slice 3a) — the
/// event-aware control write path both endpoints use.
///
/// All commands except [`Command::FillRound`] are pure log writes that need only the
/// event's [`AppState`], so they delegate straight to [`apply_command`]. `FillRound` is the
/// one command that also needs the event's **meta** (its rounds + class membership) to build
/// the round's generator, so it is handled here where the [`EventRegistry`] and
/// [`EventId`] are in scope — see [`apply_fill_round`].
pub fn apply_command_in_event(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    command: Command,
) -> CommandAck {
    match command {
        Command::FillRound { round } => apply_fill_round(registry, event_id, state, round),
        // `ScheduleHeat` also needs the event meta + timer registry (the channel cap + assignment),
        // so it is handled here rather than in the log-only `apply_command`.
        Command::ScheduleHeat {
            heat,
            lineup,
            class,
            round,
            frequencies,
        } => apply_schedule_heat(
            registry,
            event_id,
            state,
            heat,
            lineup,
            class,
            round,
            frequencies,
        ),
        other => apply_command(state, other),
    }
}

/// Handle [`Command::ScheduleHeat`] (race redesign Slice 4a) — create a heat with its lineup, with
/// the **channel assignment + heat-size cap** the round-driven path also applies.
///
/// The cap (lineup ≤ the event's effective primary timer's node count) is enforced here and an
/// oversized lineup is a typed `400` (nothing appended). Channels are assigned from the timer's
/// available set unless the caller supplied an explicit `frequencies` set (the caller — a test, a
/// manual override — wins; the engine assignment is the default when none is given). A pure-sim
/// event (no resolvable timer) assigns none.
#[allow(clippy::too_many_arguments)]
fn apply_schedule_heat(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    heat: HeatId,
    lineup: Vec<gridfpv_events::CompetitorRef>,
    class: Option<gridfpv_events::ClassId>,
    round: Option<gridfpv_events::RoundId>,
    frequencies: Vec<(gridfpv_events::CompetitorRef, u16)>,
) -> CommandAck {
    use crate::round_engine;

    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };

    // Caller-supplied frequencies win (manual override / test); otherwise assign from the event's
    // timer. Either way the heat-size cap is enforced against the event's timer.
    let frequencies = if frequencies.is_empty() {
        match round_engine::assign_for_event(&meta, &registry.timers(), &lineup) {
            Ok(freqs) => freqs,
            Err(err) => {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    err.to_string(),
                ));
            }
        }
    } else {
        // A caller-supplied assignment still must fit the timer's node count (the cap).
        if let Some(timer) = round_engine::assignment_timer(&meta, &registry.timers()) {
            if lineup.len() > timer.node_count as usize {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    round_engine::AssignError::TooManyForNodes {
                        lineup: lineup.len(),
                        nodes: timer.node_count as usize,
                    }
                    .to_string(),
                ));
            }
        }
        frequencies
    };

    let event = Event::HeatScheduled {
        heat,
        lineup,
        class,
        round,
        frequencies,
    };
    match state.append(event, None) {
        Ok(_offset) => CommandAck::ok(),
        Err(err) => CommandAck::failed(err),
    }
}

/// Handle [`Command::FillRound`] (race redesign Slice 3a): build the round's generator from
/// the event meta + the log, append the next tagged [`Event::HeatScheduled`] the generator
/// emits, and ack.
///
/// - A `Scheduled` outcome appends the heat tagged with `round` (and `class` when the round
///   is single-class), lineup from the generator's plan — then acks ok.
/// - A `Complete` or `AlreadyScheduled` outcome appends **nothing** and acks ok (a finished
///   round, or one whose outstanding heat must be scored first, are expected terminal/no-op
///   states — typed oks, not errors).
/// - A fill error (unknown round, empty field, unknown format) acks failed with a
///   [`ProtocolError`] — `UnknownScope` for a missing round, `BadRequest` otherwise.
fn apply_fill_round(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    round: gridfpv_events::RoundId,
) -> CommandAck {
    use crate::round_engine::{self, FillError, FillOutcome};

    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };
    let events = match state.read() {
        Ok((events, _cursor)) => events,
        Err(err) => return CommandAck::failed(err),
    };

    match round_engine::fill_round(&meta, &registry.timers(), &round, &events) {
        Ok(FillOutcome::Scheduled {
            heat,
            lineup,
            frequencies: static_freqs,
        }) => {
            let class = round_engine::round_class(&meta, &round);
            // Channel assignment differs by the round's channel mode (race redesign Slice 7a):
            //
            // - **Static** (`static_freqs` is `Some`): the channel-balanced builder already chose
            //   each pilot's fixed membership channel; use them directly. The heat-size cap was
            //   honoured by the builder (heats are ≤ node_count), but re-check defensively against
            //   the timer node count so an oversized heat never slips through.
            // - **Per-heat** (`static_freqs` is `None`): first-fit from the timer's pool (Slice 4a),
            //   which also enforces the node-count cap.
            let frequencies = match static_freqs {
                Some(freqs) => {
                    if let Some(timer) = round_engine::assignment_timer(&meta, &registry.timers()) {
                        if lineup.len() > timer.node_count as usize {
                            return CommandAck::failed(ProtocolError::new(
                                ErrorCode::BadRequest,
                                round_engine::AssignError::TooManyForNodes {
                                    lineup: lineup.len(),
                                    nodes: timer.node_count as usize,
                                }
                                .to_string(),
                            ));
                        }
                    }
                    freqs
                }
                None => match round_engine::assign_for_event(&meta, &registry.timers(), &lineup) {
                    Ok(freqs) => freqs,
                    Err(err) => {
                        return CommandAck::failed(ProtocolError::new(
                            ErrorCode::BadRequest,
                            err.to_string(),
                        ));
                    }
                },
            };
            let event = Event::HeatScheduled {
                heat,
                lineup,
                class,
                round: Some(round),
                frequencies,
            };
            match state.append(event, None) {
                Ok(_offset) => CommandAck::ok(),
                Err(err) => CommandAck::failed(err),
            }
        }
        // Complete / AlreadyScheduled: nothing to append, a successful typed ack.
        Ok(FillOutcome::Complete) | Ok(FillOutcome::AlreadyScheduled) => CommandAck::ok(),
        Err(err @ FillError::UnknownRound(_)) => {
            CommandAck::failed(ProtocolError::new(ErrorCode::UnknownScope, err.to_string()))
        }
        Err(err) => CommandAck::failed(ProtocolError::new(ErrorCode::BadRequest, err.to_string())),
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
        Command::Finalize { heat } => heat_transition(state, heat, HeatCommand::Finalize),
        Command::Advance { heat } => heat_transition(state, heat, HeatCommand::Advance),
        Command::Revert { heat } => heat_transition(state, heat, HeatCommand::Revert),
        Command::Abort { heat } => heat_transition(state, heat, HeatCommand::Abort),
        Command::Restart { heat } => heat_transition(state, heat, HeatCommand::Restart),
        Command::Discard { heat } => heat_transition(state, heat, HeatCommand::Discard),

        // --- Scheduling: creates the heat, so no prior-state check. The class/round/
        // frequency tags are carried straight through (default-absent for the
        // free-text path). ---
        Command::ScheduleHeat {
            heat,
            lineup,
            class,
            round,
            frequencies,
        } => Ok(Event::HeatScheduled {
            heat,
            lineup,
            class,
            round,
            frequencies,
        }),

        // --- FillRound is intercepted by `apply_command_in_event` (it needs the event
        // meta, not just the log) and never reaches here on the real control path. The arm
        // keeps the match exhaustive; on the (test-only) bare-`apply_command` path it is a
        // clear BadRequest rather than a silent append. ---
        Command::FillRound { .. } => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "FillRound must be applied through the event-aware control path",
        )),

        // --- Registration: bind a source competitor to a pilot (no prior-state check). ---
        Command::Register {
            adapter,
            competitor,
            pilot,
        } => Ok(Event::CompetitorRegistered {
            adapter,
            competitor,
            pilot,
        }),

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
                class: None,
                round: None,
                frequencies: vec![],
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

    /// (a) A legal Stage→Arm→Start→Finish→Finalize sequence acks ok and appends the matching
    /// `HeatStateChanged` events in order.
    #[test]
    fn legal_heat_loop_sequence_appends_transitions_and_acks_ok() {
        let state = scheduled_state();
        let steps = [
            (Command::Stage { heat: heat() }, HeatTransition::Staged),
            (Command::Arm { heat: heat() }, HeatTransition::Armed),
            (Command::Start { heat: heat() }, HeatTransition::Running),
            (Command::Finish { heat: heat() }, HeatTransition::Finished),
            (
                Command::Finalize { heat: heat() },
                HeatTransition::Finalized,
            ),
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

    /// `ScheduleHeat` creates the heat with its lineup; the free-text path leaves the
    /// additive class/round/frequencies absent.
    #[test]
    fn schedule_heat_appends_heat_scheduled() {
        let state = AppState::new(InMemoryLog::default());
        let lineup = vec![CompetitorRef("A".into()), CompetitorRef("B".into())];
        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: lineup.clone(),
                class: None,
                round: None,
                frequencies: vec![],
            },
        );
        assert!(ack.ok);
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::HeatScheduled { heat: h, lineup: l, class: None, round: None, frequencies }
                if *h == heat() && *l == lineup && frequencies.is_empty()
        )));
    }

    /// A `ScheduleHeat` carrying class/round/frequencies threads them straight into the
    /// emitted `HeatScheduled` (the scheduler path).
    #[test]
    fn schedule_heat_carries_class_round_and_frequencies() {
        use gridfpv_events::{ClassId, RoundId};
        let state = AppState::new(InMemoryLog::default());
        let lineup = vec![CompetitorRef("A".into()), CompetitorRef("B".into())];
        let freqs = vec![
            (CompetitorRef("A".into()), 5658u16),
            (CompetitorRef("B".into()), 5695u16),
        ];
        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: lineup.clone(),
                class: Some(ClassId("open".into())),
                round: Some(RoundId("r1".into())),
                frequencies: freqs.clone(),
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::HeatScheduled { heat: h, class: Some(c), round: Some(r), frequencies, .. }
                if *h == heat()
                    && *c == ClassId("open".into())
                    && *r == RoundId("r1".into())
                    && *frequencies == freqs
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
                class: None,
                round: None,
                frequencies: vec![],
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

    /// `Register` acks ok and appends the `CompetitorRegistered` binding (#60).
    #[test]
    fn register_appends_competitor_registered_and_acks_ok() {
        use gridfpv_events::PilotId;
        let state = scheduled_state();
        let ack = apply_command(
            &state,
            Command::Register {
                adapter: AdapterId("rh".into()),
                competitor: CompetitorRef("node-2".into()),
                pilot: PilotId("acroace".into()),
            },
        );
        assert!(ack.ok, "got {ack:?}");
        assert!(ack.error.is_none());

        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::CompetitorRegistered { adapter, competitor, pilot }
                if *adapter == AdapterId("rh".into())
                    && *competitor == CompetitorRef("node-2".into())
                    && *pilot == PilotId("acroace".into())
        )));
    }

    /// `FillRound` (race redesign Slice 3a), through the event-aware control path, builds the
    /// round's generator from the class membership and appends a tagged `HeatScheduled`.
    #[test]
    fn fill_round_schedules_a_tagged_heat_from_membership() {
        use crate::classes::CreateClassRequest;
        use crate::events::{
            ChannelMode, CreateEventRequest, MemberSlot, NewRoundReq, SeedingRule,
        };
        use crate::pilots::CreatePilotRequest;
        use crate::scope::EventId;
        use gridfpv_engine::scoring::WinCondition;
        use gridfpv_events::{ClassId, RoundId};
        use std::collections::BTreeMap;

        let registry = EventRegistry::new(None).unwrap();
        // Seed a directory class + two pilots, an event that selects the class, the class
        // membership, and a single-round timed_qual round.
        let class = registry
            .classes()
            .create(&CreateClassRequest {
                name: "Open".into(),
                source: Default::default(),
                reference: None,
                description: None,
            })
            .unwrap()
            .id;
        let mut pilots = Vec::new();
        for cs in ["alpha", "bravo"] {
            pilots.push(
                registry
                    .pilots()
                    .create(&CreatePilotRequest {
                        callsign: cs.into(),
                        ..Default::default()
                    })
                    .unwrap()
                    .id,
            );
        }
        let event = registry
            .create(&CreateEventRequest {
                name: "E".into(),
                date: None,
                location: None,
                description: None,
                organizer: None,
            })
            .unwrap()
            .id;
        registry.set_classes(&event, vec![class.clone()]).unwrap();
        registry
            .set_class_membership(
                &event,
                class.clone(),
                pilots.iter().cloned().map(MemberSlot::new).collect(),
            )
            .unwrap();
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    label: "Qual".into(),
                    classes: vec![class.clone()],
                    format: "timed_qual".into(),
                    params: BTreeMap::from([("rounds".into(), "1".into())]),
                    win_condition: WinCondition::BestLap,
                    seeding: SeedingRule::FromRoster,
                    // Per-heat: this test asserts the whole-field single heat (the bracket path).
                    channel_mode: Some(ChannelMode::PerHeat),
                },
            )
            .unwrap();

        let state = registry.resolve(&event).unwrap();
        let event_id = EventId(event.0.clone());
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.id.clone(),
            },
        );
        assert!(ack.ok, "FillRound rejected: {ack:?}");

        // A HeatScheduled tagged with the round + the single class, lineup = the membership.
        let (events, _) = state.read().unwrap();
        let scheduled = events
            .iter()
            .find_map(|e| match e {
                Event::HeatScheduled {
                    lineup,
                    class: Some(c),
                    round: Some(r),
                    ..
                } => Some((lineup.clone(), c.clone(), r.clone())),
                _ => None,
            })
            .expect("FillRound appended a tagged HeatScheduled");
        assert_eq!(scheduled.1, ClassId(class.0.clone()));
        assert_eq!(scheduled.2, RoundId(round.id.0.clone()));
        assert_eq!(
            scheduled.0,
            pilots
                .iter()
                .map(|p| CompetitorRef(p.0.clone()))
                .collect::<Vec<_>>()
        );
    }

    /// `FillRound` on a round that does not exist is an `UnknownScope` rejection (no append).
    #[test]
    fn fill_round_unknown_round_is_rejected() {
        use crate::scope::EventId;
        use gridfpv_events::RoundId;

        let registry = EventRegistry::new(None).unwrap();
        let event = EventId(crate::events::PRACTICE_EVENT_ID.into());
        let state = registry.resolve(&event).unwrap();
        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::FillRound {
                round: RoundId("nope".into()),
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::UnknownScope);
        let (events, _) = state.read().unwrap();
        assert!(events.is_empty(), "a rejected FillRound appends nothing");
    }

    /// Build an event selecting one timer (created from `req`) over a class with `pilots`, plus a
    /// single-round timed_qual round. Returns the registry, the event id, and the round id.
    #[cfg(test)]
    fn event_with_timer_and_round(
        timer_req: crate::timers::CreateTimerRequest,
        pilots: &[&str],
    ) -> (EventRegistry, EventId, gridfpv_events::RoundId) {
        use crate::classes::CreateClassRequest;
        use crate::events::{
            ChannelMode, CreateEventRequest, MemberSlot, NewRoundReq, SeedingRule,
        };
        use crate::pilots::CreatePilotRequest;
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let registry = EventRegistry::new(None).unwrap();
        let timer = registry.timers().create(&timer_req).unwrap();
        let class = registry
            .classes()
            .create(&CreateClassRequest {
                name: "Open".into(),
                source: Default::default(),
                reference: None,
                description: None,
            })
            .unwrap()
            .id;
        let pilot_ids: Vec<_> = pilots
            .iter()
            .map(|cs| {
                registry
                    .pilots()
                    .create(&CreatePilotRequest {
                        callsign: (*cs).into(),
                        ..Default::default()
                    })
                    .unwrap()
                    .id
            })
            .collect();
        let event = registry
            .create(&CreateEventRequest {
                name: "E".into(),
                date: None,
                location: None,
                description: None,
                organizer: None,
            })
            .unwrap()
            .id;
        registry.set_classes(&event, vec![class.clone()]).unwrap();
        registry
            .set_class_membership(
                &event,
                class.clone(),
                pilot_ids.into_iter().map(MemberSlot::new).collect(),
            )
            .unwrap();
        registry.set_timers(&event, vec![timer.id]).unwrap();
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    label: "Qual".into(),
                    classes: vec![class],
                    format: "timed_qual".into(),
                    params: BTreeMap::from([("rounds".into(), "1".into())]),
                    win_condition: WinCondition::BestLap,
                    seeding: SeedingRule::FromRoster,
                    // Per-heat: this test asserts first-fit channel assignment from the timer pool.
                    channel_mode: Some(ChannelMode::PerHeat),
                },
            )
            .unwrap();
        (registry, EventId(event.0.clone()), round.id)
    }

    /// `FillRound` assigns channels from the event's selected timer onto the heat — the lineup gets
    /// first-fit Raceband frequencies in seed order (race redesign Slice 4a).
    #[test]
    fn fill_round_assigns_frequencies_from_the_selected_timer() {
        use crate::timers::{ChannelCapability, CreateTimerRequest, TimerKind};
        let timer_req = CreateTimerRequest {
            name: "8-node".into(),
            kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
            channel_capability: Some(ChannelCapability::Flexible),
            node_count: Some(8),
            available_channels: Some(crate::channels::RACEBAND_MHZ.to_vec()),
        };
        let (registry, event_id, round) =
            event_with_timer_and_round(timer_req, &["alpha", "bravo"]);
        let state = registry.resolve(&event_id).unwrap();
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.clone(),
            },
        );
        assert!(ack.ok, "FillRound rejected: {ack:?}");

        let (events, _) = state.read().unwrap();
        let freqs = events
            .iter()
            .find_map(|e| match e {
                Event::HeatScheduled { frequencies, .. } if !frequencies.is_empty() => {
                    Some(frequencies.clone())
                }
                _ => None,
            })
            .expect("the scheduled heat carries an assigned frequency set");
        // Top two seeds get Raceband R1, R2 in order.
        assert_eq!(freqs.len(), 2);
        assert_eq!(freqs[0].1, 5658);
        assert_eq!(freqs[1].1, 5695);
    }

    /// `FillRound` rejects an oversized lineup with a typed `BadRequest` (the heat-size cap) and
    /// appends nothing (race redesign Slice 4a).
    #[test]
    fn fill_round_rejects_a_lineup_over_the_node_cap() {
        use crate::timers::{ChannelCapability, CreateTimerRequest, TimerKind};
        // A 2-node timer, but the round fields four pilots — the heat exceeds the cap.
        let timer_req = CreateTimerRequest {
            name: "2-node".into(),
            kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
            channel_capability: Some(ChannelCapability::Flexible),
            node_count: Some(2),
            available_channels: Some(crate::channels::RACEBAND_MHZ.to_vec()),
        };
        let (registry, event_id, round) =
            event_with_timer_and_round(timer_req, &["a", "b", "c", "d"]);
        let state = registry.resolve(&event_id).unwrap();
        let before = state.read().unwrap().0.len();
        let ack =
            apply_command_in_event(&registry, &event_id, &state, Command::FillRound { round });
        assert!(!ack.ok, "an oversized heat must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let after = state.read().unwrap().0.len();
        assert_eq!(before, after, "a rejected FillRound appends nothing");
    }
}
