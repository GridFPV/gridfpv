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
//! | [`Command::VoidDetection`] | the `target` offset exists and is a lap-gate pass (raw [`Pass`](gridfpv_events::Pass), or a synthetic `LapInserted`/`LapSplit`); the target's owning heat is not **Final** | [`Event::DetectionVoided`] |
//! | [`Command::AdjustLap`] | the `target` offset exists and is a lap-gate pass (raw or synthetic, as above); the target's owning heat is not **Final** | [`Event::LapAdjusted`] |
//! | [`Command::InsertLap`] | a tagged insert names a scheduled heat that is not **Final**; an untagged (legacy) one requires the positionally-active heat at the log tail to not be **Final** | [`Event::LapInserted`] |
//! | [`Command::SplitLap`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass) (the lap's ending pass); the target's owning heat is not **Final** | [`Event::LapSplit`] |
//! | [`Command::VoidHeat`] | the heat exists in the log and is not **Final** | [`Event::HeatVoided`] |
//! | [`Command::ApplyPenalty`] | the heat exists in the log and is not **Final** | [`Event::PenaltyApplied`] |
//! | [`Command::DeductPoints`] | the heat exists in the log and is not **Final** | [`Event::PenaltyApplied`] (`PointsDeducted`) |
//! | [`Command::ThrowOutLap`] | the `target` offset exists and is a [`Pass`](gridfpv_events::Pass); the target's owning heat is not **Final** | [`Event::LapThrownOut`] |
//! | [`Command::FileProtest`] | the heat exists in the log (**allowed on a Final heat** — filing changes no result) | [`Event::ProtestFiled`] |
//! | [`Command::ResolveProtest`] | the `target` offset exists and is a [`Event::ProtestFiled`] (**allowed on a Final heat** — resolving changes no result) | [`Event::ProtestResolved`] |
//! | [`Command::ReverseRuling`] | the `target` offset exists and is a reversible ruling (penalty / throw-out / protest resolution / heat-void); the target's owning heat is not **Final** | [`Event::RulingReversed`] |
//!
//! ## An official (Final) result is locked — Revert to marshal
//!
//! Every **result-changing** marshaling command above is additionally gated on the marshaled
//! heat's folded state ([`heat::heat_state`], the same fold the FSM checks use) not being
//! [`Final`](gridfpv_engine::heat::HeatState::Final): an official result must not silently
//! re-score under a ruling — the RD `Revert`s it first (the sanctioned re-open), marshals, and
//! re-finalizes. Heat-addressed commands check their own heat ([`require_not_final`]);
//! target-addressed ones resolve the target's **owning heat** first ([`heat_of_offset`] —
//! by tag, by ruling-chain recursion, or positionally) and check that. The heat-loop
//! transitions themselves (`Revert`/`Discard`/`Restart`, …) are untouched. **Protests are
//! exempt**: filing and resolving a protest changes no result, so both stay legal on a Final
//! heat (an upheld protest is then acted on via Revert, where the open-protest Finalize gate
//! composes correctly).
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
use gridfpv_events::{Event, HeatId, HeatTransition, LogRef};

use crate::app::{AppState, resolve_event};
use crate::control::{
    AdvanceOutcome, AdvanceStop, Command, CommandAck, CommandOutcome, FillMode, FillRoundOutcome,
    FillStop, ScheduledHeat,
};
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
        // `Start` is the **arm** (it opens the gate to detections). It is the last moment Grid can
        // refuse before RotorHazard is driving a live race, so it carries the GridFPV-plugin
        // backstop (#405) — see [`refuse_arm_without_plugin`].
        Command::Start { heat } => match refuse_arm_without_plugin(registry, event_id) {
            Some(err) => CommandAck::failed(err),
            None => apply_command(state, Command::Start { heat }),
        },
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
        // #117 S3: both need the event meta (the layouts, the round that names them) and the
        // timer registry (the enabled node set a layout lays channels onto), so neither can be
        // answered from the log alone.
        Command::SetHeatLayout { heat, layout } => {
            apply_set_heat_layout(registry, event_id, state, heat, layout)
        }
        Command::OverrideHeatSeating {
            heat,
            lineup,
            frequencies,
        } => apply_override_heat_seating(registry, event_id, state, heat, lineup, frequencies),
        other => apply_command(state, other),
    }
}

/// The **arm-time GridFPV-plugin backstop** (#405): refuse to arm a heat when the event races a
/// RotorHazard timer whose plugin is no longer [`Present`](crate::timers::PluginPresence::Present).
///
/// Selection is the primary gate (`PUT /events/{event_id}/timers` refuses a plugin-less RH timer),
/// but a plugin can disappear *after* a valid selection — the RD restarts RotorHazard without it,
/// or it fails to load on boot — and a pre-existing event may have been persisted with one already
/// selected. Selection was legitimate when it was made, so the refusal has to move to the last
/// point before Grid commits: the arm.
///
/// **Every selected RotorHazard timer is checked, not just the effective primary.** Alternates are
/// hot standby that take over on a primary drop (#112), so an alternate with no plugin is a race
/// Grid would silently fall into conducting without one — exactly the #403 class of failure.
///
/// Mock timers are never checked (the requirement is RotorHazard-specific), and an event with no
/// resolvable timers is left alone. Returns the typed `400` to ack with, or `None` to proceed.
fn refuse_arm_without_plugin(
    registry: &EventRegistry,
    event_id: &EventId,
) -> Option<ProtocolError> {
    let meta = registry.meta_of(event_id)?;
    let timers = registry.timers();
    for id in &meta.timers {
        // An id that no longer resolves is a stale selection, not a plugin problem — the event
        // simply has one fewer source. Skip it rather than blocking the race on it.
        let Some(timer) = timers.get(id) else {
            continue;
        };
        if let Some(refusal) = timer.selection_refusal() {
            return Some(ProtocolError::new(
                ErrorCode::BadRequest,
                refusal.arm_message(&timer.name),
            ));
        }
    }
    None
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

    // Validate→append under the command serialization lock (see `apply_command`): two
    // concurrent ScheduleHeats with one id must not both pass the fresh-id check.
    let _guard = state.command_guard();
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
        // #117 S3: `ScheduleHeat` is the free-text / manual path — it names no round, so there is
        // no round default layout to apply, and any layout binding for the heat is set afterwards
        // through `SetHeatLayout`. Assign from the timer's allowed set, as before.
        match round_engine::assign_for_event(&meta, &registry.timers(), None, &lineup) {
            Ok(freqs) => freqs,
            Err(err) => {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    err.to_string(),
                ));
            }
        }
    } else {
        // A caller-supplied assignment still must fit the timer's **enabled** node set (#412):
        // the cap is how many seats the RD has left switched on, not how wide the timer is.
        if let Some(timer) = round_engine::assignment_timer(&meta, &registry.timers()) {
            if lineup.len() > timer.seat_capacity() {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    round_engine::AssignError::TooManyForNodes {
                        lineup: lineup.len(),
                        nodes: timer.seat_capacity(),
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
    // An empty lineup is a heat nobody can fly — stageable but unraceable (raw-API guard).
    if lineup.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "a heat needs at least one competitor in its lineup",
        ));
    }
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
///
/// Both non-error arms carry **what happened**, not just that something did: the caller turns
/// them into the ack's [`FillRoundOutcome`] so a no-op fill is distinguishable from a productive
/// one from the response alone (#395).
enum FillStep {
    /// A heat was appended — re-fold and draw the next (the `All` loop continues here). Carries
    /// the heat it scheduled, named, for the outcome.
    Appended(ScheduledHeat),
    /// Nothing to append now; the round is done for this command. Carries **why** — finished,
    /// awaiting an outstanding heat's result, or refused for this field (#394) — plus the
    /// refusal's reason when there is one.
    Terminal(FillStop, Option<String>),
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
            layout,
            field_draw,
        }) => {
            let class = round_engine::round_class(meta, round);
            // Channel assignment differs by the round's channel mode (race redesign Slice 7a):
            //
            // - **Static** (`static_freqs` is `Some`): the channel-balanced builder already chose
            //   each pilot's fixed membership channel; use them directly. The heat-size cap was
            //   honoured by the builder (heats are ≤ the enabled node set), but re-check defensively
            //   against the timer so an oversized heat never slips through.
            // - **Per-heat** (`static_freqs` is `None`): first-fit from the timer's pool (Slice 4a),
            //   which also enforces the enabled-node cap.
            let frequencies = match static_freqs {
                Some(freqs) => {
                    if let Some(timer) = round_engine::assignment_timer(meta, &registry.timers()) {
                        if lineup.len() > timer.seat_capacity() {
                            return FillStep::Failed(CommandAck::failed(ProtocolError::new(
                                ErrorCode::BadRequest,
                                round_engine::AssignError::TooManyForNodes {
                                    lineup: lineup.len(),
                                    nodes: timer.seat_capacity(),
                                }
                                .to_string(),
                            )));
                        }
                    }
                    freqs
                }
                // No layout and no static channels: first-fit from the timer's allowed set.
                // A heat that *does* fly a layout arrives here with `static_freqs: Some(..)`
                // already resolved from it by `fill_round` — the layout IS the assignment.
                None => {
                    match round_engine::assign_for_event(meta, &registry.timers(), None, &lineup) {
                        Ok(freqs) => freqs,
                        Err(err) => {
                            return FillStep::Failed(CommandAck::failed(ProtocolError::new(
                                ErrorCode::BadRequest,
                                err.to_string(),
                            )));
                        }
                    }
                }
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
            // The heat's FRIENDLY name for the ack (repo display rule: a raw heat id must never
            // reach a user). Resolved against the pre-append log, where the new heat is the next
            // position in the round — exactly the "‹Round› Heat N" the console will show.
            let name = match meta.rounds.iter().find(|def| &def.id == round) {
                Some(def) => round_engine::heat_display_name(def, &events, &heat),
                None => heat.0.clone(),
            };
            let scheduled = ScheduledHeat {
                heat: heat.clone(),
                name,
                lineup: lineup.clone(),
                frequencies: frequencies.clone(),
            };
            // #117 S3: record WHICH LAYOUT this heat flies, before the schedule that carries the
            // channels it produced. Appending it makes the answer a fact about the heat rather than
            // something re-derived later from a round whose default may since have changed — so a
            // heat that has raced keeps not just its channels but the name of the tuning it raced
            // on. Only when there is one: a round naming no layouts logs nothing new.
            if layout.is_some() {
                if let Err(err) = state.append(
                    Event::HeatLayoutSet {
                        heat: heat.clone(),
                        layout,
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
                Ok(_offset) => FillStep::Appended(scheduled),
                Err(err) => FillStep::Failed(CommandAck::failed(err)),
            }
        }
        // Nothing to append — three distinct successful terminal states, kept distinct all the
        // way to the wire (#395): the round is finished, it is waiting on an outstanding heat's
        // result, or its format refuses this field entirely (#394).
        Ok(FillOutcome::Complete) => FillStep::Terminal(FillStop::Complete, None),
        Ok(FillOutcome::AlreadyScheduled) => FillStep::Terminal(FillStop::AwaitingResult, None),
        Ok(FillOutcome::Blocked { reason }) => FillStep::Terminal(FillStop::Blocked, Some(reason)),
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
    // The whole read-draw-append loop runs under the command serialization lock (see
    // `apply_command`): a concurrent fill/schedule must not interleave with the duplicate-id
    // and round-state reads this fill bases its draws on.
    let _guard = state.command_guard();
    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };

    // The round's FRIENDLY label frames every message this command produces (repo display rule);
    // a round id only appears if the round somehow is not in meta, which the fill itself then
    // rejects as `UnknownScope`.
    let label = meta
        .rounds
        .iter()
        .find(|def| def.id == round)
        .map_or_else(|| round.0.clone(), |def| def.label.clone());

    match mode {
        // The original single-step fill — one generator draw, at most one heat appended.
        FillMode::Next => match fill_round_once(registry, &meta, state, &round) {
            FillStep::Appended(heat) => {
                let detail = format!("{label}: scheduled {}.", heat.name);
                fill_ack(vec![heat], FillStop::SingleStep, detail)
            }
            FillStep::Terminal(stop, reason) => {
                fill_ack(Vec::new(), stop, no_heat(&label, stop, reason))
            }
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
            let mut scheduled: Vec<ScheduledHeat> = Vec::new();
            for _ in 0..MAX_FILL_ALL_HEATS {
                match fill_round_once(registry, &meta, state, &round) {
                    FillStep::Appended(heat) => {
                        scheduled.push(heat);
                        continue;
                    }
                    FillStep::Terminal(stop, reason) => {
                        // The heats drawn on the way are the answer when there are any; the stop
                        // reason is the answer when there are none.
                        let detail = if scheduled.is_empty() {
                            no_heat(&label, stop, reason)
                        } else {
                            let n = scheduled.len();
                            format!(
                                "{label}: generated {n} {}.",
                                if n == 1 { "heat" } else { "heats" }
                            )
                        };
                        return fill_ack(scheduled, stop, detail);
                    }
                    FillStep::Failed(ack) => return ack,
                }
            }
            eprintln!(
                "FillRound(All) for round {:?} hit the {MAX_FILL_ALL_HEATS}-heat cap without the \
                 generator reporting complete — stopping. A real format converges in far fewer; \
                 this indicates a generator bug, not a {MAX_FILL_ALL_HEATS}-heat round.",
                round.0,
            );
            // A capped fill is a FAILURE the RD must see, not an ok with a thousand junk heats
            // quietly in the append-only log — the heats it did draw are visible either way.
            CommandAck::failed(ProtocolError::new(
                ErrorCode::Internal,
                format!(
                    "the round's generator never reported complete after {MAX_FILL_ALL_HEATS} \
                     heats — stopping the fill; check the round's format configuration"
                ),
            ))
        }
    }
}

/// Pack a fill's effect into the ack (#395): what it scheduled, why it stopped, and the sentence
/// an RD can read. Always `ok` — every [`FillStop`] is a success; they differ in what to do next.
fn fill_ack(scheduled: Vec<ScheduledHeat>, stopped: FillStop, detail: String) -> CommandAck {
    CommandAck::ok_with(CommandOutcome::FillRound(FillRoundOutcome {
        scheduled,
        stopped,
        detail,
    }))
}

/// The RD-facing sentence for a fill that scheduled **nothing** — the case that used to arrive as
/// a bare `{"ok":true}` and send people looking downstream for a bug that was not there.
///
/// Each reason says what to do next, and the [`Blocked`](FillStop::Blocked) one carries the
/// generator's own words (#394) rather than guessing. `round` is the round's friendly label.
fn no_heat(round: &str, stopped: FillStop, reason: Option<String>) -> String {
    match stopped {
        FillStop::Blocked => match reason {
            // e.g. "Head-to-Head needs at least 2 pilots in the field — this round has 1. …"
            Some(reason) => format!("{round}: no heat generated — {reason}"),
            None => {
                format!("{round}: no heat generated — the round's format cannot race this field.")
            }
        },
        FillStop::AwaitingResult => {
            format!("{round}: no new heat — its outstanding heat has not been scored yet.")
        }
        FillStop::Complete => format!("{round}: no new heat — the round is complete."),
        // Unreachable: a single-step fill that scheduled nothing reports one of the reasons
        // above, never the mode's own contract. Worded so it is still honest if it ever lands.
        FillStop::SingleStep => format!("{round}: no new heat."),
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
///
/// **Which of those three happened is now in the ack** (#401), as
/// [`CommandOutcome::Advance`](crate::control::CommandOutcome::Advance). It used to be nowhere: all
/// three acked a bare `{"ok":true}`, so "Advance loaded the next heat" and "Advance had nothing to
/// load" were byte-identical — the same defect #395 fixed for `FillRound`, and hit far more often,
/// because "nothing to advance to" is the routine end of every round rather than a
/// misconfiguration. The ack now names the heat it loaded, or says **positively** that there was
/// none and why ([`AdvanceStop`]).
fn apply_advance(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    heat: HeatId,
) -> CommandAck {
    // The multi-step advance (transition + generator draw + selection) runs under the command
    // serialization lock, like every other validated write.
    let _guard = state.command_guard();
    // 1. Record the `Final → Advanced` transition (engine legality + event shape reused). A
    //    non-`Final` or unknown heat is rejected verbatim here, appending nothing.
    let advanced = match heat_transition(state, heat.clone(), HeatCommand::Advance) {
        Ok(event) => event,
        Err(err) => return CommandAck::failed(err),
    };
    if let Err(err) = state.append(advanced, None) {
        return CommandAck::failed(err);
    }

    // `None` only for an event that vanished between dispatch and here (the caller resolved it to
    // get this far). Without meta there are no round defs, so nothing can be named or generated —
    // folded into "no round to advance within" below rather than unwrapped.
    let meta = registry.meta_of(event_id);

    let (events, _cursor) = match state.read() {
        Ok(read) => read,
        Err(err) => return CommandAck::failed(err),
    };
    // The advanced heat's FRIENDLY name frames every sentence this command produces (repo display
    // rule); a raw id only surfaces if the heat resolves to no round and carries no label.
    let from = logged_heat_name(meta.as_ref(), &events, &heat);

    // 2. Pick the next heat to run. The advanced heat is now `Final` (not `Scheduled`), so
    //    `on_deck` against it is exactly "the next still-`Scheduled` heat" — the heat to load,
    //    filtered to the rounds this event still defines (#439). Removing a round leaves its
    //    `HeatScheduled` entries in the append-only log; loading one would put the RD on a heat
    //    that appears in no console list and whose round config (layouts, staging timer, min-lap)
    //    is gone from meta — and naming it in the ack would print its raw id, since there is no
    //    round left to derive a friendly name from. No meta (the event vanished under us) means
    //    no round list to filter by, and the fill below is skipped for the same reason.
    let defined = meta
        .as_ref()
        .map(|m| crate::live_state::defined_round_ids(&m.rounds));
    if let Some(next) = crate::live_state::on_deck(&events, &heat, defined.as_deref()) {
        // A next heat is already scheduled — follow Live control to it.
        let loaded = describe_logged_heat(meta.as_ref(), &events, &next);
        let detail = format!(
            "Advanced {from}: loaded {}, the heat already on deck.",
            loaded.name
        );
        return match select_next_heat(state, next) {
            Ok(()) => advance_ack(Some(loaded), AdvanceStop::LoadedOnDeck, detail),
            Err(err) => CommandAck::failed(err),
        };
    }

    // Nothing is on deck: ask the advanced heat's round generator for the next heat (a round
    // mid-fill), then select whatever it scheduled. An untagged heat has no round to draw from.
    let round = crate::live_state::round_of_heat(&events, &heat);
    let (Some(round), Some(meta)) = (round, meta) else {
        // Stated positively (#401): there was no generator to ask, which is a different answer
        // from "the round is complete" — a distinction the bare ok could not make at all.
        return advance_ack(
            None,
            AdvanceStop::Untagged,
            format!(
                "Advanced {from}: nothing to advance to — it is not part of a round, so there is \
                 no next heat to generate, and none is on deck."
            ),
        );
    };
    // The round's FRIENDLY label, like every other RD-facing sentence (repo display rule).
    let round_label = meta
        .rounds
        .iter()
        .find(|def| def.id == round)
        .map_or_else(|| round.0.clone(), |def| def.label.clone());

    // One generator draw (the `FillMode::Next` step). It either appends the next heat or reports
    // the round terminal (nothing to schedule).
    match fill_round_once(registry, &meta, state, &round) {
        // A heat was just scheduled in this round — and it is by construction the only still-
        // `Scheduled` heat (nothing was on deck a moment ago), so select it directly.
        FillStep::Appended(generated) => {
            let detail = format!("Advanced {from}: generated and loaded {}.", generated.name);
            match select_next_heat(state, generated.heat.clone()) {
                Ok(()) => advance_ack(Some(generated), AdvanceStop::Generated, detail),
                Err(err) => CommandAck::failed(err),
            }
        }
        // Round complete / awaiting a result / refused for this field: nothing more to load. The
        // reason used to be discarded right here (#401) and the ack said only `ok:true`; it now
        // rides out on the outcome, in the generator's own words for a refusal (#394).
        FillStep::Terminal(stop, reason) => {
            let (stopped, detail) = nothing_to_advance_to(&from, &round_label, stop, reason);
            advance_ack(None, stopped, detail)
        }
        // A generator/append error: surface it verbatim (the Advanced transition stands).
        FillStep::Failed(ack) => ack,
    }
}

/// Pack an advance's effect into the ack (#401): the heat it loaded (if any), what it did, and the
/// sentence an RD can read. Always `ok` — every [`AdvanceStop`] is a success (the `Advanced`
/// transition was recorded in all of them); they differ in what the RD has to do next.
fn advance_ack(loaded: Option<ScheduledHeat>, stopped: AdvanceStop, detail: String) -> CommandAck {
    CommandAck::ok_with(CommandOutcome::Advance(AdvanceOutcome {
        loaded,
        stopped,
        detail,
    }))
}

/// The discriminator and RD-facing sentence for an advance that loaded **nothing** — the case that
/// used to arrive as a bare `{"ok":true}` at the end of every single round.
///
/// Reuses the fill path's own [`FillStop`] as the source of truth for *why* the generator had
/// nothing, so the two commands cannot drift into telling the RD different stories about the same
/// round. `from` is the advanced heat's friendly name, `round` its round's label.
fn nothing_to_advance_to(
    from: &str,
    round: &str,
    stopped: FillStop,
    reason: Option<String>,
) -> (AdvanceStop, String) {
    match stopped {
        FillStop::Blocked => (
            AdvanceStop::Blocked,
            match reason {
                // e.g. "Head-to-Head needs at least 2 pilots in the field — this round has 1. …"
                Some(reason) => format!("Advanced {from}: nothing to advance to — {reason}"),
                None => format!(
                    "Advanced {from}: nothing to advance to — {round}'s format cannot race this \
                     field."
                ),
            },
        ),
        FillStop::AwaitingResult => (
            AdvanceStop::AwaitingResult,
            format!(
                "Advanced {from}: nothing to advance to — {round}'s outstanding heat has not been \
                 scored yet."
            ),
        ),
        // `SingleStep` is `FillMode::Next`'s own contract, never a terminal state
        // `fill_round_once` reports, so only `Complete` reaches here in practice. It is grouped
        // rather than panicked on, and the sentence is worded to stay true either way.
        FillStop::Complete | FillStop::SingleStep => (
            AdvanceStop::RoundComplete,
            format!("Advanced {from}: nothing to advance to — {round} is complete."),
        ),
    }
}

/// Describe a heat **already in the log** for an ack's outcome (#401) — its friendly name, plus the
/// lineup and channels it was last scheduled with, so a caller learns what it was moved onto
/// without a second round-trip. The same shape a freshly generated heat reports.
fn describe_logged_heat(
    meta: Option<&crate::events::EventMeta>,
    events: &[Event],
    heat: &HeatId,
) -> ScheduledHeat {
    let (lineup, frequencies, _label) = crate::round_engine::logged_heat_schedule(events, heat);
    ScheduledHeat {
        heat: heat.clone(),
        name: logged_heat_name(meta, events, heat),
        lineup,
        frequencies,
    }
}

/// The **friendly display name** of a heat already in the log (repo display rule: a raw heat id
/// must never reach a user).
///
/// Goes through [`round_engine::heat_display_name`](crate::round_engine::heat_display_name) — the
/// server-side twin of the console's `heatNameById` — whenever the heat resolves to a round. A
/// manually built, untagged heat has no round to name it within, so its RD-typed label stands in;
/// the raw id is the last resort the display rule allows, and only when the heat has neither.
fn logged_heat_name(
    meta: Option<&crate::events::EventMeta>,
    events: &[Event],
    heat: &HeatId,
) -> String {
    if let (Some(meta), Some(round)) = (meta, crate::live_state::round_of_heat(events, heat)) {
        if let Some(def) = meta.rounds.iter().find(|def| def.id == round) {
            return crate::round_engine::heat_display_name(def, events, heat);
        }
    }
    let (_, _, label) = crate::round_engine::logged_heat_schedule(events, heat);
    match label.map(|label| label.trim().to_string()) {
        Some(label) if !label.is_empty() => label,
        _ => heat.0.clone(),
    }
}

/// Append a [`Event::CurrentHeatSelected`] to move Live control onto `next`, the same selection the
/// RD's manual "select heat" records — so the `current_heat` fold follows it. The heat is one we
/// just derived from the log (on-deck / freshly generated), so it is known-scheduled; no further
/// validation needed.
///
/// Hands back the append error rather than an ack: the caller pairs a successful selection with the
/// outcome describing *which* heat it loaded (#401).
fn select_next_heat(state: &AppState, next: HeatId) -> Result<(), ProtocolError> {
    state.append(Event::CurrentHeatSelected { heat: next }, None)?;
    Ok(())
}

/// A heat's **friendly name** and the round it belongs to, for a refusal that has to name it
/// (CLAUDE.md: never a raw heat id).
///
/// `None` when the heat is not tagged to a round this event still defines — the caller then has
/// nothing to resolve against and refuses on existence instead.
fn named_heat<'a>(
    meta: &'a crate::events::EventMeta,
    events: &[Event],
    heat: &HeatId,
) -> Option<(&'a crate::events::RoundDef, String)> {
    let (_, _, round, _, _) = logged_schedule_full(events, heat);
    let round = meta.rounds.iter().find(|r| Some(&r.id) == round.as_ref())?;
    let name = crate::round_engine::heat_display_name(round, events, heat);
    Some((round, name))
}

/// Refuse anything that would re-tune a heat **past `Scheduled`** (#117 S3).
///
/// The binding rule, and the same one #387's re-materialization follows: a staged, armed, running
/// or finalized heat is either on the timer or in the record, and *a heat that has raced keeps the
/// channels it raced on*. Re-tuning one would relabel a result after the fact.
///
/// Names the heat by its friendly name, never its id.
fn require_retunable(
    events: &[Event],
    heat: &HeatId,
    name: &str,
    what: &str,
) -> Result<(), ProtocolError> {
    let state = gridfpv_engine::heat::heat_state(events, heat);
    if matches!(state, Some(gridfpv_engine::heat::HeatState::Scheduled)) {
        return Ok(());
    }
    Err(ProtocolError::new(
        ErrorCode::BadRequest,
        format!(
            "{name} has already been staged, so its {what} can no longer change — a heat keeps the \
             channels it raced on. Abort or restart it first to put it back to Scheduled."
        ),
    ))
}

/// Handle [`Command::SetHeatLayout`] (#117 S3): bind a `Scheduled` heat to one of the channel
/// layouts its round names, and **re-tune it** to that layout.
///
/// Two events, in order: the [`Event::HeatLayoutSet`] recording the choice, then a fresh
/// [`Event::HeatScheduled`] carrying the channels the layout gives each seat. Appending the bind
/// first means a reader folding the log in order never sees a heat carrying one layout's channels
/// while still recorded against another.
///
/// The lineup, class, round tag and RD-typed label are carried through unchanged: this re-tunes the
/// heat, it does not re-draw it.
///
/// Refusals, all typed `400`s naming the heat, the layout and the timer by their friendly names:
/// a heat past `Scheduled`; a layout the event does not have; a layout the heat's **round** does not
/// name; and any [`AssignError`](crate::round_engine::AssignError) the layout produces (a node it
/// says nothing about, a lineup wider than the enabled node set).
fn apply_set_heat_layout(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    heat: HeatId,
    layout: Option<gridfpv_events::LayoutId>,
) -> CommandAck {
    use crate::round_engine;

    let _guard = state.command_guard();
    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };
    let (events, _cursor) = match state.read() {
        Ok(read) => read,
        Err(err) => return CommandAck::failed(err),
    };
    let Some((round, name)) = named_heat(&meta, &events, &heat) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no heat scheduled with id {:?} in this event", heat.0),
        ));
    };
    if let Err(err) = require_retunable(&events, &heat, &name, "channel layout") {
        return CommandAck::failed(err);
    }

    // The layout must exist AND be one this round flies. The round is where the RD decided which
    // layouts this phase of the event may use; a heat reaching past that list would quietly
    // contradict the decision one level up.
    let resolved = match &layout {
        Some(id) => match meta.layout(id) {
            Some(found) if round.layouts.contains(id) => Some(found.clone()),
            Some(found) => {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    format!(
                        "{:?} does not fly the {:?} channel layout — add it to the round first, or \
                         pick one of the layouts it does fly",
                        round.label, found.name
                    ),
                ));
            }
            None => {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    "this event has no such channel layout — pick one from its Channel layouts \
                     page"
                        .to_string(),
                ));
            }
        },
        None => None,
    };

    // Re-tune. With the bind cleared, the heat falls back to the round's default layout for *its
    // position* — the alternating default (#117 S3), resolved through exactly the same helper the
    // fill uses, so clearing a bind restores what the round would have given this heat anyway
    // rather than dropping it onto the first layout. With no layouts named, the auto-pick.
    let (lineup, class, round_tag, _freqs, label) = logged_schedule_full(&events, &heat);
    let effective = match &resolved {
        Some(found) => Some(found.clone()),
        None => round_engine::default_layout_for_heat(&meta, Some(round), &events, &heat).cloned(),
    };
    let frequencies = match round_engine::assign_for_event(
        &meta,
        &registry.timers(),
        effective.as_ref(),
        &lineup,
    ) {
        Ok(freqs) => freqs,
        Err(err) => {
            return CommandAck::failed(ProtocolError::new(ErrorCode::BadRequest, err.to_string()));
        }
    };

    if let Err(err) = state.append(
        Event::HeatLayoutSet {
            heat: heat.clone(),
            layout: effective.as_ref().map(|l| l.id.clone()),
        },
        None,
    ) {
        return CommandAck::failed(err);
    }
    match state.append(
        Event::HeatScheduled {
            heat,
            lineup,
            class,
            round: round_tag,
            frequencies,
            label,
        },
        None,
    ) {
        Ok(_offset) => CommandAck::ok(),
        Err(err) => CommandAck::failed(err),
    }
}

/// Handle [`Command::OverrideHeatSeating`] (#117 S3): set a `Scheduled` heat's pilots and their
/// channels by hand, and make the choice **stick**.
///
/// Records the [`Event::HeatSeatingOverridden`] first — that is the durable half, the one a round
/// re-fill and a round edit's re-materialization both re-apply — then re-emits the heat's schedule
/// so the heat is seated that way immediately.
///
/// An **empty lineup clears** the override: the heat is re-formed from its round's plan, exactly as
/// if the RD had never touched it. That is the only way out, and it is deliberately explicit.
///
/// Refusals, typed `400`s naming the heat and the timer by their friendly names: a heat past
/// `Scheduled`; a repeated pilot; a lineup wider than the timer's **enabled** node set; and any
/// [`AssignError`](crate::round_engine::AssignError) raised while filling the channels the RD did
/// not type in from the heat's layout.
fn apply_override_heat_seating(
    registry: &EventRegistry,
    event_id: &EventId,
    state: &AppState,
    heat: HeatId,
    lineup: Vec<gridfpv_events::CompetitorRef>,
    frequencies: Vec<(gridfpv_events::CompetitorRef, u16)>,
) -> CommandAck {
    use crate::round_engine;

    let _guard = state.command_guard();
    let Some(meta) = registry.meta_of(event_id) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    };
    let (events, _cursor) = match state.read() {
        Ok(read) => read,
        Err(err) => return CommandAck::failed(err),
    };
    let Some((round, name)) = named_heat(&meta, &events, &heat) else {
        return CommandAck::failed(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no heat scheduled with id {:?} in this event", heat.0),
        ));
    };
    if let Err(err) = require_retunable(&events, &heat, &name, "seating") {
        return CommandAck::failed(err);
    }
    if let Err(err) = require_distinct_lineup(&lineup) {
        return CommandAck::failed(err);
    }
    // The same membership/roster validation a hand-built heat gets: an override may re-seat the
    // heat, but not with somebody who is not in this event.
    let class = round_engine::round_class(&meta, &round.id);
    if !lineup.is_empty() {
        if let Err(err) =
            validate_tagged_lineup(registry, &meta, &lineup, &class, &Some(round.id.clone()))
        {
            return CommandAck::failed(err);
        }
    }

    if let Err(err) = state.append(
        Event::HeatSeatingOverridden {
            heat: heat.clone(),
            lineup: lineup.clone(),
            frequencies: frequencies.clone(),
        },
        None,
    ) {
        return CommandAck::failed(err);
    }

    // Re-form the heat under the override we just recorded, through the same round-fill machinery
    // — so what the RD sees now is exactly what a later re-fill will reproduce.
    let (events, _cursor) = match state.read() {
        Ok(read) => read,
        Err(err) => return CommandAck::failed(err),
    };
    let (plan_lineup, _class, round_tag, plan_freqs, label) = logged_schedule_full(&events, &heat);
    let seated = if lineup.is_empty() {
        plan_lineup
    } else {
        lineup
    };
    let layout = round_engine::layout_for_heat(&meta, Some(round), &events, &heat).cloned();
    let assigned = if frequencies.is_empty() {
        match round_engine::assign_for_event(&meta, &registry.timers(), layout.as_ref(), &seated) {
            Ok(freqs) => freqs,
            Err(err) => {
                return CommandAck::failed(ProtocolError::new(
                    ErrorCode::BadRequest,
                    err.to_string(),
                ));
            }
        }
    } else {
        frequencies
    };
    // With no layout and no typed channels there is nothing better than what the heat already had
    // — never blank a heat's channels as a side effect of re-seating it.
    let assigned = if assigned.is_empty() {
        plan_freqs
    } else {
        assigned
    };

    match state.append(
        Event::HeatScheduled {
            heat,
            lineup: seated,
            class,
            round: round_tag,
            frequencies: assigned,
            label,
        },
        None,
    ) {
        Ok(_offset) => CommandAck::ok(),
        Err(err) => CommandAck::failed(err),
    }
}

/// A heat's most recent `HeatScheduled` payload: `(lineup, class, round, frequencies, label)`.
type LoggedSchedule = (
    Vec<gridfpv_events::CompetitorRef>,
    Option<gridfpv_events::ClassId>,
    Option<gridfpv_events::RoundId>,
    Vec<(gridfpv_events::CompetitorRef, u16)>,
    Option<String>,
);

/// A heat's full **most recent** `HeatScheduled` payload — for a command that re-emits the
/// schedule rather than drawing a new one.
fn logged_schedule_full(events: &[Event], heat: &HeatId) -> LoggedSchedule {
    let mut out = (Vec::new(), None, None, Vec::new(), None);
    for event in events {
        if let Event::HeatScheduled {
            heat: h,
            lineup,
            class,
            round,
            frequencies,
            label,
        } = event
        {
            if h == heat {
                out = (
                    lineup.clone(),
                    class.clone(),
                    round.clone(),
                    frequencies.clone(),
                    label.clone(),
                );
            }
        }
    }
    out
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
    // The whole validate→append runs under the command serialization lock: without it, a
    // concurrent appender (the auto-official driver, another console) could change the very
    // state the validation just read — a ruling landing on a heat that went Final in the
    // window, Finalize slipping past a fresh protest, duplicate heat ids both passing.
    let _guard = state.command_guard();
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

        // --- The two #117 S3 channel decisions are the same case: both resolve a layout against
        // the event's meta, so neither can be validated from the log alone. Same arm, same
        // reasoning as `FillRound` above. ---
        Command::SetHeatLayout { .. } => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "SetHeatLayout must be applied through the event-aware control path",
        )),
        Command::OverrideHeatSeating { .. } => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "OverrideHeatSeating must be applied through the event-aware control path",
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

        // --- Marshaling adjudications: validate targets where cheap, reject any result-changing
        // ruling on an OFFICIAL (Final) heat (Revert is the sanctioned re-open), then append. ---
        Command::VoidDetection { target } => {
            // A void may target a pass — or a prior DetectionVoided (void-the-void, the
            // sanctioned RESTORE of a mistakenly-removed pass; the fold walks the chain).
            require_void_target(state, target)?;
            require_target_heat_not_final(state, target)?;
            require_target_in_current_run(state, target)?;
            // Voiding a pass whose lap carries an EFFECTIVE throw-out would leave the
            // throw-out dangling while the neighbouring laps merge and COUNT — the ruling
            // must be unwound first, in order.
            require_no_effective_throw_out(state, target)?;
            Ok(Event::DetectionVoided { target })
        }
        Command::AdjustLap { target, at } => {
            require_pass_target(state, target)?;
            require_target_heat_not_final(state, target)?;
            require_target_in_current_run(state, target)?;
            require_sane_source_time(at)?;
            {
                let (events, _cursor) = state.read()?;
                if let (Some(h), Some(c)) = (
                    heat_of_offset(&events, target),
                    competitor_of_pass_target(&events, target),
                ) {
                    // The re-timed pass itself is exempt (re-asserting its own instant is fine).
                    require_no_instant_collision(state, &h, &c, at, Some(target))?;
                }
            }
            Ok(Event::LapAdjusted { target, at })
        }
        Command::SplitLap { target, at } => {
            // The split's target is the pass that *ends* the over-long lap — a real Pass,
            // validated exactly like `VoidDetection`/`AdjustLap`.
            require_pass_target(state, target)?;
            require_target_heat_not_final(state, target)?;
            require_target_in_current_run(state, target)?;
            require_sane_source_time(at)?;
            {
                let (events, _cursor) = state.read()?;
                if let (Some(h), Some(c)) = (
                    heat_of_offset(&events, target),
                    competitor_of_pass_target(&events, target),
                ) {
                    require_no_instant_collision(state, &h, &c, at, None)?;
                }
            }
            Ok(Event::LapSplit { target, at })
        }
        Command::InsertLap {
            adapter,
            competitor,
            at,
            heat,
        } => {
            // A tagged insertion must name a real heat (the tag is what routes it into that
            // heat's scoring window even when a different heat is live) that is not Final; an
            // untagged one is a legacy client and attributes positionally, so the lock checks
            // the heat the insertion WOULD land in — the positionally-active heat at the log
            // tail.
            require_sane_source_time(at)?;
            match &heat {
                Some(h) => {
                    require_scheduled_heat(state, h)?;
                    require_not_final(state, h)?;
                    require_no_instant_collision(state, h, &competitor, at, None)?;
                }
                None => {
                    let (events, _cursor) = state.read()?;
                    if let Some(active) = events
                        .len()
                        .checked_sub(1)
                        .and_then(|tail| positional_heat_at(&events, tail))
                    {
                        require_not_final_in(&events, &active)?;
                    }
                }
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
            require_not_final(state, &heat)?;
            // Voiding needs a run to void — a pre-run void was window-inert (it applied to
            // nothing) yet blocked a real void later via the duplicate guard below.
            require_heat_has_run(state, &heat)?;
            // One EFFECTIVE void per heat *this run*: a stacked second void made the first
            // reversal a silent no-op (the heat stayed voided behind an ok-acked
            // ReverseRuling); a void from an ABANDONED run is inert and must not block.
            require_heat_not_voided(state, &heat)?;
            Ok(Event::HeatVoided { heat })
        }
        Command::ApplyPenalty {
            heat,
            competitor,
            penalty,
        } => {
            require_scheduled_heat(state, &heat)?;
            require_not_final(state, &heat)?;
            // A time penalty must WORSEN the target's result: a zero/negative `micros` (a
            // typo'd sign, a buggy client) would silently *improve* the penalized pilot's
            // deciding time while the audit trail reads as a penalty.
            if let gridfpv_events::Penalty::TimeAdded { micros } = &penalty {
                if *micros <= 0 {
                    return Err(ProtocolError::new(
                        ErrorCode::BadRequest,
                        "a time penalty must add a positive number of microseconds",
                    ));
                }
            }
            // ONE effective DQ per competitor per heat (time/points penalties stack by
            // design; a status can't): a double-clicked duplicate made reversing "the" DQ a
            // silent no-op — the stacked copy kept the pilot disqualified.
            if matches!(&penalty, gridfpv_events::Penalty::Disqualify { .. }) {
                require_not_already_disqualified(state, &heat, &competitor)?;
            }
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
            require_not_final(state, &heat)?;
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
            require_target_heat_not_final(state, target)?;
            require_target_in_current_run(state, target)?;
            // ONE effective throw-out per lap: a stacked duplicate made ReverseRuling a
            // silent no-op (the other copy kept excluding the lap) — the same effectively-
            // once rule VoidHeat and ResolveProtest already follow.
            require_no_effective_throw_out(state, target)?;
            Ok(Event::LapThrownOut { target })
        }
        // File a protest against a heat result — the append-only filing fact. Deliberately NOT
        // gated on Final: a protest changes no result, and disputing an already-official one is
        // exactly what protests are for (the RD Reverts only if the protest is upheld).
        Command::FileProtest {
            heat,
            competitor,
            note,
        } => {
            require_scheduled_heat(state, &heat)?;
            // A protest contests a RUN's result, so the heat must have one: filed before the
            // heat ever ran (or in the gap after a reset), the filing counted for the
            // Finalize gate but sat OUTSIDE every run-windowed audit view — an invisible
            // blocker the RD couldn't resolve.
            require_heat_has_run(state, &heat)?;
            Ok(Event::ProtestFiled {
                heat,
                competitor,
                note,
            })
        }
        // Resolve a filed protest — the target must be a real `ProtestFiled`. Also NOT gated on
        // Final: a protest filed against an official result must be resolvable (e.g. denied)
        // without re-opening it; an upheld one is acted on via Revert, where the open-protest
        // Finalize gate composes correctly.
        Command::ResolveProtest { target, outcome } => {
            require_protest_target(state, target)?;
            // One EFFECTIVE resolution per filing: a second (double-click, a second console)
            // used to be recorded too — possibly with a contradictory outcome — and then
            // reversing "the" resolution silently failed to re-open the protest (the other
            // resolution still closed it). Reversing the standing resolution is the sanctioned
            // way to re-decide.
            require_protest_unresolved(state, target)?;
            Ok(Event::ProtestResolved { target, outcome })
        }
        Command::ReverseRuling { target } => {
            // Generalized reversal (Slice 6): the target must be a real *ruling* — a penalty, a
            // throw-out, a protest resolution, or a heat-void. Validated so an out-of-range or
            // non-ruling offset is a typed BadRequest (nothing appended). Reversal DOES change
            // the result, so it is uniformly locked on a Final heat (even when its target is a
            // protest resolution — revert-first keeps the official record honest).
            require_ruling_target(state, target)?;
            require_target_heat_not_final(state, target)?;
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

/// Reject a result-changing marshaling command on a heat whose folded state is **`Final`** —
/// an official result is locked; `Revert` (Final → Unofficial) is the sanctioned re-open.
///
/// Folds with the same [`heat::heat_state`] the FSM legality checks use, so "official" here is
/// exactly the state the heat-loop (and the live view's phase badge) sees. Any other state —
/// including "never scheduled" (`None`, which the existence checks reject separately) — passes.
/// A locked heat maps to a typed [`ErrorCode::BadRequest`]; nothing is appended.
fn require_not_final(state: &AppState, heat: &HeatId) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    require_not_final_in(&events, heat)
}

/// [`require_not_final`] over an already-read log slice (the shared core, so a caller that has
/// the events in hand — e.g. the target-addressed path — doesn't re-read the log).
fn require_not_final_in(events: &[Event], heat: &HeatId) -> Result<(), ProtocolError> {
    if heat::heat_state(events, heat) == Some(gridfpv_engine::heat::HeatState::Final) {
        Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "heat {:?} result is official (Final) — Revert it to marshal",
                heat.0
            ),
        ))
    } else {
        Ok(())
    }
}

/// The Final-lock check for the **target-addressed** marshaling commands (`VoidDetection` /
/// `AdjustLap` / `SplitLap` / `ThrowOutLap` / `ReverseRuling`): resolve the target's owning
/// heat ([`heat_of_offset`]) and require it not be `Final`. A target whose owning heat cannot
/// be resolved passes — there is nothing official to protect.
fn require_target_heat_not_final(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match heat_of_offset(&events, target) {
        Some(heat) => require_not_final_in(&events, &heat),
        None => Ok(()),
    }
}

/// Resolve the heat that **owns** the event at log offset `target` — the heat a ruling aimed at
/// that offset would re-score. `None` when no owning heat resolves (an out-of-range offset, or
/// an event before any heat ran).
///
/// The routing rules mirror [`crate::app::heat_window_offsets`] (the one window fold results
/// and audits are keyed on), by what the event itself can say about where it belongs:
///
/// - **Heat-tagged** events (heat-loop, `HeatVoided`/`PenaltyApplied`/`ProtestFiled`, a tagged
///   `LapInserted`) own their heat outright — the tag, never the position.
/// - **Offset-addressed rulings** (`DetectionVoided`/`LapAdjusted`/`LapSplit`/`LapThrownOut`/
///   `ProtestResolved`/`RulingReversed`) belong to whichever heat *their* target is in —
///   recurse down the chain ("reverse the ruling that voided the pass…"). Targets always point
///   backwards, so the walk terminates; a malformed forward/self target resolves to `None`.
/// - **Untagged** events (raw `Pass`es, a legacy untagged `LapInserted`) attribute
///   **positionally** ([`positional_heat_at`]) — the heat whose heat-loop span contains the
///   offset, the same `active`-cursor rule `heat_window_offsets` applies.
pub(crate) fn heat_of_offset(events: &[Event], target: LogRef) -> Option<HeatId> {
    let mut offset = target.0 as usize;
    loop {
        match events.get(offset)? {
            Event::HeatScheduled { heat, .. }
            | Event::HeatStateChanged { heat, .. }
            | Event::HeatVoided { heat }
            | Event::PenaltyApplied { heat, .. }
            | Event::ProtestFiled { heat, .. } => return Some(heat.clone()),
            Event::LapInserted { heat: Some(h), .. } => return Some(h.clone()),
            // A bridge-stamped pass belongs to its TAG — the same rule `heat_window_offsets`
            // scores by. Resolving it positionally let the Final lock consult the WRONG heat
            // (a late tagged pass after another heat staged), accepting rulings that changed
            // a Final result — or rejecting legal ones over an unrelated Final heat.
            Event::Pass(p) if p.heat.is_some() => return p.heat.clone(),
            Event::DetectionVoided { target }
            | Event::LapAdjusted { target, .. }
            | Event::LapSplit { target, .. }
            | Event::LapThrownOut { target }
            | Event::ProtestResolved { target, .. }
            | Event::RulingReversed { target } => {
                let next = target.0 as usize;
                if next >= offset {
                    return None; // malformed chain (targets must point backwards) — bail, don't loop
                }
                offset = next;
            }
            _ => return positional_heat_at(events, offset),
        }
    }
}

/// The heat **positionally active** at log offset `offset`: the heat of the latest heat-loop
/// event (`HeatScheduled` / `HeatStateChanged`) at or before it. This is the same `active`
/// cursor [`crate::app::heat_window_offsets`] walks to attribute untagged events (raw passes,
/// legacy insertions) to a heat — kept a small faithful re-walk here because the window fold
/// interleaves it with tag/target routing and run-start trimming that don't apply to a single
/// offset lookup. `None` before any heat has appeared in the log.
fn positional_heat_at(events: &[Event], offset: usize) -> Option<HeatId> {
    let mut active: Option<&HeatId> = None;
    for event in events.iter().take(offset.saturating_add(1)) {
        if let Event::HeatScheduled { heat, .. } | Event::HeatStateChanged { heat, .. } = event {
            active = Some(heat);
        }
    }
    active.cloned()
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

/// Require that `target` names a **lap-gate pass** in the log — raw
/// ([`Pass`](gridfpv_events::Pass)) or **synthetic** (a marshaling
/// [`LapInserted`](Event::LapInserted) / [`LapSplit`](Event::LapSplit), which the corrected-pass
/// fold treats as passes and supports voiding / re-timing — "void the void"). The cheap target
/// check for the offset-addressed marshaling commands (`VoidDetection`, `AdjustLap`).
///
/// Raw-`Pass`-only here was a bug: the RotorHazard save-then-pull catch-up path records
/// recovered laps as `LapInserted`, so those laps' boundary refs were un-voidable — a
/// re-detection commit on such a heat bounced with "not a detected pass" (live 2026-07-03).
/// An out-of-range or non-pass offset is [`ErrorCode::BadRequest`]; nothing is appended.
/// Require that `target` is a valid [`Command::VoidDetection`] target: a lap-gate pass (raw or
/// synthetic, like [`require_pass_target`]) — or a prior [`Event::DetectionVoided`], the
/// **void-the-void RESTORE** path (the corrected-pass fold walks the chain). Restore used to be
/// unreachable through the command layer entirely: a mistaken one-click removal was permanent.
fn require_void_target(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match events.get(target.0 as usize) {
        Some(
            Event::Pass(_)
            | Event::LapInserted { .. }
            | Event::LapSplit { .. }
            | Event::DetectionVoided { .. },
        ) => Ok(()),
        Some(_) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "log offset {} is not a detected pass or a prior removal",
                target.0
            ),
        )),
        None => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("log offset {} is out of range", target.0),
        )),
    }
}

/// Require that `target` sits inside its owning heat's CURRENT run window. A ruling aimed at an
/// abandoned run's pass (a stale marshaling screen after a Restart/Discard) used to be accepted
/// and appended — then silently dropped by every window fold: an ack-ok correction with zero
/// effect and no audit trace. Rejecting it tells the RD their screen is stale.
fn require_target_in_current_run(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let Some(heat) = heat_of_offset(&events, target) else {
        return Ok(()); // unowned target — the type guards already vetted it
    };
    let run_start = crate::live_state::current_run_start(&events, &heat) as u64;
    if target.0 < run_start {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "log offset {} belongs to an ABANDONED run of heat {:?} (it was reset since) — \
                 refresh and correct the current run instead",
                target.0, heat.0
            ),
        ));
    }
    Ok(())
}

/// Require that no EFFECTIVE (non-reversed) [`Event::LapThrownOut`] targets `target` — shared by
/// `ThrowOutLap` (one effective throw-out per lap) and `VoidDetection` (voiding a thrown-out
/// lap's pass would leave the throw-out dangling while the merged neighbour lap COUNTS).
fn require_no_effective_throw_out(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let reversed: std::collections::HashSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::RulingReversed { target } => Some(target.0),
            _ => None,
        })
        .collect();
    let standing = events.iter().enumerate().any(|(offset, e)| {
        matches!(e, Event::LapThrownOut { target: t } if t.0 == target.0)
            && !reversed.contains(&(offset as u64))
    });
    if standing {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "the lap ending at offset {} carries a standing throw-out — reverse it first",
                target.0
            ),
        ));
    }
    Ok(())
}

/// Require that `competitor` carries no EFFECTIVE (non-reversed) disqualification in `heat` —
/// one DQ status per pilot per heat (time/points penalties stack by design; a status cannot).
fn require_not_already_disqualified(
    state: &AppState,
    heat: &HeatId,
    competitor: &gridfpv_events::CompetitorRef,
) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let reversed: std::collections::HashSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::RulingReversed { target } => Some(target.0),
            _ => None,
        })
        .collect();
    let standing = events.iter().enumerate().any(|(offset, e)| {
        matches!(
            e,
            Event::PenaltyApplied {
                heat: h,
                competitor: c,
                penalty: gridfpv_events::Penalty::Disqualify { .. },
            } if h == heat && c == competitor
        ) && !reversed.contains(&(offset as u64))
    });
    if standing {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "competitor {:?} is already disqualified in heat {:?} — reverse that DQ first",
                competitor.0, heat.0
            ),
        ));
    }
    Ok(())
}

/// Require that `at` does not COLLIDE with an existing corrected pass instant of `competitor`
/// in the target's heat: two same-instant passes fold into a ZERO-duration lap — a physically
/// impossible 0.000s that then wins best-lap and corrupts every ranking it feeds. Fuzz-caught:
/// an AdjustLap delta landing exactly on the neighbouring pass's instant.
fn require_no_instant_collision(
    state: &AppState,
    heat: &HeatId,
    competitor: &gridfpv_events::CompetitorRef,
    at: gridfpv_events::SourceTime,
    exempt: Option<LogRef>,
) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let window = crate::app::heat_window_offsets(&events, heat);
    let (surviving, _voided) =
        gridfpv_projection::corrected_and_voided_passes(window.iter().map(|(o, e)| (*o, e)));
    let collides = surviving.iter().any(|(offset, pass)| {
        pass.competitor == *competitor && pass.at == at && exempt.is_none_or(|x| x.0 != *offset)
    });
    if collides {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "{:?} already has a pass at exactly {}µs — two same-instant passes would fold \
                 into an impossible zero-duration lap",
                competitor.0, at.micros
            ),
        ));
    }
    Ok(())
}

/// The competitor a pass-target belongs to, from the raw log (for the collision guard).
fn competitor_of_pass_target(
    events: &[Event],
    target: LogRef,
) -> Option<gridfpv_events::CompetitorRef> {
    match events.get(target.0 as usize)? {
        Event::Pass(p) => Some(p.competitor.clone()),
        Event::LapInserted { competitor, .. } => Some(competitor.clone()),
        Event::LapSplit { target, .. } => competitor_of_pass_target(events, *target),
        _ => None,
    }
}

/// Require a **sane source-clock instant** for an inserted/re-timed crossing: positive, and
/// within 24h of the source epoch. A typo'd/unit-confused `at` (0, negative, or absurd) was
/// accepted and became the heat's earliest pass — hijacking `race_start` so a Timed window
/// closed before every REAL lap, silently zeroing the whole heat's scored counts.
fn require_sane_source_time(at: gridfpv_events::SourceTime) -> Result<(), ProtocolError> {
    const MAX_SANE_MICROS: i64 = 24 * 60 * 60 * 1_000_000; // 24h of race-relative source clock
    if at.micros < 1 || at.micros > MAX_SANE_MICROS {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "source time {}µs is out of range (must be positive and within 24h of the \
                 source clock's start)",
                at.micros
            ),
        ));
    }
    Ok(())
}

/// Require that `heat` has a CURRENT run (a `Running` since its latest reset) — the shared
/// precondition for run-scoped rulings that would otherwise be recorded yet apply to nothing.
fn require_heat_has_run(state: &AppState, heat: &HeatId) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    if crate::live_state::current_run_start(&events, heat) >= events.len() {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "heat {:?} has no run yet — there is nothing to rule on",
                heat.0
            ),
        ));
    }
    Ok(())
}

fn require_pass_target(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    match events.get(target.0 as usize) {
        Some(Event::Pass(_) | Event::LapInserted { .. } | Event::LapSplit { .. }) => Ok(()),
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
    // A protest contests a specific run's result: one filed before a RESET (Abort / Restart /
    // Discard — the heat re-races anyway) dies with the abandoned run. Without this boundary,
    // a pre-Restart protest blocked Finalize forever while the run-windowed audit view showed
    // no protest at all — an RD deadlock. The boundary is the latest reset (not the run start):
    // a protest filed before the re-run's `Running` still counts against the new result.
    let reset_boundary = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            Event::HeatStateChanged {
                heat: h,
                transition:
                    HeatTransition::Aborted | HeatTransition::Restarted | HeatTransition::Discarded,
            } if h == heat => Some(i as u64 + 1),
            _ => None,
        })
        .next_back()
        .unwrap_or(0);
    // Filed protests for this heat since the latest reset, with no effective resolution.
    events
        .iter()
        .enumerate()
        .filter(|(offset, e)| {
            matches!(e, Event::ProtestFiled { heat: h, .. } if h == heat)
                && *offset as u64 >= reset_boundary
                && !resolved.contains(&(*offset as u64))
        })
        .count()
}

/// Require that the `ProtestFiled` at `target` has **no effective (non-reversed) resolution**
/// yet — the double-resolve guard for [`Command::ResolveProtest`]. A filing whose resolution was
/// reversed counts as unresolved again (re-deciding it is exactly the reversal's purpose).
fn require_protest_unresolved(state: &AppState, target: LogRef) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let reversed: std::collections::HashSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::RulingReversed { target } => Some(target.0),
            _ => None,
        })
        .collect();
    let already = events.iter().enumerate().any(|(offset, e)| {
        matches!(e, Event::ProtestResolved { target: t, .. } if t.0 == target.0)
            && !reversed.contains(&(offset as u64))
    });
    if already {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "protest at offset {} is already resolved — reverse that resolution to re-decide it",
                target.0
            ),
        ));
    }
    Ok(())
}

/// Require that `heat` is not already **effectively voided** (a [`Event::HeatVoided`] with no
/// [`Event::RulingReversed`] undoing it) — the double-void guard for [`Command::VoidHeat`].
fn require_heat_not_voided(state: &AppState, heat: &HeatId) -> Result<(), ProtocolError> {
    let (events, _cursor) = state.read()?;
    let reversed: std::collections::HashSet<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::RulingReversed { target } => Some(target.0),
            _ => None,
        })
        .collect();
    let run_start = crate::live_state::current_run_start(&events, heat) as u64;
    let voided = events.iter().enumerate().any(|(offset, e)| {
        matches!(e, Event::HeatVoided { heat: h } if h == heat)
            && offset as u64 >= run_start
            && !reversed.contains(&(offset as u64))
    });
    if voided {
        return Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!(
                "heat {:?} is already voided — reverse the existing void first",
                heat.0
            ),
        ));
    }
    Ok(())
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
            heat: None,
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

    /// `VoidDetection` / `AdjustLap` accept **synthetic** passes too: the RH save-then-pull
    /// catch-up path records recovered laps as `LapInserted`, and the corrected-pass fold fully
    /// supports voiding / re-timing them ("void the void") — a raw-`Pass`-only validator made
    /// those laps un-marshalable, bouncing re-detection commits with "not a detected pass".
    #[test]
    fn void_and_adjust_accept_synthetic_pass_targets() {
        let mut log = InMemoryLog::default();
        // offset 0: a marshaling-inserted lap pass (the RH catch-up shape — untagged).
        EventLog::append(
            &mut log,
            Event::LapInserted {
                adapter: AdapterId("rh-1".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(5_000_000),
                heat: None,
            },
            None,
        )
        .unwrap();
        let state = AppState::new(log);

        // Voiding the inserted lap succeeds and appends the ruling.
        let ack = apply_command(&state, Command::VoidDetection { target: LogRef(0) });
        assert!(ack.ok, "voiding a LapInserted must be accepted: {ack:?}");
        // Re-timing it succeeds too.
        let ack = apply_command(
            &state,
            Command::AdjustLap {
                target: LogRef(0),
                at: SourceTime::from_micros(5_200_000),
            },
        );
        assert!(ack.ok, "adjusting a LapInserted must be accepted: {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::DetectionVoided { target } if *target == LogRef(0)))
        );
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
        // A protest contests a RUN's result — give the heat one (the run-scoped guard).
        state
            .append(
                Event::HeatStateChanged {
                    heat: heat(),
                    transition: HeatTransition::Running,
                },
                None,
            )
            .unwrap();

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

        // Resolve the protest (offset 2 — after the schedule + the run).
        let ack = apply_command(
            &state,
            Command::ResolveProtest {
                target: LogRef(2),
                outcome: ProtestOutcome::Upheld,
            },
        );
        assert!(ack.ok, "got {ack:?}");
        let (events, _) = state.read().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ProtestResolved { target, outcome: ProtestOutcome::Upheld }
                if *target == LogRef(2)
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
        // A protest contests a RUN's result, so the fixture gives the heat a run first
        // (offset 0: Running) — the predicate windows from the current run.
        let running = Event::HeatStateChanged {
            heat: h.clone(),
            transition: HeatTransition::Running,
        };
        let filed = Event::ProtestFiled {
            heat: h.clone(),
            competitor: CompetitorRef("A".into()),
            note: "x".into(),
        };
        // A filing against the run → open.
        let base = vec![running.clone(), filed.clone()];
        assert_eq!(open_protest_count(&base, &h), 1);
        // Filing + resolution → closed.
        let resolved = vec![
            running.clone(),
            filed.clone(),
            Event::ProtestResolved {
                target: LogRef(1),
                outcome: ProtestOutcome::Denied,
            },
        ];
        assert_eq!(open_protest_count(&resolved, &h), 0);
        // Reversing the resolution (at offset 2) re-opens the protest.
        let mut reversed = resolved.clone();
        reversed.push(Event::RulingReversed { target: LogRef(2) });
        assert_eq!(open_protest_count(&reversed, &h), 1);
        // A protest for a DIFFERENT heat doesn't count.
        assert_eq!(open_protest_count(&base, &HeatId("other".into())), 0);
    }

    #[test]
    fn deep_lap_guards_reject_the_footgun_sequences() {
        use gridfpv_events::Penalty;
        // One raced heat with two passes: 0 sched, then Stage/Start/Skip drive it Running.
        let state = scheduled_state();
        for cmd in [
            Command::Stage { heat: heat() },
            Command::Start { heat: heat() },
            Command::SkipCountdown { heat: heat() },
        ] {
            assert!(apply_command(&state, cmd).ok);
        }
        let p1 = state.append(pass("A", 1_000_000, 1), None).unwrap();
        let p2 = state.append(pass("A", 4_000_000, 2), None).unwrap();

        // Degenerate source times are rejected (the race_start hijack).
        for at in [0i64, -5, 90 * 60 * 60 * 1_000_000] {
            let ack = apply_command(
                &state,
                Command::AdjustLap {
                    target: LogRef(p2),
                    at: SourceTime::from_micros(at),
                },
            );
            assert!(!ack.ok, "at={at} must be rejected");
        }

        // ONE effective throw-out per lap; reversing it re-arms.
        assert!(apply_command(&state, Command::ThrowOutLap { target: LogRef(p2) }).ok);
        let dup = apply_command(&state, Command::ThrowOutLap { target: LogRef(p2) });
        assert!(!dup.ok, "stacked throw-out must be rejected");
        // Voiding the thrown-out lap's pass is rejected while the ruling stands.
        let void_over_throw = apply_command(&state, Command::VoidDetection { target: LogRef(p2) });
        assert!(
            !void_over_throw.ok,
            "void over a standing throw-out must be rejected"
        );
        let (events, _) = state.read().unwrap();
        let throw_offset = events
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e, Event::LapThrownOut { .. }).then_some(i as u64))
            .unwrap();
        assert!(
            apply_command(
                &state,
                Command::ReverseRuling {
                    target: LogRef(throw_offset)
                }
            )
            .ok
        );
        assert!(
            apply_command(&state, Command::ThrowOutLap { target: LogRef(p2) }).ok,
            "after the reversal a fresh throw-out is legal again"
        );

        // ONE effective DQ per pilot per heat; time penalties still stack.
        let dq = |state: &AppState| {
            apply_command(
                state,
                Command::ApplyPenalty {
                    heat: heat(),
                    competitor: CompetitorRef("A".into()),
                    penalty: Penalty::Disqualify { reason: None },
                },
            )
        };
        assert!(dq(&state).ok);
        assert!(!dq(&state).ok, "stacked DQ must be rejected");
        for _ in 0..2 {
            assert!(
                apply_command(
                    &state,
                    Command::ApplyPenalty {
                        heat: heat(),
                        competitor: CompetitorRef("A".into()),
                        penalty: Penalty::TimeAdded { micros: 1_000_000 },
                    }
                )
                .ok,
                "time penalties stack by design"
            );
        }

        // Restore (void-the-void) is a sanctioned command path now.
        assert!(apply_command(&state, Command::VoidDetection { target: LogRef(p1) }).ok);
        let (events, _) = state.read().unwrap();
        let void_offset = events
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e, Event::DetectionVoided { .. }).then_some(i as u64))
            .unwrap();
        assert!(
            apply_command(
                &state,
                Command::VoidDetection {
                    target: LogRef(void_offset)
                }
            )
            .ok,
            "void-the-void (restore) must be accepted"
        );

        // An adjust landing EXACTLY on another pass's instant is rejected (a zero-duration
        // lap would fold — an impossible 0.000s best lap corrupting every ranking).
        let collide = apply_command(
            &state,
            Command::AdjustLap {
                target: LogRef(p2),
                at: SourceTime::from_micros(1_000_000), // p1's exact instant
            },
        );
        assert!(!collide.ok, "same-instant adjust must be rejected");
        // Re-asserting the pass's OWN instant is fine (exempt).
        assert!(
            apply_command(
                &state,
                Command::AdjustLap {
                    target: LogRef(p2),
                    at: SourceTime::from_micros(4_000_000),
                }
            )
            .ok
        );

        // A stale-run target is rejected after a Restart (the abandoned-run trap).
        assert!(apply_command(&state, Command::ForceEnd { heat: heat() }).ok);
        assert!(apply_command(&state, Command::Restart { heat: heat() }).ok);
        let stale = apply_command(&state, Command::VoidDetection { target: LogRef(p2) });
        assert!(
            !stale.ok,
            "a ruling on an abandoned run's pass must be rejected"
        );
        assert!(
            stale.error.unwrap().message.contains("ABANDONED"),
            "the rejection explains the staleness"
        );

        // Protests + heat-voids need a run: the reset heat (Scheduled again) rejects both.
        let protest = apply_command(
            &state,
            Command::FileProtest {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                note: "pre-run".into(),
            },
        );
        assert!(!protest.ok, "a protest needs a run to contest");
        let void_heat = apply_command(&state, Command::VoidHeat { heat: heat() });
        assert!(!void_heat.ok, "a heat-void needs a run to void");
    }

    #[test]
    fn a_protest_dies_with_the_run_it_contests() {
        // Filed against run 1, then the heat is Restarted (it re-races anyway): the protest
        // must NOT keep gating Finalize — the old whole-log predicate deadlocked the RD
        // (Finalize rejected over a protest no run-windowed view could even show).
        let h = heat();
        let running = |t| Event::HeatStateChanged {
            heat: h.clone(),
            transition: t,
        };
        let events = vec![
            running(HeatTransition::Running),
            Event::ProtestFiled {
                heat: h.clone(),
                competitor: CompetitorRef("A".into()),
                note: "run-1 grievance".into(),
            },
            running(HeatTransition::Finished),
            running(HeatTransition::Restarted),
            running(HeatTransition::Running), // the re-run
        ];
        assert_eq!(
            open_protest_count(&events, &h),
            0,
            "a pre-restart protest must not block the re-run's Finalize"
        );
        // A protest filed against the RE-RUN is open as usual.
        let mut with_new = events.clone();
        with_new.push(Event::ProtestFiled {
            heat: h.clone(),
            competitor: CompetitorRef("A".into()),
            note: "run-2 grievance".into(),
        });
        assert_eq!(open_protest_count(&with_new, &h), 1);
    }

    /// The **generalized** `ReverseRuling` (Slice 6) accepts a throw-out, a protest resolution, and
    /// a heat-void as targets — not just a penalty — and still rejects a non-ruling.
    #[test]
    fn reverse_ruling_accepts_any_ruling_target() {
        let mut log = InMemoryLog::default();
        // offset 0: the heat; offset 1: its run; offset 2: a pass INSIDE the run (rulings
        // are run-scoped now — a target before the run's start is rejected as stale).
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
        EventLog::append(
            &mut log,
            Event::HeatStateChanged {
                heat: heat(),
                transition: HeatTransition::Running,
            },
            None,
        )
        .unwrap();
        EventLog::append(&mut log, pass("A", 1_000_000, 1), None).unwrap();
        let state = AppState::new(log);

        // Append a throw-out (offset 3), a heat-void (offset 4) — both reversible rulings.
        assert!(apply_command(&state, Command::ThrowOutLap { target: LogRef(2) }).ok);
        assert!(apply_command(&state, Command::VoidHeat { heat: heat() }).ok);

        for target in [LogRef(3), LogRef(4)] {
            let ack = apply_command(&state, Command::ReverseRuling { target });
            assert!(ack.ok, "reversing ruling at {target:?} failed: {ack:?}");
        }
        // But reversing the pass at offset 2 (not a ruling) is rejected.
        let ack = apply_command(&state, Command::ReverseRuling { target: LogRef(2) });
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().code, ErrorCode::BadRequest);
    }

    // ── The official-result lock: no result-changing ruling on a Final heat ──────────────────

    /// The exact rejection every locked command answers on a Final heat.
    fn final_lock_message() -> String {
        "heat \"q-1\" result is official (Final) — Revert it to marshal".to_string()
    }

    /// Drive `q-1` (already scheduled) to **Final** on the bare `apply_command` path, banking one
    /// real pass while Running. Returns the state and the pass's global offset (a valid target
    /// for the offset-addressed commands).
    fn final_state_with_pass() -> (AppState, u64) {
        let state = scheduled_state();
        for cmd in [
            Command::Stage { heat: heat() },
            Command::Start { heat: heat() },
            Command::SkipCountdown { heat: heat() },
        ] {
            let ack = apply_command(&state, cmd.clone());
            assert!(ack.ok, "driving q-1 to Running via {cmd:?}: {ack:?}");
        }
        let pass_offset = state.append(pass("A", 1_000_000, 1), None).unwrap();
        for cmd in [
            Command::ForceEnd { heat: heat() },
            Command::Finalize { heat: heat() },
        ] {
            let ack = apply_command(&state, cmd.clone());
            assert!(ack.ok, "driving q-1 to Final via {cmd:?}: {ack:?}");
        }
        (state, pass_offset)
    }

    /// Every result-changing marshaling command is rejected on a **Final** heat with the exact
    /// "official — Revert it to marshal" BadRequest (appending nothing), and the SAME command is
    /// accepted after `Revert` re-opens the result to Unofficial.
    #[test]
    fn result_changing_commands_are_locked_on_a_final_heat_until_revert() {
        let (state, pass_offset) = final_state_with_pass();
        let commands = [
            Command::VoidHeat { heat: heat() },
            Command::ApplyPenalty {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::TimeAdded { micros: 2_000_000 },
            },
            Command::DeductPoints {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                points: 1,
            },
            // A heat-TAGGED insert names the Final heat directly.
            Command::InsertLap {
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(2_000_000),
                heat: Some(heat()),
            },
            // The offset-addressed commands resolve the pass's OWNING heat (positional → q-1).
            Command::VoidDetection {
                target: LogRef(pass_offset),
            },
            Command::AdjustLap {
                target: LogRef(pass_offset),
                at: SourceTime::from_micros(1_500_000),
            },
            Command::SplitLap {
                target: LogRef(pass_offset),
                at: SourceTime::from_micros(500_000),
            },
            Command::ThrowOutLap {
                target: LogRef(pass_offset),
            },
        ];

        // On the Final heat: every command bounces with the exact message, appending nothing.
        for cmd in &commands {
            let (before, _) = state.read().unwrap();
            let ack = apply_command(&state, cmd.clone());
            assert!(!ack.ok, "{cmd:?} must be rejected on a Final heat");
            let err = ack.error.expect("a failed ack carries the error");
            assert_eq!(err.code, ErrorCode::BadRequest, "{cmd:?}");
            assert_eq!(err.message, final_lock_message(), "{cmd:?}");
            let (after, _) = state.read().unwrap();
            assert_eq!(
                before.len(),
                after.len(),
                "{cmd:?} appended on a Final heat"
            );
        }

        // Revert (Final → Unofficial) is the sanctioned re-open…
        let ack = apply_command(&state, Command::Revert { heat: heat() });
        assert!(ack.ok, "Revert re-opens the result: {ack:?}");
        // …after which the very same commands are accepted.
        for cmd in &commands {
            let ack = apply_command(&state, cmd.clone());
            assert!(ack.ok, "{cmd:?} must be accepted after Revert: {ack:?}");
        }
    }

    /// The target-addressed resolution end to end: voiding a pass that belongs to a Final heat
    /// via its offset is rejected; the SAME offset is voidable after Revert.
    #[test]
    fn void_by_offset_is_locked_while_the_owning_heat_is_final() {
        let (state, pass_offset) = final_state_with_pass();

        let ack = apply_command(
            &state,
            Command::VoidDetection {
                target: LogRef(pass_offset),
            },
        );
        assert!(!ack.ok, "voiding a Final heat's pass must be rejected");
        assert_eq!(ack.error.unwrap().message, final_lock_message());

        assert!(apply_command(&state, Command::Revert { heat: heat() }).ok);
        let ack = apply_command(
            &state,
            Command::VoidDetection {
                target: LogRef(pass_offset),
            },
        );
        assert!(ack.ok, "the same offset voids after Revert: {ack:?}");
    }

    /// `ReverseRuling` is locked on a Final heat too — its owning heat resolves through the
    /// ruling chain (the reversal targets a penalty whose TAG names the Final heat).
    #[test]
    fn reverse_ruling_is_locked_via_the_ruling_chain_owning_heat() {
        // Bank a penalty while the heat is still Unofficial (legal), then finalize.
        let state = scheduled_state();
        for cmd in [
            Command::Stage { heat: heat() },
            Command::Start { heat: heat() },
            Command::SkipCountdown { heat: heat() },
            Command::ForceEnd { heat: heat() },
        ] {
            assert!(apply_command(&state, cmd).ok);
        }
        assert!(
            apply_command(
                &state,
                Command::ApplyPenalty {
                    heat: heat(),
                    competitor: CompetitorRef("A".into()),
                    penalty: Penalty::TimeAdded { micros: 1_000_000 },
                },
            )
            .ok
        );
        let (events, _) = state.read().unwrap();
        let penalty_offset = (events.len() - 1) as u64;
        assert!(matches!(events.last(), Some(Event::PenaltyApplied { .. })));
        assert!(apply_command(&state, Command::Finalize { heat: heat() }).ok);

        // Reversing the penalty would change the OFFICIAL result — rejected.
        let ack = apply_command(
            &state,
            Command::ReverseRuling {
                target: LogRef(penalty_offset),
            },
        );
        assert!(!ack.ok);
        assert_eq!(ack.error.unwrap().message, final_lock_message());

        // Revert, then the reversal lands.
        assert!(apply_command(&state, Command::Revert { heat: heat() }).ok);
        let ack = apply_command(
            &state,
            Command::ReverseRuling {
                target: LogRef(penalty_offset),
            },
        );
        assert!(ack.ok, "reversal after Revert: {ack:?}");
    }

    /// An UNTAGGED (legacy) `InsertLap` attributes positionally, so the lock checks the
    /// positionally-active heat at the log tail — Final rejects, post-Revert accepts, and an
    /// empty log (no heat to protect) always accepts.
    #[test]
    fn untagged_insert_lap_checks_the_positionally_active_heat_at_the_tail() {
        let insert = || Command::InsertLap {
            adapter: AdapterId("vd".into()),
            competitor: CompetitorRef("A".into()),
            at: SourceTime::from_micros(3_000_000),
            heat: None,
        };

        // The log tail sits inside q-1's span and q-1 is Final → the insertion would attribute
        // to the official result: rejected.
        let (state, _) = final_state_with_pass();
        let ack = apply_command(&state, insert());
        assert!(!ack.ok, "untagged insert on a Final tail must be rejected");
        assert_eq!(ack.error.unwrap().message, final_lock_message());

        // After Revert the same insertion lands.
        assert!(apply_command(&state, Command::Revert { heat: heat() }).ok);
        assert!(apply_command(&state, insert()).ok);

        // With no heat in the log at all there is nothing official to protect — allowed.
        let empty = AppState::new(InMemoryLog::default());
        assert!(apply_command(&empty, insert()).ok);
    }

    /// Protests are EXEMPT from the lock: filing and resolving change no result, so both stay
    /// legal on a Final heat (disputing an official result is what protests are for) — and the
    /// heat remains Final throughout.
    #[test]
    fn file_and_resolve_protest_are_allowed_on_a_final_heat() {
        use gridfpv_engine::heat::{HeatState, heat_state};
        use gridfpv_events::ProtestOutcome;

        let (state, _) = final_state_with_pass();

        let ack = apply_command(
            &state,
            Command::FileProtest {
                heat: heat(),
                competitor: CompetitorRef("A".into()),
                note: "protesting the official result".into(),
            },
        );
        assert!(ack.ok, "FileProtest on a Final heat: {ack:?}");
        let (events, _) = state.read().unwrap();
        let filed_offset = (events.len() - 1) as u64;
        assert!(matches!(events.last(), Some(Event::ProtestFiled { .. })));

        // Resolving it (here: denied) needs no Revert either.
        let ack = apply_command(
            &state,
            Command::ResolveProtest {
                target: LogRef(filed_offset),
                outcome: ProtestOutcome::Denied,
            },
        );
        assert!(ack.ok, "ResolveProtest on a Final heat: {ack:?}");

        // The result stayed official the whole time.
        let (events, _) = state.read().unwrap();
        assert_eq!(heat_state(&events, &heat()), Some(HeatState::Final));
    }

    /// `heat_of_offset` resolves an offset's owning heat by tag, by ruling-chain recursion, and
    /// positionally — mirroring `app::heat_window_offsets`' routing rules.
    #[test]
    fn heat_of_offset_resolves_tags_chains_and_positional_attribution() {
        use gridfpv_events::ProtestOutcome;

        let h1 = HeatId("h-1".into());
        let h2 = HeatId("h-2".into());
        let schedule = |h: &HeatId| Event::HeatScheduled {
            heat: h.clone(),
            lineup: vec![],
            class: None,
            round: None,
            frequencies: vec![],
            label: None,
        };
        let events = vec![
            pass("X", 500_000, 1),   // 0: a pass before ANY heat — unattributable
            schedule(&h1),           // 1: h1 opens
            pass("A", 1_000_000, 2), // 2: positional → h1
            Event::LapInserted {
                // 3: UNTAGGED legacy insert — positional → h1
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(1_500_000),
                heat: None,
            },
            schedule(&h2),           // 4: h2 opens (closes h1's span)
            pass("B", 2_000_000, 3), // 5: positional → h2
            Event::PenaltyApplied {
                // 6: TAGGED for h1 while h2 is active — tag beats position
                heat: h1.clone(),
                competitor: CompetitorRef("A".into()),
                penalty: Penalty::TimeAdded { micros: 1_000_000 },
            },
            Event::DetectionVoided { target: LogRef(2) }, // 7: chain → pass@2 → h1
            Event::ProtestFiled {
                // 8: tagged h1
                heat: h1.clone(),
                competitor: CompetitorRef("A".into()),
                note: "contact".into(),
            },
            Event::ProtestResolved {
                // 9: chain → filed@8 → h1
                target: LogRef(8),
                outcome: ProtestOutcome::Denied,
            },
            Event::RulingReversed { target: LogRef(9) }, // 10: chain, two hops → h1
            Event::LapInserted {
                // 11: TAGGED insert for h2
                adapter: AdapterId("vd".into()),
                competitor: CompetitorRef("B".into()),
                at: SourceTime::from_micros(2_500_000),
                heat: Some(h2.clone()),
            },
            Event::RulingReversed { target: LogRef(12) }, // 12: malformed SELF-target — bails
        ];

        assert_eq!(heat_of_offset(&events, LogRef(0)), None, "pre-heat pass");
        assert_eq!(heat_of_offset(&events, LogRef(2)), Some(h1.clone()));
        assert_eq!(
            heat_of_offset(&events, LogRef(3)),
            Some(h1.clone()),
            "untagged insert attributes positionally"
        );
        assert_eq!(heat_of_offset(&events, LogRef(5)), Some(h2.clone()));
        assert_eq!(
            heat_of_offset(&events, LogRef(6)),
            Some(h1.clone()),
            "the tag wins over the active span"
        );
        assert_eq!(heat_of_offset(&events, LogRef(7)), Some(h1.clone()));
        assert_eq!(
            heat_of_offset(&events, LogRef(10)),
            Some(h1.clone()),
            "ruling-chain recursion (reversal → resolution → filing)"
        );
        assert_eq!(heat_of_offset(&events, LogRef(11)), Some(h2.clone()));
        assert_eq!(heat_of_offset(&events, LogRef(99)), None, "out of range");
        assert_eq!(
            heat_of_offset(&events, LogRef(12)),
            None,
            "a malformed self-target bails instead of looping"
        );
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
                    layouts: Vec::new(),
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
                    min_lap_secs: None,
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
        use gridfpv_events::RoundId;

        let registry = EventRegistry::new(None).unwrap();
        let event = registry
            .create(&crate::events::CreateEventRequest::named("Test Event"))
            .unwrap()
            .id;
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

    // --- #395: the ack must say what the fill DID, not just that it was accepted -------------

    /// Build an event over a class with `pilots` and one round of `format` labelled `label`.
    /// Returns the registry, the event id, and the round id — enough to issue a `FillRound` and
    /// read its ack.
    #[cfg(test)]
    fn event_with_round(
        label: &str,
        format: &str,
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
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    layouts: Vec::new(),
                    label: label.into(),
                    classes: vec![class],
                    format: format.into(),
                    params: BTreeMap::from([("rounds".into(), "1".into())]),
                    win_condition: Some(WinCondition::FirstToLaps { n: 1 }),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(60),
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap();
        (registry, EventId(event.0.clone()), round.id)
    }

    /// Issue a single-step `FillRound` and return the outcome the ack carries.
    #[cfg(test)]
    fn fill_next(
        registry: &EventRegistry,
        event: &EventId,
        round: &gridfpv_events::RoundId,
    ) -> crate::control::FillRoundOutcome {
        let state = registry.resolve(event).unwrap();
        let ack = apply_command_in_event(
            registry,
            event,
            &state,
            Command::FillRound {
                round: round.clone(),
                mode: FillMode::Next,
            },
        );
        assert!(ack.ok, "FillRound rejected: {ack:?}");
        match ack.outcome {
            Some(CommandOutcome::FillRound(outcome)) => outcome,
            other => panic!("FillRound must report its outcome, got {other:?}"),
        }
    }

    /// The channels a heat is currently scheduled on, in seat order — its most recent
    /// `HeatScheduled`, the same "latest wins" rule every reader folds by.
    #[cfg(test)]
    fn channels_of(state: &AppState, heat: &HeatId) -> Vec<u16> {
        let (events, _) = state.read().unwrap();
        let mut out = Vec::new();
        for event in &events {
            if let Event::HeatScheduled {
                heat: h,
                frequencies,
                ..
            } = event
            {
                if h == heat {
                    out = frequencies.iter().map(|(_, f)| *f).collect();
                }
            }
        }
        out
    }

    /// The lineup a heat is currently seated with — its most recent `HeatScheduled`, the same
    /// "latest wins" rule [`channels_of`] and every reader fold by.
    #[cfg(test)]
    fn lineup_of(state: &AppState, heat: &HeatId) -> Vec<CompetitorRef> {
        let (events, _) = state.read().unwrap();
        let mut out = Vec::new();
        for event in &events {
            if let Event::HeatScheduled {
                heat: h, lineup, ..
            } = event
            {
                if h == heat {
                    out = lineup.clone();
                }
            }
        }
        out
    }

    #[test]
    fn a_rounds_heats_alternate_layouts_and_clearing_a_bind_restores_that_default() {
        // #117 S3's alternation end to end through the command path, including the *second*
        // `layouts.first()`: `SetHeatLayout { layout: None }` re-tunes the heat to its round's
        // default, and that default is now the heat's own place in the cycle. Dropping a cleared
        // heat 2 back onto the first layout would be the very bug this change removes, one
        // command later.
        use crate::events::{
            ChannelMode, LayoutNode, NewChannelLayoutRequest, NewRoundReq, SeedingRule,
        };
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let (registry, event, _warmup) =
            event_with_round("Warmup", "timed_qual", &["a", "b", "c", "d"]);

        // Two complete tunings of the Mock's eight nodes: the seeded Raceband order, and its
        // reverse — so which layout a heat flies is legible from its channels alone.
        let seeded = registry
            .add_channel_layout(
                &event,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        let a = seeded
            .layouts
            .iter()
            .find(|l| l.name == "Bracket A")
            .unwrap()
            .id
            .clone();
        let reversed: Vec<LayoutNode> = crate::channels::RACEBAND_MHZ
            .iter()
            .rev()
            .enumerate()
            .map(|(node, channel)| LayoutNode {
                node: node as u32,
                channel: *channel,
            })
            .collect();
        let both = registry
            .add_channel_layout(
                &event,
                NewChannelLayoutRequest {
                    name: "Bracket B".into(),
                    nodes: Some(reversed),
                },
            )
            .unwrap();
        let b = both
            .layouts
            .iter()
            .find(|l| l.name == "Bracket B")
            .unwrap()
            .id
            .clone();

        // A round flying both, in 2-up heats, so four pilots draw two heats.
        let classes = registry.meta_of(&event).unwrap().classes;
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    layouts: vec![a.clone(), b],
                    label: "Qualifying".into(),
                    classes,
                    format: "timed_qual".into(),
                    params: BTreeMap::from([
                        ("rounds".into(), "1".into()),
                        ("heat_size".into(), "2".into()),
                    ]),
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(60),
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap()
            .id;

        let first = fill_next(&registry, &event, &round);
        let second = fill_next(&registry, &event, &round);
        let h1 = first.scheduled[0].heat.clone();
        let h2 = second.scheduled[0].heat.clone();
        let state = registry.resolve(&event).unwrap();
        assert_eq!(channels_of(&state, &h1), vec![5658, 5695], "heat 1 flies A");
        assert_eq!(
            channels_of(&state, &h2),
            vec![5917, 5880],
            "heat 2 flies B — the alternation, not a second copy of A"
        );

        // The RD re-picks heat 2 onto A …
        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::SetHeatLayout {
                heat: h2.clone(),
                layout: Some(a),
            },
        );
        assert!(ack.ok, "{ack:?}");
        assert_eq!(channels_of(&state, &h2), vec![5658, 5695]);

        // … and then clears the bind. It goes back to B, the default for ITS position.
        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::SetHeatLayout {
                heat: h2.clone(),
                layout: None,
            },
        );
        assert!(ack.ok, "{ack:?}");
        assert_eq!(channels_of(&state, &h2), vec![5917, 5880]);
        assert_eq!(
            channels_of(&state, &h1),
            vec![5658, 5695],
            "heat 1 was never touched by any of it"
        );
    }

    /// #440: `OverrideHeatSeating { lineup: [] }` is the documented — and only — way OUT of a
    /// manual override: *"the heat is re-formed from its round's plan, exactly as if the RD had
    /// never touched it."*
    ///
    /// The clear has to re-form from the **round's plan**, not from the heat's most recent
    /// `HeatScheduled` — which, one command after an override, *is* the override. A clear that
    /// re-applies the very lineup it was asked to discard is a clear that does nothing, and
    /// nothing else re-forms the heat: the round's own fill returns `AlreadyScheduled` for it, so
    /// the "cleared" heat races the override unless an unrelated round edit later rematerializes
    /// it.
    ///
    /// **Two defects, in order.** Today the command does not even get that far:
    /// `require_distinct_lineup` runs before the empty-lineup branch and rejects the clear as "a
    /// heat needs at least one competitor in its lineup", so the documented escape hatch is
    /// unreachable. Behind that guard sits the re-application itself. This test fails on the
    /// first today and on the second once the guard is fixed, which is the order they must be
    /// fixed in.
    #[test]
    #[ignore = "known bug #440: the clear re-applies the override's own lineup — un-ignore with the fix"]
    fn clearing_a_seating_override_re_forms_the_heat_from_its_rounds_plan() {
        let (registry, event, round) = event_with_round(
            "Qualifying",
            "timed_qual",
            &["alpha", "bravo", "charlie", "delta"],
        );
        let filled = fill_next(&registry, &event, &round);
        let heat = filled.scheduled[0].heat.clone();
        let plan = filled.scheduled[0].lineup.clone();
        assert!(
            plan.len() > 2,
            "the round's plan must seat more than the override does, or the clear proves \
             nothing: {plan:?}"
        );
        let state = registry.resolve(&event).unwrap();
        assert_eq!(lineup_of(&state, &heat), plan);

        // The RD re-seats the heat by hand — two of the field, in the other order.
        let by_hand = vec![plan[1].clone(), plan[0].clone()];
        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::OverrideHeatSeating {
                heat: heat.clone(),
                lineup: by_hand.clone(),
                frequencies: vec![],
            },
        );
        assert!(ack.ok, "{ack:?}");
        assert_eq!(
            lineup_of(&state, &heat),
            by_hand,
            "the override is in force before it is cleared"
        );

        // … and then clears it.
        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::OverrideHeatSeating {
                heat: heat.clone(),
                lineup: vec![],
                frequencies: vec![],
            },
        );
        assert!(
            ack.ok,
            "an empty lineup is the documented clear, not an empty heat — it must be accepted: \
             {ack:?}"
        );

        let (events, _) = state.read().unwrap();
        assert!(
            crate::round_engine::heat_seating_override(&events, &heat).is_none(),
            "an empty lineup clears the override in the fold …"
        );
        assert_eq!(
            lineup_of(&state, &heat),
            plan,
            "… so the heat must be seated back to its round's plan, exactly as if the RD had \
             never touched it"
        );
        // The channels follow the lineup they were re-formed for — one per seat, no pilot left
        // holding a channel assigned for a lineup the heat no longer has.
        let seated: Vec<CompetitorRef> = {
            let mut out = Vec::new();
            for event in &events {
                if let Event::HeatScheduled {
                    heat: h,
                    frequencies,
                    ..
                } = event
                {
                    if h == &heat {
                        out = frequencies.iter().map(|(c, _)| c.clone()).collect();
                    }
                }
            }
            out
        };
        assert_eq!(
            seated, plan,
            "every planned pilot has a channel: {seated:?}"
        );
    }

    /// An event over `pilots` with one round: `timed_qual`, naming `layouts`, in `channel_mode`.
    /// Each pilot is seated on the matching entry of `channels` at **membership** — the fixed
    /// per-member channel a [`ChannelMode::Static`] round races on.
    ///
    /// Returns the registry, the event id and the round id.
    #[cfg(test)]
    fn event_with_static_channels(
        pilots: &[(&str, u16)],
    ) -> (EventRegistry, EventId, gridfpv_events::RoundId) {
        use crate::classes::CreateClassRequest;
        use crate::events::{
            ChannelMode, CreateEventRequest, MemberSlot, NewRoundReq, SeedingRule,
        };
        use crate::pilots::CreatePilotRequest;
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let registry = EventRegistry::new(None).unwrap();
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
        let members: Vec<MemberSlot> = pilots
            .iter()
            .map(|(cs, channel)| MemberSlot {
                pilot: registry
                    .pilots()
                    .create(&CreatePilotRequest {
                        callsign: (*cs).into(),
                        ..Default::default()
                    })
                    .unwrap()
                    .id,
                channel: Some(*channel),
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
            .set_class_membership(&event, class.clone(), members)
            .unwrap();
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    // NO layouts: the round says nothing about channels, because its members
                    // already carry their own.
                    layouts: Vec::new(),
                    label: "Qualifying".into(),
                    classes: vec![class],
                    format: "timed_qual".into(),
                    params: BTreeMap::from([("rounds".into(), "1".into())]),
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(60),
                    channel_mode: Some(ChannelMode::Static),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap();
        (registry, EventId(event.0.clone()), round.id)
    }

    /// #441: `SetHeatLayout { layout: None }` must record the **cleared** bind.
    ///
    /// `heat_layout_bind` is three-valued on purpose — `Some(Some(l))` is *"the RD bound this
    /// heat"*, `Some(None)` is *"the RD cleared it"*, `None` is *"never touched"*. The clear
    /// writes `Some(Some(current_default))` instead, so the middle state is unreachable and a
    /// cleared heat is indistinguishable from one the RD deliberately pinned to that layout. That
    /// is what freezes the heat against a later round edit (the same defect as
    /// `editing_a_rounds_layouts_re_tunes_the_heats_it_generated` in `events.rs`), and it means
    /// "clear" is a write the RD cannot undo by clearing again.
    ///
    /// Clearing still leaves the heat flying its round's default — that is what a cleared bind
    /// *means*, and both halves are asserted here so a fix cannot satisfy one by breaking the
    /// other.
    #[test]
    #[ignore = "known bug #441: SetHeatLayout{layout:None} writes an explicit bind to the current default — un-ignore with the fix"]
    fn clearing_a_heats_layout_bind_records_the_cleared_state() {
        use crate::events::{ChannelMode, NewChannelLayoutRequest, NewRoundReq, SeedingRule};
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let (registry, event, _warmup) = event_with_round("Warmup", "timed_qual", &["a", "b"]);
        let seeded = registry
            .add_channel_layout(
                &event,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        let a = seeded
            .layouts
            .iter()
            .find(|l| l.name == "Bracket A")
            .unwrap()
            .id
            .clone();
        let classes = registry.meta_of(&event).unwrap().classes;
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    layouts: vec![a.clone()],
                    label: "Qualifying".into(),
                    classes,
                    format: "timed_qual".into(),
                    params: BTreeMap::from([("rounds".into(), "1".into())]),
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(60),
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap()
            .id;

        let filled = fill_next(&registry, &event, &round);
        let heat = filled.scheduled[0].heat.clone();
        let state = registry.resolve(&event).unwrap();
        assert_eq!(
            channels_of(&state, &heat),
            vec![5658, 5695],
            "the generated heat flies the round's only layout"
        );

        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::SetHeatLayout {
                heat: heat.clone(),
                layout: None,
            },
        );
        assert!(ack.ok, "{ack:?}");

        let (events, _) = state.read().unwrap();
        assert_eq!(
            crate::round_engine::heat_layout_bind(&events, &heat),
            Some(None),
            "the documented cleared bind — not a fresh explicit bind to whatever the default \
             happens to be right now"
        );
        // …and a cleared bind still resolves to the round's default, which is what it means.
        let meta = registry.meta_of(&event).unwrap();
        let round_def = meta.rounds.iter().find(|r| r.id == round).unwrap();
        assert_eq!(
            crate::round_engine::layout_for_heat(&meta, Some(round_def), &events, &heat)
                .map(|l| l.id.clone()),
            Some(a),
            "a cleared bind falls back to the round's default layout"
        );
        assert_eq!(
            channels_of(&state, &heat),
            vec![5658, 5695],
            "so the heat's channels do not move"
        );
    }

    /// #441 (the bonus defect): clearing a layout bind on a round that names **no** layouts must
    /// not blow away a `Static`-mode heat's fixed channels.
    ///
    /// A Static round's channels come from **membership** — each pilot's own assigned frequency —
    /// not from a layout. The clear runs `assign_for_event(None)` with no channel-mode check, so
    /// it hands the heat a fresh IMD auto-pick from the timer's pool and every pilot in it is
    /// silently moved off the channel their VTX is actually on.
    #[test]
    #[ignore = "known bug #441: the layout-less clear auto-picks over a Static heat's membership channels — un-ignore with the fix"]
    fn clearing_a_bind_on_a_layout_less_round_keeps_a_static_heats_fixed_channels() {
        // Two adjacent Raceband channels — a pair the IMD auto-pick would never choose, so a
        // clobber is unmistakable.
        let (registry, event, round) =
            event_with_static_channels(&[("alpha", 5732), ("bravo", 5769)]);
        let filled = fill_next(&registry, &event, &round);
        let heat = filled.scheduled[0].heat.clone();
        let state = registry.resolve(&event).unwrap();
        let fixed = channels_of(&state, &heat);
        assert_eq!(
            fixed,
            vec![5732, 5769],
            "the Static heat races its members' own assigned channels"
        );

        let ack = apply_command_in_event(
            &registry,
            &event,
            &state,
            Command::SetHeatLayout {
                heat: heat.clone(),
                layout: None,
            },
        );
        assert!(ack.ok, "{ack:?}");

        assert_eq!(
            channels_of(&state, &heat),
            fixed,
            "there was no layout to clear — a Static heat's channels are its members', and \
             moving a pilot off the frequency their VTX is on is exactly what must not happen \
             as a side effect"
        );
    }

    /// The bug, stated as the assertion that was impossible before (#395): a fill that scheduled
    /// a heat and a fill that scheduled nothing must be told apart **from the response alone** —
    /// no diffing the event log afterwards.
    ///
    /// Both used to answer a byte-identical `{"ok":true}`, which is what sent an API caller
    /// hunting downstream through the log, the projection and the read routes for a bug that was
    /// never there.
    #[test]
    fn a_productive_fill_and_a_no_op_fill_are_distinguishable_from_the_ack() {
        // Productive: two pilots, head-to-head, nothing scheduled yet → one heat drawn.
        let (registry, event, round) = event_with_round("Test Round", "head_to_head", &["a", "b"]);
        let drew = fill_next(&registry, &event, &round);
        assert_eq!(drew.stopped, FillStop::SingleStep);
        assert_eq!(drew.scheduled.len(), 1, "{drew:?}");
        // The heat is identified BOTH ways: a wire handle to address it with, and the friendly
        // name a message prints (repo display rule — a raw heat id must never reach a user).
        let heat = &drew.scheduled[0];
        assert_eq!(heat.name, "Test Round Heat 1", "{heat:?}");
        assert_eq!(heat.lineup.len(), 2);
        assert!(!heat.heat.0.is_empty());
        assert!(drew.detail.contains("Test Round Heat 1"), "{}", drew.detail);

        // No-op: fill again with the drawn heat still unscored → nothing appended, and the ack
        // says WHY rather than repeating the previous answer verbatim.
        let waited = fill_next(&registry, &event, &round);
        assert_eq!(waited.stopped, FillStop::AwaitingResult);
        assert!(waited.scheduled.is_empty(), "{waited:?}");
        assert!(
            waited.detail.contains("not been scored"),
            "the no-op must name its cause: {}",
            waited.detail
        );

        // The two acks are different values — the property the issue asks for, asserted directly.
        assert_ne!(drew, waited);
    }

    /// #394 + #395 together, which is how they were hit: a one-pilot head-to-head fill acks ok,
    /// schedules nothing, and the ack **names the real reason** instead of the round being
    /// "complete or awaiting a score" on a round where nothing has raced.
    #[test]
    fn a_one_pilot_head_to_head_fill_acks_the_shortfall_not_completion() {
        let (registry, event, round) = event_with_round("Test Round", "head_to_head", &["solo"]);
        let blocked = fill_next(&registry, &event, &round);

        assert_eq!(blocked.stopped, FillStop::Blocked);
        assert!(blocked.scheduled.is_empty(), "{blocked:?}");
        // The sentence an RD reads: the round by its LABEL (never its id), the shortfall, and the
        // format that fits a solo pilot.
        let detail = &blocked.detail;
        assert!(detail.starts_with("Test Round:"), "{detail}");
        assert!(detail.contains("Head-to-Head"), "{detail}");
        assert!(detail.contains("at least 2"), "{detail}");
        assert!(detail.contains("has 1"), "{detail}");
        assert!(detail.contains("timed_qual"), "{detail}");
        assert!(
            !detail.contains("complete"),
            "the round is NOT complete: {detail}"
        );
        assert!(
            !detail.contains(round.0.as_str()),
            "no raw round id: {detail}"
        );

        // And nothing was appended — the ack is the only place this was ever visible.
        let state = registry.resolve(&event).unwrap();
        let (events, _) = state.read().unwrap();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::HeatScheduled { .. })),
            "a blocked fill schedules nothing"
        );
    }

    /// The ack stays **additive**: a command with no interesting effect omits `outcome` entirely,
    /// so its wire form is byte-identical to before and every existing client keeps parsing.
    #[test]
    fn an_ordinary_ack_carries_no_outcome_and_serializes_unchanged() {
        let ok = serde_json::to_string(&CommandAck::ok()).unwrap();
        assert_eq!(ok, r#"{"ok":true}"#);
        let failed = CommandAck::failed(ProtocolError::new(ErrorCode::BadRequest, "nope"));
        assert!(
            !serde_json::to_string(&failed).unwrap().contains("outcome"),
            "a failure's answer is `error`, not an outcome"
        );
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
                    layouts: Vec::new(),
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
                    min_lap_secs: None,
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

    /// `FillRound` on a timer with **no allowed channels** refuses, names the timer, and appends
    /// nothing (#117 S1, #402).
    ///
    /// This is the bench case and it is the whole point of S1: both RotorHazard timers report
    /// `Flexible` with an EMPTY `available_channels`, so before this the fill *succeeded* and
    /// scheduled a heat in which **not one pilot had a channel**. Silence at exactly the moment the
    /// RD could still fix it. Now the configuration gap reaches them as a `400` that says which
    /// timer and where to go.
    #[test]
    fn fill_round_refuses_when_the_timer_has_no_allowed_channels() {
        use crate::timers::{ChannelCapability, CreateTimerRequest, TimerKind};
        let timer_req = CreateTimerRequest {
            name: "NuclearHazard".into(),
            kind: TimerKind::Mock { laps: 1, lap_ms: 1 },
            // The bench shape: flexible capability, nothing configured.
            channel_capability: Some(ChannelCapability::Flexible),
            node_count: Some(8),
            available_channels: None,
        };
        let (registry, event_id, round) =
            event_with_timer_and_round(timer_req, &["alpha", "bravo"]);
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
        assert!(
            !ack.ok,
            "an unconfigured timer must not silently seat a heat"
        );
        let err = ack.error.unwrap();
        assert_eq!(err.code, ErrorCode::BadRequest);
        assert!(
            err.message.contains("NuclearHazard") && err.message.contains("Timers page"),
            "the RD must be told WHICH timer and WHERE to fix it: {}",
            err.message
        );
        assert_eq!(
            before,
            state.read().unwrap().0.len(),
            "a refused FillRound appends nothing — no un-channelled heat is scheduled"
        );
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
                    layouts: Vec::new(),
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
                    min_lap_secs: None,
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
                    layouts: Vec::new(),
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
                    min_lap_secs: None,
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
        // An open-practice round: node-seated (`ActiveNodes` seeding), no class membership.
        // Its single channel heat is one draw — the (dynamic) format the RD single-steps, never
        // fill-all.
        let round = registry
            .add_round(
                &event,
                NewRoundReq {
                    layouts: Vec::new(),
                    label: "Open Practice".into(),
                    classes: vec![],
                    format: "open_practice".into(),
                    params: BTreeMap::new(),
                    win_condition: None,
                    seeding: SeedingRule::ActiveNodes {
                        nodes: vec![0, 1, 2],
                    },
                    time_limit_secs: None,
                    channel_mode: None,
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
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

    /// #439: `Advance` must never load a heat whose round the event no longer defines.
    ///
    /// Removing a round is allowed while all its heats are still `Scheduled` (#418), and it is
    /// documented to drop those heats "from every list the console reads". But the RD does not
    /// reach the next heat through a list — they press Advance. Selecting a ghost heat loads a
    /// race whose round config (layouts, staging timer, min-lap) is gone from event meta and
    /// which appears on no console screen to be fixed or skipped.
    ///
    /// The scratch round's heat is drawn **after** the keeper round's heat 1 and before anything
    /// the keeper draws next, so it is exactly the heat `on_deck` reaches for first.
    #[test]
    fn advance_never_loads_a_removed_rounds_heat() {
        use crate::events::{ChannelMode, NewRoundReq, SeedingRule};
        use gridfpv_engine::scoring::WinCondition;
        use std::collections::BTreeMap;

        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();

        // Heat 1 of the round that stays.
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
        let heat1 = heat_ids_in_round(&state, &round)[0].clone();

        // A scratch round over the same field, filled once — its heat lands between heat 1 and
        // whatever the keeper round draws next.
        let classes = registry.meta_of(&event_id).unwrap().classes;
        let scratch = registry
            .add_round(
                &event_id,
                NewRoundReq {
                    layouts: Vec::new(),
                    label: "Scratch".into(),
                    classes,
                    format: "head_to_head".into(),
                    params: BTreeMap::from([("group_size".into(), "2".into())]),
                    win_condition: Some(WinCondition::BestLap),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(60),
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap();
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::FillRound {
                    round: scratch.id.clone(),
                    mode: FillMode::Next,
                },
            )
            .ok
        );
        let ghost = heat_ids_in_round(&state, &scratch.id)[0].clone();

        // The RD throws the scratch round away. Nothing of it has raced, so it goes (#418).
        registry
            .remove_round(&event_id, &scratch.id)
            .expect("a round whose heats are all still Scheduled removes");

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

        // Neither the ack nor Live control may name the ghost …
        if let Some(CommandOutcome::Advance(outcome)) = &ack.outcome {
            assert_ne!(
                outcome.loaded.as_ref().map(|h| h.heat.clone()),
                Some(ghost.clone()),
                "the ack names a heat of a round this event no longer defines: {outcome:?}"
            );
        }
        assert_ne!(
            current_heat_of(&state),
            Some(ghost.clone()),
            "Advance loaded a heat whose round was removed — it is on no console screen and its \
             round config is gone from event meta"
        );

        // … and positively: Advance moved on within the round that still exists.
        let loaded = current_heat_of(&state).expect("Advance loaded a heat");
        assert!(
            heat_ids_in_round(&state, &round).contains(&loaded),
            "Advance must move on within a round the event still defines, got {loaded:?}"
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

    // --- #401: the ack must say what the Advance DID, not just that it was accepted ----------

    /// Advance `heat` through the control path and return the outcome the ack carries. Panics if
    /// the command was rejected or reported no outcome — every accepted `Advance` must report one.
    #[cfg(test)]
    fn advance_outcome(
        registry: &EventRegistry,
        event_id: &EventId,
        state: &AppState,
        heat: &HeatId,
    ) -> crate::control::AdvanceOutcome {
        let ack = apply_command_in_event(
            registry,
            event_id,
            state,
            Command::Advance { heat: heat.clone() },
        );
        assert!(ack.ok, "Advance rejected: {ack:?}");
        match ack.outcome {
            Some(CommandOutcome::Advance(outcome)) => outcome,
            other => panic!("Advance must report its outcome, got {other:?}"),
        }
    }

    /// The bug, stated as the assertion that was impossible before (#401): the **three** things an
    /// `Advance` can do — load the heat already on deck, generate the next one, find nothing to
    /// advance to — must be told apart **from the ack alone**.
    ///
    /// All three used to answer a byte-identical `{"ok":true}`. The third is the routine end of
    /// every round, so the ambiguity was not a rare misconfiguration but the normal case.
    #[test]
    fn the_three_advance_outcomes_are_distinguishable_from_the_ack_alone() {
        // A round whose two heats are BOTH pre-scheduled: advancing heat 1 loads the on-deck
        // heat 2, and advancing heat 2 finds nothing.
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
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
        let (heat1, heat2) = (heats[0].clone(), heats[1].clone());

        drive_heat_to_final(&registry, &event_id, &state, &heat1);
        let loaded = advance_outcome(&registry, &event_id, &state, &heat1);
        drive_heat_to_final(&registry, &event_id, &state, &heat2);
        let nothing = advance_outcome(&registry, &event_id, &state, &heat2);

        // A second, identically-seeded round taken down the generate path instead: only heat 1 is
        // filled, so advancing it has to DRAW heat 2 rather than find it.
        let (gen_registry, gen_event, gen_round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let gen_state = gen_registry.resolve(&gen_event).unwrap();
        assert!(
            apply_command_in_event(
                &gen_registry,
                &gen_event,
                &gen_state,
                Command::FillRound {
                    round: gen_round.clone(),
                    mode: FillMode::Next,
                },
            )
            .ok
        );
        let gen_heat1 = heat_ids_in_round(&gen_state, &gen_round)[0].clone();
        drive_heat_to_final(&gen_registry, &gen_event, &gen_state, &gen_heat1);
        let generated = advance_outcome(&gen_registry, &gen_event, &gen_state, &gen_heat1);

        // Each says positively what it did — no two alike, and none of them inferred from what is
        // missing.
        assert_eq!(loaded.stopped, AdvanceStop::LoadedOnDeck);
        assert_eq!(generated.stopped, AdvanceStop::Generated);
        assert_eq!(nothing.stopped, AdvanceStop::RoundComplete);
        assert_ne!(loaded.stopped, generated.stopped);
        assert_ne!(loaded.stopped, nothing.stopped);
        assert_ne!(generated.stopped, nothing.stopped);
    }

    /// The on-deck case **names the heat it loaded** — by its friendly name, never a raw id
    /// (repo display rule) — and hands back the wire handle alongside it.
    #[test]
    fn advance_to_an_on_deck_heat_names_the_heat_it_loaded() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
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
        let (heat1, heat2) = (heats[0].clone(), heats[1].clone());
        drive_heat_to_final(&registry, &event_id, &state, &heat1);

        let outcome = advance_outcome(&registry, &event_id, &state, &heat1);
        assert_eq!(outcome.stopped, AdvanceStop::LoadedOnDeck);
        let loaded = outcome.loaded.expect("the on-deck heat is named");
        assert_eq!(loaded.heat, heat2, "the wire handle addresses the heat");
        assert_eq!(loaded.name, "Round Robin Heat 2", "{loaded:?}");
        assert_eq!(loaded.lineup.len(), 2, "the lineup it was scheduled with");
        // The sentence an RD reads: both heats by friendly name, no raw id anywhere in it.
        let detail = &outcome.detail;
        assert!(detail.contains("Round Robin Heat 1"), "{detail}");
        assert!(detail.contains("Round Robin Heat 2"), "{detail}");
        assert!(detail.contains("on deck"), "{detail}");
        assert!(
            !detail.contains(heat1.0.as_str()),
            "no raw heat id: {detail}"
        );
        assert!(
            !detail.contains(heat2.0.as_str()),
            "no raw heat id: {detail}"
        );
    }

    /// The generate case says it **generated** the heat (not merely that one was loaded) and names
    /// it — the distinction between "the RD's fill already covered this" and "Advance drew it".
    #[test]
    fn advance_that_generates_the_next_heat_names_what_it_drew() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
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
        let heat1 = heat_ids_in_round(&state, &round)[0].clone();
        drive_heat_to_final(&registry, &event_id, &state, &heat1);

        let outcome = advance_outcome(&registry, &event_id, &state, &heat1);
        assert_eq!(outcome.stopped, AdvanceStop::Generated);
        let drawn = outcome.loaded.expect("the generated heat is named");
        assert_eq!(drawn.name, "Round Robin Heat 2", "{drawn:?}");
        // It really is the heat the log gained, and the one Live control now sits on.
        let heats = heat_ids_in_round(&state, &round);
        assert_eq!(heats.len(), 2, "Advance drew the next heat");
        assert_eq!(drawn.heat, heats[1]);
        assert_eq!(current_heat_of(&state), Some(drawn.heat.clone()));
        assert!(
            outcome
                .detail
                .contains("generated and loaded Round Robin Heat 2"),
            "{}",
            outcome.detail
        );
    }

    /// The case the issue calls out as worst: "nothing to advance to" is the **routine** end of a
    /// round, and it must be stated positively — the reason in the ack, not inferred from a
    /// missing field (the exact shape of the original bug).
    #[test]
    fn advance_with_nothing_to_advance_to_says_so_and_says_why() {
        let (registry, event_id, round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
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
        let (heat1, heat2) = (heats[0].clone(), heats[1].clone());
        drive_heat_to_final(&registry, &event_id, &state, &heat1);
        advance_outcome(&registry, &event_id, &state, &heat1);
        drive_heat_to_final(&registry, &event_id, &state, &heat2);

        let outcome = advance_outcome(&registry, &event_id, &state, &heat2);
        // Positively stated: the discriminator IS the answer. `loaded` being empty is a
        // consequence, never the signal a caller has to read.
        assert_eq!(outcome.stopped, AdvanceStop::RoundComplete);
        assert!(outcome.loaded.is_none(), "{outcome:?}");
        let detail = &outcome.detail;
        assert!(detail.contains("nothing to advance to"), "{detail}");
        // Named by their friendly names — the round by its LABEL, the heat by its display name.
        assert!(detail.contains("Round Robin Heat 2"), "{detail}");
        assert!(detail.contains("Round Robin is complete"), "{detail}");
        assert!(
            !detail.contains(round.0.as_str()),
            "no raw round id: {detail}"
        );
        assert!(
            !detail.contains(heat2.0.as_str()),
            "no raw heat id: {detail}"
        );
        // And "nothing to advance to" is still an ok — the `Advanced` transition happened.
        assert_eq!(
            heat_state(&state, &heat2),
            Some(gridfpv_engine::heat::HeatState::Final)
        );
    }

    /// A manually built, **untagged** heat has no round to draw from — a different answer from
    /// "the round is complete", and one the bare ok could not distinguish at all.
    #[test]
    fn advance_on_an_untagged_heat_reports_that_it_has_no_round() {
        use gridfpv_events::CompetitorRef;

        let (registry, event_id, _round) =
            round_robin_event(&["alpha", "bravo", "charlie", "delta"], 2);
        let state = registry.resolve(&event_id).unwrap();
        let heat = HeatId("free-1".into());
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::ScheduleHeat {
                    heat: heat.clone(),
                    lineup: vec![
                        CompetitorRef("node-0".into()),
                        CompetitorRef("node-1".into())
                    ],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: Some("Grudge Match".into()),
                },
            )
            .ok
        );
        drive_heat_to_final(&registry, &event_id, &state, &heat);

        let outcome = advance_outcome(&registry, &event_id, &state, &heat);
        assert_eq!(outcome.stopped, AdvanceStop::Untagged);
        assert!(outcome.loaded.is_none(), "{outcome:?}");
        // Even here the heat is named the way the RD named it, not by its id (display rule).
        let detail = &outcome.detail;
        assert!(detail.contains("Grudge Match"), "{detail}");
        assert!(detail.contains("not part of a round"), "{detail}");
        assert!(!detail.contains("free-1"), "no raw heat id: {detail}");
    }

    /// The additive property #395 established, re-checked with `Advance` in the enum: a command
    /// whose acceptance IS the whole answer still acks a byte-identical bare `{"ok":true}`.
    #[test]
    fn a_command_with_nothing_to_report_still_acks_a_bare_ok() {
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
        let heat = heat_ids_in_round(&state, &round)[0].clone();
        let ack = apply_command_in_event(&registry, &event_id, &state, Command::Stage { heat });
        assert!(ack.ok);
        assert_eq!(serde_json::to_string(&ack).unwrap(), r#"{"ok":true}"#);
    }

    // ── The arm-time GridFPV-plugin backstop (#405) ───────────────────────────

    /// An event from [`round_robin_event`] with a **RotorHazard timer added to its selection**,
    /// selected while its plugin was `Present` — the state the arm-time backstop guards. Returns
    /// the registry, event id, app state and the first heat of the (already filled) round, staged
    /// and ready to arm.
    fn event_with_a_selected_rh_timer() -> (
        EventRegistry,
        EventId,
        AppState,
        HeatId,
        crate::timers::TimerId,
    ) {
        use crate::timers::{CreateTimerRequest, PluginPresence, TimerKind};

        let (registry, event_id, round) = round_robin_event(&["alpha", "bravo"], 2);
        let state = registry.resolve(&event_id).unwrap();
        let rh = registry
            .timers()
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .unwrap();
        // Selected while healthy — a legitimate selection under the #405 gate.
        registry.timers().set_plugin(
            &rh.id,
            PluginPresence::Present {
                plugin_version: "0.1.0".into(),
                rhapi_version: "1.4".into(),
                capabilities: vec!["hello".into()],
            },
        );
        let mut selection = registry.timers_of(&event_id).unwrap();
        selection.push(rh.id.clone());
        registry.set_timers(&event_id, selection).unwrap();

        apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::FillRound {
                round: round.clone(),
                mode: FillMode::Next,
            },
        );
        let heat = heat_ids_in_round(&state, &round)[0].clone();
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::Stage { heat: heat.clone() }
            )
            .ok,
            "staging is never gated — the gate is at selection, and the backstop is at the arm"
        );
        (registry, event_id, state, heat, rh.id)
    }

    #[test]
    fn arming_is_refused_once_a_selected_rh_timers_plugin_stops_answering() {
        // #405: a plugin can disappear **after** a valid selection (RH restarted without it, or it
        // failed to load). The selection was legitimate when it was made, so the refusal moves to
        // the last point before Grid commits to a live race — the arm.
        use crate::timers::PluginPresence;
        let (registry, event_id, state, heat, rh) = event_with_a_selected_rh_timer();

        // Present → the arm goes through.
        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Start { heat: heat.clone() },
        );
        assert!(ack.ok, "a Present plugin arms normally: {ack:?}");

        // Restart the heat so it can be armed again, then take the plugin away.
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::Restart { heat: heat.clone() }
            )
            .ok
        );
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::Stage { heat: heat.clone() }
            )
            .ok
        );
        registry.timers().set_plugin(&rh, PluginPresence::Missing);

        let ack = apply_command_in_event(
            &registry,
            &event_id,
            &state,
            Command::Start { heat: heat.clone() },
        );
        assert!(!ack.ok, "a vanished plugin must refuse the arm");
        let message = ack.error.unwrap().message;
        assert!(message.contains("Field RH"), "{message}");
        assert!(message.contains("no longer answering"), "{message}");
        assert!(!message.contains(&rh.0), "no raw timer id: {message}");

        // Nothing was appended — the heat is still Staged and can be armed once the plugin is back.
        assert_eq!(
            heat::heat_state(&state.read().unwrap().0, &heat),
            Some(gridfpv_engine::heat::HeatState::Staged)
        );
    }

    #[test]
    fn the_arm_backstop_says_connect_it_when_the_timer_was_never_probed() {
        // `plugin: None` is a different problem with a different fix — a Director restart resets
        // presence to "never probed", and the answer is "connect it", not "install a plugin".
        let (registry, event_id, state, heat, rh) = event_with_a_selected_rh_timer();
        // Re-pointing the timer at a new URL is what the Director does on a reconfigure: it wipes
        // the live presence back to `None` pending a re-probe (#382).
        registry
            .timers()
            .update(
                &rh,
                &crate::timers::UpdateTimerRequest {
                    kind: Some(crate::timers::TimerKind::Rotorhazard {
                        url: "http://other-rh.local:5000".into(),
                    }),
                    ..Default::default()
                },
            )
            .unwrap();

        let ack = apply_command_in_event(&registry, &event_id, &state, Command::Start { heat });
        assert!(!ack.ok);
        let message = ack.error.unwrap().message;
        assert!(message.contains("Field RH"), "{message}");
        assert!(message.contains("not connected"), "{message}");
        assert!(!message.contains("no longer answering"), "{message}");
    }

    #[test]
    fn the_arm_backstop_leaves_mock_only_events_alone() {
        // Mock timers are unaffected (#405): the whole existing Stage → Start path over the
        // built-in sim must be untouched.
        let (registry, event_id, round) = round_robin_event(&["alpha", "bravo"], 2);
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
        let heat = heat_ids_in_round(&state, &round)[0].clone();
        assert!(
            apply_command_in_event(
                &registry,
                &event_id,
                &state,
                Command::Stage { heat: heat.clone() }
            )
            .ok
        );
        assert!(apply_command_in_event(&registry, &event_id, &state, Command::Start { heat }).ok);
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
