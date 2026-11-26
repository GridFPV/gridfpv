//! The server's web page, and what it tells a page that asks who it is.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_serverd::app::{AppState, router, router_with_pages};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

struct Rig {
    _data: tempfile::TempDir,
    assets: tempfile::TempDir,
    app: Router,
}

/// A server with a web page: a folder holding an `index.html` and one other file.
fn rig() -> Rig {
    let data = tempfile::tempdir().unwrap();
    let assets = tempfile::tempdir().unwrap();
    std::fs::write(assets.path().join("index.html"), "<title>the page</title>").unwrap();
    std::fs::create_dir(assets.path().join("assets")).unwrap();
    std::fs::write(assets.path().join("assets/app.js"), "console.log('app')").unwrap();
    let state = AppState::open(data.path()).unwrap();
    Rig {
        app: router_with_pages(state, Some(assets.path())),
        assets,
        _data: data,
    }
}

async fn get(app: &Router, method: Method, path: &str) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let kind = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, kind, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn it_says_it_is_a_server_and_that_the_caller_is_a_visitor() {
    let rig = rig();
    let (status, kind, body) = get(&rig.app, Method::GET, "/public/hello").await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.contains("json"));
    let hello: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(hello["server"], true);
    assert_eq!(hello["caller"], "Visitor");
    assert_eq!(hello["director_sign_in"], false);
    assert_eq!(hello["kiosk"], false);
    assert!(hello.get("event").is_none() && hello.get("pilot").is_none());
    assert!(!hello["version"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn one_events_hello_is_not_the_servers() {
    // Under an event's own address the answer is about that event, as a Director's is.
    let data = tempfile::tempdir().unwrap();
    let state = AppState::open(data.path()).unwrap();
    let token = state
        .book
        .new_token(&gridfpv_serverd::book::ClubId("club".into()), "t")
        .unwrap()
        .1;
    let app = router(state);
    let put = Request::builder()
        .method(Method::PUT)
        .uri("/push/events/e")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"meta":{"id":"e","name":"Friday","created_at":1,"persistent":true}}"#,
        ))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(put).await.unwrap().status(),
        StatusCode::OK
    );
    let (status, _, body) = get(&app, Method::GET, "/api/clubs/club/events/e/public/hello").await;
    assert_eq!(status, StatusCode::OK);
    let hello: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(hello["server"], false);
    assert_eq!(hello["event"], "Friday");
}

#[tokio::test]
async fn the_page_and_its_files_are_served() {
    let rig = rig();
    for path in ["/", "/index.html"] {
        let (status, kind, body) = get(&rig.app, Method::GET, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(kind.contains("html"), "{path}: {kind}");
        assert!(body.contains("the page"));
    }
    let (status, kind, body) = get(&rig.app, Method::GET, "/assets/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(kind.contains("javascript"), "{kind}");
    assert!(body.contains("console.log"));
}

#[tokio::test]
async fn an_address_that_is_no_file_gets_the_page() {
    // The page reads where it is from the part after the `#`, but a person may type or
    // bookmark a path. They get the page, not an error.
    let rig = rig();
    for path in ["/events", "/e/club/friday", "/anything/at/all"] {
        let (status, kind, body) = get(&rig.app, Method::GET, path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(kind.contains("html") && body.contains("the page"), "{path}");
    }
}

#[tokio::test]
async fn a_wrong_api_address_gets_an_error_in_json_and_never_the_page() {
    let rig = rig();
    for path in [
        "/api",
        "/api/",
        "/api/nope",
        "/api/clubs/club/nope",
        "/push",
        "/push/nope",
        "/public/nope",
        "/public/event/state",
    ] {
        let (status, kind, body) = get(&rig.app, Method::GET, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(kind.contains("json"), "{path}: {kind}");
        assert!(!body.contains("the page"), "{path}");
        let error: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(error["code"], "UnknownScope", "{path}");
    }
}

#[tokio::test]
async fn the_api_still_answers_beside_the_page() {
    let rig = rig();
    let (status, _, body) = get(&rig.app, Method::GET, "/api/clubs").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("My club"));
    let (status, _, _) = get(&rig.app, Method::GET, "/health").await;
    assert_eq!(status, StatusCode::OK);
    // Pushing still needs a token.
    let (status, _, _) = get(&rig.app, Method::GET, "/push/hello").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn nothing_outside_the_page_folder_is_served() {
    let rig = rig();
    // A file beside the folder, which a path that climbs out would reach.
    let secret = rig
        .assets
        .path()
        .parent()
        .unwrap()
        .join("gridfpv-pages-test-secret.txt");
    std::fs::write(&secret, "SECRET").unwrap();
    for path in [
        "/../gridfpv-pages-test-secret.txt",
        "/assets/../../gridfpv-pages-test-secret.txt",
        "/%2e%2e/gridfpv-pages-test-secret.txt",
        "/assets/%2e%2e/%2e%2e/gridfpv-pages-test-secret.txt",
    ] {
        let (_, _, body) = get(&rig.app, Method::GET, path).await;
        assert!(
            !body.contains("SECRET"),
            "{path} read a file outside the page folder"
        );
    }
    std::fs::remove_file(secret).ok();
}

#[tokio::test]
async fn the_servers_own_data_is_not_under_the_page() {
    // The data folder is not the page folder: the book with the token hashes is not served.
    let rig = rig();
    for path in [
        "/server.json",
        "/clubs/club",
        "/gridfpv-server-data/server.json",
    ] {
        let (_, _, body) = get(&rig.app, Method::GET, path).await;
        assert!(
            !body.contains("\"tokens\"") && !body.contains("\"hash\""),
            "{path}: {body}"
        );
    }
}

#[tokio::test]
async fn with_no_page_folder_everything_else_is_a_json_error() {
    let data = tempfile::tempdir().unwrap();
    let app = router_with_pages(AppState::open(data.path()).unwrap(), None);
    for path in ["/", "/index.html", "/anything"] {
        let (status, kind, _) = get(&app, Method::GET, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(kind.contains("json"), "{path}");
    }
    // And it still says what it is, and still answers the API.
    let (status, _, body) = get(&app, Method::GET, "/public/hello").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"server\":true"));
    assert_eq!(get(&app, Method::GET, "/api/clubs").await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_page_cannot_be_written_to() {
    let rig = rig();
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        let (status, _, _) = get(&rig.app, method.clone(), "/index.html").await;
        assert!(status.is_client_error(), "{method}: {status}");
    }
    assert_eq!(
        std::fs::read_to_string(rig.assets.path().join("index.html")).unwrap(),
        "<title>the page</title>"
    );
}
