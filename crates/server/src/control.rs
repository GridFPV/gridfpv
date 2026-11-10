//! The competition over HTTP: commands in, facts logged, a snapshot and a change feed out.
//!
//! The rules live in `gridfpv-engine` and are pure. This module is the plumbing around
//! them, and it keeps to one pattern:
//!
//! 1. Lock the event's log, then its folded [`EventState`].
//! 2. **Catch up**: apply any log entries the state has not seen, then append the facts
//!    that time alone has made true (a countdown reaching its tone, a timed race running
//!    out), each stamped with the moment it fell due.
//! 3. Do the one thing asked: read a snapshot, or check a command and append its facts.
//! 4. Tell the change feed the log grew.
//!
//! Every read and every write goes through [`with_event`], so nothing ever sees a state
//! that is behind its log or behind the clock.
//!
//! | route | what |
//! |-------|------|
//! | `GET /events/{id}/state` | the [`EventView`] snapshot |
//! | `POST /events/{id}/commands` | one [`Command`]; refused with `Refused` (409) and a reason |
//! | `POST /events/{id}/presets/bracket` | generate a bracket's phases |
//! | `POST /events/{id}/presets/mains` | generate stacked mains |
//! | `POST /events/{id}/passes` | a gate crossing from outside |
//! | `GET /events/{id}/changes` | server-sent `cursor` events |
//!
//! **The change feed** carries no data, only the log cursor. A console that hears a new
//! cursor reads the snapshot again. That is simple and cannot drift; the snapshot is small
//! at club scale. Deltas can come later without changing this contract.

use std::convert::Infallible;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::stream::{self, Stream};
use gridfpv_engine::presets::{self, Bracket, Elimination, Mains};
use gridfpv_engine::state::EventState;
use gridfpv_events::{
    AdapterId, Command, Event, FieldSlice, Format, GateIndex, Pass, PhaseDef, PhaseId, SeatRef,
    SeatSource, SourceTime,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;

use crate::app::now_micros;
use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventId, EventRegistry, EventSource};
use crate::view::EventView;

/// One event, locked and caught up, for the length of one request.
pub struct Session<'a> {
    log: &'a mut (dyn EventSource + Send),
    state: &'a mut EventState,
    changed: &'a watch::Sender<u64>,
    registry: &'a EventRegistry,
    /// The Director's clock for this request, in epoch microseconds.
    pub now: i64,
}

fn internal(what: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(ErrorCode::Internal, what.to_string())
}

fn unknown_event(id: &EventId) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::UnknownScope,
        format!("no event with id {:?}", id.0),
    )
}

/// Lock an event, catch its state up to its log and to the clock, and run `f` on it.
pub fn with_event<R>(
    registry: &EventRegistry,
    id: &EventId,
    f: impl FnOnce(&mut Session<'_>) -> Result<R, ProtocolError>,
) -> Result<R, ProtocolError> {
    let log = registry.log(id).ok_or_else(|| unknown_event(id))?;
    let live = registry.live(id).ok_or_else(|| unknown_event(id))?;
    // Always the log first, then the state.
    let mut log = log
        .lock()
        .map_err(|_| internal("the event log is poisoned"))?;
    let mut state = live
        .state
        .lock()
        .map_err(|_| internal("the event state is poisoned"))?;
    let mut session = Session {
        log: &mut *log,
        state: &mut state,
        changed: &live.changed,
        registry,
        now: now_micros(),
    };
    session.catch_up()?;
    f(&mut session)
}

impl Session<'_> {
    /// The event's state, caught up.
    pub fn state(&self) -> &EventState {
        self.state
    }

    fn catch_up(&mut self) -> Result<(), ProtocolError> {
        for stored in self.log.read_from(self.state.applied).map_err(internal)? {
            self.state
                .apply(stored.offset, stored.recorded_at, &stored.event);
        }
        for (at, fact) in self.state.due(self.now) {
            self.append(at, fact)?;
        }
        Ok(())
    }

    fn append(&mut self, at: i64, event: Event) -> Result<(), ProtocolError> {
        let offset = self.log.append(event.clone(), Some(at)).map_err(internal)?;
        self.state.apply(offset, Some(at), &event);
        self.changed.send_replace(self.state.applied);
        Ok(())
    }

    /// Check commands in order, each against the state the ones before it would leave, and
    /// append the facts of all of them — or none, if any is refused.
    pub fn commands(&mut self, commands: Vec<Command>) -> Result<(), ProtocolError> {
        let mut scratch;
        let mut facts = Vec::new();
        // One command needs no scratch copy of the state.
        if let [only] = &commands[..] {
            facts = self
                .state
                .check(only.clone(), self.now)
                .map_err(|refusal| self.refused(self.state, refusal))?;
        } else {
            scratch = self.state.clone();
            for command in commands {
                let checked = scratch
                    .check(command, self.now)
                    .map_err(|refusal| self.refused(&scratch, refusal))?;
                for fact in checked {
                    scratch.apply(scratch.applied, Some(self.now), &fact);
                    facts.push(fact);
                }
            }
        }
        for fact in facts {
            self.append(self.now, fact)?;
        }
        Ok(())
    }

    /// Log an observation from a timer. A pass is stamped with the race that is running
    /// now; a pass while no race runs belongs to no race.
    pub fn observe(&mut self, mut event: Event) -> Result<(), ProtocolError> {
        if let Event::Pass(pass) = &mut event {
            pass.race = self
                .state
                .races
                .running()
                .map(|race| race.setup.race.clone());
        }
        self.append(self.now, event)
    }

    fn outcome(&self) -> CommandOutcome {
        CommandOutcome {
            cursor: self.state.applied,
        }
    }
}

impl Session<'_> {
    /// A refusal as the sentence a director reads. The engine names things by id, because
    /// ids are all it has. A raw id must never reach a screen, so each quoted id is swapped
    /// here for its name: a race for "race N of ‹heat›", a phase for its label, a pilot for
    /// their callsign. `state` is the state the command was checked against.
    fn refused(&self, state: &EventState, refusal: impl std::fmt::Display) -> ProtocolError {
        let mut message = refusal.to_string();
        // An id appears as the engine's debug form, `RaceId("…")`, or as a bare `"…"`.
        let swap = |message: String, kind: &str, id: &str, name: &str| {
            message
                .replace(&format!("{kind}({id:?})"), name)
                .replace(&format!("{id:?}"), name)
        };
        let label = |phase: &PhaseId| {
            state.tree.get(phase).map_or_else(
                || "a phase that no longer exists".to_owned(),
                |def| def.label.clone(),
            )
        };
        // Races first: a race id contains its heat's id.
        for race in state.races.iter() {
            let number = state
                .races
                .of_heat(&race.setup.heat)
                .position(|other| other.setup.race == race.setup.race)
                .map_or(1, |index| index + 1);
            let name = format!("{number} of {}", label(&race.setup.heat));
            message = swap(message, "RaceId", &race.setup.race.0, &name);
        }
        // Longest ids first, so a phase is never swapped inside a longer id that holds it.
        let mut phases: Vec<&PhaseDef> = state.tree.iter().collect();
        phases.sort_by_key(|def| std::cmp::Reverse(def.id.0.len()));
        for def in phases {
            message = swap(message, "PhaseId", &def.id.0, &def.label);
        }
        for pilot in self.registry.pilots().list() {
            message = swap(message, "PilotId", &pilot.id.0, &pilot.callsign);
        }
        ProtocolError::new(ErrorCode::Refused, message)
    }
}

/// What a write answers: where the log is now. The snapshot at this cursor (or later)
/// shows the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CommandOutcome {
    #[ts(type = "number")]
    pub cursor: u64,
}

/// `GET /events/{event_id}/state` — the competition snapshot. An open read.
pub async fn get_state(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Json<EventView>, ProtocolError> {
    with_event(&registry, &event_id, |session| {
        Ok(Json(EventView::build(session.state(), session.now)))
    })
}

/// `POST /events/{event_id}/commands` — one [`Command`], RD-gated. Allowed: its facts are
/// logged and the new cursor returned. Not allowed: `Refused` (409) with the reason, and
/// nothing is logged.
pub async fn post_command(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(command): Json<Command>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    with_event(&registry, &event_id, |session| {
        session.commands(vec![command])?;
        Ok(Json(session.outcome()))
    })
}

/// A seat every heat of a generated bracket is flown on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PresetSeat {
    pub adapter: AdapterId,
    pub seat: SeatRef,
}

/// Single or double elimination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum EliminationKind {
    Single,
    Double,
}

impl From<EliminationKind> for Elimination {
    fn from(kind: EliminationKind) -> Self {
        match kind {
            EliminationKind::Single => Elimination::Single,
            EliminationKind::Double => Elimination::Double,
        }
    }
}

/// What `POST /events/{id}/presets/bracket` takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct BracketRequest {
    /// The id for the bracket phase. Its rounds and heats get ids beneath it.
    pub id: PhaseId,
    /// The director's name for it.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<PhaseId>,
    /// The bracket's field, in seed order.
    pub field: Vec<FieldSlice>,
    /// Who starts on each seeded place, top seed first.
    pub entrants: Vec<SeatSource>,
    /// The seats every heat is flown on. How many there are is the heat size.
    pub seats: Vec<PresetSeat>,
    /// How many pilots move on from each heat (at most half of it).
    pub advance: u32,
    pub elimination: EliminationKind,
    pub format: Format,
    /// A different format for the final only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub final_format: Option<Format>,
}

/// What `POST /events/{id}/presets/mains` takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MainsRequest {
    /// The id for the phase that holds every main.
    pub id: PhaseId,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<PhaseId>,
    /// The phase the mains are seeded from (qualifying).
    pub seeded_from: PhaseId,
    /// How many pilots that phase ranks.
    pub pilots: u32,
    /// The names of the mains, best first. Left out: as many as the field needs, named
    /// "A-Main", "B-Main", and so on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub names: Option<Vec<String>>,
    /// How many pilots start in each main, bump-ups included.
    pub per_main: u32,
    /// How many pilots bump up from each main into the one above.
    pub bump: u32,
    pub seats: Vec<PresetSeat>,
    pub advance: u32,
    pub elimination: EliminationKind,
    pub format: Format,
    /// A different format for the top main's final only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub top_final_format: Option<Format>,
}

fn seats(seats: Vec<PresetSeat>) -> Vec<(AdapterId, SeatRef)> {
    seats.into_iter().map(|s| (s.adapter, s.seat)).collect()
}

/// "A-Main", "B-Main", … and past Z, "Main 27".
fn main_names(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| match u8::try_from(index) {
            Ok(letter) if letter < 26 => format!("{}-Main", char::from(b'A' + letter)),
            _ => format!("Main {}", index + 1),
        })
        .collect()
}

/// Log generated phases, all or none.
fn define(session: &mut Session<'_>, defs: Vec<PhaseDef>) -> Result<(), ProtocolError> {
    session.commands(
        defs.into_iter()
            .map(|def| Command::Record(Box::new(Event::PhaseDefined { def })))
            .collect(),
    )
}

/// `POST /events/{event_id}/presets/bracket` — generate a bracket's phases, RD-gated.
pub async fn post_bracket(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<BracketRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    let defs = presets::bracket(Bracket {
        id: body.id,
        label: body.label,
        parent: body.parent,
        field: body.field,
        entrants: body.entrants,
        seats: seats(body.seats),
        advance: body.advance,
        elimination: body.elimination.into(),
        format: body.format,
        final_format: body.final_format,
    })
    .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))?;
    with_event(&registry, &event_id, |session| {
        define(session, defs)?;
        Ok(Json(session.outcome()))
    })
}

/// `POST /events/{event_id}/presets/mains` — generate stacked mains, RD-gated.
pub async fn post_mains(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<MainsRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    let names = body.names.unwrap_or_else(|| {
        main_names(presets::mains_needed(body.pilots, body.per_main, body.bump))
    });
    let defs = presets::mains(Mains {
        id: body.id,
        label: body.label,
        parent: body.parent,
        seeded_from: body.seeded_from,
        pilots: body.pilots,
        names,
        per_main: body.per_main,
        bump: body.bump,
        seats: seats(body.seats),
        advance: body.advance,
        elimination: body.elimination.into(),
        format: body.format,
        top_final_format: body.top_final_format,
    })
    .map_err(|e| ProtocolError::new(ErrorCode::BadRequest, e.to_string()))?;
    with_event(&registry, &event_id, |session| {
        define(session, defs)?;
        Ok(Json(session.outcome()))
    })
}

/// A gate crossing from outside the Director (`POST /events/{id}/passes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PassRequest {
    pub adapter: AdapterId,
    pub seat: SeatRef,
    /// When, on the timer's own clock.
    pub at: SourceTime,
}

/// `POST /events/{event_id}/passes` — log one gate crossing, RD-gated.
///
/// This is the door a timer's passes come in by: the pass is stamped with the race that is
/// running now. Until the timer adapters are wired back in it is used by tools and tests.
pub async fn post_pass(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
    Json(body): Json<PassRequest>,
) -> Result<Json<CommandOutcome>, ProtocolError> {
    with_event(&registry, &event_id, |session| {
        session.observe(Event::Pass(Pass {
            adapter: body.adapter,
            seat: body.seat,
            at: body.at,
            sequence: None,
            gate: GateIndex::LAP,
            signal: None,
            race: None,
        }))?;
        Ok(Json(session.outcome()))
    })
}

/// `GET /events/{event_id}/changes` — server-sent events. An open read.
///
/// Sends a `cursor` event with the log's length on connect, then again each time it moves.
/// The feed also keeps time: it wakes when a countdown or a timed race falls due, so the
/// start and the stop are logged on time while anyone is listening. (When nobody is, they
/// are logged with the same timestamps the next time anyone looks.)
pub async fn get_changes(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ProtocolError> {
    let live = registry
        .live(&event_id)
        .ok_or_else(|| unknown_event(&event_id))?;
    let feed = Feed {
        registry,
        event_id,
        changes: live.changed.subscribe(),
        sent: None,
    };
    let stream = stream::unfold(feed, |mut feed| async move {
        let cursor = feed.next().await?;
        let event = SseEvent::default().event("cursor").data(cursor.to_string());
        Some((Ok(event), feed))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

struct Feed {
    registry: EventRegistry,
    event_id: EventId,
    changes: watch::Receiver<u64>,
    /// The last cursor sent.
    sent: Option<u64>,
}

impl Feed {
    /// The next cursor to send, once it differs from the last. `None` ends the feed: the
    /// event is gone.
    async fn next(&mut self) -> Option<u64> {
        loop {
            // Catching up here is what logs a start or a stop that has fallen due.
            let (cursor, next_due) = with_event(&self.registry, &self.event_id, |session| {
                Ok((session.state().applied, session.state().next_due()))
            })
            .ok()?;
            if self.sent != Some(cursor) {
                self.sent = Some(cursor);
                return Some(cursor);
            }
            let until_due = next_due
                .map(|at| Duration::from_micros(u64::try_from(at - now_micros()).unwrap_or(0)));
            tokio::select! {
                changed = self.changes.changed() => changed.ok()?,
                () = sleep_until_due(until_due) => {}
            }
        }
    }
}

/// Sleep for `wait`, or for ever when nothing is due.
async fn sleep_until_due(wait: Option<Duration>) {
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::router;
    use crate::events::CreateEventRequest;
    use crate::view::{RaceStateView, SeatWhoView};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use gridfpv_events::{
        CompletionRule, EndCondition, FirstLap, Measure, PilotId, RaceId, RaceRules, RankingRule,
    };
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    const S: i64 = 1_000_000;

    fn new_event(registry: &EventRegistry) -> EventId {
        registry
            .create(&CreateEventRequest::named("Test Event"))
            .unwrap()
            .id
    }

    async fn send(
        registry: &EventRegistry,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        token: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
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
        let response = router(registry.clone())
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    async fn command(registry: &EventRegistry, event: &EventId, command: Command) -> StatusCode {
        let body = serde_json::to_value(command).unwrap();
        send(
            registry,
            "POST",
            &format!("/events/{}/commands", event.0),
            Some(body),
            None,
        )
        .await
        .0
    }

    async fn state(registry: &EventRegistry, event: &EventId) -> EventView {
        let (status, body) = send(
            registry,
            "GET",
            &format!("/events/{}/state", event.0),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        serde_json::from_value(body).unwrap()
    }

    fn format(staging_micros: i64) -> Format {
        Format {
            name: "First to 1".into(),
            race: RaceRules {
                end: EndCondition::FirstToLaps(1),
                staging_micros,
                min_lap_micros: None,
                first_lap: FirstLap::default(),
            },
            completion: CompletionRule::SavedRaces(1),
            ranking: Some(RankingRule {
                sort: vec![Measure::LastFinish],
            }),
            points: Vec::new(),
        }
    }

    /// Four pilots in one bracket with four seats: a single final heat, "finals/final".
    fn bracket_request(staging_micros: i64) -> serde_json::Value {
        let pilots: Vec<PilotId> = ["ana", "ben", "cat", "dee"]
            .iter()
            .map(|name| PilotId((*name).into()))
            .collect();
        serde_json::to_value(BracketRequest {
            id: PhaseId("finals".into()),
            label: "Finals".into(),
            parent: None,
            field: vec![FieldSlice::Imported(pilots.clone())],
            entrants: pilots.into_iter().map(SeatSource::Pilot).collect(),
            seats: (0..4)
                .map(|i| PresetSeat {
                    adapter: AdapterId("oft".into()),
                    seat: SeatRef(format!("seat-{i}")),
                })
                .collect(),
            advance: 2,
            elimination: EliminationKind::Double,
            format: format(staging_micros),
            final_format: None,
        })
        .unwrap()
    }

    async fn with_bracket(staging_micros: i64) -> (EventRegistry, EventId) {
        let registry = EventRegistry::new(None).unwrap();
        let event = new_event(&registry);
        let (status, _) = send(
            &registry,
            "POST",
            &format!("/events/{}/presets/bracket", event.0),
            Some(bracket_request(staging_micros)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        (registry, event)
    }

    async fn pass(registry: &EventRegistry, event: &EventId, seat: &str, at: i64) {
        let (status, _) = send(
            registry,
            "POST",
            &format!("/events/{}/passes", event.0),
            Some(json!({ "adapter": "oft", "seat": seat, "at": at })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_heat_is_raced_start_to_save_over_http_and_the_snapshot_shows_it() {
        let (registry, event) = with_bracket(0).await;
        let heat = PhaseId("finals/final".into());
        let before = state(&registry, &event).await;
        assert_eq!(before.phases.len(), 2);
        assert_eq!(before.phases[1].lineup.len(), 4);
        assert_eq!(
            before.phases[1].lineup[0].who,
            SeatWhoView::Pilot(PilotId("ana".into()))
        );
        assert_eq!(before.current_race, None);

        assert_eq!(
            command(&registry, &event, Command::StartRace { heat: heat.clone() }).await,
            StatusCode::OK
        );
        let race = RaceId("finals/final#1".into());
        // Dee (seat 3) flies the lap fastest; Ana never crosses.
        for (seat, at) in [("seat-1", 0), ("seat-2", 0), ("seat-3", 0)] {
            pass(&registry, &event, seat, at).await;
        }
        for (seat, at) in [("seat-3", 9 * S), ("seat-1", 10 * S), ("seat-2", 11 * S)] {
            pass(&registry, &event, seat, at).await;
        }
        let running = state(&registry, &event).await;
        assert_eq!(running.current_race, Some(race.clone()));
        assert_eq!(running.races[0].state, RaceStateView::Running);
        assert!(running.races[0].started_at.is_some());
        assert_eq!(running.races[0].seats[3].finish, Some(1));
        assert_eq!(running.races[0].seats[3].laps.len(), 1);
        assert_eq!(running.races[0].seats[3].laps[0].micros, 9 * S);
        assert_eq!(running.races[0].seats[0].finish, Some(4));

        assert_eq!(
            command(&registry, &event, Command::StopRace { race: race.clone() }).await,
            StatusCode::OK
        );
        assert_eq!(
            command(
                &registry,
                &event,
                Command::SaveRace {
                    race,
                    needs_marshal: false,
                    note: None
                }
            )
            .await,
            StatusCode::OK
        );
        let after = state(&registry, &event).await;
        assert_eq!(after.current_race, None);
        assert!(after.phases.iter().all(|phase| phase.complete));
        let order: Vec<&str> = after.phases[0]
            .ranking
            .iter()
            .map(|placed| placed.pilot.0.as_str())
            .collect();
        assert_eq!(order, vec!["dee", "ben", "cat", "ana"]);
        assert!(!after.phases[0].ranking[3].flew);
        assert!(after.cursor > before.cursor);
    }

    #[tokio::test]
    async fn a_refused_command_is_a_409_with_the_reason_and_logs_nothing() {
        let (registry, event) = with_bracket(0).await;
        let before = state(&registry, &event).await.cursor;
        let (status, body) = send(
            &registry,
            "POST",
            &format!("/events/{}/commands", event.0),
            Some(json!({ "StopRace": { "race": "nope" } })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let error: ProtocolError = serde_json::from_value(body).unwrap();
        assert_eq!(error.code, ErrorCode::Refused);
        assert!(error.message.contains("no race"), "{}", error.message);
        assert_eq!(state(&registry, &event).await.cursor, before);
    }

    #[tokio::test]
    async fn a_preset_is_all_or_nothing() {
        let registry = EventRegistry::new(None).unwrap();
        let event = new_event(&registry);
        let mut request = bracket_request(0);
        request["parent"] = json!("no-such-phase");
        let (status, _) = send(
            &registry,
            "POST",
            &format!("/events/{}/presets/bracket", event.0),
            Some(request),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(state(&registry, &event).await.phases.is_empty());

        // A shape that cannot work is a bad request, not a refusal.
        let mut request = bracket_request(0);
        request["advance"] = json!(3);
        let (status, _) = send(
            &registry,
            "POST",
            &format!("/events/{}/presets/bracket", event.0),
            Some(request),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn mains_are_named_for_the_field_when_no_names_are_given() {
        let (registry, event) = with_bracket(0).await;
        let request = serde_json::to_value(MainsRequest {
            id: PhaseId("mains".into()),
            label: "Mains".into(),
            parent: None,
            seeded_from: PhaseId("finals".into()),
            pilots: 20,
            names: None,
            per_main: 8,
            bump: 2,
            seats: (0..4)
                .map(|i| PresetSeat {
                    adapter: AdapterId("oft".into()),
                    seat: SeatRef(format!("seat-{i}")),
                })
                .collect(),
            advance: 2,
            elimination: EliminationKind::Double,
            format: format(0),
            top_final_format: None,
        })
        .unwrap();
        let (status, _) = send(
            &registry,
            "POST",
            &format!("/events/{}/presets/mains", event.0),
            Some(request),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let view = state(&registry, &event).await;
        let labels: Vec<&str> = view
            .phases
            .iter()
            .filter(|phase| phase.def.parent == Some(PhaseId("mains".into())))
            .map(|phase| phase.def.label.as_str())
            .collect();
        assert_eq!(labels, vec!["A-Main", "B-Main", "C-Main"]);
    }

    #[tokio::test]
    async fn writes_need_the_rd_token_and_reads_do_not() {
        let (registry, event) = with_bracket(0).await;
        let token = registry.tokens().issue_rd_token();
        let start = json!({ "StartRace": { "heat": "finals/final" } });
        let uri = format!("/events/{}/commands", event.0);
        let (status, _) = send(&registry, "POST", &uri, Some(start.clone()), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(&registry, "POST", &uri, Some(start), Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        // The snapshot is an open read.
        assert_eq!(state(&registry, &event).await.races.len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_event_is_a_404_on_every_route() {
        let registry = EventRegistry::new(None).unwrap();
        for (method, uri, body) in [
            ("GET", "/events/nope/state", None),
            ("GET", "/events/nope/changes", None),
            (
                "POST",
                "/events/nope/commands",
                Some(json!({ "StartRace": { "heat": "h" } })),
            ),
            (
                "POST",
                "/events/nope/presets/bracket",
                Some(bracket_request(0)),
            ),
        ] {
            let (status, _) = send(&registry, method, uri, body, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn the_competition_is_rebuilt_from_the_log_after_a_restart() {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-control-restart-{}-{}",
            std::process::id(),
            now_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let before = {
            let registry = EventRegistry::new(Some(dir.clone())).unwrap();
            let event = new_event(&registry);
            let (status, _) = send(
                &registry,
                "POST",
                &format!("/events/{}/presets/bracket", event.0),
                Some(bracket_request(0)),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            command(
                &registry,
                &event,
                Command::StartRace {
                    heat: PhaseId("finals/final".into()),
                },
            )
            .await;
            pass(&registry, &event, "seat-0", 0).await;
            (event.clone(), state(&registry, &event).await)
        };
        let registry = EventRegistry::new(Some(dir.clone())).unwrap();
        let mut after = state(&registry, &before.0).await;
        after.now = before.1.now;
        assert_eq!(after, before.1);
        drop(registry);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn feed(registry: &EventRegistry, event: &EventId) -> Feed {
        Feed {
            registry: registry.clone(),
            event_id: event.clone(),
            changes: registry.live(event).unwrap().changed.subscribe(),
            sent: None,
        }
    }

    #[tokio::test]
    async fn the_change_feed_sends_the_cursor_on_connect_and_each_time_it_moves() {
        let (registry, event) = with_bracket(0).await;
        let mut feed = feed(&registry, &event);
        let first = feed.next().await.unwrap();
        assert_eq!(first, state(&registry, &event).await.cursor);

        let waiting = tokio::spawn(async move { feed.next().await });
        command(
            &registry,
            &event,
            Command::StartRace {
                heat: PhaseId("finals/final".into()),
            },
        )
        .await;
        let second = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the feed did not wake")
            .unwrap()
            .unwrap();
        assert_eq!(second, first + 1);
    }

    #[tokio::test]
    async fn the_change_feed_logs_the_start_when_the_countdown_ends() {
        // A 50 ms countdown.
        let (registry, event) = with_bracket(50_000).await;
        command(
            &registry,
            &event,
            Command::StartRace {
                heat: PhaseId("finals/final".into()),
            },
        )
        .await;
        let mut feed = feed(&registry, &event);
        let staged = feed.next().await.unwrap();
        // Nothing else happens; the feed wakes by itself at the tone.
        let started = tokio::time::timeout(Duration::from_secs(5), feed.next())
            .await
            .expect("the feed did not wake at the tone")
            .unwrap();
        assert_eq!(started, staged + 1);
        let view = state(&registry, &event).await;
        assert_eq!(view.races[0].state, RaceStateView::Running);
        // Logged at the tone itself, not at the moment the feed woke.
        assert_eq!(view.races[0].started_at, view.races[0].tone_at);
    }

    #[tokio::test]
    async fn the_change_feed_ends_when_its_event_is_deleted() {
        let (registry, event) = with_bracket(0).await;
        let mut feed = feed(&registry, &event);
        feed.next().await.unwrap();
        registry.delete(&event).unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(5), feed.next())
            .await
            .expect("the feed did not end");
        assert_eq!(ended, None);
    }

    #[tokio::test]
    async fn a_refusal_names_things_by_their_names_never_by_id() {
        let (registry, event) = with_bracket(0).await;
        let heat = PhaseId("finals/final".into());
        command(&registry, &event, Command::StartRace { heat: heat.clone() }).await;
        let refuse = |body: serde_json::Value| {
            let registry = registry.clone();
            let uri = format!("/events/{}/commands", event.0);
            async move {
                let (status, body) = send(&registry, "POST", &uri, Some(body), None).await;
                assert_eq!(status, StatusCode::CONFLICT);
                serde_json::from_value::<ProtocolError>(body)
                    .unwrap()
                    .message
            }
        };
        // Saving a race that is still running.
        let message = refuse(json!({ "SaveRace": { "race": "finals/final#1" } })).await;
        assert_eq!(message, "cannot save race 1 of Final while it is Running");
        // Starting again while it runs.
        let message = refuse(json!({ "StartRace": { "heat": "finals/final" } })).await;
        assert!(message.contains("race 1 of Final"), "{message}");
        assert!(!message.contains("finals"), "{message}");
        assert!(!message.contains('#'), "{message}");
    }
}
