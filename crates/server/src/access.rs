//! Who is asking, and what they may do (docs/primitives.md §11).
//!
//! The Director is used from two places. The **Grid computer itself** is trusted: that is
//! the director, sitting at it. **Everything else on the network** is a visitor: a phone at
//! the field, a tablet, another computer. A visitor may look at what is public and nothing
//! more, unless they hold the director's access token.
//!
//! The gate is the one place that rule is applied. [`guard`] puts it around the whole app:
//! [`gate_api`] on every API route, and [`gate_files`] on everything else (the console's
//! files and pages). The two differ in one way only, which is what a visitor may read. A
//! request is judged by which of the two actually answers it, never by the look of its
//! path, so a new API route is closed to visitors until it is listed here.
//!
//! A router with no gate around it (the unit tests' `router(...)`) behaves as before.
//!
//! # Who counts as local
//!
//! All of these must hold ([`classify`]):
//!
//! - the connection comes from this computer (a loopback address);
//! - the request was not passed on by a proxy (no `Forwarded`, `X-Forwarded-*`, `X-Real-IP`
//!   or `Via` header), since a proxy on this computer makes every visitor look local;
//! - the address the browser asked for (`Host`) is this computer by name: `localhost` or a
//!   loopback address. A page on another site that points its own name at 127.0.0.1 ("DNS
//!   rebinding") fails this, and is treated as a visitor.
//!
//! Anything that cannot be told is a visitor. A request with no known peer address is a
//! visitor.
//!
//! # What a visitor may do
//!
//! Without the token: read the console's files ([`may_read_files`]), and read `/health`,
//! `/about`, `/time` and everything under `/public` ([`is_public_api`]). Everything else
//! answers 401.
//!
//! With the director's token (a `Bearer` header, or the session cookie set by
//! `POST /public/session`): everything, exactly as on the Grid computer.
//!
//! # Guessing the token
//!
//! A visitor who sends a wrong token is counted, by address. After
//! [`MAX_FAILURES`] wrong tries in [`FAILURE_WINDOW`], that address is refused (429) until
//! the window has passed, whatever token it sends.
//!
//! # Requests made by another website
//!
//! A browser tells the server which site a request came from (`Origin`). A request that
//! changes something and comes from a site that is neither this Director nor a page on this
//! computer is refused (403), whoever the peer is. This is what stops a web page the
//! director happens to have open from pressing buttons in Grid.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::{HeaderMap, Method, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// Where a request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The Grid computer itself.
    Local,
    /// Anywhere else.
    Network,
}

/// What the gate worked out about a request. Handlers read it from the request's extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub origin: Origin,
    /// Whether the caller showed the director's token. Always `false` for a local caller,
    /// who needs none.
    pub director: bool,
}

impl Access {
    /// Whether this caller may use the whole Director: local, or a visitor with the token.
    pub fn full(self) -> bool {
        self.origin == Origin::Local || self.director
    }
}

/// The name of the cookie that carries the director's token for a visitor's browser. A
/// browser sends it by itself with the requests a page cannot add a header to: the change
/// feed, a video, a picture.
pub const SESSION_COOKIE: &str = "gridfpv_director";

/// Wrong tokens allowed from one address inside [`FAILURE_WINDOW`].
pub const MAX_FAILURES: u32 = 10;
/// How long wrong tries are remembered.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// The most addresses remembered at once. Past this the oldest are forgotten.
const MAX_TRACKED: usize = 1024;

/// Wrong-token tries, by address.
#[derive(Clone, Default)]
pub struct Guesses {
    seen: Arc<Mutex<HashMap<IpAddr, (u32, Instant)>>>,
}

impl Guesses {
    /// Whether `ip` has used up its tries for now.
    pub fn blocked(&self, ip: IpAddr, now: Instant) -> bool {
        let mut seen = self.seen.lock().expect("guesses lock poisoned");
        match seen.get(&ip) {
            Some((_, since)) if now.duration_since(*since) >= FAILURE_WINDOW => {
                seen.remove(&ip);
                false
            }
            Some((count, _)) => *count >= MAX_FAILURES,
            None => false,
        }
    }

    /// Count one wrong try from `ip`.
    pub fn failed(&self, ip: IpAddr, now: Instant) {
        let mut seen = self.seen.lock().expect("guesses lock poisoned");
        if seen.len() >= MAX_TRACKED && !seen.contains_key(&ip) {
            // Forget whoever has been quiet the longest, so the list cannot grow without end.
            if let Some(oldest) = seen
                .iter()
                .min_by_key(|(_, (_, since))| *since)
                .map(|(ip, _)| *ip)
            {
                seen.remove(&oldest);
            }
        }
        let entry = seen.entry(ip).or_insert((0, now));
        if now.duration_since(entry.1) >= FAILURE_WINDOW {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    /// A right token clears the count.
    pub fn cleared(&self, ip: IpAddr) {
        self.seen.lock().expect("guesses lock poisoned").remove(&ip);
    }
}

/// Whether a host name, as written in a `Host` header or an origin, is this computer:
/// `localhost` or a loopback address. A port and IPv6 brackets are allowed.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }
    // "[::1]:8080", "[::1]", "127.0.0.1:8080", "localhost:8080", "localhost".
    let name = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, after)) if after.is_empty() || is_port(after) => inside,
            _ => return false,
        }
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if is_port(&format!(":{port}")) => name,
            Some(_) => return false,
            None => host,
        }
    };
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    name.parse::<IpAddr>()
        .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// `":8080"`: a colon and a port number.
fn is_port(text: &str) -> bool {
    text.strip_prefix(':')
        .is_some_and(|port| !port.is_empty() && port.parse::<u16>().is_ok())
}

/// Headers a proxy adds. Any of them means the peer address is the proxy's, not the caller's.
const PROXY_HEADERS: [&str; 6] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
    "via",
];

/// The host a request was sent to: the `Host` header, which must agree with the address in
/// the request line when both are there. `None` when there is none, more than one, or they
/// disagree.
fn requested_host(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    let mut hosts = headers.get_all(HOST).iter();
    let header = match (hosts.next(), hosts.next()) {
        (Some(one), None) => Some(one.to_str().ok()?.to_string()),
        (None, _) => None,
        (Some(_), Some(_)) => return None,
    };
    let line = uri
        .authority()
        .map(|authority| authority.as_str().to_string());
    match (header, line) {
        (Some(header), Some(line)) if !header.eq_ignore_ascii_case(&line) => None,
        (Some(header), _) => Some(header),
        (None, Some(line)) => Some(line),
        (None, None) => None,
    }
}

/// Where a request came from. See the module docs for the three things that must hold for
/// [`Origin::Local`]; everything else is [`Origin::Network`].
pub fn classify(peer: Option<SocketAddr>, headers: &HeaderMap, uri: &Uri) -> Origin {
    let Some(peer) = peer else {
        return Origin::Network;
    };
    if !peer.ip().to_canonical().is_loopback() {
        return Origin::Network;
    }
    if PROXY_HEADERS.iter().any(|name| headers.contains_key(*name)) {
        return Origin::Network;
    }
    match requested_host(headers, uri) {
        Some(host) if is_loopback_host(&host) => Origin::Local,
        _ => Origin::Network,
    }
}

/// Percent-decode a path once, for checking only. Bytes that do not decode are kept.
fn decoded(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            ) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path that is written in a way no page of ours writes one: a `..` or `.` step, an empty
/// step, a backslash, or an escaped slash or percent sign. Such a path is never public,
/// because what it names depends on who reads it.
fn is_odd_path(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") || lower.contains("%25") {
        return true;
    }
    let path = decoded(raw);
    if path.contains('\\') || path.contains('\0') {
        return true;
    }
    let mut steps = path.split('/');
    // A path starts with "/", so the first step is empty. A single trailing "/" is fine.
    steps.next();
    let steps: Vec<&str> = steps.collect();
    steps
        .iter()
        .enumerate()
        .any(|(i, step)| *step == ".." || *step == "." || (step.is_empty() && i + 1 < steps.len()))
}

/// The API paths anyone on the network may read.
const PUBLIC_READS: [&str; 3] = ["/health", "/about", "/time"];
/// Everything under here is made for visitors.
pub const PUBLIC_PREFIX: &str = "/public";
/// Signing in and out as the director, from a visitor's browser. Open to visitors because
/// it is how one stops being one; the handler checks the token itself.
pub const SESSION_PATH: &str = "/public/session";
/// Signing in and out as a pilot, with the pilot's own code (see `pilot_access`).
pub const PILOT_SESSION_PATH: &str = "/public/pilot/session";
/// What a pilot does for themselves, such as signing up for a heat. A visitor may send
/// these, because a pilot is a visitor to the gate. Each route checks for itself who the
/// pilot is, from their sign-in, and does nothing without one.
pub const PILOT_PREFIX: &str = "/public/pilot/";

/// Whether a visitor with no token may make this request of the **API**.
pub fn is_public_api(method: &Method, raw_path: &str) -> bool {
    if is_odd_path(raw_path) {
        return false;
    }
    let path = decoded(raw_path);
    let read = method == Method::GET || method == Method::HEAD;
    if path == SESSION_PATH || path == PILOT_SESSION_PATH || path.starts_with(PILOT_PREFIX) {
        return read || method == Method::POST || method == Method::DELETE;
    }
    if !read {
        return false;
    }
    PUBLIC_READS.contains(&path.as_str())
        || path == PUBLIC_PREFIX
        || path
            .strip_prefix(PUBLIC_PREFIX)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether a visitor with no token may make this request of the **console's files**: any
/// plain read. Only a request that no API route answered gets here.
pub fn may_read_files(method: &Method, raw_path: &str) -> bool {
    (method == Method::GET || method == Method::HEAD) && !is_odd_path(raw_path)
}

/// The token a request carries: a `Bearer` header first, else the session cookie.
pub fn presented_token(headers: &HeaderMap) -> Option<String> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("Bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim().to_string());
    if bearer.is_some() {
        return bearer;
    }
    cookie_token(headers)
}

/// Every token a request carries: the `Bearer` header's, then the session cookie's. A
/// caller is the director if **either** is good. A browser that is signed in by cookie may
/// still send an old token it once had typed in, and that old token must not undo the
/// sign-in.
pub fn presented_tokens(headers: &HeaderMap) -> Vec<String> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("Bearer") && !token.trim().is_empty())
        .map(|(_, token)| token.trim().to_string());
    let mut tokens: Vec<String> = bearer.into_iter().collect();
    if let Some(cookie) = cookie_token(headers)
        && !tokens.contains(&cookie)
    {
        tokens.push(cookie);
    }
    tokens
}

/// The session cookie's value, if the request has one.
pub fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, value)| *name == SESSION_COOKIE && !value.is_empty())
        .map(|(_, value)| value.to_string())
}

/// Whether a request that changes something came from a page it may come from: this
/// Director's own pages, or a page on this computer. A request with no `Origin` header is
/// not from a web page in another site (a browser always sends it for those), so it passes.
pub fn same_site(headers: &HeaderMap, uri: &Uri) -> bool {
    let mut origins = headers.get_all(ORIGIN).iter();
    let origin = match (origins.next(), origins.next()) {
        (None, _) => return true,
        (Some(one), None) => match one.to_str() {
            Ok(text) => text.trim(),
            Err(_) => return false,
        },
        (Some(_), Some(_)) => return false,
    };
    // "scheme://host[:port]". "null" (a sandboxed page, a file) is never ours.
    let Some((scheme, host)) = origin.split_once("://") else {
        return false;
    };
    if host.is_empty() || host.contains('/') {
        return false;
    }
    // The desktop app's own window, on the systems where it has a scheme of its own.
    if scheme.eq_ignore_ascii_case("tauri") {
        return true;
    }
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    if is_loopback_host(host) {
        return true;
    }
    requested_host(headers, uri).is_some_and(|requested| requested.eq_ignore_ascii_case(host))
}

fn refuse(code: ErrorCode, message: &str) -> Response {
    ProtocolError::new(code, message).into_response()
}

/// Which part of the app a request reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    Api,
    Files,
}

/// The gate on every API route. See the module docs.
pub async fn gate_api(
    State(registry): State<EventRegistry>,
    request: Request,
    next: Next,
) -> Response {
    gate(Part::Api, registry, request, next).await
}

/// The gate on the console's files and pages. See the module docs.
pub async fn gate_files(
    State(registry): State<EventRegistry>,
    request: Request,
    next: Next,
) -> Response {
    gate(Part::Files, registry, request, next).await
}

/// Put the gate around a whole app: `api` is every API route, `files` answers everything
/// else. This is the only way the Director's wiring builds what it serves.
pub fn guard(api: Router, files: Router, registry: &EventRegistry) -> Router {
    let on_api = axum::middleware::from_fn_with_state(registry.clone(), gate_api);
    let on_files = axum::middleware::from_fn_with_state(registry.clone(), gate_files);
    api.route_layer(on_api)
        .fallback_service(files.layer(on_files))
}

async fn gate(part: Part, registry: EventRegistry, mut request: Request, next: Next) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    let origin = classify(peer, request.headers(), request.uri());
    let method = request.method().clone();
    let reads = method == Method::GET || method == Method::HEAD || method == Method::OPTIONS;

    if !reads && !same_site(request.headers(), request.uri()) {
        return refuse(
            ErrorCode::Forbidden,
            "This request came from another website, so Grid did not act on it.",
        );
    }

    if origin == Origin::Local {
        request.extensions_mut().insert(Access {
            origin,
            director: false,
        });
        return next.run(request).await;
    }

    // A visitor. The address is the peer's; with none known they all share one count.
    let ip = peer.map_or(IpAddr::from([0, 0, 0, 0]), |peer| peer.ip().to_canonical());
    let guesses = registry.guesses();
    let now = Instant::now();
    let public = match part {
        Part::Api => is_public_api(&method, request.uri().path()),
        Part::Files => may_read_files(&method, request.uri().path()),
    };
    let tokens = presented_tokens(request.headers());

    let mut director = false;
    if !tokens.is_empty() {
        if guesses.blocked(ip, now) {
            if !public {
                return refuse(
                    ErrorCode::TooManyRequests,
                    "Too many wrong access tokens from this device. Wait a minute and try again.",
                );
            }
        } else if tokens.iter().any(|token| {
            registry
                .tokens()
                .session(token)
                .is_some_and(|session| session.role.can_control())
        }) {
            guesses.cleared(ip);
            director = true;
        } else {
            guesses.failed(ip, now);
        }
    }

    if !director && !public {
        return refuse(
            ErrorCode::Unauthorized,
            "Only the director can do this from another device. It needs the director's access token.",
        );
    }
    request.extensions_mut().insert(Access { origin, director });
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }
    fn peer(text: &str) -> Option<SocketAddr> {
        Some(text.parse().unwrap())
    }
    fn uri(text: &str) -> Uri {
        text.parse().unwrap()
    }

    #[test]
    fn loopback_hosts_are_recognised_in_every_spelling() {
        for host in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "localhost:8080",
            "Localhost.:8080",
            "127.0.0.1",
            "127.0.0.1:8080",
            "127.9.9.9:1",
            "[::1]",
            "[::1]:8080",
            "[::ffff:127.0.0.1]:8080",
            " 127.0.0.1:8080 ",
        ] {
            assert!(is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn everything_else_is_not_a_loopback_host() {
        for host in [
            "",
            " ",
            "192.168.1.5",
            "192.168.1.5:8080",
            "10.0.0.1",
            "0.0.0.0",
            "0.0.0.0:8080",
            "evil.example",
            "evil.example:8080",
            "localhost.evil.example",
            "127.0.0.1.evil.example",
            "notlocalhost",
            "localhost:notaport",
            "localhost:",
            "localhost:99999",
            "127.0.0.1:8080:9",
            "[::1",
            "[::1]x",
            "[::1]:",
            "::1",
            "[2001:db8::1]:8080",
            "[::ffff:192.168.1.5]",
            "localhost@evil.example",
            "127.0.0.1@evil.example",
            "128.0.0.1",
            "1.127.0.0.1",
        ] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn a_plain_request_from_this_computer_is_local() {
        for (who, host) in [
            ("127.0.0.1:50000", "127.0.0.1:8080"),
            ("127.0.0.1:50000", "localhost:8080"),
            ("[::1]:50000", "[::1]:8080"),
            ("[::1]:50000", "localhost"),
            ("[::ffff:127.0.0.1]:50000", "127.0.0.1:8080"),
            ("127.0.0.2:50000", "127.0.0.1:8080"),
        ] {
            assert_eq!(
                classify(peer(who), &headers(&[("host", host)]), &uri("/events")),
                Origin::Local,
                "{who} asking for {host}"
            );
        }
    }

    #[test]
    fn a_peer_that_is_not_this_computer_is_a_visitor_whatever_it_claims() {
        for who in [
            "192.168.1.20:50000",
            "10.0.0.7:1",
            "172.16.0.3:1",
            "169.254.3.4:1",
            "8.8.8.8:1",
            "0.0.0.0:1",
            "[fe80::1]:1",
            "[2001:db8::7]:1",
            "[::ffff:192.168.1.20]:1",
            "[::]:1",
        ] {
            for host in ["127.0.0.1:8080", "localhost", "192.168.1.5:8081"] {
                assert_eq!(
                    classify(peer(who), &headers(&[("host", host)]), &uri("/events")),
                    Origin::Network,
                    "{who} asking for {host}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_peer_is_a_visitor() {
        assert_eq!(
            classify(None, &headers(&[("host", "127.0.0.1:8080")]), &uri("/")),
            Origin::Network
        );
    }

    #[test]
    fn a_request_passed_on_by_a_proxy_is_a_visitor() {
        for name in PROXY_HEADERS {
            assert_eq!(
                classify(
                    peer("127.0.0.1:50000"),
                    &headers(&[("host", "127.0.0.1:8080"), (name, "127.0.0.1")]),
                    &uri("/events")
                ),
                Origin::Network,
                "{name}"
            );
            // Header names are not case sensitive.
            assert_eq!(
                classify(
                    peer("127.0.0.1:50000"),
                    &headers(&[("host", "localhost"), (&name.to_uppercase(), "")]),
                    &uri("/events")
                ),
                Origin::Network,
                "{name} in capitals"
            );
        }
    }

    #[test]
    fn a_loopback_peer_asking_for_another_name_is_a_visitor() {
        // DNS rebinding: the browser is on this computer, but the page is someone else's.
        for host in [
            "evil.example:8080",
            "192.168.1.5:8080",
            "gridfpv.local:8080",
            "localhost.evil.example",
            "",
        ] {
            assert_eq!(
                classify(
                    peer("127.0.0.1:50000"),
                    &headers(&[("host", host)]),
                    &uri("/events")
                ),
                Origin::Network,
                "{host}"
            );
        }
    }

    #[test]
    fn no_host_two_hosts_or_hosts_that_disagree_make_a_visitor() {
        let who = peer("127.0.0.1:50000");
        assert_eq!(
            classify(who, &headers(&[]), &uri("/events")),
            Origin::Network
        );
        assert_eq!(
            classify(
                who,
                &headers(&[("host", "127.0.0.1:8080"), ("host", "127.0.0.1:8080")]),
                &uri("/events")
            ),
            Origin::Network
        );
        // The request line names one host and the header another.
        assert_eq!(
            classify(
                who,
                &headers(&[("host", "127.0.0.1:8080")]),
                &uri("http://evil.example:8080/events")
            ),
            Origin::Network
        );
        // They agree, or only the request line has it (HTTP/2).
        assert_eq!(
            classify(
                who,
                &headers(&[("host", "127.0.0.1:8080")]),
                &uri("http://127.0.0.1:8080/events")
            ),
            Origin::Local
        );
        assert_eq!(
            classify(who, &headers(&[]), &uri("http://localhost:8080/events")),
            Origin::Local
        );
    }

    #[test]
    fn a_visitor_may_read_the_console_files() {
        for path in [
            "/",
            "/index.html",
            "/assets/index-abc123.js",
            "/favicon.ico",
            "/some/console/page",
            "/some/page/",
            "/healthz",
            "/publicx",
        ] {
            assert!(may_read_files(&Method::GET, path), "GET {path}");
            assert!(may_read_files(&Method::HEAD, path), "HEAD {path}");
            for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
                assert!(!may_read_files(&method, path), "{method} {path}");
            }
        }
        for odd in ["//events", "/assets/../events", "/a/%2e%2e/b", "/a\\b"] {
            assert!(!may_read_files(&Method::GET, odd), "{odd}");
        }
    }

    #[test]
    fn a_visitor_may_read_the_public_api_paths() {
        for path in [
            "/health",
            "/about",
            "/time",
            "/public",
            "/public/hello",
            "/public/events/night-race/state",
            "/public/session",
        ] {
            assert!(is_public_api(&Method::GET, path), "GET {path}");
            assert!(is_public_api(&Method::HEAD, path), "HEAD {path}");
        }
    }

    #[test]
    fn a_visitor_may_not_read_anything_else() {
        for path in [
            "/events",
            "/events/",
            "/events/night-race/state",
            "/events/night-race/changes",
            "/events/night-race/signal",
            "/active-event",
            "/pilots",
            "/classes",
            "/timers",
            "/timers/gate/live",
            "/timers/discovered",
            "/formats",
            "/video/settings",
            "/video/status",
            "/overlay/look",
            "/overlay/logo/left",
            "/network",
            "/backup",
            "/diagnostics",
            "/",
            "/index.html",
            "/data",
            "/data/export/copy.zip",
            "/formats",
            "/heats",
            "/health/",
            "/health/deep",
            "/about/x",
            "/time/x",
            "/publicx",
            "/public-events",
            "",
        ] {
            assert!(!is_public_api(&Method::GET, path), "GET {path}");
        }
    }

    #[test]
    fn a_visitor_may_not_change_anything_even_under_public() {
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::CONNECT,
            Method::TRACE,
        ] {
            for path in [
                "/public",
                "/public/hello",
                "/health",
                "/events",
                "/events/night-race/commands",
                "/network",
            ] {
                assert!(!is_public_api(&method, path), "{method} {path}");
            }
        }
        // Signing in and out is the one thing a visitor sends.
        assert!(is_public_api(&Method::POST, SESSION_PATH));
        assert!(is_public_api(&Method::DELETE, SESSION_PATH));
        assert!(!is_public_api(&Method::PUT, SESSION_PATH));
        assert!(!is_public_api(&Method::PATCH, SESSION_PATH));
        // What a pilot does for themselves is theirs to send; the route checks who they are.
        assert!(is_public_api(&Method::POST, "/public/pilot/signup/take"));
        assert!(is_public_api(&Method::POST, "/public/pilot/session"));
        assert!(is_public_api(&Method::DELETE, "/public/pilot/session"));
        assert!(!is_public_api(&Method::PUT, "/public/pilot/signup/take"));
        assert!(!is_public_api(&Method::PATCH, "/public/pilot/signup/take"));
        // Only under that prefix, written plainly.
        assert!(!is_public_api(&Method::POST, "/public/pilot"));
        assert!(!is_public_api(&Method::POST, "/public/pilots/x"));
        assert!(!is_public_api(&Method::POST, "/public/pilot/../../events"));
        assert!(!is_public_api(
            &Method::POST,
            "/public/pilot/..%2f..%2fevents"
        ));
        assert!(!is_public_api(&Method::POST, "/public/event/state"));
        assert!(!is_public_api(&Method::POST, "/public/session/"));
        assert!(!is_public_api(&Method::POST, "/public/session/x"));
    }

    #[test]
    fn a_path_written_to_fool_the_check_is_never_public() {
        for path in [
            "/public/../events",
            "/public/../../events",
            "/public/./../events",
            "/public/%2e%2e/events",
            "/public/%2E%2E/events",
            "/public/..%2fevents",
            "/public/..%2Fevents",
            "/public%2f..%2fevents",
            "/public/..\\events",
            "/public/%5c..%5cevents",
            "/public//events",
            "//events",
            "///events",
            "/./events",
            "/../events",
            "/assets/../events",
            "/assets/..%2fevents",
            "/%2e%2e/events",
            "/public/%252e%252e/events",
            "/x/%00/events",
            "/public/hello/..",
            "/public/hello/.",
        ] {
            assert!(!is_public_api(&Method::GET, path), "{path}");
        }
        // An escaped letter spells an API path: checked as what it spells.
        assert!(!is_public_api(&Method::GET, "/%65vents"));
        assert!(!is_public_api(&Method::GET, "/e%76ents/x/state"));
        assert!(!is_public_api(&Method::GET, "/%70ilots"));
        // And an escaped letter in a public path is still public.
        assert!(is_public_api(&Method::GET, "/publi%63/hello"));
    }

    #[test]
    fn the_token_is_read_from_a_bearer_header_or_the_cookie() {
        assert_eq!(
            presented_token(&headers(&[("authorization", "Bearer abc")])).as_deref(),
            Some("abc")
        );
        assert_eq!(
            presented_token(&headers(&[("authorization", "bearer   abc  ")])).as_deref(),
            Some("abc")
        );
        assert_eq!(
            presented_token(&headers(&[("cookie", "gridfpv_director=abc")])).as_deref(),
            Some("abc")
        );
        assert_eq!(
            presented_token(&headers(&[(
                "cookie",
                "theme=dark; gridfpv_director=abc; other=1"
            )]))
            .as_deref(),
            Some("abc")
        );
        // Two cookie headers.
        assert_eq!(
            presented_token(&headers(&[
                ("cookie", "a=1"),
                ("cookie", "gridfpv_director=abc")
            ]))
            .as_deref(),
            Some("abc")
        );
        // The header wins over the cookie.
        assert_eq!(
            presented_token(&headers(&[
                ("authorization", "Bearer header"),
                ("cookie", "gridfpv_director=cookie")
            ]))
            .as_deref(),
            Some("header")
        );
        for none in [
            headers(&[]),
            headers(&[("authorization", "Basic abc")]),
            headers(&[("authorization", "Bearer")]),
            headers(&[("authorization", "Bearer ")]),
            headers(&[("authorization", "abc")]),
            headers(&[("cookie", "gridfpv_director=")]),
            headers(&[("cookie", "xgridfpv_director=abc")]),
            headers(&[("cookie", "gridfpv_directorx=abc")]),
            headers(&[("cookie", "other=gridfpv_director=abc")]),
        ] {
            assert_eq!(presented_token(&none), None, "{none:?}");
        }
        // Both are offered when both are there, the header first, and once when they match.
        assert_eq!(
            presented_tokens(&headers(&[
                ("authorization", "Bearer header"),
                ("cookie", "gridfpv_director=cookie")
            ])),
            ["header", "cookie"]
        );
        assert_eq!(
            presented_tokens(&headers(&[
                ("authorization", "Bearer same"),
                ("cookie", "gridfpv_director=same")
            ])),
            ["same"]
        );
        assert_eq!(
            presented_tokens(&headers(&[("cookie", "gridfpv_director=cookie")])),
            ["cookie"]
        );
        assert!(presented_tokens(&headers(&[("authorization", "Basic abc")])).is_empty());
        assert!(presented_tokens(&headers(&[])).is_empty());
        // A bad header falls back to the cookie.
        assert_eq!(
            presented_token(&headers(&[
                ("authorization", "Basic abc"),
                ("cookie", "gridfpv_director=cookie")
            ]))
            .as_deref(),
            Some("cookie")
        );
    }

    #[test]
    fn a_change_from_our_own_page_or_this_computer_passes() {
        let ours = |origin: &str, host: &str| {
            same_site(&headers(&[("origin", origin), ("host", host)]), &uri("/x"))
        };
        // No Origin at all: not a web page on another site.
        assert!(same_site(
            &headers(&[("host", "127.0.0.1:8080")]),
            &uri("/x")
        ));
        assert!(ours("http://127.0.0.1:8080", "127.0.0.1:8080"));
        assert!(ours("http://192.168.1.5:8081", "192.168.1.5:8081"));
        assert!(ours("http://GRID.local:8081", "grid.local:8081"));
        // A page on this computer, such as the dev server, calling the Director.
        assert!(ours("http://localhost:5173", "127.0.0.1:8080"));
        assert!(ours("http://127.0.0.1:5173", "192.168.1.5:8081"));
        assert!(ours("http://[::1]:5173", "127.0.0.1:8080"));
        assert!(ours("tauri://localhost", "127.0.0.1:8080"));
        assert!(ours("https://localhost", "localhost"));
    }

    #[test]
    fn a_change_from_another_site_does_not_pass() {
        let theirs = |origin: &str, host: &str| {
            !same_site(&headers(&[("origin", origin), ("host", host)]), &uri("/x"))
        };
        assert!(theirs("http://evil.example", "127.0.0.1:8080"));
        assert!(theirs("https://evil.example", "127.0.0.1:8080"));
        assert!(theirs("http://evil.example:8080", "127.0.0.1:8080"));
        // Same host, another port, is another site.
        assert!(theirs("http://192.168.1.5:9000", "192.168.1.5:8081"));
        assert!(theirs("http://192.168.1.5", "192.168.1.5:8081"));
        assert!(theirs("http://192.168.1.6:8081", "192.168.1.5:8081"));
        assert!(theirs("null", "127.0.0.1:8080"));
        assert!(theirs("", "127.0.0.1:8080"));
        assert!(theirs("evil.example", "127.0.0.1:8080"));
        assert!(theirs("file://", "127.0.0.1:8080"));
        assert!(theirs("chrome-extension://abcdef", "127.0.0.1:8080"));
        assert!(theirs("http://localhost.evil.example", "127.0.0.1:8080"));
        assert!(theirs("http://127.0.0.1.evil.example", "127.0.0.1:8080"));
        assert!(theirs("http://127.0.0.1:8080/path", "127.0.0.1:8080"));
        assert!(theirs("http://localhost@evil.example", "127.0.0.1:8080"));
        // Two Origin headers: refuse, not pick one.
        assert!(!same_site(
            &headers(&[
                ("origin", "http://127.0.0.1:8080"),
                ("origin", "http://evil.example"),
                ("host", "127.0.0.1:8080")
            ]),
            &uri("/x")
        ));
        // Their page with their name pointed at us (rebinding): same origin by the browser's
        // rule. It passes here and is stopped by `classify`, which makes it a visitor.
        assert!(!theirs("http://evil.example:8080", "evil.example:8080"));
    }

    #[test]
    fn wrong_tokens_are_counted_per_address_and_forgotten_after_the_window() {
        let guesses = Guesses::default();
        let a: IpAddr = "192.168.1.20".parse().unwrap();
        let b: IpAddr = "192.168.1.21".parse().unwrap();
        let start = Instant::now();
        for n in 0..MAX_FAILURES {
            assert!(!guesses.blocked(a, start), "still allowed after {n} tries");
            guesses.failed(a, start);
        }
        assert!(guesses.blocked(a, start));
        // Another address is not touched.
        assert!(!guesses.blocked(b, start));
        // Just before the window ends it still holds; at the end it lets go.
        let nearly = start + FAILURE_WINDOW - Duration::from_millis(1);
        assert!(guesses.blocked(a, nearly));
        assert!(!guesses.blocked(a, start + FAILURE_WINDOW));
        // And the count starts again from nothing.
        guesses.failed(a, start + FAILURE_WINDOW);
        assert!(!guesses.blocked(a, start + FAILURE_WINDOW));
    }

    #[test]
    fn a_right_token_clears_the_count() {
        let guesses = Guesses::default();
        let a: IpAddr = "192.168.1.20".parse().unwrap();
        let now = Instant::now();
        for _ in 0..MAX_FAILURES - 1 {
            guesses.failed(a, now);
        }
        guesses.cleared(a);
        guesses.failed(a, now);
        assert!(!guesses.blocked(a, now));
    }

    #[test]
    fn a_try_after_the_window_starts_a_new_count() {
        let guesses = Guesses::default();
        let a: IpAddr = "192.168.1.20".parse().unwrap();
        let start = Instant::now();
        for _ in 0..MAX_FAILURES - 1 {
            guesses.failed(a, start);
        }
        // One more, but a window later: it is the first of a new count, not the tenth.
        guesses.failed(a, start + FAILURE_WINDOW);
        assert!(!guesses.blocked(a, start + FAILURE_WINDOW));
    }

    #[test]
    fn the_list_of_addresses_cannot_grow_without_end() {
        let guesses = Guesses::default();
        let start = Instant::now();
        for n in 0..(MAX_TRACKED as u32 + 500) {
            let ip = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + n));
            guesses.failed(ip, start + Duration::from_micros(u64::from(n)));
        }
        assert!(guesses.seen.lock().unwrap().len() <= MAX_TRACKED);
    }

    #[test]
    fn full_access_is_local_or_a_visitor_with_the_token() {
        let access = |origin, director| Access { origin, director }.full();
        assert!(access(Origin::Local, false));
        assert!(access(Origin::Network, true));
        assert!(!access(Origin::Network, false));
    }
}
