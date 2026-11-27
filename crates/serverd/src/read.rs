//! What anyone may read from this server (§13.9): the clubs, their events, and each event
//! as a visitor may see it.
//!
//! | route | what |
//! |-------|------|
//! | `GET /api/clubs` | the clubs on this server |
//! | `GET /api/clubs/{club}/events` | a club's events, newest first |
//! | `GET /api/clubs/{club}/events/{event}` | one event, as the list shows it |
//! | `GET /api/clubs/{club}/events/{event}/public/…` | one event, for a visitor |
//!
//! **One event is read exactly as a Director's Public door gives its open event**
//! (`gridfpv_server::spectate`): the same routes under `public/`, the same shapes, and the
//! same things left out. So the page a spectator opens at the race is the page they open
//! here; only the address in front differs (§11.7: one page, served from two places).
//!
//! What a visitor may see is decided by the Director's own functions, applied here to
//! whatever was pushed. The server does not trust that a Director sent only what is safe.
//!
//! No token is needed and nothing here changes anything.

use std::convert::Infallible;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::stream::{self, Stream};
use gridfpv_server::access::Door;
use gridfpv_server::classes::Class;
use gridfpv_server::error::ErrorCode;
use gridfpv_server::events::EventId;
use gridfpv_server::pilots::Pilot;
use gridfpv_server::public::{Caller, PublicHello};
use gridfpv_server::remote::{PushClub, PushHeader, ServerEvent};
use gridfpv_server::spectate::{
    self, Mentioned, PublicEvent, PublicEventName, class_for_visitors, pilot_for_visitors,
    timer_for_visitors,
};
use gridfpv_server::timers::{SeatChannel, TimerView};
use gridfpv_server::view::EventView;

use crate::app::AppState;
use crate::book::ClubId;
use crate::push::ApiError;
use crate::store::Copy;

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

fn not_found(what: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, ErrorCode::UnknownScope, what)
}

/// One event a club holds, with its header. An event whose header has not arrived is not
/// there yet for a reader: it has no name to be shown by.
pub(crate) struct Shown {
    pub(crate) copy: Arc<Copy>,
    pub(crate) header: PushHeader,
}

/// An event as a list shows it.
pub(crate) fn server_event(
    id: EventId,
    meta: gridfpv_server::events::EventMeta,
    hidden: bool,
) -> ServerEvent {
    ServerEvent {
        hidden,
        id,
        name: meta.name,
        date: meta.date,
        location: meta.location,
        description: meta.description,
        organizer: meta.organizer,
        created_at: meta.created_at,
    }
}

pub(crate) fn shown(state: &AppState, club: &str, event: &str) -> Result<Shown, ApiError> {
    let gone = || not_found("This server holds no such event.");
    let club = state.book.club(&ClubId(club.to_owned())).ok_or_else(gone)?;
    let copy = state
        .store
        .get(&club.id, &EventId(event.to_owned()))?
        .ok_or_else(gone)?;
    let header = copy
        .header()?
        .and_then(|json| serde_json::from_str::<PushHeader>(&json).ok())
        .ok_or_else(gone)?;
    Ok(Shown { copy, header })
}

impl Shown {
    /// The event's snapshot, for a visitor.
    pub(crate) fn view(&self) -> Result<EventView, ApiError> {
        let now = self.copy.director_now(now_micros());
        let view = self.copy.with_state(|state| EventView::build(state, now))?;
        Ok(spectate::for_visitors(view))
    }

    fn mentioned(&self) -> Result<Mentioned, ApiError> {
        Ok(spectate::mentioned(&self.view()?))
    }
}

/// `GET /api/clubs`.
pub async fn get_clubs(State(state): State<AppState>) -> Json<Vec<PushClub>> {
    Json(
        state
            .book
            .clubs()
            .into_iter()
            .map(|club| PushClub {
                id: club.id.0,
                name: club.name,
            })
            .collect(),
    )
}

/// `GET /api/clubs/{club}/events`: newest first. A hidden event is listed only for someone
/// who runs the club (§13.9).
pub async fn get_events(
    State(state): State<AppState>,
    crate::people::MaybeSigned(standing): crate::people::MaybeSigned,
    Path(club): Path<String>,
) -> Result<Json<Vec<ServerEvent>>, ApiError> {
    let club = state
        .book
        .club(&ClubId(club))
        .ok_or_else(|| not_found("This server has no such club."))?;
    let runs = standing.is_some_and(|s| s.may(crate::accounts::Act::RunClub(&club.id)));
    let hidden: std::collections::HashSet<String> = state
        .book
        .event_notes(&club.id)
        .into_iter()
        .filter(|note| note.hidden)
        .map(|note| note.event)
        .collect();
    let mut events = Vec::new();
    for id in state.store.list(&club.id)? {
        let hidden = hidden.contains(&id.0);
        if hidden && !runs {
            continue;
        }
        // One event that cannot be read must not hide the others.
        let Ok(shown) = shown(&state, &club.id.0, &id.0) else {
            continue;
        };
        events.push(server_event(id, shown.header.meta, hidden));
    }
    events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    Ok(Json(events))
}

type EventPath = Path<(String, String)>;

/// `GET /api/clubs/{club}/events/{event}`: one event as a list shows it. For anyone who has
/// its address, a hidden event too (§13.9: hidden is unlisted, not closed).
pub async fn get_one_event(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<ServerEvent>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    let hidden = state.book.event_note(&ClubId(club), &event).hidden;
    Ok(Json(server_event(
        EventId(event),
        shown.header.meta,
        hidden,
    )))
}

/// `GET …/public/hello`: a reader here is always a visitor, and nobody signs in.
pub async fn get_hello(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<PublicHello>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    Ok(Json(PublicHello {
        version: gridfpv_server::BUILD_VERSION.to_owned(),
        caller: Caller::Visitor,
        door: Some(Door::Public),
        event: Some(shown.header.meta.name),
        director_sign_in: false,
        pilot: None,
        kiosk: false,
        kiosk_name: None,
        server: false,
    }))
}

/// `GET /public/hello`, at the server's own address: this is a server, not a Director. The
/// console's page asks this first, and shows the server's list of events when told so.
pub async fn get_server_hello() -> Json<PublicHello> {
    Json(PublicHello {
        version: gridfpv_server::BUILD_VERSION.to_owned(),
        caller: Caller::Visitor,
        door: None,
        event: None,
        director_sign_in: false,
        pilot: None,
        kiosk: false,
        kiosk_name: None,
        server: true,
    })
}

/// `GET …/public/event`.
pub async fn get_event(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<PublicEvent>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    Ok(Json(PublicEvent {
        event: Some(PublicEventName {
            id: shown.header.meta.id,
            name: shown.header.meta.name,
        }),
    }))
}

/// `GET …/public/event/state`.
pub async fn get_state(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<EventView>, ApiError> {
    Ok(Json(shown(&state, &club, &event)?.view()?))
}

/// `GET …/public/event/pilots`: the pilots the event refers to, callsign and colour only.
pub async fn get_pilots(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<Vec<Pilot>>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    let wanted = shown.mentioned()?.pilots;
    Ok(Json(
        shown
            .header
            .pilots
            .into_iter()
            .filter(|pilot| wanted.contains(&pilot.id))
            .map(pilot_for_visitors)
            .collect(),
    ))
}

/// `GET …/public/event/classes`: the classes the event refers to, name only.
pub async fn get_classes(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<Vec<Class>>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    let wanted = shown.mentioned()?.classes;
    Ok(Json(
        shown
            .header
            .classes
            .into_iter()
            .filter(|class| wanted.contains(&class.id))
            .map(class_for_visitors)
            .collect(),
    ))
}

/// `GET …/public/event/timers`: the timers the event is flown on, name and seats only.
pub async fn get_timers(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<Vec<TimerView>>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    let wanted = shown.mentioned()?.timers;
    Ok(Json(
        shown
            .header
            .timers
            .into_iter()
            .filter(|view| wanted.contains(&view.timer.id))
            .map(timer_for_visitors)
            .collect(),
    ))
}

/// `GET …/public/event/channels`: the video channel of each seat the event is flown on.
pub async fn get_channels(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Json<Vec<SeatChannel>>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    let wanted = shown.mentioned()?.timers;
    Ok(Json(
        shown
            .header
            .channels
            .into_iter()
            .filter(|channel| wanted.contains(&channel.adapter))
            .collect(),
    ))
}

/// `GET …/public/event/changes`: server-sent events. A `cursor` event on connect, and again
/// each time the copy grows or its header is replaced. A page that hears one reads again.
pub async fn get_changes(
    State(state): State<AppState>,
    Path((club, event)): EventPath,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let shown = shown(&state, &club, &event)?;
    let watch = shown.copy.watch();
    // The first is sent at once; each later one waits for a change. The feed ends when the
    // copy is removed.
    let stream = stream::unfold((watch, true), |(mut watch, first)| async move {
        if !first {
            watch.changed().await.ok()?;
        }
        let cursor = *watch.borrow_and_update();
        let event = SseEvent::default().event("cursor").data(cursor.to_string());
        Some((Ok(event), (watch, false)))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
