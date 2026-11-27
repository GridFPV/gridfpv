//! A pilot asking, on the server, for something only the Director can do
//! (docs/primitives.md §13.4): signing up for a heat (§11.5), and checking in to an open
//! seat (§11.9).
//!
//! **The Director is the only writer of an event.** So the server changes nothing. It
//! holds the ask, tells the Director, and gives the pilot the Director's answer:
//!
//! 1. A signed-in pilot asks (`…/public/pilot/signup/…`, the same addresses a pilot uses
//!    at the race). The request waits.
//! 2. The Director is listening (`GET /push/listen`, a stream it opened itself). The
//!    server tells it at once that something is waiting.
//! 3. The Director reads what is waiting (`GET /push/requests`), decides each ask as that
//!    pilot's own, and answers (`POST /push/requests/{id}`).
//! 4. The pilot's request ends with that answer. What changed comes back with the
//!    Director's next push, which is how the page sees it.
//!
//! **Checking in needs the race code** (§13.13). Taking an open seat gives a pilot the laps
//! flown on it, so it is only for someone at the race. The director shows a code there,
//! the pilot types it, and the server passes it on with the ask. The Director checks it:
//! the server never learns the code and cannot say whether one is right.
//!
//! Nothing here is kept. An ask lives only as long as the pilot's request waits. If no
//! Director is listening, the pilot is told so at once.
//!
//! **Who is asking** is decided here and nowhere else: the pilot of this club that the
//! signed-in account holds, and that the event names (§13.7). Nothing in the request's
//! body names a pilot.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::stream::{self, Stream};
use gridfpv_events::{AdapterId, PhaseId, PilotId, RaceId, SeatRef, SignupSeat};
use gridfpv_server::error::ErrorCode;
use gridfpv_server::events::EventId;
use gridfpv_server::remote::{PilotAsk, PushAnswer, PushRequest};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

use crate::app::AppState;
use crate::book::ClubId;
use crate::people::Signed;
use crate::push::{ApiError, Pusher};
use crate::read::shown;

/// How long a pilot's ask waits for the Director's answer.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// The most characters of a race code that are passed on to the Director.
const CODE_MAX: usize = 64;

/// How often the stream to a Director says it is still there.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

struct Waiting {
    request: PushRequest,
    answer: oneshot::Sender<PushAnswer>,
}

struct ClubDesk {
    /// How many Directors of the club are listening now.
    listeners: usize,
    waiting: Vec<Waiting>,
    /// Bumped each time an ask is added, to wake the listeners.
    bell: watch::Sender<u64>,
}

impl Default for ClubDesk {
    fn default() -> Self {
        Self {
            listeners: 0,
            waiting: Vec::new(),
            bell: watch::channel(0).0,
        }
    }
}

/// The asks that are waiting for a Director, by club. Nothing is kept on disk.
#[derive(Default)]
pub struct Asks {
    clubs: Mutex<HashMap<ClubId, ClubDesk>>,
}

/// Why an ask got no answer.
#[derive(Debug, PartialEq, Eq)]
pub enum NoAnswer {
    /// No Director of the club is connected.
    NobodyListening,
    /// A Director is connected and did not answer in time.
    TooSlow,
}

/// One Director listening. Dropping it says the Director has gone.
pub struct Listener {
    asks: Arc<Asks>,
    club: ClubId,
    bell: watch::Receiver<u64>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut clubs = self.asks.lock();
        if let Some(desk) = clubs.get_mut(&self.club) {
            desk.listeners = desk.listeners.saturating_sub(1);
        }
    }
}

impl Asks {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ClubId, ClubDesk>> {
        self.clubs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A Director of the club starts listening.
    pub fn listen(self: &Arc<Self>, club: &ClubId) -> Listener {
        let mut clubs = self.lock();
        let desk = clubs.entry(club.clone()).or_default();
        desk.listeners += 1;
        Listener {
            asks: Arc::clone(self),
            club: club.clone(),
            bell: desk.bell.subscribe(),
        }
    }

    /// Whether a Director of the club is listening.
    pub fn listening(&self, club: &ClubId) -> bool {
        self.lock().get(club).is_some_and(|desk| desk.listeners > 0)
    }

    /// What is waiting for the club's Director, oldest first.
    pub fn waiting(&self, club: &ClubId) -> Vec<PushRequest> {
        self.lock()
            .get(club)
            .map(|desk| desk.waiting.iter().map(|w| w.request.clone()).collect())
            .unwrap_or_default()
    }

    /// Give an ask its answer. `false` when no such ask is waiting: it was answered
    /// already, or the pilot stopped waiting.
    pub fn answer(&self, club: &ClubId, id: &str, answer: PushAnswer) -> bool {
        let found = {
            let mut clubs = self.lock();
            clubs.get_mut(club).and_then(|desk| {
                let at = desk.waiting.iter().position(|w| w.request.id == id)?;
                Some(desk.waiting.remove(at))
            })
        };
        match found {
            // The pilot may have gone away a moment ago. Then nobody hears it.
            Some(waiting) => waiting.answer.send(answer).is_ok(),
            None => false,
        }
    }

    /// Put an ask before the club's Director and wait for the answer.
    pub async fn ask(
        &self,
        club: &ClubId,
        event: EventId,
        pilot: PilotId,
        ask: PilotAsk,
        code: Option<String>,
        within: Duration,
    ) -> Result<PushAnswer, NoAnswer> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut buf = [0u8; 12];
            getrandom::fill(&mut buf).expect("the operating system gives random bytes");
            buf.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        {
            let mut clubs = self.lock();
            let desk = clubs.entry(club.clone()).or_default();
            if desk.listeners == 0 {
                return Err(NoAnswer::NobodyListening);
            }
            desk.waiting.push(Waiting {
                request: PushRequest {
                    id: id.clone(),
                    event,
                    pilot,
                    ask,
                    code,
                },
                answer: tx,
            });
            desk.bell.send_modify(|count| *count += 1);
        }
        // Whatever ends the wait, the ask is not left behind for a Director to act on
        // after the pilot was told there was no answer.
        let _tidy = Tidy {
            asks: self,
            club,
            id: &id,
        };
        match tokio::time::timeout(within, rx).await {
            Ok(Ok(answer)) => Ok(answer),
            _ => Err(NoAnswer::TooSlow),
        }
    }
}

/// Takes an ask off the desk when the pilot stops waiting for it.
struct Tidy<'a> {
    asks: &'a Asks,
    club: &'a ClubId,
    id: &'a str,
}

impl Drop for Tidy<'_> {
    fn drop(&mut self) {
        let mut clubs = self.asks.lock();
        if let Some(desk) = clubs.get_mut(self.club) {
            desk.waiting.retain(|w| w.request.id != self.id);
        }
    }
}

// ── For the Director ────────────────────────────────────────────────────────

/// `GET /push/listen`: a stream the Director opens and keeps open. An `asks` event is sent
/// when it connects with something already waiting, and each time an ask is added.
pub async fn get_listen(
    State(state): State<AppState>,
    Pusher(club): Pusher,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let listener = state.asks.listen(&club.id);
    let asks = Arc::clone(&state.asks);
    let stream = stream::unfold((listener, true), move |(mut listener, first)| {
        let asks = Arc::clone(&asks);
        async move {
            loop {
                if first {
                    // Anything that was waiting before the Director connected.
                    listener.bell.borrow_and_update();
                    if asks.waiting(&listener.club).is_empty() {
                        // Say hello, so the Director knows the stream is open.
                        let hello = SseEvent::default().event("listening").data("");
                        return Some((Ok(hello), (listener, false)));
                    }
                } else {
                    listener.bell.changed().await.ok()?;
                    listener.bell.borrow_and_update();
                }
                let count = asks.waiting(&listener.club).len();
                if count > 0 || first {
                    let event = SseEvent::default().event("asks").data(count.to_string());
                    return Some((Ok(event), (listener, false)));
                }
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
}

/// `GET /push/requests`: what is waiting for this club's Director.
pub async fn get_requests(
    State(state): State<AppState>,
    Pusher(club): Pusher,
) -> Json<Vec<PushRequest>> {
    Json(state.asks.waiting(&club.id))
}

/// `POST /push/requests/{id}`: the Director's answer.
pub async fn post_answer(
    State(state): State<AppState>,
    Pusher(club): Pusher,
    Path(id): Path<String>,
    Json(answer): Json<PushAnswer>,
) -> Result<StatusCode, ApiError> {
    if state.asks.answer(&club.id, &id, answer) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownScope,
            "Nobody is waiting on that any more.",
        ))
    }
}

// ── For the pilot ───────────────────────────────────────────────────────────

/// `POST …/public/pilot/signup/take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeBody {
    pub heat: PhaseId,
    pub seat: SignupSeat,
}

/// `POST …/public/pilot/signup/next`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextBody {
    pub phase: PhaseId,
    pub seat: SignupSeat,
}

/// `POST …/public/pilot/signup/leave`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveBody {
    pub heat: PhaseId,
}

fn refused(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, ErrorCode::Refused, message)
}

/// The pilot of this event that the signed-in account holds.
fn pilot_of(
    state: &AppState,
    standing: &crate::accounts::Standing,
    club: &str,
    event: &str,
) -> Result<(ClubId, EventId, PilotId), ApiError> {
    let shown = shown(state, club, event)?;
    let club = ClubId(club.to_owned());
    let held: Vec<String> = state
        .accounts
        .claims_of(&standing.account)?
        .into_iter()
        .filter(|claim| claim.club == club && claim.how.holds())
        .map(|claim| claim.pilot)
        .collect();
    let pilot = shown
        .header
        .pilots
        .iter()
        .find(|pilot| held.contains(&pilot.id.0))
        .map(|pilot| pilot.id.clone())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "Your account is not tied to a pilot in this event.",
            )
        })?;
    Ok((club, EventId(event.to_owned()), pilot))
}

async fn put_before_director(
    state: &AppState,
    standing: &crate::accounts::Standing,
    club: String,
    event: String,
    ask: PilotAsk,
    code: Option<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (club, event, pilot) = pilot_of(state, standing, &club, &event)?;
    // Passed on as typed, within reason. Whether it is right is the Director's to say.
    let code = code
        .map(|code| code.trim().chars().take(CODE_MAX).collect::<String>())
        .filter(|code| !code.is_empty());
    match state
        .asks
        .ask(&club, event, pilot, ask, code, ANSWER_WITHIN)
        .await
    {
        Ok(PushAnswer { refused: None, .. }) => Ok(Json(serde_json::json!({}))),
        // About the race code: its own status, so the page knows to ask for the code.
        Ok(PushAnswer {
            refused: Some(why),
            needs_code: true,
        }) => Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            ErrorCode::Refused,
            why,
        )),
        Ok(PushAnswer {
            refused: Some(why), ..
        }) => Err(refused(why)),
        Err(NoAnswer::NobodyListening) => Err(refused(
            "The race computer is not connected right now. Try again in a moment, or sign up at the race.",
        )),
        Err(NoAnswer::TooSlow) => Err(refused(
            "The race computer did not answer. Look at the list again before you try once more.",
        )),
    }
}

type EventPath = Path<(String, String)>;

/// `POST …/public/pilot/signup/take`.
pub async fn post_take(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): EventPath,
    Json(body): Json<TakeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ask = PilotAsk::Take {
        heat: body.heat,
        seat: body.seat,
    };
    put_before_director(&state, &standing, club, event, ask, None).await
}

/// `POST …/public/pilot/signup/next`.
pub async fn post_next(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): EventPath,
    Json(body): Json<NextBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ask = PilotAsk::Next {
        phase: body.phase,
        seat: body.seat,
    };
    put_before_director(&state, &standing, club, event, ask, None).await
}

/// `POST …/public/pilot/signup/leave`.
pub async fn post_leave(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): EventPath,
    Json(body): Json<LeaveBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    put_before_director(
        &state,
        &standing,
        club,
        event,
        PilotAsk::Leave { heat: body.heat },
        None,
    )
    .await
}

/// `POST …/public/pilot/checkin/take`: the same body a pilot sends at the race, and the
/// race code that shows they are there (§13.13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeSeatBody {
    pub race: RaceId,
    pub adapter: AdapterId,
    pub seat: SeatRef,
    #[serde(default)]
    pub code: Option<String>,
}

/// `POST …/public/pilot/checkin/drop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropSeatBody {
    pub race: RaceId,
}

/// `POST …/public/pilot/checkin/take`.
pub async fn post_take_seat(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): EventPath,
    Json(body): Json<TakeSeatBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ask = PilotAsk::TakeSeat {
        race: body.race,
        adapter: body.adapter,
        seat: body.seat,
    };
    put_before_director(&state, &standing, club, event, ask, body.code).await
}

/// `POST …/public/pilot/checkin/drop`.
pub async fn post_drop_seat(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path((club, event)): EventPath,
    Json(body): Json<DropSeatBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    put_before_director(
        &state,
        &standing,
        club,
        event,
        PilotAsk::DropSeat { race: body.race },
        None,
    )
    .await
}
