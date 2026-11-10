//! The Director's HTTP API — one axum [`Router`] over the [`EventRegistry`].
//!
//! This is the skeleton surface: everything here survives the rebuild from the primitives
//! in `docs/primitives.md`, and the competition routes are added back on top of it.
//!
//! | route | what |
//! |-------|------|
//! | `GET /health` | liveness (`ok`) |
//! | `GET /about` | which build is running, and its contract version |
//! | `GET /time` | the Director's wall clock, in epoch microseconds |
//! | `GET /events`, `POST /events`, `DELETE /events/{id}` | the event registry |
//! | `GET /active-event`, `PUT /active-event` | the Director's active event |
//! | `POST /events/{id}/auth/join-token` | mint a read-only join token |
//! | `GET /events/{id}/state` | the event's competition snapshot |
//! | `POST /events/{id}/commands` | one console command: checked, then logged as facts |
//! | `POST /events/{id}/presets/bracket`, `…/presets/mains` | generate phases from a preset |
//! | `POST /events/{id}/passes` | a gate crossing from outside (tools and tests) |
//! | `GET /events/{id}/changes` | server-sent events: the log cursor, each time it moves |
//! | `GET /pilots`, `POST /pilots`, `PUT /pilots/{id}`, `DELETE /pilots/{id}` | the pilot directory |
//! | `GET /classes`, `POST /classes`, `PUT /classes/{id}`, `DELETE /classes/{id}`, `PUT /classes/{id}/hidden` | the class directory |
//!
//! Reads are open; every write demands [`ControlAuth`] first (open until an RD token is
//! configured — see [`crate::auth`]). Every failure is the one typed [`ProtocolError`].
//!
//! The module also owns the split between an API path and a console (SPA) path: see
//! [`is_api_path`] and [`smart_fallback`].

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use gridfpv_events::{ClassId, PilotId};

use crate::auth::{ControlAuth, JoinTokenResponse};
use crate::classes::{
    Class, ClassError, ClassErrorKind, CreateClassRequest, SetClassHiddenRequest,
    UpdateClassRequest,
};
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{
    ActiveEvent, CreateEventRequest, EventId, EventMeta, EventRegistry, RegistryError,
    RegistryErrorKind, SetActiveEventRequest,
};
use crate::pilots::{CreatePilotRequest, Pilot, PilotError, PilotErrorKind, UpdatePilotRequest};

/// Build the API [`Router`] over `registry`.
///
/// The registry is the single router state: it holds the events, the token store, and the
/// app-level pilot and class directories. An unknown id anywhere is a typed
/// [`ProtocolError`] 404 (`UnknownScope`), the same shape a wrong route gets.
pub fn router(registry: EventRegistry) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        // The product identity: WHICH build is this rig running? The console footer reads
        // it, and a bug report from the field should quote it. The version is the BUILD's
        // version (see [`crate::BUILD_VERSION`]); the contract version is the independent
        // wire-compat integer.
        .route(
            "/about",
            get(|| async {
                Json(serde_json::json!({
                    "name": "GridFPV",
                    "version": crate::BUILD_VERSION,
                    "contract_version": crate::CONTRACT_VERSION,
                }))
            }),
        )
        // The Director's wall clock, in epoch microseconds (open, no auth). A console measures
        // its offset from this (round-trip-corrected) so anything it counts down reads off
        // *server* time, not the device's own clock — which can differ by a second or more on
        // a separate laptop.
        .route(
            "/time",
            get(|| async { Json(serde_json::json!({ "now_micros": now_micros() })) }),
        )
        // The event registry: an open list, an RD-gated create and a permanent delete.
        .route("/events", get(list_events).post(create_event))
        .route("/events/{event_id}", axum::routing::delete(delete_event))
        // The Director's active event: an open read so any client resumes into the selected
        // event on connect/reload, and an RD-gated write to set it.
        .route("/active-event", get(get_active_event).put(set_active_event))
        .route("/events/{event_id}/auth/join-token", post(mint_join_token))
        // The competition: one snapshot, one door for commands, the preset generators, a
        // pass from outside, and the change feed. See [`crate::control`].
        .route("/events/{event_id}/state", get(crate::control::get_state))
        .route(
            "/events/{event_id}/commands",
            post(crate::control::post_command),
        )
        .route(
            "/events/{event_id}/presets/bracket",
            post(crate::control::post_bracket),
        )
        .route(
            "/events/{event_id}/presets/mains",
            post(crate::control::post_mains),
        )
        .route("/events/{event_id}/passes", post(crate::control::post_pass))
        .route(
            "/events/{event_id}/changes",
            get(crate::control::get_changes),
        )
        // The app-level pilot directory: an open list; create/edit/delete are RD-gated.
        .route("/pilots", get(list_pilots).post(create_pilot))
        .route("/pilots/{pilot_id}", put(update_pilot).delete(delete_pilot))
        // The app-level class directory: an open list; create/edit/delete are RD-gated. The
        // built-in classes are read-only, but any class can be hidden.
        .route("/classes", get(list_classes).post(create_class))
        .route(
            "/classes/{class_id}",
            put(update_class).delete(delete_class),
        )
        .route("/classes/{class_id}/hidden", put(set_class_hidden))
        // Any path that matched no route above is a typed 404, not the SPA shell — see
        // [`api_fallback`] / [`smart_fallback`].
        .fallback(api_fallback)
        .with_state(registry)
}

/// `GET /events` — list every event's [`EventMeta`], in id order.
///
/// Reads are open on the LAN, so listing events needs no token.
async fn list_events(State(registry): State<EventRegistry>) -> Json<Vec<EventMeta>> {
    Json(registry.list())
}

/// `POST /events` — create a new event from a [`CreateEventRequest`], RD-gated.
///
/// The id is **auto-generated** (a slug of the name + a short random suffix) — names are
/// display-only, ids are never user-entered. The request's optional descriptive fields
/// (`date`/`location`/`description`/`organizer`) are stored on the new event's meta. The event
/// gets its own SQLite-backed log under the configured data dir (or an in-memory log when none
/// is configured) and the freshly-created [`EventMeta`] is returned.
async fn create_event(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreateEventRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry.create(&body).map_err(registry_error_to_protocol)?;
    Ok(Json(meta))
}

/// `DELETE /events/{event_id}` — **permanently** delete an event and ALL of its data, RD-gated.
///
/// The delete is total and irreversible: the registry entry, the event's persisted state (its
/// `<id>.sqlite` log plus the WAL/SHM sidecars under the data dir), and the active-event pointer
/// if it pointed at this event are all removed — so nothing of it survives a restart. An unknown
/// id is a typed 404 (`UnknownScope`). On success an empty 200 is returned.
async fn delete_event(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<StatusCode, ProtocolError> {
    // The registry types the failure: an unknown id is a 404, and a file-removal (I/O) failure
    // is a 500 — the in-memory drop already happened, so an I/O failure must NOT read as a 404.
    registry
        .delete(&event_id)
        .map_err(registry_error_to_protocol)?;
    Ok(StatusCode::OK)
}

/// `GET /active-event` — the Director's currently-active event, or `null`.
///
/// An **open read** (no token): every client reads this on connect/reload to resume into the
/// selected event (or fall to the picker when `null`). The active event is Director state, not
/// per-client browser state, so a reload / reconnect / app-restart resumes into the same event
/// all clients are on.
async fn get_active_event(State(registry): State<EventRegistry>) -> Json<ActiveEvent> {
    Json(ActiveEvent {
        event: registry.active(),
    })
}

/// `PUT /active-event` — set the Director's active event, RD-gated.
///
/// The body's `id` must name a known event, else a typed 404 (`UnknownScope`). On success the
/// active event is persisted server-side (surviving a Director restart) and its [`EventMeta`]
/// is returned.
async fn set_active_event(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<SetActiveEventRequest>,
) -> Result<Json<EventMeta>, ProtocolError> {
    let meta = registry
        .set_active(&body.id)
        .map_err(registry_error_to_protocol)?;
    Ok(Json(meta))
}

/// `POST /events/{event_id}/auth/join-token` — mint a fresh **read-only** join token, RD-gated.
///
/// The token store is Director-wide, so the minted token is not tied to the event; the path is
/// rooted under an event so an unknown event id is a typed 404. On success a brand-new
/// read-only token is returned as a [`JoinTokenResponse`].
async fn mint_join_token(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<JoinTokenResponse>, ProtocolError> {
    if registry.meta_of(&event_id).is_none() {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            format!("no event with id {:?}", event_id.0),
        ));
    }
    let token = registry.tokens().issue_join_token();
    Ok(Json(JoinTokenResponse { token }))
}

/// `GET /pilots` — list every pilot in the directory, in id order. An **open read**.
async fn list_pilots(State(registry): State<EventRegistry>) -> Json<Vec<Pilot>> {
    Json(registry.pilots().list())
}

/// `POST /pilots` — create a pilot from a [`CreatePilotRequest`], RD-gated.
///
/// The `callsign` is required; the id is auto-generated server-side (a callsign slug + suffix).
/// The new [`Pilot`] is returned and the directory persisted. A missing/blank callsign is a
/// `BadRequest`.
async fn create_pilot(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreatePilotRequest>,
) -> Result<Json<Pilot>, ProtocolError> {
    let pilot = registry
        .pilots()
        .create(&body)
        .map_err(pilot_error_to_protocol)?;
    Ok(Json(pilot))
}

/// `PUT /pilots/{pilot_id}` — edit a pilot's callsign/metadata, RD-gated.
///
/// An unknown id is a typed 404 (`UnknownScope`). On success the updated [`Pilot`] is returned and
/// the directory persisted.
async fn update_pilot(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(pilot_id): Path<PilotId>,
    Json(body): Json<UpdatePilotRequest>,
) -> Result<Json<Pilot>, ProtocolError> {
    let pilot = registry
        .pilots()
        .update(&pilot_id, &body)
        .map_err(pilot_error_to_protocol)?;
    Ok(Json(pilot))
}

/// `DELETE /pilots/{pilot_id}` — remove a pilot from the directory, RD-gated.
///
/// An unknown id is a 404 (`UnknownScope`). On success an empty 200 is returned and the directory
/// persisted.
async fn delete_pilot(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(pilot_id): Path<PilotId>,
) -> Result<StatusCode, ProtocolError> {
    registry
        .pilots()
        .delete(&pilot_id)
        .map_err(pilot_error_to_protocol)?;
    Ok(StatusCode::OK)
}

/// `GET /classes` — list every class in the directory, in id order. An **open read**.
async fn list_classes(State(registry): State<EventRegistry>) -> Json<Vec<Class>> {
    Json(registry.classes().list())
}

/// `POST /classes` — create a class from a [`CreateClassRequest`], RD-gated.
///
/// The `name` is required; the id is auto-generated server-side (a name slug + suffix). The new
/// [`Class`] is returned and the directory persisted. A missing/blank name is a `BadRequest`.
async fn create_class(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<CreateClassRequest>,
) -> Result<Json<Class>, ProtocolError> {
    let class = registry
        .classes()
        .create(&body)
        .map_err(class_error_to_protocol)?;
    Ok(Json(class))
}

/// `PUT /classes/{class_id}` — edit a class's name/source/metadata, RD-gated.
///
/// An unknown id is a typed 404 (`UnknownScope`). On success the updated [`Class`] is returned and
/// the directory persisted.
async fn update_class(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(class_id): Path<ClassId>,
    Json(body): Json<UpdateClassRequest>,
) -> Result<Json<Class>, ProtocolError> {
    let class = registry
        .classes()
        .update(&class_id, &body)
        .map_err(class_error_to_protocol)?;
    Ok(Json(class))
}

/// `DELETE /classes/{class_id}` — remove a class from the directory, RD-gated.
///
/// An unknown id is a 404 (`UnknownScope`). On success an empty 200 is returned and the directory
/// persisted.
async fn delete_class(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(class_id): Path<ClassId>,
) -> Result<StatusCode, ProtocolError> {
    registry
        .classes()
        .delete(&class_id)
        .map_err(class_error_to_protocol)?;
    Ok(StatusCode::OK)
}

/// `PUT /classes/{class_id}/hidden` — hide or un-hide a class, RD-gated.
///
/// The body is `{ hidden: bool }`. Hiding is a **visibility preference**, not an edit, so it is
/// valid for **built-in** classes too (never a read-only rejection): the class stays in the
/// directory; a picker simply stops offering it. The choice is persisted to a sidecar so a hidden
/// built-in survives the boot re-seed. An unknown id is a typed 404 (`UnknownScope`). On success
/// the updated [`Class`] (with its fresh `hidden` flag) is returned.
async fn set_class_hidden(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(class_id): Path<ClassId>,
    Json(body): Json<SetClassHiddenRequest>,
) -> Result<Json<Class>, ProtocolError> {
    let class = registry
        .classes()
        .set_hidden(&class_id, body.hidden)
        .map_err(class_error_to_protocol)?;
    Ok(Json(class))
}

/// Map a [`RegistryError`] to a typed [`ProtocolError`]: an unknown id is an `UnknownScope`
/// (404) and an **I/O / persistence** failure is `Internal` (500). The last is the load-bearing
/// case: the in-memory state is mutated before the write-through, so a failed write must surface
/// as a 500 (not a 404) — the change did not durably land.
fn registry_error_to_protocol(error: RegistryError) -> ProtocolError {
    let code = match error.kind {
        RegistryErrorKind::NotFound => ErrorCode::UnknownScope,
        RegistryErrorKind::Io => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// Map a [`PilotError`] to a typed [`ProtocolError`]: a validation failure is a `BadRequest`
/// (400), an unknown id is an `UnknownScope` (404), and a persistence failure is `Internal` (500).
fn pilot_error_to_protocol(error: PilotError) -> ProtocolError {
    let code = match error.kind {
        PilotErrorKind::Invalid => ErrorCode::BadRequest,
        PilotErrorKind::NotFound => ErrorCode::UnknownScope,
        PilotErrorKind::Internal => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// Map a [`ClassError`] to a typed [`ProtocolError`]: a validation failure is a `BadRequest`
/// (400), an unknown id is an `UnknownScope` (404), and a persistence failure is `Internal` (500).
fn class_error_to_protocol(error: ClassError) -> ProtocolError {
    let code = match error.kind {
        ClassErrorKind::Invalid => ErrorCode::BadRequest,
        // A read-only built-in edit/delete is a rejected bad request (the built-in is canonical).
        ClassErrorKind::ReadOnly => ErrorCode::BadRequest,
        ClassErrorKind::NotFound => ErrorCode::UnknownScope,
        ClassErrorKind::Internal => ErrorCode::Internal,
    };
    ProtocolError::new(code, error.to_string())
}

/// Whether a request path addresses a known **API tree**.
///
/// The blanket SPA fallback (in the Director wiring) serves `index.html` for any unmatched
/// path so client-side routes resolve. That is wrong for a *mistyped API* path, which should
/// surface a typed [`ProtocolError`], not an HTML 200 the client cannot parse. This predicate
/// names the API prefixes so [`smart_fallback`] can split "mistyped API → 404 JSON" from
/// "genuine client-side route → SPA shell".
///
/// A path matches when it equals a prefix exactly or continues with `/` (so `/eventsxyz` is
/// *not* an API path, but `/events`, `/events/`, and `/events/zzz` all are).
pub fn is_api_path(path: &str) -> bool {
    const API_PREFIXES: [&str; 8] = [
        "/health",
        "/about",
        "/time",
        "/events",
        "/active-event",
        "/pilots",
        "/classes",
        "/diagnostics",
    ];
    API_PREFIXES.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// The typed-404 fallback for **unmatched API-tree** paths.
///
/// Mounted as [`router`]'s own fallback so a request that matched no concrete route returns a
/// [`ProtocolError`] of [`ErrorCode::UnknownScope`] as JSON (HTTP 404) — the one uniform error
/// shape — rather than falling through to the SPA shell. Non-API paths never reach this
/// fallback when the SPA service is composed via [`smart_fallback`]; reached directly (a bare
/// `router` with no SPA) every unmatched path is a typed 404, which is the correct API-only
/// behaviour.
async fn api_fallback(req: Request<Body>) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::UnknownScope,
        format!(
            "no protocol route matches {} {}",
            req.method(),
            req.uri().path()
        ),
    )
}

/// Compose the Director's outer fallback: a **mistyped API path → typed 404 JSON**,
/// **any other path → the SPA `spa` service** (the client-side router shell).
///
/// The Director mounts the API [`router`] first, then needs *one* fallback for everything it
/// does not own. A naive `fallback_service(spa)` serves `index.html` for a mistyped API path
/// too — so a wrong `/events/...` arrives as an HTML 200 the client cannot parse. This wraps
/// the SPA service so a path under a known API tree ([`is_api_path`]) instead gets the typed
/// [`ProtocolError`] 404 from [`api_fallback`], while genuine client-side routes still resolve
/// to the SPA shell.
///
/// Generic over the inner SPA service so the Director's `ServeDir(+ index.html fallback)`
/// drops straight in (its `Response<ServeFileSystemResponseBody>` is mapped into the axum
/// [`Response`]); the returned service is itself usable with [`Router::fallback_service`].
pub fn smart_fallback<S, B>(spa: S) -> Router
where
    S: tower::Service<Request<Body>, Response = Response<B>, Error = std::convert::Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send + 'static,
    B: axum::body::HttpBody<Data = axum::body::Bytes> + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    Router::new()
        // A `Router` with no routes matches nothing, so *every* request hits this fallback;
        // we branch on the path there.
        .fallback(move |req: Request<Body>| {
            let mut spa = spa.clone();
            async move {
                if is_api_path(req.uri().path()) {
                    api_fallback(req).await.into_response()
                } else {
                    // Drive the SPA service for a genuine client-side route, mapping its
                    // body into the axum `Body` the fallback returns.
                    use tower::ServiceExt;
                    match spa.ready().await {
                        Ok(svc) => match svc.call(req).await {
                            Ok(response) => response.map(Body::new),
                            Err(infallible) => match infallible {},
                        },
                        Err(infallible) => match infallible {},
                    }
                }
            }
        })
}

/// Current server wall-clock time in **microseconds** since the Unix epoch — what `GET /time`
/// serves.
pub(crate) fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Render a [`ProtocolError`] as an HTTP error response: the JSON error body under the status
/// its [`ErrorCode`] maps to.
impl IntoResponse for ProtocolError {
    fn into_response(self) -> Response {
        let status = match self.code {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::UnknownScope => StatusCode::NOT_FOUND,
            ErrorCode::StaleCursor => StatusCode::GONE,
            ErrorCode::VersionMismatch => StatusCode::UPGRADE_REQUIRED,
            ErrorCode::BadRequest => StatusCode::BAD_REQUEST,
            ErrorCode::Refused => StatusCode::CONFLICT,
            ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self)).into_response()
    }
}

// `EventId` / `ClassId` / `PilotId` are transparent string newtypes, so axum's `Path`
// extractor deserializes a single path segment straight into them via serde.

#[cfg(test)]
mod tests {
    // `Body` and `Request` come in via `use super::*` (they are `use`d at module level for
    // the smart fallback); the tests reach them through the glob.
    use super::*;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    /// A registry holding exactly one **created** event.
    fn new_registry() -> EventRegistry {
        let registry = EventRegistry::new(None).unwrap();
        registry
            .create(&CreateEventRequest::named("Test Event"))
            .expect("create the test event");
        registry
    }

    /// The id of a test registry's single event.
    fn sole_event(registry: &EventRegistry) -> EventId {
        let mut list = registry.list();
        assert_eq!(
            list.len(),
            1,
            "this helper is for a registry holding exactly one created event"
        );
        list.remove(0).id
    }

    /// Send one request (an optional JSON body, an optional bearer token) through the router
    /// and return the status plus the raw body bytes.
    async fn send(
        registry: EventRegistry,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        token: Option<&str>,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header("Authorization", format!("Bearer {token}"));
        }
        let body = match body {
            Some(json) => {
                builder = builder.header("Content-Type", "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = router(registry)
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    /// Parse a body as the typed error every failure returns.
    fn error_of(raw: &[u8]) -> ProtocolError {
        serde_json::from_slice(raw).expect("a ProtocolError body")
    }

    // --- /health, /about, /time ------------------------------------------------------------

    #[tokio::test]
    async fn health_about_and_time_answer_without_a_token() {
        let registry = new_registry();
        // Even on a gated Director the three identity reads stay open.
        let _rd = registry.tokens().issue_rd_token();

        let (status, raw) = send(registry.clone(), "GET", "/health", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(raw, b"ok");

        let (status, raw) = send(registry.clone(), "GET", "/about", None, None).await;
        assert_eq!(status, StatusCode::OK);
        let about: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(about["name"], "GridFPV");
        assert_eq!(about["version"], crate::BUILD_VERSION);
        assert_eq!(about["contract_version"], crate::CONTRACT_VERSION.version);

        let before = now_micros();
        let (status, raw) = send(registry, "GET", "/time", None, None).await;
        let after = now_micros();
        assert_eq!(status, StatusCode::OK);
        let time: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let now = time["now_micros"]
            .as_i64()
            .expect("an integer microsecond count");
        assert!(
            before <= now && now <= after,
            "{before} <= {now} <= {after}"
        );
    }

    // --- the API / SPA split ---------------------------------------------------------------

    #[test]
    fn is_api_path_matches_only_the_known_trees() {
        // Exact prefix and any `/`-continuation are API paths.
        for p in [
            "/health",
            "/about",
            "/time",
            "/events",
            "/events/zzz/q-1",
            "/active-event",
            "/pilots",
            "/pilots/bogus",
            "/classes",
            "/classes/open/hidden",
        ] {
            assert!(is_api_path(p), "{p} should be an API path");
        }
        // A bare client-side route — and a prefix that is only a substring — are NOT.
        for p in [
            "/",
            "/heats/q-1/live",
            "/eventsxyz",
            "/timers",
            "/index.html",
        ] {
            assert!(!is_api_path(p), "{p} should NOT be an API path");
        }
    }

    #[tokio::test]
    async fn an_unknown_route_under_an_api_tree_is_a_typed_404() {
        let registry = new_registry();
        for uri in ["/events/zzz/nope/extra", "/pilots/a/b/c"] {
            let (status, raw) = send(registry.clone(), "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(error_of(&raw).code, ErrorCode::UnknownScope, "{uri}");
        }
    }

    #[tokio::test]
    async fn a_real_route_still_works_alongside_the_api_fallback() {
        let registry = new_registry();
        let event = sole_event(&registry);
        let (status, raw) = send(registry, "GET", "/events", None, None).await;
        assert_eq!(status, StatusCode::OK);
        let list: Vec<EventMeta> = serde_json::from_slice(&raw).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, event);
    }

    #[tokio::test]
    async fn smart_fallback_serves_spa_for_non_api_routes_and_404s_api_ones() {
        use axum::response::Html;

        // An inner "SPA" service that returns a recognisable shell for any path.
        let spa = tower::service_fn(|_req: Request<Body>| async {
            Ok::<_, std::convert::Infallible>(Html("<title>RD Console</title>").into_response())
        });
        let app = smart_fallback(spa);

        // A genuine client-side route → the SPA shell (200), not a typed 404.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/heats/q-1/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("RD Console"));

        // A mistyped API path → typed 404 ProtocolError, NOT the SPA shell.
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/events/zzz/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(error_of(&bytes).code, ErrorCode::UnknownScope);
    }

    // --- the event registry over HTTP ------------------------------------------------------

    #[tokio::test]
    async fn creating_an_event_returns_its_meta_and_lists_it() {
        let registry = EventRegistry::new(None).unwrap();
        let (status, raw) = send(registry.clone(), "GET", "/events", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(raw, b"[]", "a fresh Director lists no events");

        let (status, raw) = send(
            registry.clone(),
            "POST",
            "/events",
            Some(json!({ "name": "Spring Cup", "location": "Main field" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let created: EventMeta = serde_json::from_slice(&raw).unwrap();
        assert_eq!(created.name, "Spring Cup");
        assert_eq!(created.location.as_deref(), Some("Main field"));
        assert!(created.id.0.starts_with("spring-cup-"));

        let (_, raw) = send(registry, "GET", "/events", None, None).await;
        let list: Vec<EventMeta> = serde_json::from_slice(&raw).unwrap();
        assert_eq!(list, vec![created]);
    }

    #[tokio::test]
    async fn creating_an_event_requires_an_rd_token_once_configured() {
        let registry = EventRegistry::new(None).unwrap();
        let rd = registry.tokens().issue_rd_token();
        let body = json!({ "name": "Gated" });

        let (status, raw) = send(
            registry.clone(),
            "POST",
            "/events",
            Some(body.clone()),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(error_of(&raw).code, ErrorCode::Unauthorized);
        assert!(
            registry.list().is_empty(),
            "the rejected create made nothing"
        );

        let (status, _) = send(registry.clone(), "POST", "/events", Some(body), Some(&rd)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(registry.list().len(), 1);
    }

    #[tokio::test]
    async fn active_event_is_none_until_set_then_resumes() {
        let registry = new_registry();

        // A fresh Director has no active event → the picker.
        let (status, raw) = send(registry.clone(), "GET", "/active-event", None, None).await;
        assert_eq!(status, StatusCode::OK);
        let active: ActiveEvent = serde_json::from_slice(&raw).unwrap();
        assert!(active.event.is_none());

        // Setting it (open Director — no token needed) returns the created event's meta…
        let event = sole_event(&registry);
        let (status, raw) = send(
            registry.clone(),
            "PUT",
            "/active-event",
            Some(json!({ "id": event.0 })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let meta: EventMeta = serde_json::from_slice(&raw).unwrap();
        assert_eq!(meta.id, event);

        // …and now the open read resumes into it.
        let (_, raw) = send(registry, "GET", "/active-event", None, None).await;
        let active: ActiveEvent = serde_json::from_slice(&raw).unwrap();
        assert_eq!(active.event.map(|m| m.id), Some(event));
    }

    #[tokio::test]
    async fn setting_an_unknown_active_event_is_404() {
        let registry = new_registry();
        let (status, raw) = send(
            registry,
            "PUT",
            "/active-event",
            Some(json!({ "id": "no-such-event" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error_of(&raw).code, ErrorCode::UnknownScope);
    }

    #[tokio::test]
    async fn setting_the_active_event_requires_an_rd_token_once_configured() {
        let registry = new_registry();
        // Configure a control credential so the full-trust default closes.
        let _rd = registry.tokens().issue_rd_token();
        let event = sole_event(&registry);
        let (status, _) = send(
            registry.clone(),
            "PUT",
            "/active-event",
            Some(json!({ "id": event.0 })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(registry.active().is_none());
    }

    #[tokio::test]
    async fn delete_event_removes_a_created_event() {
        let registry = new_registry();
        let created = registry
            .create(&CreateEventRequest::named("Doomed"))
            .unwrap();
        assert!(registry.meta_of(&created.id).is_some());

        let uri = format!("/events/{}", created.id.0);
        let (status, _) = send(registry.clone(), "DELETE", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK);
        // Gone from the registry and the listing.
        assert!(registry.meta_of(&created.id).is_none());
        assert!(!registry.list().iter().any(|m| m.id == created.id));
    }

    #[tokio::test]
    async fn delete_event_rejects_unknown() {
        let registry = new_registry();
        let (status, raw) = send(registry, "DELETE", "/events/no-such-event", None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error_of(&raw).code, ErrorCode::UnknownScope);
    }

    #[tokio::test]
    async fn delete_event_requires_an_rd_token_once_configured() {
        let registry = new_registry();
        let _rd = registry.tokens().issue_rd_token();
        let created = registry
            .create(&CreateEventRequest::named("Gated"))
            .unwrap();
        let uri = format!("/events/{}", created.id.0);
        let (status, _) = send(registry.clone(), "DELETE", &uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // The event is untouched after the rejected delete.
        assert!(registry.meta_of(&created.id).is_some());
    }

    // --- join tokens -----------------------------------------------------------------------

    /// `POST /events/{id}/auth/join-token` for the registry's single event.
    async fn post_join_token(
        registry: EventRegistry,
        token: Option<&str>,
    ) -> (StatusCode, Option<JoinTokenResponse>) {
        let uri = format!("/events/{}/auth/join-token", sole_event(&registry).0);
        let (status, raw) = send(registry, "POST", &uri, None, token).await;
        (status, serde_json::from_slice(&raw).ok())
    }

    #[tokio::test]
    async fn rd_token_mints_a_join_token_that_reads_but_cannot_control() {
        use crate::auth::Role;

        let registry = new_registry();
        let tokens = registry.tokens();
        let rd = tokens.issue_rd_token();

        // An RD mints a fresh read-only join token over HTTP.
        let (status, body) = post_join_token(registry.clone(), Some(&rd)).await;
        assert_eq!(status, StatusCode::OK);
        let join = body.expect("a JoinTokenResponse body").token;
        assert!(!join.is_empty());

        // The minted token authenticates a READ as a read-only session…
        let read = tokens
            .authenticate_read(Some(&join))
            .unwrap()
            .expect("the minted token resolves a session");
        assert_eq!(read.role, Role::ReadOnly);
        // …but is rejected on CONTROL.
        assert_eq!(
            tokens.authenticate_control(Some(&join)).unwrap_err().code,
            ErrorCode::Unauthorized
        );
    }

    #[tokio::test]
    async fn minting_a_join_token_requires_an_rd_token_once_one_is_configured() {
        let registry = new_registry();
        let tokens = registry.tokens();
        // Configure a control credential so the full-trust default closes: without this,
        // control is open and a no-token mint would succeed.
        let _rd = tokens.issue_rd_token();

        // No token → 401 (control is now gated).
        let (status, _) = post_join_token(registry.clone(), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // A read-only/join token → 401 (it may not mint another).
        let join = tokens.issue_join_token();
        let (status, _) = post_join_token(registry.clone(), Some(&join)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // An unknown token → 401.
        let (status, _) = post_join_token(registry, Some("not-a-real-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn minting_a_join_token_is_open_when_no_rd_token_is_configured() {
        // Full-trust default: an unconfigured Director has no control credential, so a
        // no-token caller may mint a join token (control is open).
        let registry = new_registry();
        let (status, body) = post_join_token(registry, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_some_and(|b| !b.token.is_empty()));
    }

    #[tokio::test]
    async fn minting_a_join_token_for_an_unknown_event_is_404() {
        let registry = new_registry();
        let (status, raw) = send(
            registry,
            "POST",
            "/events/no-such-event/auth/join-token",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error_of(&raw).code, ErrorCode::UnknownScope);
    }

    // --- the pilot directory over HTTP -----------------------------------------------------

    #[tokio::test]
    async fn pilots_can_be_created_listed_edited_and_deleted() {
        let registry = EventRegistry::new(None).unwrap();

        let (status, raw) = send(
            registry.clone(),
            "POST",
            "/pilots",
            Some(json!({ "callsign": "AcroAce" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let created: Pilot = serde_json::from_slice(&raw).unwrap();
        assert_eq!(created.callsign, "AcroAce");

        let (status, raw) = send(registry.clone(), "GET", "/pilots", None, None).await;
        assert_eq!(status, StatusCode::OK);
        let list: Vec<Pilot> = serde_json::from_slice(&raw).unwrap();
        assert_eq!(list, vec![created.clone()]);

        let uri = format!("/pilots/{}", created.id.0);
        let (status, raw) = send(
            registry.clone(),
            "PUT",
            &uri,
            Some(json!({ "callsign": "Ace" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let updated: Pilot = serde_json::from_slice(&raw).unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.callsign, "Ace");

        let (status, _) = send(registry.clone(), "DELETE", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(registry.pilots().list().is_empty());
    }

    #[tokio::test]
    async fn pilot_errors_are_typed() {
        let registry = EventRegistry::new(None).unwrap();

        // A blank callsign is a 400.
        let (status, raw) = send(
            registry.clone(),
            "POST",
            "/pilots",
            Some(json!({ "callsign": "   " })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error_of(&raw).code, ErrorCode::BadRequest);

        // An unknown id is a 404 on edit and on delete.
        let (status, raw) = send(
            registry.clone(),
            "PUT",
            "/pilots/nobody",
            Some(json!({ "callsign": "Ace" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error_of(&raw).code, ErrorCode::UnknownScope);
        let (status, _) = send(registry.clone(), "DELETE", "/pilots/nobody", None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Once an RD token is configured, a write without it is a 401; the read stays open.
        let _rd = registry.tokens().issue_rd_token();
        let (status, _) = send(
            registry.clone(),
            "POST",
            "/pilots",
            Some(json!({ "callsign": "Ace" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(registry, "GET", "/pilots", None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    // --- the class directory over HTTP -----------------------------------------------------

    #[tokio::test]
    async fn classes_can_be_created_listed_edited_hidden_and_deleted() {
        let registry = EventRegistry::new(None).unwrap();
        let built_ins = registry.classes().list().len();

        let (status, raw) = send(
            registry.clone(),
            "POST",
            "/classes",
            Some(json!({ "name": "Club Whoop" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let created: Class = serde_json::from_slice(&raw).unwrap();
        assert_eq!(created.name, "Club Whoop");
        assert!(!created.hidden);

        let (status, raw) = send(registry.clone(), "GET", "/classes", None, None).await;
        assert_eq!(status, StatusCode::OK);
        let list: Vec<Class> = serde_json::from_slice(&raw).unwrap();
        assert_eq!(list.len(), built_ins + 1);
        assert!(list.contains(&created));

        let uri = format!("/classes/{}", created.id.0);
        let (status, raw) = send(
            registry.clone(),
            "PUT",
            &uri,
            Some(json!({ "name": "Club Whoops" })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let updated: Class = serde_json::from_slice(&raw).unwrap();
        assert_eq!(updated.name, "Club Whoops");

        let (status, raw) = send(
            registry.clone(),
            "PUT",
            &format!("{uri}/hidden"),
            Some(json!({ "hidden": true })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let hidden: Class = serde_json::from_slice(&raw).unwrap();
        assert!(hidden.hidden);

        let (status, _) = send(registry.clone(), "DELETE", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(registry.classes().list().len(), built_ins);
    }

    #[tokio::test]
    async fn a_built_in_class_cannot_be_deleted_but_can_be_hidden() {
        let registry = EventRegistry::new(None).unwrap();
        let built_in = registry
            .classes()
            .list()
            .into_iter()
            .next()
            .expect("the directory seeds built-in classes");
        let uri = format!("/classes/{}", built_in.id.0);

        // Read-only: a delete is a 400, and the class is still there.
        let (status, raw) = send(registry.clone(), "DELETE", &uri, None, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error_of(&raw).code, ErrorCode::BadRequest);
        assert!(registry.classes().exists(&built_in.id));

        // Hiding is a preference, not an edit, so it works on a built-in.
        let (status, raw) = send(
            registry.clone(),
            "PUT",
            &format!("{uri}/hidden"),
            Some(json!({ "hidden": true })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let hidden: Class = serde_json::from_slice(&raw).unwrap();
        assert!(hidden.hidden);

        // An unknown class is a 404.
        let (status, raw) = send(
            registry,
            "PUT",
            "/classes/no-such-class/hidden",
            Some(json!({ "hidden": true })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error_of(&raw).code, ErrorCode::UnknownScope);
    }

    #[test]
    fn registry_error_kinds_map_to_the_right_status() {
        // An I/O / persistence failure is a 500 — the load-bearing case: in-memory state was
        // already mutated, so it must NOT read as a 404.
        assert_eq!(
            registry_error_to_protocol(RegistryError::io("disk full")).code,
            ErrorCode::Internal
        );
        assert_eq!(
            registry_error_to_protocol(RegistryError::not_found("nope")).code,
            ErrorCode::UnknownScope
        );
    }
}
