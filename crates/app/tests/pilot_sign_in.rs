//! A pilot signing in at the race (docs/primitives.md §11.3), through the app as served.
//!
//! The director reads each pilot's code from the console. A pilot types it (or scans it)
//! on their phone and is then known by name, for that event, on that one device. Being
//! signed in as a pilot must open nothing of the director's.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_server::access::MAX_FAILURES;
use gridfpv_server::events::EventRegistry;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const HERE: &str = "127.0.0.1:50000";
const PHONE: &str = "192.168.1.77:50000";
const TABLET: &str = "192.168.1.78:50000";

struct Rig {
    registry: EventRegistry,
    app: Router,
    dir: PathBuf,
    event: String,
    ace: String,
    bee: String,
    open: String,
}

impl Drop for Rig {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

#[derive(Clone)]
struct Call {
    method: Method,
    uri: String,
    peer: String,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

fn call(method: Method, uri: &str) -> Call {
    Call {
        method,
        uri: uri.to_string(),
        peer: HERE.to_string(),
        headers: Vec::new(),
        body: None,
    }
}

impl Call {
    fn from(mut self, peer: &str) -> Self {
        self.peer = peer.to_string();
        self
    }
    fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    fn pilot(self, session: &str) -> Self {
        self.header("cookie", &format!("gridfpv_pilot={session}"))
    }
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {:?}", self.body))
    }
    fn set_cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .map(|value| value.to_str().unwrap().to_string())
    }
    fn session(&self) -> String {
        let set = self.set_cookie().expect("a cookie was set");
        set.split(';')
            .next()
            .unwrap()
            .split_once('=')
            .unwrap()
            .1
            .to_string()
    }
}

async fn send(rig: &Rig, call: Call) -> Answer {
    let local = call.peer.starts_with("127.");
    let mut request = Request::builder()
        .method(call.method)
        .uri(&call.uri)
        .header(
            header::HOST,
            if local {
                "127.0.0.1:8080"
            } else {
                "192.168.1.5:8081"
            },
        );
    for (name, value) in &call.headers {
        request = request.header(name, value);
    }
    let body = match &call.body {
        Some(body) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(call.peer.parse::<SocketAddr>().unwrap()));
    let response = rig.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn direct(rig_app: &Router, method: Method, uri: &str, body: Value) -> Value {
    let mut request = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(HERE.parse::<SocketAddr>().unwrap()));
    let response = rig_app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(
        status.is_success(),
        "{method} {uri} answered {status}: {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// A Director with an open event that Ace and Bee are in. Cat is in the director's list
/// but not in the event.
async fn rig() -> Rig {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gridfpv-pilot-sign-in-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<title>console</title>").unwrap();
    let registry = EventRegistry::new(None).unwrap();
    let app = build_app(registry.clone(), &dir);
    let id = |value: Value| value["id"].as_str().unwrap().to_string();
    let ace = id(direct(&app, Method::POST, "/pilots", json!({"callsign": "Ace"})).await);
    let bee = id(direct(&app, Method::POST, "/pilots", json!({"callsign": "Bee"})).await);
    direct(&app, Method::POST, "/pilots", json!({"callsign": "Cat"})).await;
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
    direct(&app, Method::PUT, "/active-event", json!({"id": event})).await;
    direct(
        &app,
        Method::POST,
        &format!("/events/{event}/commands"),
        json!({"Record": {"RosterSet": {"class": open, "pilots": [ace, bee]}}}),
    )
    .await;
    Rig {
        registry,
        app,
        dir,
        event,
        ace,
        bee,
        open,
    }
}

/// The director reads the codes. Gives each pilot's code by pilot id.
async fn codes(rig: &Rig) -> Value {
    let answer = send(
        rig,
        call(Method::GET, &format!("/events/{}/access", rig.event)),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(
        answer.headers[header::CACHE_CONTROL],
        "no-store",
        "a browser must not keep the codes"
    );
    answer.json()
}

async fn code_of(rig: &Rig, pilot: &str) -> String {
    codes(rig).await["pilots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pilot"] == pilot)
        .unwrap_or_else(|| panic!("{pilot} has a code"))["code"]
        .as_str()
        .unwrap()
        .to_string()
}

fn sign_in(code: &str) -> Call {
    call(Method::POST, "/public/pilot/session")
        .from(PHONE)
        .json(json!({"code": code}))
}

/// Who the Director says a browser is signed in as.
async fn pilot_of(rig: &Rig, peer: &str, session: &str) -> Option<String> {
    let hello = send(
        rig,
        call(Method::GET, "/public/hello").from(peer).pilot(session),
    )
    .await
    .json();
    hello["pilot"]["callsign"].as_str().map(str::to_string)
}

/// A code that is certainly wrong: the right one with its first character changed.
fn wrong(code: &str) -> String {
    let mut bytes = code.as_bytes().to_vec();
    bytes[0] = if bytes[0] == b'A' { b'B' } else { b'A' };
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn the_director_reads_a_code_for_every_pilot_in_the_event_and_no_other() {
    let rig = rig().await;
    let listed = codes(&rig).await;
    let pilots = listed["pilots"].as_array().unwrap();
    let mut ids: Vec<&str> = pilots
        .iter()
        .map(|p| p["pilot"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected = [rig.ace.as_str(), rig.bee.as_str()];
    expected.sort_unstable();
    assert_eq!(ids, expected, "Cat is not in the event and has no code");
    for entry in pilots {
        let code = entry["code"].as_str().unwrap();
        assert_eq!(code.len(), 6);
        assert!(
            code.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );
        assert_eq!(entry["signed_in"], false);
    }
    assert_ne!(pilots[0]["code"], pilots[1]["code"]);
    // The network is off in this rig: there is no address to give.
    assert!(listed.get("address").is_none(), "{listed}");
    // Reading again gives the same codes.
    assert_eq!(codes(&rig).await, listed);
}

#[tokio::test]
async fn a_pilot_signs_in_with_their_code_and_is_known_by_name() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let answer = send(&rig, sign_in(&code)).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.json(), json!({"id": rig.ace, "callsign": "Ace"}));
    let set = answer.set_cookie().unwrap();
    assert!(set.starts_with("gridfpv_pilot="));
    for part in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(set.contains(part), "{part} in {set}");
    }
    let session = answer.session();
    assert!(!session.contains(&code), "the cookie is not the code");
    assert_eq!(
        pilot_of(&rig, PHONE, &session).await.as_deref(),
        Some("Ace")
    );
    // The director's list now says so.
    let listed = codes(&rig).await;
    let ace = listed["pilots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["pilot"] == rig.ace.as_str())
        .unwrap();
    assert_eq!(ace["signed_in"], true);
    // A browser with no sign-in is nobody.
    let hello = send(&rig, call(Method::GET, "/public/hello").from(PHONE))
        .await
        .json();
    assert!(hello.get("pilot").is_none(), "{hello}");
}

#[tokio::test]
async fn a_code_works_however_it_is_typed() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    for typed in [
        code.to_lowercase(),
        format!("{}-{}", &code[..3], &code[3..]),
        format!(" {} {} ", &code[..3], &code[3..]),
    ] {
        let answer = send(&rig, sign_in(&typed)).await;
        assert_eq!(answer.status, StatusCode::OK, "{typed:?}: {}", answer.body);
    }
}

#[tokio::test]
async fn a_wrong_code_is_refused_the_same_way_whatever_is_wrong_with_it() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let mut said = Vec::new();
    for (n, typed) in [
        wrong(&code),
        String::new(),
        "AAAAAA".into(),
        code[..5].to_string(),
        format!("{code}A"),
        "not a code at all".into(),
        "'; DROP TABLE pilots; --".into(),
        "🙂🙂🙂🙂🙂🙂".into(),
    ]
    .into_iter()
    .enumerate()
    {
        // Each from its own device, so none is shut out for guessing.
        let device = format!("192.168.2.{}:1", n + 1);
        let answer = send(&rig, sign_in(&typed).from(&device)).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{typed:?}");
        assert!(answer.set_cookie().is_none(), "{typed:?} set a cookie");
        said.push(answer.json()["message"].as_str().unwrap().to_string());
    }
    said.dedup();
    assert_eq!(said.len(), 1, "one message for every wrong code: {said:?}");
    assert!(
        !codes(&rig).await.to_string().contains("\"signed_in\":true"),
        "nobody was signed in"
    );
}

#[tokio::test]
async fn a_body_that_is_not_a_sign_in_is_a_plain_error() {
    let rig = rig().await;
    for body in [
        json!({}),
        json!({"code": 7}),
        json!({"code": null}),
        json!([]),
        json!("ABC234"),
    ] {
        let answer = send(
            &rig,
            call(Method::POST, "/public/pilot/session")
                .from(PHONE)
                .json(body.clone()),
        )
        .await;
        assert!(answer.status.is_client_error(), "{body}: {}", answer.status);
        assert!(answer.set_cookie().is_none());
    }
    // No body at all.
    let answer = send(
        &rig,
        call(Method::POST, "/public/pilot/session").from(PHONE),
    )
    .await;
    assert!(answer.status.is_client_error());
}

#[tokio::test]
async fn one_device_at_a_time_a_second_sign_in_replaces_the_first() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let phone = send(&rig, sign_in(&code)).await.session();
    let tablet = send(&rig, sign_in(&code).from(TABLET)).await.session();
    assert_ne!(phone, tablet);
    assert_eq!(
        pilot_of(&rig, PHONE, &phone).await,
        None,
        "the phone is signed out"
    );
    assert_eq!(
        pilot_of(&rig, TABLET, &tablet).await.as_deref(),
        Some("Ace")
    );
    // The same device signing in again also gets a fresh sign-in.
    let again = send(&rig, sign_in(&code).from(TABLET)).await.session();
    assert_eq!(pilot_of(&rig, TABLET, &tablet).await, None);
    assert_eq!(pilot_of(&rig, TABLET, &again).await.as_deref(), Some("Ace"));
}

#[tokio::test]
async fn two_pilots_are_signed_in_side_by_side() {
    let rig = rig().await;
    let ace = send(&rig, sign_in(&code_of(&rig, &rig.ace).await))
        .await
        .session();
    let bee = send(&rig, sign_in(&code_of(&rig, &rig.bee).await).from(TABLET))
        .await
        .session();
    assert_eq!(pilot_of(&rig, PHONE, &ace).await.as_deref(), Some("Ace"));
    assert_eq!(pilot_of(&rig, TABLET, &bee).await.as_deref(), Some("Bee"));
    // A sign-in is the pilot's, not the device's: it names the same pilot from anywhere.
    assert_eq!(pilot_of(&rig, TABLET, &ace).await.as_deref(), Some("Ace"));
}

#[tokio::test]
async fn being_signed_in_as_a_pilot_opens_nothing_of_the_directors() {
    let rig = rig().await;
    let session = send(&rig, sign_in(&code_of(&rig, &rig.ace).await))
        .await
        .session();
    let access = format!("/events/{}/access", rig.event);
    for (method, uri, body) in [
        (Method::GET, "/events".to_string(), None),
        (Method::GET, "/pilots".to_string(), None),
        (Method::GET, format!("/events/{}/state", rig.event), None),
        // Above all: not the other pilots' codes.
        (Method::GET, access.clone(), None),
        (Method::GET, format!("{access}/{}/qr", rig.bee), None),
        (Method::POST, format!("{access}/{}/reset", rig.bee), None),
        (Method::POST, format!("{access}/{}/reset", rig.ace), None),
        (
            Method::DELETE,
            format!("{access}/{}/session", rig.bee),
            None,
        ),
        (
            Method::POST,
            "/events".to_string(),
            Some(json!({"name": "Sneaky"})),
        ),
        (
            Method::POST,
            format!("/events/{}/commands", rig.event),
            Some(json!({"StartRace": {"heat": "h"}})),
        ),
        (
            Method::PUT,
            "/network".to_string(),
            Some(json!({"enabled": true})),
        ),
    ] {
        let mut request = call(method.clone(), &uri).from(PHONE).pilot(&session);
        request.body = body;
        let answer = send(&rig, request).await;
        assert_eq!(
            answer.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri}: {}",
            answer.body
        );
    }
    // The pilot cookie is not the director's cookie either.
    let as_director = call(Method::GET, "/events")
        .from(PHONE)
        .header("cookie", &format!("gridfpv_director={session}"));
    assert_eq!(
        send(&rig, as_director).await.status,
        StatusCode::UNAUTHORIZED
    );
    let hello = send(
        &rig,
        call(Method::GET, "/public/hello")
            .from(PHONE)
            .pilot(&session),
    )
    .await
    .json();
    assert_eq!(
        hello["caller"], "Visitor",
        "a pilot is still a visitor to the console"
    );
}

#[tokio::test]
async fn nothing_a_visitor_can_read_holds_a_code() {
    let rig = rig().await;
    let listed = codes(&rig).await;
    let all_codes: Vec<&str> = listed["pilots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["code"].as_str().unwrap())
        .collect();
    let session = send(&rig, sign_in(all_codes[0])).await.session();
    for uri in [
        "/public/hello",
        "/public/event",
        "/public/event/state",
        "/public/event/pilots",
        "/public/event/classes",
        "/public/event/timers",
        "/about",
        "/",
    ] {
        for request in [
            call(Method::GET, uri).from(PHONE),
            call(Method::GET, uri).from(PHONE).pilot(&session),
        ] {
            let answer = send(&rig, request).await;
            assert_eq!(answer.status, StatusCode::OK, "{uri}");
            for code in &all_codes {
                assert!(!answer.body.contains(code), "{uri} holds a code");
            }
            assert!(!answer.body.contains(&session), "{uri} holds a sign-in");
        }
    }
}

#[tokio::test]
async fn wrong_codes_from_one_device_shut_it_out_for_a_while() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    for n in 0..MAX_FAILURES {
        let answer = send(&rig, sign_in(&wrong(&code))).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "guess {n}");
    }
    // Now the right code is refused too, from that device.
    let shut = send(&rig, sign_in(&code)).await;
    assert_eq!(shut.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(shut.json()["code"], "TooManyRequests");
    assert!(shut.set_cookie().is_none());
    // Another device is not touched, and the first can still watch.
    assert_eq!(
        send(&rig, sign_in(&code).from(TABLET)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        send(&rig, call(Method::GET, "/public/event").from(PHONE))
            .await
            .status,
        StatusCode::OK
    );
    // Guessing pilot codes does not shut the device out of the director's sign-in count,
    // and the other way round: the two are counted apart.
    assert!(
        !rig.registry
            .guesses()
            .blocked("192.168.1.77".parse().unwrap(), std::time::Instant::now())
    );
}

#[tokio::test]
async fn a_right_code_clears_the_count_of_wrong_ones() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    for round in 0..3 {
        for _ in 0..MAX_FAILURES - 1 {
            send(&rig, sign_in(&wrong(&code))).await;
        }
        assert_eq!(
            send(&rig, sign_in(&code)).await.status,
            StatusCode::OK,
            "round {round}"
        );
    }
}

#[tokio::test]
async fn the_director_gives_a_new_code_and_the_old_one_is_dead() {
    let rig = rig().await;
    let old = code_of(&rig, &rig.ace).await;
    let bee = code_of(&rig, &rig.bee).await;
    let session = send(&rig, sign_in(&old)).await.session();
    let reset = send(
        &rig,
        call(
            Method::POST,
            &format!("/events/{}/access/{}/reset", rig.event, rig.ace),
        ),
    )
    .await;
    assert_eq!(reset.status, StatusCode::OK, "{}", reset.body);
    let fresh = reset.json()["code"].as_str().unwrap().to_string();
    assert_ne!(fresh, old);
    assert_eq!(reset.json()["signed_in"], false);
    assert_eq!(
        pilot_of(&rig, PHONE, &session).await,
        None,
        "the device is signed out"
    );
    assert_eq!(
        send(&rig, sign_in(&old)).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        send(&rig, sign_in(&fresh).from(TABLET)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        code_of(&rig, &rig.bee).await,
        bee,
        "the other pilot keeps their code"
    );
}

#[tokio::test]
async fn the_director_signs_a_device_out_and_the_code_still_works() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let session = send(&rig, sign_in(&code)).await.session();
    let uri = format!("/events/{}/access/{}/session", rig.event, rig.ace);
    assert_eq!(
        send(&rig, call(Method::DELETE, &uri)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(pilot_of(&rig, PHONE, &session).await, None);
    // Doing it again, or for a pilot who was never signed in, is fine.
    assert_eq!(
        send(&rig, call(Method::DELETE, &uri)).await.status,
        StatusCode::NO_CONTENT
    );
    let bee = format!("/events/{}/access/{}/session", rig.event, rig.bee);
    assert_eq!(
        send(&rig, call(Method::DELETE, &bee)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(send(&rig, sign_in(&code)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_pilot_signs_themselves_out() {
    let rig = rig().await;
    let ace = send(&rig, sign_in(&code_of(&rig, &rig.ace).await))
        .await
        .session();
    let bee = send(&rig, sign_in(&code_of(&rig, &rig.bee).await).from(TABLET))
        .await
        .session();
    let out = send(
        &rig,
        call(Method::DELETE, "/public/pilot/session")
            .from(PHONE)
            .pilot(&ace),
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.set_cookie().unwrap().contains("Max-Age=0"));
    assert_eq!(pilot_of(&rig, PHONE, &ace).await, None);
    assert_eq!(pilot_of(&rig, TABLET, &bee).await.as_deref(), Some("Bee"));
    // Signing out with nothing, or with rubbish, is fine and ends nobody else's sign-in.
    for request in [
        call(Method::DELETE, "/public/pilot/session").from(PHONE),
        call(Method::DELETE, "/public/pilot/session")
            .from(PHONE)
            .pilot("rubbish"),
        call(Method::DELETE, "/public/pilot/session")
            .from(PHONE)
            .pilot(""),
    ] {
        assert_eq!(send(&rig, request).await.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(pilot_of(&rig, TABLET, &bee).await.as_deref(), Some("Bee"));
}

#[tokio::test]
async fn a_sign_in_is_for_the_event_it_was_made_in() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let session = send(&rig, sign_in(&code)).await.session();
    // The director opens another event, with Ace in it too.
    let other = direct(&rig.app, Method::POST, "/events", json!({"name": "Sunday"})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    direct(
        &rig.app,
        Method::POST,
        &format!("/events/{other}/commands"),
        json!({"Record": {"RosterSet": {"class": rig.open, "pilots": [rig.ace]}}}),
    )
    .await;
    direct(&rig.app, Method::PUT, "/active-event", json!({"id": other})).await;
    assert_eq!(
        pilot_of(&rig, PHONE, &session).await,
        None,
        "not signed in at Sunday"
    );
    // Friday's code does not sign in at Sunday, and Sunday has a code of its own.
    assert_eq!(
        send(&rig, sign_in(&code)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let sunday = send(&rig, call(Method::GET, &format!("/events/{other}/access")))
        .await
        .json()["pilots"][0]["code"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(sunday, code);
    assert_eq!(
        send(&rig, sign_in(&sunday).from(TABLET)).await.status,
        StatusCode::OK
    );
    // Back to Friday: the phone is still signed in there.
    direct(
        &rig.app,
        Method::PUT,
        "/active-event",
        json!({"id": rig.event}),
    )
    .await;
    assert_eq!(
        pilot_of(&rig, PHONE, &session).await.as_deref(),
        Some("Ace")
    );
}

#[tokio::test]
async fn a_pilot_taken_out_of_the_event_is_signed_out() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let session = send(&rig, sign_in(&code)).await.session();
    direct(
        &rig.app,
        Method::POST,
        &format!("/events/{}/commands", rig.event),
        json!({"Record": {"RosterSet": {"class": rig.open, "pilots": [rig.bee]}}}),
    )
    .await;
    assert_eq!(pilot_of(&rig, PHONE, &session).await, None);
    assert_eq!(
        send(&rig, sign_in(&code)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let listed = codes(&rig).await;
    assert_eq!(listed["pilots"].as_array().unwrap().len(), 1);
    assert_eq!(listed["pilots"][0]["pilot"], rig.bee.as_str());
}

#[tokio::test]
async fn with_no_event_open_nobody_can_sign_in_and_it_says_why() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let session = send(&rig, sign_in(&code)).await.session();
    send(
        &rig,
        call(Method::DELETE, &format!("/events/{}", rig.event)),
    )
    .await;
    let answer = send(&rig, sign_in(&code)).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.json()["message"], "No event is open.");
    assert_eq!(pilot_of(&rig, PHONE, &session).await, None);
}

#[tokio::test]
async fn the_directors_routes_say_so_for_an_event_or_pilot_that_is_not_there() {
    let rig = rig().await;
    for (method, uri) in [
        (Method::GET, "/events/no-such-event/access".to_string()),
        (
            Method::POST,
            format!("/events/no-such-event/access/{}/reset", rig.ace),
        ),
        (
            Method::DELETE,
            format!("/events/no-such-event/access/{}/session", rig.ace),
        ),
        (
            Method::GET,
            format!("/events/no-such-event/access/{}/qr", rig.ace),
        ),
        (
            Method::POST,
            format!("/events/{}/access/no-such-pilot/reset", rig.event),
        ),
        (
            Method::GET,
            format!("/events/{}/access/no-such-pilot/qr", rig.event),
        ),
    ] {
        let answer = send(&rig, call(method.clone(), &uri)).await;
        assert_eq!(
            answer.status,
            StatusCode::NOT_FOUND,
            "{method} {uri}: {}",
            answer.body
        );
        assert_eq!(answer.json()["code"], "UnknownScope");
    }
    // A pilot in the director's list but not in the event has no code to reset.
    let cat = send(&rig, call(Method::GET, "/pilots")).await.json();
    let cat = cat
        .as_array()
        .unwrap()
        .iter()
        .find(|pilot| pilot["callsign"] == "Cat")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let answer = send(
        &rig,
        call(
            Method::POST,
            &format!("/events/{}/access/{cat}/reset", rig.event),
        ),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert!(
        answer.json()["message"]
            .as_str()
            .unwrap()
            .contains("not in this event")
    );
}

#[tokio::test]
async fn the_qr_code_needs_an_address_to_point_at() {
    let rig = rig().await;
    let uri = format!("/events/{}/access/{}/qr", rig.event, rig.ace);
    // The network is off: there is nothing to put in a QR code.
    let off = send(&rig, call(Method::GET, &uri)).await;
    assert_eq!(off.status, StatusCode::CONFLICT);
    assert!(
        off.json()["message"]
            .as_str()
            .unwrap()
            .contains("Switch on Public, on the Network page")
    );
    // The Remote RD door is not where pilots come in: it gives no address for a code.
    let network = rig.registry.network();
    let on_at = |port| gridfpv_server::network::DoorRequest {
        enabled: true,
        port: Some(port),
    };
    network
        .set(gridfpv_server::network::Door::Remote, on_at(8092))
        .unwrap();
    network.report(
        gridfpv_server::network::Door::Remote,
        gridfpv_server::network::Listener::Up(8092),
    );
    let remote_only = send(&rig, call(Method::GET, &uri)).await;
    assert_eq!(remote_only.status, StatusCode::CONFLICT);
    // The Public door is open. (Only checked where this computer has an address.)
    network
        .set(gridfpv_server::network::Door::Public, on_at(8090))
        .unwrap();
    network.report(
        gridfpv_server::network::Door::Public,
        gridfpv_server::network::Listener::Up(8090),
    );
    if gridfpv_server::network::local_addresses().is_empty() {
        return;
    }
    let on = send(&rig, call(Method::GET, &uri)).await;
    assert_eq!(on.status, StatusCode::OK);
    assert_eq!(on.headers[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(on.headers[header::CACHE_CONTROL], "no-store");
    assert!(
        on.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert!(on.body.contains("<svg"));
    assert!(!on.body.contains("<script"));
    // The list now gives the address the code's link starts with.
    let listed = codes(&rig).await;
    assert!(listed["address"].as_str().unwrap().starts_with("http://"));
    assert!(listed["address"].as_str().unwrap().ends_with(":8090/"));
    // Another pilot's picture is another picture.
    let bee = send(
        &rig,
        call(
            Method::GET,
            &format!("/events/{}/access/{}/qr", rig.event, rig.bee),
        ),
    )
    .await;
    assert_ne!(bee.body, on.body);
}

#[tokio::test]
async fn another_website_cannot_sign_a_browser_in_or_out() {
    let rig = rig().await;
    let code = code_of(&rig, &rig.ace).await;
    let forged = send(&rig, sign_in(&code).header("origin", "http://evil.example")).await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    assert!(forged.set_cookie().is_none());
    let session = send(&rig, sign_in(&code)).await.session();
    let out = send(
        &rig,
        call(Method::DELETE, "/public/pilot/session")
            .from(PHONE)
            .pilot(&session)
            .header("origin", "http://evil.example"),
    )
    .await;
    assert_eq!(out.status, StatusCode::FORBIDDEN);
    assert_eq!(
        pilot_of(&rig, PHONE, &session).await.as_deref(),
        Some("Ace")
    );
}

#[tokio::test]
async fn a_made_up_sign_in_is_nobody() {
    let rig = rig().await;
    code_of(&rig, &rig.ace).await;
    for session in [
        "",
        "x",
        "null",
        "undefined",
        &rig.ace,
        "gridfpv_pilot",
        "a=b",
    ] {
        assert_eq!(pilot_of(&rig, PHONE, session).await, None, "{session:?}");
    }
}

#[tokio::test]
async fn the_grid_computer_can_sign_in_as_a_pilot_too() {
    // A kiosk is a browser on the Grid computer itself.
    let rig = rig().await;
    let code = code_of(&rig, &rig.bee).await;
    let answer = send(
        &rig,
        call(Method::POST, "/public/pilot/session").json(json!({"code": code})),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(
        pilot_of(&rig, HERE, &answer.session()).await.as_deref(),
        Some("Bee")
    );
}
