//! Looking at the certificate a server shows, so the director can accept it on purpose
//! (docs/primitives.md §13.5), against real TLS with certificates made here.
//!
//! What must hold: the director is shown the one certificate the server really presents;
//! accepting it makes that server, and only that certificate, trusted; the look itself
//! sends no link token and follows nothing; and Grid says when accepting would not help.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_app::link::{Client, Memory, sync_all};
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::link::{LinkSettings, LinkTrust};
use gridfpv_server::link_cert::{fingerprint, look};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::ClubId;
use http_body_util::BodyExt;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use serde_json::{Value, json};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tower::ServiceExt;

/// Something that speaks HTTPS on a port of this computer. Stopped when dropped.
struct Tls {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Tls {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Tls {
    fn url(&self, host: &str) -> String {
        format!("https://{host}:{}", self.addr.port())
    }
}

/// `app` behind TLS with the given certificate chain and key.
async fn serve(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    app: Router,
) -> Tls {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let app = app.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tls), TowerToHyperService::new(app))
                    .await;
            });
        }
    });
    Tls { addr, task }
}

fn key_der(key: &KeyPair) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()))
}

/// A certificate a server made for itself, for `names`.
struct Own {
    der: CertificateDer<'static>,
    pem: String,
    key: PrivateKeyDer<'static>,
}

fn self_signed(names: &[&str]) -> Own {
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
        .unwrap()
        .self_signed(&key)
        .unwrap();
    Own {
        der: cert.der().clone(),
        pem: cert.pem(),
        key: key_der(&key),
    }
}

/// A real Grid server with one club and a link token.
fn grid_server() -> (tempfile::TempDir, AppState, String) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state
        .book
        .new_token(&ClubId("club".into()), "laptop")
        .unwrap()
        .1;
    (dir, state, token)
}

/// A page that keeps what it was asked: the method, the path, and whether a link token
/// came with it.
type Seen = Arc<Mutex<Vec<(String, String, bool)>>>;

fn recorder(answer: StatusCode, location: Option<String>) -> (Router, Seen) {
    let seen: Seen = Arc::default();
    let keep = seen.clone();
    let app = Router::new().fallback(move |request: Request<Body>| {
        let keep = keep.clone();
        let location = location.clone();
        async move {
            keep.lock().unwrap().push((
                request.method().to_string(),
                request.uri().path().to_owned(),
                request.headers().contains_key(header::AUTHORIZATION),
            ));
            let mut response = axum::http::Response::builder().status(answer);
            if let Some(location) = location {
                response = response.header(header::LOCATION, location);
            }
            response.body(Body::empty()).unwrap()
        }
    });
    (app, seen)
}

fn base64_of(pem: &str) -> String {
    pem.lines()
        .filter(|line| !line.starts_with("-----"))
        .collect()
}

#[tokio::test]
async fn the_director_is_shown_the_certificate_the_server_really_has_and_accepting_it_works() {
    let own = self_signed(&["localhost"]);
    let (_dir, state, token) = grid_server();
    let server = serve(vec![own.der.clone()], own.key, router(state.clone())).await;
    let url = server.url("localhost");

    let found = look(&url, &LinkTrust::System).await.unwrap();
    // It is that certificate, and no other.
    assert_eq!(found.fingerprint, fingerprint(&own.der));
    assert_eq!(base64_of(&found.pem), base64_of(&own.pem));
    // Grid does not take it by itself, and accepting it would be enough.
    assert!(!found.trusted);
    assert!(found.stands_alone);

    // The director accepts it: what the look gave is what is kept.
    let registry = EventRegistry::new(None).unwrap();
    let meta = registry
        .create(&CreateEventRequest::named("Friday"))
        .unwrap();
    registry
        .link()
        .save(LinkSettings {
            url: url.clone(),
            token: Some(token),
            on: true,
            trust: LinkTrust::ExtraAuthorities {
                pem: found.pem.clone(),
            },
        })
        .unwrap();
    let client = Client::new(&registry.link().stored()).unwrap();
    assert_eq!(client.hello().await.unwrap().club.name, "My club");
    sync_all(&registry, &client, &mut Memory::default())
        .await
        .unwrap();
    assert!(
        state
            .store
            .get(&ClubId("club".into()), &EventId(meta.id.0.clone()))
            .unwrap()
            .is_some()
    );

    // Looked at again with that trust, there is nothing left to accept.
    let again = look(
        &url,
        &LinkTrust::ExtraAuthorities {
            pem: found.pem.clone(),
        },
    )
    .await
    .unwrap();
    assert!(again.trusted);
    assert_eq!(again.fingerprint, found.fingerprint);
}

#[tokio::test]
async fn accepting_one_certificate_does_not_trust_another_made_for_the_same_address() {
    let first = self_signed(&["localhost"]);
    let second = self_signed(&["localhost"]);
    let (app, _) = recorder(StatusCode::OK, None);
    let server = serve(vec![second.der.clone()], second.key, app).await;
    let url = server.url("localhost");

    let found = look(
        &url,
        &LinkTrust::ExtraAuthorities {
            pem: first.pem.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(found.fingerprint, fingerprint(&second.der));
    assert_ne!(found.fingerprint, fingerprint(&first.der));
    assert!(
        !found.trusted,
        "the certificate accepted before is not this one"
    );
    assert!(found.stands_alone);
}

#[tokio::test]
async fn a_certificate_signed_by_an_authority_nobody_knows_is_said_to_need_that_authority() {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Test Club Authority");
    let ca = ca.self_signed(&ca_key).unwrap();
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&key, &ca, &ca_key)
        .unwrap();
    let (app, _) = recorder(StatusCode::OK, None);
    let server = serve(vec![cert.der().clone()], key_der(&key), app).await;
    let url = server.url("localhost");

    let found = look(&url, &LinkTrust::System).await.unwrap();
    assert_eq!(found.fingerprint, fingerprint(cert.der()));
    assert!(!found.trusted);
    // Accepting the server's own certificate would not help: the authority is what is
    // missing.
    assert!(!found.stands_alone);

    // With the authority added, Grid takes it.
    let found = look(&url, &LinkTrust::ExtraAuthorities { pem: ca.pem() })
        .await
        .unwrap();
    assert!(found.trusted);
}

#[tokio::test]
async fn a_certificate_made_for_another_address_would_not_help_if_accepted() {
    let own = self_signed(&["grid.example.org"]);
    let (app, _) = recorder(StatusCode::OK, None);
    let server = serve(vec![own.der.clone()], own.key, app).await;

    let found = look(&server.url("localhost"), &LinkTrust::System)
        .await
        .unwrap();
    assert_eq!(found.fingerprint, fingerprint(&own.der));
    assert!(!found.trusted);
    assert!(!found.stands_alone);
}

#[tokio::test]
async fn the_look_asks_for_one_page_sends_no_token_and_follows_nothing() {
    // The server at the address answers with a redirect to somewhere else.
    let elsewhere_cert = self_signed(&["localhost"]);
    let (elsewhere_app, elsewhere_seen) = recorder(StatusCode::OK, None);
    let elsewhere = serve(
        vec![elsewhere_cert.der.clone()],
        elsewhere_cert.key,
        elsewhere_app,
    )
    .await;

    let own = self_signed(&["localhost"]);
    let (app, seen) = recorder(
        StatusCode::FOUND,
        Some(format!("{}/health", elsewhere.url("localhost"))),
    );
    let server = serve(vec![own.der.clone()], own.key, app).await;

    let found = look(&server.url("localhost"), &LinkTrust::System)
        .await
        .unwrap();
    // The certificate is the one at the address given, not where it points.
    assert_eq!(found.fingerprint, fingerprint(&own.der));
    assert!(
        elsewhere_seen.lock().unwrap().is_empty(),
        "the redirect was followed"
    );

    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty());
    for (method, path, token) in seen {
        assert_eq!(method, "GET");
        assert_eq!(path, "/health");
        assert!(!token, "a link token was sent while only looking");
    }
}

#[tokio::test]
async fn a_server_that_is_not_there_or_does_not_speak_https_is_said_plainly() {
    // Nothing is listening.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let why = look(&format!("https://localhost:{port}"), &LinkTrust::System)
        .await
        .unwrap_err();
    assert!(why.starts_with("The server could not be reached"), "{why}");

    // Something is listening and speaks plain HTTP.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let _ = tcp
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                .await;
        }
    });
    let why = look(&format!("https://localhost:{port}"), &LinkTrust::System)
        .await
        .unwrap_err();
    assert!(why.starts_with("The server could not be reached"), "{why}");
    task.abort();
}

// ── The route ───────────────────────────────────────────────────────────────

async fn ask(app: &Router, here: bool, body: Value) -> (StatusCode, Value) {
    let (peer, host) = if here {
        ("127.0.0.1:50000", "127.0.0.1:8080")
    } else {
        ("192.168.1.77:50000", "192.168.1.5:8081")
    };
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/server-link/certificate")
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_director_asks_grid_to_look_and_a_visitor_cannot() {
    let own = self_signed(&["localhost"]);
    let (app, seen) = recorder(StatusCode::OK, None);
    let server = serve(vec![own.der.clone()], own.key, app).await;
    let url = server.url("localhost");

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<title>console</title>").unwrap();
    let registry = EventRegistry::new(Some(dir.path().join("data"))).unwrap();
    let director = build_app(registry.clone(), dir.path());

    // A phone at the field cannot make Grid connect to things.
    let (status, _) = ask(&director, false, json!({ "url": url })).await;
    assert!(
        status == StatusCode::FORBIDDEN
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::NOT_FOUND,
        "{status}"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "Grid connected for a visitor"
    );

    let (status, found) = ask(&director, true, json!({ "url": url })).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["fingerprint"], json!(fingerprint(&own.der)));
    assert_eq!(found["trusted"], json!(false));
    assert_eq!(found["stands_alone"], json!(true));
    assert!(
        found["pem"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    // Looking changes nothing that is set.
    assert_eq!(registry.link().view().url, "");

    // What cannot be looked at is refused in words, before anything is connected to.
    let before = seen.lock().unwrap().len();
    for (body, words) in [
        (json!({ "url": "" }), "address first"),
        (json!({ "url": "http://grid.example.org" }), "https://"),
        (
            json!({ "url": url, "trust": { "ExtraAuthorities": { "pem": "not a certificate" } } }),
            "certificate",
        ),
    ] {
        let (status, answer) = ask(&director, true, body.clone()).await;
        assert!(status.is_client_error(), "{body}: {status}");
        assert!(
            answer["message"].as_str().unwrap().contains(words),
            "{body}: {answer}"
        );
    }
    assert_eq!(seen.lock().unwrap().len(), before);
}
