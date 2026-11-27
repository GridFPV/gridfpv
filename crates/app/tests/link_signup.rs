//! A pilot signing up for a heat on the server, decided by the Director
//! (docs/primitives.md §13.4, §11.5).
//!
//! Each test starts the server program's own router on a port of this computer, runs a
//! Director with its data in a folder, and talks over real HTTP. The pilot's request goes
//! to the server; the Director hears of it on the stream it opened, and answers.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_app::link::{Client, Memory, spawn, sync_all};
use gridfpv_app::listen::answer_waiting;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::link::{LinkSettings, LinkTrust};
use gridfpv_serverd::accounts::{AccountId, InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::ClubId;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn club() -> ClubId {
    ClubId("club".into())
}

// ── The server ──────────────────────────────────────────────────────────────

struct Server {
    _dir: tempfile::TempDir,
    state: AppState,
    addr: SocketAddr,
    token: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state.book.new_token(&club(), "laptop").unwrap().1;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        _dir: dir,
        state,
        addr,
        token,
        task,
    }
}

impl Server {
    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A member, and the secret of a session they are signed in with.
    fn member_signed_in(&self, name: &str) -> (AccountId, String) {
        let (_, secret) = self
            .state
            .accounts
            .new_invite(
                NewInvite {
                    kind: InviteKind::Personal {
                        club: club(),
                        role: Role::Pilot,
                        pilot: None,
                    },
                    label: name.into(),
                    max_uses: 1,
                    expires_at: None,
                },
                None,
                1,
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
            // At the real time: a session made in 1970 ran out long ago.
            .join_with_invite(&secret, &sign_in, name, gridfpv_serverd::people::now_ms())
            .unwrap();
        (account.id, session)
    }
}

// ── The Director ────────────────────────────────────────────────────────────

struct Director {
    _dir: tempfile::TempDir,
    registry: EventRegistry,
    app: Router,
}

fn director() -> Director {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<title>console</title>").unwrap();
    let registry = EventRegistry::new(Some(dir.path().join("data"))).unwrap();
    Director {
        app: build_app(registry.clone(), dir.path()),
        registry,
        _dir: dir,
    }
}

/// A request to the Director: from the Grid computer (`here`), or from a phone at the field.
async fn ask(
    d: &Director,
    here: bool,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (peer, host) = if here {
        ("127.0.0.1:50000", "127.0.0.1:8080")
    } else {
        ("192.168.1.77:50000", "192.168.1.5:8081")
    };
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, host);
    let body = match body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let response = d.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn direct(d: &Director, method: Method, uri: &str, body: Value) -> Value {
    let (status, value) = ask(d, true, method.clone(), uri, Some(body.clone())).await;
    assert!(
        status.is_success(),
        "{method} {uri} {body} answered {status}: {value}"
    );
    value
}

/// Add a pilot to the Director.
async fn pilot(d: &Director, callsign: &str) -> String {
    direct(d, Method::POST, "/pilots", json!({"callsign": callsign})).await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn pilots_of(d: &Director) -> Vec<Value> {
    let (status, list) = ask(d, true, Method::GET, "/pilots", None).await;
    assert_eq!(status, StatusCode::OK);
    list.as_array().unwrap().clone()
}

fn link(d: &Director, s: &Server) {
    d.registry
        .link()
        .save(LinkSettings {
            url: s.url(),
            token: Some(s.token.clone()),
            on: true,
            trust: LinkTrust::System,
        })
        .unwrap();
}

fn client(d: &Director) -> Client {
    Client::new(&d.registry.link().stored()).unwrap()
}

async fn until(what: &str, seconds: u64, mut check: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !check() {
        assert!(tokio::time::Instant::now() < end, "never happened: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn set_account(d: &Director, pilot: &str, account: Value) -> (StatusCode, Value) {
    ask(
        d,
        true,
        Method::PUT,
        &format!("/pilots/{pilot}/server-account"),
        Some(json!({"account": account})),
    )
    .await
}

// ── A race day with sign-up ─────────────────────────────────────────────────

const QUALI: &str = "quali";

fn seat(n: u32) -> Value {
    json!({"adapter": "bench", "seat": format!("seat-{n}")})
}

fn heat(n: u32) -> String {
    format!("{QUALI}/heat-{n}")
}

/// An open event whose qualifying phase has sign-up on, with Ace and Bee in it. On the
/// server Ace is Pia's and Bee is Quin's, and the event has been pushed.
struct Day {
    event: String,
    pia: String,
    quin: String,
}

async fn race_day(s: &Server, d: &Director) -> Day {
    let ace = pilot(d, "Ace").await;
    let bee = pilot(d, "Bee").await;
    let class = direct(
        d,
        Method::POST,
        "/classes",
        json!({"name": "Open", "source": "Custom"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let event = direct(d, Method::POST, "/events", json!({"name": "Friday Night"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    direct(d, Method::PUT, "/active-event", json!({"id": event})).await;
    let commands = format!("/events/{event}/commands");
    direct(
        d,
        Method::POST,
        &commands,
        json!({"Record": {"RosterSet": {"class": class, "pilots": [ace, bee]}}}),
    )
    .await;
    direct(
        d,
        Method::POST,
        &commands,
        json!({"Record": {"PhaseDefined": {"def": {
            "id": QUALI,
            "label": "Qualifying",
            "kind": {"Group": {"field": [{"Class": class}], "combine": "Stack"}},
            "format": {
                "name": "One race each",
                "race": {"end": "Open", "staging_micros": 0, "first_lap": "HoleShot"},
                "completion": {"SavedRaces": 1},
                "ranking": {"sort": ["LastFinish"]}
            }
        }}}}),
    )
    .await;
    set_rules(d, &event, json!({})).await;

    link(d, s);
    let (pia, pias) = s.member_signed_in("Pia");
    let (quin, quins) = s.member_signed_in("Quin");
    assert_eq!(set_account(d, &ace, json!(pia.0)).await.0, StatusCode::OK);
    assert_eq!(set_account(d, &bee, json!(quin.0)).await.0, StatusCode::OK);
    sync_all(&d.registry, &client(d), &mut Memory::default())
        .await
        .unwrap();
    Day {
        event,
        pia: pias,
        quin: quins,
    }
}

/// The director sets the phase's sign-up rules.
async fn set_rules(d: &Director, event: &str, over: Value) {
    let mut rules = json!({"seats": [seat(1), seat(2), seat(3)], "open_heats": 3, "open": true});
    for (key, value) in over.as_object().unwrap() {
        rules[key] = value.clone();
    }
    direct(
        d,
        Method::POST,
        &format!("/events/{event}/commands"),
        json!({"Signup": {"Set": {"phase": QUALI, "rules": rules}}}),
    )
    .await;
}

/// Who is on each seat of each heat, as the Director has it: `"Ace,-,-"`.
async fn spots(d: &Director, event: &str) -> Vec<String> {
    let (_, state) = ask(
        d,
        true,
        Method::GET,
        &format!("/events/{event}/state"),
        None,
    )
    .await;
    sheet(&state, &pilots_of(d).await)
}

/// The same, read from the server's copy as anyone may read it.
async fn spots_on_server(s: &Server, d: &Director, event: &str) -> Vec<String> {
    let request = Request::builder()
        .uri(format!("/api/clubs/club/events/{event}/public/event/state"))
        .body(Body::empty())
        .unwrap();
    let response = router(s.state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    sheet(
        &serde_json::from_slice(&bytes).unwrap(),
        &pilots_of(d).await,
    )
}

fn sheet(state: &Value, pilots: &[Value]) -> Vec<String> {
    let signup = &state["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["def"]["id"] == QUALI)
        .expect("the qualifying phase")["signup"];
    signup["heats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|heat| {
            heat["seats"]
                .as_array()
                .unwrap()
                .iter()
                .map(|on| match on.as_str() {
                    None => "-".to_owned(),
                    Some(id) => pilots
                        .iter()
                        .find(|pilot| pilot["id"] == id)
                        .map_or(id.to_owned(), |pilot| {
                            pilot["callsign"].as_str().unwrap().to_owned()
                        }),
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect()
}

/// A signed-in member's request to the server. It waits there for the Director.
fn as_member(
    s: &Server,
    session: &str,
    event: &str,
    route: &str,
    body: Value,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = router(s.state.clone());
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/api/clubs/club/events/{event}/public/pilot/signup/{route}"
        ))
        .header(header::HOST, "grid.example.org")
        .header(header::COOKIE, format!("grid_session={session}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    tokio::spawn(async move {
        let response = app.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    })
}

/// The Director, one step at a time: it is listening, and answers what is waiting when
/// told to. Returns the pilot's answer.
async fn decided(
    s: &Server,
    d: &Director,
    asking: tokio::task::JoinHandle<(StatusCode, Value)>,
) -> (StatusCode, Value) {
    let client = client(d);
    // A Director that is connected. The stream is held open for as long as this runs.
    let _stream = client.listen().await.unwrap();
    let registry = d.registry.clone();
    let _ = s;
    let end = tokio::time::Instant::now() + Duration::from_secs(8);
    while !asking.is_finished() {
        assert!(
            tokio::time::Instant::now() < end,
            "the pilot never got an answer"
        );
        answer_waiting(&registry, &client).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    asking.await.unwrap()
}

/// Hold a Director's stream open, so the server takes asks, without answering any.
async fn connected(d: &Director) -> reqwest::Response {
    client(d).listen().await.unwrap()
}

// ── One ask at a time ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_spot_taken_on_the_server_is_taken_on_the_director_and_comes_back_up() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    assert_eq!(spots(&d, &day.event).await, ["-,-,-", "-,-,-", "-,-,-"]);

    let _stream = connected(&d).await;
    let asking = as_member(
        &s,
        &day.pia,
        &day.event,
        "take",
        json!({"heat": heat(1), "seat": seat(2)}),
    );
    let (status, answer) = decided(&s, &d, asking).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    // Pia's account holds Ace, so it is Ace who has the spot.
    assert_eq!(spots(&d, &day.event).await[0], "-,Ace,-");

    // The server changed nothing itself: its copy still shows no spot until the Director
    // pushes, and then it shows what the Director decided.
    assert_eq!(spots_on_server(&s, &d, &day.event).await[0], "-,-,-");
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    assert_eq!(spots_on_server(&s, &d, &day.event).await[0], "-,Ace,-");
}

#[tokio::test]
async fn the_directors_rules_decide_and_its_reason_reaches_the_pilot() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    let _stream = connected(&d).await;
    let take = |session: &str, n: u32| {
        as_member(
            &s,
            session,
            &day.event,
            "take",
            json!({"heat": heat(1), "seat": seat(n)}),
        )
    };
    assert_eq!(decided(&s, &d, take(&day.pia, 2)).await.0, StatusCode::OK);

    // Quin asks for the seat Ace has.
    let (status, why) = decided(&s, &d, take(&day.quin, 2)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}");
    let words = why["message"].as_str().unwrap();
    assert!(!words.is_empty() && !words.contains("PilotId"), "{words}");
    assert_eq!(spots(&d, &day.event).await[0], "-,Ace,-");

    // The director closes sign-up. A pilot's ask is a pilot's: it is refused, as it
    // would be at the race.
    set_rules(&d, &day.event, json!({"open": false})).await;
    let (status, why) = decided(&s, &d, take(&day.quin, 3)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{why}");
    assert_eq!(spots(&d, &day.event).await[0], "-,Ace,-");
}

#[tokio::test]
async fn two_pilots_asking_for_one_seat_at_the_same_moment_never_both_get_it() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    let _stream = connected(&d).await;
    let client = client(&d);
    let ask_for =
        |session: &str, route: &str, body: Value| as_member(&s, session, &day.event, route, body);

    for round in 0..15 {
        // Both ask for the same seat before the Director has looked at either.
        let seat_no = 1 + (round % 3);
        let body = json!({"heat": heat(1), "seat": seat(seat_no)});
        let (first, second) = if round % 2 == 0 {
            (&day.pia, &day.quin)
        } else {
            (&day.quin, &day.pia)
        };
        let a = ask_for(first, "take", body.clone());
        let b = ask_for(second, "take", body);
        let end = tokio::time::Instant::now() + Duration::from_secs(8);
        while s.state.asks.waiting(&club()).len() < 2 {
            assert!(tokio::time::Instant::now() < end, "both asks never arrived");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The Director answers everything that is waiting, in one go.
        assert_eq!(answer_waiting(&d.registry, &client).await.unwrap(), 2);
        let (a, b) = (a.await.unwrap(), b.await.unwrap());
        let got: Vec<StatusCode> = vec![a.0, b.0];
        assert_eq!(
            got.iter()
                .filter(|status| **status == StatusCode::OK)
                .count(),
            1,
            "round {round}: exactly one of them gets the seat: {a:?} {b:?}"
        );
        assert_eq!(
            got.iter()
                .filter(|status| **status == StatusCode::CONFLICT)
                .count(),
            1,
            "round {round}: and the other is told no: {a:?} {b:?}"
        );
        // The sheet has one pilot on that seat, and nobody anywhere else in the heat.
        let row = spots(&d, &day.event).await[0].clone();
        let taken: Vec<&str> = row.split(',').filter(|who| *who != "-").collect();
        assert_eq!(taken.len(), 1, "round {round}: {row}");
        assert!(taken[0] == "Ace" || taken[0] == "Bee", "{row}");
        assert_ne!(
            row.split(',').nth(seat_no as usize - 1).unwrap(),
            "-",
            "{row}"
        );
        // Nothing is left waiting on the server.
        assert!(s.state.asks.waiting(&club()).is_empty());

        // Whoever got it gives it up, ready for the next round. The other has nothing to
        // leave, and is told so rather than taking anyone's spot away.
        let leaving_a = ask_for(first, "leave", json!({"heat": heat(1)}));
        let leaving_b = ask_for(second, "leave", json!({"heat": heat(1)}));
        while s.state.asks.waiting(&club()).len() < 2 {
            assert!(tokio::time::Instant::now() < end, "both asks never arrived");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        answer_waiting(&d.registry, &client).await.unwrap();
        let left = [leaving_a.await.unwrap().0, leaving_b.await.unwrap().0];
        assert_eq!(
            left.iter()
                .filter(|status| **status == StatusCode::OK)
                .count(),
            1,
            "round {round}: {left:?}"
        );
        assert_eq!(spots(&d, &day.event).await[0], "-,-,-", "round {round}");
    }
    // What the Director wrote is what the server ends up with.
    sync_all(&d.registry, &client, &mut Memory::default())
        .await
        .unwrap();
    assert_eq!(spots_on_server(&s, &d, &day.event).await[0], "-,-,-");
}

#[tokio::test]
async fn the_next_free_spot_and_leaving_work_the_same_way() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    let _stream = connected(&d).await;

    let next = as_member(
        &s,
        &day.quin,
        &day.event,
        "next",
        json!({"phase": QUALI, "seat": seat(1)}),
    );
    assert_eq!(decided(&s, &d, next).await.0, StatusCode::OK);
    assert_eq!(spots(&d, &day.event).await[0], "Bee,-,-");

    // Pia cannot give up Bee's spot: she has none in that heat.
    let not_hers = as_member(&s, &day.pia, &day.event, "leave", json!({"heat": heat(1)}));
    assert_eq!(decided(&s, &d, not_hers).await.0, StatusCode::CONFLICT);
    assert_eq!(spots(&d, &day.event).await[0], "Bee,-,-");

    let leave = as_member(&s, &day.quin, &day.event, "leave", json!({"heat": heat(1)}));
    assert_eq!(decided(&s, &d, leave).await.0, StatusCode::OK);
    assert_eq!(spots(&d, &day.event).await[0], "-,-,-");
}

#[tokio::test]
async fn only_the_event_that_is_open_at_the_race_takes_sign_ups() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    let _stream = connected(&d).await;
    // The director opens another event.
    let other = direct(&d, Method::POST, "/events", json!({"name": "Saturday"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    direct(&d, Method::PUT, "/active-event", json!({"id": other})).await;

    let asking = as_member(
        &s,
        &day.pia,
        &day.event,
        "take",
        json!({"heat": heat(1), "seat": seat(1)}),
    );
    let (status, why) = decided(&s, &d, asking).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        why["message"]
            .as_str()
            .unwrap()
            .contains("not the one open"),
        "{why}"
    );
    assert_eq!(spots(&d, &day.event).await[0], "-,-,-");
}

#[tokio::test]
async fn an_ask_the_director_cannot_stand_behind_is_refused_or_left_alone() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    let _stream = connected(&d).await;
    let client = client(&d);
    let event = gridfpv_server::events::EventId(day.event.clone());
    let take = gridfpv_server::remote::PilotAsk::Take {
        heat: gridfpv_events::PhaseId(heat(1)),
        seat: serde_json::from_value(seat(1)).unwrap(),
    };

    // The server names a pilot this Director does not have.
    let asks = s.state.asks.clone();
    let ghost = {
        let (event, take) = (event.clone(), take.clone());
        tokio::spawn(async move {
            asks.ask(
                &club(),
                event,
                gridfpv_events::PilotId("ghost-9".into()),
                take,
                None,
                Duration::from_secs(5),
            )
            .await
        })
    };
    until("the ask is waiting", 5, || {
        !s.state.asks.waiting(&club()).is_empty()
    })
    .await;
    assert_eq!(answer_waiting(&d.registry, &client).await.unwrap(), 1);
    let refused = ghost.await.unwrap().unwrap().refused.unwrap();
    assert!(refused.contains("does not have that pilot"), "{refused}");
    assert_eq!(spots(&d, &day.event).await[0], "-,-,-");

    // An event this Director does not have is another Director's: it is not answered.
    let asks = s.state.asks.clone();
    let elsewhere = tokio::spawn(async move {
        asks.ask(
            &club(),
            gridfpv_server::events::EventId("elsewhere".into()),
            gridfpv_events::PilotId("ghost-9".into()),
            take,
            None,
            Duration::from_millis(400),
        )
        .await
    });
    until("the ask is waiting", 5, || {
        !s.state.asks.waiting(&club()).is_empty()
    })
    .await;
    assert_eq!(answer_waiting(&d.registry, &client).await.unwrap(), 0);
    assert_eq!(
        s.state.asks.waiting(&club()).len(),
        1,
        "it is left for whoever has it"
    );
    assert!(elsewhere.await.unwrap().is_err());
}

// ── The task, left to run ───────────────────────────────────────────────────

#[tokio::test]
async fn left_running_a_sign_up_on_the_server_lands_and_shows_there_by_itself() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    // Nobody is connected yet: the pilot is told so, at once.
    let (status, why) = as_member(
        &s,
        &day.pia,
        &day.event,
        "take",
        json!({"heat": heat(1), "seat": seat(1)}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        why["message"].as_str().unwrap().contains("not connected"),
        "{why}"
    );

    let task = spawn(d.registry.clone());
    until("the Director is listening", 10, || {
        s.state.asks.listening(&club())
    })
    .await;

    // Nothing here answers by hand. The Director hears of the ask and decides it.
    let began = std::time::Instant::now();
    let (status, answer) = as_member(
        &s,
        &day.pia,
        &day.event,
        "take",
        json!({"heat": heat(1), "seat": seat(1)}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "it was heard at once, not on a slow poll: {:?}",
        began.elapsed()
    );
    assert_eq!(spots(&d, &day.event).await[0], "Ace,-,-");

    // And the server's own copy shows it, pushed by the same task.
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while spots_on_server(&s, &d, &day.event).await[0] != "Ace,-,-" {
        assert!(
            tokio::time::Instant::now() < end,
            "the spot never showed on the server"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A second ask, on the same stream.
    let (status, _) = as_member(
        &s,
        &day.quin,
        &day.event,
        "take",
        json!({"heat": heat(1), "seat": seat(2)}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(spots(&d, &day.event).await[0], "Ace,Bee,-");

    // The director switches the link off. The server soon knows nobody is listening.
    d.registry
        .link()
        .save(LinkSettings {
            url: s.url(),
            token: None,
            on: false,
            trust: LinkTrust::System,
        })
        .unwrap();
    until("the server sees the Director has gone", 20, || {
        !s.state.asks.listening(&club())
    })
    .await;
    task.abort();
}

/// Something between the Director and the server that can cut every connection, as a
/// network that drops would.
struct Wire {
    addr: SocketAddr,
    open: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    accepting: tokio::task::JoinHandle<()>,
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.accepting.abort();
        self.cut();
    }
}

impl Wire {
    async fn to(server: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let open = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let held = std::sync::Arc::clone(&open);
        let accepting = tokio::spawn(async move {
            while let Ok((mut near, _)) = listener.accept().await {
                let carry = tokio::spawn(async move {
                    if let Ok(mut far) = tokio::net::TcpStream::connect(server).await {
                        tokio::io::copy_bidirectional(&mut near, &mut far)
                            .await
                            .ok();
                    }
                });
                held.lock().unwrap().push(carry);
            }
        });
        Self {
            addr,
            open,
            accepting,
        }
    }

    /// Drop every connection that is open. New ones are still taken.
    fn cut(&self) {
        for carry in self.open.lock().unwrap().drain(..) {
            carry.abort();
        }
    }
}

#[tokio::test]
async fn left_running_a_stream_that_drops_is_opened_again() {
    let s = server().await;
    let d = director();
    let day = race_day(&s, &d).await;
    // From here the Director reaches the server over a wire that can be cut.
    let wire = Wire::to(s.addr).await;
    d.registry
        .link()
        .save(LinkSettings {
            url: format!("http://{}", wire.addr),
            token: Some(s.token.clone()),
            on: true,
            trust: LinkTrust::System,
        })
        .unwrap();
    let task = spawn(d.registry.clone());
    until("the Director is listening", 10, || {
        s.state.asks.listening(&club())
    })
    .await;

    wire.cut();
    until("the server sees the stream is gone", 10, || {
        !s.state.asks.listening(&club())
    })
    .await;
    // Nobody does anything. The Director opens the stream again by itself.
    until("the Director is listening again", 20, || {
        s.state.asks.listening(&club())
    })
    .await;

    let (status, answer) = as_member(
        &s,
        &day.pia,
        &day.event,
        "take",
        json!({"heat": heat(2), "seat": seat(3)}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(spots(&d, &day.event).await[1], "-,-,Ace");
    task.abort();
}
