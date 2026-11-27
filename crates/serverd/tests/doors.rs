//! Every route of the server, and who may use it (docs/primitives.md §13.2, §13.6, §13.7,
//! §13.9).
//!
//! The tests beside this one check each feature in depth. This one checks the whole
//! server at once, so that nothing is left open by being forgotten:
//!
//! - **every route in `app.rs` is listed here**, with who it is for. A route that is added
//!   and not listed fails the test, and so does one that is listed and no longer there;
//! - each route turns away everyone it is not for, with `401` for nobody and `403` for
//!   somebody without the right;
//! - each route lets in the least person it is for;
//! - every route that changes something refuses a request made by another website.

use std::collections::BTreeSet;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_serverd::accounts::{InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Who a route is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    /// Anyone, signed in or not.
    Anyone,
    /// Any signed-in account.
    Account,
    /// A Director, with a link token.
    Token,
    /// Any member of the club in the address.
    Member,
    /// A director or a claims approver of the club.
    Answers,
    /// A director of the club, or an owner of the server.
    Runs,
    /// A signed-in account that holds a pilot the event names.
    PilotOfEvent,
    /// An owner of the server, and nobody else: not even a club's director.
    Owner,
}

/// Every route, who it is for, and a body it would take.
fn doors() -> Vec<(Method, &'static str, Door, Option<Value>)> {
    use Door::*;
    let seat = json!({"adapter": "bench", "seat": "seat-1"});
    let public = "/api/clubs/{club}/events/{event}/public";
    let leak = |path: String| -> &'static str { Box::leak(path.into_boxed_str()) };
    let mut all = vec![
        (Method::GET, "/health", Anyone, None),
        (Method::GET, "/public/hello", Anyone, None),
        // ── A Director's routes ─────────────────────────────────────────────
        (Method::GET, "/push/hello", Token, None),
        (Method::GET, "/push/events", Token, None),
        (Method::GET, "/push/events/{id}", Token, None),
        (
            Method::PUT,
            "/push/events/{id}",
            Token,
            Some(json!({"meta": {"id": "x", "name": "X", "created_at": 1, "persistent": true}})),
        ),
        (Method::DELETE, "/push/events/{id}", Token, None),
        (
            Method::POST,
            "/push/events/{id}/log",
            Token,
            Some(json!({"from": 0, "before": "00", "entries": []})),
        ),
        (Method::GET, "/push/pilots", Token, None),
        (Method::PUT, "/push/pilots", Token, Some(json!([]))),
        (Method::GET, "/push/members", Token, None),
        (Method::GET, "/push/listen", Token, None),
        (Method::GET, "/push/requests", Token, None),
        (Method::POST, "/push/requests/{id}", Token, Some(json!({}))),
        (
            Method::PUT,
            "/push/pilots/{pilot}/account",
            Token,
            Some(json!({"account": "acct"})),
        ),
        (Method::DELETE, "/push/pilots/{pilot}/account", Token, None),
        // ── Reading, for anyone ─────────────────────────────────────────────
        (Method::GET, "/api/clubs", Anyone, None),
        (Method::GET, "/api/clubs/{club}/events", Anyone, None),
        (
            Method::GET,
            "/api/clubs/{club}/events/{event}",
            Anyone,
            None,
        ),
        (Method::GET, "/api/sign-in/providers", Anyone, None),
        (Method::GET, "/api/server", Anyone, None),
        (Method::GET, "/auth/{provider}/start", Anyone, None),
        (Method::GET, "/auth/{provider}/callback", Anyone, None),
        (Method::GET, "/api/me", Anyone, None),
        (Method::POST, "/api/sign-out", Anyone, None),
        (
            Method::POST,
            "/api/email/confirm",
            Anyone,
            Some(json!({"secret": "gfe_nothing"})),
        ),
        (
            Method::POST,
            "/api/password/forgot",
            Anyone,
            Some(json!({"who": "doorman"})),
        ),
        (
            Method::POST,
            "/api/sign-in/password",
            Anyone,
            Some(json!({"username": "doorman", "password": "a long password"})),
        ),
        (
            Method::POST,
            "/api/sign-up/password",
            Anyone,
            Some(json!({"username": "newcomer", "password": "a long password"})),
        ),
        (Method::GET, "/api/invites/{secret}", Anyone, None),
        (Method::GET, "/api/reconnect/{secret}", Anyone, None),
        (Method::GET, "/api/clubs/{club}", Anyone, None),
        (Method::GET, "/api/club-words/{word}", Anyone, None),
        // ── Clubs themselves ────────────────────────────────────────────────
        (
            Method::POST,
            "/api/clubs",
            Owner,
            Some(json!({"name": "West FPV"})),
        ),
        (
            Method::PUT,
            "/api/server/password-sign-in",
            Owner,
            Some(json!({"on": true})),
        ),
        (
            Method::PUT,
            "/api/server/sign-up",
            Owner,
            Some(json!({"open": true})),
        ),
        (
            Method::PUT,
            "/api/clubs/{club}/archived",
            Owner,
            Some(json!({"archived": true})),
        ),
        // ── Helping someone who cannot sign in ──────────────────────────────
        (Method::GET, "/api/accounts/{account}/sign-ins", Owner, None),
        (
            Method::DELETE,
            "/api/accounts/{account}/sign-ins/{provider}/{subject}",
            Owner,
            None,
        ),
        (
            Method::POST,
            "/api/accounts/{account}/reconnect",
            Owner,
            None,
        ),
        (
            Method::DELETE,
            "/api/accounts/{account}/reconnect",
            Owner,
            None,
        ),
        // ── An account's own things ─────────────────────────────────────────
        (
            Method::PATCH,
            "/api/me",
            Account,
            Some(json!({"name": "New"})),
        ),
        (
            Method::DELETE,
            "/api/me/sign-ins/{provider}/{subject}",
            Account,
            None,
        ),
        (
            Method::PUT,
            "/api/me/primary-club",
            Account,
            Some(json!({"club": "north"})),
        ),
        (
            Method::PUT,
            "/api/me/profile",
            Account,
            Some(json!({"callsign": "Ace"})),
        ),
        (
            Method::PUT,
            "/api/me/email",
            Account,
            Some(json!({"address": "pilot@example.org"})),
        ),
        (Method::DELETE, "/api/me/email", Account, None),
        (
            Method::PUT,
            "/api/me/password",
            Account,
            Some(json!({"username": "newname", "password": "a long password"})),
        ),
        (Method::POST, "/api/invites/{secret}/accept", Account, None),
        (Method::POST, "/api/clubs/{club}/join", Account, None),
        (Method::DELETE, "/api/clubs/{club}/join", Account, None),
        (Method::GET, "/api/me/results", Account, None),
        // ── Running a club ──────────────────────────────────────────────────
        (
            Method::PATCH,
            "/api/clubs/{club}",
            Runs,
            Some(json!({"name": "New name"})),
        ),
        (Method::GET, "/api/clubs/{club}/tokens", Runs, None),
        (
            Method::POST,
            "/api/clubs/{club}/tokens",
            Runs,
            Some(json!({"label": "laptop"})),
        ),
        (Method::DELETE, "/api/clubs/{club}/tokens/{id}", Runs, None),
        (
            Method::PUT,
            "/api/clubs/{club}/events/{event}/listing",
            Runs,
            Some(json!({"hidden": true})),
        ),
        (
            Method::DELETE,
            "/api/clubs/{club}/events/{event}",
            Runs,
            None,
        ),
        (Method::GET, "/api/clubs/{club}/invites", Runs, None),
        (
            Method::POST,
            "/api/clubs/{club}/invites",
            Runs,
            Some(json!({"kind": "Personal", "label": "Someone"})),
        ),
        (Method::DELETE, "/api/clubs/{club}/invites/{id}", Runs, None),
        (Method::GET, "/api/clubs/{club}/members", Runs, None),
        (
            Method::POST,
            "/api/clubs/{club}/members",
            Runs,
            Some(json!({"account": "acct_nobody"})),
        ),
        (
            Method::PUT,
            "/api/clubs/{club}/open",
            Runs,
            Some(json!({"open": true})),
        ),
        (Method::GET, "/api/clubs/{club}/join-asks", Runs, None),
        (
            Method::PUT,
            "/api/clubs/{club}/join-asks/{account}",
            Runs,
            None,
        ),
        (
            Method::DELETE,
            "/api/clubs/{club}/join-asks/{account}",
            Runs,
            None,
        ),
        (Method::GET, "/api/clubs/{club}/people", Runs, None),
        (
            Method::PUT,
            "/api/clubs/{club}/members/{account}",
            Runs,
            Some(json!({"role": "Pilot"})),
        ),
        (
            Method::DELETE,
            "/api/clubs/{club}/members/{account}",
            Runs,
            None,
        ),
        (
            Method::DELETE,
            "/api/clubs/{club}/pilots/{pilot}/holder",
            Runs,
            None,
        ),
        // ── A club's pilots ─────────────────────────────────────────────────
        (Method::GET, "/api/clubs/{club}/pilots", Member, None),
        (
            Method::POST,
            "/api/clubs/{club}/pilots/{pilot}/claim",
            Member,
            None,
        ),
        // Letting go of a pilot, or of an ask, acts only on the caller's own. It needs no
        // say in the club: someone outside it has nothing there to let go of.
        (
            Method::DELETE,
            "/api/clubs/{club}/pilots/{pilot}/claim",
            Account,
            None,
        ),
        // Saying yes to an ask is for an approver too. Giving a pilot with no ask is a
        // director's alone, which `claims.rs` checks; here the least person is an approver.
        (
            Method::PUT,
            "/api/clubs/{club}/pilots/{pilot}/holder",
            Answers,
            Some(json!({"account": "acct"})),
        ),
        (
            Method::DELETE,
            "/api/clubs/{club}/pilots/{pilot}/asks/{account}",
            Answers,
            None,
        ),
    ];
    // ── One event, for anyone to read ───────────────────────────────────────
    for read in [
        "hello",
        "event",
        "event/state",
        "event/changes",
        "event/pilots",
        "event/classes",
        "event/timers",
        "event/channels",
    ] {
        all.push((Method::GET, leak(format!("{public}/{read}")), Anyone, None));
    }
    // ── And for a pilot of the event to ask the Director ────────────────────
    for (ask, body) in [
        ("signup/take", json!({"heat": "quali/heat-1", "seat": seat})),
        ("signup/next", json!({"phase": "quali", "seat": seat})),
        ("signup/leave", json!({"heat": "quali/heat-1"})),
        (
            "checkin/take",
            json!({"race": "practice#1", "adapter": "bench", "seat": "seat-1"}),
        ),
        ("checkin/drop", json!({"race": "practice#1"})),
    ] {
        all.push((
            Method::POST,
            leak(format!("{public}/pilot/{ask}")),
            PilotOfEvent,
            Some(body),
        ));
    }
    all
}

// ── Reading the routes out of `app.rs` ──────────────────────────────────────

/// The text of the call that opens at `open` (a `(`), up to the `)` that closes it.
fn call(text: &str, open: usize) -> &str {
    let mut depth = 0usize;
    for (at, byte) in text.bytes().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..=at];
                }
            }
            _ => {}
        }
    }
    panic!("a call in app.rs is never closed");
}

/// The first `"…"` in a piece of code.
fn first_string(text: &str) -> &str {
    let start = text.find('"').expect("a route has a path") + 1;
    let len = text[start..].find('"').expect("a path is closed");
    &text[start..start + len]
}

/// Every `METHOD path` the server's router is given, read from its source. A route inside
/// a `.nest(prefix, …)` has the prefix in front.
fn routes_in_source() -> BTreeSet<(String, String)> {
    let source = include_str!("../src/app.rs");
    let start = source
        .find("pub fn router(state: AppState) -> Router {")
        .expect("app.rs has the router");
    let end = start
        + source[start..]
            .find(".with_state(state)")
            .expect("the router is finished");
    let text = &source[start..end];

    let mut nests: Vec<(usize, usize, &str)> = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find(".nest(") {
        let open = at + found + ".nest".len();
        let whole = call(text, open);
        nests.push((open, open + whole.len(), first_string(whole)));
        at = open + 1;
    }

    let mut found = BTreeSet::new();
    let mut at = 0;
    while let Some(next) = text[at..].find(".route(") {
        let open = at + next + ".route".len();
        let whole = call(text, open);
        let prefix = nests
            .iter()
            .find(|(from, to, _)| *from < open && open < *to)
            .map_or("", |(_, _, prefix)| prefix);
        let path = format!("{prefix}{}", first_string(whole));
        let mut any = false;
        for method in ["get", "post", "put", "delete", "patch"] {
            // `get(handler)`, `.put(handler)`, `axum::routing::post(handler)`.
            let used = whole.match_indices(&format!("{method}(")).any(|(i, _)| {
                i == 0
                    || !whole.as_bytes()[i - 1].is_ascii_alphanumeric()
                        && whole.as_bytes()[i - 1] != b'_'
            });
            if used {
                any = true;
                found.insert((method.to_uppercase(), path.clone()));
            }
        }
        assert!(any, "no method was read for the route {path}");
        at = open + 1;
    }
    found
}

#[test]
fn every_route_the_server_has_is_listed_here_with_who_it_is_for() {
    let listed: BTreeSet<(String, String)> = doors()
        .into_iter()
        .map(|(method, path, _, _)| (method.to_string(), path.to_owned()))
        .collect();
    assert_eq!(
        listed.len(),
        doors().len(),
        "a route is listed here more than once"
    );
    let real = routes_in_source();
    assert!(real.len() >= 60, "too few routes were read from app.rs");
    let unlisted: Vec<_> = real.difference(&listed).collect();
    let gone: Vec<_> = listed.difference(&real).collect();
    assert!(
        unlisted.is_empty(),
        "these routes are in app.rs and not in doors(): say who each is for: {unlisted:?}"
    );
    assert!(
        gone.is_empty(),
        "these routes are in doors() and no longer in app.rs: {gone:?}"
    );
}

// ── A server with people in it ──────────────────────────────────────────────

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    /// Link tokens: one for each club.
    north: String,
    south: String,
    /// Sessions.
    owner: String,
    director: String,
    approver: String,
    /// A pilot of North who holds Ace, a pilot the event names.
    flier: String,
    /// A pilot of North who holds nobody.
    pilot: String,
    /// A director of South, with no place in North.
    elsewhere: String,
}

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"},{"id":"south","name":"South FPV"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    // Passwords are on here, with someone who has one, so the password routes have
    // someone to let in. Anyone may make an account, so signing up has too.
    state.book.set_password_sign_in(true).unwrap();
    state.book.set_open_sign_up(true).unwrap();
    {
        let sign_in = gridfpv_serverd::accounts::NewSignIn {
            provider: gridfpv_serverd::accounts::PASSWORD_PROVIDER,
            subject: "doorman",
            label: "doorman",
        };
        let (doorman, _) = state
            .accounts
            .join_open(&sign_in, "Doorman", now_ms())
            .unwrap();
        let hash = gridfpv_serverd::password::hash_password("a long password");
        state
            .accounts
            .set_password_hash(&doorman.id, &hash, now_ms())
            .unwrap();
    }
    let north = state.book.new_token(&club("north"), "laptop").unwrap().1;
    let south = state.book.new_token(&club("south"), "laptop").unwrap().1;
    let person = |name: &str, kind: InviteKind| -> (String, String) {
        let now = now_ms();
        let (_, secret) = state
            .accounts
            .new_invite(
                NewInvite {
                    kind,
                    label: name.into(),
                    max_uses: 1,
                    expires_at: None,
                },
                None,
                now,
            )
            .unwrap();
        let subject = format!("subject-of-{name}");
        let sign_in = NewSignIn {
            provider: "discord",
            subject: &subject,
            label: name,
        };
        let (account, _, session) = state
            .accounts
            .join_with_invite(&secret, &sign_in, name, now)
            .unwrap();
        (account.id.0, session)
    };
    let member = |name: &str, of: &str, role: Role| {
        person(
            name,
            InviteKind::Personal {
                club: club(of),
                role,
                pilot: None,
            },
        )
    };
    let owner = person("Oz", InviteKind::Owner).1;
    let director = member("Dee", "north", Role::Director).1;
    let approver = member("App", "north", Role::Approver).1;
    let (flier_id, flier) = member("Fay", "north", Role::Pilot);
    let pilot = member("Pip", "north", Role::Pilot).1;
    let elsewhere = member("Sal", "south", Role::Director).1;

    let rig = Rig {
        app: router(state.clone()),
        state,
        north,
        south,
        owner,
        director,
        approver,
        flier,
        pilot,
        elsewhere,
        _dir: dir,
    };
    // North has an event, Friday, that names Ace. Fay holds Ace.
    let header = json!({
        "meta": {"id": "friday", "name": "Friday", "created_at": 1, "persistent": true},
        "pilots": [{"id": "ace-1", "callsign": "Ace", "vtx_types": []}]
    });
    let (status, _) = send(
        &rig,
        Who::Token(&rig.north),
        &Method::PUT,
        "/push/events/friday",
        Some(&header),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    rig.state
        .accounts
        .give_pilot(
            &club("north"),
            "ace-1",
            &gridfpv_serverd::accounts::AccountId(flier_id),
            None,
            now_ms(),
        )
        .unwrap();
    rig
}

#[derive(Clone, Copy)]
enum Who<'a> {
    Nobody,
    Session(&'a str),
    Token(&'a str),
}

/// The address a route is asked at: its `{…}` parts filled in for North's Friday.
fn address(path: &str) -> String {
    path.replace("{club}", "north")
        .replace("{event}", "friday")
        .replace("{pilot}", "ace-1")
        .replace("{account}", "acct-nobody")
        .replace("{provider}", "discord")
        .replace("{subject}", "nobody")
        .replace("{secret}", "no-such-invite")
        .replace("{word}", "north-fpv")
        // An event id for a Director's own routes; a token or an invite id elsewhere.
        .replace("{id}", "friday")
}

async fn send(
    rig: &Rig,
    who: Who<'_>,
    method: &Method,
    path: &str,
    body: Option<&Value>,
    origin: Option<&str>,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method(method.clone())
        .uri(address(path))
        .header(header::HOST, "grid.example.org");
    match who {
        Who::Nobody => {}
        Who::Session(session) => {
            request = request.header(header::COOKIE, format!("{SESSION_COOKIE}={session}"));
        }
        Who::Token(token) => {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
    }
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = rig.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    // Only the start of the body: one route is a stream that never ends.
    let mut body = response.into_body();
    let text = match tokio::time::timeout(
        std::time::Duration::from_millis(200),
        http_body_util::BodyExt::frame(&mut body),
    )
    .await
    {
        Ok(Some(Ok(frame))) => frame
            .into_data()
            .map(|data| String::from_utf8_lossy(&data).into_owned())
            .unwrap_or_default(),
        _ => String::new(),
    };
    (status, text)
}

fn turned_away(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
}

#[tokio::test]
async fn each_route_turns_away_everyone_it_is_not_for() {
    for (method, path, door, body) in doors() {
        // A fresh server each time: a route that was wrongly let through must not change
        // what the next one sees.
        let rig = rig().await;
        let body = body.as_ref();
        let ask = |who| send(&rig, who, &method, path, body, None);
        let what = format!("{method} {path}");
        let sessions = [
            ("the owner", &rig.owner),
            ("a director", &rig.director),
            ("an approver", &rig.approver),
            ("a pilot", &rig.pilot),
            ("another club's director", &rig.elsewhere),
        ];
        match door {
            Door::Anyone => {}
            Door::Token => {
                let (status, _) = ask(Who::Nobody).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "nobody: {what}");
                // A sign-in is not a link token, not even an owner's.
                for (who, session) in sessions {
                    let (status, _) = ask(Who::Session(session)).await;
                    assert_eq!(status, StatusCode::UNAUTHORIZED, "{who}: {what}");
                }
                for token in ["gfs_made-up", "", "Bearer"] {
                    let (status, _) = ask(Who::Token(token)).await;
                    assert_eq!(status, StatusCode::UNAUTHORIZED, "token {token:?}: {what}");
                }
            }
            Door::Account
            | Door::Member
            | Door::Answers
            | Door::Runs
            | Door::PilotOfEvent
            | Door::Owner => {
                let (status, _) = ask(Who::Nobody).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "nobody: {what}");
                // A link token is not a sign-in.
                let (status, _) = ask(Who::Token(&rig.north)).await;
                assert_eq!(status, StatusCode::UNAUTHORIZED, "a link token: {what}");
                let (status, _) = ask(Who::Session("not-a-session")).await;
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "a made-up session: {what}"
                );

                let refused: &[(&str, &String)] = match door {
                    Door::Account => &[],
                    Door::Member => &[("another club's director", &rig.elsewhere)],
                    Door::Answers => &[
                        ("a pilot", &rig.pilot),
                        ("another club's director", &rig.elsewhere),
                    ],
                    Door::Runs => &[
                        ("a pilot", &rig.pilot),
                        ("an approver", &rig.approver),
                        ("another club's director", &rig.elsewhere),
                    ],
                    // Holding no pilot the event names: even those who run the club.
                    Door::PilotOfEvent => &[
                        ("a pilot with no pilot", &rig.pilot),
                        ("an approver", &rig.approver),
                        ("a director", &rig.director),
                        ("the owner", &rig.owner),
                        ("another club's director", &rig.elsewhere),
                    ],
                    Door::Owner => &[
                        ("a pilot", &rig.pilot),
                        ("an approver", &rig.approver),
                        ("a director", &rig.director),
                        ("another club's director", &rig.elsewhere),
                    ],
                    Door::Anyone | Door::Token => unreachable!(),
                };
                for (who, session) in refused {
                    let (status, text) = ask(Who::Session(session)).await;
                    assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {what}: {text}");
                }
            }
        }
    }
}

#[tokio::test]
async fn each_route_lets_in_the_least_person_it_is_for() {
    for (method, path, door, body) in doors() {
        let rig = rig().await;
        let body = body.as_ref();
        let what = format!("{method} {path}");
        let who = match door {
            Door::Anyone => Who::Nobody,
            Door::Token => Who::Token(&rig.north),
            Door::Account | Door::Member => Who::Session(&rig.pilot),
            Door::Answers => Who::Session(&rig.approver),
            Door::Runs => Who::Session(&rig.director),
            Door::PilotOfEvent => Who::Session(&rig.flier),
            Door::Owner => Who::Session(&rig.owner),
        };
        let (status, text) = send(&rig, who, &method, path, body, None).await;
        assert!(
            !turned_away(status),
            "{what} turned away the person it is for ({status}): {text}"
        );
        assert!(
            !status.is_server_error(),
            "{what} failed for the person it is for ({status}): {text}"
        );
        // The owner may do whatever a club's director may.
        if matches!(door, Door::Runs | Door::Answers) {
            let (status, text) =
                send(&rig, Who::Session(&rig.owner), &method, path, body, None).await;
            assert!(
                !turned_away(status),
                "{what} turned the owner away ({status}): {text}"
            );
        }
    }
}

#[tokio::test]
async fn every_route_that_changes_something_refuses_another_website() {
    let mut checked = 0;
    for (method, path, door, body) in doors() {
        if method == Method::GET {
            continue;
        }
        // A Director's routes carry a token in a header a page of another site cannot
        // set, and no cookie. They are checked the same way all the same.
        let rig = rig().await;
        let who = match door {
            Door::Token => Who::Token(&rig.north),
            Door::PilotOfEvent => Who::Session(&rig.flier),
            _ => Who::Session(&rig.owner),
        };
        for origin in [
            "https://evil.example",
            "http://grid.example.org.evil.example",
            "null",
        ] {
            let (status, text) = send(&rig, who, &method, path, body.as_ref(), Some(origin)).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {path} from {origin}: {text}"
            );
            assert!(text.contains("another website"), "{method} {path}: {text}");
        }
        // The same request from the server's own page is not refused for where it is from.
        let (status, text) = send(
            &rig,
            who,
            &method,
            path,
            body.as_ref(),
            Some("https://grid.example.org"),
        )
        .await;
        assert!(
            !text.contains("another website"),
            "{method} {path} from its own page ({status}): {text}"
        );
        checked += 1;
    }
    assert!(checked >= 25, "only {checked} changing routes were checked");
}

#[tokio::test]
async fn one_clubs_token_and_one_clubs_director_reach_nothing_of_another_club() {
    let rig = rig().await;
    // South's Director asks for North's event by its id: it is told of South's, which has
    // none.
    let (status, _) = send(
        &rig,
        Who::Token(&rig.south),
        &Method::GET,
        "/push/events/friday",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, list) = send(
        &rig,
        Who::Token(&rig.south),
        &Method::GET,
        "/push/events",
        None,
        None,
    )
    .await;
    assert_eq!(list, "[]");
    let (_, pilots) = send(
        &rig,
        Who::Token(&rig.south),
        &Method::GET,
        "/push/pilots",
        None,
        None,
    )
    .await;
    assert_eq!(pilots, "[]", "South is not told North's pilots");
    let (_, members) = send(
        &rig,
        Who::Token(&rig.south),
        &Method::GET,
        "/push/members",
        None,
        None,
    )
    .await;
    assert!(!members.contains("Fay"), "{members}");
    assert!(members.contains("Sal"), "{members}");
    // And South's Director removing "friday" does not touch North's.
    let (status, _) = send(
        &rig,
        Who::Token(&rig.south),
        &Method::DELETE,
        "/push/events/friday",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &rig,
        Who::Nobody,
        &Method::GET,
        "/api/clubs/north/events/friday",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "North's Friday is still there");
    assert_eq!(rig.state.book.tokens().len(), 2);
}

#[tokio::test]
async fn letting_go_of_a_pilot_only_ever_lets_go_of_your_own() {
    let rig = rig().await;
    let holds_ace = |rig: &Rig| {
        rig.state
            .accounts
            .claims_in(&club("north"))
            .unwrap()
            .into_iter()
            .any(|claim| claim.pilot == "ace-1" && claim.how.holds())
    };
    assert!(holds_ace(&rig));
    // Everyone but Fay, who holds Ace, tries to let go of Ace.
    for (who, session) in [
        ("the owner", &rig.owner),
        ("a director", &rig.director),
        ("an approver", &rig.approver),
        ("a pilot", &rig.pilot),
        ("another club's director", &rig.elsewhere),
    ] {
        let (status, _) = send(
            &rig,
            Who::Session(session),
            &Method::DELETE,
            "/api/clubs/north/pilots/ace-1/claim",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{who}");
        assert!(
            holds_ace(&rig),
            "{who} let go of a pilot that is not theirs"
        );
    }
    let (status, _) = send(
        &rig,
        Who::Session(&rig.flier),
        &Method::DELETE,
        "/api/clubs/north/pilots/ace-1/claim",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!holds_ace(&rig));
}

#[tokio::test]
async fn an_address_that_is_no_route_is_a_plain_not_found_and_never_a_page() {
    let rig = rig().await;
    for path in [
        "/api",
        "/api/",
        "/api/nothing",
        "/api/clubs/north/nothing",
        "/api/clubs/north/events/friday/public/nothing",
        "/api/clubs/north/events/friday/public/pilot/signup",
        "/push",
        "/push/nothing",
        "/auth",
        "/auth/discord",
        "/public/nothing",
    ] {
        for who in [
            Who::Nobody,
            Who::Session(&rig.owner),
            Who::Token(&rig.north),
        ] {
            let (status, text) = send(&rig, who, &Method::GET, path, None, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(!text.contains("<html"), "{path}: {text}");
        }
    }
    // A method a route does not take is not let through as another one.
    for (method, path) in [
        (Method::DELETE, "/api/clubs"),
        (Method::POST, "/api/clubs/north/events"),
        (Method::PUT, "/api/me"),
        (Method::POST, "/push/hello"),
        (Method::DELETE, "/health"),
    ] {
        let (status, _) = send(&rig, Who::Session(&rig.owner), &method, path, None, None).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
    }
}
