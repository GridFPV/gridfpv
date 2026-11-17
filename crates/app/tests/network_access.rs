//! Who may do what, from where (docs/primitives.md §11).
//!
//! The Grid computer is the director. Any other device is a visitor: it may read the public
//! pages and nothing else, unless it shows the director's access token. These tests drive
//! the very app the Director serves ([`build_app`]), first with made-up callers and then
//! over real sockets, and try every way in that could be thought of.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::{build_app, local_origin, run_director};
use gridfpv_server::access::MAX_FAILURES;
use gridfpv_server::events::{CreateEventRequest, EventRegistry};
use gridfpv_server::network::DEFAULT_PORT;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

const TOKEN: &str = "the-directors-token";
/// A caller on the Grid computer.
const HERE: &str = "127.0.0.1:50000";
/// A phone at the field.
const PHONE: &str = "192.168.1.77:50000";
/// The address the phone opens.
const LAN_HOST: &str = "192.168.1.5:8081";
const EVENT: &str = "Friday Night Whoops";

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gridfpv-access-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A Director with one open event and a console that is one small page.
struct Rig {
    registry: EventRegistry,
    app: Router,
    event: String,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn rig(token: bool) -> Rig {
    let dir = temp_dir("rig");
    std::fs::write(dir.join("index.html"), "<title>console</title>").unwrap();
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("assets/app.js"), "console.log('app')").unwrap();
    let registry = EventRegistry::new(None).unwrap();
    let meta = registry.create(&CreateEventRequest::named(EVENT)).unwrap();
    registry.set_active(&meta.id).unwrap();
    if token {
        assert!(registry.tokens().register_rd_token(TOKEN));
    }
    Rig {
        app: build_app(registry.clone(), &dir),
        registry,
        event: meta.id.0.clone(),
        dir,
    }
}

/// One request, as it would arrive.
#[derive(Clone)]
struct Call {
    method: Method,
    uri: String,
    peer: Option<SocketAddr>,
    host: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

fn call(method: Method, uri: &str) -> Call {
    Call {
        method,
        uri: uri.to_string(),
        peer: Some(HERE.parse().unwrap()),
        host: Some("127.0.0.1:8080".into()),
        headers: Vec::new(),
        body: None,
    }
}
fn get(uri: &str) -> Call {
    call(Method::GET, uri)
}
fn post(uri: &str, body: Value) -> Call {
    call(Method::POST, uri).json(body)
}

impl Call {
    /// From a phone on the network, asking for the address a phone would.
    fn visitor(mut self) -> Self {
        self.peer = Some(PHONE.parse().unwrap());
        self.host = Some(LAN_HOST.into());
        self
    }
    fn from(mut self, peer: &str) -> Self {
        self.peer = Some(peer.parse().unwrap());
        self
    }
    fn no_peer(mut self) -> Self {
        self.peer = None;
        self
    }
    fn host(mut self, host: &str) -> Self {
        self.host = Some(host.into());
        self
    }
    fn no_host(mut self) -> Self {
        self.host = None;
        self
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    fn bearer(self, token: &str) -> Self {
        self.header("authorization", &format!("Bearer {token}"))
    }
    fn cookie(self, session: &str) -> Self {
        self.header("cookie", &format!("gridfpv_director={session}"))
    }
    fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

struct Answer {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }
    fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or_default().to_string()
    }
    fn set_cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .map(|value| value.to_str().unwrap().to_string())
    }
    /// The session a sign-in's cookie holds.
    fn session(&self) -> String {
        let set = self.set_cookie().expect("a cookie was set");
        let value = set.split(';').next().unwrap();
        value.split_once('=').unwrap().1.to_string()
    }
}

async fn send(app: &Router, call: Call) -> Answer {
    let mut request = Request::builder().method(call.method).uri(&call.uri);
    if let Some(host) = &call.host {
        request = request.header(header::HOST, host);
    }
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
    if let Some(peer) = call.peer {
        request.extensions_mut().insert(ConnectInfo(peer));
    }
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    // A feed never ends: its status is the answer, its body is not waited for.
    let streaming = headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|kind| kind.to_str().unwrap_or_default().contains("event-stream"));
    let body = if streaming {
        String::new()
    } else {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Answer {
        status,
        headers,
        body,
    }
}

async fn status(app: &Router, call: Call) -> StatusCode {
    send(app, call).await.status
}

/// The names of every event, read as the director on the Grid computer.
async fn event_names(rig: &Rig) -> Vec<String> {
    let mut call = get("/events");
    if rig.registry.tokens().has_control_credential() {
        call = call.bearer(TOKEN);
    }
    let answer = send(&rig.app, call).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    answer
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["name"].as_str().unwrap().to_string())
        .collect()
}

/// Every route path the API declares, read from its source, with `{…}` filled in.
fn api_paths() -> Vec<String> {
    let source = include_str!("../../server/src/app.rs");
    let mut paths = vec!["/diagnostics".to_string()];
    let mut rest = source;
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        let Some(open) = rest.find('"') else { break };
        // The path is the first thing in the call.
        if !rest[..open].trim().is_empty() {
            continue;
        }
        let quoted = &rest[open + 1..];
        let close = quoted.find('"').unwrap();
        let mut path = String::new();
        let mut inside = false;
        for ch in quoted[..close].chars() {
            match ch {
                '{' => {
                    inside = true;
                    path.push('x');
                }
                '}' => inside = false,
                _ if inside => {}
                _ => path.push(ch),
            }
        }
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    paths
}

const METHODS: [Method; 6] = [
    Method::GET,
    Method::HEAD,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
];

/// What a visitor with no token may ask of the API. Everything else must answer 401.
fn open_to_visitors(method: &Method, path: &str) -> bool {
    let read = *method == Method::GET || *method == Method::HEAD;
    match path {
        "/health" | "/about" | "/time" | "/public/hello" => read,
        // The open event, as a visitor may see it (`spectator.rs` checks what is in them).
        "/public/event"
        | "/public/event/state"
        | "/public/event/changes"
        | "/public/event/pilots"
        | "/public/event/classes"
        | "/public/event/timers"
        | "/public/event/channels" => read,
        // Signing in and out, as the director or as a pilot. Each checks what it is shown.
        "/public/session" | "/public/pilot/session" => {
            read || *method == Method::POST || *method == Method::DELETE
        }
        // What a pilot does for themselves. Each route refuses a caller with no sign-in.
        _ if path.starts_with("/public/pilot/") => {
            read || *method == Method::POST || *method == Method::DELETE
        }
        _ => false,
    }
}

// ── Every route ─────────────────────────────────────────────────────────────

#[test]
fn the_route_list_was_read() {
    let paths = api_paths();
    assert!(
        paths.len() >= 40,
        "only {} routes found: {paths:?}",
        paths.len()
    );
    for known in [
        "/events",
        "/events/x/state",
        "/events/x/commands",
        "/events/x/changes",
        "/events/x/races/x/video/x",
        "/pilots",
        "/timers/x/live",
        "/video/settings",
        "/data/export",
        "/network",
        "/public/hello",
        "/public/session",
        "/diagnostics",
    ] {
        assert!(paths.iter().any(|path| path == known), "{known} is missing");
    }
}

#[tokio::test]
async fn a_visitor_with_no_token_is_refused_on_every_route_but_the_public_ones() {
    for token_set in [false, true] {
        let rig = rig(token_set);
        for path in api_paths() {
            for method in METHODS {
                let answer = send(&rig.app, call(method.clone(), &path).visitor()).await;
                if path == "/public/session" && method == Method::POST {
                    // The gate lets it through, and the sign-in itself refuses a caller
                    // with no token. The words are the sign-in's, not the gate's.
                    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
                    assert!(answer.body.contains("not the director's access token"));
                } else if open_to_visitors(&method, &path) {
                    assert_ne!(
                        answer.status,
                        StatusCode::UNAUTHORIZED,
                        "{method} {path} should be open to a visitor"
                    );
                    assert_ne!(answer.status, StatusCode::FORBIDDEN, "{method} {path}");
                } else {
                    assert_eq!(
                        answer.status,
                        StatusCode::UNAUTHORIZED,
                        "{method} {path} (token set: {token_set}) answered {}: {}",
                        answer.status,
                        answer.body
                    );
                    // HEAD has no body to read.
                    if method != Method::HEAD {
                        assert_eq!(answer.code(), "Unauthorized", "{method} {path}");
                        assert!(
                            !answer.body.contains(EVENT),
                            "{method} {path} gave something away"
                        );
                    }
                }
            }
        }
        // And nothing was changed by any of it.
        assert_eq!(event_names(&rig).await, [EVENT]);
    }
}

#[tokio::test]
async fn the_grid_computer_is_refused_on_none_of_them() {
    let rig = rig(false);
    let mut reached = 0;
    for path in api_paths() {
        let answer = send(&rig.app, get(&path)).await;
        assert_ne!(answer.status, StatusCode::UNAUTHORIZED, "GET {path}");
        assert_ne!(answer.status, StatusCode::FORBIDDEN, "GET {path}");
        if answer.status == StatusCode::OK {
            reached += 1;
        }
    }
    assert!(reached >= 15, "only {reached} routes answered a plain read");
}

#[tokio::test]
async fn a_visitor_with_the_token_is_refused_on_none_of_them() {
    let rig = rig(true);
    for path in api_paths() {
        let answer = send(&rig.app, get(&path).visitor().bearer(TOKEN)).await;
        assert_ne!(answer.status, StatusCode::UNAUTHORIZED, "GET {path}");
        assert_ne!(answer.status, StatusCode::FORBIDDEN, "GET {path}");
    }
}

// ── Who is local ────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_grid_computer_reads_and_writes_with_no_token_when_none_is_set() {
    let rig = rig(false);
    assert_eq!(status(&rig.app, get("/events")).await, StatusCode::OK);
    let made = send(&rig.app, post("/events", json!({"name": "Second"}))).await;
    assert!(made.status.is_success(), "{}", made.body);
    assert_eq!(event_names(&rig).await.len(), 2);
    // By name and over IPv6 too.
    for (peer, host) in [
        ("127.0.0.1:1", "localhost:8080"),
        ("[::1]:1", "[::1]:8080"),
        ("[::1]:1", "localhost"),
        ("[::ffff:127.0.0.1]:1", "127.0.0.1:8080"),
    ] {
        assert_eq!(
            status(&rig.app, get("/events").from(peer).host(host)).await,
            StatusCode::OK,
            "{peer} asking for {host}"
        );
    }
}

#[tokio::test]
async fn with_a_token_set_the_grid_computer_still_reads_freely_and_writes_with_the_token() {
    let rig = rig(true);
    assert_eq!(status(&rig.app, get("/events")).await, StatusCode::OK);
    let refused = send(&rig.app, post("/events", json!({"name": "Second"}))).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    let made = send(
        &rig.app,
        post("/events", json!({"name": "Second"})).bearer(TOKEN),
    )
    .await;
    assert!(made.status.is_success(), "{}", made.body);
    assert_eq!(event_names(&rig).await.len(), 2);
}

#[tokio::test]
async fn any_caller_that_is_not_this_computer_is_a_visitor() {
    let rig = rig(false);
    for peer in [
        "192.168.1.77:1",
        "10.0.0.9:1",
        "172.16.4.4:1",
        "169.254.9.9:1",
        "100.64.0.3:1",
        "8.8.8.8:1",
        "0.0.0.0:1",
        "[fe80::1]:1",
        "[2001:db8::9]:1",
        "[::ffff:192.168.1.77]:1",
    ] {
        // Whatever address it says it asked for.
        for host in [LAN_HOST, "127.0.0.1:8080", "localhost", "[::1]:8080"] {
            let call = get("/events").from(peer).host(host);
            assert_eq!(
                status(&rig.app, call).await,
                StatusCode::UNAUTHORIZED,
                "{peer} asking for {host}"
            );
            let write = post("/events", json!({"name": "Sneaky"}))
                .from(peer)
                .host(host);
            assert_eq!(
                status(&rig.app, write).await,
                StatusCode::UNAUTHORIZED,
                "{peer} asking for {host}"
            );
        }
    }
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn a_caller_with_no_known_address_is_a_visitor() {
    let rig = rig(false);
    assert_eq!(
        status(&rig.app, get("/events").no_peer()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            &rig.app,
            post("/events", json!({"name": "Sneaky"})).no_peer()
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn a_request_passed_on_by_a_proxy_on_this_computer_is_a_visitor() {
    let rig = rig(false);
    for name in [
        "forwarded",
        "x-forwarded-for",
        "X-Forwarded-For",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "via",
    ] {
        for value in ["127.0.0.1", "for=127.0.0.1", "192.168.1.77", ""] {
            let read = get("/events").header(name, value);
            assert_eq!(
                status(&rig.app, read).await,
                StatusCode::UNAUTHORIZED,
                "{name}: {value}"
            );
            let write = post("/events", json!({"name": "Sneaky"})).header(name, value);
            assert_eq!(
                status(&rig.app, write).await,
                StatusCode::UNAUTHORIZED,
                "{name}: {value}"
            );
        }
    }
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn a_page_that_points_its_own_name_at_this_computer_is_a_visitor() {
    // DNS rebinding: the browser is on the Grid computer, the page is not ours.
    let rig = rig(false);
    for host in [
        "evil.example",
        "evil.example:8080",
        "localhost.evil.example:8080",
        "127.0.0.1.evil.example",
        "192.168.1.5:8080",
        "gridfpv.local:8080",
    ] {
        assert_eq!(
            status(&rig.app, get("/events").host(host)).await,
            StatusCode::UNAUTHORIZED,
            "{host}"
        );
        // The browser would say the request is from the page's own origin.
        let write = post("/events", json!({"name": "Sneaky"}))
            .host(host)
            .header("origin", &format!("http://{host}"));
        assert_eq!(
            status(&rig.app, write).await,
            StatusCode::UNAUTHORIZED,
            "{host}"
        );
    }
    assert_eq!(
        status(&rig.app, get("/events").no_host()).await,
        StatusCode::UNAUTHORIZED,
        "no Host at all"
    );
    let two = get("/events").header("host", "127.0.0.1:8080");
    assert_eq!(
        status(&rig.app, two).await,
        StatusCode::UNAUTHORIZED,
        "two Host headers"
    );
    assert_eq!(event_names(&rig).await, [EVENT]);
}

// ── What a visitor gets ─────────────────────────────────────────────────────

#[tokio::test]
async fn a_visitor_gets_the_console_files_and_can_send_them_nothing() {
    let rig = rig(false);
    let page = send(&rig.app, get("/").visitor()).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<title>console</title>"));
    let script = send(&rig.app, get("/assets/app.js").visitor()).await;
    assert_eq!(script.status, StatusCode::OK);
    assert!(script.body.contains("console.log"));
    // A page of the console is the console.
    let deep = send(&rig.app, get("/some/page").visitor()).await;
    assert_eq!(deep.status, StatusCode::OK);
    assert!(deep.body.contains("<title>console</title>"));
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        for path in ["/", "/index.html", "/assets/app.js", "/some/page"] {
            assert_eq!(
                status(&rig.app, call(method.clone(), path).visitor()).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }
}

#[tokio::test]
async fn a_path_written_to_fool_the_gate_gives_a_visitor_nothing() {
    for token_set in [false, true] {
        let rig = rig(token_set);
        let events = format!("/events/{}/state", rig.event);
        let mut tries = vec![
            "/public/../events".to_string(),
            "/public/../../events".to_string(),
            "/public/%2e%2e/events".to_string(),
            "/public/%2E%2E/events".to_string(),
            "/public/..%2fevents".to_string(),
            "/public%2f..%2fevents".to_string(),
            "/public/..%5cevents".to_string(),
            "/public//events".to_string(),
            "//events".to_string(),
            "///events".to_string(),
            "/./events".to_string(),
            "/../events".to_string(),
            "/assets/../events".to_string(),
            "/assets/..%2fevents".to_string(),
            "/%2e%2e/events".to_string(),
            "/%65vents".to_string(),
            "/EVENTS".to_string(),
            "/Events".to_string(),
            "/events/".to_string(),
            "/events//".to_string(),
            "/events?x=/public/hello".to_string(),
            "/events#/public/hello".to_string(),
            "/events;/public/hello".to_string(),
            "/public/hello/../../events".to_string(),
            "/public/hello;/../../events".to_string(),
            "/health/../events".to_string(),
            "/health%2f..%2fevents".to_string(),
            "/time/../pilots".to_string(),
            "/about/../active-event".to_string(),
            "/public/hello%00/../../events".to_string(),
            "/public/session/../../events".to_string(),
        ];
        tries.push(format!("/public/..{events}"));
        tries.push(format!("/{events}"));
        tries.push(format!("/public/%2e%2e{events}"));
        for path in tries {
            // Some of these cannot even be written as a request; that is a fine outcome.
            let Ok(uri) = path.parse::<axum::http::Uri>() else {
                continue;
            };
            let answer = send(&rig.app, get(&uri.to_string()).visitor()).await;
            assert!(
                !answer.body.contains(EVENT),
                "GET {path} gave the event away: {} {}",
                answer.status,
                answer.body
            );
            // It is refused, not found, or it is the console's own page. Never data.
            let page = answer.status == StatusCode::OK && answer.body.contains("<title>console");
            assert!(
                page || answer.status == StatusCode::UNAUTHORIZED
                    || answer.status == StatusCode::NOT_FOUND
                    || answer.status == StatusCode::BAD_REQUEST
                    || answer.status == StatusCode::METHOD_NOT_ALLOWED,
                "GET {path} answered {} {}",
                answer.status,
                answer.body
            );
            let write = send(
                &rig.app,
                post(&uri.to_string(), json!({"name": "Sneaky"})).visitor(),
            )
            .await;
            assert!(
                write.status == StatusCode::UNAUTHORIZED,
                "POST {path} answered {}",
                write.status
            );
        }
        assert_eq!(event_names(&rig).await, [EVENT]);
    }
}

#[tokio::test]
async fn hello_tells_each_caller_who_they_are() {
    let rig = rig(true);
    let local = send(&rig.app, get("/public/hello")).await.json();
    assert_eq!(local["caller"], "Local");
    assert_eq!(local["event"], EVENT);
    assert_eq!(local["director_sign_in"], true);
    assert!(local["version"].as_str().is_some_and(|v| !v.is_empty()));

    let visitor = send(&rig.app, get("/public/hello").visitor()).await.json();
    assert_eq!(visitor["caller"], "Visitor");
    assert_eq!(visitor["event"], EVENT);

    let director = send(&rig.app, get("/public/hello").visitor().bearer(TOKEN))
        .await
        .json();
    assert_eq!(director["caller"], "Director");

    // A wrong token is still a visitor, and still gets the public answer.
    let wrong = send(&rig.app, get("/public/hello").visitor().bearer("nope")).await;
    assert_eq!(wrong.status, StatusCode::OK);
    assert_eq!(wrong.json()["caller"], "Visitor");

    // With no token set on the Director, nobody can sign in from another device.
    let open = self::rig(false);
    let hello = send(&open.app, get("/public/hello").visitor()).await.json();
    assert_eq!(hello["director_sign_in"], false);
    assert_eq!(hello["caller"], "Visitor");
    // And with no event open there is no name to give.
    open.registry
        .delete(&gridfpv_server::events::EventId(open.event.clone()))
        .unwrap();
    let hello = send(&open.app, get("/public/hello").visitor()).await.json();
    assert!(hello.get("event").is_none(), "{hello}");
}

// ── The director's token, from another device ───────────────────────────────

#[tokio::test]
async fn a_visitor_with_the_token_has_the_whole_director() {
    let rig = rig(true);
    let read = send(&rig.app, get("/events").visitor().bearer(TOKEN)).await;
    assert_eq!(read.status, StatusCode::OK);
    assert!(read.body.contains(EVENT));
    let made = send(
        &rig.app,
        post("/events", json!({"name": "From the tablet"}))
            .visitor()
            .bearer(TOKEN),
    )
    .await;
    assert!(made.status.is_success(), "{}", made.body);
    assert_eq!(event_names(&rig).await.len(), 2);
}

#[tokio::test]
async fn with_no_token_set_no_token_a_visitor_sends_lets_them_in() {
    // The Grid computer is open with no token set. That must never spread to the network.
    let rig = rig(false);
    for (n, token) in ["", "x", "null", "undefined", TOKEN, "Bearer", "admin"]
        .into_iter()
        .enumerate()
    {
        // Each from its own device, so none is shut out for guessing.
        let device = format!("192.168.1.{}:1", 100 + n);
        let read = get("/events").visitor().from(&device).bearer(token);
        assert_eq!(
            status(&rig.app, read).await,
            StatusCode::UNAUTHORIZED,
            "{token:?}"
        );
        let write = post("/events", json!({"name": "Sneaky"}))
            .visitor()
            .from(&device)
            .bearer(token);
        assert_eq!(
            status(&rig.app, write).await,
            StatusCode::UNAUTHORIZED,
            "{token:?}"
        );
        let cookie = get("/events").visitor().from(&device).cookie(token);
        assert_eq!(
            status(&rig.app, cookie).await,
            StatusCode::UNAUTHORIZED,
            "{token:?}"
        );
    }
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn a_read_only_join_token_does_not_open_the_director_to_a_visitor() {
    let rig = rig(true);
    let join = rig.registry.tokens().issue_join_token();
    assert_eq!(
        status(&rig.app, get("/events").visitor().bearer(&join)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            &rig.app,
            post("/events", json!({"name": "Sneaky"}))
                .visitor()
                .bearer(&join)
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn wrong_tokens_from_one_device_shut_it_out_for_a_while() {
    let rig = rig(true);
    for n in 0..MAX_FAILURES {
        assert_eq!(
            status(
                &rig.app,
                get("/events").visitor().bearer(&format!("guess-{n}"))
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "guess {n}"
        );
    }
    // Now even the right token is refused from that device, in a header or a cookie.
    let shut = send(&rig.app, get("/events").visitor().bearer(TOKEN)).await;
    assert_eq!(shut.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(shut.code(), "TooManyRequests");
    assert_eq!(
        status(
            &rig.app,
            post("/events", json!({"name": "Sneaky"}))
                .visitor()
                .bearer(TOKEN)
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Signing in is refused too.
    assert_eq!(
        status(
            &rig.app,
            call(Method::POST, "/public/session")
                .visitor()
                .bearer(TOKEN)
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    // The public pages still answer it, as a visitor.
    let hello = send(&rig.app, get("/public/hello").visitor().bearer(TOKEN)).await;
    assert_eq!(hello.status, StatusCode::OK);
    assert_eq!(hello.json()["caller"], "Visitor");
    assert_eq!(status(&rig.app, get("/").visitor()).await, StatusCode::OK);
    // With no token at all it is an ordinary visitor: refused, not shut out.
    assert_eq!(
        status(&rig.app, get("/events").visitor()).await,
        StatusCode::UNAUTHORIZED
    );
    // Another device is not touched, and neither is the Grid computer.
    let other = get("/events")
        .visitor()
        .from("192.168.1.78:1")
        .bearer(TOKEN);
    assert_eq!(status(&rig.app, other).await, StatusCode::OK);
    assert_eq!(status(&rig.app, get("/events")).await, StatusCode::OK);
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn a_right_token_clears_the_count_of_wrong_ones() {
    let rig = rig(true);
    for round in 0..3 {
        for n in 0..MAX_FAILURES - 1 {
            status(&rig.app, get("/events").visitor().bearer(&format!("g{n}"))).await;
        }
        assert_eq!(
            status(&rig.app, get("/events").visitor().bearer(TOKEN)).await,
            StatusCode::OK,
            "round {round}"
        );
    }
}

#[tokio::test]
async fn the_count_is_per_device_whatever_port_it_calls_from() {
    let rig = rig(true);
    for n in 0..MAX_FAILURES {
        let from = format!("192.168.1.77:{}", 40000 + n);
        status(
            &rig.app,
            get("/events").visitor().from(&from).bearer("guess"),
        )
        .await;
    }
    let again = get("/events")
        .visitor()
        .from("192.168.1.77:9")
        .bearer(TOKEN);
    assert_eq!(status(&rig.app, again).await, StatusCode::TOO_MANY_REQUESTS);
    // The same device seen as an IPv4 address inside IPv6 is the same device.
    let mapped = get("/events")
        .visitor()
        .from("[::ffff:192.168.1.77]:9")
        .bearer(TOKEN);
    assert_eq!(
        status(&rig.app, mapped).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

// ── Signing in from a browser on another device ─────────────────────────────

#[tokio::test]
async fn a_director_signs_in_from_another_device_and_the_cookie_does_the_rest() {
    let rig = rig(true);
    let signed = send(
        &rig.app,
        call(Method::POST, "/public/session")
            .visitor()
            .bearer(TOKEN),
    )
    .await;
    assert_eq!(signed.status, StatusCode::NO_CONTENT);
    let set = signed.set_cookie().unwrap();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Path=/"));
    let session = signed.session();
    assert_ne!(session, TOKEN, "the cookie never holds the access token");
    assert!(session.len() >= 32);

    // With only the cookie: reads, the change feed, and writes.
    assert_eq!(
        status(&rig.app, get("/events").visitor().cookie(&session)).await,
        StatusCode::OK
    );
    let feed = get(&format!("/events/{}/changes", rig.event))
        .visitor()
        .cookie(&session);
    assert_eq!(status(&rig.app, feed).await, StatusCode::OK);
    let made = send(
        &rig.app,
        post("/events", json!({"name": "From the tablet"}))
            .visitor()
            .cookie(&session)
            .header("origin", &format!("http://{LAN_HOST}")),
    )
    .await;
    assert!(made.status.is_success(), "{}", made.body);
    let hello = send(&rig.app, get("/public/hello").visitor().cookie(&session)).await;
    assert_eq!(hello.json()["caller"], "Director");

    // Signing out ends it at once.
    let out = send(
        &rig.app,
        call(Method::DELETE, "/public/session")
            .visitor()
            .cookie(&session),
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert!(out.set_cookie().unwrap().contains("Max-Age=0"));
    assert_eq!(
        status(&rig.app, get("/events").visitor().cookie(&session)).await,
        StatusCode::UNAUTHORIZED
    );
    // The access token itself still works.
    assert_eq!(
        status(&rig.app, get("/events").visitor().bearer(TOKEN)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn signing_in_takes_the_access_token_and_nothing_else() {
    let rig = rig(true);
    let sign_in = || call(Method::POST, "/public/session").visitor();
    for wrong in [
        sign_in(),
        sign_in().bearer("nope"),
        sign_in().bearer(""),
        sign_in().header("authorization", "Basic dGhlLWRpcmVjdG9ycy10b2tlbg=="),
        sign_in().header("authorization", TOKEN),
        // The token in a cookie, or in the body, is not a sign-in.
        sign_in().cookie(TOKEN),
        sign_in().json(json!({"token": TOKEN})),
    ] {
        let answer = send(&rig.app, wrong).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{}", answer.body);
        assert!(answer.set_cookie().is_none(), "no cookie on a refusal");
    }
    // A read-only token cannot sign in as the director.
    let join = rig.registry.tokens().issue_join_token();
    assert_eq!(
        status(&rig.app, sign_in().bearer(&join)).await,
        StatusCode::UNAUTHORIZED
    );
    // A sign-in cannot be used to make another one.
    let session = send(&rig.app, sign_in().bearer(TOKEN)).await.session();
    assert_eq!(
        status(&rig.app, sign_in().bearer(&session)).await,
        StatusCode::UNAUTHORIZED
    );
    // With no token set on the Director there is nothing to sign in with, from anywhere.
    let open = self::rig(false);
    for caller in [
        sign_in().bearer(TOKEN),
        call(Method::POST, "/public/session"),
    ] {
        assert_eq!(status(&open.app, caller).await, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn signing_out_never_removes_the_access_token_and_always_answers() {
    let rig = rig(true);
    let sign_out = || call(Method::DELETE, "/public/session").visitor();
    for caller in [
        sign_out(),
        sign_out().cookie("unknown"),
        sign_out().cookie(TOKEN),
        sign_out().bearer(TOKEN),
        sign_out().bearer(TOKEN).cookie(TOKEN),
    ] {
        let answer = send(&rig.app, caller).await;
        assert_eq!(answer.status, StatusCode::NO_CONTENT);
    }
    assert_eq!(
        status(&rig.app, get("/events").visitor().bearer(TOKEN)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn two_devices_sign_in_and_out_on_their_own() {
    let rig = rig(true);
    let sign_in = |peer: &str| {
        call(Method::POST, "/public/session")
            .visitor()
            .from(peer)
            .bearer(TOKEN)
    };
    let tablet = send(&rig.app, sign_in("192.168.1.80:1")).await.session();
    let laptop = send(&rig.app, sign_in("192.168.1.81:1")).await.session();
    assert_ne!(tablet, laptop);
    send(
        &rig.app,
        call(Method::DELETE, "/public/session")
            .visitor()
            .cookie(&tablet),
    )
    .await;
    assert_eq!(
        status(&rig.app, get("/events").visitor().cookie(&tablet)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(&rig.app, get("/events").visitor().cookie(&laptop)).await,
        StatusCode::OK
    );
}

// ── Requests made by another website ────────────────────────────────────────

#[tokio::test]
async fn a_change_asked_for_by_another_website_is_refused_even_on_the_grid_computer() {
    let rig = rig(false);
    for origin in [
        "http://evil.example",
        "https://evil.example",
        "http://evil.example:8080",
        "http://192.168.1.50",
        "http://localhost.evil.example",
        "null",
        "file://",
        "chrome-extension://abcdefgh",
    ] {
        for (method, path, body) in [
            (Method::POST, "/events", Some(json!({"name": "Sneaky"}))),
            (Method::PUT, "/network", Some(json!({"enabled": true}))),
            (
                Method::DELETE,
                format!("/events/{}", rig.event).as_str(),
                None,
            ),
            (Method::PATCH, "/events", None),
        ] {
            let mut request = call(method.clone(), path).header("origin", origin);
            request.body = body;
            let answer = send(&rig.app, request).await;
            assert_eq!(
                answer.status,
                StatusCode::FORBIDDEN,
                "{method} {path} from {origin}: {}",
                answer.body
            );
            assert_eq!(answer.code(), "Forbidden");
        }
    }
    assert_eq!(event_names(&rig).await, [EVENT]);
    assert!(!rig.registry.network().settings().enabled);
}

#[tokio::test]
async fn a_change_from_the_directors_own_pages_goes_through() {
    let rig = rig(false);
    for (n, origin) in [
        "http://127.0.0.1:8080",
        "http://localhost:5173",
        "http://[::1]:5173",
        "tauri://localhost",
    ]
    .into_iter()
    .enumerate()
    {
        let made = send(
            &rig.app,
            post("/events", json!({"name": format!("Event {n}")})).header("origin", origin),
        )
        .await;
        assert!(made.status.is_success(), "{origin}: {}", made.body);
    }
    assert_eq!(event_names(&rig).await.len(), 5);
}

#[tokio::test]
async fn another_website_cannot_use_a_signed_in_browser_either() {
    let rig = rig(true);
    let session = send(
        &rig.app,
        call(Method::POST, "/public/session")
            .visitor()
            .bearer(TOKEN),
    )
    .await
    .session();
    let forged = send(
        &rig.app,
        post("/events", json!({"name": "Sneaky"}))
            .visitor()
            .cookie(&session)
            .header("origin", "http://evil.example"),
    )
    .await;
    assert_eq!(forged.status, StatusCode::FORBIDDEN);
    // Nor sign the browser out, or in.
    let out = send(
        &rig.app,
        call(Method::DELETE, "/public/session")
            .visitor()
            .cookie(&session)
            .header("origin", "http://evil.example"),
    )
    .await;
    assert_eq!(out.status, StatusCode::FORBIDDEN);
    assert_eq!(
        status(&rig.app, get("/events").visitor().cookie(&session)).await,
        StatusCode::OK
    );
    assert_eq!(event_names(&rig).await, [EVENT]);
}

#[tokio::test]
async fn only_a_page_on_this_computer_gets_a_cors_answer() {
    let rig = rig(false);
    let allow = |answer: &Answer| {
        answer
            .headers
            .get("access-control-allow-origin")
            .map(|value| value.to_str().unwrap().to_string())
    };
    for origin in [
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "tauri://localhost",
    ] {
        let read = send(&rig.app, get("/events").header("origin", origin)).await;
        assert_eq!(allow(&read).as_deref(), Some(origin), "{origin}");
        let preflight = send(
            &rig.app,
            call(Method::OPTIONS, "/events")
                .header("origin", origin)
                .header("access-control-request-method", "POST")
                .header("access-control-request-headers", "content-type"),
        )
        .await;
        assert_eq!(allow(&preflight).as_deref(), Some(origin), "{origin}");
    }
    for origin in [
        "http://evil.example",
        "https://evil.example:8080",
        "http://192.168.1.50:8081",
        "http://localhost.evil.example",
        "null",
    ] {
        let read = send(&rig.app, get("/events").header("origin", origin)).await;
        assert_eq!(allow(&read), None, "{origin}");
        let preflight = send(
            &rig.app,
            call(Method::OPTIONS, "/events")
                .header("origin", origin)
                .header("access-control-request-method", "POST"),
        )
        .await;
        assert_eq!(allow(&preflight), None, "{origin}");
    }
}

#[test]
fn a_local_origin_is_a_page_on_this_computer() {
    for origin in [
        "http://localhost",
        "http://localhost:5173",
        "https://localhost:8443",
        "http://127.0.0.1:8080",
        "http://[::1]:8080",
        "tauri://localhost",
        "TAURI://localhost",
        "HTTP://LOCALHOST:5173",
    ] {
        assert!(local_origin(origin), "{origin}");
    }
    for origin in [
        "",
        "null",
        "localhost",
        "http://",
        "http://evil.example",
        "http://localhost.evil.example",
        "http://localhost:5173/path",
        "http://192.168.1.5:8081",
        "ftp://localhost",
        "file://localhost",
        "http://localhost@evil.example",
    ] {
        assert!(!local_origin(origin), "{origin}");
    }
}

// ── The network setting, through the API ────────────────────────────────────

#[tokio::test]
async fn the_network_setting_is_read_and_changed_by_the_director_only() {
    let rig = rig(true);
    let view = send(&rig.app, get("/network")).await.json();
    assert_eq!(view["enabled"], false);
    assert_eq!(view["port"], DEFAULT_PORT);
    assert_eq!(view["state"], "Off");
    assert_eq!(view["addresses"], json!([]));

    let change = || call(Method::PUT, "/network").json(json!({"enabled": true, "port": 9000}));
    // A visitor, the Grid computer with no token, a wrong token: all refused.
    assert_eq!(
        status(&rig.app, change().visitor()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(status(&rig.app, change()).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        status(&rig.app, change().bearer("nope")).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(!rig.registry.network().settings().enabled);

    let changed = send(&rig.app, change().bearer(TOKEN)).await;
    assert_eq!(changed.status, StatusCode::OK, "{}", changed.body);
    assert_eq!(changed.json()["enabled"], true);
    assert_eq!(changed.json()["port"], 9000);
    assert_eq!(changed.json()["state"], "Starting");
    assert_eq!(rig.registry.network().settings().port, 9000);

    for bad in [
        json!({"enabled": true, "port": 80}),
        json!({"enabled": true, "port": 0}),
        json!({"enabled": true, "port": 1023}),
    ] {
        let answer = send(
            &rig.app,
            call(Method::PUT, "/network").json(bad).bearer(TOKEN),
        )
        .await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    }
    for malformed in [
        json!({}),
        json!({"port": 9000}),
        json!({"enabled": "yes"}),
        json!({"enabled": true, "port": 70000}),
        json!({"enabled": true, "port": -1}),
        json!({"enabled": true, "port": "9000"}),
        json!([]),
    ] {
        let answer = send(
            &rig.app,
            call(Method::PUT, "/network")
                .json(malformed.clone())
                .bearer(TOKEN),
        )
        .await;
        assert!(
            answer.status.is_client_error(),
            "{malformed} answered {}",
            answer.status
        );
    }
    assert_eq!(rig.registry.network().settings().port, 9000);
}

// ── Over real sockets ───────────────────────────────────────────────────────

/// One request over a real connection, written out by hand so the test decides every
/// header. The whole answer is read until the Director closes the connection.
async fn raw(addr: SocketAddr, request: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
        .await
        .map_err(|_| std::io::Error::other("no answer in 5 s"))??;
    Ok(String::from_utf8_lossy(&answer).into_owned())
}

fn request(method: &str, path: &str, host: &str, extra: &str, body: &str) -> String {
    let mut text =
        format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}");
    if !body.is_empty() {
        text.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    text.push_str("\r\n");
    text.push_str(body);
    text
}

fn status_of(answer: &str) -> u16 {
    answer
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status in {answer:?}"))
}

async fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A running Director, started the way the desktop app starts it.
struct Running {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<(), String>>,
    dir: PathBuf,
}

impl Running {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut self.task).await;
        std::fs::remove_dir_all(&self.dir).ok();
    }
    fn local(&self, port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }
    /// `GET /network`, asked from this computer.
    async fn network(&self) -> Value {
        let answer = raw(
            self.addr,
            &request("GET", "/network", &self.addr.to_string(), "", ""),
        )
        .await
        .unwrap();
        serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }
    async fn set_network(&self, body: Value) -> u16 {
        let answer = raw(
            self.addr,
            &request(
                "PUT",
                "/network",
                &self.addr.to_string(),
                "",
                &body.to_string(),
            ),
        )
        .await
        .unwrap();
        status_of(&answer)
    }
    /// Wait until the network listener says `state`.
    async fn until(&self, state: &str) -> Value {
        // Long enough for a failed listener's next try, on a slow build machine.
        for _ in 0..240 {
            let view = self.network().await;
            if view["state"] == state {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("never reached {state}: {}", self.network().await);
    }
}

async fn start(bind: SocketAddr, data: Option<PathBuf>) -> Running {
    let dir = temp_dir("run");
    std::fs::write(dir.join("index.html"), "<title>console</title>").unwrap();
    let bound: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let report = bound.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let assets = dir.clone();
    let task = tokio::spawn(async move {
        run_director(
            bind,
            data,
            assets,
            move |ready| *report.lock().unwrap() = Some(ready.bound),
            async move {
                let _ = stopped.await;
            },
        )
        .await
        .map_err(|e| e.to_string())
    });
    let mut addr = None;
    for _ in 0..200 {
        if let Some(found) = *bound.lock().unwrap() {
            addr = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut addr = addr.expect("the Director said where it listens");
    if addr.ip().is_unspecified() {
        addr.set_ip(Ipv4Addr::LOCALHOST.into());
    }
    Running {
        addr,
        stop: Some(stop),
        task,
        dir,
    }
}

async fn start_local() -> Running {
    start(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), None).await
}

#[tokio::test]
async fn over_a_real_socket_this_computer_is_the_director() {
    let running = start_local().await;
    let host = running.addr.to_string();
    let made = raw(
        running.addr,
        &request("POST", "/events", &host, "", r#"{"name":"Real"}"#),
    )
    .await
    .unwrap();
    assert!((200..300).contains(&status_of(&made)), "{made}");
    let hello = raw(
        running.addr,
        &request("GET", "/public/hello", &host, "", ""),
    )
    .await
    .unwrap();
    assert!(hello.contains(r#""caller":"Local""#), "{hello}");
    running.stop().await;
}

#[tokio::test]
async fn over_a_real_socket_the_tricks_make_a_visitor() {
    let running = start_local().await;
    let host = running.addr.to_string();
    let port = running.addr.port();
    let events = |host: &str, extra: &str| request("GET", "/events", host, extra, "");
    for (what, text) in [
        ("another name", events("evil.example", "")),
        (
            "another name with our port",
            events(&format!("evil.example:{port}"), ""),
        ),
        (
            "a proxy header",
            events(&host, "X-Forwarded-For: 127.0.0.1\r\n"),
        ),
        (
            "a forwarded header",
            events(&host, "Forwarded: for=127.0.0.1\r\n"),
        ),
        ("a via header", events(&host, "Via: 1.1 proxy\r\n")),
        (
            "an absolute request line naming another host",
            format!(
                "GET http://evil.example/events HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
            ),
        ),
        ("no host at all", "GET /events HTTP/1.0\r\n\r\n".to_string()),
    ] {
        let answer = raw(running.addr, &text).await.unwrap();
        let code = status_of(&answer);
        assert!(code == 401 || code == 400, "{what}: {answer}");
        assert!(!answer.contains("\"name\""), "{what} gave data away");
    }
    // Two Host headers: refused by the gate, or by the HTTP layer before it.
    let two = format!(
        "GET /events HTTP/1.1\r\nHost: {host}\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    let answer = raw(running.addr, &two).await.unwrap();
    assert!(matches!(status_of(&answer), 400 | 401), "{answer}");
    // The honest request still works.
    let honest = raw(running.addr, &events(&host, "")).await.unwrap();
    assert_eq!(status_of(&honest), 200, "{honest}");
    let by_name = raw(running.addr, &events(&format!("localhost:{port}"), ""))
        .await
        .unwrap();
    assert_eq!(status_of(&by_name), 200, "{by_name}");
    running.stop().await;
}

#[tokio::test]
async fn the_listener_for_other_devices_is_off_until_switched_on_and_off_again_after() {
    let running = start_local().await;
    let port = free_port().await;
    assert_eq!(running.network().await["state"], "Off");
    assert!(
        TcpStream::connect(running.local(port)).await.is_err(),
        "nothing listens before it is switched on"
    );

    assert_eq!(
        running
            .set_network(json!({"enabled": true, "port": port}))
            .await,
        200
    );
    let view = running.until("On").await;
    assert_eq!(view["port"], port);
    let hello = raw(
        running.local(port),
        &request("GET", "/public/hello", &format!("127.0.0.1:{port}"), "", ""),
    )
    .await
    .unwrap();
    assert_eq!(status_of(&hello), 200, "{hello}");

    assert_eq!(running.set_network(json!({"enabled": false})).await, 200);
    running.until("Off").await;
    // The port is let go of.
    let mut closed = false;
    for _ in 0..40 {
        if TcpStream::connect(running.local(port)).await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "the listener is gone after it is switched off");
    // The Director's own listener is untouched.
    assert_eq!(running.network().await["state"], "Off");
    running.stop().await;
}

#[tokio::test]
async fn changing_the_port_moves_the_listener() {
    let running = start_local().await;
    let first = free_port().await;
    let second = free_port().await;
    assert_ne!(first, second);
    running
        .set_network(json!({"enabled": true, "port": first}))
        .await;
    running.until("On").await;
    assert!(TcpStream::connect(running.local(first)).await.is_ok());

    running
        .set_network(json!({"enabled": true, "port": second}))
        .await;
    for _ in 0..100 {
        let view = running.network().await;
        if view["state"] == "On" && view["port"] == second {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(TcpStream::connect(running.local(second)).await.is_ok());
    assert!(
        TcpStream::connect(running.local(first)).await.is_err(),
        "the old port is closed"
    );
    // Setting the same thing again changes nothing and breaks nothing.
    running
        .set_network(json!({"enabled": true, "port": second}))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(running.network().await["state"], "On");
    assert!(TcpStream::connect(running.local(second)).await.is_ok());
    running.stop().await;
}

#[tokio::test]
async fn a_port_that_is_taken_is_reported_and_picked_up_when_it_is_freed() {
    let running = start_local().await;
    // Another program holds the port, on every address.
    let holder = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
    let port = holder.local_addr().unwrap().port();
    running
        .set_network(json!({"enabled": true, "port": port}))
        .await;
    let view = running.until("Failed").await;
    assert!(
        view["problem"]
            .as_str()
            .unwrap()
            .contains(&format!("Port {port} is in use")),
        "{view}"
    );
    assert_eq!(view["addresses"], json!([]));
    // The other program goes away. Grid looks again by itself.
    drop(holder);
    let view = running.until("On").await;
    assert_eq!(view["port"], port);
    assert!(view.get("problem").is_none() || view["addresses"] == json!([]));
    running.stop().await;
}

#[tokio::test]
async fn a_failed_listener_can_be_moved_to_a_free_port_or_switched_off() {
    let running = start_local().await;
    let holder = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
    let taken = holder.local_addr().unwrap().port();
    running
        .set_network(json!({"enabled": true, "port": taken}))
        .await;
    running.until("Failed").await;
    let free = free_port().await;
    running
        .set_network(json!({"enabled": true, "port": free}))
        .await;
    running.until("On").await;
    assert!(TcpStream::connect(running.local(free)).await.is_ok());
    running
        .set_network(json!({"enabled": true, "port": taken}))
        .await;
    running.until("Failed").await;
    assert!(
        TcpStream::connect(running.local(free)).await.is_err(),
        "the listener that worked is closed when the port is changed"
    );
    running.set_network(json!({"enabled": false})).await;
    let view = running.until("Off").await;
    assert!(view.get("problem").is_none(), "{view}");
    running.stop().await;
}

#[tokio::test]
async fn the_directors_own_port_cannot_be_picked() {
    let running = start_local().await;
    assert_eq!(
        running
            .set_network(json!({"enabled": true, "port": running.addr.port()}))
            .await,
        400
    );
    assert_eq!(running.network().await["state"], "Off");
    running.stop().await;
}

#[tokio::test]
async fn switching_off_ends_the_connections_that_were_open() {
    let running = start_local().await;
    let host = running.addr.to_string();
    let made = raw(
        running.addr,
        &request("POST", "/events", &host, "", r#"{"name":"Live"}"#),
    )
    .await
    .unwrap();
    let id = serde_json::from_str::<Value>(made.split("\r\n\r\n").nth(1).unwrap()).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let port = free_port().await;
    running
        .set_network(json!({"enabled": true, "port": port}))
        .await;
    running.until("On").await;

    // Follow the event's changes through the network listener, and leave the feed open.
    let mut feed = TcpStream::connect(running.local(port)).await.unwrap();
    feed.write_all(
        format!("GET /events/{id}/changes HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut first = [0u8; 256];
    let read = tokio::time::timeout(Duration::from_secs(3), feed.read(&mut first))
        .await
        .expect("the feed answered")
        .unwrap();
    let head = String::from_utf8_lossy(&first[..read]).into_owned();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    running.set_network(json!({"enabled": false})).await;
    running.until("Off").await;
    // The feed is cut: reading it ends (or fails) soon, and does not hang.
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        let mut sink = [0u8; 1024];
        loop {
            match feed.read(&mut sink).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "the open feed was not closed by switching off"
    );
    running.stop().await;
}

#[tokio::test]
async fn many_quick_changes_end_in_the_last_one() {
    let running = start_local().await;
    let ports = [free_port().await, free_port().await, free_port().await];
    for round in 0..12 {
        let port = ports[round % ports.len()];
        running
            .set_network(json!({"enabled": round % 4 != 3, "port": port}))
            .await;
    }
    let last = ports[2];
    running
        .set_network(json!({"enabled": true, "port": last}))
        .await;
    for _ in 0..100 {
        let view = running.network().await;
        if view["state"] == "On" && view["port"] == last {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(running.network().await["state"], "On");
    assert!(TcpStream::connect(running.local(last)).await.is_ok());
    for other in [ports[0], ports[1]] {
        if other != last {
            assert!(TcpStream::connect(running.local(other)).await.is_err());
        }
    }
    running.stop().await;
}

#[tokio::test]
async fn a_saved_on_setting_starts_the_listener_with_the_director() {
    let data = temp_dir("saved");
    let port = free_port().await;
    std::fs::write(
        data.join("network.json"),
        json!({"enabled": true, "port": port}).to_string(),
    )
    .unwrap();
    let running = start(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Some(data.clone()),
    )
    .await;
    let view = running.until("On").await;
    assert_eq!(view["port"], port);
    assert!(TcpStream::connect(running.local(port)).await.is_ok());
    // Switched off, it stays off after a restart.
    running.set_network(json!({"enabled": false})).await;
    running.until("Off").await;
    running.stop().await;
    let again = start(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Some(data.clone()),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(again.network().await["state"], "Off");
    assert!(TcpStream::connect(again.local(port)).await.is_err());
    again.stop().await;
    std::fs::remove_dir_all(data).ok();
}

#[tokio::test]
async fn stopping_the_director_stops_the_listener_for_other_devices() {
    let running = start_local().await;
    let port = free_port().await;
    running
        .set_network(json!({"enabled": true, "port": port}))
        .await;
    running.until("On").await;
    let addr = running.local(port);
    running.stop().await;
    let mut closed = false;
    for _ in 0..40 {
        if TcpStream::connect(addr).await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "the network listener outlived the Director");
}

#[tokio::test]
async fn a_director_started_on_the_network_says_it_is_always_on_and_adds_no_listener() {
    let running = start(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), None).await;
    let view = running.network().await;
    assert_eq!(view["state"], "Always");
    assert_eq!(view["port"], running.addr.port());
    // The switch is kept but starts nothing.
    let port = free_port().await;
    running
        .set_network(json!({"enabled": true, "port": port}))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(running.network().await["state"], "Always");
    assert!(TcpStream::connect(running.local(port)).await.is_err());
    running.stop().await;
}

/// The real thing: a caller that reaches this computer by its network address is a visitor.
/// It needs the computer to have a network address, which a build machine and a laptop do.
#[tokio::test]
async fn a_caller_that_comes_in_by_this_computers_network_address_is_a_visitor() {
    let Some(lan) = gridfpv_server::network::local_addresses()
        .into_iter()
        .next()
    else {
        eprintln!("skipped: this computer has no network address");
        return;
    };
    let running = start_local().await;
    let port = free_port().await;
    running
        .set_network(json!({"enabled": true, "port": port}))
        .await;
    let view = running.until("On").await;
    let address = format!("http://{lan}:{port}/");
    assert!(
        view["addresses"]
            .as_array()
            .unwrap()
            .contains(&json!(address)),
        "{address} is one of the addresses given: {view}"
    );

    let there = SocketAddr::from((lan, port));
    let host = format!("{lan}:{port}");
    let hello = raw(there, &request("GET", "/public/hello", &host, "", ""))
        .await
        .unwrap();
    assert_eq!(status_of(&hello), 200, "{hello}");
    assert!(hello.contains(r#""caller":"Visitor""#), "{hello}");
    let page = raw(there, &request("GET", "/", &host, "", ""))
        .await
        .unwrap();
    assert_eq!(status_of(&page), 200);
    assert!(page.contains("<title>console</title>"));

    for (method, path, body) in [
        ("GET", "/events", ""),
        ("GET", "/pilots", ""),
        ("GET", "/network", ""),
        ("GET", "/video/settings", ""),
        ("POST", "/events", r#"{"name":"Sneaky"}"#),
        ("PUT", "/network", r#"{"enabled":false}"#),
    ] {
        // Claiming to be local in the Host header changes nothing: the peer is not.
        for claimed in [host.as_str(), "127.0.0.1:8080", "localhost"] {
            let answer = raw(there, &request(method, path, claimed, "", body))
                .await
                .unwrap();
            assert_eq!(
                status_of(&answer),
                401,
                "{method} {path} as {claimed}: {answer}"
            );
        }
    }
    // Nothing was changed, and the listener is still on.
    let events = raw(
        running.addr,
        &request("GET", "/events", &running.addr.to_string(), "", ""),
    )
    .await
    .unwrap();
    assert!(!events.contains("Sneaky"), "{events}");
    assert_eq!(running.network().await["state"], "On");
    // The same listener, reached from this computer by its loopback address, is local.
    let local = raw(
        running.local(port),
        &request("GET", "/events", &format!("127.0.0.1:{port}"), "", ""),
    )
    .await
    .unwrap();
    assert_eq!(status_of(&local), 200, "{local}");
    running.stop().await;
}
