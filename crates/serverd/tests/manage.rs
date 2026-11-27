//! Running a club from the server's page (docs/primitives.md §13.2, §13.6, §13.9): its
//! name, its link tokens, and which of its events are listed or removed.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_events::{AdapterId, Event};
use gridfpv_serverd::accounts::{InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
use gridfpv_storage::Chain;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Rig {
    dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    /// A link token for each club, made on the command line as an owner would.
    north: String,
    south: String,
}

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

/// A server with two clubs.
fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"},{"id":"south","name":"South FPV"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let north = state.book.new_token(&club("north"), "laptop").unwrap().1;
    let south = state.book.new_token(&club("south"), "laptop").unwrap().1;
    Rig {
        app: router(state.clone()),
        state,
        north,
        south,
        dir,
    }
}

impl Rig {
    /// The same server, started again on the same files.
    fn restarted(self) -> Rig {
        let Rig {
            dir, north, south, ..
        } = self;
        let state = AppState::open(dir.path()).unwrap();
        Rig {
            app: router(state.clone()),
            state,
            north,
            south,
            dir,
        }
    }

    /// Someone signed in, and their session.
    fn person(&self, name: &str, kind: InviteKind) -> String {
        let now = now_ms();
        let (_, secret) = self
            .state
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
        self.state
            .accounts
            .join_with_invite(&secret, &sign_in, name, now)
            .unwrap()
            .2
    }

    fn member(&self, name: &str, club_id: &str, role: Role) -> String {
        self.person(
            name,
            InviteKind::Personal {
                club: club(club_id),
                role,
                pilot: None,
            },
        )
    }

    fn owner(&self, name: &str) -> String {
        self.person(name, InviteKind::Owner)
    }
}

#[derive(Clone, Copy)]
enum By<'a> {
    Nobody,
    Session(&'a str),
    Token(&'a str),
}

async fn send(
    rig: &Rig,
    by: By<'_>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "grid.example.org");
    match by {
        By::Nobody => {}
        By::Session(session) => {
            request = request.header(header::COOKIE, format!("{SESSION_COOKIE}={session}"));
        }
        By::Token(token) => {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn entries(count: usize) -> Vec<(String, Option<i64>)> {
    (0..count)
        .map(|i| {
            let event = Event::AdapterConnected {
                adapter: AdapterId(format!("timer-{i}")),
            };
            (
                serde_json::to_string(&event).unwrap(),
                Some(1_000 + i as i64),
            )
        })
        .collect()
}

fn chain(entries: &[(String, Option<i64>)]) -> Chain {
    entries
        .iter()
        .fold(Chain::start(), |c, (json, at)| c.then(json, *at))
}

fn batch(entries: &[(String, Option<i64>)], from: usize) -> Value {
    let list: Vec<Value> = entries[from..]
        .iter()
        .map(|(json, at)| {
            let event: Value = serde_json::from_str(json).unwrap();
            json!({ "recorded_at": at, "event": event })
        })
        .collect();
    json!({ "from": from, "before": chain(&entries[..from]).to_hex(), "entries": list })
}

fn header(id: &str, name: &str, created_at: i64) -> Value {
    json!({ "meta": { "id": id, "name": name, "created_at": created_at, "persistent": true } })
}

/// Send a whole event up with a token: its header and `count` entries.
async fn send_event(rig: &Rig, token: &str, id: &str, name: &str, count: usize) {
    let (status, _) = send(
        rig,
        By::Token(token),
        Method::PUT,
        &format!("/push/events/{id}"),
        Some(header(id, name, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "header of {id}");
    let (status, _) = send(
        rig,
        By::Token(token),
        Method::POST,
        &format!("/push/events/{id}/log"),
        Some(batch(&entries(count), 0)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "log of {id}");
}

/// The names in a club's list of events, as `by` sees it.
async fn listed(rig: &Rig, by: By<'_>, club: &str) -> Vec<String> {
    let (status, body) = send(
        rig,
        by,
        Method::GET,
        &format!("/api/clubs/{club}/events"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut names: Vec<String> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            let name = event["name"].as_str().unwrap();
            if event["hidden"] == json!(true) {
                format!("{name} (hidden)")
            } else {
                name.to_owned()
            }
        })
        .collect();
    names.sort();
    names
}

async fn opens(rig: &Rig, club: &str, event: &str) -> StatusCode {
    send(
        rig,
        By::Nobody,
        Method::GET,
        &format!("/api/clubs/{club}/events/{event}/public/event"),
        None,
    )
    .await
    .0
}

/// Every route that runs a club, with a body that would be taken.
fn manage_routes(club: &str, token_id: &str, event: &str) -> Vec<(Method, String, Option<Value>)> {
    vec![
        (
            Method::PATCH,
            format!("/api/clubs/{club}"),
            Some(json!({ "name": "A new name" })),
        ),
        (Method::GET, format!("/api/clubs/{club}/tokens"), None),
        (
            Method::POST,
            format!("/api/clubs/{club}/tokens"),
            Some(json!({ "label": "another" })),
        ),
        (
            Method::PUT,
            format!("/api/clubs/{club}/events/{event}/listing"),
            Some(json!({ "hidden": true })),
        ),
        (
            Method::DELETE,
            format!("/api/clubs/{club}/tokens/{token_id}"),
            None,
        ),
        (
            Method::DELETE,
            format!("/api/clubs/{club}/events/{event}"),
            None,
        ),
    ]
}

fn token_id(rig: &Rig, club_id: &str) -> String {
    rig.state
        .book
        .tokens()
        .into_iter()
        .find(|token| token.club == club(club_id) && token.revoked_at.is_none())
        .unwrap()
        .id
}

#[tokio::test]
async fn only_a_clubs_directors_and_the_servers_owners_run_it() {
    let rig = rig();
    send_event(&rig, &rig.north, "open-day", "Open Day", 3).await;
    let pilot = rig.member("Pip", "north", Role::Pilot);
    let approver = rig.member("App", "north", Role::Approver);
    let elsewhere = rig.member("Sal", "south", Role::Director);
    let id = token_id(&rig, "north");

    for (method, path, body) in manage_routes("north", &id, "open-day") {
        let (status, _) = send(&rig, By::Nobody, method.clone(), &path, body.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "nobody {method} {path}");
        // A link token is for pushing. It is not a sign-in.
        let (status, _) = send(
            &rig,
            By::Token(&rig.north),
            method.clone(),
            &path,
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a token {method} {path}");
        for (who, session) in [
            ("a pilot", &pilot),
            ("an approver", &approver),
            ("another club's director", &elsewhere),
        ] {
            let (status, _) = send(
                &rig,
                By::Session(session),
                method.clone(),
                &path,
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who} {method} {path}");
        }
    }
    // Nothing was changed by any of that.
    assert_eq!(
        rig.state.book.club(&club("north")).unwrap().name,
        "North FPV"
    );
    assert_eq!(rig.state.book.tokens().len(), 2);
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Open Day"]);
    assert_eq!(opens(&rig, "north", "open-day").await, StatusCode::OK);

    // A director of the club may do every one of them, and so may an owner.
    for who in ["director", "owner"] {
        let rig = self::rig();
        send_event(&rig, &rig.north, "open-day", "Open Day", 3).await;
        let session = match who {
            "director" => rig.member("Dee", "north", Role::Director),
            _ => rig.owner("Oz"),
        };
        let id = token_id(&rig, "north");
        for (method, path, body) in manage_routes("north", &id, "open-day") {
            let (status, _) = send(&rig, By::Session(&session), method.clone(), &path, body).await;
            assert!(status.is_success(), "{who} {method} {path}: {status}");
        }
    }
}

#[tokio::test]
async fn a_club_that_is_not_there_is_not_found_by_an_owner_and_not_told_to_anyone_else() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let director = rig.member("Dee", "north", Role::Director);
    for (method, path, body) in manage_routes("nowhere", "abc", "open-day") {
        let (status, _) = send(
            &rig,
            By::Session(&owner),
            method.clone(),
            &path,
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "owner {method} {path}");
        let (status, _) = send(&rig, By::Session(&director), method.clone(), &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "director {method} {path}");
    }
}

#[tokio::test]
async fn a_request_from_another_website_changes_nothing() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    let request = Request::builder()
        .method(Method::PATCH)
        .uri("/api/clubs/north")
        .header(header::HOST, "grid.example.org")
        .header(header::ORIGIN, "https://evil.example")
        .header(header::COOKIE, format!("{SESSION_COOKIE}={director}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "name": "Owned" }).to_string()))
        .unwrap();
    let response = rig.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        rig.state.book.club(&club("north")).unwrap().name,
        "North FPV"
    );
}

#[tokio::test]
async fn a_new_name_shows_everywhere_the_club_is_named() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    let (status, body) = send(
        &rig,
        By::Session(&director),
        Method::PATCH,
        "/api/clubs/north",
        Some(json!({ "name": "  Northern Lights FPV  " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "id": "north", "name": "Northern Lights FPV", "word": "northern-lights-fpv", "archived": false, "open": false })
    );

    let (_, clubs) = send(&rig, By::Nobody, Method::GET, "/api/clubs", None).await;
    assert_eq!(
        clubs,
        json!([
            { "id": "north", "name": "Northern Lights FPV", "word": "northern-lights-fpv", "archived": false, "open": false },
            { "id": "south", "name": "South FPV", "word": "south-fpv", "archived": false, "open": false },
        ])
    );
    let (_, hello) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(hello["club"]["name"], "Northern Lights FPV");
    let (_, me) = send(&rig, By::Session(&director), Method::GET, "/api/me", None).await;
    assert_eq!(
        me["account"]["clubs"][0]["club"]["name"],
        "Northern Lights FPV"
    );

    // It is still there after a restart.
    let rig = rig.restarted();
    assert_eq!(
        rig.state.book.club(&club("north")).unwrap().name,
        "Northern Lights FPV"
    );
}

#[tokio::test]
async fn a_name_that_will_not_do_is_refused_in_words_and_the_old_one_stays() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    for name in ["", "   ", &"x".repeat(81), "two\nlines"] {
        let (status, body) = send(
            &rig,
            By::Session(&director),
            Method::PATCH,
            "/api/clubs/north",
            Some(json!({ "name": name })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}");
        assert!(body["message"].as_str().unwrap().contains("name"), "{body}");
    }
    assert_eq!(
        rig.state.book.club(&club("north")).unwrap().name,
        "North FPV"
    );
}

#[tokio::test]
async fn a_director_makes_a_token_that_pushes_into_their_club_and_no_other() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    let (status, made) = send(
        &rig,
        By::Session(&director),
        Method::POST,
        "/api/clubs/north/tokens",
        Some(json!({ "label": "  Dee's laptop " })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let secret = made["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("gfs_"), "{secret}");
    assert_eq!(made["token"]["label"], "Dee's laptop");
    assert!(made["token"].get("revoked_at").is_none());

    let (status, hello) = send(&rig, By::Token(&secret), Method::GET, "/push/hello", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(hello["club"]["id"], "north");

    send_event(&rig, &secret, "club-night", "Club Night", 2).await;
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Club Night"]);
    assert!(listed(&rig, By::Nobody, "south").await.is_empty());
}

#[tokio::test]
async fn the_list_of_tokens_never_holds_a_token_and_is_newest_first() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    let mut secrets = vec![rig.north.clone()];
    for label in ["second", "third"] {
        // A millisecond apart, so the order is the order they were made in.
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        let (_, made) = send(
            &rig,
            By::Session(&director),
            Method::POST,
            "/api/clubs/north/tokens",
            Some(json!({ "label": label })),
        )
        .await;
        secrets.push(made["secret"].as_str().unwrap().to_owned());
    }
    let (status, list) = send(
        &rig,
        By::Session(&director),
        Method::GET,
        "/api/clubs/north/tokens",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let labels: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|token| token["label"].as_str().unwrap())
        .collect();
    // South's token is not in North's list.
    assert_eq!(labels, ["third", "second", "laptop"]);
    let text = list.to_string();
    for secret in &secrets {
        assert!(!text.contains(secret), "a token is in the list");
        assert!(!text.contains(&secret[4..]), "a token is in the list");
    }
    assert!(!text.contains("hash"), "{text}");
    for token in list.as_array().unwrap() {
        let mut keys: Vec<&str> = token
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(keys, ["created_at", "id", "label"]);
    }
}

#[tokio::test]
async fn a_label_that_will_not_do_makes_no_token() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    for label in ["", "  ", &"x".repeat(81), "a\tb"] {
        let (status, body) = send(
            &rig,
            By::Session(&director),
            Method::POST,
            "/api/clubs/north/tokens",
            Some(json!({ "label": label })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label:?}");
        assert!(
            body["message"].as_str().unwrap().contains("label"),
            "{body}"
        );
    }
    assert_eq!(rig.state.book.tokens().len(), 2);
}

#[tokio::test]
async fn a_token_taken_back_on_the_page_stops_working_at_once_and_stays_in_the_list() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    let id = token_id(&rig, "north");
    let path = format!("/api/clubs/north/tokens/{id}");

    let (status, _) = send(&rig, By::Session(&director), Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // South's token is not touched.
    let (status, _) = send(
        &rig,
        By::Token(&rig.south),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, list) = send(
        &rig,
        By::Session(&director),
        Method::GET,
        "/api/clubs/north/tokens",
        None,
    )
    .await;
    assert_eq!(list[0]["id"], json!(id));
    assert!(list[0]["revoked_at"].as_i64().unwrap() > 0);

    // It cannot be taken back twice.
    let (status, _) = send(&rig, By::Session(&director), Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // And it stays taken back after a restart.
    let rig = rig.restarted();
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_director_cannot_take_back_another_clubs_token_by_naming_it_under_their_own() {
    let rig = rig();
    let director = rig.member("Dee", "north", Role::Director);
    let theirs = token_id(&rig, "south");
    let (status, _) = send(
        &rig,
        By::Session(&director),
        Method::DELETE,
        &format!("/api/clubs/north/tokens/{theirs}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &rig,
        By::Token(&rig.south),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "South's token still works");
}

#[tokio::test]
async fn a_hidden_event_is_listed_only_for_those_who_run_the_club_and_still_opens() {
    let rig = rig();
    send_event(&rig, &rig.north, "open-day", "Open Day", 3).await;
    send_event(&rig, &rig.north, "test-day", "Test Day", 3).await;
    // South has an event with the same id. Hiding North's must not hide it.
    send_event(&rig, &rig.south, "test-day", "South Test", 1).await;
    let director = rig.member("Dee", "north", Role::Director);
    let owner = rig.owner("Oz");
    let pilot = rig.member("Pip", "north", Role::Pilot);
    let approver = rig.member("App", "north", Role::Approver);
    let elsewhere = rig.member("Sal", "south", Role::Director);

    let (status, body) = send(
        &rig,
        By::Session(&director),
        Method::PUT,
        "/api/clubs/north/events/test-day/listing",
        Some(json!({ "hidden": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "hidden": true }));

    for by in [
        By::Nobody,
        By::Session(&pilot),
        By::Session(&approver),
        By::Session(&elsewhere),
        // A session that is not one reads as nobody.
        By::Session("not-a-session"),
    ] {
        assert_eq!(listed(&rig, by, "north").await, ["Open Day"]);
    }
    for by in [By::Session(&director), By::Session(&owner)] {
        assert_eq!(
            listed(&rig, by, "north").await,
            ["Open Day", "Test Day (hidden)"]
        );
    }
    assert_eq!(listed(&rig, By::Nobody, "south").await, ["South Test"]);

    // Anyone with the address can still open it, and it still takes what a Director sends.
    assert_eq!(opens(&rig, "north", "test-day").await, StatusCode::OK);
    let more = entries(5);
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::POST,
        "/push/events/test-day/log",
        Some(batch(&more, 3)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Hidden stays hidden over a restart.
    let rig = rig.restarted();
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Open Day"]);

    // Shown again, it is in everyone's list.
    let (status, body) = send(
        &rig,
        By::Session(&director),
        Method::PUT,
        "/api/clubs/north/events/test-day/listing",
        Some(json!({ "hidden": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "hidden": false }));
    assert_eq!(
        listed(&rig, By::Nobody, "north").await,
        ["Open Day", "Test Day"]
    );
    assert!(rig.state.book.event_notes(&club("north")).is_empty());
}

#[tokio::test]
async fn one_event_is_read_by_its_address_by_anyone_hidden_or_not() {
    let rig = rig();
    send_event(&rig, &rig.north, "open-day", "Open Day", 3).await;
    send_event(&rig, &rig.north, "test-day", "Test Day", 3).await;
    let owner = rig.owner("Oz");
    let (status, _) = send(
        &rig,
        By::Session(&owner),
        Method::PUT,
        "/api/clubs/north/events/test-day/listing",
        Some(json!({ "hidden": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for by in [By::Nobody, By::Session(&owner)] {
        let (status, body) = send(
            &rig,
            by,
            Method::GET,
            "/api/clubs/north/events/open-day",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "id": "open-day", "name": "Open Day", "created_at": 1, "hidden": false })
        );
        let (status, body) = send(
            &rig,
            by,
            Method::GET,
            "/api/clubs/north/events/test-day",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({ "id": "test-day", "name": "Test Day", "created_at": 1, "hidden": true })
        );
    }

    // Not there: never sent, another club's, not an id, or removed.
    send_event(&rig, &rig.south, "south-only", "South Only", 1).await;
    for path in [
        "/api/clubs/north/events/no-such-event",
        "/api/clubs/north/events/south-only",
        "/api/clubs/nowhere/events/open-day",
        "/api/clubs/north/events/..%2Fbook",
    ] {
        let (status, _) = send(&rig, By::Nobody, Method::GET, path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let (status, _) = send(
        &rig,
        By::Session(&owner),
        Method::DELETE,
        "/api/clubs/north/events/test-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &rig,
        By::Nobody,
        Method::GET,
        "/api/clubs/north/events/test-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_event_that_is_not_there_cannot_be_hidden_or_removed() {
    let rig = rig();
    send_event(&rig, &rig.south, "south-only", "South Only", 1).await;
    let director = rig.member("Dee", "north", Role::Director);
    for event in [
        "no-such-event",
        "south-only",
        "Not%20An%20Id",
        "..%2F..%2Fbook",
    ] {
        let (status, _) = send(
            &rig,
            By::Session(&director),
            Method::PUT,
            &format!("/api/clubs/north/events/{event}/listing"),
            Some(json!({ "hidden": true })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "hide {event}");
        let (status, _) = send(
            &rig,
            By::Session(&director),
            Method::DELETE,
            &format!("/api/clubs/north/events/{event}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "remove {event}");
    }
    assert!(rig.state.book.event_notes(&club("north")).is_empty());
    assert_eq!(opens(&rig, "south", "south-only").await, StatusCode::OK);
}

#[tokio::test]
async fn a_removed_event_is_gone_and_a_director_cannot_put_it_back_by_itself() {
    let rig = rig();
    let all = entries(4);
    send_event(&rig, &rig.north, "open-day", "Open Day", 4).await;
    send_event(&rig, &rig.north, "club-night", "Club Night", 2).await;
    send_event(&rig, &rig.south, "open-day", "South Open Day", 2).await;
    let director = rig.member("Dee", "north", Role::Director);

    let (status, _) = send(
        &rig,
        By::Session(&director),
        Method::DELETE,
        "/api/clubs/north/events/open-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Gone for everyone, those who run the club too.
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Club Night"]);
    assert_eq!(
        listed(&rig, By::Session(&director), "north").await,
        ["Club Night"]
    );
    assert_eq!(
        opens(&rig, "north", "open-day").await,
        StatusCode::NOT_FOUND
    );
    // The other club's event of the same id is not touched.
    assert_eq!(opens(&rig, "south", "open-day").await, StatusCode::OK);
    assert_eq!(listed(&rig, By::Nobody, "south").await, ["South Open Day"]);

    // Every way a Director could send it is refused, saying it was removed.
    let removed =
        json!({ "reason": "Removed", "held": { "len": 0, "chain": Chain::start().to_hex() } });
    let tries = [
        (Method::GET, "/push/events/open-day".to_owned(), None),
        (
            Method::PUT,
            "/push/events/open-day".to_owned(),
            Some(header("open-day", "Open Day", 1)),
        ),
        (
            Method::POST,
            "/push/events/open-day/log".to_owned(),
            Some(batch(&all, 0)),
        ),
        (
            Method::POST,
            "/push/events/open-day/log".to_owned(),
            Some(batch(&all, 4)),
        ),
    ];
    for (method, path, body) in tries.clone() {
        let (status, answer) = send(&rig, By::Token(&rig.north), method.clone(), &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path}");
        assert_eq!(answer, removed, "{method} {path}");
    }
    assert_eq!(
        opens(&rig, "north", "open-day").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Club Night"]);

    // The Director is told so when it asks what the server holds.
    let (status, held) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/events",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let mut held = held.as_array().unwrap().clone();
    held.sort_by_key(|event| event["id"].as_str().unwrap().to_owned());
    assert_eq!(
        held,
        [
            json!({ "id": "club-night", "len": 2, "chain": chain(&entries(2)).to_hex(), "removed": false }),
            json!({ "id": "open-day", "len": 0, "chain": Chain::start().to_hex(), "removed": true }),
        ]
    );
    // South's Director sees only South's.
    let (_, held) = send(
        &rig,
        By::Token(&rig.south),
        Method::GET,
        "/push/events",
        None,
    )
    .await;
    assert_eq!(
        held,
        json!([{ "id": "open-day", "len": 2, "chain": chain(&entries(2)).to_hex(), "removed": false }])
    );

    // Removed stays removed over a restart.
    let rig = rig.restarted();
    for (method, path, body) in tries {
        let (status, _) = send(&rig, By::Token(&rig.north), method, &path, body).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    // The director of the event says to send it again. Then it is taken, from the start.
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::DELETE,
        "/push/events/open-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(rig.state.book.event_notes(&club("north")).is_empty());
    send_event(&rig, &rig.north, "open-day", "Open Day", 4).await;
    assert_eq!(opens(&rig, "north", "open-day").await, StatusCode::OK);
    assert_eq!(
        listed(&rig, By::Nobody, "north").await,
        ["Club Night", "Open Day"]
    );
}

#[tokio::test]
async fn removing_a_hidden_event_does_not_leave_it_hidden_when_it_is_sent_again() {
    let rig = rig();
    send_event(&rig, &rig.north, "open-day", "Open Day", 2).await;
    let director = rig.member("Dee", "north", Role::Director);
    for (method, path, body) in [
        (
            Method::PUT,
            "/api/clubs/north/events/open-day/listing",
            Some(json!({ "hidden": true })),
        ),
        (Method::DELETE, "/api/clubs/north/events/open-day", None),
    ] {
        let (status, _) = send(&rig, By::Session(&director), method, path, body).await;
        assert!(status.is_success());
    }
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::DELETE,
        "/push/events/open-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    send_event(&rig, &rig.north, "open-day", "Open Day", 2).await;
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Open Day"]);
}

#[tokio::test]
async fn one_clubs_token_cannot_bring_back_what_another_club_removed() {
    let rig = rig();
    send_event(&rig, &rig.north, "open-day", "Open Day", 2).await;
    let owner = rig.owner("Oz");
    let (status, _) = send(
        &rig,
        By::Session(&owner),
        Method::DELETE,
        "/api/clubs/north/events/open-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // South saying "send it again" for the same id is about South's own event.
    let (status, _) = send(
        &rig,
        By::Token(&rig.south),
        Method::DELETE,
        "/push/events/open-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/events/open-day",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "North's is still removed");
}

#[tokio::test]
async fn what_the_server_holds_is_told_only_to_a_token() {
    let rig = rig();
    send_event(&rig, &rig.north, "open-day", "Open Day", 2).await;
    let owner = rig.owner("Oz");
    for by in [By::Nobody, By::Session(&owner), By::Token("gfs_made-up")] {
        let (status, _) = send(&rig, by, Method::GET, "/push/events", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // A club with nothing sent has an empty list, not an error.
    let (status, held) = send(
        &rig,
        By::Token(&rig.south),
        Method::GET,
        "/push/events",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held, json!([]));
}

#[tokio::test]
async fn records_written_before_events_were_noted_still_open() {
    let dir = tempfile::tempdir().unwrap();
    // A book from an earlier build: no `events` at all.
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"}],"tokens":[]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    assert!(state.book.event_notes(&club("north")).is_empty());
    let note = state.book.event_note(&club("north"), "open-day");
    assert!(!note.hidden && note.removed_at.is_none());
}

// ── Clubs themselves: made, found by a friendly address, archived (§13.2) ───

use gridfpv_serverd::book::word_of;

async fn clubs_as(rig: &Rig, by: By<'_>) -> Vec<String> {
    let (status, body) = send(rig, by, Method::GET, "/api/clubs", None).await;
    assert_eq!(status, StatusCode::OK);
    body.as_array()
        .unwrap()
        .iter()
        .map(|club| {
            let name = club["name"].as_str().unwrap();
            if club["archived"] == json!(true) {
                format!("{name} (archived)")
            } else {
                name.to_owned()
            }
        })
        .collect()
}

async fn archive(rig: &Rig, owner: &str, club: &str, archived: bool) -> (StatusCode, Value) {
    send(
        rig,
        By::Session(owner),
        Method::PUT,
        &format!("/api/clubs/{club}/archived"),
        Some(json!({"archived": archived})),
    )
    .await
}

#[test]
fn a_friendly_address_is_the_names_letters_and_digits() {
    for (name, word) in [
        ("Recon FPV", "recon-fpv"),
        ("  North   FPV!  ", "north-fpv"),
        ("Whoop & Co. (2026)", "whoop-co-2026"),
        ("ÅLESUND Drone", "ålesund-drone"),
        ("---", "club"),
        ("🚁", "club"),
        ("a/b?c#d", "a-b-c-d"),
    ] {
        assert_eq!(word_of(name), word, "{name:?}");
    }
    // Never long, and never ending on a dash.
    let long = word_of(&"abcdefghi ".repeat(9));
    assert!(long.chars().count() <= 40, "{long}");
    assert!(!long.ends_with('-'), "{long}");
    // Safe in an address as it stands.
    for name in ["Recon FPV", "a/b?c#d", "x%20y", "<b>"] {
        assert!(
            word_of(name)
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-')
        );
    }
}

#[tokio::test]
async fn clubs_written_before_they_had_addresses_get_one_each_and_no_two_the_same() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"a","name":"Whoop Club"},{"id":"b","name":"Whoop club"},{"id":"c","name":"Whoop Club"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let words: Vec<String> = state.book.clubs().into_iter().map(|c| c.word).collect();
    assert_eq!(words, ["whoop-club", "whoop-club-2", "whoop-club-3"]);
    // The same every time it is read, with nothing written yet.
    let again = AppState::open(dir.path()).unwrap();
    assert_eq!(
        again
            .book
            .clubs()
            .into_iter()
            .map(|c| c.word)
            .collect::<Vec<_>>(),
        words
    );
    assert_eq!(
        again.book.club_by_word("whoop-club-2").unwrap().id,
        club("b")
    );
}

#[tokio::test]
async fn only_an_owner_makes_a_club_and_it_is_a_club_like_any_other() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let director = rig.member("Dee", "north", Role::Director);
    let body = || Some(json!({"name": "West FPV"}));
    let (status, _) = send(&rig, By::Nobody, Method::POST, "/api/clubs", body()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::POST,
        "/api/clubs",
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, said) = send(
        &rig,
        By::Session(&director),
        Method::POST,
        "/api/clubs",
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(said["message"].as_str().unwrap().contains("owner"));
    assert_eq!(clubs_as(&rig, By::Nobody).await, ["North FPV", "South FPV"]);

    let (status, made) = send(
        &rig,
        By::Session(&owner),
        Method::POST,
        "/api/clubs",
        body(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(made["name"], "West FPV");
    assert_eq!(made["word"], "west-fpv");
    assert_eq!(made["archived"], false);
    let id = made["id"].as_str().unwrap().to_owned();
    // A random id that is safe as a folder name on any computer.
    assert_eq!(id.len(), 13);
    assert!(id.starts_with('c'));
    assert!(
        id[1..]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(
        clubs_as(&rig, By::Nobody).await,
        ["North FPV", "South FPV", "West FPV"]
    );

    // It takes a token, an event, an invite and a director, as the first club does.
    let (status, token) = send(
        &rig,
        By::Session(&owner),
        Method::POST,
        &format!("/api/clubs/{id}/tokens"),
        Some(json!({"label": "West laptop"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = token["secret"].as_str().unwrap().to_owned();
    send_event(&rig, &token, "west-night", "West Night", 3).await;
    assert_eq!(listed(&rig, By::Nobody, &id).await, ["West Night"]);
    assert!(listed(&rig, By::Nobody, "north").await.is_empty());
    assert_eq!(opens(&rig, &id, "west-night").await, StatusCode::OK);
    let (status, invite) = send(
        &rig,
        By::Session(&owner),
        Method::POST,
        &format!("/api/clubs/{id}/invites"),
        Some(json!({"kind": "Personal", "label": "Wes", "role": "Director"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{invite}");
    // North's director has no say in it.
    let (status, _) = send(
        &rig,
        By::Session(&director),
        Method::PATCH,
        &format!("/api/clubs/{id}"),
        Some(json!({"name": "Mine now"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // And it is still there after a restart, events and all.
    let rig = rig.restarted();
    assert_eq!(
        clubs_as(&rig, By::Nobody).await,
        ["North FPV", "South FPV", "West FPV"]
    );
    assert_eq!(listed(&rig, By::Nobody, &id).await, ["West Night"]);
}

#[tokio::test]
async fn two_clubs_of_one_name_get_two_ids_and_two_addresses_and_a_bad_name_makes_none() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let make = |name: &str| {
        send(
            &rig,
            By::Session(&owner),
            Method::POST,
            "/api/clubs",
            Some(json!({"name": name})),
        )
    };
    let (_, first) = make("North FPV").await;
    let (_, second) = make("north fpv").await;
    assert_eq!(first["word"], "north-fpv-2", "North already has north-fpv");
    assert_eq!(second["word"], "north-fpv-3");
    assert_ne!(first["id"], second["id"]);
    for bad in ["", "   ", &"x".repeat(81), "two\nlines"] {
        let (status, said) = make(bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert!(said["message"].as_str().unwrap().contains("name"));
    }
    assert_eq!(clubs_as(&rig, By::Nobody).await.len(), 4);
}

async fn by_word(rig: &Rig, word: &str) -> (StatusCode, Value) {
    send(
        rig,
        By::Nobody,
        Method::GET,
        &format!("/api/club-words/{word}"),
        None,
    )
    .await
}

async fn rename(rig: &Rig, owner: &str, club: &str, name: &str) -> (StatusCode, Value) {
    send(
        rig,
        By::Session(owner),
        Method::PATCH,
        &format!("/api/clubs/{club}"),
        Some(json!({"name": name})),
    )
    .await
}

#[tokio::test]
async fn a_friendly_address_follows_the_name_and_the_old_one_still_leads_there() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let (status, found) = by_word(&rig, "north-fpv").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["id"], "north");
    // However it is typed.
    assert_eq!(by_word(&rig, "North-FPV").await.1["id"], "north");
    assert_eq!(by_word(&rig, "nowhere").await.0, StatusCode::NOT_FOUND);

    let (status, renamed) = rename(&rig, &owner, "north", "Northern Whoops").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["id"], "north", "the id never changes");
    assert_eq!(renamed["word"], "northern-whoops");
    assert_eq!(by_word(&rig, "northern-whoops").await.1["id"], "north");
    assert_eq!(
        by_word(&rig, "north-fpv").await.1["id"],
        "north",
        "a link posted before the rename still works"
    );
    assert_eq!(
        by_word(&rig, "north-fpv").await.1["name"],
        "Northern Whoops"
    );

    // A change that makes the same word changes no address.
    let (_, same) = rename(&rig, &owner, "north", "Northern   WHOOPS!").await;
    assert_eq!(same["word"], "northern-whoops");
    assert_eq!(same["name"], "Northern   WHOOPS!");

    // Another club takes the old word: now it leads there.
    let (_, taken) = rename(&rig, &owner, "south", "North FPV").await;
    assert_eq!(taken["word"], "north-fpv");
    assert_eq!(by_word(&rig, "north-fpv").await.1["id"], "south");
    assert_eq!(by_word(&rig, "south-fpv").await.1["id"], "south");
    assert_eq!(by_word(&rig, "northern-whoops").await.1["id"], "north");

    // A club cannot take a word another club has now: it gets a number, and keeps it
    // when it is renamed to the same thing again.
    let (_, clash) = rename(&rig, &owner, "north", "North FPV").await;
    assert_eq!(clash["word"], "north-fpv-2");
    let (_, again) = rename(&rig, &owner, "north", "North FPV").await;
    assert_eq!(again["word"], "north-fpv-2");
    assert_eq!(by_word(&rig, "north-fpv").await.1["id"], "south");

    // All of it is kept.
    let rig = rig.restarted();
    let (_, found) = send(
        &rig,
        By::Nobody,
        Method::GET,
        "/api/club-words/northern-whoops",
        None,
    )
    .await;
    assert_eq!(found["id"], "north");
    let (_, found) = send(
        &rig,
        By::Nobody,
        Method::GET,
        "/api/club-words/south-fpv",
        None,
    )
    .await;
    assert_eq!(found["id"], "south");
}

#[tokio::test]
async fn only_an_owner_archives_a_club_and_one_that_is_not_there_is_not_found() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let director = rig.member("Dee", "north", Role::Director);
    let (status, _) = archive(&rig, &director, "north", true).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "not even its own director");
    let (status, _) = send(
        &rig,
        By::Nobody,
        Method::PUT,
        "/api/clubs/north/archived",
        Some(json!({"archived": true})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(clubs_as(&rig, By::Nobody).await, ["North FPV", "South FPV"]);
    let (status, _) = archive(&rig, &owner, "nowhere", true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Bringing back a club that is not archived changes nothing.
    let (status, same) = archive(&rig, &owner, "north", false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(same["archived"], false);
}

#[tokio::test]
async fn an_archived_club_is_off_the_list_and_read_only_and_its_events_still_open() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let director = rig.member("Dee", "north", Role::Director);
    send_event(&rig, &rig.north, "friday", "Friday", 3).await;
    let (_, invite) = send(
        &rig,
        By::Session(&director),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({"kind": "Group", "label": "Discord", "max_uses": 20, "days": 7})),
    )
    .await;
    let invite = invite["secret"].as_str().unwrap().to_owned();

    let (status, done) = archive(&rig, &owner, "north", true).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["archived"], true);

    // Off the list for everyone but an owner, who can bring it back.
    assert_eq!(clubs_as(&rig, By::Nobody).await, ["South FPV"]);
    assert_eq!(clubs_as(&rig, By::Session(&director)).await, ["South FPV"]);
    assert_eq!(
        clubs_as(&rig, By::Session(&owner)).await,
        ["North FPV (archived)", "South FPV"]
    );
    // Still there by its link, marked, with its events.
    let (status, found) = send(&rig, By::Nobody, Method::GET, "/api/clubs/north", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["archived"], true);
    let (_, found) = send(
        &rig,
        By::Nobody,
        Method::GET,
        "/api/club-words/north-fpv",
        None,
    )
    .await;
    assert_eq!(found["archived"], true);
    assert_eq!(listed(&rig, By::Nobody, "north").await, ["Friday"]);
    assert_eq!(opens(&rig, "north", "friday").await, StatusCode::OK);

    // A Director's pushes are refused, and it is told why: not that its token is wrong.
    for (method, path, body) in [
        (Method::GET, "/push/hello".to_owned(), None),
        (Method::GET, "/push/events".to_owned(), None),
        (
            Method::PUT,
            "/push/events/saturday".to_owned(),
            Some(header("saturday", "Saturday", 2)),
        ),
        (
            Method::POST,
            "/push/events/friday/log".to_owned(),
            Some(batch(&entries(5), 3)),
        ),
        (Method::DELETE, "/push/events/friday".to_owned(), None),
    ] {
        let (status, said) = send(&rig, By::Token(&rig.north), method, &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        let said = said["message"].as_str().unwrap();
        assert!(said.contains("North FPV is archived"), "{path}: {said}");
    }
    // South's Director is not touched.
    let (status, _) = send(
        &rig,
        By::Token(&rig.south),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Nothing in the club can be changed, by its director or by an owner.
    let tok = token_id(&rig, "north");
    for by in [&director, &owner] {
        for (method, path, body) in manage_routes("north", &tok, "friday") {
            if method == Method::GET {
                continue;
            }
            let (status, said) = send(&rig, By::Session(by), method.clone(), &path, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{method} {path}");
            assert!(
                said["message"].as_str().unwrap().contains("archived"),
                "{method} {path}: {said}"
            );
        }
        for (method, path, body) in [
            (
                Method::POST,
                "/api/clubs/north/invites",
                Some(json!({"kind": "Personal", "label": "New"})),
            ),
            (Method::POST, "/api/clubs/north/pilots/ace-1/claim", None),
            (
                Method::POST,
                "/api/clubs/north/events/friday/public/pilot/signup/next",
                Some(json!({})),
            ),
        ] {
            let (status, _) = send(&rig, By::Session(by), method, path, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}");
        }
    }
    assert_eq!(
        listed(&rig, By::Nobody, "north").await,
        ["Friday"],
        "nothing was removed"
    );
    // Reading is as it was for those who run it.
    let (status, _) = send(
        &rig,
        By::Session(&director),
        Method::GET,
        "/api/clubs/north/members",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The invite that was out no longer works.
    let (status, _) = send(
        &rig,
        By::Nobody,
        Method::GET,
        &format!("/api/invites/{invite}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // South's things are as they were.
    let (status, _) = send(
        &rig,
        By::Session(&owner),
        Method::PATCH,
        "/api/clubs/south",
        Some(json!({"name": "South Whoops"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // It is still archived after a restart.
    let rig = rig.restarted();
    assert_eq!(clubs_as(&rig, By::Nobody).await, ["South Whoops"]);
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_club_brought_back_works_again_and_invites_are_made_anew() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let director = rig.member("Dee", "north", Role::Director);
    send_event(&rig, &rig.north, "friday", "Friday", 3).await;
    let (_, invite) = send(
        &rig,
        By::Session(&director),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({"kind": "Personal", "label": "Ace"})),
    )
    .await;
    let old = invite["secret"].as_str().unwrap().to_owned();
    archive(&rig, &owner, "north", true).await;
    // Archiving twice is the same as once.
    let (status, twice) = archive(&rig, &owner, "north", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(twice["archived"], true);

    let (status, back) = archive(&rig, &owner, "north", false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(back["archived"], false);
    assert_eq!(clubs_as(&rig, By::Nobody).await, ["North FPV", "South FPV"]);

    // The Director carries on from where it was, with the same token.
    let (status, hello) = send(
        &rig,
        By::Token(&rig.north),
        Method::GET,
        "/push/hello",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hello}");
    let (status, _) = send(
        &rig,
        By::Token(&rig.north),
        Method::POST,
        "/push/events/friday/log",
        Some(batch(&entries(5), 3)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The director runs it again.
    let (status, _) = send(
        &rig,
        By::Session(&director),
        Method::PATCH,
        "/api/clubs/north",
        Some(json!({"name": "North Again"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The old invite stays dead, and a new one is made.
    let (status, _) = send(
        &rig,
        By::Nobody,
        Method::GET,
        &format!("/api/invites/{old}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &rig,
        By::Session(&director),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({"kind": "Personal", "label": "Ace"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
