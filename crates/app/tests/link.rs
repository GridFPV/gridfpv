//! The Director pushing to a remote server (docs/primitives.md §13.4), against a real one.
//!
//! Each test starts the server program's own router on a port of this computer, runs a
//! Director with its data in a folder, and pushes over real HTTP.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_app::link::{Client, Memory, PushError, spawn, sync_all};
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::link::{ClubSettings, LinkEventState, LinkState, LinkTrust};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::ClubId;
use gridfpv_storage::Chain;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const MARK: &str = "ZZSECRET";

// ── The server ──────────────────────────────────────────────────────────────

struct Server {
    dir: tempfile::TempDir,
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

async fn serve(state: AppState, addr: &str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, task)
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state
        .book
        .new_token(&ClubId("club".into()), "laptop")
        .unwrap()
        .1;
    let (addr, task) = serve(state.clone(), "127.0.0.1:0").await;
    Server {
        dir,
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

    /// What the server holds of an event: how many entries, and each one's stored text.
    fn held(&self, event: &str) -> Option<Vec<(String, Option<i64>)>> {
        let copy = self
            .state
            .store
            .get(&ClubId("club".into()), &EventId(event.to_owned()))
            .unwrap()?;
        Some(
            copy.read_raw_from(0)
                .unwrap()
                .into_iter()
                .map(|e| (e.json, e.recorded_at))
                .collect(),
        )
    }

    fn header(&self, event: &str) -> Option<Value> {
        let copy = self
            .state
            .store
            .get(&ClubId("club".into()), &EventId(event.to_owned()))
            .unwrap()?;
        copy.header()
            .unwrap()
            .map(|json| serde_json::from_str(&json).unwrap())
    }

    async fn read(&self, path: &str) -> (StatusCode, Value) {
        let request = Request::builder().uri(path).body(Body::empty()).unwrap();
        let response = router(self.state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

// ── The Director ────────────────────────────────────────────────────────────

struct Director {
    dir: tempfile::TempDir,
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
        dir,
    }
}

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

fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("an id").to_owned()
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64
}

/// An event with two pilots in a heat on the bench, and a race started. Private things are
/// marked. Returns the event's id.
async fn event(d: &Director, name: &str) -> String {
    let ace = id_of(
        &direct(
            d,
            Method::POST,
            "/pilots",
            json!({
                "callsign": format!("Ace of {name}"),
                "name": format!("{MARK}-realname"),
                "team": format!("{MARK}-team")
            }),
        )
        .await,
    );
    let bee = id_of(
        &direct(
            d,
            Method::POST,
            "/pilots",
            json!({"callsign": format!("Bee of {name}")}),
        )
        .await,
    );
    direct(
        d,
        Method::POST,
        "/pilots",
        json!({"callsign": format!("{MARK}-bystander")}),
    )
    .await;
    let class = id_of(
        &direct(
            d,
            Method::POST,
            "/classes",
            json!({
                "name": format!("Open {name}"),
                "source": "Custom",
                "description": format!("{MARK}-classnote")
            }),
        )
        .await,
    );
    let event = id_of(
        &direct(
            d,
            Method::POST,
            "/events",
            json!({
                "name": name,
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
            "id": "heat-1",
            "label": "Heat 1",
            "kind": {"Heat": {"lineup": [
                {"adapter": "bench", "seat": "seat-1", "source": {"Pilot": ace}},
                {"adapter": "bench", "seat": "seat-2", "source": {"Pilot": bee}}
            ]}},
            "format": {
                "name": "Open",
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
        json!({"StartRace": {"heat": "heat-1"}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    event
}

/// Record `count` crossings on the bench.
async fn passes(d: &Director, event: &str, count: usize) {
    let mut at = now_micros();
    for i in 0..count {
        let seat = if i % 2 == 0 { "seat-1" } else { "seat-2" };
        direct(
            d,
            Method::POST,
            &format!("/events/{event}/passes"),
            json!({"adapter": "bench", "seat": seat, "at": at}),
        )
        .await;
        at += 1_500_000;
    }
}

/// The Director's log, as stored.
fn log_of(d: &Director, event: &str) -> Vec<(String, Option<i64>)> {
    let log = d.registry.log(&EventId(event.to_owned())).unwrap();
    let log = log.lock().unwrap();
    log.read_raw_from(0)
        .unwrap()
        .into_iter()
        .map(|e| (e.json, e.recorded_at))
        .collect()
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

fn link(d: &Director, url: &str, token: &str, on: bool) {
    link_to(&d.registry, url, Some(token), on, LinkTrust::System);
}

fn client(d: &Director) -> Client {
    client_of(&d.registry)
}

fn view(d: &Director) -> gridfpv_server::link::LinkView {
    let open = d.registry.active().map(|event| event.id);
    d.registry.link().view(&d.registry.list(), open.as_ref())
}

fn status_of(d: &Director, event: &str) -> (u64, u64, LinkEventState) {
    let view = view(d);
    let found = view
        .events
        .into_iter()
        .find(|e| e.id.0 == event)
        .unwrap_or_else(|| panic!("no status for {event}"));
    (found.len, found.sent, found.state)
}

/// Wait until `check` holds, looking often. Fails with `what` if it never does.
async fn until(what: &str, seconds: u64, mut check: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !check() {
        assert!(tokio::time::Instant::now() < end, "never happened: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Someone who runs the club does `method` to an event on the server's page.
async fn on_the_page(s: &Server, method: Method, path: &str, body: Option<Value>) {
    use gridfpv_serverd::accounts::{InviteKind, NewInvite, NewSignIn};
    use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
    let now = now_ms();
    let (_, secret) = s
        .state
        .accounts
        .new_invite(
            NewInvite {
                kind: InviteKind::Owner,
                label: "owner".into(),
                max_uses: 1,
                expires_at: None,
            },
            None,
            now,
        )
        .unwrap();
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let subject = format!(
        "owner-{}",
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let session = s
        .state
        .accounts
        .join_with_invite(
            &secret,
            &NewSignIn {
                provider: "discord",
                subject: &subject,
                label: "Oz",
            },
            "Oz",
            now,
        )
        .unwrap()
        .2;
    let request = Request::builder()
        .method(method.clone())
        .uri(path)
        .header(header::COOKIE, format!("{SESSION_COOKIE}={session}"));
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = router(s.state.clone()).oneshot(request).await.unwrap();
    assert!(
        response.status().is_success(),
        "{method} {path}: {}",
        response.status()
    );
}

async fn remove_on_the_page(s: &Server, event: &str) {
    on_the_page(
        s,
        Method::DELETE,
        &format!("/api/clubs/club/events/{event}"),
        None,
    )
    .await;
}

async fn send_it_again(d: &Director, event: &str) {
    let (status, _) = ask(
        d,
        true,
        Method::POST,
        &format!("/server-link/events/{event}/replace"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ── One sync at a time ──────────────────────────────────────────────────────

#[tokio::test]
async fn one_sync_puts_the_whole_event_on_the_server() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    passes(&d, &event, 6).await;
    link(&d, &s.url(), &s.token, true);

    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();

    // The log, to the letter. (Sending may itself add a fact that fell due, so the
    // Director's log is read after.)
    let ours = log_of(&d, &event);
    assert!(ours.len() > 8);
    assert_eq!(s.held(&event).unwrap(), ours);
    assert_eq!(
        status_of(&d, &event),
        (ours.len() as u64, ours.len() as u64, LinkEventState::Sent)
    );

    // What the server shows is what the Director's own Public door shows.
    let base = format!("/api/clubs/club/events/{event}/public");
    let (status, mut theirs) = s.read(&format!("{base}/event/state")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, mut mine) = ask(&d, false, Method::GET, "/public/event/state", None).await;
    theirs.as_object_mut().unwrap().remove("now");
    mine.as_object_mut().unwrap().remove("now");
    assert_eq!(theirs, mine);
    let (_, pilots) = s.read(&format!("{base}/event/pilots")).await;
    let (_, mine) = ask(&d, false, Method::GET, "/public/event/pilots", None).await;
    assert_eq!(pilots.as_array().unwrap().len(), 2);
    assert_eq!(pilots, mine);
    let (_, list) = s.read("/api/clubs/club/events").await;
    assert_eq!(list[0]["name"], "Friday");
    assert_eq!(list[0]["date"], "2026-11-27");
}

#[tokio::test]
async fn what_the_director_keeps_from_visitors_does_not_leave_in_the_header() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();

    let header = s.header(&event).unwrap();
    let text = header.to_string();
    assert!(!text.contains(MARK), "{text}");
    // The names a page needs are there, and only for what the event refers to.
    assert_eq!(header["pilots"].as_array().unwrap().len(), 2);
    assert_eq!(header["classes"].as_array().unwrap().len(), 1);
    assert_eq!(header["meta"]["name"], "Friday");
    assert_eq!(header["meta"]["location"], "Main field");
    // The event's own details all go up.
    assert_eq!(
        header["meta"]["description"],
        "Whoops only. Bring spare props."
    );
    assert_eq!(header["meta"]["organizer"], "Recon FPV");
}

#[tokio::test]
async fn a_second_sync_sends_only_what_is_new() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();

    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let first = s.held(&event).unwrap();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), first, "nothing new: nothing sent");

    passes(&d, &event, 4).await;
    let (len, sent, state) = status_of(&d, &event);
    assert_eq!(
        (len, sent, state),
        (first.len() as u64, first.len() as u64, LinkEventState::Sent),
        "status is of the last sync"
    );
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let ours = log_of(&d, &event);
    assert_eq!(ours.len(), first.len() + 4);
    assert_eq!(s.held(&event).unwrap(), ours);
    assert_eq!(
        ours[..first.len()],
        first[..],
        "what was there was not touched"
    );
}

#[tokio::test]
async fn after_a_restart_the_director_asks_the_server_where_it_got_to() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    passes(&d, &event, 3).await;

    // A new client and nothing remembered, as after the Director is started again.
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));

    // And a whole new Director over the same data folder.
    let again = EventRegistry::new(Some(d.dir.path().join("data"))).unwrap();
    let remembered = again
        .link()
        .club_of(&EventId(event.clone()))
        .expect("the club, and that the event goes to it, are remembered");
    let client = Client::new(&remembered).unwrap();
    sync_all(&again, &client, &mut Memory::default())
        .await
        .unwrap();
    let log = again.log(&EventId(event.clone())).unwrap();
    let ours: Vec<_> = log
        .lock()
        .unwrap()
        .read_raw_from(0)
        .unwrap()
        .into_iter()
        .map(|e| (e.json, e.recorded_at))
        .collect();
    assert_eq!(s.held(&event).unwrap(), ours);
}

#[tokio::test]
async fn a_long_log_goes_in_several_batches_and_arrives_whole() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Long").await;
    passes(&d, &event, 1_150).await;
    link(&d, &s.url(), &s.token, true);
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    let ours = log_of(&d, &event);
    assert!(ours.len() > 1_150, "more than two batches of 500");
    assert_eq!(s.held(&event).unwrap(), ours);
    let chain = ours
        .iter()
        .fold(Chain::start(), |c, (j, at)| c.then(j, *at));
    let copy = s
        .state
        .store
        .get(&ClubId("club".into()), &EventId(event.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(copy.held().chain, chain);
}

#[tokio::test]
async fn every_event_is_pushed_and_one_in_trouble_does_not_hold_the_others() {
    let s = server().await;
    let d = director();
    let first = event(&d, "First").await;
    let second = event(&d, "Second").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&first).unwrap(), log_of(&d, &first));
    assert_eq!(s.held(&second).unwrap(), log_of(&d, &second));

    // The server's copy of the first is swapped for some other log under the same id.
    s.state
        .store
        .delete(&ClubId("club".into()), &EventId(first.clone()))
        .unwrap();
    let other = s
        .state
        .store
        .get_or_create(&ClubId("club".into()), &EventId(first.clone()))
        .unwrap()
        .unwrap();
    other
        .append(
            0,
            Chain::start(),
            &[(
                r#"{"AdapterConnected":{"adapter":"someone-else"}}"#.to_owned(),
                Some(1),
            )],
        )
        .unwrap()
        .unwrap();

    passes(&d, &first, 2).await;
    passes(&d, &second, 2).await;
    sync_all(&d.registry, &client, &mut Memory::default())
        .await
        .unwrap();

    assert_eq!(status_of(&d, &first).2, LinkEventState::Different);
    assert_eq!(
        s.held(&first).unwrap().len(),
        1,
        "the other log was left alone"
    );
    assert_eq!(
        s.held(&second).unwrap(),
        log_of(&d, &second),
        "the second still went up"
    );
    assert_eq!(status_of(&d, &second).2, LinkEventState::Sent);
}

#[tokio::test]
async fn a_different_event_on_the_server_is_replaced_only_when_the_director_says() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let id = EventId(event.clone());
    let club = ClubId("club".into());
    // The server already holds something else under this id.
    let other = s.state.store.get_or_create(&club, &id).unwrap().unwrap();
    other
        .append(
            0,
            Chain::start(),
            &[(r#"{"AdapterConnected":{"adapter":"old"}}"#.to_owned(), None)],
        )
        .unwrap()
        .unwrap();
    drop(other);

    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    for _ in 0..3 {
        sync_all(&d.registry, &client, &mut memory).await.unwrap();
        let (len, sent, state) = status_of(&d, &event);
        assert_eq!(state, LinkEventState::Different);
        assert_eq!(sent, 1);
        assert!(len > 1);
        assert_eq!(
            s.held(&event).unwrap().len(),
            1,
            "never overwritten on its own"
        );
    }

    let (status, _) = ask(
        &d,
        true,
        Method::POST,
        &format!("/server-link/events/{event}/replace"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);
    // Asked once: the next sync does not remove it again.
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
}

#[tokio::test]
async fn an_event_brought_back_shorter_is_a_different_event() {
    // The Director's data was put back from a backup taken earlier, and then raced on.
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let backup = log_of(&d, &event).len();
    passes(&d, &event, 5).await;
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    let on_server = s.held(&event).unwrap();

    // The same event as it was at the backup, on another Director, with other laps since.
    let d2 = director();
    let data = d2.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    {
        let from = gridfpv_storage::SqliteLog::open(
            d.dir.path().join("data").join(format!("{event}.sqlite")),
        )
        .unwrap();
        let mut to =
            gridfpv_storage::SqliteLog::open(data.join(format!("{event}.sqlite"))).unwrap();
        use gridfpv_storage::EventLog;
        let old = from.read_raw_range(0, backup as u64).unwrap();
        to.append_raw_batch(old.iter().map(|e| (e.json.as_str(), e.recorded_at)))
            .unwrap();
        to.set_meta("event_meta", &from.get_meta("event_meta").unwrap().unwrap())
            .unwrap();
        assert_eq!(to.len().unwrap(), backup as u64);
    }
    let restored = EventRegistry::new(Some(data)).unwrap();
    assert!(restored.meta_of(&EventId(event.clone())).is_some());
    let restored = Director {
        app: build_app(restored.clone(), d2.dir.path()),
        registry: restored,
        dir: d2.dir,
    };
    passes(&restored, &event, 2).await;
    link(&restored, &s.url(), &s.token, true);

    // Shorter than the server's copy: not the same log. Nothing is sent.
    sync_all(
        &restored.registry,
        &client(&restored),
        &mut Memory::default(),
    )
    .await
    .unwrap();
    assert_eq!(status_of(&restored, &event).2, LinkEventState::Different);
    assert_eq!(s.held(&event).unwrap(), on_server);

    // Raced on past the server's length: still not the same log.
    passes(&restored, &event, 10).await;
    assert!(log_of(&restored, &event).len() > on_server.len());
    sync_all(
        &restored.registry,
        &client(&restored),
        &mut Memory::default(),
    )
    .await
    .unwrap();
    assert_eq!(status_of(&restored, &event).2, LinkEventState::Different);
    assert_eq!(s.held(&event).unwrap(), on_server);
}

#[tokio::test]
async fn a_server_that_lost_its_copy_is_sent_it_again() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();

    s.state
        .store
        .delete(&ClubId("club".into()), &EventId(event.clone()))
        .unwrap();
    passes(&d, &event, 2).await;
    // The Director still thinks the server has the start. The server says otherwise, and
    // the event is marked as failed for this round, not as different.
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert!(matches!(
        status_of(&d, &event).2,
        LinkEventState::Failed { .. }
    ));
    // Next round it asks, and sends all of it.
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);
}

#[tokio::test]
async fn an_event_removed_on_the_server_is_not_put_back_until_the_director_says() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let other = self::event(&d, "Saturday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);

    remove_on_the_page(&s, &event).await;
    assert!(s.held(&event).is_none());

    // More is raced. The Director still thinks the server has the start, and tries.
    passes(&d, &event, 2).await;
    for round in 0..3 {
        sync_all(&d.registry, &client, &mut memory).await.unwrap();
        let (len, sent, state) = status_of(&d, &event);
        assert_eq!(state, LinkEventState::Removed, "round {round}");
        assert_eq!(sent, 0);
        assert_eq!(len, log_of(&d, &event).len() as u64);
        assert!(s.held(&event).is_none(), "put back in round {round}");
        assert!(s.header(&event).is_none());
    }
    // The other event is not held up by it.
    assert_eq!(status_of(&d, &other).2, LinkEventState::Sent);
    passes(&d, &other, 1).await;
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&other).unwrap(), log_of(&d, &other));

    send_it_again(&d, &event).await;
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);
    assert_eq!(s.header(&event).unwrap()["meta"]["name"], "Friday");
    // And it keeps up from there as any event does.
    passes(&d, &event, 1).await;
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
}

#[tokio::test]
async fn a_director_with_nothing_new_to_send_still_learns_its_event_was_removed() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();

    remove_on_the_page(&s, &event).await;
    // Before it is time to ask again, the Director has no way to know.
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);
    // When it asks what the server holds, it finds out.
    memory.check_now();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(status_of(&d, &event).2, LinkEventState::Removed);
    assert!(s.held(&event).is_none());
}

#[tokio::test]
async fn a_director_started_after_its_event_was_removed_does_not_send_it() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    sync_all(&d.registry, &client, &mut Memory::default())
        .await
        .unwrap();
    remove_on_the_page(&s, &event).await;

    // Nothing remembered: as after the Director is started again.
    let mut memory = Memory::default();
    for _ in 0..2 {
        sync_all(&d.registry, &client, &mut memory).await.unwrap();
        assert_eq!(status_of(&d, &event).2, LinkEventState::Removed);
        assert!(s.held(&event).is_none());
    }
}

#[tokio::test]
async fn a_new_name_alone_does_not_put_a_removed_event_back() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    remove_on_the_page(&s, &event).await;

    // Only the header changes: no entry is sent, so the header is what is refused.
    let pilot = gridfpv_server::link::header(&d.registry, &EventId(event.clone()))
        .await
        .unwrap()
        .pilots[0]
        .id
        .0
        .clone();
    let len = log_of(&d, &event).len();
    direct(
        &d,
        Method::PUT,
        &format!("/pilots/{pilot}"),
        json!({"callsign": "Renamed"}),
    )
    .await;
    assert_eq!(log_of(&d, &event).len(), len);
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(status_of(&d, &event).2, LinkEventState::Removed);
    assert!(s.held(&event).is_none());
    assert!(s.header(&event).is_none());
}

#[tokio::test]
async fn a_director_with_nothing_new_to_send_notices_the_server_lost_its_copy() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();

    // The copy is gone with nobody having removed it: a server put back from a backup.
    s.state
        .store
        .delete(&ClubId("club".into()), &EventId(event.clone()))
        .unwrap();
    memory.check_now();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
    assert_eq!(s.header(&event).unwrap()["meta"]["name"], "Friday");
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);
}

#[tokio::test]
async fn a_director_notices_a_copy_that_was_changed_under_it() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let id = EventId(event.clone());
    let club = ClubId("club".into());
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();

    // The server's copy is swapped for another log, as an old backup would be.
    s.state.store.delete(&club, &id).unwrap();
    let other = s.state.store.get_or_create(&club, &id).unwrap().unwrap();
    other
        .append(
            0,
            Chain::start(),
            &[(r#"{"AdapterConnected":{"adapter":"old"}}"#.to_owned(), None)],
        )
        .unwrap()
        .unwrap();
    drop(other);

    memory.check_now();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(status_of(&d, &event).2, LinkEventState::Different);
    assert_eq!(s.held(&event).unwrap().len(), 1, "never overwritten");
}

#[tokio::test]
async fn a_hidden_event_is_still_kept_up_to_date() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    on_the_page(
        &s,
        Method::PUT,
        &format!("/api/clubs/club/events/{event}/listing"),
        Some(json!({ "hidden": true })),
    )
    .await;
    passes(&d, &event, 2).await;
    memory.check_now();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
    assert_eq!(status_of(&d, &event).2, LinkEventState::Sent);
    let (_, list) = s.read("/api/clubs/club/events").await;
    assert_eq!(list, json!([]), "still hidden from the list");
}

/// A small source of numbers that is the same every run for the same seed.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: u64) -> u64 {
        // xorshift64*: plenty for picking what to do next.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) % sides
    }
}

/// Whether the server says this event was removed there.
fn removed_on_server(s: &Server, event: &str) -> bool {
    s.state
        .book
        .event_note(&ClubId("club".into()), event)
        .removed_at
        .is_some()
}

/// Many rounds of whatever can happen to an event, in an order picked by `seed`: laps,
/// a renamed pilot, the server losing its copy, someone removing or hiding the event on
/// the server's page, the director sending it again. After each round the Director and
/// the server must agree, with no step by hand that a real director would not take.
async fn mixed_changes(seed: u64, rounds: usize) {
    let s = server().await;
    let d = director();
    let names = ["Friday", "Saturday", "Sunday"];
    let mut events = Vec::new();
    for name in names {
        events.push(event(&d, name).await);
    }
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let club = ClubId("club".into());
    let mut story = Vec::new();

    for round in 0..rounds {
        // One to three things happen between two looks.
        for _ in 0..=dice.roll(3) {
            let which = dice.roll(events.len() as u64) as usize;
            let event = events[which].clone();
            let what = dice.roll(10);
            story.push(format!("round {round}: {} does {what}", names[which]));
            match what {
                // Racing goes on. The commonest thing by far.
                0..=3 => passes(&d, &event, 1 + dice.roll(4) as usize).await,
                // A pilot is renamed: the header changes and the log does not.
                4 => {
                    let header = gridfpv_server::link::header(&d.registry, &EventId(event.clone()))
                        .await
                        .unwrap();
                    let pilot = header.pilots[0].id.0.clone();
                    direct(
                        &d,
                        Method::PUT,
                        &format!("/pilots/{pilot}"),
                        json!({"callsign": format!("Ace {round}")}),
                    )
                    .await;
                }
                // The server loses its copy with nobody having removed it.
                5 => {
                    s.state
                        .store
                        .delete(&club, &EventId(event.clone()))
                        .unwrap();
                }
                // Someone who runs the club removes it on the page, if it is there.
                6 if s.held(&event).is_some() && s.header(&event).is_some() => {
                    remove_on_the_page(&s, &event).await;
                }
                // The director says to send it again, removed or not.
                7 => send_it_again(&d, &event).await,
                // Hidden, or shown again.
                8 if s.held(&event).is_some() => {
                    on_the_page(
                        &s,
                        Method::PUT,
                        &format!("/api/clubs/club/events/{event}/listing"),
                        Some(json!({ "hidden": dice.roll(2) == 0 })),
                    )
                    .await;
                }
                // Nothing at all.
                _ => {}
            }
        }

        // The Director looks until nothing is left to do. A real one looks every second;
        // a few looks is all it should ever need. One event in trouble is told on its
        // own row and tried again, so a look itself never fails here.
        let mut settled = false;
        for _ in 0..4 {
            memory.check_now();
            sync_all(&d.registry, &client, &mut memory)
                .await
                .unwrap_or_else(|why| panic!("seed {seed}: a look failed: {why}\n{story:#?}"));
            settled = events.iter().all(|event| {
                matches!(
                    status_of(&d, event).2,
                    LinkEventState::Sent | LinkEventState::Removed
                )
            });
            if settled {
                break;
            }
        }
        assert!(settled, "seed {seed}: never settled\n{story:#?}");

        for (event, name) in events.iter().zip(names) {
            let (len, sent, state) = status_of(&d, event);
            let ours = log_of(&d, event);
            assert_eq!(len, ours.len() as u64, "seed {seed}, {name}\n{story:#?}");
            if removed_on_server(&s, event) {
                // Removed stays removed until the director says otherwise.
                assert_eq!(
                    state,
                    LinkEventState::Removed,
                    "seed {seed}, {name}\n{story:#?}"
                );
                assert_eq!(sent, 0);
                assert!(
                    s.held(event).is_none(),
                    "seed {seed}: {name} was put back\n{story:#?}"
                );
            } else {
                assert_eq!(
                    state,
                    LinkEventState::Sent,
                    "seed {seed}, {name}\n{story:#?}"
                );
                // Entry for entry what the Director has, in the same order.
                assert_eq!(
                    s.held(event).unwrap(),
                    ours,
                    "seed {seed}: {name} differs on the server\n{story:#?}"
                );
                assert_eq!(sent, len);
                // And the header is the one the Director has now.
                let header = gridfpv_server::link::header(&d.registry, &EventId(event.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    s.header(event).unwrap()["pilots"],
                    serde_json::to_value(&header.pilots).unwrap(),
                    "seed {seed}: {name}'s header is stale\n{story:#?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn whatever_happens_in_whatever_order_the_server_ends_up_agreeing_with_the_director() {
    // A handful of orders on every run. `GRIDFPV_SOAK_SEEDS=500` tries many more by hand.
    let seeds = std::env::var("GRIDFPV_SOAK_SEEDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(6);
    for seed in 1..=seeds {
        mixed_changes(seed, 25).await;
    }
}

#[tokio::test]
async fn the_header_is_sent_again_when_a_name_in_it_changes() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let before = s.header(&event).unwrap();
    let pilot = before["pilots"][0]["id"].as_str().unwrap().to_owned();

    // A pilot is given another callsign: nothing is added to the log.
    let len = log_of(&d, &event).len();
    direct(
        &d,
        Method::PUT,
        &format!("/pilots/{pilot}"),
        json!({"callsign": "Renamed"}),
    )
    .await;
    assert_eq!(log_of(&d, &event).len(), len);
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let after = s.header(&event).unwrap();
    assert!(after["pilots"].to_string().contains("Renamed"), "{after}");

    // And the event's own name.
    direct(
        &d,
        Method::PUT,
        &format!("/events/{event}"),
        json!({"name": "Friday, renamed"}),
    )
    .await;
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.header(&event).unwrap()["meta"]["name"], "Friday, renamed");
    let (_, list) = s.read("/api/clubs/club/events").await;
    assert_eq!(list[0]["name"], "Friday, renamed");
}

#[tokio::test]
async fn an_event_removed_on_the_director_stays_on_the_server_and_leaves_the_status() {
    let s = server().await;
    let d = director();
    let first = event(&d, "First").await;
    let second = event(&d, "Second").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(view(&d).events.len(), 2);

    let (status, body) = ask(&d, true, Method::DELETE, &format!("/events/{first}"), None).await;
    assert!(status.is_success(), "{body}");
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let view = view(&d);
    assert_eq!(view.events.len(), 1);
    assert_eq!(view.events[0].id.0, second);
    // Pushing never removes anything from the server: the record of a raced event stays.
    assert!(s.held(&first).is_some());
}

#[tokio::test]
async fn a_token_the_server_does_not_take_stops_the_sync_and_says_so() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), "gfs_not-the-token", true);
    let client = client(&d);
    assert_eq!(client.hello().await.unwrap_err(), PushError::Token);
    assert_eq!(
        sync_all(&d.registry, &client, &mut Memory::default()).await,
        Err(PushError::Token)
    );
    assert!(s.held(&event).is_none());

    // Taken back while in use.
    link(&d, &s.url(), &s.token, true);
    let client = self::client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let id = s.state.book.tokens()[0].id.clone();
    s.state.book.revoke_token(&id).unwrap();
    passes(&d, &event, 1).await;
    assert_eq!(
        sync_all(&d.registry, &client, &mut memory).await,
        Err(PushError::Token)
    );
}

#[tokio::test]
async fn an_archived_club_takes_nothing_and_the_director_is_told_why_and_carries_on_when_it_is_back()
 {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    link(&d, &s.url(), &s.token, true);
    let client = client(&d);
    let mut memory = Memory::default();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    let before = s.held(&event).unwrap();

    // An owner archives the club (§13.2). The token is still right.
    let club = s.state.book.clubs()[0].clone();
    s.state.book.archive_club(&club.id, true).unwrap();
    passes(&d, &event, 2).await;
    let told = |err: PushError| match err {
        PushError::Closed(why) => why,
        other => panic!("the token was blamed, or only one event: {other:?}"),
    };
    let why = told(client.hello().await.unwrap_err());
    assert!(why.contains("is archived on this server"), "{why}");
    assert!(why.contains(&club.name), "{why}");
    let why = told(
        sync_all(&d.registry, &client, &mut memory)
            .await
            .unwrap_err(),
    );
    assert!(why.contains("is archived"), "{why}");
    assert_eq!(s.held(&event).unwrap(), before, "nothing new was taken");

    // Brought back, the same Director sends what it had waiting.
    s.state.book.archive_club(&club.id, false).unwrap();
    sync_all(&d.registry, &client, &mut memory).await.unwrap();
    assert_eq!(s.held(&event).unwrap(), log_of(&d, &event));
}

#[tokio::test]
async fn a_server_that_is_not_there_is_an_error_and_not_a_hang() {
    let d = director();
    event(&d, "Friday").await;
    // A port nothing listens on.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);
    link(&d, &format!("http://{dead}"), "gfs_x", true);
    let client = client(&d);
    let started = std::time::Instant::now();
    let err = sync_all(&d.registry, &client, &mut Memory::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PushError::Server(why) if why.starts_with("The server could not be reached")),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn something_that_is_not_a_grid_server_is_said_plainly() {
    // A web server that answers every path with a page.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let other = Router::new().fallback(|| async { "<html>hello</html>" });
    let task = tokio::spawn(async move { axum::serve(listener, other).await.unwrap() });
    let d = director();
    link(&d, &format!("http://{addr}"), "gfs_x", true);
    let err = client(&d).hello().await.unwrap_err();
    assert!(
        matches!(&err, PushError::Server(why) if why.contains("Is this a Grid server?")),
        "{err:?}"
    );
    task.abort();
}

// ── The settings, over the Director's API ───────────────────────────────────

/// Save a club over the Director's API. Returns its id.
async fn save_club(d: &Director, url: &str, token: &str) -> String {
    let view = direct(
        d,
        Method::POST,
        "/server-link/clubs",
        json!({"url": url, "token": token}),
    )
    .await;
    view["clubs"]
        .as_array()
        .unwrap()
        .last()
        .expect("the club was saved")["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Say over the Director's API which club an event goes to.
async fn send_to(d: &Director, event: &str, club: Option<&str>) -> Value {
    direct(
        d,
        Method::PUT,
        &format!("/server-link/events/{event}/club"),
        json!({"club": club}),
    )
    .await
}

#[tokio::test]
async fn clubs_are_saved_by_the_director_and_closed_to_visitors() {
    let d = director();
    let event = event(&d, "Friday").await;
    let settings = json!({"url": "https://grid.example.org/", "token": "gfs_secretsecret"});

    for (method, path, body) in [
        (Method::GET, "/server-link".to_owned(), None),
        (
            Method::POST,
            "/server-link/clubs".to_owned(),
            Some(settings.clone()),
        ),
        (
            Method::PUT,
            "/server-link/clubs/x".to_owned(),
            Some(settings.clone()),
        ),
        (Method::DELETE, "/server-link/clubs/x".to_owned(), None),
        (
            Method::PUT,
            format!("/server-link/events/{event}/club"),
            Some(json!({"club": null})),
        ),
        (
            Method::POST,
            format!("/server-link/events/{event}/replace"),
            Some(json!({})),
        ),
    ] {
        let (status, _) = ask(&d, false, method.clone(), &path, body).await;
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
            "{method} {path}: {status}"
        );
    }
    assert!(view(&d).clubs.is_empty(), "a visitor changed nothing");

    let saved = direct(&d, Method::POST, "/server-link/clubs", settings).await;
    let club = saved["clubs"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(saved["clubs"][0]["url"], "https://grid.example.org");
    assert_eq!(saved["clubs"].as_array().unwrap().len(), 1);
    assert!(
        !saved.to_string().contains("secretsecret"),
        "the token is not given back"
    );
    // Saving a club sends nothing: the event goes to none until the director says.
    assert_eq!(saved["state"], "NotSet");
    assert_eq!(saved["open"], json!(event));
    assert!(saved.get("club").is_none());
    assert_eq!(saved["events"][0]["state"], "NoClub");
    let (_, read) = ask(&d, true, Method::GET, "/server-link", None).await;
    assert_eq!(read, saved);

    // Refused settings say why, and change nothing.
    for (method, path, bad, status, word) in [
        (
            Method::POST,
            "/server-link/clubs".to_owned(),
            json!({"url": "http://grid.example.org", "token": "gfs_abcabc"}),
            StatusCode::BAD_REQUEST,
            "https",
        ),
        (
            Method::POST,
            "/server-link/clubs".to_owned(),
            json!({"url": "https://grid.example.org", "token": "nope"}),
            StatusCode::BAD_REQUEST,
            "token",
        ),
        (
            Method::POST,
            "/server-link/clubs".to_owned(),
            json!({"url": "https://grid.example.org"}),
            StatusCode::BAD_REQUEST,
            "token",
        ),
        (
            Method::POST,
            "/server-link/clubs".to_owned(),
            json!({"url": "", "token": "gfs_abcabc"}),
            StatusCode::BAD_REQUEST,
            "address",
        ),
        // Another address needs its own token: the old one is not sent there.
        (
            Method::PUT,
            format!("/server-link/clubs/{club}"),
            json!({"url": "https://other.example.org"}),
            StatusCode::BAD_REQUEST,
            "token",
        ),
        (
            Method::PUT,
            "/server-link/clubs/nope".to_owned(),
            json!({"url": "https://grid.example.org", "token": "gfs_abcabc"}),
            StatusCode::NOT_FOUND,
            "not saved",
        ),
        (
            Method::PUT,
            format!("/server-link/events/{event}/club"),
            json!({"club": "nope"}),
            StatusCode::NOT_FOUND,
            "not saved",
        ),
        (
            Method::PUT,
            "/server-link/events/no-such-event/club".to_owned(),
            json!({"club": club}),
            StatusCode::NOT_FOUND,
            "no event",
        ),
    ] {
        let (got, body) = ask(&d, true, method.clone(), &path, Some(bad.clone())).await;
        assert_eq!(got, status, "{method} {path} {bad}");
        assert!(body["message"].as_str().unwrap().contains(word), "{body}");
    }
    let (status, _) = ask(&d, true, Method::DELETE, "/server-link/clubs/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, still) = ask(&d, true, Method::GET, "/server-link", None).await;
    assert_eq!(still, saved);

    // A name, and the same address with no token: the token is kept.
    let named = direct(
        &d,
        Method::PUT,
        &format!("/server-link/clubs/{club}"),
        json!({"name": "  Recon  ", "url": "https://grid.example.org"}),
    )
    .await;
    assert_eq!(named["clubs"][0]["name"], "Recon");
    assert_eq!(
        d.registry.link().stored().clubs[0].token,
        "gfs_secretsecret"
    );

    // The event goes to the club, and then to none again.
    let sent = send_to(&d, &event, Some(&club)).await;
    assert_eq!(sent["club"], json!(club));
    assert_eq!(sent["events"][0]["club"], json!(club));
    assert_eq!(sent["state"], "Connecting");
    let none = send_to(&d, &event, None).await;
    assert!(none.get("club").is_none());
    assert_eq!(none["events"][0]["state"], "NoClub");
    assert_eq!(none["state"], "NotSet");

    // Forgetting a club leaves its events going nowhere.
    send_to(&d, &event, Some(&club)).await;
    let gone = direct(
        &d,
        Method::DELETE,
        &format!("/server-link/clubs/{club}"),
        Value::Null,
    )
    .await;
    assert_eq!(gone["clubs"], json!([]));
    assert_eq!(gone["events"][0]["state"], "NoClub");
    assert!(d.registry.link().stored().events.is_empty());

    let (status, _) = ask(
        &d,
        true,
        Method::POST,
        "/server-link/events/no-such-event/replace",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── The task, left to run ───────────────────────────────────────────────────

#[tokio::test]
async fn left_running_it_connects_pushes_and_keeps_up_with_a_race() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let task = spawn(d.registry.clone());

    // Nothing set: nothing happens.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(view(&d).state, LinkState::NotSet);
    assert!(s.held(&event).is_none());

    // A club is saved, and the event goes to none: still nothing.
    let club = save_club(&d, &s.url(), &s.token).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(view(&d).state, LinkState::NotSet);
    assert!(s.held(&event).is_none());
    assert!(
        !s.state.asks.listening(&ClubId("club".into())),
        "nothing is connected to a club no open event goes to"
    );

    // The event goes to the club: it goes up.
    send_to(&d, &event, Some(&club)).await;
    until("the event is on the server", 10, || {
        s.held(&event)
            .is_some_and(|held| held == log_of(&d, &event))
    })
    .await;
    until("the link says it is connected", 10, || {
        view(&d).state == LinkState::Connected
    })
    .await;
    let seen = view(&d);
    assert_eq!(seen.club.as_deref(), Some(club.as_str()));
    // The server said which club the token is for, and it is kept with the saved club.
    assert_eq!(seen.clubs[0].club.as_ref().unwrap().name, "My club");
    assert_eq!(
        d.registry.link().stored().clubs[0]
            .club
            .as_ref()
            .unwrap()
            .name,
        "My club"
    );
    assert!(seen.server_version.is_some());

    // A lap goes up at once: the open event's own change wakes the task, it does not wait
    // for its one-second look. Six laps, each waited for, take about six seconds if every
    // one waits for the look, and a fraction of a second if none does.
    let started = std::time::Instant::now();
    for _ in 0..6 {
        passes(&d, &event, 1).await;
        until("the lap is on the server", 5, || {
            s.held(&event).unwrap() == log_of(&d, &event)
        })
        .await;
    }
    // (The bound is loose on purpose: a busy build machine runs this ten times slower.)
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );

    // Sent to no club: a lap after that stays on the Director.
    send_to(&d, &event, None).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let before = s.held(&event).unwrap().len();
    passes(&d, &event, 2).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(s.held(&event).unwrap().len(), before);
    assert_eq!(view(&d).state, LinkState::NotSet);

    // To the club again: it catches up.
    send_to(&d, &event, Some(&club)).await;
    until("it caught up", 10, || {
        s.held(&event).unwrap() == log_of(&d, &event)
    })
    .await;
    task.abort();
}

#[tokio::test]
async fn left_running_it_says_when_the_server_is_away_and_picks_up_when_it_is_back() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let club = save_club(&d, &s.url(), &s.token).await;
    send_to(&d, &event, Some(&club)).await;
    let task = spawn(d.registry.clone());
    until("first push", 10, || {
        s.held(&event).is_some_and(|h| h == log_of(&d, &event))
    })
    .await;

    // The server goes away. Racing goes on.
    s.task.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    passes(&d, &event, 3).await;
    until("the link says it failed", 15, || {
        matches!(view(&d).state, LinkState::Failed { .. })
    })
    .await;
    // The Director is not held up by it.
    let (status, _) = ask(
        &d,
        true,
        Method::GET,
        &format!("/events/{event}/state"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The server comes back at the same address, with the same data.
    // (One state for the server and for looking: a copy is opened once per state.)
    let again = AppState::open(s.dir.path()).unwrap();
    let (addr, back) = serve(again.clone(), &s.addr.to_string()).await;
    assert_eq!(addr, s.addr);
    until("it caught up after the server came back", 20, || {
        let copy = again
            .store
            .get(&ClubId("club".into()), &EventId(event.clone()))
            .unwrap()
            .unwrap();
        copy.held().len as usize == log_of(&d, &event).len()
            && view(&d).state == LinkState::Connected
    })
    .await;
    task.abort();
    back.abort();
}

#[tokio::test]
async fn left_running_a_wrong_token_is_shown_and_a_right_one_fixes_it() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let task = spawn(d.registry.clone());
    let club = save_club(&d, &s.url(), "gfs_wrong").await;
    send_to(&d, &event, Some(&club)).await;
    until(
        "the link says the token was not taken",
        10,
        || matches!(view(&d).state, LinkState::Failed { ref message } if message.contains("token")),
    )
    .await;
    assert!(s.held(&event).is_none());
    // Saving the right token wakes it at once: no waiting out the retry.
    let started = std::time::Instant::now();
    direct(
        &d,
        Method::PUT,
        &format!("/server-link/clubs/{club}"),
        json!({"url": s.url(), "token": s.token}),
    )
    .await;
    until("pushed with the right token", 10, || {
        s.held(&event).is_some_and(|h| h == log_of(&d, &event))
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    task.abort();
}

// ── More than one club ──────────────────────────────────────────────────────

fn recon() -> ClubId {
    ClubId("club".into())
}

/// A second club on the server, and a link token for it.
fn north(s: &Server) -> (ClubId, String) {
    let club = s.state.book.new_club("North").unwrap();
    let token = s.state.book.new_token(&club.id, "laptop").unwrap().1;
    (club.id, token)
}

/// How many entries of an event a club on the server holds.
fn held_in(s: &Server, club: &ClubId, event: &str) -> Option<usize> {
    let copy = s
        .state
        .store
        .get(club, &EventId(event.to_owned()))
        .unwrap()?;
    Some(copy.read_raw_from(0).unwrap().len())
}

/// The director opens an event.
async fn open(d: &Director, event: &str) {
    direct(d, Method::PUT, "/active-event", json!({"id": event})).await;
}

fn state_of(d: &Director, event: &str) -> LinkEventState {
    status_of(d, event).2
}

#[tokio::test]
async fn one_look_sends_only_the_events_that_go_to_the_clients_club() {
    let s = server().await;
    let (north_id, north_token) = north(&s);
    let d = director();
    let mine = event(&d, "Recon night").await;
    let theirs = event(&d, "North night").await;
    let nobodys = event(&d, "Garage").await;
    let to_recon = save_club(&d, &s.url(), &s.token).await;
    let to_north = save_club(&d, &s.url(), &north_token).await;
    send_to(&d, &mine, Some(&to_recon)).await;
    send_to(&d, &theirs, Some(&to_north)).await;

    let desk = d.registry.link();
    let club = desk.club_of(&EventId(mine.clone())).unwrap();
    desk.begin(&club);
    let client = Client::new(&club).unwrap();
    sync_all(&d.registry, &client, &mut Memory::default())
        .await
        .unwrap();

    assert_eq!(held_in(&s, &recon(), &mine), Some(log_of(&d, &mine).len()));
    for (club, event) in [
        (&recon(), &theirs),
        (&recon(), &nobodys),
        (&north_id, &mine),
        (&north_id, &theirs),
        (&north_id, &nobodys),
    ] {
        assert_eq!(held_in(&s, club, event), None, "{club:?} {event}");
    }
    assert_eq!(state_of(&d, &mine), LinkEventState::Sent);
    assert_eq!(state_of(&d, &theirs), LinkEventState::Waiting);
    assert_eq!(state_of(&d, &nobodys), LinkEventState::NoClub);
    // Neither club learned of a pilot who flew only for the other.
    let pilots = |club: &ClubId| -> Vec<String> {
        s.state
            .accounts
            .club_pilots(club)
            .unwrap()
            .into_iter()
            .map(|pilot| pilot.callsign)
            .collect()
    };
    let recons = pilots(&recon());
    assert_eq!(recons.len(), 2, "{recons:?}");
    assert!(recons.iter().all(|c| c.ends_with("of Recon night")));
    assert_eq!(pilots(&north_id), Vec::<String>::new());
}

#[tokio::test]
async fn left_running_only_the_open_events_club_is_connected() {
    let s = server().await;
    let (north_id, north_token) = north(&s);
    let d = director();
    let mine = event(&d, "Recon night").await;
    let also_mine = event(&d, "Recon league").await;
    let theirs = event(&d, "North night").await;
    let to_recon = save_club(&d, &s.url(), &s.token).await;
    let to_north = save_club(&d, &s.url(), &north_token).await;
    send_to(&d, &mine, Some(&to_recon)).await;
    send_to(&d, &also_mine, Some(&to_recon)).await;
    send_to(&d, &theirs, Some(&to_north)).await;
    open(&d, &mine).await;
    let task = spawn(d.registry.clone());

    // Recon's event is open. Both of Recon's events go up: one connection serves the club.
    until("Recon's events are on the server", 10, || {
        held_in(&s, &recon(), &mine) == Some(log_of(&d, &mine).len())
            && held_in(&s, &recon(), &also_mine) == Some(log_of(&d, &also_mine).len())
            && view(&d).state == LinkState::Connected
    })
    .await;
    until("the Director listens to Recon", 10, || {
        s.state.asks.listening(&recon())
    })
    .await;
    // North is not talked to at all.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(held_in(&s, &north_id, &theirs), None);
    assert!(!s.state.asks.listening(&north_id));
    assert_eq!(state_of(&d, &theirs), LinkEventState::Waiting);
    let seen = view(&d);
    assert_eq!(seen.club.as_deref(), Some(to_recon.as_str()));
    assert_eq!(seen.open.as_ref().unwrap().0, mine);

    // The director opens North's event. Grid goes over to North, and lets go of Recon.
    open(&d, &theirs).await;
    until("North's event is on the server", 10, || {
        held_in(&s, &north_id, &theirs) == Some(log_of(&d, &theirs).len())
            && view(&d).state == LinkState::Connected
            && view(&d).club.as_deref() == Some(to_north.as_str())
    })
    .await;
    until("the Director listens to North and not to Recon", 20, || {
        s.state.asks.listening(&north_id) && !s.state.asks.listening(&recon())
    })
    .await;
    assert_eq!(state_of(&d, &theirs), LinkEventState::Sent);
    assert_eq!(state_of(&d, &mine), LinkEventState::Waiting);
    // Each club's name came from the server, and is kept with its saved club.
    let names: Vec<_> = view(&d)
        .clubs
        .iter()
        .map(|club| club.club.as_ref().unwrap().name.clone())
        .collect();
    assert_eq!(names, ["My club", "North"]);

    // Something changes in Recon's event while North's is open. It waits.
    let before = held_in(&s, &recon(), &mine).unwrap();
    passes(&d, &mine, 2).await;
    passes(&d, &theirs, 1).await;
    until("North's lap is on the server", 10, || {
        held_in(&s, &north_id, &theirs) == Some(log_of(&d, &theirs).len())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(held_in(&s, &recon(), &mine), Some(before));
    // Nothing of one club ever reached the other.
    assert_eq!(held_in(&s, &north_id, &mine), None);
    assert_eq!(held_in(&s, &north_id, &also_mine), None);
    assert_eq!(held_in(&s, &recon(), &theirs), None);

    // Recon's other event is opened: Recon is reached again, and both its events catch up.
    open(&d, &also_mine).await;
    until("Recon caught up", 10, || {
        held_in(&s, &recon(), &mine) == Some(log_of(&d, &mine).len())
    })
    .await;
    until("the Director listens to Recon and not to North", 20, || {
        s.state.asks.listening(&recon()) && !s.state.asks.listening(&north_id)
    })
    .await;
    task.abort();
}

#[tokio::test]
async fn left_running_two_servers_are_each_reached_only_for_their_own_event() {
    let a = server().await;
    let b = server().await;
    let d = director();
    let at_a = event(&d, "At A").await;
    let at_b = event(&d, "At B").await;
    let to_a = save_club(&d, &a.url(), &a.token).await;
    let to_b = save_club(&d, &b.url(), &b.token).await;
    send_to(&d, &at_a, Some(&to_a)).await;
    send_to(&d, &at_b, Some(&to_b)).await;
    open(&d, &at_a).await;
    let task = spawn(d.registry.clone());

    until("A has its event", 10, || {
        a.held(&at_a).is_some_and(|h| h == log_of(&d, &at_a))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(b.held(&at_b).is_none() && b.held(&at_a).is_none());
    assert!(!b.state.asks.listening(&recon()));

    open(&d, &at_b).await;
    until("B has its event", 10, || {
        b.held(&at_b).is_some_and(|h| h == log_of(&d, &at_b))
    })
    .await;
    assert!(a.held(&at_b).is_none() && b.held(&at_a).is_none());
    until("A is let go of", 20, || !a.state.asks.listening(&recon())).await;
    // A's token was never shown to B, nor B's to A: each server took its own.
    assert_eq!(view(&d).state, LinkState::Connected);
    task.abort();
}

#[tokio::test]
async fn left_running_a_club_that_is_away_does_not_hold_up_going_over_to_another() {
    let s = server().await;
    let d = director();
    let stuck = event(&d, "Stuck").await;
    let fine = event(&d, "Fine").await;
    // A server that is not there.
    let gone = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let to_gone = save_club(&d, &gone, "gfs_nobodyhome").await;
    let to_s = save_club(&d, &s.url(), &s.token).await;
    send_to(&d, &stuck, Some(&to_gone)).await;
    send_to(&d, &fine, Some(&to_s)).await;
    open(&d, &stuck).await;
    let task = spawn(d.registry.clone());
    until("the link says the server is away", 15, || {
        matches!(view(&d).state, LinkState::Failed { .. })
    })
    .await;

    // The director opens the other event. Grid does not wait out its retry of the first.
    let started = std::time::Instant::now();
    open(&d, &fine).await;
    until("the other club has its event", 10, || {
        s.held(&fine).is_some_and(|h| h == log_of(&d, &fine))
            && view(&d).state == LinkState::Connected
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    task.abort();
}

#[tokio::test]
async fn left_running_an_event_moved_to_another_club_goes_up_there_and_stays_where_it_was() {
    let s = server().await;
    let (north_id, north_token) = north(&s);
    let d = director();
    let event = event(&d, "Friday").await;
    let to_recon = save_club(&d, &s.url(), &s.token).await;
    let to_north = save_club(&d, &s.url(), &north_token).await;
    send_to(&d, &event, Some(&to_recon)).await;
    let task = spawn(d.registry.clone());
    until("it is in Recon", 10, || {
        held_in(&s, &recon(), &event) == Some(log_of(&d, &event).len())
    })
    .await;
    let in_recon = held_in(&s, &recon(), &event).unwrap();

    send_to(&d, &event, Some(&to_north)).await;
    until("it is in North", 10, || {
        held_in(&s, &north_id, &event) == Some(log_of(&d, &event).len())
    })
    .await;
    passes(&d, &event, 2).await;
    until("North keeps up", 10, || {
        held_in(&s, &north_id, &event) == Some(log_of(&d, &event).len())
    })
    .await;
    // Recon's copy is not removed, and takes nothing more.
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert_eq!(held_in(&s, &recon(), &event), Some(in_recon));
    task.abort();
}

#[tokio::test]
async fn left_running_forgetting_the_club_stops_the_sending() {
    let s = server().await;
    let d = director();
    let event = event(&d, "Friday").await;
    let club = save_club(&d, &s.url(), &s.token).await;
    send_to(&d, &event, Some(&club)).await;
    let task = spawn(d.registry.clone());
    until("first push", 10, || {
        s.held(&event).is_some_and(|h| h == log_of(&d, &event))
    })
    .await;
    until("listening", 10, || s.state.asks.listening(&recon())).await;

    direct(
        &d,
        Method::DELETE,
        &format!("/server-link/clubs/{club}"),
        Value::Null,
    )
    .await;
    let before = s.held(&event).unwrap().len();
    passes(&d, &event, 2).await;
    until("the server sees the Director has gone", 20, || {
        !s.state.asks.listening(&recon())
    })
    .await;
    assert_eq!(s.held(&event).unwrap().len(), before);
    assert_eq!(view(&d).state, LinkState::NotSet);
    assert_eq!(state_of(&d, &event), LinkEventState::NoClub);
    task.abort();
}

#[tokio::test]
async fn an_event_removed_on_the_director_goes_to_no_club_any_more() {
    let d = director();
    let event = event(&d, "Friday").await;
    let club = save_club(&d, "https://grid.example.org", "gfs_abcabc").await;
    send_to(&d, &event, Some(&club)).await;
    assert_eq!(d.registry.link().stored().events.len(), 1);
    let (status, _) = ask(&d, true, Method::DELETE, &format!("/events/{event}"), None).await;
    assert!(status.is_success(), "{status}");
    assert!(d.registry.link().stored().events.is_empty());
    assert!(view(&d).events.is_empty());
    // It is not in the file either.
    let again = EventRegistry::new(Some(d.dir.path().join("data"))).unwrap();
    assert!(again.link().stored().events.is_empty());
    assert_eq!(again.link().stored().clubs.len(), 1);
}
