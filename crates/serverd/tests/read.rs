//! What a server shows of an event it holds.
//!
//! The strongest thing to check is the rule itself (§11.7): the server shows what the
//! Director's own Public door shows. So these tests run a whole Director, race an event on
//! it, push that event here, and compare the two, answer by answer. The Director's side is
//! planted with values a visitor must never see, and **everything the Director knows is
//! pushed**, private fields and all, to prove it is the server that leaves them out.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::ClubId;
use gridfpv_storage::Chain;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Every planted value holds this, so one search finds any of them.
const MARK: &str = "ZZSECRET";

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64
}

// ── The Director ────────────────────────────────────────────────────────────

struct Director {
    registry: EventRegistry,
    app: Router,
    dir: PathBuf,
}

impl Drop for Director {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn director() -> Director {
    let dir = std::env::temp_dir().join(format!("gridfpv-serverd-read-{}", now_micros()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<title>console</title>").unwrap();
    let registry = EventRegistry::new(None).unwrap();
    Director {
        app: build_app(registry.clone(), &dir),
        registry,
        dir,
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

async fn director_get(d: &Director, uri: &str) -> Value {
    let (status, value) = ask(d, true, Method::GET, uri, None).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {value}");
    value
}

/// What the Director's Public door gives a phone.
async fn visitor_get(d: &Director, uri: &str) -> Value {
    let (status, value) = ask(d, false, Method::GET, uri, None).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {value}");
    value
}

fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("an id").to_owned()
}

/// Race an event, with a marked value in every place a director can put one. Returns the
/// event's id. With `finish` false the race is left running.
async fn race_day(d: &Director, finish: bool) -> String {
    let ace = id_of(
        &direct(
            d,
            Method::POST,
            "/pilots",
            json!({
                "callsign": "Ace",
                "name": format!("{MARK}-realname"),
                "team": format!("{MARK}-team"),
                "color": "#FF0000",
                "multigp_id": format!("{MARK}-multigp")
            }),
        )
        .await,
    );
    let bee = id_of(&direct(d, Method::POST, "/pilots", json!({"callsign": "Bee"})).await);
    direct(
        d,
        Method::POST,
        "/pilots",
        json!({"callsign": format!("{MARK}-bystander")}),
    )
    .await;
    let open = id_of(
        &direct(
            d,
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
        d,
        Method::POST,
        "/classes",
        json!({"name": format!("{MARK}-unusedclass"), "source": "Custom"}),
    )
    .await;
    let gate = id_of(
        &direct(
            d,
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
        d,
        Method::POST,
        "/timers",
        json!({"name": format!("{MARK}-sparetimer"), "kind": "Oft", "address": "10.9.9.9:7270"}),
    )
    .await;

    let event = id_of(
        &direct(
            d,
            Method::POST,
            "/events",
            json!({
                "name": "Friday Night Whoops",
                "date": "2026-11-27",
                "location": "Main field",
                "description": "Whoops only. Bring spare props.",
                "organizer": "Recon FPV"
            }),
        )
        .await,
    );
    direct(d, Method::PUT, "/active-event", json!({"id": event})).await;

    let commands = format!("/events/{event}/commands");
    let record = |fact: Value| json!({"Record": fact});
    direct(
        d,
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
    direct(
        d,
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
    direct(d, Method::POST, &commands, record(json!({"PhaseDefined": {"def": {
        "id": "heat-2",
        "label": "Qualifying Heat 2",
        "kind": {"Heat": {"lineup": [{"adapter": gate, "seat": "0", "source": {"Pilot": ace}}]}},
        "format": format
    }}}))).await;

    direct(
        d,
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
                d,
                Method::POST,
                &passes,
                json!({"adapter": "bench", "seat": seat, "at": at}),
            )
            .await;
        }
        at += 3_000_000;
    }
    if !finish {
        return event;
    }
    direct(
        d,
        Method::POST,
        &commands,
        json!({"StopRace": {"race": "heat-1#1"}}),
    )
    .await;
    direct(
        d,
        Method::POST,
        &commands,
        json!({"SaveRace": {
            "race": "heat-1#1",
            "needs_marshal": true,
            "note": format!("{MARK}-racenote")
        }}),
    )
    .await;
    let recorded: gridfpv_events::Event = serde_json::from_value(json!({"VideoRecorded": {
        "race": "heat-1#1",
        "file": format!("C:\\{MARK}-videos\\race.mp4"),
        "first_frame_at": at - 20_000_000,
        "clean": true,
        "laps_key": format!("{MARK}-lapskey")
    }}))
    .unwrap();
    gridfpv_server::video::log_video(&d.registry, &EventId(event.clone()), recorded);
    direct(
        d,
        Method::POST,
        &commands,
        json!({"FileProtest": {
            "race": "heat-1#1",
            "by": bee,
            "note": format!("{MARK}-protest")
        }}),
    )
    .await;
    event
}

// ── The server ──────────────────────────────────────────────────────────────

struct Server {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    token: String,
}

fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state
        .book
        .new_token(&ClubId("club".into()), "laptop")
        .unwrap()
        .1;
    Server {
        app: router(state.clone()),
        state,
        token,
        _dir: dir,
    }
}

async fn call(s: &Server, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {}", s.token));
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = s.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The body of a log push, with each entry's text exactly as the Director holds it. Built
/// as text: going through `serde_json::Value` would re-order an entry's keys, and the
/// server would then hold other text than the Director's chain was worked out from.
fn log_body(
    entries: &[(String, Option<i64>)],
    from: usize,
    before: Chain,
    sent_at: Option<i64>,
) -> String {
    let list: Vec<String> = entries
        .iter()
        .map(|(json, at)| match at {
            Some(at) => format!("{{\"recorded_at\":{at},\"event\":{json}}}"),
            None => format!("{{\"event\":{json}}}"),
        })
        .collect();
    let sent = sent_at.map_or(String::new(), |at| format!(",\"sent_at\":{at}"));
    format!(
        "{{\"from\":{from},\"before\":\"{}\",\"entries\":[{}]{sent}}}",
        before.to_hex(),
        list.join(",")
    )
}

async fn call_text(s: &Server, method: Method, path: &str, body: String) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {}", s.token))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = s.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Read from the server with no token, as anyone may.
async fn read(s: &Server, path: &str) -> (StatusCode, Value) {
    let request = Request::builder().uri(path).body(Body::empty()).unwrap();
    let response = s.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn read_ok(s: &Server, path: &str) -> Value {
    let (status, value) = read(s, path).await;
    assert_eq!(status, StatusCode::OK, "GET {path}: {value}");
    value
}

/// The Director's log from `from` on, as the entries of a push.
fn log_entries(d: &Director, event: &str, from: u64) -> Vec<(String, Option<i64>)> {
    let log = d.registry.log(&EventId(event.to_owned())).unwrap();
    let log = log.lock().unwrap();
    log.read_from(from)
        .unwrap()
        .into_iter()
        .map(|stored| {
            (
                serde_json::to_string(&stored.event).unwrap(),
                stored.recorded_at,
            )
        })
        .collect()
}

/// Push everything the Director holds about an event: its log from where the server's copy
/// ends, and a header with the Director's **own** lists, nothing left out.
async fn push_all(d: &Director, s: &Server, event: &str) {
    let all = log_entries(d, event, 0);
    let held = s
        .state
        .store
        .get(&ClubId("club".into()), &EventId(event.to_owned()))
        .unwrap()
        .map_or(0, |copy| copy.held().len) as usize;
    let before = all[..held]
        .iter()
        .fold(Chain::start(), |c, (j, at)| c.then(j, *at));
    let body = log_body(&all[held..], held, before, Some(now_micros()));
    let (status, value) =
        call_text(s, Method::POST, &format!("/push/events/{event}/log"), body).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    // The server holds the Director's log, to the letter.
    assert_eq!(value["len"], all.len());
    let chain = all.iter().fold(Chain::start(), |c, (j, at)| c.then(j, *at));
    assert_eq!(value["chain"], chain.to_hex());

    let events = director_get(d, "/events").await;
    let meta = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == event)
        .unwrap()
        .clone();
    // The channels a Director would send: one per seat of each timer.
    let header = json!({
        "meta": meta,
        "pilots": director_get(d, "/pilots").await,
        "classes": director_get(d, "/classes").await,
        "timers": director_get(d, "/timers").await,
        "channels": [
            {"adapter": "bench", "seat": "seat-1", "freq_mhz": 5658},
            {"adapter": format!("{MARK}-not-in-the-event"), "seat": "0", "freq_mhz": 5800}
        ]
    });
    assert!(
        header.to_string().contains(MARK),
        "the header carries the director's own things"
    );
    let (status, value) = call(
        s,
        Method::PUT,
        &format!("/push/events/{event}"),
        Some(header),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

fn base(event: &str) -> String {
    format!("/api/clubs/club/events/{event}/public")
}

/// A snapshot with the clock taken out: the two ends are asked at different instants.
fn without_now(mut view: Value) -> Value {
    view.as_object_mut().unwrap().remove("now");
    view
}

fn sorted_by_id(list: Value) -> Vec<Value> {
    let mut list = list.as_array().unwrap().clone();
    list.sort_by_key(|item| item.to_string());
    list
}

// ── The tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_server_shows_what_the_directors_public_door_shows() {
    let d = director();
    let s = server();
    let event = race_day(&d, true).await;
    push_all(&d, &s, &event).await;
    let base = base(&event);

    let ours = read_ok(&s, &format!("{base}/event/state")).await;
    let theirs = visitor_get(&d, "/public/event/state").await;
    assert!(ours["races"].as_array().unwrap().len() == 1, "{ours}");
    assert!(
        !ours["races"][0]["seats"][0]["laps"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(without_now(ours), without_now(theirs));

    for list in ["pilots", "classes"] {
        let ours = sorted_by_id(read_ok(&s, &format!("{base}/event/{list}")).await);
        let theirs = sorted_by_id(visitor_get(&d, &format!("/public/event/{list}")).await);
        assert!(!ours.is_empty(), "{list}");
        assert_eq!(ours, theirs, "{list}");
    }
    // The timer's live status is the Director's to know at that moment; who it is and its
    // seats are the same.
    let ours = read_ok(&s, &format!("{base}/event/timers")).await;
    let theirs = visitor_get(&d, "/public/event/timers").await;
    assert_eq!(ours.as_array().unwrap().len(), 1);
    assert_eq!(ours[0]["timer"], theirs[0]["timer"]);

    let named = read_ok(&s, &format!("{base}/event")).await;
    assert_eq!(named, visitor_get(&d, "/public/event").await);
    assert_eq!(named["event"]["name"], "Friday Night Whoops");
}

#[tokio::test]
async fn nothing_the_director_keeps_from_visitors_can_be_read_here() {
    let d = director();
    let s = server();
    let event = race_day(&d, true).await;
    push_all(&d, &s, &event).await;
    let base = base(&event);

    // The marks did reach the server: they are in what it stores.
    let copy = s
        .state
        .store
        .get(&ClubId("club".into()), &EventId(event.clone()))
        .unwrap()
        .unwrap();
    let stored: String = copy
        .read_raw_from(0)
        .unwrap()
        .into_iter()
        .map(|e| e.json)
        .collect();
    assert!(stored.contains(&format!("{MARK}-racenote")));
    assert!(
        copy.header()
            .unwrap()
            .unwrap()
            .contains(&format!("{MARK}-realname"))
    );

    for path in [
        "/api/clubs".to_owned(),
        "/api/clubs/club/events".to_owned(),
        format!("{base}/hello"),
        format!("{base}/event"),
        format!("{base}/event/state"),
        format!("{base}/event/pilots"),
        format!("{base}/event/classes"),
        format!("{base}/event/timers"),
        format!("{base}/event/channels"),
    ] {
        let text = read_ok(&s, &path).await.to_string();
        assert!(
            !text.contains(MARK),
            "{path} gives out a marked value: {text}"
        );
        assert!(
            !text.to_lowercase().contains(&MARK.to_lowercase()),
            "{path}: {text}"
        );
    }
    // And each list holds only what the event refers to.
    let pilots = read_ok(&s, &format!("{base}/event/pilots")).await;
    let callsigns: Vec<&str> = pilots
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["callsign"].as_str().unwrap())
        .collect();
    assert_eq!(callsigns.len(), 2);
    assert!(callsigns.contains(&"Ace") && callsigns.contains(&"Bee"));
    let channels = read_ok(&s, &format!("{base}/event/channels")).await;
    assert_eq!(
        channels,
        json!([{"adapter": "bench", "seat": "seat-1", "freq_mhz": 5658}])
    );
}

#[tokio::test]
async fn the_server_follows_the_event_as_more_is_pushed() {
    let d = director();
    let s = server();
    let event = race_day(&d, false).await;
    push_all(&d, &s, &event).await;
    let base = base(&event);

    let running = read_ok(&s, &format!("{base}/event/state")).await;
    assert_eq!(
        without_now(running.clone()),
        without_now(visitor_get(&d, "/public/event/state").await)
    );
    assert_eq!(running["current_race"], "heat-1#1");

    // The race is stopped and saved on the Director. The server knows nothing of it yet.
    let commands = format!("/events/{event}/commands");
    direct(
        &d,
        Method::POST,
        &commands,
        json!({"StopRace": {"race": "heat-1#1"}}),
    )
    .await;
    direct(
        &d,
        Method::POST,
        &commands,
        json!({"SaveRace": {"race": "heat-1#1", "needs_marshal": false}}),
    )
    .await;
    let stale = read_ok(&s, &format!("{base}/event/state")).await;
    assert_eq!(stale["cursor"], running["cursor"]);
    assert_eq!(stale["current_race"], "heat-1#1");

    // Pushed, and the two agree again.
    push_all(&d, &s, &event).await;
    let saved = read_ok(&s, &format!("{base}/event/state")).await;
    assert!(saved["cursor"].as_u64() > running["cursor"].as_u64());
    assert!(saved.get("current_race").is_none(), "{saved}");
    assert_eq!(
        without_now(saved),
        without_now(visitor_get(&d, "/public/event/state").await)
    );
}

#[tokio::test]
async fn the_clock_a_page_is_given_is_the_directors() {
    let d = director();
    let s = server();
    let event = race_day(&d, false).await;
    push_all(&d, &s, &event).await;
    let base = base(&event);

    // Pushed with the Director's own time: the two clocks agree to within the test's run.
    let now = read_ok(&s, &format!("{base}/event/state")).await["now"]
        .as_i64()
        .unwrap();
    assert!((now - now_micros()).abs() < 5_000_000, "{now}");

    // A Director whose clock is an hour ahead says so with its next batch.
    let held = s
        .state
        .store
        .get(&ClubId("club".into()), &EventId(event.clone()))
        .unwrap()
        .unwrap()
        .held();
    let hour = 3_600_000_000i64;
    let (status, _) = call(&s, Method::POST, &format!("/push/events/{event}/log"), Some(json!({
        "from": held.len, "before": held.chain.to_hex(), "entries": [], "sent_at": now_micros() + hour
    }))).await;
    assert_eq!(status, StatusCode::OK);
    let ahead = read_ok(&s, &format!("{base}/event/state")).await["now"]
        .as_i64()
        .unwrap();
    assert!((ahead - now_micros() - hour).abs() < 5_000_000, "{ahead}");

    // A batch that is refused does not move the clock: it may be from some other Director.
    let (status, _) = call(&s, Method::POST, &format!("/push/events/{event}/log"), Some(json!({
        "from": held.len, "before": Chain::start().to_hex(), "entries": [], "sent_at": now_micros() - hour
    }))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let still = read_ok(&s, &format!("{base}/event/state")).await["now"]
        .as_i64()
        .unwrap();
    assert!((still - now_micros() - hour).abs() < 5_000_000, "{still}");

    // And it is remembered when the copy is opened again.
    let again = AppState::open(s._dir.path()).unwrap();
    let copy = again
        .store
        .get(&ClubId("club".into()), &EventId(event.clone()))
        .unwrap()
        .unwrap();
    assert!((copy.director_now(0) - hour).abs() < 5_000_000);
}

#[tokio::test]
async fn an_event_is_listed_once_its_header_has_arrived() {
    let d = director();
    let s = server();
    let event = race_day(&d, true).await;

    assert_eq!(read_ok(&s, "/api/clubs/club/events").await, json!([]));

    // The log alone: held, but with no name to show it by.
    let body = log_body(&log_entries(&d, &event, 0), 0, Chain::start(), None);
    let (status, _) = call_text(&s, Method::POST, &format!("/push/events/{event}/log"), body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read_ok(&s, "/api/clubs/club/events").await, json!([]));
    for path in [
        "hello",
        "event",
        "event/state",
        "event/pilots",
        "event/changes",
    ] {
        let (status, body) = read(&s, &format!("{}/{path}", base(&event))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body["code"], "UnknownScope");
    }

    push_all(&d, &s, &event).await;
    let list = read_ok(&s, "/api/clubs/club/events").await;
    assert_eq!(
        list,
        json!([{
            "id": event,
            "name": "Friday Night Whoops",
            "date": "2026-11-27",
            "location": "Main field",
            "description": "Whoops only. Bring spare props.",
            "organizer": "Recon FPV",
            "created_at": list[0]["created_at"],
            "hidden": false
        }])
    );
    assert!(list[0]["created_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn events_are_listed_newest_first() {
    let s = server();
    for (id, name, created) in [
        ("old", "Old", 100),
        ("new", "New", 300),
        ("mid", "Mid", 200),
        ("also-mid", "Also mid", 200),
    ] {
        let (status, _) = call(
            &s,
            Method::PUT,
            &format!("/push/events/{id}"),
            Some(json!({
                "meta": {"id": id, "name": name, "created_at": created, "persistent": true}
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let list = read_ok(&s, "/api/clubs/club/events").await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["new", "also-mid", "mid", "old"]);
    // No date or place given: the keys are not there at all.
    assert!(list[0].get("date").is_none() && list[0].get("location").is_none());
    assert!(list[0].get("description").is_none() && list[0].get("organizer").is_none());
}

#[tokio::test]
async fn an_event_with_a_header_and_no_log_yet_reads_as_an_empty_event() {
    let s = server();
    call(
        &s,
        Method::PUT,
        "/push/events/e",
        Some(json!({
            "meta": {"id": "e", "name": "Not started", "created_at": 1, "persistent": true}
        })),
    )
    .await;
    let view = read_ok(&s, &format!("{}/event/state", base("e"))).await;
    assert_eq!(view["cursor"], 0);
    assert_eq!(view["races"], json!([]));
    for list in ["pilots", "classes", "timers", "channels"] {
        assert_eq!(
            read_ok(&s, &format!("{}/event/{list}", base("e"))).await,
            json!([])
        );
    }
    let hello = read_ok(&s, &format!("{}/hello", base("e"))).await;
    assert_eq!(hello["caller"], "Visitor");
    assert_eq!(hello["director_sign_in"], false);
    assert_eq!(hello["event"], "Not started");
}

#[tokio::test]
async fn what_is_not_there_is_not_found() {
    let s = server();
    call(
        &s,
        Method::PUT,
        "/push/events/e",
        Some(json!({
            "meta": {"id": "e", "name": "E", "created_at": 1, "persistent": true}
        })),
    )
    .await;
    for path in [
        "/api/clubs/nope/events",
        "/api/clubs/nope/events/e/public/event/state",
        "/api/clubs/club/events/nope/public/event/state",
        "/api/clubs/club/events/..%2f..%2fserver/public/event",
        "/api/clubs/..%2fclub/events/e/public/event",
        "/api/clubs/club/events/E/public/event",
    ] {
        let (status, body) = read(&s, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
    }
    // The clubs are listed.
    assert_eq!(
        read_ok(&s, "/api/clubs").await,
        json!([{"id": "club", "name": "My club", "word": "my-club", "archived": false}])
    );
}

#[tokio::test]
async fn reading_needs_no_token_and_cannot_change_anything() {
    let s = server();
    call(
        &s,
        Method::PUT,
        "/push/events/e",
        Some(json!({
            "meta": {"id": "e", "name": "E", "created_at": 1, "persistent": true}
        })),
    )
    .await;
    for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
        for path in [
            "/api/clubs",
            "/api/clubs/club/events",
            "/api/clubs/club/events/e/public/event/state",
        ] {
            let request = Request::builder()
                .method(method.clone())
                .uri(path)
                .body(Body::empty())
                .unwrap();
            let status = s.app.clone().oneshot(request).await.unwrap().status();
            // An owner makes a club with a POST there (§13.2). Nobody else does.
            let want = if method == Method::POST && path == "/api/clubs" {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::METHOD_NOT_ALLOWED
            };
            assert_eq!(status, want, "{method} {path}");
        }
    }
    assert_eq!(
        read_ok(&s, "/api/clubs/club/events")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        read_ok(&s, "/api/clubs").await.as_array().unwrap().len(),
        1,
        "no club was made"
    );
}

/// Read server-sent events off a response until `count` `cursor` values have come.
async fn cursors(body: &mut Body, count: usize) -> Vec<u64> {
    let mut text = String::new();
    let mut found = Vec::new();
    while found.len() < count {
        let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
            .await
            .expect("a cursor within five seconds")
            .expect("the feed is still open")
            .unwrap();
        if let Some(data) = frame.data_ref() {
            text.push_str(&String::from_utf8_lossy(data));
        }
        found = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|n| n.trim().parse().unwrap())
            .collect();
    }
    found
}

#[tokio::test]
async fn a_page_following_an_event_hears_each_push() {
    let d = director();
    let s = server();
    let event = race_day(&d, false).await;
    push_all(&d, &s, &event).await;
    let first = log_entries(&d, &event, 0).len() as u64;

    let request = Request::builder()
        .uri(format!("{}/event/changes", base(&event)))
        .body(Body::empty())
        .unwrap();
    let response = s.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("event-stream")
    );
    let mut body = response.into_body();
    // At once, where the copy stands.
    assert_eq!(cursors(&mut body, 1).await, vec![first]);

    let commands = format!("/events/{event}/commands");
    direct(
        &d,
        Method::POST,
        &commands,
        json!({"StopRace": {"race": "heat-1#1"}}),
    )
    .await;
    push_all(&d, &s, &event).await;
    // The log and the header arrive close together, and may be heard as one change or two.
    // Either way the next thing heard is where the copy now stands.
    let heard = cursors(&mut body, 1).await;
    assert!(heard[0] > first, "{heard:?}");
    assert_eq!(heard[0], log_entries(&d, &event, 0).len() as u64);
}

#[tokio::test]
async fn the_feed_ends_when_the_copy_is_removed() {
    let s = server();
    call(
        &s,
        Method::PUT,
        "/push/events/e",
        Some(json!({
            "meta": {"id": "e", "name": "E", "created_at": 1, "persistent": true}
        })),
    )
    .await;
    let request = Request::builder()
        .uri(format!("{}/event/changes", base("e")))
        .body(Body::empty())
        .unwrap();
    let mut body = s.app.clone().oneshot(request).await.unwrap().into_body();
    assert_eq!(cursors(&mut body, 1).await, vec![0]);

    let (status, _) = call(&s, Method::DELETE, "/push/events/e", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = body.frame().await {
            if frame.is_err() {
                break;
            }
        }
    })
    .await;
    assert!(end.is_ok(), "the feed went on after its event was removed");
}
