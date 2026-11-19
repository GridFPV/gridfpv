//! Check-in (docs/primitives.md §11.9): practice run as one long heat with open seats. A
//! pilot takes a seat part-way through, and the laps on it are theirs until someone else
//! takes it or they drop it. Through the app as served: the director's commands, the
//! pilot's own routes, and who may use which.

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
const PRACTICE: &str = "practice";

struct Rig {
    app: Router,
    dir: PathBuf,
    event: String,
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

impl Rig {
    fn pilot(&self, callsign: &str) -> &str {
        &self
            .pilots
            .iter()
            .find(|(name, _)| name == callsign)
            .unwrap_or_else(|| panic!("no pilot {callsign}"))
            .1
    }
    fn callsign(&self, id: &str) -> String {
        self.pilots
            .iter()
            .find(|(_, of)| of == id)
            .map_or(id.to_string(), |(name, _)| name.clone())
    }
    /// One command from the director's console.
    async fn command(&self, command: Value) -> Answer {
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
    /// The practice race, which is started by the rig.
    fn race(&self) -> String {
        format!("{PRACTICE}#1")
    }
    /// A crossing on a seat, `at` seconds on the bench clock.
    async fn cross(&self, n: u32, at: i64) {
        let mut body = seat(n);
        body["at"] = json!(at * 1_000_000);
        direct(
            &self.app,
            Method::POST,
            &format!("/events/{}/passes", self.event),
            body,
        )
        .await;
    }
    /// Each seat as the snapshot shows it: who has it now, then each stint as
    /// `Callsign:lap,lap` with a `*` while it is still open.
    async fn seats(&self, state: &Value) -> Vec<String> {
        state["races"][0]["seats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|seat| {
                let holder = seat["holder"]
                    .as_str()
                    .map_or("-".to_string(), |id| self.callsign(id));
                let stints: Vec<String> = seat["stints"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|stint| {
                        let laps: Vec<String> = stint["laps"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|lap| (lap["micros"].as_i64().unwrap() / 1_000_000).to_string())
                            .collect();
                        format!(
                            "{}{}:{}",
                            self.callsign(stint["pilot"].as_str().unwrap()),
                            if stint["open"] == true { "*" } else { "" },
                            laps.join(",")
                        )
                    })
                    .collect();
                format!("{holder} [{}]", stints.join(" "))
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
    async fn take(&self, who: &(String, String), n: u32) -> Answer {
        let mut body = seat(n);
        body["race"] = json!(self.race());
        send(
            &self.app,
            &who.0,
            Method::POST,
            "/public/pilot/checkin/take",
            Some(&who.1),
            Some(body),
            &[],
        )
        .await
    }
    async fn drop_seat(&self, who: &(String, String)) -> Answer {
        send(
            &self.app,
            &who.0,
            Method::POST,
            "/public/pilot/checkin/drop",
            Some(&who.1),
            Some(json!({ "race": self.race() })),
            &[],
        )
        .await
    }
}

/// An open event with one practice heat of two open seats and one seat with Cat named on
/// it, and its race running. Ace, Bee, Cat and Dan are in the event. Eve is in the
/// director's list only.
async fn rig() -> Rig {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gridfpv-check-in-{nanos}"));
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
    let cat = &pilots[2].1;
    let mut seats = vec![seat(1), seat(2), seat(3)];
    seats[0]["source"] = json!("Open");
    seats[1]["source"] = json!("Open");
    seats[2]["source"] = json!({ "Pilot": cat });
    direct(
        &app,
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
        &app,
        Method::POST,
        &commands,
        json!({"StartRace": {"heat": PRACTICE}}),
    )
    .await;
    Rig {
        app,
        dir,
        event,
        pilots,
    }
}

#[tokio::test]
async fn a_pilot_takes_a_seat_from_their_phone_and_the_laps_from_then_on_are_theirs() {
    let rig = rig().await;
    let ace = rig.sign_in("Ace").await;
    // Someone is lapping on seat 1 with no name.
    rig.cross(1, 0).await;
    rig.cross(1, 20).await;
    assert_eq!(rig.take(&ace, 1).await.status, StatusCode::OK);
    // Her first crossing starts her first lap; the lap under way was not hers.
    rig.cross(1, 45).await;
    rig.cross(1, 60).await;
    rig.cross(1, 72).await;
    let state = rig.state().await;
    assert_eq!(
        rig.seats(&state).await,
        ["Ace [Ace*:15,12]", "- []", "- []"]
    );
    // The seat itself still shows every lap flown on it.
    assert_eq!(
        state["races"][0]["seats"][0]["laps"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    // A visitor sees who is on the seat and their laps too.
    let public = send(
        &rig.app,
        "192.168.1.200:1",
        Method::GET,
        "/public/event/state",
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(public.status, StatusCode::OK);
    assert_eq!(
        rig.seats(&public.json()).await,
        ["Ace [Ace*:15,12]", "- []", "- []"]
    );
}

#[tokio::test]
async fn a_seat_changes_hands_when_someone_else_takes_it_and_a_pilot_can_drop_their_own() {
    let rig = rig().await;
    let ace = rig.sign_in("Ace").await;
    let bee = rig.sign_in("Bee").await;
    assert_eq!(rig.take(&ace, 1).await.status, StatusCode::OK);
    rig.cross(1, 0).await;
    rig.cross(1, 10).await;
    // Bee takes the seat from Ace. Ace keeps the lap she flew.
    assert_eq!(rig.take(&bee, 1).await.status, StatusCode::OK);
    rig.cross(1, 30).await;
    rig.cross(1, 41).await;
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["Bee [Ace:10 Bee*:11]", "- []", "- []"]
    );
    // Ace has no seat now, so she has nothing to drop, and Bee's seat is untouched.
    let nothing = rig.drop_seat(&ace).await;
    assert_eq!(nothing.status, StatusCode::CONFLICT);
    assert_eq!(nothing.message(), "You do not have a seat to drop.");
    assert_eq!(rig.drop_seat(&bee).await.status, StatusCode::OK);
    rig.cross(1, 55).await;
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["- [Ace:10 Bee:11]", "- []", "- []"]
    );
    // A pilot who takes a second seat leaves the first.
    assert_eq!(rig.take(&ace, 1).await.status, StatusCode::OK);
    assert_eq!(rig.take(&ace, 2).await.status, StatusCode::OK);
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["- [Ace:10 Bee:11 Ace:]", "Ace [Ace*:]", "- []"]
    );
}

#[tokio::test]
async fn the_director_can_put_a_pilot_on_a_seat_and_drop_them() {
    let rig = rig().await;
    let take = |pilot: &str, n: u32| {
        let mut fact = seat(n);
        fact["race"] = json!(rig.race());
        fact["pilot"] = json!(rig.pilot(pilot));
        json!({"Record": {"SeatTaken": fact}})
    };
    assert_eq!(rig.command(take("Dan", 2)).await.status, StatusCode::OK);
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["- []", "Dan [Dan*:]", "- []"]
    );
    let mut fact = seat(2);
    fact["race"] = json!(rig.race());
    let dropped = rig.command(json!({"Record": {"SeatDropped": fact}})).await;
    assert_eq!(dropped.status, StatusCode::OK);
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["- []", "- [Dan:]", "- []"]
    );
}

#[tokio::test]
async fn a_refusal_is_a_sentence_with_names_and_changes_nothing() {
    let rig = rig().await;
    let ace = rig.sign_in("Ace").await;
    let cat = rig.sign_in("Cat").await;
    // Seat 3 has Cat named on it.
    let named = rig.take(&ace, 3).await;
    assert_eq!(named.status, StatusCode::CONFLICT);
    assert_eq!(
        named.message(),
        "that seat in race 1 of Open Practice has a pilot named on it, so it cannot be taken"
    );
    // Cat has her own seat in the race.
    let own = rig.take(&cat, 1).await;
    assert_eq!(own.status, StatusCode::CONFLICT);
    assert_eq!(
        own.message(),
        "pilot Cat already has a seat of their own in race 1 of Open Practice"
    );
    assert_eq!(rig.take(&ace, 1).await.status, StatusCode::OK);
    let twice = rig.take(&ace, 1).await;
    assert_eq!(twice.status, StatusCode::CONFLICT);
    assert_eq!(
        twice.message(),
        "pilot Ace already has that seat in race 1 of Open Practice"
    );
    for message in [named.message(), own.message(), twice.message()] {
        for (_, id) in &rig.pilots {
            assert!(!message.contains(id.as_str()), "{message}");
        }
        assert!(!message.contains(PRACTICE), "{message}");
    }
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["Ace [Ace*:]", "- []", "- []"]
    );
}

#[tokio::test]
async fn only_a_signed_in_pilot_can_take_or_drop_and_only_for_themselves() {
    let rig = rig().await;
    let mut take = seat(1);
    take["race"] = json!(rig.race());
    // A body that names another pilot is ignored: the pilot is the sign-in, nothing else.
    take["pilot"] = json!(rig.pilot("Bee"));
    for (what, cookie) in [
        ("no sign-in", None),
        ("a made-up sign-in", Some("not-a-sign-in")),
        ("a pilot's id as the sign-in", Some(rig.pilot("Ace"))),
    ] {
        for (route, body) in [
            ("take", take.clone()),
            ("drop", json!({ "race": rig.race() })),
        ] {
            let answer = send(
                &rig.app,
                "192.168.1.77:1",
                Method::POST,
                &format!("/public/pilot/checkin/{route}"),
                cookie,
                Some(body),
                &[],
            )
            .await;
            assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{what}, {route}");
            assert_eq!(answer.message(), "Sign in with your code first.");
        }
    }
    let ace = rig.sign_in("Ace").await;
    let answer = send(
        &rig.app,
        &ace.0,
        Method::POST,
        "/public/pilot/checkin/take",
        Some(&ace.1),
        Some(take),
        &[],
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    // Ace took it, whatever the body said.
    assert_eq!(
        rig.seats(&rig.state().await).await,
        ["Ace [Ace*:]", "- []", "- []"]
    );
    // A pilot's sign-in does not open the director's command door.
    let mut fact = seat(2);
    fact["race"] = json!(rig.race());
    fact["pilot"] = json!(rig.pilot("Bee"));
    let answer = send(
        &rig.app,
        &ace.0,
        Method::POST,
        &format!("/events/{}/commands", rig.event),
        Some(&ace.1),
        Some(json!({"Record": {"SeatTaken": fact}})),
        &[],
    )
    .await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_checked_in_pilot_is_ranked_on_their_own_laps_once_the_race_is_saved() {
    let rig = rig().await;
    let commands = format!("/events/{}/commands", rig.event);
    // Rank this practice on the fastest lap.
    let mut state = rig.state().await;
    let mut def = state["phases"][0]["def"].take();
    def["format"]["ranking"] = json!({"sort": ["BestLap"]});
    let ace = rig.sign_in("Ace").await;
    let bee = rig.sign_in("Bee").await;
    // A fast lap on seat 1 before anyone took it: nobody's.
    rig.cross(1, 0).await;
    rig.cross(1, 5).await;
    assert_eq!(rig.take(&ace, 1).await.status, StatusCode::OK);
    rig.cross(1, 20).await;
    rig.cross(1, 35).await;
    rig.cross(1, 47).await;
    assert_eq!(rig.take(&bee, 2).await.status, StatusCode::OK);
    rig.cross(2, 50).await;
    rig.cross(2, 61).await;
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"StopRace": {"race": rig.race()}}),
    )
    .await;
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"SaveRace": {"race": rig.race()}}),
    )
    .await;
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"Record": {"PhaseDefined": {"def": def}}}),
    )
    .await;
    let state = rig.state().await;
    let ranking: Vec<(String, i64)> = state["phases"][0]["ranking"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|placed| placed["flew"] == true)
        .map(|placed| {
            (
                rig.callsign(placed["pilot"].as_str().unwrap()),
                placed["best_lap_micros"].as_i64().unwrap() / 1_000_000,
            )
        })
        .collect();
    // Bee's 11 beats Ace's 12. The 5 second lap flown before Ace took the seat is nobody's.
    assert_eq!(ranking, [("Bee".to_string(), 11), ("Ace".to_string(), 12)]);
}
