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
//! | heat-loop (`Stage`/`Start`/`SkipCountdown`/`ForceEnd`/`Finalize`/`Advance`/`Revert`/`Abort`/`Restart`/`Discard`) | [`heat::heat_state`] folds the heat's current state; [`heat::apply`] checks the transition is legal | [`Event::HeatStateChanged`] with the engine-returned [`HeatTransition`](gridfpv_events::HeatTransition) |
//! | [`Command::ScheduleHeat`] | the id is genuinely **new**, the lineup seats no competitor twice, and a `round`/`class` tag resolves against the event meta (round exists; class selected + round-eligible; pilot refs are eligible members) — #335 | [`Event::HeatScheduled`] |
//! | [`Command::SetCurrentHeat`] | the heat exists in the log | [`Event::CurrentHeatSelected`] |
//! | [`Command::Register`] | none (the binding is always recordable; last-registration-wins folds downstream) | [`Event::CompetitorRegistered`] |
//! | [`Command::VoidDetection`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) | [`Event::DetectionVoided`] |
//! | [`Command::AdjustLap`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) | [`Event::LapAdjusted`] |
//! | [`Command::InsertLap`] | none (it adds a pass) | [`Event::LapInserted`] |
//! | [`Command::SplitLap`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) (the lap's ending pass) | [`Event::LapSplit`] |
//! | [`Command::VoidHeat`] | the heat exists in the log | [`Event::HeatVoided`] |
//! | [`Command::ApplyPenalty`] | the heat exists in the log | [`Event::PenaltyApplied`] |
//! | [`Command::DeductPoints`] | the heat exists in the log | [`Event::PenaltyApplied`] (`PointsDeducted`) |
//! | [`Command::ThrowOutLap`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) | [`Event::LapThrownOut`] |
//! | [`Command::FileProtest`] | the heat exists in the log | [`Event::ProtestFiled`] |
//! | [`Command::ResolveProtest`] | the `target` offset exists and is a [`Event::ProtestFiled`] | [`Event::ProtestResolved`] |
//! | [`Command::ReverseRuling`] | the `target` offset exists and is a reversible ruling (penalty / throw-out / protest resolution / heat-void) | [`Event::RulingReversed`] |
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
use crate::control::{Command, CommandAck, FillMode};
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
        Command::FillRound { round, mode } => {
            apply_fill_round(registry, event_id, state, round, mode)
        }
        // `Advance` is more than the `Final → Advanced` transition: advancing a finalized heat
        // **loads the next heat to run** into Live control. That may need a generator draw (a
        // round mid-fill) and always needs to select the next heat, so it runs through the
        // event-aware path where the registry/meta are in scope — see [`apply_advance`].
        Command::Advance { heat } => apply_advance(registry, event_id, state, heat),
        // `ScheduleHeat` also needs the event meta + timer registry (the channel cap + assignment),
        // so it is handled here rather than in the log-only `apply_command`.
        Command::ScheduleHeat {
            heat,
            lineup,
            class,
            round,
            frequencies,
            label,
        } => apply_schedule_heat(
            registry,
            event_id,
            state,
            heat,
            lineup,
            class,
            round,
            frequencies,
            label,
        ),
        other => apply_command(state, other),
    }
}

/// Handle [`Command::ScheduleHeat`] (race redesign Slice 4a) — create a heat with its lineup, with
/// the **channel assignment + heat-size cap** the round-driven path also applies.
///
/// Validated before anything is appended (#335, the #330 hardening pattern — a raw API call must
/// not lay down a heat the UI could never build); every failure is a typed `400`:
///
/// - the heat id is genuinely **new** ([`require_new_heat_id`] — a duplicate would re-seed an
///   existing, possibly Final, heat back to `Scheduled`);
/// - the lineup seats no competitor twice ([`require_distinct_lineup`]);
/// - a `round` tag names one of the event's rounds; a `class` tag names a class the event
///   selects — and, when both are tagged, one **eligible** for that round;
/// - on a tagged heat, every lineup ref that names a **directory pilot** is a member of the
///   tagged round's eligible classes' membership (the same field FillRound / the console's
///   eligible-members picker resolves) — see [`validate_tagged_lineup`]. Non-pilot refs
///   (`node-{i}` timer seats, sim free-text names) pass through, so practice-style heats keep
///   scheduling.
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
    label: Option<String>,
) -> CommandAck {
    use crate::round_engine;

    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };

    // #335 validation, all-or-nothing before the append: the log-only guards (fresh id, distinct
    // lineup) plus the meta-scoped tag/membership checks.
    if let Err(err) = require_new_heat_id(state, &heat)
        .and_then(|()| require_distinct_lineup(&lineup))
        .and_then(|()| validate_tagged_lineup(registry, &meta, &lineup, &class, &round))
    {
        return CommandAck::failed(err);
    }

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
        label,
    };
    match state.append(event, None) {
        Ok(_offset) => CommandAck::ok(),
        Err(err) => CommandAck::failed(err),
    }
}

/// Require that `heat` names a genuinely **new** heat — one the log never scheduled (#335).
///
/// The fold ([`heat::heat_state`]) deliberately re-seeds a repeated `HeatScheduled` back to
/// `Scheduled` (robustness on replay), so *accepting* a duplicate id would silently reset an
/// existing — possibly finished/**Final** — heat and orphan its result (#341). A re-run of a raced
/// heat is a `Discard`/`Restart` transition on the existing heat, never a re-schedule; a new heat
/// takes a fresh id (the console mints collision-checked ids for exactly this reason).
fn require_new_heat_id(state: &AppState, heat: &HeatId) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    if heat::heat_state(&events, heat).is_some() {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "a heat with id {:?} already exists; use Discard/Restart to re-run it, or pick a \
                 fresh id",
                heat.0
            ),
        ));
    }
    Ok(())
}

/// Reject a lineup that seats the **same competitor twice** (#335): a lineup ref is the handle
/// passes/channels key on, so a duplicate would merge two seats into one pilot's lap stream.
fn require_distinct_lineup(lineup: &[gridfpv_events::CompetitorRef]) -> Result<(), ProtocolError> {
    let mut seen = std::collections::BTreeSet::new();
    for competitor in lineup {
        if !seen.insert(competitor.0.as_str()) {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                format!(
                    "competitor {:?} appears more than once in the lineup",
                    competitor.0
                ),
            ));
        }
    }
    Ok(())
}

/// Validate a `ScheduleHeat`'s **round/class tag + lineup** against the event meta (#335).
///
/// - A `round` tag must name one of the event's rounds; a `class` tag must be one the event
///   selects and — when a round is also tagged — **eligible** for that round (in the round's
///   `classes`). An untagged (free-text / practice) heat skips all of this.
/// - On a tagged heat, every lineup ref that names a **directory pilot** must be an *eligible
///   member*: the union of the tagged round's eligible classes' membership (mirroring how
///   FillRound's `FromRoster` field and the console's eligible-members picker resolve), or the
///   tagged class's own membership when only a class is tagged. Refs that are **not** directory
///   pilots pass through — `node-{i}` timer seats (the open-practice channel lineup) and sim
///   free-text names have no membership to check — so practice-style heats keep scheduling.
fn validate_tagged_lineup(
    registry: &EventRegistry,
    meta: &crate::events::EventMeta,
    lineup: &[gridfpv_events::CompetitorRef],
    class: &Option<gridfpv_events::ClassId>,
    round: &Option<gridfpv_events::RoundId>,
) -> Result<(), ProtocolError> {
    // (1) The round tag resolves to this event's round definition.
    let round_def = match round {
        Some(id) => match meta.rounds.iter().find(|r| &r.id == id) {
            Some(def) => Some(def),
            None => {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!("no round with id {:?} in this event", id.0),
                ));
            }
        },
        None => None,
    };

    // (2) The class tag is selected by the event, and eligible for the tagged round.
    if let Some(class_id) = class {
        if !meta.classes.contains(class_id) {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                format!("class {:?} is not selected by this event", class_id.0),
            ));
        }
        if let Some(def) = round_def {
            if !def.classes.contains(class_id) {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!(
                        "class {:?} is not eligible for round {:?}",
                        class_id.0, def.id.0
                    ),
                ));
            }
        }
    }

    // (3) Membership: only for a tagged heat, and only for pilot-shaped refs.
    if round_def.is_none() && class.is_none() {
        return Ok(());
    }
    // The eligible member set — the tagged round's eligible classes (the round wins when both are
    // tagged: its `classes` already contain the class per check 2), else the tagged class alone.
    let eligible_classes: Vec<&gridfpv_events::ClassId> = match round_def {
        Some(def) => def.classes.iter().collect(),
        None => class.iter().collect(),
    };
    let mut members = std::collections::BTreeSet::new();
    for cls in eligible_classes {
        if let Some(membership) = meta.classes_membership.iter().find(|m| &m.class == cls) {
            for slot in &membership.pilots {
                members.insert(slot.pilot.0.as_str());
            }
        }
    }
    let pilots = registry.pilots();
    for competitor in lineup {
        // A ref is "pilot-shaped" when it names a directory pilot — the console's build path
        // seats pilot ids verbatim as refs. Anything else (node seats, free-text) passes.
        if pilots.exists(&gridfpv_events::PilotId(competitor.0.clone()))
            && !members.contains(competitor.0.as_str())
        {
            return Err(ProtocolError::new(
                ErrorCode::BadRequest,
                format!(
                    "pilot {:?} is not a member of the tagged round/class's eligible classes",
                    competitor.0
                ),
            ));
        }
    }
    Ok(())
}

/// The defensive cap on `FillMode::All`'s loop (#216): a real deterministic format always
/// converges to `Complete` in far fewer than this, so hitting it means the generator never
/// reported done — a logic bug, not a request for a 1000th heat. We stop and log rather than
/// spin unbounded. (Mirrors [`round_engine::MAX_HEATS_PER_ROUND`].)
const MAX_FILL_ALL_HEATS: usize = 1_000;

/// One iteration of filling a round: build the generator from the current log, and if it emits
/// the next heat, append the tagged [`Event::HeatScheduled`]. Returns whether a heat was
/// appended (so the `All` loop knows to draw again) or the round reached a terminal/no-op state,
/// or a failed ack to surface verbatim.
enum FillStep {
    /// A heat was appended — re-fold and draw the next (the `All` loop continues here).
    Appended,
    /// `Complete`/`AlreadyScheduled` — nothing to append now; the round's done for this command.
    Terminal,
    /// A fill or append error; carries the ack to return as-is.
    Failed(CommandAck),
}

/// Run the generator once against the current log and append at most one heat. Re-reads the log
/// each call so a just-appended heat is folded in on the next — this is what lets `FillMode::All`
/// iterate by simply calling this until it reports [`FillStep::Terminal`].
fn fill_round_once(
    registry: &EventRegistry,
    meta: &crate::events::EventMeta,
    state: &AppState,
    round: &gridfpv_events::RoundId,
) -> FillStep {
    use crate::round_engine::{self, FillError, FillOutcome};

    let events = match state.read() {
        Ok((events, _cursor)) => events,
        Err(err) => return FillStep::Failed(CommandAck::failed(err)),
    };

    match round_engine::fill_round(meta, &registry.timers(), round, &events) {
        Ok(FillOutcome::Scheduled {
            heat,
            lineup,
            frequencies: static_freqs,
            field_draw,
        }) => {
            let class = round_engine::round_class(meta, round);
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
                    if let Some(timer) = round_engine::assignment_timer(meta, &registry.timers()) {
                        if lineup.len() > timer.node_count as usize {
                            return FillStep::Failed(CommandAck::failed(ProtocolError::new(
                                ErrorCode::BadRequest,
                                round_engine::AssignError::TooManyForNodes {
                                    lineup: lineup.len(),
                                    nodes: timer.node_count as usize,
                                }
                                .to_string(),
                            )));
                        }
                    }
                    freqs
                }
                None => match round_engine::assign_for_event(meta, &registry.timers(), &lineup) {
                    Ok(freqs) => freqs,
                    Err(err) => {
                        return FillStep::Failed(CommandAck::failed(ProtocolError::new(
                            ErrorCode::BadRequest,
                            err.to_string(),
                        )));
                    }
                },
            };
            // FREEZE-AT-FILL (#334): a carry-seeded round's first fill records its resolved
            // field BEFORE the heat, so every later read (fills, ranking, standings, dependent
            // seeding) replays this draw instead of re-resolving a source whose adjudications
            // may have since moved.
            if let Some(field) = field_draw {
                if let Err(err) = state.append(
                    Event::RoundFieldDrawn {
                        round: round.clone(),
                        field,
                    },
                    None,
                ) {
                    return FillStep::Failed(CommandAck::failed(err));
                }
            }
            let event = Event::HeatScheduled {
                heat,
                lineup,
                class,
                round: Some(round.clone()),
                frequencies,
                // A generator-filled heat keeps the derived auto-name (no custom label).
                label: None,
            };
            match state.append(event, None) {
                Ok(_offset) => FillStep::Appended,
                Err(err) => FillStep::Failed(CommandAck::failed(err)),
            }
        }
        // Complete / AlreadyScheduled: nothing to append, a successful terminal state.
        Ok(FillOutcome::Complete) | Ok(FillOutcome::AlreadyScheduled) => FillStep::Terminal,
        Err(err @ FillError::UnknownRound(_)) => FillStep::Failed(CommandAck::failed(
            ProtocolError::new(ErrorCode::UnknownScope, err.to_string()),
        )),
        Err(err) => FillStep::Failed(CommandAck::failed(ProtocolError::new(
            ErrorCode::BadRequest,
            err.to_string(),
        ))),
    }
}

/// Handle [`Command::FillRound`] (race redesign Slice 3a; fill-all added #216): build the round's
/// generator from the event meta + the log and append heat(s) per `mode`, then ack.
///
/// - [`FillMode::Next`] runs the generator **once**: a `Scheduled` outcome appends the heat tagged
///   with `round` (and `class` when single-class), lineup from the generator's plan; a
///   `Complete`/`AlreadyScheduled` appends nothing. The interactive single-step (Open Practice, and
///   the building block).
/// - [`FillMode::All`] loops [`fill_round_once`] — append, re-fold, draw again — until the round
///   reports terminal (`Complete`/`AlreadyScheduled`), filling a whole deterministic round in one
///   command. The loop re-reads the log each pass so the generator sees the just-appended heat, is
///   idempotent on an already-complete round (appends nothing), and is capped at
///   [`MAX_FILL_ALL_HEATS`] defensively (logged if hit).
///
/// Either mode acks ok once it reaches the terminal state, or acks failed (verbatim) with a
/// [`ProtocolError`] on a fill/append error — `UnknownScope` for a missing round, `BadRequest`
/// otherwise. Any heat appended before an error mid-batch stays in the log (each append is its own
/// committed event); the ack reports the failure.
pub fn apply_fill_round(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    round: gridfpv_events::RoundId,
    mode: FillMode,
) -> CommandAck {
    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };

    match mode {
        // The original single-step fill — one generator draw, at most one heat appended.
        FillMode::Next => match fill_round_once(registry, &meta, state, &round) {
            FillStep::Appended | FillStep::Terminal => CommandAck::ok(),
            FillStep::Failed(ack) => ack,
        },
        // Iterate the single step until the round is terminal, capped defensively.
        FillMode::All => {
            // An open-ended `Static` round (rounds=0) never reports Complete — its generator yields
            // the next heat on demand forever — so a batch `All` fill would loop to the cap and
            // schedule MAX_FILL_ALL_HEATS heats while acking ok (release-hardening P1-8). Reject it
            // up front and steer the RD to single-step fills.
            if let Some(def) = meta.rounds.iter().find(|r| r.id == round) {
                if crate::round_engine::is_open_ended_static(def) {
                    return CommandAck::failed(ProtocolError::new(
                        ErrorCode::BadRequest,
                        format!(
                            "round {:?} is open-ended (no fixed heat count); use single-step fill \
                             instead of fill-all",
                            round.0
                        ),
                    ));
                }
            }
            for _ in 0..MAX_FILL_ALL_HEATS {
                match fill_round_once(registry, &meta, state, &round) {
                    FillStep::Appended => continue,
                    FillStep::Terminal => return CommandAck::ok(),
                    FillStep::Failed(ack) => return ack,
                }
            }
            eprintln!(
                "FillRound(All) for round {:?} hit the {MAX_FILL_ALL_HEATS}-heat cap without the \
                 generator reporting complete — stopping. A real format converges in far fewer; \
                 this indicates a generator bug, not a {MAX_FILL_ALL_HEATS}-heat round.",
                round.0,
            );
            // We still appended up to the cap; ack ok so the RD sees the heats that were drawn.
            CommandAck::ok()
        }
    }
}

/// Handle [`Command::Advance`] — advancing a finalized heat **loads the next heat to run**.
///
/// Before this, `Advance` only recorded the `Final → Advanced` transition (which leaves the heat
/// `Final` — a terminal off-ramp) and nothing else moved: Live control stayed on the just-finished
/// heat, so the button looked like a no-op. Now it does the two-step the RD expects:
///
/// 1. Append the `Advanced` transition (reusing the engine's legality via [`heat_transition`]); a
///    heat that is not `Final` (or never scheduled) is rejected verbatim and nothing else happens.
/// 2. Find the **next heat to run** and select it (so the `current_heat` fold follows it):
///    - the **on-deck** heat — the next still-`Scheduled` heat — if one already exists; else
///    - **generate** the next heat for the advanced heat's round (one generator draw, the same
///      single-step path [`Command::FillRound`] `Next` uses) and select that; else
///    - **nothing to advance to** (the round is complete / the heat was untagged) — leave the heat
///      `Advanced` (a clear terminal), ack ok. No crash, no spurious selection.
///
/// Each step is its own append; a generated heat plus the selection are two events, which is why
/// this lives on the event-aware path (it needs the registry/meta to draw, like `FillRound`).
fn apply_advance(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    heat: HeatId,
) -> CommandAck {
    // 1. Record the `Final → Advanced` transition (engine legality + event shape reused). A
    //    non-`Final` or unknown heat is rejected verbatim here, appending nothing.
    let advanced = match heat_transition(state, heat.clone(), HeatCommand::Advance) {
        Ok(event) => event,
        Err(err) => return CommandAck::failed(err),
    };
    if let Err(err) = state.append(advanced, None) {
        return CommandAck::failed(err);
    }

    // 2. Pick the next heat to run. The advanced heat is now `Final` (not `Scheduled`), so
    //    `on_deck` against it is exactly "the next still-`Scheduled` heat" — the heat to load.
    let next = {
        let (events, _cursor) = match state.read() {
            Ok(read) => read,
            Err(err) => return CommandAck::failed(err),
        };
        crate::live_state::on_deck(&events, &heat)
    };

    if let Some(next) = next {
        // A next heat is already scheduled — follow Live control to it.
        return select_next_heat(state, next);
    }

    // Nothing is on deck: ask the advanced heat's round generator for the next heat (a round
    // mid-fill), then select whatever it scheduled. An untagged heat has no round to draw from.
    let round = {
        let (events, _cursor) = match state.read() {
            Ok(read) => read,
            Err(err) => return CommandAck::failed(err),
        };
        crate::live_state::round_of_heat(&events, &heat)
    };
    if let Some(round) = round {
        if let Some(meta) = registry.meta_of(event_id) {
            // One generator draw (the `FillMode::Next` step). It either appends the next heat or
            // reports the round terminal (nothing to schedule).
            match fill_round_once(registry, &meta, state, &round) {
                FillStep::Appended => {
                    // A heat was just scheduled in this round; on_deck now finds it. Select it.
                    let next = {
                        let (events, _cursor) = match state.read() {
                            Ok(read) => read,
                            Err(err) => return CommandAck::failed(err),
                        };
                        crate::live_state::on_deck(&events, &heat)
                    };
                    if let Some(next) = next {
                        return select_next_heat(state, next);
                    }
                }
                // Round complete / already-scheduled-elsewhere: nothing more to load.
                FillStep::Terminal => {}
                // A generator/append error: surface it verbatim (the Advanced transition stands).
                FillStep::Failed(ack) => return ack,
            }
        }
    }

    // The round is genuinely complete (or the heat was untagged and nothing is on deck): the heat
    // stays `Advanced`, a clean terminal. Ack ok — Advance succeeded, there was just no next heat.
    CommandAck::ok()
}

/// Append a [`Event::CurrentHeatSelected`] to move Live control onto `next`, the same selection the
/// RD's manual "select heat" records — so the `current_heat` fold follows it. The heat is one we
/// just derived from the log (on-deck / freshly generated), so it is known-scheduled; no further
/// validation needed.
fn select_next_heat(state: &AppState, next: HeatId) -> CommandAck {
    match state.append(Event::CurrentHeatSelected { heat: next }, None) {
        Ok(_offset) => CommandAck::ok(),
        Err(err) => CommandAck::failed(err),
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
        Command::Start { heat } => heat_transition(state, heat, HeatCommand::Start),
        Command::SkipCountdown { heat } => heat_transition(state, heat, HeatCommand::SkipCountdown),
        Command::ForceEnd { heat } => heat_transition(state, heat, HeatCommand::ForceEnd),
        Command::Finalize { heat } => {
            // Gate finalize on **open protests** (release-hardening P1-4): a heat with a filed,
            // unresolved protest must not be finalized — the result is still contested. The RD
            // resolves (or reverses a resolution of) the protest first. The auto-official protest
            // window appends its `Finalized` directly (not through this command path) but checks
            // this same `open_protest_count` predicate before doing so (issue #338).
            let (events, _cursor) = state.read()?;
            let open = open_protest_count(&events, &heat);
            if open > 0 {
                return Err(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!(
                        "cannot finalize heat {:?}: resolve {open} open protest(s) first",
                        heat.0
                    ),
                ));
            }
            heat_transition(state, heat, HeatCommand::Finalize)
        }
        // `Advance` on the real control path is intercepted by `apply_command_in_event`
        // (it loads the next heat too — see `apply_advance`). On the bare-`apply_command`
        // path it records just the `Final → Advanced` transition (no next-heat selection),
        // which is the legality-checked event with no event scope to draw a next heat from.
        Command::Advance { heat } => heat_transition(state, heat, HeatCommand::Advance),
        Command::Revert { heat } => heat_transition(state, heat, HeatCommand::Revert),
        Command::Abort { heat } => heat_transition(state, heat, HeatCommand::Abort),
        Command::Restart { heat } => heat_transition(state, heat, HeatCommand::Restart),
        Command::Discard { heat } => heat_transition(state, heat, HeatCommand::Discard),

        // --- Live-control selection: validate the heat exists, reject while the current heat
        // is mid-commit, then record the choice. Not a heat-loop transition — it moves Live
        // control's focus, not the heat's state. ---
        Command::SetCurrentHeat { heat } => {
            require_scheduled_heat(state, &heat)?;
            reject_if_current_heat_committed(state)?;
            Ok(Event::CurrentHeatSelected { heat })
        }

        // --- Scheduling: creates the heat, so the prior-state check is INVERTED — the id must
        // be new (a duplicate would re-seed an existing heat, #335) and the lineup distinct.
        // The class/round/frequency tags are carried straight through (default-absent for the
        // free-text path); the meta-scoped tag validation lives on the event-aware path
        // (`apply_schedule_heat`), which is the only one the real control endpoints drive. ---
        Command::ScheduleHeat {
            heat,
            lineup,
            class,
            round,
            frequencies,
            label,
        } => {
            require_new_heat_id(state, &heat)?;
            require_distinct_lineup(&lineup)?;
            Ok(Event::HeatScheduled {
                heat,
                lineup,
                class,
                round,
                frequencies,
                label,
            })
        }

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
        Command::SplitLap { target, at } => {
            // The split's target is the pass that *ends* the over-long lap — a real Pass,
            // validated exactly like `VoidDetection`/`AdjustLap`.
            require_pass_target(state, target)?;
            Ok(Event::LapSplit { target, at })
        }
        Command::InsertLap {
            adapter,
            competitor,
            at,
            heat,
        } => {
            // A tagged insertion must name a real heat (the tag is what routes it into that
            // heat's scoring window even when a different heat is live); an untagged one is a
            // legacy client and attributes positionally, as before.
            if let Some(h) = &heat {
                require_scheduled_heat(state, h)?;
            }
            Ok(Event::LapInserted {
                adapter,
                competitor,
                at,
                heat,
            })
        }
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
        // Sugar over `ApplyPenalty` with a points-deduction penalty (standings-only, Slice 6).
        Command::DeductPoints {
            heat,
            competitor,
            points,
        } => {
            require_scheduled_heat(state, &heat)?;
            Ok(Event::PenaltyApplied {
                heat,
                competitor,
                penalty: gridfpv_events::Penalty::PointsDeducted { points },
            })
        }
        // Throw out a valid lap: the target is the lap's end pass. Unlike `VoidDetection`, an
        // *inserted* or *split* lap is also throw-out-able (its `end_ref` addresses the synthetic
        // pass the projection emits from the `LapInserted`/`LapSplit` event), so validate against
        // any lap-end-producing event, not only a raw `Pass`.
        Command::ThrowOutLap { target } => {
            require_lap_end_target(state, target)?;
            Ok(Event::LapThrownOut { target })
        }
        // File a protest against a heat result — the append-only filing fact.
        Command::FileProtest {
            heat,
            competitor,
            note,
        } => {
            require_scheduled_heat(state, &heat)?;
            Ok(Event::ProtestFiled {
                heat,
                competitor,
                note,
            })
        }
        // Resolve a filed protest — the target must be a real `ProtestFiled`.
        Command::ResolveProtest { target, outcome } => {
            require_protest_target(state, target)?;
            Ok(Event::ProtestResolved { target, outcome })
        }
        Command::ReverseRuling { target } => {
            // Generalized reversal (Slice 6): the target must be a real *ruling* — a penalty, a
            // throw-out, a protest resolution, or a heat-void. Validated so an out-of-range or
            // non-ruling offset is a typed BadRequest (nothing appended).
            require_ruling_target(state, target)?;
            Ok(Event::RulingReversed { target })
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

/// Reject a current-heat change while the **current heat is mid-commit** — its folded phase
/// is `Staged`, `Armed`, or `Running` (race-engine.html §2). After Stage the RD is committed
/// to that race; switching focus is only allowed once it is aborted back to `Scheduled` or
/// finishes to `Unofficial`/`Final` (and is always allowed when there is no current heat, or
/// the current heat is still `Scheduled`).
///
/// Computed from the same live-state derivation the read path uses (the `current_heat` fold +
/// `heat::heat_state`/[`HeatState`](gridfpv_engine::heat::HeatState)), so the lock matches what
/// the live view shows and replays deterministically. A locked phase maps to a typed
/// [`ErrorCode::BadRequest`]; nothing is appended.
fn reject_if_current_heat_committed(state: &AppState) -> Result<(), ProtocolError> {
    use gridfpv_engine::heat::HeatState;

    let (events, _cursor) = state.read()?;
    // The current heat is whatever the live view is focused on (the last heat-loop transition
    // or explicit selection, else the first scheduled heat). Reuse that exact derivation.
    let Some(current) = crate::live_state::live_state(&events).current_heat else {
        return Ok(()); // no current heat → always free to select
    };
    let committed = matches!(
        heat::heat_state(&events, &current),
        Some(HeatState::Staged | HeatState::Armed | HeatState::Running)
    );
    if committed {
        Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "cannot change the current heat while a heat is staged or running — \
             abort it or finish to Unofficial first",
        ))
    } else {
        Ok(())
    }
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

/// Require that `target` names a real lap **end** in the log — a raw [`Pass`](gridfpv_events::Pass)
/// *or* a marshaling event that synthesises a lap-gate pass ([`Event::LapInserted`] /
/// [`Event::LapSplit`]), since those are addressable lap ends in the corrected lap list
/// (`corrected_passes`) and so are legitimately throw-out-able. The cheap target check for
/// [`Command::ThrowOutLap`](crate::control::Command::ThrowOutLap). An out-of-range or non-lap-end
/// offset is [`ErrorCode::BadRequest`]; nothing is appended.
fn require_lap_end_target(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match events.get(target.0 as usize) {
        Some(Event::Pass(_) | Event::LapInserted { .. } | Event::LapSplit { .. }) => Ok(()),
        Some(_) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "log offset {} is not a lap end (a pass, inserted, or split lap)",
                target.0
            ),
        )),
        None => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("log offset {} is out of range", target.0),
        )),
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

/// Require that `target` names a **reversible ruling** in the log — the cheap target check for the
/// generalized [`Command::ReverseRuling`](crate::control::Command::ReverseRuling) (marshaling Slice
/// 6). A ruling is a [`PenaltyApplied`](Event::PenaltyApplied) (DQ / time / points), a
/// [`LapThrownOut`](Event::LapThrownOut), a [`ProtestResolved`](Event::ProtestResolved), or a
/// [`HeatVoided`](Event::HeatVoided). An out-of-range or non-ruling offset is
/// [`ErrorCode::BadRequest`]; nothing is appended.
fn require_ruling_target(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match events.get(target.0 as usize) {
        Some(
            Event::PenaltyApplied { .. }
            | Event::LapThrownOut { .. }
            | Event::ProtestResolved { .. }
            | Event::HeatVoided { .. },
        ) => Ok(()),
        Some(_) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "log offset {} is not a reversible ruling (penalty, throw-out, protest resolution, or heat-void)",
                target.0
            ),
        )),
        None => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("log offset {} is out of range", target.0),
        )),
    }
}

/// Count a heat's **open protests** (release-hardening P1-4): [`Event::ProtestFiled`] facts for
/// `heat` that have no *effective* resolution.
///
/// A protest (filed at offset `f`) is closed by a [`Event::ProtestResolved`] whose `target` is `f`
/// — **unless** that resolution was itself reversed by a [`Event::RulingReversed`] (the structural
/// "void the void"), which re-opens the protest. So the open set is: filed-for-this-heat minus the
/// filings that carry a non-reversed resolution.
///
/// This is **the** open-protest predicate: it gates the manual [`Command::Finalize`] here *and*
/// the runtime's auto-official driver (`spawn_auto_official_driver` in the app crate, issue #338)
/// — both finalize paths must agree on what "still contested" means, so the definition lives in
/// exactly one place. `pub` for that reuse.
pub fn open_protest_count(events: &[Event], heat: &HeatId) -> usize {
    use std::collections::HashSet;
    // Ruling offsets reversed by a `RulingReversed` (a reversed protest-resolution re-opens it).
    let reversed: HashSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::RulingReversed { target } => Some(target.0),
            _ => None,
        })
        .collect();
    // Filing offsets that have an effective (non-reversed) resolution.
    let resolved: HashSet<u64> = events
        .iter()
        .enumerate()
        .filter_map(|(offset, e)| match e {
            Event::ProtestResolved { target, .. } if !reversed.contains(&(offset as u64)) => {
                Some(target.0)
            }
            _ => None,
        })
        .collect();
    // Filed protests for this heat with no effective resolution are still open.
    events
        .iter()
        .enumerate()
        .filter(|(offset, e)| {
            matches!(e, Event::ProtestFiled { heat: h, .. } if h == heat)
                && !resolved.contains(&(*offset as u64))
        })
        .count()
}

/// Require that `target` names a real [`Event::ProtestFiled`] in the log — the cheap target check
/// for [`Command::ResolveProtest`](crate::control::Command::ResolveProtest). An out-of-range or
/// non-protest offset is [`ErrorCode::BadRequest`]; nothing is appended.
fn require_protest_target(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match events.get(target.0 as usize) {
        Some(Event::ProtestFiled { .. }) => Ok(()),
        Some(_) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("log offset {} is not a filed protest", target.0),
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
                label: None,
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

    /// (a) A legal Stage→Start→SkipCountdown→ForceEnd→Finalize sequence acks ok and appends the
    /// matching `HeatStateChanged` events in order. (The Armed→Running / Running→Unofficial steps
    /// are normally runtime-appended; here the overrides drive them through the command path.)
    #[test]
    fn legal_heat_loop_sequence_appends_transitions_and_acks_ok() {
        let state = scheduled_state();
        let steps = [
            (Command::Stage { heat: heat() }, HeatTransition::Staged),
            (Command::Start { heat: heat() }, HeatTransition::Armed),
            (
                Command::SkipCountdown { heat: heat() },
                HeatTransition::Running,
            ),
            (Command::ForceEnd { heat: heat() }, HeatTransition::Finished),
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
                label: None,
            },
        );
        assert!(ack.ok);
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::HeatScheduled { heat: h, lineup: l, class: None, round: None, frequencies, label: None }
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
                label: None,
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

    /// A `ScheduleHeat` carrying a custom `label` persists it on the emitted `HeatScheduled`
    /// (the build-heat custom-name path); a generator/free-text heat leaves it `None`.
    #[test]
    fn schedule_heat_carries_the_custom_label() {
        let state = AppState::new(InMemoryLog::default());
        let lineup = vec![CompetitorRef("A".into())];
        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: lineup.clone(),
                class: None,
                round: None,
                frequencies: vec![],
                label: Some("Featured Heat".into()),
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::HeatScheduled { heat: h, label: Some(l), .. }
                if *h == heat() && l == "Featured Heat"
        )));
    }

    /// A `ScheduleHeat` seating the same competitor twice is rejected with a typed `BadRequest`
    /// and appends nothing (#335) — a duplicate ref would merge two seats into one lap stream.
    #[test]
    fn schedule_heat_rejects_a_duplicate_competitor_in_the_lineup() {
        let state = AppState::new(InMemoryLog::default());
        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into()), CompetitorRef("A".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
        );
        assert!(!ack.ok, "a duplicate lineup entry must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (events, _) = state.read().unwrap();
        assert!(events.is_empty(), "a rejected ScheduleHeat appends nothing");
    }

    /// `ScheduleHeat` on an id that already exists is rejected (#335 / #341): the fold re-seeds a
    /// repeated `HeatScheduled` back to `Scheduled`, so accepting the duplicate would silently
    /// reset a **Final** heat and orphan its result. The heat must stay Final; a re-run goes
    /// through `Discard`/`Restart`, never a re-schedule.
    #[test]
    fn schedule_heat_rejects_an_existing_heat_id() {
        use gridfpv_engine::heat::{HeatState, heat_state};

        // q-1 driven all the way to Final.
        let state = drive_current_to(&[
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished,
            HeatTransition::Finalized,
        ]);
        let (before, _) = state.read().unwrap();
        assert_eq!(heat_state(&before, &heat()), Some(HeatState::Final));

        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
        );
        assert!(!ack.ok, "re-scheduling an existing id must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);

        // Nothing appended; the heat is still Final — NOT re-seeded to Scheduled.
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len(), "nothing was appended");
        assert_eq!(
            heat_state(&after, &heat()),
            Some(HeatState::Final),
            "the finished heat keeps its state"
        );

        // A merely-Scheduled heat is protected the same way (only genuinely-new ids pass).
        let state = scheduled_state();
        let ack = apply_command(
            &state,
            Command::ScheduleHeat {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
        );
        assert!(!ack.ok, "a scheduled id is not a fresh id either");
    }

    /// `SetCurrentHeat` validates the heat exists, then appends a `CurrentHeatSelected` — and the
    /// live `current_heat` derivation follows it on replay (event-sourced / deterministic).
    #[test]
    fn set_current_heat_validates_appends_and_drives_the_live_current_heat() {
        use crate::live_state::live_state;

        // Two scheduled heats; q-1 is the first (the default current heat before any selection).
        let mut log = InMemoryLog::default();
        for id in ["q-1", "q-2"] {
            EventLog::append(
                &mut log,
                Event::HeatScheduled {
                    heat: HeatId(id.into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        }
        let state = AppState::new(log);

        // Selecting q-2 acks ok and appends the CurrentHeatSelected.
        let ack = apply_command(
            &state,
            Command::SetCurrentHeat {
                heat: HeatId("q-2".into()),
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::CurrentHeatSelected { heat } if *heat == HeatId("q-2".into())
        )));

        // The live state now follows the selection (replay-deterministic).
        assert_eq!(live_state(&events).current_heat, Some(HeatId("q-2".into())));
    }

    /// `SetCurrentHeat` on a heat that was never scheduled is an `UnknownScope` rejection (no append).
    #[test]
    fn set_current_heat_on_unknown_heat_is_rejected() {
        let state = scheduled_state();
        let (before, _) = state.read().unwrap();
        let ack = apply_command(
            &state,
            Command::SetCurrentHeat {
                heat: HeatId("does-not-exist".into()),
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::UnknownScope);
        let (after, _) = state.read().unwrap();
        assert_eq!(
            before.len(),
            after.len(),
            "a rejected select appends nothing"
        );
    }

    /// A log with two scheduled heats (`q-1`, `q-2`); `q-1` is the default current heat.
    fn two_heats_state() -> AppState {
        let mut log = InMemoryLog::default();
        for id in ["q-1", "q-2"] {
            EventLog::append(
                &mut log,
                Event::HeatScheduled {
                    heat: HeatId(id.into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        }
        AppState::new(log)
    }

    /// Drive `q-1` to the given terminal transition through the command path (so the FSM
    /// legality is honoured), then return the state.
    fn drive_current_to(transitions: &[HeatTransition]) -> AppState {
        let state = two_heats_state();
        let commands: &[Command] = &[
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
        ];
        // Map the requested transition path to the matching prefix of the loop commands.
        let steps = match transitions {
            [HeatTransition::Staged] => 1,
            [HeatTransition::Staged, HeatTransition::Armed] => 2,
            [.., HeatTransition::Running] => 3,
            [.., HeatTransition::Finished] => 4,
            [.., HeatTransition::Finalized] => 5,
            _ => panic!("unsupported transition path {transitions:?}"),
        };
        for command in &commands[..steps] {
            let ack = apply_command(&state, command.clone());
            assert!(ack.ok, "driving q-1 failed: {ack:?}");
        }
        state
    }

    /// `SetCurrentHeat` is **rejected** with a typed `BadRequest` while the current heat is in
    /// a committed phase (`Staged`/`Armed`/`Running`) — abort it or finish first. Nothing is
    /// appended.
    #[test]
    fn set_current_heat_is_rejected_while_current_is_staged_armed_or_running() {
        let paths: &[&[HeatTransition]] = &[
            &[HeatTransition::Staged],
            &[HeatTransition::Staged, HeatTransition::Armed],
            &[
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
            ],
        ];
        for path in paths {
            let state = drive_current_to(path);
            let (before, _) = state.read().unwrap();
            let ack = apply_command(
                &state,
                Command::SetCurrentHeat {
                    heat: HeatId("q-2".into()),
                },
            );
            assert!(!ack.ok, "{path:?}: expected rejection, got {ack:?}");
            assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest, "{path:?}");
            let (after, _) = state.read().unwrap();
            assert_eq!(before.len(), after.len(), "{path:?}: nothing appended");
        }
    }

    /// `SetCurrentHeat` is **accepted** when the current heat is `Scheduled`, `Unofficial`, or
    /// `Final` — and when there is no current heat — and the selection replays deterministically.
    #[test]
    fn set_current_heat_is_accepted_when_current_is_idle_or_scored() {
        use crate::live_state::live_state;

        // Scheduled (the default current heat before any transition).
        let state = two_heats_state();
        let ack = apply_command(
            &state,
            Command::SetCurrentHeat {
                heat: HeatId("q-2".into()),
            },
        );
        assert!(ack.ok, "scheduled current must allow a switch: {ack:?}");

        // Unofficial (Running → Finished) and Final (→ Finalized): each allows the switch.
        for path in [
            &[
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ][..],
            &[
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
                HeatTransition::Finalized,
            ][..],
        ] {
            let state = drive_current_to(path);
            let ack = apply_command(
                &state,
                Command::SetCurrentHeat {
                    heat: HeatId("q-2".into()),
                },
            );
            assert!(
                ack.ok,
                "{path:?}: a scored current must allow a switch: {ack:?}"
            );
            let (events, _) = state.read().unwrap();
            // Replay-deterministic: the live current heat follows the accepted selection.
            assert_eq!(
                live_state(&events).current_heat,
                Some(HeatId("q-2".into())),
                "{path:?}"
            );
        }

        // No current heat (empty log): a select still only fails the existence check, not the lock.
        let empty = AppState::new(InMemoryLog::default());
        let ack = apply_command(
            &empty,
            Command::SetCurrentHeat {
                heat: HeatId("q-1".into()),
            },
        );
        // The heat does not exist, so this is UnknownScope — *not* the BadRequest lock.
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::UnknownScope);
    }

    /// After aborting a staged current heat back to `Scheduled`, the picker is free again — the
    /// switch is accepted (the abort-to-switch path).
    #[test]
    fn set_current_heat_is_accepted_after_abort_back_to_scheduled() {
        let state = drive_current_to(&[HeatTransition::Staged]);
        // While staged it is locked.
        let ack = apply_command(
            &state,
            Command::SetCurrentHeat {
                heat: HeatId("q-2".into()),
            },
        );
        assert!(!ack.ok, "staged current is locked");

        // Abort q-1 back to Scheduled, then the switch is accepted.
        let ack = apply_command(
            &state,
            Command::Abort {
                heat: HeatId("q-1".into()),
            },
        );
        assert!(ack.ok, "abort failed: {ack:?}");
        let ack = apply_command(
            &state,
            Command::SetCurrentHeat {
                heat: HeatId("q-2".into()),
            },
        );
        assert!(ack.ok, "after abort the switch must be allowed: {ack:?}");
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
                penalty: penalty.clone(),
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
                label: None,
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

    /// `SplitLap` validates the target is a real pass (the lap's ending pass), then appends
    /// `LapSplit`. A non-pass / out-of-range target is rejected and appends nothing.
    #[test]
    fn split_lap_validates_target_and_appends() {
        let mut log = InMemoryLog::default();
        EventLog::append(&mut log, pass("A", 1_000_000, 1), None).unwrap(); // offset 0: a pass
        EventLog::append(
            &mut log,
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            None,
        )
        .unwrap(); // offset 1: not a pass
        let state = AppState::new(log);

        let at = SourceTime::from_micros(500_000);
        let ack = apply_command(
            &state,
            Command::SplitLap {
                target: LogRef(0),
                at,
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(
            |e| matches!(e, Event::LapSplit { target, at: a } if *target == LogRef(0) && *a == at)
        ));

        // A non-pass target is rejected, nothing appended.
        let (before, _) = state.read().unwrap();
        let ack = apply_command(
            &state,
            Command::SplitLap {
                target: LogRef(1),
                at,
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len());
    }

    /// `ReverseRuling` validates the target is a real `PenaltyApplied`, then appends
    /// `RulingReversed`. A non-penalty / out-of-range target is rejected and appends nothing.
    #[test]
    fn reverse_ruling_validates_penalty_target_and_appends() {
        let state = scheduled_state(); // offset 0: HeatScheduled
        // Apply a penalty so there is a real ruling to reverse (lands at offset 1).
        let ack = apply_command(
            &state,
            Command::ApplyPenalty {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::Disqualify { reason: None },
            },
        );
        assert!(ack.ok, "got {ack:?}");

        // Reversing the penalty at offset 1 succeeds and appends `RulingReversed`.
        let ack = apply_command(&state, Command::ReverseRuling { target: LogRef(1) });
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::RulingReversed { target } if *target == LogRef(1)))
        );

        // Reversing a non-penalty (the HeatScheduled at offset 0) is rejected, nothing appended.
        let (before, _) = state.read().unwrap();
        let ack = apply_command(&state, Command::ReverseRuling { target: LogRef(0) });
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        // And an out-of-range target is rejected too.
        let ack = apply_command(
            &state,
            Command::ReverseRuling {
                target: LogRef(999),
            },
        );
        assert!(!ack.ok);
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len());
    }

    // --- Slice 6 adjudication commands ---------------------------------------------------------

    /// `DeductPoints` appends a `PenaltyApplied { PointsDeducted }` for the competitor.
    #[test]
    fn deduct_points_appends_a_points_penalty() {
        let state = scheduled_state();
        let ack = apply_command(
            &state,
            Command::DeductPoints {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                points: 5,
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::PenaltyApplied { competitor, penalty: Penalty::PointsDeducted { points }, .. }
                if *competitor == CompetitorRef("A".into()) && *points == 5
        )));
    }

    /// `ThrowOutLap` validates the target is a real pass, then appends `LapThrownOut`. A non-pass
    /// or out-of-range target is rejected and appends nothing.
    #[test]
    fn throw_out_lap_validates_pass_target_and_appends() {
        let mut log = InMemoryLog::default();
        EventLog::append(&mut log, pass("A", 1_000_000, 1), None).unwrap(); // offset 0: a pass
        EventLog::append(
            &mut log,
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            None,
        )
        .unwrap(); // offset 1: not a pass
        let state = AppState::new(log);

        let ack = apply_command(&state, Command::ThrowOutLap { target: LogRef(0) });
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::LapThrownOut { target } if *target == LogRef(0)))
        );

        // A non-pass target (the HeatScheduled at offset 1) is rejected, nothing appended.
        let (before, _) = state.read().unwrap();
        let ack = apply_command(&state, Command::ThrowOutLap { target: LogRef(1) });
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len());
    }

    /// A throw-out may target an *inserted* lap (whose `end_ref` is the `LapInserted` event's
    /// offset, not a raw `Pass`) — `require_lap_end_target` accepts it.
    #[test]
    fn throw_out_lap_accepts_an_inserted_lap_target() {
        let mut log = InMemoryLog::default();
        EventLog::append(
            &mut log,
            Event::LapInserted {
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(3_000_000),
                heat: None,
            },
            None,
        )
        .unwrap(); // offset 0: an inserted lap (a synthetic lap end)
        let state = AppState::new(log);

        let ack = apply_command(&state, Command::ThrowOutLap { target: LogRef(0) });
        assert!(ack.ok, "an inserted lap must be throw-out-able: {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::LapThrownOut { target } if *target == LogRef(0)))
        );
    }

    /// `FileProtest` then `ResolveProtest` append the protest pair; resolving validates the target
    /// is a real `ProtestFiled`, and a non-protest / out-of-range target is rejected.
    #[test]
    fn file_then_resolve_protest_appends_the_pair() {
        use gridfpv_events::ProtestOutcome;
        let state = scheduled_state(); // offset 0: HeatScheduled

        let ack = apply_command(
            &state,
            Command::FileProtest {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                note: "cut the course".into(),
            },
        );
        assert!(ack.ok, "got {ack:?}"); // ProtestFiled lands at offset 1
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ProtestFiled { competitor, note, .. }
                if *competitor == CompetitorRef("A".into()) && note == "cut the course"
        )));

        // Resolve the protest at offset 1.
        let ack = apply_command(
            &state,
            Command::ResolveProtest {
                target: LogRef(1),
                outcome: ProtestOutcome::Upheld,
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ProtestResolved { target, outcome: ProtestOutcome::Upheld }
                if *target == LogRef(1)
        )));

        // Resolving a non-protest (the HeatScheduled at offset 0) is rejected, nothing appended.
        let (before, _) = state.read().unwrap();
        let ack = apply_command(
            &state,
            Command::ResolveProtest {
                target: LogRef(0),
                outcome: ProtestOutcome::Denied,
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (after, _) = state.read().unwrap();
        assert_eq!(before.len(), after.len());
    }

    /// P1-4: `Finalize` is **gated on open protests** — a heat with a filed, unresolved protest
    /// cannot be finalized (rejected, appends nothing); once the protest is resolved, finalize is
    /// allowed.
    #[test]
    fn finalize_is_gated_on_open_protests() {
        use gridfpv_events::ProtestOutcome;
        let state = scheduled_state(); // offset 0: HeatScheduled q-1
        // Drive q-1 to Unofficial (finalizable).
        for cmd in [
            Command::Stage { heat: heat() },
            Command::Start { heat: heat() },
            Command::SkipCountdown { heat: heat() },
            Command::ForceEnd { heat: heat() },
        ] {
            assert!(apply_command(&state, cmd).ok);
        }

        // File a protest against the heat.
        assert!(
            apply_command(
                &state,
                Command::FileProtest {
                    heat: heat(),
                    competitor: CompetitorRef("A".into()),
                    note: "contested".into(),
                },
            )
            .ok
        );
        let (before, _) = state.read().unwrap();
        let filed = before
            .iter()
            .position(|e| matches!(e, Event::ProtestFiled { .. }))
            .expect("protest filed") as u64;

        // Finalize is rejected while the protest is open — and appends nothing.
        let ack = apply_command(&state, Command::Finalize { heat: heat() });
        assert!(!ack.ok, "finalize must be blocked by an open protest");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (after, _) = state.read().unwrap();
        assert_eq!(
            before.len(),
            after.len(),
            "blocked finalize appends nothing"
        );

        // Resolve the protest, then finalize is allowed.
        assert!(
            apply_command(
                &state,
                Command::ResolveProtest {
                    target: LogRef(filed),
                    outcome: ProtestOutcome::Denied,
                },
            )
            .ok
        );
        let ack = apply_command(&state, Command::Finalize { heat: heat() });
        assert!(ack.ok, "after resolution finalize is allowed: {ack:?}");
    }

    /// P1-4 unit: `open_protest_count` — a filed protest is open until an *effective* (non-reversed)
    /// resolution closes it; reversing the resolution re-opens it.
    #[test]
    fn open_protest_count_tracks_resolution_and_reversal() {
        use gridfpv_events::ProtestOutcome;
        let h = heat();
        let filed = Event::ProtestFiled {
            heat: h.clone(),
            competitor: CompetitorRef("A".into()),
            note: "x".into(),
        };
        // Just a filing → open.
        assert_eq!(open_protest_count(std::slice::from_ref(&filed), &h), 1);
        // Filing + resolution → closed.
        let resolved = vec![
            filed.clone(),
            Event::ProtestResolved {
                target: LogRef(0),
                outcome: ProtestOutcome::Denied,
            },
        ];
        assert_eq!(open_protest_count(&resolved, &h), 0);
        // Reversing the resolution (at offset 1) re-opens the protest.
        let mut reversed = resolved.clone();
        reversed.push(Event::RulingReversed { target: LogRef(1) });
        assert_eq!(open_protest_count(&reversed, &h), 1);
        // A protest for a DIFFERENT heat doesn't count.
        assert_eq!(open_protest_count(&[filed], &HeatId("other".into())), 0);
    }

    /// The **generalized** `ReverseRuling` (Slice 6) accepts a throw-out, a protest resolution, and
    /// a heat-void as targets — not just a penalty — and still rejects a non-ruling.
    #[test]
    fn reverse_ruling_accepts_any_ruling_target() {
        let mut log = InMemoryLog::default();
        // offset 0: a pass (so a throw-out has a valid target)
        EventLog::append(&mut log, pass("A", 1_000_000, 1), None).unwrap();
        // offset 1: the heat
        EventLog::append(
            &mut log,
            Event::HeatScheduled {
                heat: heat(),
                lineup: vec![CompetitorRef("A".into())],
                class: None,
                round: None,
                frequencies: vec![],
                label: None,
            },
            None,
        )
        .unwrap();
        let state = AppState::new(log);

        // Append a throw-out (offset 2), a heat-void (offset 3) — both reversible rulings.
        assert!(apply_command(&state, Command::ThrowOutLap { target: LogRef(0) }).ok);
        assert!(apply_command(&state, Command::VoidHeat { heat: heat() }).ok);

        // Reversing the throw-out (offset 2) and the heat-void (offset 3) both succeed.
        for target in [LogRef(2), LogRef(3)] {
            let ack = apply_command(&state, Command::ReverseRuling { target });
            assert!(ack.ok, "reversing ruling at {target:?} failed: {ack:?}");
        }
        // But reversing the pass at offset 0 (not a ruling) is rejected.
        let ack = apply_command(&state, Command::ReverseRuling { target: LogRef(0) });
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
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
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    // Best-lap only ranks, so a scored round needs a race time to end (validation).
                    time_limit_secs: Some(60),
                    // Per-heat: this test asserts the whole-field single heat (the bracket path).
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
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
                mode: FillMode::Next,
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
                mode: FillMode::Next,
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
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    // Best-lap only ranks, so a scored round needs a race time to end (validation).
                    time_limit_secs: Some(60),
                    // Per-heat: this test asserts first-fit channel assignment from the timer pool.
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                },
            )
            .unwrap();
        (registry, EventId(event.0.clone()), round.id)
    }

    /// `FillRound` assigns channels from the event's selected timer onto the heat — the lineup gets
    /// the **IMD-best** Raceband subset for its simultaneous size (#209 auto-pick), laid onto the
    /// seeds in order (race redesign Slice 4a + #209).
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
                mode: FillMode::Next,
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
        // #209: the two seeds get the IMD-cleanest 2-channel Raceband subset — the widest-spread
        // pair R1 (5658) and R8 (5917) — in seed order (lowest channel → top seed), not the naive
        // first-fit R1, R2.
        assert_eq!(freqs.len(), 2);
        assert_eq!(freqs[0].1, 5658);
        assert_eq!(freqs[1].1, 5917);
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
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round,
                mode: FillMode::Next,
            },
        );
        assert!(!ack.ok, "an oversized heat must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let after = state.read().unwrap().0.len();
        assert_eq!(before, after, "a rejected FillRound appends nothing");
    }

    // --- #335: ScheduleHeat tag + membership validation (the event-aware path) ---------------

    /// An 8-node event over a class of `pilots` with a single timed_qual round — the tagged
    /// ScheduleHeat fixture. Returns the registry, event id, round id, class id, and the member
    /// pilot ids (in membership order).
    #[cfg(test)]
    fn tagged_schedule_fixture(
        pilots: &[&str],
    ) -> (
        EventRegistry,
        EventId,
        gridfpv_events::RoundId,
        gridfpv_events::ClassId,
        Vec<gridfpv_events::PilotId>,
    ) {
        use crate::timers::{ChannelCapability, CreateTimerRequest, TimerKind};
        let timer_req = CreateTimerRequest {
            name: "8-node".into(),
            kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
            channel_capability: Some(ChannelCapability::Flexible),
            node_count: Some(8),
            available_channels: Some(crate::channels::RACEBAND_MHZ.to_vec()),
        };
        let (registry, event_id, round) = event_with_timer_and_round(timer_req, pilots);
        let meta = registry.meta_of(&event_id).unwrap();
        let class = meta.classes[0].clone();
        let members = meta.classes_membership[0]
            .pilots
            .iter()
            .map(|s| s.pilot.clone())
            .collect();
        (registry, event_id, round, class, members)
    }

    /// The tagged ScheduleHeat under test, with the fixture's default shape (no explicit
    /// frequencies, no label) — each test varies the tag/lineup it cares about.
    #[cfg(test)]
    fn schedule_tagged(
        registry: &EventRegistry,
        event_id: &EventId,
        state: &AppState,
        heat: &str,
        lineup: Vec<CompetitorRef>,
        class: Option<gridfpv_events::ClassId>,
        round: Option<gridfpv_events::RoundId>,
    ) -> CommandAck {
        apply_command_in_event(
            registry,
            event_id,
            state,
            Command::ScheduleHeat {
                heat: HeatId(heat.into()),
                lineup,
                class,
                round,
                frequencies: vec![],
                label: None,
            },
        )
    }

    /// A `round` tag must name one of the event's rounds (#335) — a dangling tag would create a
    /// heat no round view lists and no generator accounts for.
    #[test]
    fn schedule_heat_rejects_an_unknown_round_tag() {
        use gridfpv_events::RoundId;
        let (registry, event_id, _round, _class, members) = tagged_schedule_fixture(&["a", "b"]);
        let state = registry.resolve(&event_id).unwrap();
        let lineup = vec![CompetitorRef(members[0].0.clone())];
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-1",
            lineup,
            None,
            Some(RoundId("nope".into())),
        );
        assert!(!ack.ok, "a dangling round tag must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (events, _) = state.read().unwrap();
        assert!(events.is_empty(), "a rejected ScheduleHeat appends nothing");
    }

    /// A `class` tag must be one the event **selects** (#335) — mirroring the membership PUT's
    /// class-selection guard (#330).
    #[test]
    fn schedule_heat_rejects_an_unselected_class_tag() {
        use gridfpv_events::ClassId;
        let (registry, event_id, _round, _class, members) = tagged_schedule_fixture(&["a", "b"]);
        let state = registry.resolve(&event_id).unwrap();
        let lineup = vec![CompetitorRef(members[0].0.clone())];
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-1",
            lineup,
            Some(ClassId("nope".into())),
            None,
        );
        assert!(!ack.ok, "an unselected class tag must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
    }

    /// With BOTH tags, the class must be **eligible** for the round (in its `classes`) — a
    /// selected-but-ineligible class would file the heat under a round that never runs it (#335).
    #[test]
    fn schedule_heat_rejects_a_class_not_eligible_for_the_round() {
        use crate::classes::CreateClassRequest;
        let (registry, event_id, round, class, members) = tagged_schedule_fixture(&["a", "b"]);
        // A second directory class, selected by the event but NOT eligible for the round.
        let spec = registry
            .classes()
            .create(&CreateClassRequest {
                name: "Spec".into(),
                source: Default::default(),
                reference: None,
                description: None,
            })
            .unwrap()
            .id;
        registry
            .set_classes(&event_id, vec![class, spec.clone()])
            .unwrap();
        let state = registry.resolve(&event_id).unwrap();
        let lineup = vec![CompetitorRef(members[0].0.clone())];
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-1",
            lineup,
            Some(spec),
            Some(round),
        );
        assert!(!ack.ok, "a round-ineligible class tag must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
    }

    /// On a tagged heat, a lineup ref naming a **directory pilot** must be an eligible member —
    /// a real pilot outside the round's classes' membership must not be seated (#335, closing
    /// the manual bypass of FillRound's membership-resolved field).
    #[test]
    fn schedule_heat_rejects_a_non_member_pilot_on_a_tagged_heat() {
        use crate::pilots::CreatePilotRequest;
        let (registry, event_id, round, class, members) = tagged_schedule_fixture(&["a", "b"]);
        // A directory pilot who is NOT in the round's class membership.
        let outsider = registry
            .pilots()
            .create(&CreatePilotRequest {
                callsign: "outsider".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let state = registry.resolve(&event_id).unwrap();
        let lineup = vec![
            CompetitorRef(members[0].0.clone()),
            CompetitorRef(outsider.0.clone()),
        ];
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-1",
            lineup,
            Some(class),
            Some(round),
        );
        assert!(!ack.ok, "a non-member directory pilot must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        let (events, _) = state.read().unwrap();
        assert!(events.is_empty(), "a rejected ScheduleHeat appends nothing");
    }

    /// The happy paths stay open (#335): eligible members schedule tagged; **non-pilot refs**
    /// (`node-{i}` timer seats — the open-practice channel lineup — and sim free-text names)
    /// pass the membership check; and an **untagged** heat validates none of it.
    #[test]
    fn schedule_heat_accepts_members_node_seats_and_untagged_lineups() {
        use crate::pilots::CreatePilotRequest;
        let (registry, event_id, round, class, members) = tagged_schedule_fixture(&["a", "b"]);
        let state = registry.resolve(&event_id).unwrap();

        // Eligible members, tagged with their round + class — the console's build path.
        let lineup: Vec<CompetitorRef> =
            members.iter().map(|p| CompetitorRef(p.0.clone())).collect();
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-1",
            lineup,
            Some(class),
            Some(round.clone()),
        );
        assert!(ack.ok, "eligible members schedule tagged: {ack:?}");

        // A node-seat ref on a tagged heat is NOT membership-checked (the practice-style path).
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-2",
            vec![CompetitorRef("node-0".into())],
            None,
            Some(round),
        );
        assert!(ack.ok, "node-seat refs pass the membership check: {ack:?}");

        // An untagged heat skips tag validation entirely — even for a known non-member pilot
        // (the free-text / ad-hoc path stays as permissive as before).
        let outsider = registry
            .pilots()
            .create(&CreatePilotRequest {
                callsign: "outsider".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let ack = schedule_tagged(
            &registry,
            &event_id,
            &state,
            "x-3",
            vec![CompetitorRef(outsider.0.clone())],
            None,
            None,
        );
        assert!(
            ack.ok,
            "an untagged heat is not membership-checked: {ack:?}"
        );
    }

    /// Build an event with an 8-node Raceband timer over a class of `pilots`, plus a single
    /// **head_to_head** round (`group_size=heat_size`) — a *deterministic* format whose one
    /// generator step emits the whole round's heats (the field split into groups). Returns the
    /// registry, event id, and round id. Used by the fill-all (#216) tests. (Was `round_robin`
    /// before that format was carved out for the primitives-first release; head_to_head has the same
    /// "one step emits the whole round" shape the fill-all tests need.)
    #[cfg(test)]
    fn round_robin_event(
        pilots: &[&str],
        heat_size: u32,
    ) -> (EventRegistry, EventId, gridfpv_events::RoundId) {
        use crate::classes::CreateClassRequest;
        use crate::events::{
            ChannelMode, CreateEventRequest, MemberSlot, NewRoundReq, SeedingRule,
        };
        use crate::pilots::CreatePilotRequest;
        use crate::timers::{ChannelCapability, CreateTimerRequest, TimerKind};
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let registry = EventRegistry::new(None).unwrap();
        let timer = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "8-node".into(),
                kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
                channel_capability: Some(ChannelCapability::Flexible),
                node_count: Some(8),
                available_channels: Some(crate::channels::RACEBAND_MHZ.to_vec()),
            })
            .unwrap();
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
                    label: "Round Robin".into(),
                    classes: vec![class],
                    format: "head_to_head".into(),
                    params: BTreeMap::from([("group_size".into(), heat_size.to_string())]),
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    // Best-lap only ranks, so a scored round needs a race time to end (validation).
                    time_limit_secs: Some(60),
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                },
            )
            .unwrap();
        (registry, EventId(event.0.clone()), round.id)
    }

    /// Count the heats tagged with `round` currently in the log.
    #[cfg(test)]
    fn heats_in_round(state: &AppState, round: &gridfpv_events::RoundId) -> usize {
        let (events, _) = state.read().unwrap();
        events
            .iter()
            .filter(|e| matches!(e, Event::HeatScheduled { round: Some(r), .. } if r == round))
            .count()
    }

    /// `FillRound { mode: All }` (#216) on a deterministic format fills the **whole** round in one
    /// command: a round_robin (`rounds=1`, `heat_size=2`) over 4 pilots partitions the field into
    /// **2 heats**, and one fill-all command schedules both — where the single-step `Next` would
    /// schedule only the first.
    #[test]
    fn fill_round_all_fills_the_whole_deterministic_round() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();

        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.clone(),
                mode: FillMode::All,
            },
        );
        assert!(ack.ok, "FillRound(All) rejected: {ack:?}");
        // 4 pilots at heat_size 2 → 2 heats, both scheduled by the one fill-all command.
        assert_eq!(
            heats_in_round(&state, &round),
            2,
            "fill-all schedules the whole round's heats in one command"
        );
    }

    /// Determinism + idempotency (#216): a second `FillRound { mode: All }` on a round already
    /// filled to its terminal state appends **nothing more** (the generator is deterministic, and
    /// every plan it wants now is already scheduled), and the heats are unchanged.
    #[test]
    fn fill_round_all_is_idempotent_on_a_filled_round() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
        let all = || {
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::FillRound {
                    round: round.clone(),
                    mode: FillMode::All,
                },
            )
        };

        assert!(all().ok);
        let after_first: Vec<_> = {
            let (events, _) = state.read().unwrap();
            events.to_vec()
        };
        assert_eq!(heats_in_round(&state, &round), 2);

        // Re-run: still ok, but nothing new — the log is byte-identical.
        assert!(all().ok, "re-filling a complete round is a typed ok");
        let after_second: Vec<_> = {
            let (events, _) = state.read().unwrap();
            events.to_vec()
        };
        assert_eq!(
            heats_in_round(&state, &round),
            2,
            "a re-run of fill-all adds no heats"
        );
        assert_eq!(
            after_first, after_second,
            "fill-all is idempotent: the second run appends nothing"
        );
    }

    /// P1-8: a `FillMode::All` on an **open-ended `Static` round** (`rounds=0`) is rejected up front
    /// — its generator never reports Complete, so without the guard a fill-all would schedule up to
    /// the 1000-heat cap while acking ok. Here it acks **failed** (BadRequest) and schedules nothing.
    #[test]
    fn fill_round_all_rejects_an_open_ended_static_round() {
        use crate::classes::CreateClassRequest;
        use crate::events::{CreateEventRequest, MemberSlot, NewRoundReq, SeedingRule};
        use crate::pilots::CreatePilotRequest;
        use crate::timers::{ChannelCapability, CreateTimerRequest, TimerKind};
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let registry = EventRegistry::new(None).unwrap();
        let timer = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "8-node".into(),
                kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
                channel_capability: Some(ChannelCapability::Flexible),
                node_count: Some(8),
                available_channels: Some(crate::channels::RACEBAND_MHZ.to_vec()),
            })
            .unwrap();
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
        let pilots: Vec<_> = ["alpha", "bravo"]
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
        let channels = [5658u16, 5695u16];
        registry
            .set_class_membership(
                &event,
                class.clone(),
                pilots
                    .into_iter()
                    .zip(channels)
                    .map(|(pilot, ch)| MemberSlot {
                        pilot,
                        channel: Some(ch),
                    })
                    .collect(),
            )
            .unwrap();
        registry.set_timers(&event, vec![timer.id]).unwrap();
        // A `timed_qual` round defaults to `ChannelMode::Static`; `rounds=0` makes it open-ended.
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    label: "Time Trials".into(),
                    classes: vec![class],
                    format: "timed_qual".into(),
                    params: BTreeMap::from([("rounds".into(), "0".into())]),
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(60),
                    channel_mode: None,
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                },
            )
            .unwrap();
        let event_id = EventId(event.0.clone());
        let state = registry.resolve(&event_id).unwrap();

        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.id.clone(),
                mode: FillMode::All,
            },
        );
        assert!(
            !ack.ok,
            "fill-all on an open-ended static round must be rejected"
        );
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        assert_eq!(
            heats_in_round(&state, &round.id),
            0,
            "the rejected fill-all scheduled NO heats (not 1000)"
        );
    }

    /// Open practice still **single-steps** (#216): its one channel heat is one draw, so a `Next`
    /// fill schedules exactly that heat — fill-all is not used for the (dynamic) practice format.
    /// (Open practice rounds also auto-create their heat on creation; here we drive `Next` against
    /// a fresh round to assert the single-step path appends exactly one heat.)
    #[test]
    fn open_practice_fills_a_single_heat_per_step() {
        use crate::events::{CreateEventRequest, NewRoundReq, SeedingRule};
        use std::collections::BTreeMap;

        let registry = EventRegistry::new(None).unwrap();
        let event = registry
            .create(&CreateEventRequest {
                name: "Practice".into(),
                date: None,
                location: None,
                description: None,
                organizer: None,
            })
            .unwrap()
            .id;
        // An open-practice round: channel-seated (`AllChannels` seeding), no class membership.
        // Its single channel heat is one draw — the (dynamic) format the RD single-steps, never
        // fill-all.
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    label: "Open Practice".into(),
                    classes: vec![],
                    format: "open_practice".into(),
                    params: BTreeMap::new(),
                    win_condition: None,
                    seeding: SeedingRule::AllChannels {
                        channels: vec![0, 1, 2],
                    },
                    time_limit_secs: None,
                    channel_mode: None,
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                },
            )
            .unwrap();
        let event_id = EventId(event.0.clone());
        let state = registry.resolve(&event_id).unwrap();
        let next = || {
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::FillRound {
                    round: round.id.clone(),
                    mode: FillMode::Next,
                },
            )
        };

        // The registry `add_round` does not auto-fill (that is the HTTP handler's job), so the
        // round starts empty. The single-step Next schedules exactly the one channel heat.
        assert_eq!(heats_in_round(&state, &round.id), 0);
        assert!(next().ok, "open-practice Next rejected");
        assert_eq!(
            heats_in_round(&state, &round.id),
            1,
            "a single-step fill schedules open practice's one channel heat"
        );

        // It does not multiply: a second Next is idempotent (the heat is already scheduled).
        assert!(next().ok);
        assert_eq!(
            heats_in_round(&state, &round.id),
            1,
            "open practice stays at its single heat — single-stepping it never adds more"
        );
    }

    // --- Advance loads the next heat (fix: Advance was a no-op on Live control) ---------------

    /// The current heat the live projection is focused on, for the Advance tests to assert against.
    #[cfg(test)]
    fn current_heat_of(state: &AppState) -> Option<HeatId> {
        let (events, _) = state.read().unwrap();
        crate::live_state::live_state(&events).current_heat
    }

    /// Drive `heat` from `Scheduled` all the way to `Final` through the control path (the runtime
    /// override transitions stand in for the auto-clock so a test can finalize without timers).
    #[cfg(test)]
    fn drive_heat_to_final(
        registry: &EventRegistry,
        event_id: &EventId,
        state: &AppState,
        heat: &HeatId,
    ) {
        for cmd in [
            Command::Stage { heat: heat.clone() },
            Command::Start { heat: heat.clone() },
            Command::SkipCountdown { heat: heat.clone() },
            Command::ForceEnd { heat: heat.clone() },
            Command::Finalize { heat: heat.clone() },
        ] {
            let ack = apply_command_in_event(registry, event_id, state, cmd);
            assert!(ack.ok, "driving {heat:?} to Final: {ack:?}");
        }
        assert_eq!(
            heat_state(state, heat),
            Some(gridfpv_engine::heat::HeatState::Final),
            "{heat:?} should be Final before Advance"
        );
    }

    /// Fold a single heat's current state through the control path (test helper).
    #[cfg(test)]
    fn heat_state(state: &AppState, heat: &HeatId) -> Option<gridfpv_engine::heat::HeatState> {
        let (events, _) = state.read().unwrap();
        gridfpv_engine::heat::heat_state(&events, heat)
    }

    /// The heat ids tagged with `round`, in first-scheduled order (test helper).
    #[cfg(test)]
    fn heat_ids_in_round(state: &AppState, round: &gridfpv_events::RoundId) -> Vec<HeatId> {
        let (events, _) = state.read().unwrap();
        let mut out = Vec::new();
        for e in &events {
            if let Event::HeatScheduled {
                heat,
                round: Some(r),
                ..
            } = e
            {
                if r == round && !out.contains(heat) {
                    out.push(heat.clone());
                }
            }
        }
        out
    }

    /// The core fix: with the round's heats already scheduled, finalizing heat 1 and then
    /// `Advance`-ing it **loads heat 2** into Live control (the on-deck case). Before the fix
    /// Advance only recorded the `Advanced` transition and `current_heat` stayed on heat 1.
    #[test]
    fn advance_loads_the_next_already_scheduled_heat() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();

        // Fill the whole round up front: 4 pilots at heat_size 2 → 2 scheduled heats.
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::FillRound {
                    round: round.clone(),
                    mode: FillMode::All,
                },
            )
            .ok
        );
        let heats = heat_ids_in_round(&state, &round);
        assert_eq!(heats.len(), 2, "the round has two scheduled heats");
        let (heat1, heat2) = (heats[0].clone(), heats[1].clone());

        // Heat 1 is the current heat (first scheduled); drive it to Final, then Advance.
        drive_heat_to_final(&registry, &event_id, &state, &heat1);
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Advance {
                heat: heat1.clone(),
            },
        );
        assert!(ack.ok, "Advance rejected: {ack:?}");

        // Live control now follows to heat 2 (Scheduled, ready to Stage); heat 1 is Advanced/Final.
        assert_eq!(
            current_heat_of(&state),
            Some(heat2.clone()),
            "Advance loaded the next heat"
        );
        assert_eq!(
            heat_state(&state, &heat2),
            Some(gridfpv_engine::heat::HeatState::Scheduled),
            "the loaded heat is ready to Stage"
        );
        assert_eq!(
            heat_state(&state, &heat1),
            Some(gridfpv_engine::heat::HeatState::Final),
            "the advanced heat stays Final (terminal)"
        );
    }

    /// The generate-if-needed case: only heat 1 is pre-scheduled. Advancing it draws the round's
    /// next heat from the generator and selects it — no manual fill in between.
    #[test]
    fn advance_generates_and_loads_the_next_heat_when_none_is_on_deck() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();

        // Single-step fill: only heat 1 exists; heat 2 is *not* yet scheduled.
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::FillRound {
                    round: round.clone(),
                    mode: FillMode::Next,
                },
            )
            .ok
        );
        assert_eq!(
            heat_ids_in_round(&state, &round).len(),
            1,
            "only heat 1 yet"
        );
        let heat1 = heat_ids_in_round(&state, &round)[0].clone();

        drive_heat_to_final(&registry, &event_id, &state, &heat1);
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Advance {
                heat: heat1.clone(),
            },
        );
        assert!(ack.ok, "Advance rejected: {ack:?}");

        // Advance generated heat 2 and loaded it.
        let heats = heat_ids_in_round(&state, &round);
        assert_eq!(
            heats.len(),
            2,
            "Advance drew the next heat from the generator"
        );
        let heat2 = heats[1].clone();
        assert_eq!(
            current_heat_of(&state),
            Some(heat2.clone()),
            "Advance loaded the generated heat"
        );
        assert_eq!(
            heat_state(&state, &heat2),
            Some(gridfpv_engine::heat::HeatState::Scheduled)
        );
    }

    /// The round-complete case: advancing the *last* heat of a finished round leaves it Advanced
    /// (a clean terminal) — no next heat, no crash, and Live control stays on the advanced heat.
    #[test]
    fn advance_on_the_last_heat_of_a_complete_round_stays_advanced() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();

        // Fill the whole round, then drive BOTH heats to Final so the round is genuinely complete.
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::FillRound {
                    round: round.clone(),
                    mode: FillMode::All,
                },
            )
            .ok
        );
        let heats = heat_ids_in_round(&state, &round);
        assert_eq!(heats.len(), 2);
        let (heat1, heat2) = (heats[0].clone(), heats[1].clone());

        // Advance heat 1 to load heat 2, then finalize heat 2 and Advance it: nothing is left.
        drive_heat_to_final(&registry, &event_id, &state, &heat1);
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::Advance {
                    heat: heat1.clone()
                },
            )
            .ok
        );
        drive_heat_to_final(&registry, &event_id, &state, &heat2);
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Advance {
                heat: heat2.clone(),
            },
        );
        assert!(
            ack.ok,
            "Advance on the last heat is still a typed ok: {ack:?}"
        );

        // No new heat was scheduled; heat 2 stays Final/Advanced and remains the current heat.
        assert_eq!(
            heat_ids_in_round(&state, &round).len(),
            2,
            "a complete round draws no further heats"
        );
        assert_eq!(
            heat_state(&state, &heat2),
            Some(gridfpv_engine::heat::HeatState::Final),
            "the last advanced heat stays Final"
        );
        assert_eq!(
            current_heat_of(&state),
            Some(heat2),
            "with nothing to advance to, Live control stays on the advanced heat"
        );
    }

    /// `Advance` is replay-deterministic: the log it produces (transition + generated heat +
    /// selection) re-folds to the same live state every time, so a recorded session replays
    /// identically. (The fixture ids are random per build, so we assert on the *replayed log*, not
    /// on two independently-seeded fixtures.)
    #[test]
    fn advance_is_deterministic_on_replay() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
        apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.clone(),
                mode: FillMode::Next,
            },
        );
        let heat1 = heat_ids_in_round(&state, &round)[0].clone();
        drive_heat_to_final(&registry, &event_id, &state, &heat1);
        apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Advance { heat: heat1 },
        );

        // Re-folding the same recorded log twice yields the same live state (same current heat).
        let (events, _) = state.read().unwrap();
        assert_eq!(
            crate::live_state::live_state(&events),
            crate::live_state::live_state(&events),
            "Advance's log re-folds deterministically",
        );
        // And the generated next heat is the one loaded.
        let heat2 = heat_ids_in_round(&state, &round)[1].clone();
        assert_eq!(
            crate::live_state::live_state(&events).current_heat,
            Some(heat2),
        );
    }

    /// `Advance` on a heat that is not `Final` (here: never finalized) is rejected verbatim by the
    /// engine's legality and appends nothing — the next-heat load only runs after a legal Advance.
    #[test]
    fn advance_on_a_non_final_heat_is_rejected_and_loads_nothing() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
        apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.clone(),
                mode: FillMode::All,
            },
        );
        let heats = heat_ids_in_round(&state, &round);
        let heat1 = heats[0].clone();
        let before = state.read().unwrap().0.len();

        // Heat 1 is still Scheduled — Advance is illegal.
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Advance { heat: heat1 },
        );
        assert!(!ack.ok, "Advance on a non-Final heat must be rejected");
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
        assert_eq!(
            state.read().unwrap().0.len(),
            before,
            "a rejected Advance appends nothing (no transition, no selection)"
        );
    }

    /// Wire-compat (#216): an older `FillRound` payload with **no `mode`** deserializes to the
    /// single-step default (`Next`), so existing clients keep working unchanged.
    #[test]
    fn fill_round_without_mode_defaults_to_next() {
        let cmd: Command =
            serde_json::from_str(r#"{ "FillRound": { "round": "qual-r1" } }"#).unwrap();
        assert_eq!(
            cmd,
            Command::FillRound {
                round: gridfpv_events::RoundId("qual-r1".into()),
                mode: FillMode::Next,
            }
        );
    }
}
