//! What a visitor is told about the open event (docs/primitives.md §11.2).
//!
//! The spectator routes give out the console's own shapes with the director's things taken
//! out. These tests plant a marked value in every place a director can put one, check that
//! the director still sees each of them, and then read everything a visitor can read and
//! look for any of them.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_server::events::EventRegistry;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Every planted value holds this, so one search finds any of them.
const MARK: &str = "ZZSECRET";

struct Rig {
    registry: EventRegistry,
    app: Router,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn rig() -> Rig {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gridfpv-spectator-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<title>console</title>").unwrap();
    let registry = EventRegistry::new(None).unwrap();
    Rig {
        app: build_app(registry.clone(), &dir),
        registry,
        dir,
    }
}

/// Who is asking.
#[derive(Clone, Copy, PartialEq)]
enum From {
    /// The Grid computer: the director.
    Here,
    /// A phone at the field.
    Phone,
}

async fn send(
    rig: &Rig,
    from: From,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let (peer, host) = match from {
        From::Here => ("127.0.0.1:50000", "127.0.0.1:8080"),
        From::Phone => ("192.168.1.77:50000", "192.168.1.5:8081"),
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
    let response = rig.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let streaming = response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|kind| kind.to_str().unwrap_or_default().contains("event-stream"));
    if streaming {
        return (status, String::new());
    }
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A write by the director that must work. Gives back the answer as JSON.
async fn direct(rig: &Rig, method: Method, uri: &str, body: Value) -> Value {
    let (status, text) = send(rig, From::Here, method.clone(), uri, Some(body.clone())).await;
    assert!(
        status.is_success(),
        "{method} {uri} {body} answered {status}: {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

async fn visit(rig: &Rig, uri: &str) -> (StatusCode, String) {
    send(rig, From::Phone, Method::GET, uri, None).await
}

async fn visit_json(rig: &Rig, uri: &str) -> Value {
    let (status, text) = visit(rig, uri).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {text}");
    serde_json::from_str(&text).unwrap()
}

fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("an id").to_string()
}

/// A race day with a marked value everywhere one can be put.
struct Day {
    event: String,
    other_event: String,
}

async fn race_day(rig: &Rig) -> Day {
    // Pilots: one racing, one who is only in the director's list.
    let ace = id_of(
        &direct(
            rig,
            Method::POST,
            "/pilots",
            json!({
                "callsign": "Ace",
                "name": format!("{MARK}-realname"),
                "phonetic": format!("{MARK}-phonetic"),
                "team": format!("{MARK}-team"),
                "color": "#FF0000",
                "country": "US",
                "multigp_id": format!("{MARK}-multigp"),
                "velocidrone_id": format!("{MARK}-velocidrone")
            }),
        )
        .await,
    );
    let bee = id_of(&direct(rig, Method::POST, "/pilots", json!({"callsign": "Bee"})).await);
    direct(
        rig,
        Method::POST,
        "/pilots",
        json!({"callsign": format!("{MARK}-bystander")}),
    )
    .await;

    // Classes: one in the event, one not.
    let open = id_of(
        &direct(
            rig,
            Method::POST,
            "/classes",
            json!({
                "name": "Open",
                "source": "Custom",
                "reference": format!("{MARK}-classref"),
                "description": format!("{MARK}-classnote")
            }),
        )
        .await,
    );
    direct(
        rig,
        Method::POST,
        "/classes",
        json!({"name": format!("{MARK}-unusedclass"), "source": "Custom"}),
    )
    .await;

    // Timers: one a heat is flown on, one spare.
    let gate = id_of(
        &direct(
            rig,
            Method::POST,
            "/timers",
            json!({
                "name": "Gate",
                "kind": "Oft",
                "address": format!("{}-host.example:7270", MARK.to_lowercase()),
                "own_id": format!("{MARK}-ownid")
            }),
        )
        .await,
    );
    direct(
        rig,
        Method::POST,
        "/timers",
        json!({"name": format!("{MARK}-sparetimer"), "kind": "Oft", "address": "10.9.9.9:7270"}),
    )
    .await;

    // Two events. Only the second is opened.
    let other_event = id_of(
        &direct(
            rig,
            Method::POST,
            "/events",
            json!({"name": format!("{MARK}-otherevent")}),
        )
        .await,
    );
    let event = id_of(
        &direct(
            rig,
            Method::POST,
            "/events",
            json!({
                "name": "Friday Night Whoops",
                "location": format!("{MARK}-location"),
                "description": format!("{MARK}-eventnote"),
                "organizer": format!("{MARK}-organizer")
            }),
        )
        .await,
    );
    direct(rig, Method::PUT, "/active-event", json!({"id": event})).await;

    let commands = format!("/events/{event}/commands");
    let record = |fact: Value| json!({"Record": fact});
    direct(
        rig,
        Method::POST,
        &commands,
        record(json!({"RosterSet": {"class": open, "pilots": [ace, bee]}})),
    )
    .await;
    let format = json!({
        "name": "Open",
        "race": {"end": "Open", "staging_micros": 0, "first_lap": "HoleShot"},
        "completion": "DirectorDecides"
    });
    // A heat on the bench, where crossings can be put in by hand, and one on the timer.
    direct(
        rig,
        Method::POST,
        &commands,
        record(json!({"PhaseDefined": {"def": {
            "id": "heat-1",
            "label": "Qualifying Heat 1",
            "kind": {"Heat": {"lineup": [
                {"adapter": "bench", "seat": "seat-1", "source": {"Pilot": ace}},
                {"adapter": "bench", "seat": "seat-2", "source": {"Pilot": bee}}
            ]}},
            "format": format
        }}})),
    )
    .await;
    direct(
        rig,
        Method::POST,
        &commands,
        record(json!({"PhaseDefined": {"def": {
            "id": "heat-2",
            "label": "Qualifying Heat 2",
            "kind": {"Heat": {"lineup": [
                {"adapter": gate, "seat": "0", "source": {"Pilot": ace}}
            ]}},
            "format": format
        }}})),
    )
    .await;

    // Race 1: flown, saved with a note, with a video, a removed lap, a false start and a protest.
    direct(
        rig,
        Method::POST,
        &commands,
        json!({"StartRace": {"heat": "heat-1"}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    let passes = format!("/events/{event}/passes");
    let mut at = now_micros();
    for _ in 0..4 {
        for seat in ["seat-1", "seat-2"] {
            direct(
                rig,
                Method::POST,
                &passes,
                json!({"adapter": "bench", "seat": seat, "at": at}),
            )
            .await;
        }
        at += 3_000_000;
    }
    direct(
        rig,
        Method::POST,
        &commands,
        json!({"StopRace": {"race": "heat-1#1"}}),
    )
    .await;
    direct(
        rig,
        Method::POST,
        &commands,
        json!({"SaveRace": {
            "race": "heat-1#1",
            "needs_marshal": true,
            "note": format!("{MARK}-racenote")
        }}),
    )
    .await;
    // The Director's recorder logs this itself; nothing over the API may.
    let recorded: gridfpv_events::Event = serde_json::from_value(json!({"VideoRecorded": {
        "race": "heat-1#1",
        "file": format!("C:\\{MARK}-videos\\race.mp4"),
        "first_frame_at": at - 20_000_000,
        "clean": true,
        "overlay_file": format!("C:\\{MARK}-videos\\overlay.mp4"),
        "overlay_first_frame_at": at - 19_900_000,
        "laps_key": format!("{MARK}-lapskey")
    }}))
    .unwrap();
    gridfpv_server::video::log_video(
        &rig.registry,
        &gridfpv_server::events::EventId(event.clone()),
        recorded,
    );
    let state = director_state(rig, &event).await;
    let crossing = state["races"][0]["seats"][0]["laps"][1]["crossing"].clone();
    direct(
        rig,
        Method::POST,
        &commands,
        record(json!({"LapRemoved": {
            "race": "heat-1#1",
            "crossing": crossing,
            "reason": "FalseDetection",
            "note": format!("{MARK}-lapnote")
        }})),
    )
    .await;
    direct(
        rig,
        Method::POST,
        &commands,
        record(json!({"FalseStart": {"race": "heat-1#1", "pilot": bee}})),
    )
    .await;
    direct(
        rig,
        Method::POST,
        &commands,
        json!({"FileProtest": {
            "race": "heat-1#1",
            "by": bee,
            "note": format!("{MARK}-protest")
        }}),
    )
    .await;

    // Race 2: started and thrown away, with a note.
    direct(
        rig,
        Method::POST,
        &commands,
        json!({"StartRace": {"heat": "heat-1"}}),
    )
    .await;
    direct(
        rig,
        Method::POST,
        &commands,
        json!({"DiscardRace": {
            "race": "heat-1#2",
            "reason": "Other",
            "note": format!("{MARK}-discardnote")
        }}),
    )
    .await;

    Day { event, other_event }
}

fn now_micros() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros(),
    )
    .unwrap()
}

async fn director_state(rig: &Rig, event: &str) -> Value {
    let (status, text) = send(
        rig,
        From::Here,
        Method::GET,
        &format!("/events/{event}/state"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    serde_json::from_str(&text).unwrap()
}

/// Everything a visitor can read, as one text.
async fn everything_a_visitor_reads(rig: &Rig) -> String {
    let mut all = String::new();
    for uri in [
        "/public/hello",
        "/public/event",
        "/public/event/state",
        "/public/event/pilots",
        "/public/event/classes",
        "/public/event/timers",
        "/health",
        "/about",
        "/time",
        "/",
    ] {
        let (status, text) = visit(rig, uri).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}: {text}");
        all.push_str(&text);
        all.push('\n');
    }
    all
}

/// Every marked value in a text, shortest first, for a readable failure.
fn marks_in(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let needle = MARK.to_lowercase();
    let mut found: Vec<String> = lower
        .match_indices(&needle)
        .map(|(at, _)| {
            lower[at..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-')
                .collect()
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

#[tokio::test]
async fn the_director_sees_every_planted_value() {
    // Without this, the test below could pass because nothing was planted.
    let rig = rig();
    let day = race_day(&rig).await;
    let mut seen = director_state(&rig, &day.event).await.to_string();
    for uri in ["/pilots", "/classes", "/timers", "/events"] {
        seen.push_str(&send(&rig, From::Here, Method::GET, uri, None).await.1);
    }
    let found = marks_in(&seen);
    for planted in [
        "realname",
        "phonetic",
        "team",
        "multigp",
        "velocidrone",
        "bystander",
        "classref",
        "classnote",
        "unusedclass",
        "host",
        "ownid",
        "sparetimer",
        "otherevent",
        "location",
        "eventnote",
        "organizer",
        "racenote",
        "videos",
        "protest",
    ] {
        let wanted = format!("{}-{planted}", MARK.to_lowercase());
        assert!(
            found.iter().any(|mark| mark.starts_with(&wanted)),
            "{wanted} was not planted where the director can see it. Found: {found:?}"
        );
    }
}

#[tokio::test]
async fn a_visitor_is_told_none_of_them() {
    let rig = rig();
    race_day(&rig).await;
    let all = everything_a_visitor_reads(&rig).await;
    assert_eq!(marks_in(&all), Vec::<String>::new());
}

#[tokio::test]
async fn a_visitor_still_gets_what_a_spectator_needs() {
    let rig = rig();
    race_day(&rig).await;

    let event = visit_json(&rig, "/public/event").await;
    assert_eq!(event["event"]["name"], "Friday Night Whoops");

    let state = visit_json(&rig, "/public/event/state").await;
    let phases: Vec<&str> = state["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|phase| phase["def"]["label"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["Qualifying Heat 1", "Qualifying Heat 2"]);
    let races = state["races"].as_array().unwrap();
    assert_eq!(races.len(), 2);
    assert_eq!(races[0]["state"], "Saved");
    assert_eq!(races[1]["state"], "Discarded");
    // The laps are there, with the removed one folded in as the director ruled.
    let ace = &races[0]["seats"][0];
    assert_eq!(ace["laps"].as_array().unwrap().len(), 2);
    assert!(ace["hole_shot"].is_object());
    assert!(ace["best_lap_micros"].is_number());
    assert!(ace["finish"].is_number());
    // A false start is a ruling everyone at the field knows of.
    assert_eq!(races[0]["seats"][1]["false_start"], true);
    assert!(state["cursor"].as_u64().unwrap() > 10);
    assert_eq!(state["rosters"][0]["pilots"].as_array().unwrap().len(), 2);

    let pilots = visit_json(&rig, "/public/event/pilots").await;
    let mut callsigns: Vec<&str> = pilots
        .as_array()
        .unwrap()
        .iter()
        .map(|pilot| pilot["callsign"].as_str().unwrap())
        .collect();
    callsigns.sort_unstable();
    assert_eq!(callsigns, ["Ace", "Bee"]);
    let ace = pilots
        .as_array()
        .unwrap()
        .iter()
        .find(|pilot| pilot["callsign"] == "Ace")
        .unwrap();
    // A pilot is a handle, a callsign and a colour. Nothing else.
    let mut keys: Vec<&String> = ace.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["callsign", "color", "id", "vtx_types"]);
    assert_eq!(ace["vtx_types"], json!([]));

    let classes = visit_json(&rig, "/public/event/classes").await;
    assert_eq!(classes.as_array().unwrap().len(), 1);
    assert_eq!(classes[0]["name"], "Open");

    let timers = visit_json(&rig, "/public/event/timers").await;
    assert_eq!(timers.as_array().unwrap().len(), 1);
    assert_eq!(timers[0]["timer"]["name"], "Gate");
    assert_eq!(timers[0]["timer"]["address"], "");
}

#[tokio::test]
async fn the_visitors_snapshot_has_no_field_the_directors_does_not() {
    // The same shape, with things taken out: never something of its own.
    fn keys(value: &Value, path: &str, into: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    let here = format!("{path}.{key}");
                    into.push(here.clone());
                    keys(inner, &here, into);
                }
            }
            Value::Array(items) => {
                for inner in items {
                    keys(inner, &format!("{path}[]"), into);
                }
            }
            _ => {}
        }
    }
    let rig = rig();
    let day = race_day(&rig).await;
    let mut theirs = Vec::new();
    keys(
        &visit_json(&rig, "/public/event/state").await,
        "",
        &mut theirs,
    );
    let mut ours = Vec::new();
    keys(&director_state(&rig, &day.event).await, "", &mut ours);
    for key in &theirs {
        assert!(
            ours.contains(key),
            "{key} is in the visitor's snapshot only"
        );
    }
    // And these are gone from every race and seat.
    for gone in [".races[].video", ".races[].note"] {
        assert!(ours.iter().any(|key| key == gone), "{gone} was planted");
        assert!(
            !theirs.iter().any(|key| key.starts_with(gone)),
            "{gone} leaked"
        );
    }
    let state = visit_json(&rig, "/public/event/state").await;
    assert_eq!(state["protests"], json!([]));
    assert_eq!(state["proposed_disqualifications"], json!([]));
    for race in state["races"].as_array().unwrap() {
        assert_eq!(race["needs_marshal"], false);
        for seat in race["seats"].as_array().unwrap() {
            assert_eq!(seat["uncounted"], json!([]));
        }
    }
}

#[tokio::test]
async fn with_no_event_open_there_is_nothing_to_read_and_it_says_so() {
    let rig = rig();
    let event = visit_json(&rig, "/public/event").await;
    assert_eq!(event, json!({}));
    for uri in [
        "/public/event/state",
        "/public/event/changes",
        "/public/event/pilots",
        "/public/event/classes",
        "/public/event/timers",
    ] {
        let (status, text) = visit(&rig, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "GET {uri}");
        let error: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(error["code"], "UnknownScope");
        assert_eq!(error["message"], "No event is open.");
    }
    // An event that exists but is not open is the same to a visitor.
    direct(
        &rig,
        Method::POST,
        "/events",
        json!({"name": format!("{MARK}-closed")}),
    )
    .await;
    assert_eq!(visit_json(&rig, "/public/event").await, json!({}));
    assert_eq!(
        visit(&rig, "/public/event/state").await.0,
        StatusCode::NOT_FOUND
    );
    assert!(marks_in(&visit(&rig, "/public/hello").await.1).is_empty());
}

#[tokio::test]
async fn a_visitor_follows_the_open_event_and_cannot_ask_for_another() {
    let rig = rig();
    let day = race_day(&rig).await;
    // There is no way to name an event: the id in a query or a path is not looked at.
    for uri in [
        format!("/public/event/state?event={}", day.other_event),
        format!("/public/event?id={}", day.other_event),
        format!("/public/event/pilots?event={}", day.other_event),
    ] {
        let (status, text) = visit(&rig, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert!(marks_in(&text).is_empty(), "{uri}: {text}");
    }
    for uri in [
        format!("/public/event/{}", day.other_event),
        format!("/public/event/{}/state", day.other_event),
        format!("/public/events/{}/state", day.other_event),
        "/public/events".to_string(),
    ] {
        assert_eq!(visit(&rig, &uri).await.0, StatusCode::NOT_FOUND, "{uri}");
    }
    // The console's own routes stay shut, for the open event too.
    for uri in [
        format!("/events/{}/state", day.event),
        format!("/events/{}/state", day.other_event),
        format!("/events/{}/signal?race=heat-1%231", day.event),
        format!("/events/{}/races/heat-1%231/video/recording", day.event),
    ] {
        assert_eq!(visit(&rig, &uri).await.0, StatusCode::UNAUTHORIZED, "{uri}");
    }

    // The director opens the other event: the visitor's reads follow it.
    let before = visit_json(&rig, "/public/event").await["event"]["id"].clone();
    direct(
        &rig,
        Method::PUT,
        "/active-event",
        json!({"id": day.other_event}),
    )
    .await;
    let after = visit_json(&rig, "/public/event").await;
    assert_ne!(after["event"]["id"], before);
    let state = visit_json(&rig, "/public/event/state").await;
    assert_eq!(state["races"], json!([]));
    assert_eq!(visit_json(&rig, "/public/event/pilots").await, json!([]));
    assert_eq!(visit_json(&rig, "/public/event/timers").await, json!([]));
}

#[tokio::test]
async fn the_change_feed_is_open_to_a_visitor_and_carries_only_a_number() {
    let rig = rig();
    race_day(&rig).await;
    let (status, _) = visit(&rig, "/public/event/changes").await;
    assert_eq!(status, StatusCode::OK);
    // Read the first event off the stream by hand.
    let mut request = Request::builder()
        .uri("/public/event/changes")
        .header(header::HOST, "192.168.1.5:8081")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "192.168.1.77:50000".parse::<SocketAddr>().unwrap(),
    ));
    let response = rig.app.clone().oneshot(request).await.unwrap();
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
        .await
        .expect("the feed sent its first event")
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(frame.data_ref().unwrap()).into_owned();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("event: cursor"));
    let data = lines.next().unwrap().strip_prefix("data: ").unwrap();
    assert!(data.parse::<u64>().unwrap() > 10, "{text}");
}

#[tokio::test]
async fn a_visitor_cannot_change_anything_through_the_public_routes() {
    let rig = rig();
    let day = race_day(&rig).await;
    let before = director_state(&rig, &day.event).await["cursor"].clone();
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        for uri in [
            "/public/event",
            "/public/event/state",
            "/public/event/changes",
            "/public/event/pilots",
            "/public/event/classes",
            "/public/event/timers",
            "/public/hello",
        ] {
            let (status, _) = send(
                &rig,
                From::Phone,
                method.clone(),
                uri,
                Some(json!({"StartRace": {"heat": "heat-2"}})),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        }
    }
    assert_eq!(director_state(&rig, &day.event).await["cursor"], before);
}
