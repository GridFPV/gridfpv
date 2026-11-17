//! Pilots signing themselves up for heats (docs/primitives.md §11.5), through the app as
//! served: the director's commands, the pilot's own routes, and who may use which.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_server::events::EventRegistry;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const HERE: &str = "127.0.0.1:50000";
const QUALI: &str = "quali";

struct Rig {
    app: Router,
    dir: PathBuf,
    event: String,
    open: String,
    /// Pilot ids by callsign, in the order they were added.
    pilots: Vec<(String, String)>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

struct Answer {
    status: StatusCode,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", self.body))
    }
    fn message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
}

/// One request. `peer` decides who is asking; `cookie` is a pilot's sign-in.
async fn send(
    app: &Router,
    peer: &str,
    method: Method,
    uri: &str,
    cookie: Option<&str>,
    body: Option<Value>,
    extra: &[(&str, &str)],
) -> Answer {
    let local = peer.starts_with("127.");
    let mut request = Request::builder().method(method).uri(uri).header(
        header::HOST,
        if local {
            "127.0.0.1:8080"
        } else {
            "192.168.1.5:8081"
        },
    );
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, format!("gridfpv_pilot={cookie}"));
    }
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
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
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

/// A write by the director that must work.
async fn direct(app: &Router, method: Method, uri: &str, body: Value) -> Value {
    let answer = send(
        app,
        HERE,
        method.clone(),
        uri,
        None,
        Some(body.clone()),
        &[],
    )
    .await;
    assert!(
        answer.status.is_success(),
        "{method} {uri} {body} answered {}: {}",
        answer.status,
        answer.body
    );
    serde_json::from_str(&answer.body).unwrap_or(Value::Null)
}

fn seat(n: u32) -> Value {
    json!({"adapter": "bench", "seat": format!("seat-{n}")})
}
fn heat(n: u32) -> String {
    format!("{QUALI}/heat-{n}")
}

fn rules(over: Value) -> Value {
    let mut rules = json!({
        "seats": [seat(1), seat(2), seat(3)],
        "open_heats": 3,
        "open": true
    });
    for (key, value) in over.as_object().unwrap() {
        rules[key] = value.clone();
    }
    rules
}

impl Rig {
    fn pilot(&self, callsign: &str) -> &str {
        &self
            .pilots
            .iter()
            .find(|(name, _)| name == callsign)
            .unwrap_or_else(|| panic!("no pilot {callsign}"))
            .1
    }
    /// A sign-up command from the director's console.
    async fn command(&self, command: Value) -> Answer {
        send(
            &self.app,
            HERE,
            Method::POST,
            &format!("/events/{}/commands", self.event),
            None,
            Some(json!({ "Signup": command })),
            &[],
        )
        .await
    }
    async fn set(&self, over: Value) {
        let answer = self
            .command(json!({"Set": {"phase": QUALI, "rules": rules(over)}}))
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    }
    /// The snapshot as the director reads it.
    async fn state(&self) -> Value {
        send(
            &self.app,
            HERE,
            Method::GET,
            &format!("/events/{}/state", self.event),
            None,
            None,
            &[],
        )
        .await
        .json()
    }
    async fn signup(&self) -> Value {
        self.state().await["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|phase| phase["def"]["id"] == QUALI)
            .expect("the qualifying phase")["signup"]
            .clone()
    }
    /// Who is on each seat of each heat, by callsign: `"Ace,-,Bee"`.
    async fn spots(&self) -> Vec<String> {
        let signup = self.signup().await;
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
                        None => "-".to_string(),
                        Some(id) => self
                            .pilots
                            .iter()
                            .find(|(_, of)| of == id)
                            .map_or(id.to_string(), |(name, _)| name.clone()),
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect()
    }
    /// Sign a pilot in on their own phone and give back the sign-in.
    async fn sign_in(&self, callsign: &str) -> (String, String) {
        let access = send(
            &self.app,
            HERE,
            Method::GET,
            &format!("/events/{}/access", self.event),
            None,
            None,
            &[],
        )
        .await
        .json();
        let id = self.pilot(callsign);
        let code = access["pilots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pilot"] == id)
            .unwrap_or_else(|| panic!("{callsign} has a code"))["code"]
            .as_str()
            .unwrap()
            .to_string();
        let index = self
            .pilots
            .iter()
            .position(|(name, _)| name == callsign)
            .unwrap();
        let phone = format!("192.168.1.{}:50000", 100 + index);
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/public/pilot/session")
            .header(header::HOST, "192.168.1.5:8081")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "code": code }).to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(phone.parse::<SocketAddr>().unwrap()));
        let response = self.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let set = response.headers()[header::SET_COOKIE].to_str().unwrap();
        let session = set.split(';').next().unwrap().split_once('=').unwrap().1;
        (phone, session.to_string())
    }
    /// A pilot's own request, from their phone.
    async fn as_pilot(&self, who: &(String, String), route: &str, body: Value) -> Answer {
        send(
            &self.app,
            &who.0,
            Method::POST,
            &format!("/public/pilot/signup/{route}"),
            Some(&who.1),
            Some(body),
            &[],
        )
        .await
    }
    async fn take(&self, who: &(String, String), n: u32, s: u32) -> Answer {
        self.as_pilot(who, "take", json!({"heat": heat(n), "seat": seat(s)}))
            .await
    }
}

/// An open event with a qualifying phase for the class Open: Ace, Bee, Cat and Dan are in
/// it. Eve is in the director's list only.
async fn rig() -> Rig {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gridfpv-sign-up-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<title>console</title>").unwrap();
    let registry = EventRegistry::new(None).unwrap();
    let app = build_app(registry, &dir);
    let id = |value: Value| value["id"].as_str().unwrap().to_string();
    let mut pilots = Vec::new();
    for callsign in ["Ace", "Bee", "Cat", "Dan", "Eve"] {
        let made = direct(
            &app,
            Method::POST,
            "/pilots",
            json!({ "callsign": callsign }),
        )
        .await;
        pilots.push((callsign.to_string(), id(made)));
    }
    let open = id(direct(
        &app,
        Method::POST,
        "/classes",
        json!({"name": "Open", "source": "Custom"}),
    )
    .await);
    let event = id(direct(
        &app,
        Method::POST,
        "/events",
        json!({"name": "Friday Night"}),
    )
    .await);
    direct(&app, Method::PUT, "/active-event", json!({ "id": event })).await;
    let commands = format!("/events/{event}/commands");
    let in_event: Vec<&String> = pilots.iter().take(4).map(|(_, id)| id).collect();
    direct(
        &app,
        Method::POST,
        &commands,
        json!({"Record": {"RosterSet": {"class": open, "pilots": in_event}}}),
    )
    .await;
    direct(
        &app,
        Method::POST,
        &commands,
        json!({"Record": {"PhaseDefined": {"def": {
            "id": QUALI,
            "label": "Qualifying",
            "kind": {"Group": {"field": [{"Class": open}], "combine": "Stack"}},
            "format": {
                "name": "One race each",
                "race": {"end": "Open", "staging_micros": 0, "first_lap": "HoleShot"},
                "completion": {"SavedRaces": 1},
                "ranking": {"sort": ["LastFinish"]}
            }
        }}}}),
    )
    .await;
    Rig {
        app,
        dir,
        event,
        open,
        pilots,
    }
}

// ── The director turns it on, and everyone sees it ──────────────────────────

#[tokio::test]
async fn the_director_turns_sign_up_on_and_the_snapshot_shows_the_heats() {
    let rig = rig().await;
    assert!(rig.signup().await.is_null(), "off until it is turned on");
    rig.set(json!({"run_limit": 3, "held_limit": 2})).await;
    let signup = rig.signup().await;
    assert_eq!(signup["rules"]["open_heats"], 3);
    assert_eq!(signup["rules"]["run_limit"], 3);
    assert_eq!(signup["rules"]["held_limit"], 2);
    assert_eq!(signup["rules"]["rest_heats"], 0);
    assert_eq!(signup["rules"]["open"], true);
    let heats = signup["heats"].as_array().unwrap();
    assert_eq!(heats.len(), 3);
    for (index, heat) in heats.iter().enumerate() {
        assert_eq!(heat["number"], index + 1);
        assert_eq!(heat["seats"], json!([null, null, null]));
        assert_eq!(heat["state"], "Open");
        assert_eq!(heat["full"], false);
    }
    // Every pilot in the phase is listed with their runs. Eve is not in the event.
    let pilots = signup["pilots"].as_array().unwrap();
    assert_eq!(pilots.len(), 4);
    for pilot in pilots {
        assert_eq!(pilot["used"], 0);
        assert_eq!(pilot["held"], 0);
        assert_eq!(pilot["allowed"], 3);
    }
    // The heats are plain phases, named after the phase.
    let labels: Vec<String> = rig.state().await["phases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|phase| phase["def"]["parent"] == QUALI)
        .map(|phase| phase["def"]["label"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        labels,
        [
            "Qualifying Heat 1",
            "Qualifying Heat 2",
            "Qualifying Heat 3"
        ]
    );
}

#[tokio::test]
async fn a_visitor_sees_the_same_sign_up_sheet() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    assert_eq!(rig.take(&ace, 1, 2).await.status, StatusCode::OK);
    let public = send(
        &rig.app,
        "192.168.1.200:1",
        Method::GET,
        "/public/event/state",
        None,
        None,
        &[],
    )
    .await
    .json();
    let theirs = public["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["def"]["id"] == QUALI)
        .unwrap()["signup"]
        .clone();
    assert_eq!(theirs, rig.signup().await);
    assert_eq!(theirs["heats"][0]["seats"][1], rig.pilot("Ace"));
}

#[tokio::test]
async fn rules_that_cannot_work_are_refused_in_words() {
    let rig = rig().await;
    for (over, words) in [
        (json!({"seats": []}), "at least one seat"),
        (json!({"seats": [seat(1), seat(1)]}), "listed twice"),
        (json!({"open_heats": 0}), "at least one heat"),
        (json!({"open_heats": 21}), "too many heats"),
        (json!({"held_limit": 0}), "leave it out for no limit"),
        (json!({"run_limit": 0}), "leave it out for no limit"),
        (json!({"rest_heats": 99}), "too long"),
    ] {
        let answer = rig
            .command(json!({"Set": {"phase": QUALI, "rules": rules(over.clone())}}))
            .await;
        assert_eq!(answer.status, StatusCode::CONFLICT, "{over}");
        assert_eq!(answer.json()["code"], "Refused");
        assert!(
            answer.message().contains(words),
            "{over}: {}",
            answer.message()
        );
    }
    assert!(rig.signup().await.is_null());
    // A body that is not rules at all is a plain error, and still changes nothing.
    for bad in [
        json!({"Set": {"phase": QUALI, "rules": {"seats": "all"}}}),
        json!({"Set": {"phase": QUALI, "rules": {"seats": [seat(1)], "open_heats": -1}}}),
        json!({"Set": {"rules": rules(json!({}))}}),
        json!({"Nonsense": {}}),
    ] {
        let answer = rig.command(bad.clone()).await;
        assert!(answer.status.is_client_error(), "{bad}: {}", answer.status);
    }
    assert!(rig.signup().await.is_null());
}

// ── A pilot signs themselves up ─────────────────────────────────────────────

#[tokio::test]
async fn a_pilot_takes_a_seat_leaves_it_and_takes_the_next_free_one() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    let bee = rig.sign_in("Bee").await;

    let taken = rig.take(&ace, 2, 3).await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert!(taken.json()["cursor"].as_u64().is_some());
    assert_eq!(rig.spots().await, ["-,-,-", "-,-,Ace", "-,-,-"]);

    let left = rig.as_pilot(&ace, "leave", json!({"heat": heat(2)})).await;
    assert_eq!(left.status, StatusCode::OK, "{}", left.body);
    assert_eq!(rig.spots().await, ["-,-,-", "-,-,-", "-,-,-"]);

    // Next free on seat 1, twice: the first two heats.
    for who in [&ace, &bee] {
        let next = rig
            .as_pilot(who, "next", json!({"phase": QUALI, "seat": seat(1)}))
            .await;
        assert_eq!(next.status, StatusCode::OK, "{}", next.body);
    }
    assert_eq!(rig.spots().await, ["Ace,-,-", "Bee,-,-", "-,-,-"]);
}

#[tokio::test]
async fn the_pilot_who_acts_is_the_one_signed_in_whatever_the_body_says() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    // Ace tries to sign Bee up, and to act as the director.
    let answer = rig
        .as_pilot(
            &ace,
            "take",
            json!({
                "heat": heat(1),
                "seat": seat(1),
                "pilot": rig.pilot("Bee"),
                "by": "Director"
            }),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        rig.spots().await[0],
        "Ace,-,-",
        "it is Ace who is in, not Bee"
    );

    // Bee is in heat 2. Ace cannot take Bee's spot away by naming Bee.
    let bee = rig.sign_in("Bee").await;
    rig.take(&bee, 2, 1).await;
    let answer = rig
        .as_pilot(
            &ace,
            "leave",
            json!({"heat": heat(2), "pilot": rig.pilot("Bee")}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert!(
        answer.message().contains("Ace is not in Qualifying Heat 2"),
        "{}",
        answer.message()
    );
    assert_eq!(rig.spots().await[1], "Bee,-,-");
}

#[tokio::test]
async fn nobody_signs_up_without_being_signed_in_as_a_pilot() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let take = json!({"heat": heat(1), "seat": seat(1)});
    for (what, cookie) in [
        ("no sign-in", None),
        ("an empty sign-in", Some("")),
        ("a made-up sign-in", Some("not-a-sign-in")),
        ("a pilot's id as the sign-in", Some(rig.pilot("Ace"))),
    ] {
        for route in ["take", "next", "leave"] {
            let body = match route {
                "next" => json!({"phase": QUALI, "seat": seat(1)}),
                "leave" => json!({"heat": heat(1)}),
                _ => take.clone(),
            };
            let answer = send(
                &rig.app,
                "192.168.1.77:1",
                Method::POST,
                &format!("/public/pilot/signup/{route}"),
                cookie,
                Some(body),
                &[],
            )
            .await;
            assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{what}, {route}");
            assert_eq!(answer.message(), "Sign in with your code first.");
        }
    }
    // The director's own computer is not a pilot either: the director uses commands.
    let local = send(
        &rig.app,
        HERE,
        Method::POST,
        "/public/pilot/signup/take",
        None,
        Some(take),
        &[],
    )
    .await;
    assert_eq!(local.status, StatusCode::UNAUTHORIZED);
    assert_eq!(rig.spots().await, ["-,-,-", "-,-,-", "-,-,-"]);
}

#[tokio::test]
async fn a_pilot_cannot_reach_the_directors_commands() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    let commands = format!("/events/{}/commands", rig.event);
    for command in [
        json!({"Signup": {"Take": {"heat": heat(1), "seat": seat(1), "pilot": rig.pilot("Bee"), "by": "Director"}}}),
        json!({"Signup": {"SetRuns": {"phase": QUALI, "pilot": rig.pilot("Ace"), "extra": 50}}}),
        json!({"Signup": {"Set": {"phase": QUALI, "rules": rules(json!({"run_limit": 99}))}}}),
        json!({"Signup": {"Drop": {"heat": heat(1), "pilot": rig.pilot("Bee")}}}),
        json!({"Signup": {"CloseHeat": {"heat": heat(1), "closed": true}}}),
    ] {
        let answer = send(
            &rig.app,
            &ace.0,
            Method::POST,
            &commands,
            Some(&ace.1),
            Some(command.clone()),
            &[],
        )
        .await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{command}");
    }
    assert_eq!(rig.spots().await, ["-,-,-", "-,-,-", "-,-,-"]);
    assert_eq!(rig.signup().await["rules"].get("run_limit"), None);
}

#[tokio::test]
async fn a_refusal_names_things_by_their_names_and_changes_nothing() {
    let rig = rig().await;
    rig.set(json!({"run_limit": 1})).await;
    let ace = rig.sign_in("Ace").await;
    let bee = rig.sign_in("Bee").await;
    assert_eq!(rig.take(&ace, 1, 1).await.status, StatusCode::OK);
    let before = rig.state().await["cursor"].clone();

    let taken = rig.take(&bee, 1, 1).await;
    assert_eq!(taken.status, StatusCode::CONFLICT);
    assert_eq!(taken.json()["code"], "Refused");
    assert_eq!(
        taken.message(),
        "that seat in Qualifying Heat 1 is already taken"
    );

    let none_left = rig.take(&ace, 2, 1).await;
    assert_eq!(none_left.status, StatusCode::CONFLICT);
    assert_eq!(none_left.message(), "Ace has no runs left. The limit is 1");

    let twice = rig.take(&ace, 1, 2).await;
    assert_eq!(twice.status, StatusCode::CONFLICT);

    let not_a_seat = rig
        .as_pilot(
            &bee,
            "take",
            json!({"heat": heat(1), "seat": {"adapter": "bench", "seat": "seat-9"}}),
        )
        .await;
    assert_eq!(not_a_seat.status, StatusCode::CONFLICT);
    assert_eq!(
        not_a_seat.message(),
        "that seat is not one of the seats of Qualifying Heat 1"
    );

    let no_heat = rig
        .as_pilot(
            &bee,
            "take",
            json!({"heat": "quali/heat-99", "seat": seat(1)}),
        )
        .await;
    assert_eq!(no_heat.status, StatusCode::CONFLICT);

    let not_in = rig.as_pilot(&bee, "leave", json!({"heat": heat(1)})).await;
    assert_eq!(not_in.message(), "Bee is not in Qualifying Heat 1");

    // No raw id reached a pilot's screen.
    for answer in [&taken, &none_left, &twice, &not_a_seat, &not_in] {
        for (_, id) in &rig.pilots {
            assert!(!answer.body.contains(id.as_str()), "{}", answer.body);
        }
        assert!(!answer.body.contains("quali/heat-1"), "{}", answer.body);
        assert!(!answer.body.contains("PhaseId"), "{}", answer.body);
        assert!(!answer.body.contains("PilotId"), "{}", answer.body);
    }
    assert_eq!(rig.state().await["cursor"], before, "nothing was logged");
}

#[tokio::test]
async fn while_sign_up_is_closed_a_pilot_is_told_so() {
    let rig = rig().await;
    rig.set(json!({"open": false})).await;
    let ace = rig.sign_in("Ace").await;
    let answer = rig.take(&ace, 1, 1).await;
    assert_eq!(answer.status, StatusCode::CONFLICT);
    assert_eq!(answer.message(), "sign-up for Qualifying is not open");
    rig.set(json!({"open": true})).await;
    assert_eq!(rig.take(&ace, 1, 1).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_body_that_is_not_a_request_is_a_plain_error() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    for (route, body) in [
        ("take", json!({})),
        ("take", json!({"heat": heat(1)})),
        ("take", json!({"heat": heat(1), "seat": "seat-1"})),
        ("take", json!({"heat": 7, "seat": seat(1)})),
        ("take", json!([])),
        ("next", json!({"seat": seat(1)})),
        ("leave", json!({})),
        ("leave", json!({"heat": null})),
    ] {
        let answer = rig.as_pilot(&ace, route, body.clone()).await;
        assert!(
            answer.status.is_client_error(),
            "{route} {body}: {}",
            answer.status
        );
        assert_ne!(answer.status, StatusCode::UNAUTHORIZED);
    }
    // A route that does not exist is not a way in: the pilot's routes are the three above,
    // and anything else a visitor posts is refused before it is even looked for.
    for route in ["drop", "pull", "set", "close", "../../events"] {
        let answer = rig.as_pilot(&ace, route, json!({"heat": heat(1)})).await;
        assert!(
            answer.status == StatusCode::UNAUTHORIZED || answer.status == StatusCode::NOT_FOUND,
            "{route}: {}",
            answer.status
        );
    }
    assert_eq!(rig.spots().await, ["-,-,-", "-,-,-", "-,-,-"]);
}

#[tokio::test]
async fn another_website_cannot_sign_a_pilot_up_or_out_of_a_heat() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    rig.take(&ace, 1, 1).await;
    for (route, body) in [
        ("take", json!({"heat": heat(2), "seat": seat(1)})),
        ("leave", json!({"heat": heat(1)})),
    ] {
        let answer = send(
            &rig.app,
            &ace.0,
            Method::POST,
            &format!("/public/pilot/signup/{route}"),
            Some(&ace.1),
            Some(body),
            &[("origin", "http://evil.example")],
        )
        .await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN, "{route}");
    }
    assert_eq!(rig.spots().await, ["Ace,-,-", "-,-,-", "-,-,-"]);
}

#[tokio::test]
async fn a_pilot_who_is_signed_out_or_taken_out_of_the_event_cannot_act() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    assert_eq!(rig.take(&ace, 1, 1).await.status, StatusCode::OK);
    // The director signs Ace's phone out.
    send(
        &rig.app,
        HERE,
        Method::DELETE,
        &format!("/events/{}/access/{}/session", rig.event, rig.pilot("Ace")),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(rig.take(&ace, 2, 1).await.status, StatusCode::UNAUTHORIZED);
    // Bee signs in, then is taken out of the class.
    let bee = rig.sign_in("Bee").await;
    direct(
        &rig.app,
        Method::POST,
        &format!("/events/{}/commands", rig.event),
        json!({"Record": {"RosterSet": {"class": rig.open, "pilots": [rig.pilot("Ace")]}}}),
    )
    .await;
    assert_eq!(rig.take(&bee, 2, 1).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(rig.spots().await, ["Ace,-,-", "-,-,-", "-,-,-"]);
}

#[tokio::test]
async fn a_sign_in_from_another_event_does_not_sign_a_pilot_up_in_this_one() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    // The director opens another event that has no sign-up at all.
    let other = direct(&rig.app, Method::POST, "/events", json!({"name": "Sunday"})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    direct(
        &rig.app,
        Method::PUT,
        "/active-event",
        json!({ "id": other }),
    )
    .await;
    let answer = rig.take(&ace, 1, 1).await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{}", answer.body);
    // Back at Friday, the sign-in works again and Friday's sheet was never touched.
    direct(
        &rig.app,
        Method::PUT,
        "/active-event",
        json!({ "id": rig.event }),
    )
    .await;
    assert_eq!(rig.spots().await, ["-,-,-", "-,-,-", "-,-,-"]);
    assert_eq!(rig.take(&ace, 1, 1).await.status, StatusCode::OK);
}

#[tokio::test]
async fn with_no_event_open_it_says_so() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    send(
        &rig.app,
        HERE,
        Method::DELETE,
        &format!("/events/{}", rig.event),
        None,
        None,
        &[],
    )
    .await;
    let answer = rig.take(&ace, 1, 1).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.message(), "No event is open.");
}

// ── Many at once ────────────────────────────────────────────────────────────

/// A pilot's request, built to be sent from a task of its own.
fn request(who: &(String, String), route: &str, body: Value) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/public/pilot/signup/{route}"))
        .header(header::HOST, "192.168.1.5:8081")
        .header(header::COOKIE, format!("gridfpv_pilot={}", who.1))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(who.0.parse::<SocketAddr>().unwrap()));
    request
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_four_pilots_grab_the_same_seat_at_once_exactly_one_gets_it() {
    for round in 0..10 {
        let rig = rig().await;
        rig.set(json!({})).await;
        let mut phones = Vec::new();
        for callsign in ["Ace", "Bee", "Cat", "Dan"] {
            phones.push(rig.sign_in(callsign).await);
        }
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(phones.len()));
        let mut grabbing = tokio::task::JoinSet::new();
        for phone in &phones {
            let app = rig.app.clone();
            let barrier = barrier.clone();
            let request = request(phone, "take", json!({"heat": heat(1), "seat": seat(2)}));
            grabbing.spawn(async move {
                barrier.wait().await;
                app.oneshot(request).await.unwrap().status()
            });
        }
        let mut got = 0;
        let mut refused = 0;
        while let Some(status) = grabbing.join_next().await {
            match status.unwrap() {
                StatusCode::OK => got += 1,
                StatusCode::CONFLICT => refused += 1,
                other => panic!("round {round}: {other}"),
            }
        }
        assert_eq!((got, refused), (1, 3), "round {round}");
        let spots = rig.spots().await;
        let first: Vec<&str> = spots[0].split(',').collect();
        assert_eq!(first[0], "-");
        assert_ne!(first[1], "-", "someone has the seat");
        assert_eq!(first[2], "-");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_pilot_tapping_every_seat_of_a_heat_at_once_ends_up_on_one() {
    for round in 0..10 {
        let rig = rig().await;
        rig.set(json!({})).await;
        let ace = rig.sign_in("Ace").await;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let mut tapping = tokio::task::JoinSet::new();
        for s in 1..=3 {
            let app = rig.app.clone();
            let barrier = barrier.clone();
            let request = request(&ace, "take", json!({"heat": heat(1), "seat": seat(s)}));
            tapping.spawn(async move {
                barrier.wait().await;
                app.oneshot(request).await.unwrap().status()
            });
        }
        let mut got = 0;
        while let Some(status) = tapping.join_next().await {
            if status.unwrap() == StatusCode::OK {
                got += 1;
            }
        }
        assert_eq!(got, 1, "round {round}");
        let spots = rig.spots().await;
        assert_eq!(
            spots[0].split(',').filter(|on| *on == "Ace").count(),
            1,
            "round {round}: {spots:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rush_for_spots_never_breaks_a_limit_or_doubles_a_seat() {
    // Four pilots, each allowed two runs, all asking for the next free spot on every seat,
    // over and over, at the same time.
    let rig = rig().await;
    rig.set(json!({"run_limit": 2, "held_limit": 2})).await;
    let mut phones = Vec::new();
    for callsign in ["Ace", "Bee", "Cat", "Dan"] {
        phones.push(rig.sign_in(callsign).await);
    }
    let mut rush = tokio::task::JoinSet::new();
    for phone in &phones {
        for s in 1..=3 {
            for _ in 0..4 {
                let app = rig.app.clone();
                let request = request(phone, "next", json!({"phase": QUALI, "seat": seat(s)}));
                rush.spawn(async move { app.oneshot(request).await.unwrap().status() });
            }
        }
    }
    let mut taken = 0;
    while let Some(status) = rush.join_next().await {
        let status = status.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "{status}"
        );
        if status == StatusCode::OK {
            taken += 1;
        }
    }
    assert_eq!(taken, 8, "four pilots, two runs each, and not one more");
    let signup = rig.signup().await;
    for pilot in signup["pilots"].as_array().unwrap() {
        assert_eq!(pilot["held"], 2, "{pilot}");
    }
    // No pilot is twice in a heat, and the heats that are not full never exceed three.
    let spots = rig.spots().await;
    for heat in &spots {
        let on: Vec<&str> = heat.split(',').filter(|on| *on != "-").collect();
        let unique: std::collections::BTreeSet<&&str> = on.iter().collect();
        assert_eq!(unique.len(), on.len(), "{heat}");
    }
    let not_full = spots.iter().filter(|heat| heat.contains('-')).count();
    assert_eq!(not_full, 3, "{spots:?}");
}

// ── The director's side, over the wire ──────────────────────────────────────

#[tokio::test]
async fn the_director_adds_drops_pulls_closes_and_gives_runs() {
    let rig = rig().await;
    rig.set(json!({"run_limit": 1})).await;
    let ok = |answer: Answer| assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let take = |n: u32, s: u32, who: &str| json!({"Take": {"heat": heat(n), "seat": seat(s), "pilot": rig.pilot(who), "by": "Director"}});
    ok(rig.command(take(1, 1, "Ace")).await);
    ok(rig.command(take(3, 2, "Bee")).await);
    // The director is not held to the run limit.
    ok(rig.command(take(2, 1, "Ace")).await);
    assert_eq!(rig.spots().await, ["Ace,-,-", "Ace,-,-", "-,Bee,-"]);

    // Pull Bee forward onto the same seat.
    ok(rig
        .command(json!({"Pull": {"pilot": rig.pilot("Bee"), "from": heat(3), "to": heat(1)}}))
        .await);
    assert_eq!(rig.spots().await, ["Ace,Bee,-", "Ace,-,-", "-,-,-"]);
    let back = rig
        .command(json!({"Pull": {"pilot": rig.pilot("Bee"), "from": heat(1), "to": heat(3)}}))
        .await;
    assert_eq!(back.status, StatusCode::CONFLICT);
    assert_eq!(
        back.message(),
        "a pilot can only be pulled forward, into an earlier heat"
    );

    // Drop Ace from heat 2 and count it.
    ok(rig
        .command(json!({"Drop": {"heat": heat(2), "pilot": rig.pilot("Ace"), "counted": true}}))
        .await);
    let runs = |signup: &Value, who: &str| {
        signup["pilots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|pilot| pilot["pilot"] == rig.pilot(who))
            .unwrap()
            .clone()
    };
    let signup = rig.signup().await;
    assert_eq!(runs(&signup, "Ace")["used"], 1);
    assert_eq!(runs(&signup, "Ace")["held"], 1);

    // One more run for Ace alone.
    ok(rig
        .command(json!({"SetRuns": {"phase": QUALI, "pilot": rig.pilot("Ace"), "extra": 2}}))
        .await);
    let signup = rig.signup().await;
    assert_eq!(runs(&signup, "Ace")["allowed"], 3);
    assert_eq!(runs(&signup, "Ace")["extra"], 2);
    assert_eq!(runs(&signup, "Bee")["allowed"], 1);

    // Close heat 1: a pilot cannot join it, the director still can.
    ok(rig
        .command(json!({"CloseHeat": {"heat": heat(1), "closed": true}}))
        .await);
    assert_eq!(rig.signup().await["heats"][0]["state"], "Closed");
    let cat = rig.sign_in("Cat").await;
    let shut = rig.take(&cat, 1, 3).await;
    assert_eq!(shut.status, StatusCode::CONFLICT);
    assert_eq!(
        shut.message(),
        "Qualifying Heat 1 is closed to changes from a phone. Ask the director"
    );
    ok(rig.command(take(1, 3, "Cat")).await);
    assert_eq!(rig.spots().await[0], "Ace,Bee,Cat");

    // Turn sign-up off: the sheet goes, the heats and lineups stay.
    ok(rig.command(json!({"Set": {"phase": QUALI}})).await);
    assert!(rig.signup().await.is_null());
    let heat_one = rig.state().await["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["def"]["id"] == heat(1))
        .unwrap()
        .clone();
    assert_eq!(heat_one["lineup"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_whole_round_heats_fill_fly_and_count_as_runs() {
    let rig = rig().await;
    rig.set(json!({"run_limit": 2})).await;
    let mut phones = Vec::new();
    for callsign in ["Ace", "Bee", "Cat", "Dan"] {
        phones.push(rig.sign_in(callsign).await);
    }
    // Three fill heat 1: heat 4 opens. Dan goes to heat 2.
    for (index, phone) in phones.iter().take(3).enumerate() {
        let answer = rig.take(phone, 1, u32::try_from(index).unwrap() + 1).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    }
    assert_eq!(rig.take(&phones[3], 2, 1).await.status, StatusCode::OK);
    assert_eq!(
        rig.spots().await,
        ["Ace,Bee,Cat", "Dan,-,-", "-,-,-", "-,-,-"]
    );
    // The run queue offers the heats with pilots, in number order.
    assert_eq!(rig.state().await["queue"], json!([heat(1), heat(2)]));

    // Heat 1 is started: nobody can leave it now, from a phone.
    let commands = format!("/events/{}/commands", rig.event);
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"StartRace": {"heat": heat(1)}}),
    )
    .await;
    let leave = rig
        .as_pilot(&phones[0], "leave", json!({"heat": heat(1)}))
        .await;
    assert_eq!(leave.status, StatusCode::CONFLICT);
    assert_eq!(rig.signup().await["heats"][0]["state"], "Racing");
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"StopRace": {"race": format!("{}#1", heat(1))}}),
    )
    .await;
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"SaveRace": {"race": format!("{}#1", heat(1))}}),
    )
    .await;
    let signup = rig.signup().await;
    assert_eq!(signup["heats"][0]["state"], "Flown");
    let ace = signup["pilots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pilot| pilot["pilot"] == rig.pilot("Ace"))
        .unwrap();
    assert_eq!(ace["used"], 1);
    assert_eq!(ace["held"], 0);
    // One run left: Ace takes one more spot, and then has none.
    assert_eq!(rig.take(&phones[0], 2, 2).await.status, StatusCode::OK);
    let none = rig.take(&phones[0], 3, 1).await;
    assert_eq!(none.status, StatusCode::CONFLICT);
    assert_eq!(none.message(), "Ace has no runs left. The limit is 2");
    // The flown heat ranks its pilots like any heat.
    let heat_one = rig.state().await["phases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|phase| phase["def"]["id"] == heat(1))
        .unwrap()
        .clone();
    assert_eq!(heat_one["complete"], true);
    assert_eq!(heat_one["ranking"].as_array().unwrap().len(), 3);
}

// ── The heat on the timer ────────────────────────────────────────────────────

impl Rig {
    /// Any command from the director's console.
    async fn direct_command(&self, command: Value) -> Answer {
        send(
            &self.app,
            HERE,
            Method::POST,
            &format!("/events/{}/commands", self.event),
            None,
            Some(command),
            &[],
        )
        .await
    }
    async fn put_on_timer(&self, n: u32) -> Answer {
        self.direct_command(json!({"PutOnTimer": {"heat": heat(n)}}))
            .await
    }
    async fn states(&self) -> Vec<String> {
        self.signup().await["heats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|heat| heat["state"].as_str().unwrap().to_string())
            .collect()
    }
    async fn visitor_state(&self) -> Value {
        send(
            &self.app,
            "192.168.1.77:50000",
            Method::GET,
            "/public/event/state",
            None,
            None,
            &[],
        )
        .await
        .json()
    }
}

#[tokio::test]
async fn the_director_puts_a_heat_on_the_timer_and_everyone_is_told_which() {
    let rig = rig().await;
    rig.set(json!({})).await;
    // Nothing is on the timer until the director says so: the field is not there at all.
    assert!(rig.state().await.get("on_timer").is_none());
    assert!(rig.visitor_state().await.get("on_timer").is_none());
    let answer = rig.put_on_timer(2).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(rig.state().await["on_timer"], heat(2));
    // A spectator's phone is told the same heat.
    assert_eq!(rig.visitor_state().await["on_timer"], heat(2));
    // Putting it there again changes nothing and is not an error.
    let before = rig.state().await["cursor"].clone();
    assert_eq!(rig.put_on_timer(2).await.status, StatusCode::OK);
    assert_eq!(rig.state().await["cursor"], before);
}

#[tokio::test]
async fn a_sign_up_heat_on_the_timer_is_closed_to_pilots_and_the_next_one_opens() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    let bee = rig.sign_in("Bee").await;
    assert_eq!(rig.take(&ace, 1, 1).await.status, StatusCode::OK);
    assert_eq!(rig.states().await, ["Open", "Open", "Open"]);
    assert_eq!(rig.put_on_timer(1).await.status, StatusCode::OK);
    // Closed to pilots, and a fourth heat so there are still three with room.
    assert_eq!(rig.states().await, ["Closed", "Open", "Open", "Open"]);
    let refused = rig.take(&bee, 1, 2).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(
        refused.message(),
        "Qualifying Heat 1 is closed to changes from a phone. Ask the director"
    );
    let stuck = rig.as_pilot(&ace, "leave", json!({"heat": heat(1)})).await;
    assert_eq!(stuck.status, StatusCode::CONFLICT);
    // The director can still add a pilot, up to the start.
    let added = rig
        .command(json!({"Take": {"heat": heat(1), "seat": seat(2), "pilot": rig.pilot("Bee"), "by": "Director"}}))
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.body);
    assert_eq!(rig.spots().await[0], "Ace,Bee,-");
    // The director moves on without flying it: heat 1 is open to pilots again.
    assert_eq!(rig.put_on_timer(2).await.status, StatusCode::OK);
    assert_eq!(rig.states().await[..2], ["Open", "Closed"]);
    assert_eq!(
        rig.as_pilot(&ace, "leave", json!({"heat": heat(1)}))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn starting_a_race_puts_its_heat_on_the_timer_and_no_other_can_go_there_until_it_is_decided()
{
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    rig.take(&ace, 2, 1).await;
    rig.put_on_timer(1).await;
    let started = rig
        .direct_command(json!({"StartRace": {"heat": heat(2)}}))
        .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    assert_eq!(rig.state().await["on_timer"], heat(2));
    let refused = rig.put_on_timer(3).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(
        refused.message(),
        "Qualifying Heat 2 has a race that is not saved or thrown out yet. Decide it first"
    );
    assert_eq!(rig.state().await["on_timer"], heat(2));
    let race = format!("{}#1", heat(2));
    rig.direct_command(json!({"StopRace": {"race": race}}))
        .await;
    rig.direct_command(json!({"SaveRace": {"race": race}}))
        .await;
    // Flown, it is still the heat on the timer until the director puts another there.
    assert_eq!(rig.state().await["on_timer"], heat(2));
    assert_eq!(rig.put_on_timer(3).await.status, StatusCode::OK);
    assert_eq!(rig.state().await["on_timer"], heat(3));
}

#[tokio::test]
async fn only_the_director_puts_a_heat_on_the_timer() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.sign_in("Ace").await;
    let commands = format!("/events/{}/commands", rig.event);
    for cookie in [None, Some(ace.1.as_str())] {
        let answer = send(
            &rig.app,
            &ace.0,
            Method::POST,
            &commands,
            cookie,
            Some(json!({"PutOnTimer": {"heat": heat(1)}})),
            &[],
        )
        .await;
        assert!(
            answer.status == StatusCode::UNAUTHORIZED || answer.status == StatusCode::FORBIDDEN,
            "{}",
            answer.status
        );
    }
    // Nor through the pilot's own routes, whatever the body says.
    let sneaky = rig
        .as_pilot(
            &ace,
            "take",
            json!({"PutOnTimer": {"heat": heat(1)}, "heat": heat(1), "seat": seat(1)}),
        )
        .await;
    assert_eq!(sneaky.status, StatusCode::OK);
    assert!(rig.state().await.get("on_timer").is_none());
    // The fact itself cannot be written into the log by a console either.
    let raw = rig
        .direct_command(json!({"Record": {"HeatOnTimer": {"heat": heat(1)}}}))
        .await;
    assert_eq!(raw.status, StatusCode::CONFLICT);
    assert!(rig.state().await.get("on_timer").is_none());
}

#[tokio::test]
async fn a_heat_that_is_not_there_or_is_not_a_heat_cannot_go_on_the_timer() {
    let rig = rig().await;
    rig.set(json!({})).await;
    for (target, words) in [(json!("nope"), "no phase"), (json!(QUALI), "Qualifying")] {
        let answer = rig
            .direct_command(json!({"PutOnTimer": {"heat": target}}))
            .await;
        assert_eq!(
            answer.status,
            StatusCode::CONFLICT,
            "{target}: {}",
            answer.body
        );
        assert!(
            answer.message().contains(words) || answer.message().contains("heat"),
            "{}",
            answer.message()
        );
    }
    for bad in [
        json!({"PutOnTimer": {}}),
        json!({"PutOnTimer": {"heat": 5}}),
        json!({"PutOnTimer": null}),
    ] {
        let answer = rig.direct_command(bad.clone()).await;
        assert!(answer.status.is_client_error(), "{bad}: {}", answer.status);
    }
    assert!(rig.state().await.get("on_timer").is_none());
}

#[tokio::test]
async fn the_heat_on_the_timer_is_kept_when_grid_is_closed_and_opened() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let data = std::env::temp_dir().join(format!("gridfpv-on-timer-{nanos}"));
    let assets = data.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<title>console</title>").unwrap();
    let event;
    {
        let app = build_app(EventRegistry::new(Some(data.clone())).unwrap(), &assets);
        event = direct(&app, Method::POST, "/events", json!({"name": "Kept"})).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        direct(&app, Method::PUT, "/active-event", json!({ "id": event })).await;
        let commands = format!("/events/{event}/commands");
        direct(
            &app,
            Method::POST,
            &commands,
            json!({"Record": {"PhaseDefined": {"def": {
                "id": "final", "label": "Final", "kind": {"Heat": {"lineup": []}}
            }}}}),
        )
        .await;
        direct(
            &app,
            Method::POST,
            &commands,
            json!({"PutOnTimer": {"heat": "final"}}),
        )
        .await;
    }
    let app = build_app(EventRegistry::new(Some(data.clone())).unwrap(), &assets);
    let state = send(
        &app,
        HERE,
        Method::GET,
        &format!("/events/{event}/state"),
        None,
        None,
        &[],
    )
    .await
    .json();
    assert_eq!(state["on_timer"], "final");
    std::fs::remove_dir_all(data).ok();
}
