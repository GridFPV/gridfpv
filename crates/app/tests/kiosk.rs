//! The kiosk (docs/primitives.md §11.4), through the app as served: a shared tablet where
//! a pilot proves who they are with their code, acts, and walks away.
//!
//! What must hold: the code is checked like a sign-in and wrong ones are counted; the
//! kiosk never signs the pilot's own phone out; the pass it gets works only in the header,
//! only for that pilot and event, and stops when the pilot is done, gets a new code, is
//! signed out by the director, or leaves the event.
#![allow(dead_code)]

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

const KIOSK: &str = "192.168.1.50:50000";
const PASS: &str = "x-gridfpv-kiosk";

impl Rig {
    async fn code(&self, callsign: &str) -> String {
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
        access["pilots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pilot"] == id)
            .unwrap_or_else(|| panic!("{callsign} has a code"))["code"]
            .as_str()
            .unwrap()
            .to_string()
    }
    async fn signed_in(&self, callsign: &str) -> bool {
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
        access["pilots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pilot"] == id)
            .unwrap()["signed_in"]
            .as_bool()
            .unwrap()
    }
    /// Type something into the kiosk's code box.
    async fn type_code(&self, from: &str, typed: &str) -> Answer {
        send(
            &self.app,
            from,
            Method::POST,
            "/public/pilot/kiosk",
            None,
            Some(json!({ "code": typed })),
            &[],
        )
        .await
    }
    /// A pilot's pass at the kiosk.
    async fn kiosk(&self, callsign: &str) -> String {
        let answer = self.type_code(KIOSK, &self.code(callsign).await).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        answer.json()["pass"].as_str().unwrap().to_string()
    }
    /// Take a seat from the kiosk, with a pass and maybe a cookie the tablet also holds.
    async fn kiosk_take(&self, pass: &str, cookie: Option<&str>, n: u32, s: u32) -> Answer {
        send(
            &self.app,
            KIOSK,
            Method::POST,
            "/public/pilot/signup/take",
            cookie,
            Some(json!({"heat": heat(n), "seat": seat(s)})),
            &[(PASS, pass)],
        )
        .await
    }
    async fn done(&self, pass: Option<&str>) -> Answer {
        let extra: Vec<(&str, &str)> = pass.map(|pass| (PASS, pass)).into_iter().collect();
        send(
            &self.app,
            KIOSK,
            Method::DELETE,
            "/public/pilot/kiosk",
            None,
            None,
            &extra,
        )
        .await
    }
    async fn hello(&self, extra: &[(&str, &str)], cookie: Option<&str>) -> Value {
        send(
            &self.app,
            KIOSK,
            Method::GET,
            "/public/hello",
            cookie,
            None,
            extra,
        )
        .await
        .json()
    }
}

#[tokio::test]
async fn a_right_code_gives_the_pilot_and_a_pass_and_sets_no_cookie() {
    let rig = rig().await;
    let code = rig.code("Ace").await;
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/public/pilot/kiosk")
        .header(header::HOST, "192.168.1.5:8081")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "code": code }).to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(KIOSK.parse::<SocketAddr>().unwrap()));
    let response = rig.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["pilot"],
        json!({"id": rig.pilot("Ace"), "callsign": "Ace"})
    );
    assert_eq!(body["idle_seconds"], 120);
    let pass = body["pass"].as_str().unwrap();
    assert!(pass.len() >= 32, "a pass is long and random: {pass}");
    assert_ne!(pass, code);
    // Exactly these three things, and nothing else about the pilot.
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["idle_seconds", "pass", "pilot"]);
}

#[tokio::test]
async fn the_code_can_be_typed_loosely_or_scanned_as_a_whole_link() {
    let rig = rig().await;
    let code = rig.code("Bee").await;
    let spaced = format!(" {}-{} ", code[..3].to_lowercase(), &code[3..]);
    let link = format!("http://192.168.1.5:8081/#/join/{code}");
    for typed in [code.clone(), spaced, link] {
        let answer = rig.type_code(KIOSK, &typed).await;
        assert_eq!(answer.status, StatusCode::OK, "{typed:?}: {}", answer.body);
        assert_eq!(answer.json()["pilot"]["callsign"], "Bee");
    }
}

#[tokio::test]
async fn a_wrong_code_is_refused_in_words_whatever_is_wrong_with_it() {
    let rig = rig().await;
    let code = rig.code("Ace").await;
    let reversed: String = code.chars().rev().collect();
    let long = "A".repeat(5000);
    let cases = [
        "",
        " ",
        "ABC",
        "ABCDEFG",
        "000000",
        "ABC DE!",
        "http://192.168.1.5:8081/#/join/",
        "http://192.168.1.5:8081/#/join/NOPE",
        "'; DROP TABLE pilots; --",
        long.as_str(),
    ];
    for (index, typed) in cases.iter().enumerate() {
        // A device of its own each time, so the count of wrong codes is not what answers.
        let from = format!("192.168.2.{}:50000", index + 1);
        let answer = rig.type_code(&from, typed).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{typed:?}");
        assert_eq!(
            answer.message(),
            "That code is not right. Check it with the director."
        );
        assert!(!answer.body.contains("pass"));
    }
    if reversed != code {
        let answer = rig.type_code("192.168.2.99:50000", &reversed).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_body_that_is_not_a_code_is_a_plain_error_not_a_crash() {
    let rig = rig().await;
    for body in [
        json!({}),
        json!({"code": 123456}),
        json!({"code": null}),
        json!({"code": ["ABCDEF"]}),
        json!([]),
        json!("ABCDEF"),
    ] {
        let answer = send(
            &rig.app,
            KIOSK,
            Method::POST,
            "/public/pilot/kiosk",
            None,
            Some(body.clone()),
            &[],
        )
        .await;
        assert!(
            answer.status.is_client_error(),
            "{body}: {} {}",
            answer.status,
            answer.body
        );
    }
}

#[tokio::test]
async fn a_pilot_who_is_not_in_the_event_has_no_code_that_works() {
    let rig = rig().await;
    // Eve is in the director's list but not in this event: she has no code at all. Give
    // her one by putting her in, read it, then take her out again.
    let everyone: Vec<&str> = ["Ace", "Bee", "Cat", "Dan", "Eve"]
        .iter()
        .map(|name| rig.pilot(name))
        .collect();
    let commands = format!("/events/{}/commands", rig.event);
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"Record": {"RosterSet": {"class": rig.open, "pilots": everyone}}}),
    )
    .await;
    let code = rig.code("Eve").await;
    assert_eq!(rig.type_code(KIOSK, &code).await.status, StatusCode::OK);
    direct(
        &rig.app,
        Method::POST,
        &commands,
        json!({"Record": {"RosterSet": {"class": rig.open, "pilots": everyone[..4]}}}),
    )
    .await;
    assert_eq!(
        rig.type_code(KIOSK, &code).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn ten_wrong_codes_make_the_kiosk_wait_and_do_not_lock_out_other_devices() {
    let rig = rig().await;
    for _ in 0..10 {
        assert_eq!(
            rig.type_code(KIOSK, "ZZZZZZ").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Now even the right code has to wait.
    let code = rig.code("Ace").await;
    let blocked = rig.type_code(KIOSK, &code).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        blocked.message(),
        "Too many wrong codes from this device. Wait a minute and try again."
    );
    // Another tablet is not held up.
    assert_eq!(
        rig.type_code("192.168.1.51:50000", &code).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn wrong_codes_at_the_kiosk_and_on_the_sign_in_page_are_one_count() {
    let rig = rig().await;
    for round in 0..10 {
        let route = if round % 2 == 0 {
            "/public/pilot/kiosk"
        } else {
            "/public/pilot/session"
        };
        let answer = send(
            &rig.app,
            KIOSK,
            Method::POST,
            route,
            None,
            Some(json!({"code": "ZZZZZZ"})),
            &[],
        )
        .await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "round {round}");
    }
    let code = rig.code("Ace").await;
    for route in ["/public/pilot/kiosk", "/public/pilot/session"] {
        let answer = send(
            &rig.app,
            KIOSK,
            Method::POST,
            route,
            None,
            Some(json!({ "code": code })),
            &[],
        )
        .await;
        assert_eq!(answer.status, StatusCode::TOO_MANY_REQUESTS, "{route}");
    }
}

#[tokio::test]
async fn a_right_code_clears_the_count_of_wrong_ones() {
    let rig = rig().await;
    let code = rig.code("Ace").await;
    for _ in 0..3 {
        for _ in 0..9 {
            rig.type_code(KIOSK, "ZZZZZZ").await;
        }
        assert_eq!(rig.type_code(KIOSK, &code).await.status, StatusCode::OK);
    }
}

#[tokio::test]
async fn the_kiosk_does_not_sign_the_pilots_own_phone_out() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let phone = rig.sign_in("Ace").await;
    assert!(rig.signed_in("Ace").await);
    let pass = rig.kiosk("Ace").await;
    // Both work, one after the other.
    assert_eq!(
        rig.kiosk_take(&pass, None, 1, 1).await.status,
        StatusCode::OK
    );
    assert_eq!(rig.take(&phone, 2, 1).await.status, StatusCode::OK);
    assert_eq!(rig.spots().await[..2], ["Ace,-,-", "Ace,-,-"]);
    // And being done at the kiosk leaves the phone signed in.
    assert_eq!(rig.done(Some(&pass)).await.status, StatusCode::NO_CONTENT);
    assert!(rig.signed_in("Ace").await);
    assert_eq!(
        rig.as_pilot(&phone, "leave", json!({"heat": heat(2)}))
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_kiosk_alone_never_shows_the_pilot_as_signed_in_on_a_device() {
    let rig = rig().await;
    let _pass = rig.kiosk("Bee").await;
    assert!(!rig.signed_in("Bee").await);
}

#[tokio::test]
async fn a_pass_acts_as_its_pilot_and_nobody_else() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.kiosk("Ace").await;
    let bee = rig.kiosk("Bee").await;
    assert_eq!(
        rig.kiosk_take(&ace, None, 1, 1).await.status,
        StatusCode::OK
    );
    assert_eq!(
        rig.kiosk_take(&bee, None, 1, 2).await.status,
        StatusCode::OK
    );
    assert_eq!(rig.spots().await[0], "Ace,Bee,-");
    // The body cannot name another pilot.
    let sneaky = send(
        &rig.app,
        KIOSK,
        Method::POST,
        "/public/pilot/signup/take",
        None,
        Some(
            json!({"heat": heat(2), "seat": seat(1), "pilot": rig.pilot("Cat"), "by": "Director"}),
        ),
        &[(PASS, &ace)],
    )
    .await;
    assert_eq!(sneaky.status, StatusCode::OK);
    assert_eq!(rig.spots().await[1], "Ace,-,-");
    // The page asks who it is with the pass, and is told.
    assert_eq!(
        rig.hello(&[(PASS, &bee)], None).await["pilot"]["callsign"],
        "Bee"
    );
    assert_eq!(rig.hello(&[], None).await["pilot"], Value::Null);
}

#[tokio::test]
async fn on_a_tablet_that_someone_once_signed_in_on_the_pass_decides_not_the_cookie() {
    let rig = rig().await;
    rig.set(json!({})).await;
    // Ace once signed in on this tablet the ordinary way. Its browser still has the cookie.
    let (_, cookie) = rig.sign_in("Ace").await;
    let bee = rig.kiosk("Bee").await;
    assert_eq!(
        rig.kiosk_take(&bee, Some(&cookie), 1, 1).await.status,
        StatusCode::OK
    );
    assert_eq!(rig.spots().await[0], "Bee,-,-");
    assert_eq!(
        rig.hello(&[(PASS, &bee)], Some(&cookie)).await["pilot"]["callsign"],
        "Bee"
    );
    // A pass that is no good must not fall back to Ace.
    for bad in ["", "nope", &bee[..bee.len() - 1], &format!("{bee}x")] {
        let answer = rig.kiosk_take(bad, Some(&cookie), 2, 1).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{bad:?}");
        assert_eq!(
            rig.hello(&[(PASS, bad)], Some(&cookie)).await["pilot"],
            Value::Null
        );
    }
    assert_eq!(rig.spots().await[1], "-,-,-");
    // After Bee is done, the same.
    rig.done(Some(&bee)).await;
    assert_eq!(
        rig.kiosk_take(&bee, Some(&cookie), 2, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_pass_is_no_use_anywhere_but_the_header() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let pass = rig.kiosk("Ace").await;
    let body = json!({"heat": heat(1), "seat": seat(1)});
    // As the pilot cookie, as the director's token, or in the address.
    let as_cookie = send(
        &rig.app,
        KIOSK,
        Method::POST,
        "/public/pilot/signup/take",
        Some(&pass),
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(as_cookie.status, StatusCode::UNAUTHORIZED);
    let bearer = format!("Bearer {pass}");
    let as_token = send(
        &rig.app,
        KIOSK,
        Method::POST,
        &format!("/events/{}/commands", rig.event),
        None,
        Some(json!({"Signup": {"Set": {"phase": QUALI}}})),
        &[("authorization", &bearer), (PASS, &pass)],
    )
    .await;
    assert!(
        as_token.status == StatusCode::UNAUTHORIZED || as_token.status == StatusCode::FORBIDDEN,
        "{}",
        as_token.status
    );
    let in_query = send(
        &rig.app,
        KIOSK,
        Method::POST,
        &format!("/public/pilot/signup/take?kiosk={pass}&x-gridfpv-kiosk={pass}"),
        None,
        Some(body),
        &[],
    )
    .await;
    assert_eq!(in_query.status, StatusCode::UNAUTHORIZED);
    assert_eq!(rig.spots().await[0], "-,-,-");
    // Sign-up is still on: the pass did not reach the director's command.
    assert!(rig.signup().await.is_object());
}

#[tokio::test]
async fn done_ends_the_pass_and_only_that_pass() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let first = rig.kiosk("Ace").await;
    let second = rig.kiosk("Ace").await;
    assert_ne!(first, second);
    assert_eq!(rig.done(Some(&first)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(
        rig.kiosk_take(&first, None, 1, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
    // A second kiosk the same pilot is at carries on.
    assert_eq!(
        rig.kiosk_take(&second, None, 1, 1).await.status,
        StatusCode::OK
    );
    // Done twice, done with nothing, done with rubbish: all the same answer.
    for pass in [Some(first.as_str()), None, Some("rubbish")] {
        assert_eq!(rig.done(pass).await.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        rig.kiosk_take(&second, None, 2, 1).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_new_code_or_the_directors_sign_out_ends_a_pass() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.kiosk("Ace").await;
    let bee = rig.kiosk("Bee").await;
    let cat = rig.kiosk("Cat").await;
    let old_code = rig.code("Ace").await;
    // A new code for Ace.
    direct(
        &rig.app,
        Method::POST,
        &format!("/events/{}/access/{}/reset", rig.event, rig.pilot("Ace")),
        json!({}),
    )
    .await;
    assert_eq!(
        rig.kiosk_take(&ace, None, 1, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        rig.type_code(KIOSK, &old_code).await.status,
        StatusCode::UNAUTHORIZED
    );
    // The director signs Bee out.
    send(
        &rig.app,
        HERE,
        Method::DELETE,
        &format!("/events/{}/access/{}/session", rig.event, rig.pilot("Bee")),
        None,
        None,
        &[],
    )
    .await;
    assert_eq!(
        rig.kiosk_take(&bee, None, 1, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Cat is untouched by either, and Ace's new code works.
    assert_eq!(
        rig.kiosk_take(&cat, None, 1, 1).await.status,
        StatusCode::OK
    );
    let again = rig.kiosk("Ace").await;
    assert_eq!(
        rig.kiosk_take(&again, None, 1, 2).await.status,
        StatusCode::OK
    );
    assert_eq!(rig.spots().await[0], "Cat,Ace,-");
}

#[tokio::test]
async fn a_pilot_taken_out_of_the_event_cannot_go_on_at_the_kiosk() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let bee = rig.kiosk("Bee").await;
    direct(
        &rig.app,
        Method::POST,
        &format!("/events/{}/commands", rig.event),
        json!({"Record": {"RosterSet": {"class": rig.open, "pilots": [rig.pilot("Ace")]}}}),
    )
    .await;
    assert_eq!(
        rig.kiosk_take(&bee, None, 1, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(rig.hello(&[(PASS, &bee)], None).await["pilot"], Value::Null);
}

#[tokio::test]
async fn a_pass_from_one_event_is_no_use_in_another() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.kiosk("Ace").await;
    let other = direct(
        &rig.app,
        Method::POST,
        "/events",
        json!({"name": "Saturday"}),
    )
    .await["id"]
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
    assert_eq!(
        rig.kiosk_take(&ace, None, 1, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
    // And the code is for Friday only.
    let friday = {
        direct(
            &rig.app,
            Method::PUT,
            "/active-event",
            json!({ "id": rig.event }),
        )
        .await;
        rig.code("Ace").await
    };
    direct(
        &rig.app,
        Method::PUT,
        "/active-event",
        json!({ "id": other }),
    )
    .await;
    assert_eq!(
        rig.type_code(KIOSK, &friday).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Back on Friday the pass still works: it was never ended.
    direct(
        &rig.app,
        Method::PUT,
        "/active-event",
        json!({ "id": rig.event }),
    )
    .await;
    assert_eq!(
        rig.kiosk_take(&ace, None, 1, 1).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn deleting_the_event_ends_its_passes() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let ace = rig.kiosk("Ace").await;
    let other = direct(
        &rig.app,
        Method::POST,
        "/events",
        json!({"name": "Saturday"}),
    )
    .await["id"]
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
    let gone = send(
        &rig.app,
        HERE,
        Method::DELETE,
        &format!("/events/{}", rig.event),
        None,
        None,
        &[],
    )
    .await;
    assert!(gone.status.is_success(), "{}: {}", gone.status, gone.body);
    assert_eq!(
        rig.kiosk_take(&ace, None, 1, 1).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn with_no_event_open_the_kiosk_says_so() {
    let rig = rig().await;
    let code = rig.code("Ace").await;
    let pass = rig.kiosk("Ace").await;
    let gone = send(
        &rig.app,
        HERE,
        Method::DELETE,
        &format!("/events/{}", rig.event),
        None,
        None,
        &[],
    )
    .await;
    assert!(gone.status.is_success(), "{}", gone.status);
    assert_eq!(
        rig.hello(&[(PASS, &pass)], None).await["pilot"],
        Value::Null
    );
    let answer = rig.type_code(KIOSK, &code).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.message(), "No event is open.");
}

#[tokio::test]
async fn another_website_cannot_use_the_kiosk_routes() {
    let rig = rig().await;
    let code = rig.code("Ace").await;
    let pass = rig.kiosk("Ace").await;
    let evil = [("origin", "http://evil.example")];
    let typed = send(
        &rig.app,
        KIOSK,
        Method::POST,
        "/public/pilot/kiosk",
        None,
        Some(json!({ "code": code })),
        &evil,
    )
    .await;
    assert_eq!(typed.status, StatusCode::FORBIDDEN);
    let ended = send(
        &rig.app,
        KIOSK,
        Method::DELETE,
        "/public/pilot/kiosk",
        None,
        None,
        &[("origin", "http://evil.example"), (PASS, &pass)],
    )
    .await;
    assert_eq!(ended.status, StatusCode::FORBIDDEN);
    // The pass was not ended by it.
    rig.set(json!({})).await;
    assert_eq!(
        rig.kiosk_take(&pass, None, 1, 1).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn only_post_and_delete_are_answered_on_the_kiosk_route() {
    let rig = rig().await;
    for method in [Method::GET, Method::PUT, Method::PATCH] {
        let answer = send(
            &rig.app,
            KIOSK,
            method.clone(),
            "/public/pilot/kiosk",
            None,
            Some(json!({"code": "ABCDEF"})),
            &[],
        )
        .await;
        assert!(
            answer.status == StatusCode::METHOD_NOT_ALLOWED
                || answer.status == StatusCode::UNAUTHORIZED
                || answer.status == StatusCode::FORBIDDEN,
            "{method}: {}",
            answer.status
        );
        assert!(!answer.body.contains("pass"));
    }
}

#[tokio::test]
async fn a_pass_never_shows_in_anything_a_visitor_or_the_director_can_read() {
    let rig = rig().await;
    rig.set(json!({})).await;
    let pass = rig.kiosk("Ace").await;
    rig.kiosk_take(&pass, None, 1, 1).await;
    let mut pages = vec![
        "/public/hello".to_string(),
        "/public/event".to_string(),
        "/public/event/state".to_string(),
        "/public/event/pilots".to_string(),
    ];
    pages.push(format!("/events/{}/access", rig.event));
    pages.push(format!("/events/{}/state", rig.event));
    for page in pages {
        let answer = send(
            &rig.app,
            HERE,
            Method::GET,
            &page,
            None,
            None,
            &[(PASS, &pass)],
        )
        .await;
        assert!(answer.status.is_success(), "{page}: {}", answer.status);
        assert!(!answer.body.contains(&pass), "{page} holds the pass");
    }
    // Nor is it written down.
    for entry in std::fs::read_dir(&rig.dir).unwrap().flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path()) {
            assert!(!text.contains(&pass), "{:?} holds the pass", entry.path());
        }
    }
}

#[tokio::test]
async fn many_pilots_at_many_kiosks_at_once_each_get_their_own_pass() {
    let rig = rig().await;
    rig.set(json!({"open_heats": 4})).await;
    let mut codes = Vec::new();
    for name in ["Ace", "Bee", "Cat", "Dan"] {
        codes.push((name, rig.code(name).await));
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(codes.len() * 5));
    let mut tasks = tokio::task::JoinSet::new();
    for round in 0..5 {
        for (index, (name, code)) in codes.iter().enumerate() {
            let app = rig.app.clone();
            let barrier = barrier.clone();
            let code = code.clone();
            let name = name.to_string();
            let from = format!("192.168.3.{}:50000", 1 + index + round * 10);
            tasks.spawn(async move {
                barrier.wait().await;
                let answer = send(
                    &app,
                    &from,
                    Method::POST,
                    "/public/pilot/kiosk",
                    None,
                    Some(json!({ "code": code })),
                    &[],
                )
                .await;
                (name, answer)
            });
        }
    }
    let mut passes = std::collections::BTreeSet::new();
    while let Some(done) = tasks.join_next().await {
        let (name, answer) = done.unwrap();
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        assert_eq!(answer.json()["pilot"]["callsign"], name.as_str());
        let pass = answer.json()["pass"].as_str().unwrap().to_string();
        assert_eq!(
            rig.hello(&[(PASS, &pass)], None).await["pilot"]["callsign"],
            name.as_str()
        );
        assert!(passes.insert(pass), "two kiosks got the same pass");
    }
    assert_eq!(passes.len(), 20);
}
