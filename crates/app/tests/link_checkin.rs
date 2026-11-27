//! A pilot checking in to an open seat from the server, decided by the Director
//! (docs/primitives.md §11.9, §13.13).
//!
//! Each test starts the server program's own router on a port of this computer, runs a
//! Director with its data in a folder, and talks over real HTTP. The pilot's request goes
//! to the server with the race code they typed; the Director hears of it on the stream it
//! opened, checks the code, and answers.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_app::link::{Client, Memory, spawn, sync_all};
use gridfpv_app::listen::answer_waiting;
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::link::{ClubSettings, LinkTrust};
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

/// Save the server as the Director's one club. With `on`, every event the Director has
/// now goes to it. Without, none does.
fn link_to(registry: &EventRegistry, url: &str, token: Option<&str>, on: bool, trust: LinkTrust) {
    let desk = registry.link();
    let settings = ClubSettings {
        name: String::new(),
        url: url.to_owned(),
        token: token.map(str::to_owned),
        trust,
    };
    let saved = desk.stored().clubs.first().map(|club| club.id.clone());
    let id = desk.save_club(saved.as_deref(), settings).unwrap();
    for meta in registry.list() {
        desk.set_event_club(&meta.id, on.then_some(id.as_str()))
            .unwrap();
    }
}

/// A client for the saved club, as the pusher would make one. An event made since the
/// club was saved goes to it too: these tests send every event.
fn client_of(registry: &EventRegistry) -> Client {
    let desk = registry.link();
    let club = desk
        .stored()
        .clubs
        .first()
        .cloned()
        .expect("a club is saved");
    for meta in registry.list() {
        if desk.club_id_of(&meta.id).is_none() {
            desk.set_event_club(&meta.id, Some(&club.id)).unwrap();
        }
    }
    desk.begin(&club);
    Client::new(&club).unwrap()
}

fn link(d: &Director, s: &Server) {
    link_to(
        &d.registry,
        &s.url(),
        Some(&s.token),
        true,
        LinkTrust::System,
    );
}

fn client(d: &Director) -> Client {
    client_of(&d.registry)
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

// ── A practice day ──────────────────────────────────────────────────────────

const PRACTICE: &str = "practice";
const RACE: &str = "practice#1";

fn seat(n: u32) -> Value {
    json!({"adapter": "bench", "seat": format!("seat-{n}")})
}

/// An open event with one practice heat of two open seats and one seat with Cat named on
/// it, and its race running. On the server Ace is Pia's and Bee is Quin's, and the event
/// has been pushed. The Director is linked, and its race code is `code`.
struct Day {
    event: String,
    ace: String,
    bee: String,
    pia: String,
    quin: String,
    code: String,
    /// The Director's stream to the server, held open: while it is, the server takes asks.
    stream: Option<reqwest::Response>,
}

async fn practice_day(s: &Server, d: &Director) -> Day {
    let ace = pilot(d, "Ace").await;
    let bee = pilot(d, "Bee").await;
    let cat = pilot(d, "Cat").await;
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
        json!({"Record": {"RosterSet": {"class": class, "pilots": [ace, bee, cat]}}}),
    )
    .await;
    let mut seats = vec![seat(1), seat(2), seat(3)];
    seats[0]["source"] = json!("Open");
    seats[1]["source"] = json!("Open");
    seats[2]["source"] = json!({ "Pilot": cat });
    direct(
        d,
        Method::POST,
        &commands,
        json!({"Record": {"PhaseDefined": {"def": {
            "id": PRACTICE,
            "label": "Open Practice",
            "kind": {"Heat": {"lineup": seats}},
            "format": {
                "name": "Open practice",
                "race": {"end": "Open", "staging_micros": 0, "first_lap": "HoleShot"},
                "completion": "DirectorDecides"
            }
        }}}}),
    )
    .await;
    direct(
        d,
        Method::POST,
        &commands,
        json!({"StartRace": {"heat": PRACTICE}}),
    )
    .await;

    link(d, s);
    let (pia, pias) = s.member_signed_in("Pia");
    let (quin, quins) = s.member_signed_in("Quin");
    assert_eq!(set_account(d, &ace, json!(pia.0)).await.0, StatusCode::OK);
    assert_eq!(set_account(d, &bee, json!(quin.0)).await.0, StatusCode::OK);
    push(d).await;
    let code = race_code(d, &event).await;
    let stream = Some(client(d).listen().await.unwrap());
    Day {
        stream,
        event,
        ace,
        bee,
        pia: pias,
        quin: quins,
        code,
    }
}

async fn push(d: &Director) {
    sync_all(&d.registry, &client(d), &mut Memory::default())
        .await
        .unwrap();
}

/// The event's race code, as the director's console reads it.
async fn race_code(d: &Director, event: &str) -> String {
    let (status, view) = ask(
        d,
        true,
        Method::GET,
        &format!("/events/{event}/race-code"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view["code"].as_str().unwrap().to_owned()
}

/// A crossing on a seat, `at` seconds on the bench clock.
async fn cross(d: &Director, event: &str, n: u32, at: i64) {
    let mut body = seat(n);
    body["at"] = json!(at * 1_000_000);
    direct(d, Method::POST, &format!("/events/{event}/passes"), body).await;
}

/// Who holds each seat of the race, by pilot id, as a state shows it.
fn holders(state: &Value) -> Vec<String> {
    state["races"][0]["seats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|seat| seat["holder"].as_str().unwrap_or("-").to_owned())
        .collect()
}

async fn holders_here(d: &Director, event: &str) -> Vec<String> {
    let (status, state) = ask(
        d,
        true,
        Method::GET,
        &format!("/events/{event}/state"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    holders(&state)
}

async fn holders_on_server(s: &Server, event: &str) -> Vec<String> {
    let request = Request::builder()
        .uri(format!("/api/clubs/club/events/{event}/public/event/state"))
        .body(Body::empty())
        .unwrap();
    let response = router(s.state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    holders(&serde_json::from_slice(&bytes).unwrap())
}

/// A signed-in member asks the server for a seat, or to give one up.
fn as_member(
    s: &Server,
    session: Option<&str>,
    event: &str,
    route: &str,
    body: Value,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let app = router(s.state.clone());
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "/api/clubs/club/events/{event}/public/pilot/checkin/{route}"
        ))
        .header(header::HOST, "grid.example.org")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        request = request.header(header::COOKIE, format!("grid_session={session}"));
    }
    let request = request.body(Body::from(body.to_string())).unwrap();
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

fn take(
    s: &Server,
    session: &str,
    day: &Day,
    n: u32,
    code: Option<&str>,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let mut body = seat(n);
    body["race"] = json!(RACE);
    if let Some(code) = code {
        body["code"] = json!(code);
    }
    as_member(s, Some(session), &day.event, "take", body)
}

/// The Director, one step at a time: it is listening, and answers what is waiting. Returns
/// the pilot's answer.
async fn decided(
    d: &Director,
    asking: tokio::task::JoinHandle<(StatusCode, Value)>,
) -> (StatusCode, Value) {
    let client = client(d);
    let end = tokio::time::Instant::now() + Duration::from_secs(8);
    while !asking.is_finished() {
        assert!(
            tokio::time::Instant::now() < end,
            "the pilot never got an answer"
        );
        answer_waiting(&d.registry, &client).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    asking.await.unwrap()
}

/// Everything the server keeps on disk, as text: its records and every event.
fn on_the_servers_disk(s: &Server) -> String {
    fn walk(dir: &std::path::Path, out: &mut String) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = String::new();
    walk(s._dir.path(), &mut out);
    out
}

// ── Taking a seat ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_seat_taken_on_the_server_with_the_race_code_is_the_pilots_and_so_are_the_laps() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);
    // Someone is lapping on seat 1 with no name.
    cross(&d, &day.event, 1, 0).await;
    cross(&d, &day.event, 1, 20).await;

    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    // Pia's account holds Ace, so it is Ace who has the seat.
    assert_eq!(
        holders_here(&d, &day.event).await,
        [day.ace.as_str(), "-", "-"]
    );

    // Her first crossing starts her first lap. The lap under way was not hers.
    cross(&d, &day.event, 1, 45).await;
    cross(&d, &day.event, 1, 60).await;
    let (_, state) = ask(
        &d,
        true,
        Method::GET,
        &format!("/events/{}/state", day.event),
        None,
    )
    .await;
    let stints = state["races"][0]["seats"][0]["stints"].as_array().unwrap();
    assert_eq!(stints.len(), 1);
    assert_eq!(stints[0]["pilot"], json!(day.ace));
    assert_eq!(stints[0]["laps"].as_array().unwrap().len(), 1);
    assert_eq!(stints[0]["laps"][0]["micros"], json!(15_000_000));

    // The server changed nothing itself. It shows the seat once the Director pushes.
    assert_eq!(holders_on_server(&s, &day.event).await, ["-", "-", "-"]);
    push(&d).await;
    assert_eq!(
        holders_on_server(&s, &day.event).await,
        [day.ace.as_str(), "-", "-"]
    );
}

#[tokio::test]
async fn the_code_is_taken_as_a_person_types_it() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let spaced = format!(
        "  {}-{} ",
        day.code[..3].to_lowercase(),
        day.code[3..].to_lowercase()
    );
    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(&spaced))).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(holders_here(&d, &day.event).await[0], day.ace);
}

#[tokio::test]
async fn without_the_race_code_or_with_a_wrong_one_no_seat_is_taken() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let wrong = if day.code == "AAAAAA" {
        "BBBBBB"
    } else {
        "AAAAAA"
    };

    for (code, words) in [
        (None, "Type the race code first"),
        (Some(""), "Type the race code first"),
        (Some("   "), "Type the race code first"),
        (Some(wrong), "That is not the race code"),
        (Some("not a code at all"), "That is not the race code"),
    ] {
        let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, code)).await;
        assert_eq!(
            status,
            StatusCode::PRECONDITION_REQUIRED,
            "{code:?}: {answer}"
        );
        assert_eq!(answer["code"], "Refused");
        assert!(
            answer["message"].as_str().unwrap().contains(words),
            "{code:?}: {answer}"
        );
        assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);
    }
    // The answer never says what the code is.
    let (_, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(wrong))).await;
    assert!(!answer.to_string().contains(&day.code));
}

#[tokio::test]
async fn a_new_race_code_ends_the_old_one() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let (status, view) = ask(
        &d,
        true,
        Method::POST,
        &format!("/events/{}/race-code/new", day.event),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    let new = view["code"].as_str().unwrap().to_owned();
    assert_ne!(new, day.code);
    assert_eq!(race_code(&d, &day.event).await, new, "it is kept");

    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{answer}");
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("not the race code")
    );
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);

    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(&new))).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(holders_here(&d, &day.event).await[0], day.ace);
}

#[tokio::test]
async fn too_many_wrong_codes_makes_a_pilot_wait_and_does_not_lock_anyone_else_out() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let wrong = if day.code == "AAAAAA" {
        "BBBBBB"
    } else {
        "AAAAAA"
    };
    for _ in 0..5 {
        let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(wrong))).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
        assert!(
            answer["message"]
                .as_str()
                .unwrap()
                .contains("not the race code")
        );
    }
    // Now even the right code is not taken from Pia for a while.
    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("Too many wrong race codes"),
        "{answer}"
    );
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);

    // Quin is not held up by Pia's guesses.
    let (status, answer) = decided(&d, take(&s, &day.quin, &day, 2, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        holders_here(&d, &day.event).await,
        ["-", day.bee.as_str(), "-"]
    );
}

#[tokio::test]
async fn the_directors_own_rules_still_decide_and_its_words_reach_the_pilot() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    // A refusal that is the Director's own rule, and not about the code or the connection.
    let its_own = |answer: &Value| {
        let words = answer["message"].as_str().unwrap().to_owned();
        assert!(!words.is_empty());
        assert!(!words.contains("race code"), "{words}");
        assert!(!words.contains("not connected"), "{words}");
        assert!(!words.contains("did not answer"), "{words}");
    };
    let _ = &its_own;

    // Seat 3 has Cat named on it. It is not an open seat.
    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 3, Some(&day.code))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "Refused");
    its_own(&answer);
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);

    // A seat the heat does not have.
    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 9, Some(&day.code))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    its_own(&answer);

    // A race that is not there.
    let mut body = seat(1);
    body["race"] = json!("practice#7");
    body["code"] = json!(day.code);
    let (status, answer) =
        decided(&d, as_member(&s, Some(&day.pia), &day.event, "take", body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    its_own(&answer);
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);

    // Once the race is stopped, nobody takes a seat in it.
    direct(
        &d,
        Method::POST,
        &format!("/events/{}/commands", day.event),
        json!({"StopRace": {"race": RACE}}),
    )
    .await;
    let (status, answer) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    its_own(&answer);
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);
}

#[tokio::test]
async fn a_pilot_who_takes_another_seat_leaves_the_first_and_one_seat_changes_hands() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let (status, _) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = decided(&d, take(&s, &day.pia, &day, 2, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        holders_here(&d, &day.event).await,
        ["-", day.ace.as_str(), "-"]
    );
    // Quin takes the seat Ace is on.
    let (status, _) = decided(&d, take(&s, &day.quin, &day, 2, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        holders_here(&d, &day.event).await,
        ["-", day.bee.as_str(), "-"]
    );
}

// ── Giving a seat up ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_pilot_drops_their_own_seat_with_no_code_and_nobody_elses() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let (status, _) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK);

    // Quin has no seat. Dropping does not take Ace's.
    let dropping = as_member(
        &s,
        Some(&day.quin),
        &day.event,
        "drop",
        json!({"race": RACE}),
    );
    let (status, answer) = decided(&d, dropping).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["message"], "You do not have a seat to drop.");
    assert_eq!(holders_here(&d, &day.event).await[0], day.ace);

    // A body that names a seat or a pilot is no way to drop someone else's.
    let mut sneaky = seat(1);
    sneaky["race"] = json!(RACE);
    sneaky["pilot"] = json!(day.ace);
    let (status, _) = decided(
        &d,
        as_member(&s, Some(&day.quin), &day.event, "drop", sneaky),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(holders_here(&d, &day.event).await[0], day.ace);

    let dropping = as_member(
        &s,
        Some(&day.pia),
        &day.event,
        "drop",
        json!({"race": RACE}),
    );
    let (status, answer) = decided(&d, dropping).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);
}

// ── Who may ask ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn only_someone_signed_in_and_tied_to_a_pilot_of_the_event_reaches_the_director() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let mut body = seat(1);
    body["race"] = json!(RACE);
    body["code"] = json!(day.code);

    // Not signed in.
    for route in ["take", "drop"] {
        let (status, _) = as_member(&s, None, &day.event, route, body.clone())
            .await
            .unwrap();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{route}");
        let (status, _) = as_member(&s, Some("not-a-session"), &day.event, route, body.clone())
            .await
            .unwrap();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{route}");
    }
    // Signed in, with no pilot in the event. Knowing the code is not enough.
    let (_, rex) = s.member_signed_in("Rex");
    for route in ["take", "drop"] {
        let (status, answer) = as_member(&s, Some(&rex), &day.event, route, body.clone())
            .await
            .unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN, "{route}: {answer}");
    }
    // A body that names another pilot is still decided as the asker's own pilot.
    let mut sneaky = body.clone();
    sneaky["pilot"] = json!(day.bee);
    let (status, _) = decided(
        &d,
        as_member(&s, Some(&day.pia), &day.event, "take", sneaky),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        holders_here(&d, &day.event).await,
        [day.ace.as_str(), "-", "-"]
    );
    // Nothing was put before the Director for those who were turned away.
    assert!(s.state.asks.waiting(&club()).is_empty());
}

#[tokio::test]
async fn with_no_director_connected_the_pilot_is_told_at_once() {
    let s = server().await;
    let d = director();
    let mut day = practice_day(&s, &d).await;
    // The Director goes away, and the server notices.
    drop(day.stream.take());
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    while s.state.asks.listening(&club()) {
        assert!(
            tokio::time::Instant::now() < end,
            "the server never noticed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let began = std::time::Instant::now();
    let (status, answer) = take(&s, &day.pia, &day, 1, Some(&day.code)).await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("not connected"),
        "{answer}"
    );
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);
}

// ── The code itself ─────────────────────────────────────────────────────────

#[tokio::test]
async fn the_server_never_keeps_the_race_code() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let (status, _) = decided(&d, take(&s, &day.pia, &day, 1, Some(&day.code))).await;
    assert_eq!(status, StatusCode::OK);
    cross(&d, &day.event, 1, 45).await;
    push(&d).await;
    // The seat is on the server, and the code that won it is not.
    assert_eq!(holders_on_server(&s, &day.event).await[0], day.ace);
    assert!(
        !on_the_servers_disk(&s).contains(&day.code),
        "the race code is on the server's disk"
    );
    // Nor is it in the Director's own log, which is what goes up.
    let log = d.registry.log(&EventId(day.event.clone())).unwrap();
    let log = log.lock().unwrap();
    for entry in log.read_raw_from(0).unwrap() {
        assert!(!entry.json.contains(&day.code), "{}", entry.json);
    }
}

#[tokio::test]
async fn a_long_code_is_cut_short_before_it_reaches_the_director() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let long = "A".repeat(5_000);
    let asking = take(&s, &day.pia, &day, 1, Some(&long));
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    let waiting = loop {
        let waiting = s.state.asks.waiting(&club());
        if !waiting.is_empty() {
            break waiting;
        }
        assert!(tokio::time::Instant::now() < end, "the ask never arrived");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(waiting[0].code.as_deref().map(str::len), Some(64));
    answer_waiting(&d.registry, &client(&d)).await.unwrap();
    let (status, _) = asking.await.unwrap();
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
}

#[tokio::test]
async fn the_race_code_is_the_directors_alone_and_is_kept_over_a_restart() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let code = format!("/events/{}/race-code", day.event);

    // A phone at the field is not shown it, cannot make a new one, and gets no QR code.
    for (method, path) in [
        (Method::GET, code.clone()),
        (Method::POST, format!("{code}/new")),
        (Method::GET, format!("{code}/qr")),
    ] {
        let (status, answer) = ask(&d, false, method.clone(), &path, None).await;
        assert!(
            status == StatusCode::FORBIDDEN
                || status == StatusCode::UNAUTHORIZED
                || status == StatusCode::NOT_FOUND,
            "{method} {path}: {status}"
        );
        assert!(!answer.to_string().contains(&day.code));
    }
    assert_eq!(
        race_code(&d, &day.event).await,
        day.code,
        "not changed by that"
    );
    // An event that is not there has no code.
    let (status, _) = ask(
        &d,
        true,
        Method::GET,
        "/events/no-such-event/race-code",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The same code after Grid is started again on the same data.
    let again = EventRegistry::new(Some(d._dir.path().join("data"))).unwrap();
    assert_eq!(
        again.pilot_access().race_code(&EventId(day.event.clone())),
        day.code
    );
}

#[tokio::test]
async fn the_qr_code_points_at_the_events_page_on_the_server_once_connected() {
    let s = server().await;
    let d = director();
    let day = practice_day(&s, &d).await;
    let path = format!("/events/{}/race-code", day.event);

    // Linked, but the Director has not heard from the server yet: there is no club to
    // put in the link.
    let (status, view) = ask(&d, true, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(view.get("link").is_none(), "{view}");
    let (status, answer) = ask(&d, true, Method::GET, &format!("{path}/qr"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert!(
        answer["message"]
            .as_str()
            .unwrap()
            .contains("not connected to a server")
    );

    // Connected: the server has said which club this is.
    let hello = client(&d).hello().await.unwrap();
    d.registry.link().report_server(hello.club, hello.version);
    let (_, view) = ask(&d, true, Method::GET, &path, None).await;
    assert_eq!(
        view["link"],
        json!(format!(
            "{}/#/here/club/{}/{}",
            s.url(),
            day.event,
            day.code
        ))
    );
    let request = Request::builder()
        .uri(format!("{path}/qr"))
        .header(header::HOST, "127.0.0.1:8080")
        .body(Body::empty())
        .unwrap();
    let mut request = request;
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:50000".parse::<SocketAddr>().unwrap(),
    ));
    let response = d.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let svg = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&svg).contains("<svg"));
}

// ── Left running ────────────────────────────────────────────────────────────

#[tokio::test]
async fn left_running_a_seat_taken_on_the_server_is_decided_and_shows_there_by_itself() {
    let s = server().await;
    let d = director();
    let mut day = practice_day(&s, &d).await;
    // The stream the tests above hold by hand is let go: the real task opens its own.
    drop(day.stream.take());
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    while s.state.asks.listening(&club()) {
        assert!(
            tokio::time::Instant::now() < end,
            "the server never noticed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let task = spawn(d.registry.clone());
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while !s.state.asks.listening(&club()) {
        assert!(
            tokio::time::Instant::now() < end,
            "the Director never listened"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Nothing here answers by hand. A wrong code first: it is refused by the Director.
    let wrong = if day.code == "AAAAAA" {
        "BBBBBB"
    } else {
        "AAAAAA"
    };
    let (status, answer) = take(&s, &day.pia, &day, 1, Some(wrong)).await.unwrap();
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{answer}");
    assert_eq!(holders_here(&d, &day.event).await, ["-", "-", "-"]);

    let began = std::time::Instant::now();
    let (status, answer) = take(&s, &day.pia, &day, 1, Some(&day.code)).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "it was heard at once, not on a slow poll: {:?}",
        began.elapsed()
    );
    assert_eq!(holders_here(&d, &day.event).await[0], day.ace);

    // The server's own copy shows the seat, pushed by the same task.
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while holders_on_server(&s, &day.event).await[0] != day.ace {
        assert!(
            tokio::time::Instant::now() < end,
            "the seat never showed on the server"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A lap flown at the race shows on the server too, as hers.
    cross(&d, &day.event, 1, 100).await;
    cross(&d, &day.event, 1, 112).await;
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let request = Request::builder()
            .uri(format!(
                "/api/clubs/club/events/{}/public/event/state",
                day.event
            ))
            .body(Body::empty())
            .unwrap();
        let response = router(s.state.clone()).oneshot(request).await.unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let state: Value = serde_json::from_slice(&bytes).unwrap();
        let stints = state["races"][0]["seats"][0]["stints"].as_array().unwrap();
        if stints.len() == 1 && stints[0]["laps"].as_array().unwrap().len() == 1 {
            assert_eq!(stints[0]["pilot"], json!(day.ace));
            break;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "her lap never showed on the server: {stints:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // She drops it, and that shows as well.
    let (status, answer) = as_member(
        &s,
        Some(&day.pia),
        &day.event,
        "drop",
        json!({"race": RACE}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{answer}");
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    while holders_on_server(&s, &day.event).await[0] != "-" {
        assert!(
            tokio::time::Instant::now() < end,
            "the drop never showed on the server"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();
}
