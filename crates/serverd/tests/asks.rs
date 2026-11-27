//! A pilot asking, on the server, for a spot in a heat (docs/primitives.md §13.4): who may
//! ask, who it is taken to be, and what they are told. The Director is stood in for by the
//! test, which listens and answers as one would.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_events::PilotId;
use gridfpv_server::events::EventId;
use gridfpv_server::remote::{PilotAsk, PushAnswer, PushRequest};
use gridfpv_serverd::accounts::{AccountId, ClubPilot, InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::asks::{Asks, NoAnswer};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

fn leave(heat: &str) -> PilotAsk {
    PilotAsk::Leave {
        heat: gridfpv_events::PhaseId(heat.into()),
    }
}

const SOON: Duration = Duration::from_millis(150);

// ── The desk itself ─────────────────────────────────────────────────────────

#[tokio::test]
async fn with_no_director_listening_a_pilot_is_told_at_once() {
    let asks = Arc::new(Asks::default());
    let began = Instant::now();
    let got = asks
        .ask(
            &club("north"),
            EventId("e".into()),
            PilotId("p".into()),
            leave("h"),
            None,
            Duration::from_secs(30),
        )
        .await;
    assert_eq!(got, Err(NoAnswer::NobodyListening));
    assert!(began.elapsed() < Duration::from_secs(1), "it did not wait");
    assert_eq!(asks.waiting(&club("north")), vec![]);
}

#[tokio::test]
async fn an_ask_waits_for_the_directors_answer_and_gets_it() {
    let asks = Arc::new(Asks::default());
    let _director = asks.listen(&club("north"));
    let asking = {
        let asks = Arc::clone(&asks);
        tokio::spawn(async move {
            asks.ask(
                &club("north"),
                EventId("friday".into()),
                PilotId("ace-1".into()),
                leave("h1"),
                None,
                Duration::from_secs(5),
            )
            .await
        })
    };
    // The Director sees it: which event, which pilot, and what they asked.
    let waiting = loop {
        let waiting = asks.waiting(&club("north"));
        if !waiting.is_empty() {
            break waiting;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(waiting.len(), 1);
    let PushRequest {
        id,
        event,
        pilot,
        ask,
        code,
    } = waiting[0].clone();
    assert_eq!(code, None);
    assert_eq!((event.0.as_str(), pilot.0.as_str()), ("friday", "ace-1"));
    assert_eq!(ask, leave("h1"));

    let no = PushAnswer {
        refused: Some("That heat is on the timer.".into()),
        needs_code: false,
    };
    assert!(asks.answer(&club("north"), &id, no.clone()));
    assert_eq!(asking.await.unwrap(), Ok(no));
    // Answered once: it is off the desk, and cannot be answered again.
    assert_eq!(asks.waiting(&club("north")), vec![]);
    assert!(!asks.answer(&club("north"), &id, PushAnswer::default()));
}

#[tokio::test]
async fn an_ask_nobody_answers_is_given_up_and_taken_off_the_desk() {
    let asks = Arc::new(Asks::default());
    let _director = asks.listen(&club("north"));
    let got = asks
        .ask(
            &club("north"),
            EventId("e".into()),
            PilotId("p".into()),
            leave("h"),
            None,
            SOON,
        )
        .await;
    assert_eq!(got, Err(NoAnswer::TooSlow));
    // A Director that looks now finds nothing to act on: the pilot was told "no answer".
    assert_eq!(asks.waiting(&club("north")), vec![]);
}

#[tokio::test]
async fn a_pilot_who_stops_waiting_takes_their_ask_with_them() {
    let asks = Arc::new(Asks::default());
    let _director = asks.listen(&club("north"));
    let asking = {
        let asks = Arc::clone(&asks);
        tokio::spawn(async move {
            asks.ask(
                &club("north"),
                EventId("e".into()),
                PilotId("p".into()),
                leave("h"),
                None,
                Duration::from_secs(30),
            )
            .await
        })
    };
    while asks.waiting(&club("north")).is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The pilot's browser goes away.
    asking.abort();
    asking.await.ok();
    assert_eq!(asks.waiting(&club("north")), vec![]);
}

#[tokio::test]
async fn a_director_hears_only_its_own_clubs_asks() {
    let asks = Arc::new(Asks::default());
    let _north = asks.listen(&club("north"));
    // South has no Director listening, whoever listens for North.
    let got = asks
        .ask(
            &club("south"),
            EventId("e".into()),
            PilotId("p".into()),
            leave("h"),
            None,
            SOON,
        )
        .await;
    assert_eq!(got, Err(NoAnswer::NobodyListening));

    let asking = {
        let asks = Arc::clone(&asks);
        tokio::spawn(async move {
            asks.ask(
                &club("north"),
                EventId("e".into()),
                PilotId("p".into()),
                leave("h"),
                None,
                Duration::from_secs(5),
            )
            .await
        })
    };
    while asks.waiting(&club("north")).is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(asks.waiting(&club("south")), vec![]);
    // South's Director cannot answer North's ask, though it knows the id.
    let id = asks.waiting(&club("north"))[0].id.clone();
    assert!(!asks.answer(&club("south"), &id, PushAnswer::default()));
    assert!(asks.answer(&club("north"), &id, PushAnswer::default()));
    assert_eq!(asking.await.unwrap(), Ok(PushAnswer::default()));
}

#[tokio::test]
async fn a_director_that_goes_away_is_no_longer_listening() {
    let asks = Arc::new(Asks::default());
    let first = asks.listen(&club("north"));
    let second = asks.listen(&club("north"));
    assert!(asks.listening(&club("north")));
    drop(first);
    assert!(asks.listening(&club("north")), "one is still there");
    drop(second);
    assert!(!asks.listening(&club("north")));
}

// ── Over HTTP ───────────────────────────────────────────────────────────────

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    north: String,
    south: String,
}

/// A server with two clubs. North has an event, Friday, that names Ace and Bee.
async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"},{"id":"south","name":"South FPV"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let north = state.book.new_token(&club("north"), "laptop").unwrap().1;
    let south = state.book.new_token(&club("south"), "laptop").unwrap().1;
    let rig = Rig {
        app: router(state.clone()),
        state,
        north,
        south,
        _dir: dir,
    };
    let header = json!({
        "meta": {"id": "friday", "name": "Friday", "created_at": 1, "persistent": true},
        "pilots": [
            {"id": "ace-1", "callsign": "Ace", "vtx_types": []},
            {"id": "bee-2", "callsign": "Bee", "vtx_types": []}
        ]
    });
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::PUT,
        "/push/events/friday",
        Some(header),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Cat is the club's pilot too, and is not in Friday.
    rig.state
        .accounts
        .note_pilots(
            &club("north"),
            &[ClubPilot {
                pilot: "cat-3".into(),
                callsign: "Cat".into(),
                color: None,
            }],
            1,
        )
        .unwrap();
    rig
}

struct Person {
    id: AccountId,
    session: String,
}

impl Rig {
    /// A member of a club who holds `pilot` there, when one is given.
    fn member(&self, name: &str, club_id: &str, pilot: Option<&str>) -> Person {
        let now = now_ms();
        let (_, secret) = self
            .state
            .accounts
            .new_invite(
                NewInvite {
                    kind: InviteKind::Personal {
                        club: club(club_id),
                        role: Role::Pilot,
                        pilot: None,
                    },
                    label: name.into(),
                    max_uses: 1,
                    expires_at: None,
                },
                None,
                now,
            )
            .unwrap();
        let subject = format!("subject-of-{name}");
        let sign_in = NewSignIn {
            provider: "discord",
            subject: &subject,
            label: name,
        };
        let (account, _, session) = self
            .state
            .accounts
            .join_with_invite(&secret, &sign_in, name, now)
            .unwrap();
        if let Some(pilot) = pilot {
            self.state
                .accounts
                .give_pilot(&club(club_id), pilot, &account.id, None, now)
                .unwrap();
        }
        Person {
            id: account.id,
            session,
        }
    }

    /// Stand in for a Director of a club: listen, and answer every ask with `answer`,
    /// keeping what was asked.
    fn director(&self, club_id: &str, answer: PushAnswer) -> Director {
        let asks = Arc::clone(&self.state.asks);
        let club = club(club_id);
        let listener = asks.listen(&club);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let task = {
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let _listener = listener;
                loop {
                    for request in asks.waiting(&club) {
                        asks.answer(&club, &request.id, answer.clone());
                        seen.lock().unwrap().push(request);
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        };
        Director { seen, task }
    }
}

struct Director {
    seen: Arc<std::sync::Mutex<Vec<PushRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Director {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum By<'a> {
    Nobody,
    Person(&'a Person),
    Token(&'a str),
}

async fn send(
    rig: &Rig,
    by: By<'_>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "grid.example.org");
    match by {
        By::Nobody => {}
        By::Person(person) => {
            request = request.header(
                header::COOKIE,
                format!("{SESSION_COOKIE}={}", person.session),
            );
        }
        By::Token(token) => {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = rig.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const TAKE: &str = "/api/clubs/north/events/friday/public/pilot/signup/take";

fn take_body() -> Value {
    json!({"heat": "quali/heat-1", "seat": {"adapter": "bench", "seat": "seat-2"}})
}

#[tokio::test]
async fn the_pilot_is_the_one_the_account_holds_and_never_one_the_request_names() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let director = rig.director("north", PushAnswer::default());

    // The body tries to name Bee. It is not read.
    let mut body = take_body();
    body["pilot"] = json!("bee-2");
    body["by"] = json!("Director");
    let (status, answer) = send(&rig, By::Person(&pia), Method::POST, TAKE, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");

    let seen = director.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].pilot.0, "ace-1");
    assert_eq!(seen[0].event.0, "friday");
    assert_eq!(
        serde_json::to_value(&seen[0].ask).unwrap(),
        json!({"Take": {"heat": "quali/heat-1", "seat": {"adapter": "bench", "seat": "seat-2"}}})
    );
}

#[tokio::test]
async fn each_of_the_three_asks_reaches_the_director_as_it_was_made() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let director = rig.director("north", PushAnswer::default());
    let base = "/api/clubs/north/events/friday/public/pilot/signup";
    let seat = json!({"adapter": "bench", "seat": "seat-1"});
    for (route, body) in [
        ("take", json!({"heat": "quali/heat-2", "seat": seat})),
        ("next", json!({"phase": "quali", "seat": seat})),
        ("leave", json!({"heat": "quali/heat-2"})),
    ] {
        let (status, why) = send(
            &rig,
            By::Person(&pia),
            Method::POST,
            &format!("{base}/{route}"),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{route}: {why}");
    }
    let asked: Vec<Value> = director
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|request| serde_json::to_value(&request.ask).unwrap())
        .collect();
    assert_eq!(
        asked,
        vec![
            json!({"Take": {"heat": "quali/heat-2", "seat": seat}}),
            json!({"Next": {"phase": "quali", "seat": seat}}),
            json!({"Leave": {"heat": "quali/heat-2"}}),
        ]
    );
}

#[tokio::test]
async fn a_seat_ask_reaches_the_director_with_the_code_as_typed_and_dropping_carries_none() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let director = rig.director("north", PushAnswer::default());
    let base = "/api/clubs/north/events/friday/public/pilot/checkin";
    let seat = |code: Value| {
        let mut body = json!({"race": "practice#1", "adapter": "bench", "seat": "seat-1"});
        if !code.is_null() {
            body["code"] = code;
        }
        body
    };
    for (route, body) in [
        ("take", seat(json!("  abc-234 "))),
        ("take", seat(Value::Null)),
        ("take", seat(json!("   "))),
        ("take", seat(json!("x".repeat(500)))),
        // A code sent with a drop is not passed on: dropping needs none.
        ("drop", json!({"race": "practice#1", "code": "ABC234"})),
    ] {
        let (status, why) = send(
            &rig,
            By::Person(&pia),
            Method::POST,
            &format!("{base}/{route}"),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{route}: {why}");
    }
    let seen = director.seen.lock().unwrap().clone();
    let take = json!({"TakeSeat": {"race": "practice#1", "adapter": "bench", "seat": "seat-1"}});
    let asked: Vec<(Value, Option<String>)> = seen
        .iter()
        .map(|request| {
            assert_eq!(request.pilot.0, "ace-1");
            assert_eq!(request.event.0, "friday");
            (
                serde_json::to_value(&request.ask).unwrap(),
                request.code.clone(),
            )
        })
        .collect();
    assert_eq!(
        asked,
        vec![
            // Trimmed, and otherwise as typed: the Director is the one that reads it.
            (take.clone(), Some("abc-234".to_owned())),
            (take.clone(), None),
            (take.clone(), None),
            (take, Some("x".repeat(64))),
            (json!({"DropSeat": {"race": "practice#1"}}), None),
        ]
    );
    // An ask with no code is sent with no `code` at all.
    assert!(
        serde_json::to_value(&seen[1])
            .unwrap()
            .get("code")
            .is_none()
    );
}

#[tokio::test]
async fn a_seat_ask_needs_a_sign_in_a_pilot_in_the_event_and_a_body_that_reads() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let rex = rig.member("Rex", "north", None);
    let director = rig.director("north", PushAnswer::default());
    let base = "/api/clubs/north/events/friday/public/pilot/checkin";
    let body =
        json!({"race": "practice#1", "adapter": "bench", "seat": "seat-1", "code": "ABC234"});
    for route in ["take", "drop"] {
        let path = format!("{base}/{route}");
        let (status, _) = send(&rig, By::Nobody, Method::POST, &path, Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{route}");
        let (status, _) = send(
            &rig,
            By::Token(&rig.north),
            Method::POST,
            &path,
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a link token is not a pilot: {route}"
        );
        let (status, _) = send(
            &rig,
            By::Person(&rex),
            Method::POST,
            &path,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}");
        // What is not a seat ask is refused before it is asked.
        let (status, _) = send(
            &rig,
            By::Person(&pia),
            Method::POST,
            &path,
            Some(json!({"seat": 7})),
        )
        .await;
        assert!(status.is_client_error(), "{route}: {status}");
        // An event the club does not hold.
        let (status, _) = send(
            &rig,
            By::Person(&pia),
            Method::POST,
            &format!("/api/clubs/north/events/no-such/public/pilot/checkin/{route}"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
    }
    assert!(director.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refusal_about_the_race_code_has_its_own_status_so_the_page_can_ask_for_it() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let _director = rig.director(
        "north",
        PushAnswer {
            refused: Some("That is not the race code.".into()),
            needs_code: true,
        },
    );
    let (status, answer) = send(
        &rig,
        By::Person(&pia),
        Method::POST,
        "/api/clubs/north/events/friday/public/pilot/checkin/take",
        Some(json!({"race": "practice#1", "adapter": "bench", "seat": "seat-1"})),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(
        answer,
        json!({"code": "Refused", "message": "That is not the race code."})
    );
}

#[tokio::test]
async fn the_directors_reason_is_given_to_the_pilot_in_its_own_words() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let _director = rig.director(
        "north",
        PushAnswer {
            refused: Some("That seat is taken.".into()),
            needs_code: false,
        },
    );
    let (status, answer) = send(
        &rig,
        By::Person(&pia),
        Method::POST,
        TAKE,
        Some(take_body()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(answer["message"], "That seat is taken.");
}

#[tokio::test]
async fn only_someone_tied_to_a_pilot_in_the_event_can_ask() {
    let rig = rig().await;
    let director = rig.director("north", PushAnswer::default());
    // Not signed in.
    let (status, _) = send(&rig, By::Nobody, Method::POST, TAKE, Some(take_body())).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A member who holds no pilot.
    let nobody = rig.member("Nobody", "north", None);
    // A member who holds a pilot of the club that is not in this event.
    let cat = rig.member("Cat", "north", Some("cat-3"));
    // Someone from another club, who holds a pilot there with the same id as one here.
    rig.state
        .accounts
        .note_pilots(
            &club("south"),
            &[ClubPilot {
                pilot: "ace-1".into(),
                callsign: "Other Ace".into(),
                color: None,
            }],
            1,
        )
        .unwrap();
    let outsider = rig.member("Otto", "south", Some("ace-1"));
    for who in [&nobody, &cat, &outsider] {
        let (status, why) =
            send(&rig, By::Person(who), Method::POST, TAKE, Some(take_body())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
        assert!(
            why["message"]
                .as_str()
                .unwrap()
                .contains("not tied to a pilot"),
            "{why}"
        );
    }
    // An event that is not there.
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let (status, _) = send(
        &rig,
        By::Person(&pia),
        Method::POST,
        "/api/clubs/north/events/nowhere/public/pilot/signup/take",
        Some(take_body()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // None of that reached the Director.
    assert!(director.seen.lock().unwrap().is_empty());
    let _ = pia.id;
}

#[tokio::test]
async fn with_no_director_connected_the_pilot_is_told_so() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    // South's Director is listening. North's is not.
    let _south = rig.director("south", PushAnswer::default());
    let began = Instant::now();
    let (status, answer) = send(
        &rig,
        By::Person(&pia),
        Method::POST,
        TAKE,
        Some(take_body()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("not connected"),
        "{answer}"
    );
    assert!(began.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_request_from_another_website_is_refused_before_it_is_asked() {
    let rig = rig().await;
    let pia = rig.member("Pia", "north", Some("ace-1"));
    let director = rig.director("north", PushAnswer::default());
    let request = Request::builder()
        .method(Method::POST)
        .uri(TAKE)
        .header(header::HOST, "grid.example.org")
        .header(header::ORIGIN, "https://evil.example.com")
        .header(header::COOKIE, format!("{SESSION_COOKIE}={}", pia.session))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(take_body().to_string()))
        .unwrap();
    let response = rig.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(director.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_directors_routes_need_its_token_and_show_only_its_club() {
    let rig = rig().await;
    for (method, path) in [
        (Method::GET, "/push/listen"),
        (Method::GET, "/push/requests"),
        (Method::POST, "/push/requests/abc"),
    ] {
        for by in [By::Nobody, By::Token("gfs_not-a-token")] {
            let (status, _) = send(&rig, by, method.clone(), path, Some(json!({}))).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }
    let pia = rig.member("Pia", "north", Some("ace-1"));
    // North's Director is connected and slow to answer.
    let _listening = rig.state.asks.listen(&club("north"));
    let app = rig.app.clone();
    let session = pia.session.clone();
    let asking = tokio::spawn(async move {
        let request = Request::builder()
            .method(Method::POST)
            .uri(TAKE)
            .header(header::HOST, "grid.example.org")
            .header(header::COOKIE, format!("{SESSION_COOKIE}={session}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(take_body().to_string()))
            .unwrap();
        app.oneshot(request).await.unwrap().status()
    });
    let waiting = loop {
        let (status, list) = send(
            &rig,
            By::Token(&rig.north),
            Method::GET,
            "/push/requests",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if list.as_array().is_some_and(|list| !list.is_empty()) {
            break list;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(waiting[0]["pilot"], "ace-1");
    let id = waiting[0]["id"].as_str().unwrap();
    // South's Director sees none of it, and cannot answer it.
    let (_, theirs) = send(
        &rig,
        By::Token(&rig.south),
        Method::GET,
        "/push/requests",
        None,
    )
    .await;
    assert_eq!(theirs, json!([]));
    let (status, _) = send(
        &rig,
        By::Token(&rig.south),
        Method::POST,
        &format!("/push/requests/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // North's answers, and the pilot's request ends with it.
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::POST,
        &format!("/push/requests/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(asking.await.unwrap(), StatusCode::OK);
    // A second answer finds nobody waiting.
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::POST,
        &format!("/push/requests/{id}"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
