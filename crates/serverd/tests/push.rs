//! The push routes, driven as a Director would drive them.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use gridfpv_events::{AdapterId, Event, SeatRef, SessionId};
use gridfpv_server::events::EventId;
use gridfpv_server::remote::{PushHello, PushRefusal, PushRefused, PushStatus};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_storage::Chain;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    token: String,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state
        .book
        .new_token(&ClubId("club".into()), "test laptop")
        .unwrap()
        .1;
    Rig {
        app: router(state.clone()),
        state,
        token,
        _dir: dir,
    }
}

/// One request. `token` of `None` sends no `Authorization` at all.
async fn call(
    app: &Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// `count` real events, each as the text a Director would have stored, with a time.
fn entries(count: usize) -> Vec<(String, Option<i64>)> {
    (0..count)
        .map(|i| {
            let event = match i % 3 {
                0 => Event::AdapterConnected {
                    adapter: AdapterId(format!("timer-{i}")),
                },
                1 => Event::SessionStarted {
                    adapter: AdapterId("timer".into()),
                    session: SessionId(format!("heat-{i}")),
                },
                _ => Event::SeatSeen {
                    adapter: AdapterId("timer".into()),
                    seat: SeatRef(format!("seat-{i}")),
                },
            };
            let at = (i % 4 != 3).then_some(1_000 + i as i64);
            (serde_json::to_string(&event).unwrap(), at)
        })
        .collect()
}

fn chain(entries: &[(String, Option<i64>)]) -> Chain {
    entries
        .iter()
        .fold(Chain::start(), |c, (json, at)| c.then(json, *at))
}

/// The body of a log push: `entries[from..to]`, with the chain of what is before it.
fn batch(entries: &[(String, Option<i64>)], from: usize, to: usize) -> Value {
    let list: Vec<Value> = entries[from..to]
        .iter()
        .map(|(json, at)| {
            let event: Value = serde_json::from_str(json).unwrap();
            match at {
                Some(at) => json!({ "recorded_at": at, "event": event }),
                None => json!({ "event": event }),
            }
        })
        .collect();
    json!({ "from": from, "before": chain(&entries[..from]).to_hex(), "entries": list })
}

fn header(id: &str, name: &str) -> Value {
    json!({ "meta": { "id": id, "name": name, "created_at": 1, "persistent": true } })
}

async fn push(rig: &Rig, id: &str, body: Value) -> (StatusCode, Value) {
    call(
        &rig.app,
        Method::POST,
        &format!("/push/events/{id}/log"),
        Some(&rig.token),
        Some(body),
    )
    .await
}

fn held(value: Value) -> PushStatus {
    serde_json::from_value(value).unwrap()
}

fn refused(value: Value) -> PushRefused {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn health_needs_no_token() {
    let rig = rig();
    let (status, body) = call(&rig.app, Method::GET, "/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn every_push_route_needs_a_token_that_stands() {
    let rig = rig();
    let all = entries(1);
    let routes: Vec<(Method, &str, Option<Value>)> = vec![
        (Method::GET, "/push/hello", None),
        (Method::GET, "/push/events/e", None),
        (Method::PUT, "/push/events/e", Some(header("e", "E"))),
        (Method::POST, "/push/events/e/log", Some(batch(&all, 0, 1))),
        (Method::DELETE, "/push/events/e", None),
    ];
    let (other, _) = rig
        .state
        .book
        .new_token(&ClubId("club".into()), "to take back")
        .map(|(record, secret)| (secret, record))
        .unwrap();
    let taken_back = other.clone();
    let id = rig.state.book.tokens()[1].id.clone();
    rig.state.book.revoke_token(&id).unwrap();

    for (method, path, body) in routes {
        for token in [
            None,
            Some(""),
            Some("gfs_wrong"),
            Some("nonsense"),
            Some(taken_back.as_str()),
        ] {
            let (status, value) = call(&rig.app, method.clone(), path, token, body.clone()).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {path} {token:?}"
            );
            assert_eq!(value["code"], "Unauthorized", "{method} {path}");
        }
    }
    // Nothing was made by the refused calls.
    assert!(
        rig.state
            .store
            .list(&ClubId("club".into()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_token_is_read_whatever_the_case_of_bearer() {
    let rig = rig();
    for scheme in ["Bearer", "bearer", "BEARER"] {
        let request = Request::builder()
            .uri("/push/hello")
            .header("authorization", format!("{scheme} {}", rig.token))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            rig.app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::OK
        );
    }
    // The token alone, or under another scheme, is not one.
    for value in [rig.token.clone(), format!("Basic {}", rig.token)] {
        let request = Request::builder()
            .uri("/push/hello")
            .header("authorization", value)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            rig.app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
async fn hello_says_which_club_the_token_is_for() {
    let rig = rig();
    rig.state
        .book
        .rename_club(&ClubId("club".into()), "Recon FPV")
        .unwrap();
    let (status, body) = call(&rig.app, Method::GET, "/push/hello", Some(&rig.token), None).await;
    assert_eq!(status, StatusCode::OK);
    let hello: PushHello = serde_json::from_value(body).unwrap();
    assert_eq!(hello.club.id, "club");
    assert_eq!(hello.club.name, "Recon FPV");
    assert!(!hello.version.is_empty());
}

#[tokio::test]
async fn an_event_goes_up_in_batches_and_the_server_says_how_far_it_has() {
    let rig = rig();
    let all = entries(7);

    // Not held yet.
    let (status, body) = call(
        &rig.app,
        Method::GET,
        "/push/events/night-1",
        Some(&rig.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "UnknownScope");

    // The header makes it.
    let (status, body) = call(
        &rig.app,
        Method::PUT,
        "/push/events/night-1",
        Some(&rig.token),
        Some(header("night-1", "Thursday night")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        held(body),
        PushStatus {
            len: 0,
            chain: Chain::start().to_hex()
        }
    );

    let (status, body) = push(&rig, "night-1", batch(&all, 0, 3)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        held(body),
        PushStatus {
            len: 3,
            chain: chain(&all[..3]).to_hex()
        }
    );

    let (status, body) = push(&rig, "night-1", batch(&all, 3, 7)).await;
    assert_eq!(status, StatusCode::OK);
    let end = PushStatus {
        len: 7,
        chain: chain(&all).to_hex(),
    };
    assert_eq!(held(body), end);

    let (status, body) = call(
        &rig.app,
        Method::GET,
        "/push/events/night-1",
        Some(&rig.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held(body), end);

    // What the server holds is the events that were sent, in order, with their times.
    let copy = rig
        .state
        .store
        .get(&ClubId("club".into()), &EventId("night-1".into()))
        .unwrap()
        .unwrap();
    let stored = copy.read_from(0).unwrap();
    assert_eq!(stored.len(), 7);
    for (stored, (json, at)) in stored.iter().zip(&all) {
        assert_eq!(stored.event, serde_json::from_str::<Event>(json).unwrap());
        assert_eq!(&stored.recorded_at, at);
    }
    // And the header is kept.
    let kept: Value = serde_json::from_str(&copy.header().unwrap().unwrap()).unwrap();
    assert_eq!(kept["meta"]["name"], "Thursday night");
}

#[tokio::test]
async fn a_log_may_arrive_before_its_header() {
    let rig = rig();
    let all = entries(2);
    let (status, body) = push(&rig, "e", batch(&all, 0, 2)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held(body).len, 2);
    let (status, body) = call(
        &rig.app,
        Method::PUT,
        "/push/events/e",
        Some(&rig.token),
        Some(header("e", "E")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held(body).len, 2);
}

#[tokio::test]
async fn a_header_replaces_the_one_before() {
    let rig = rig();
    for name in ["First name", "Second name"] {
        let (status, _) = call(
            &rig.app,
            Method::PUT,
            "/push/events/e",
            Some(&rig.token),
            Some(header("e", name)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let copy = rig
        .state
        .store
        .get(&ClubId("club".into()), &EventId("e".into()))
        .unwrap()
        .unwrap();
    let kept: Value = serde_json::from_str(&copy.header().unwrap().unwrap()).unwrap();
    assert_eq!(kept["meta"]["name"], "Second name");
}

#[tokio::test]
async fn a_header_for_another_event_is_refused() {
    let rig = rig();
    let (status, body) = call(
        &rig.app,
        Method::PUT,
        "/push/events/e",
        Some(&rig.token),
        Some(header("other", "X")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "BadRequest");
    assert!(
        rig.state
            .store
            .list(&ClubId("club".into()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_batch_sent_twice_is_kept_once() {
    let rig = rig();
    let all = entries(5);
    for _ in 0..3 {
        let (status, body) = push(&rig, "e", batch(&all, 0, 5)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            held(body),
            PushStatus {
                len: 5,
                chain: chain(&all).to_hex()
            }
        );
    }
    // Part repeated, part new.
    let more = entries(8);
    let (status, body) = push(&rig, "e", batch(&more, 2, 8)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        held(body),
        PushStatus {
            len: 8,
            chain: chain(&more).to_hex()
        }
    );
}

#[tokio::test]
async fn a_batch_past_the_end_is_told_where_to_send_from() {
    let rig = rig();
    let all = entries(6);
    push(&rig, "e", batch(&all, 0, 2)).await;
    let (status, body) = push(&rig, "e", batch(&all, 4, 6)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let refused = refused(body);
    assert_eq!(refused.reason, PushRefusal::Gap);
    assert_eq!(
        refused.held,
        PushStatus {
            len: 2,
            chain: chain(&all[..2]).to_hex()
        }
    );
    // Sending from there then works.
    let (status, body) = push(&rig, "e", batch(&all, 2, 6)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held(body).len, 6);
}

#[tokio::test]
async fn another_log_under_the_same_id_is_refused_until_the_copy_is_removed() {
    let rig = rig();
    let first = entries(4);
    push(&rig, "e", batch(&first, 0, 4)).await;

    // The event was made again: same id, other entries.
    let mut second = entries(6);
    second.rotate_left(1);
    let (status, body) = push(&rig, "e", batch(&second, 0, 6)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let was = refused(body);
    assert_eq!(was.reason, PushRefusal::Different);
    assert_eq!(
        was.held,
        PushStatus {
            len: 4,
            chain: chain(&first).to_hex()
        }
    );
    // From the end with the other log's chain: refused as well.
    let (status, body) = push(&rig, "e", batch(&second, 4, 6)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refused(body).reason, PushRefusal::Different);

    // The director says to replace it.
    let (status, _) = call(
        &rig.app,
        Method::DELETE,
        "/push/events/e",
        Some(&rig.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &rig.app,
        Method::GET,
        "/push/events/e",
        Some(&rig.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &rig.app,
        Method::DELETE,
        "/push/events/e",
        Some(&rig.token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = push(&rig, "e", batch(&second, 0, 6)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        held(body),
        PushStatus {
            len: 6,
            chain: chain(&second).to_hex()
        }
    );
}

#[tokio::test]
async fn a_batch_with_one_entry_the_server_cannot_read_keeps_none_of_it() {
    let rig = rig();
    let all = entries(3);
    push(&rig, "e", batch(&all, 0, 1)).await;

    for bad in [
        json!({ "NoSuchEvent": { "x": 1 } }),
        json!({ "SeatSeen": { "adapter": "timer" } }),
        json!("just text"),
        json!(null),
        json!(17),
    ] {
        let mut body = batch(&all, 1, 3);
        body["entries"][1]["event"] = bad.clone();
        let (status, value) = push(&rig, "e", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(value["code"], "BadRequest");
        // It names the entry by where it would have sat in the log.
        assert!(
            value["message"].as_str().unwrap().contains("entry 2"),
            "{value}"
        );
    }
    let (_, body) = call(
        &rig.app,
        Method::GET,
        "/push/events/e",
        Some(&rig.token),
        None,
    )
    .await;
    assert_eq!(held(body).len, 1);
}

#[tokio::test]
async fn an_entry_with_more_in_it_than_this_build_knows_is_kept_as_sent() {
    // A newer Director writes a field this server has never heard of. It is kept, so the
    // chain still matches the Director's and a newer server can read it later.
    let rig = rig();
    let odd = r#"{"AdapterConnected":{"adapter":"timer-1","added_later":{"a":[1,2,3]}}}"#;
    let body = json!({
        "from": 0,
        "before": Chain::start().to_hex(),
        "entries": [{ "recorded_at": 5, "event": serde_json::from_str::<Value>(odd).unwrap() }],
    });
    // Sent as text, so the spelling is ours and not serde_json's re-ordering.
    let text = body.to_string();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/push/events/e/log")
        .header("authorization", format!("Bearer {}", rig.token))
        .header("content-type", "application/json")
        .body(Body::from(text.clone()))
        .unwrap();
    let response = rig.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let got: PushStatus =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();

    let copy = rig
        .state
        .store
        .get(&ClubId("club".into()), &EventId("e".into()))
        .unwrap()
        .unwrap();
    let raw = copy.read_raw_from(0).unwrap();
    assert!(raw[0].json.contains("added_later"), "{}", raw[0].json);
    assert!(
        text.contains(&raw[0].json),
        "stored text is the text that was sent"
    );
    assert_eq!(
        got.chain,
        Chain::start().then(&raw[0].json, Some(5)).to_hex()
    );
}

#[tokio::test]
async fn what_is_not_a_batch_is_refused_and_keeps_nothing() {
    let rig = rig();
    let all = entries(2);
    let good = batch(&all, 0, 2);

    let mut no_chain = good.clone();
    no_chain["before"] = json!("not hex");
    let mut short_chain = good.clone();
    short_chain["before"] = json!("abcd");
    for body in [no_chain, short_chain] {
        let (status, value) = push(&rig, "e", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(value["code"], "BadRequest");
    }
    // Missing parts, and a negative start, do not read as a batch at all.
    for body in [
        json!({}),
        json!({ "from": 0 }),
        json!({ "from": -1, "before": Chain::start().to_hex(), "entries": [] }),
    ] {
        let (status, _) = push(&rig, "e", body).await;
        assert!(status.is_client_error(), "{status}");
    }
    assert!(
        rig.state
            .store
            .list(&ClubId("club".into()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_batch_over_the_limit_is_refused() {
    let rig = rig();
    let one = entries(1);
    let event: Value = serde_json::from_str(&one[0].0).unwrap();
    let many: Vec<Value> = (0..gridfpv_serverd::push::BATCH_MAX + 1)
        .map(|_| json!({ "event": event }))
        .collect();
    let body = json!({ "from": 0, "before": Chain::start().to_hex(), "entries": many });
    let (status, value) = push(&rig, "e", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(value["message"].as_str().unwrap().contains("5000"));
    assert!(
        rig.state
            .store
            .list(&ClubId("club".into()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn ids_that_are_not_ids_are_refused_on_every_route() {
    let rig = rig();
    let all = entries(1);
    for id in [
        "UPPER",
        "has%20space",
        "dot.dot",
        "..",
        "under_score",
        "%2e%2e%2fescape",
    ] {
        let path = format!("/push/events/{id}");
        let log = format!("{path}/log");
        let calls: Vec<(Method, &str, Option<Value>)> = vec![
            (Method::GET, &path, None),
            (Method::PUT, &path, Some(header(id, "X"))),
            (Method::DELETE, &path, None),
            (Method::POST, &log, Some(batch(&all, 0, 1))),
        ];
        for (method, path, body) in calls {
            let (status, _) = call(&rig.app, method.clone(), path, Some(&rig.token), body).await;
            assert!(
                status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
                "{method} {path}: {status}"
            );
        }
    }
    assert!(
        rig.state
            .store
            .list(&ClubId("club".into()))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_token_reaches_only_its_own_clubs_events() {
    // Two clubs on one server (§13.2), written into the book by hand: the first build has
    // no way to make a second.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North"},{"id":"south","name":"South"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let north = state
        .book
        .new_token(&ClubId("north".into()), "n")
        .unwrap()
        .1;
    let south = state
        .book
        .new_token(&ClubId("south".into()), "s")
        .unwrap()
        .1;
    let app = router(state.clone());
    let all = entries(3);

    let (status, _) = call(
        &app,
        Method::POST,
        "/push/events/shared-id/log",
        Some(&north),
        Some(batch(&all, 0, 3)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // South is told which club it is, and sees nothing of North's event.
    let (_, hello) = call(&app, Method::GET, "/push/hello", Some(&south), None).await;
    assert_eq!(hello["club"]["name"], "South");
    let (status, _) = call(
        &app,
        Method::GET,
        "/push/events/shared-id",
        Some(&south),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        Method::DELETE,
        "/push/events/shared-id",
        Some(&south),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // South pushing under the same id makes its own event, from nothing.
    let other = entries(1);
    let (status, body) = call(
        &app,
        Method::POST,
        "/push/events/shared-id/log",
        Some(&south),
        Some(batch(&other, 0, 1)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held(body).len, 1);

    // And North's is as it was.
    let (_, body) = call(
        &app,
        Method::GET,
        "/push/events/shared-id",
        Some(&north),
        None,
    )
    .await;
    assert_eq!(
        held(body),
        PushStatus {
            len: 3,
            chain: chain(&all).to_hex()
        }
    );
    assert_eq!(state.store.list(&ClubId("north".into())).unwrap().len(), 1);
    assert_eq!(state.store.list(&ClubId("south".into())).unwrap().len(), 1);
}

#[tokio::test]
async fn a_token_taken_back_stops_working_at_once() {
    let rig = rig();
    let all = entries(4);
    push(&rig, "e", batch(&all, 0, 2)).await;
    let id = rig.state.book.tokens()[0].id.clone();
    rig.state.book.revoke_token(&id).unwrap();
    let (status, _) = push(&rig, "e", batch(&all, 2, 4)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // What was sent before stays.
    let copy = rig
        .state
        .store
        .get(&ClubId("club".into()), &EventId("e".into()))
        .unwrap()
        .unwrap();
    assert_eq!(copy.held().len, 2);
}

#[tokio::test]
async fn the_copy_is_still_there_after_the_server_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let all = entries(5);
    let token = {
        let state = AppState::open(dir.path()).unwrap();
        let token = state
            .book
            .new_token(&ClubId("club".into()), "laptop")
            .unwrap()
            .1;
        let app = router(state);
        let (status, _) = call(
            &app,
            Method::POST,
            "/push/events/e/log",
            Some(&token),
            Some(batch(&all, 0, 3)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        token
    };
    let app = router(AppState::open(dir.path()).unwrap());
    let (status, body) = call(&app, Method::GET, "/push/events/e", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        held(body),
        PushStatus {
            len: 3,
            chain: chain(&all[..3]).to_hex()
        }
    );
    let (status, body) = call(
        &app,
        Method::POST,
        "/push/events/e/log",
        Some(&token),
        Some(batch(&all, 3, 5)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(held(body).len, 5);
}
