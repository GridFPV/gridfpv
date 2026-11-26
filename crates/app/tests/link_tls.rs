//! The Director pushing over HTTPS (docs/primitives.md §13.5), against a real server behind
//! real TLS, with certificates made here.
//!
//! What must hold: a certificate this computer has no reason to trust is refused and
//! nothing is sent; the same certificate is taken once the director has said to trust it;
//! and a certificate for some other name is refused whatever is trusted.

use std::net::SocketAddr;
use std::sync::Arc;

use gridfpv_app::link::{Client, Memory, PushError, sync_all};
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::link::{LinkSettings, LinkTrust};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::ClubId;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

struct Server {
    _dir: tempfile::TempDir,
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

/// The server program's router, behind TLS with the given certificate chain and key.
async fn serve(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Server {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state
        .book
        .new_token(&ClubId("club".into()), "laptop")
        .unwrap()
        .1;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let app = app.clone();
            tokio::spawn(async move {
                // A client that refuses the certificate ends the handshake: not an error here.
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(tls), TowerToHyperService::new(app))
                    .await;
            });
        }
    });
    Server {
        _dir: dir,
        state,
        addr,
        token,
        task,
    }
}

fn key_der(key: &KeyPair) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()))
}

/// A certificate authority a club might run, and a server certificate it signed for `names`.
struct Signed {
    ca_pem: String,
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

fn signed_by_a_club_authority(names: &[&str]) -> Signed {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Test Club Authority");
    let ca = ca.self_signed(&ca_key).unwrap();

    let key = KeyPair::generate().unwrap();
    let params =
        CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
    let cert = params.signed_by(&key, &ca, &ca_key).unwrap();
    Signed {
        ca_pem: ca.pem(),
        chain: vec![cert.der().clone()],
        key: key_der(&key),
    }
}

/// A Director with one event that has something in its log.
fn director() -> (EventRegistry, EventId) {
    let registry = EventRegistry::new(None).unwrap();
    let meta = registry
        .create(&CreateEventRequest::named("Friday"))
        .unwrap();
    let log = registry.log(&meta.id).unwrap();
    log.lock()
        .unwrap()
        .append(
            gridfpv_events::Event::AdapterConnected {
                adapter: gridfpv_events::AdapterId("timer".into()),
            },
            Some(1),
        )
        .unwrap();
    (registry, meta.id)
}

fn link(registry: &EventRegistry, url: String, token: &str, trust: LinkTrust) -> Client {
    registry
        .link()
        .save(LinkSettings {
            url,
            token: Some(token.to_owned()),
            on: true,
            trust,
        })
        .unwrap();
    Client::new(&registry.link().stored()).unwrap()
}

fn held(server: &Server, event: &EventId) -> Option<u64> {
    server
        .state
        .store
        .get(&ClubId("club".into()), event)
        .unwrap()
        .map(|copy| copy.held().len)
}

fn refused_for_its_certificate(err: &PushError) -> bool {
    matches!(err, PushError::Server(why)
        if why.starts_with("The server could not be reached") && why.to_lowercase().contains("certificate"))
}

#[tokio::test]
async fn a_certificate_this_computer_does_not_trust_is_refused_and_nothing_is_sent() {
    let signed = signed_by_a_club_authority(&["localhost"]);
    let server = serve(signed.chain, signed.key).await;
    let (registry, event) = director();
    let client = link(
        &registry,
        format!("https://localhost:{}", server.addr.port()),
        &server.token,
        LinkTrust::System,
    );

    let err = client.hello().await.unwrap_err();
    assert!(refused_for_its_certificate(&err), "{err:?}");
    let err = sync_all(&registry, &client, &mut Memory::default())
        .await
        .unwrap_err();
    assert!(refused_for_its_certificate(&err), "{err:?}");
    assert_eq!(
        held(&server, &event),
        None,
        "not even the event's name was sent"
    );
}

#[tokio::test]
async fn it_is_taken_once_the_director_adds_the_clubs_authority() {
    let signed = signed_by_a_club_authority(&["localhost"]);
    let server = serve(signed.chain, signed.key).await;
    let (registry, event) = director();
    let client = link(
        &registry,
        format!("https://localhost:{}", server.addr.port()),
        &server.token,
        LinkTrust::ExtraAuthorities { pem: signed.ca_pem },
    );

    assert_eq!(client.hello().await.unwrap().club.name, "My club");
    sync_all(&registry, &client, &mut Memory::default())
        .await
        .unwrap();
    assert!(held(&server, &event).unwrap() >= 1);
}

#[tokio::test]
async fn another_authority_does_not_make_it_trusted() {
    let signed = signed_by_a_club_authority(&["localhost"]);
    let someone_else = signed_by_a_club_authority(&["localhost"]);
    let server = serve(signed.chain, signed.key).await;
    let (registry, event) = director();
    let client = link(
        &registry,
        format!("https://localhost:{}", server.addr.port()),
        &server.token,
        LinkTrust::ExtraAuthorities {
            pem: someone_else.ca_pem,
        },
    );
    let err = client.hello().await.unwrap_err();
    assert!(refused_for_its_certificate(&err), "{err:?}");
    assert_eq!(held(&server, &event), None);
}

#[tokio::test]
async fn a_certificate_for_another_name_is_refused_even_from_a_trusted_authority() {
    // The authority is trusted, but the certificate is for some other server.
    let signed = signed_by_a_club_authority(&["grid.example.org"]);
    let server = serve(signed.chain, signed.key).await;
    let (registry, event) = director();
    let client = link(
        &registry,
        format!("https://localhost:{}", server.addr.port()),
        &server.token,
        LinkTrust::ExtraAuthorities { pem: signed.ca_pem },
    );
    let err = client.hello().await.unwrap_err();
    assert!(refused_for_its_certificate(&err), "{err:?}");
    assert_eq!(held(&server, &event), None);
}

#[tokio::test]
async fn a_self_signed_certificate_is_taken_when_that_one_certificate_is_added() {
    // A home server with a certificate it made for itself (§13.5). The director adds that
    // one certificate, and only it is then trusted.
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let server = serve(vec![cert.der().clone()], key_der(&key)).await;
    let url = format!("https://localhost:{}", server.addr.port());

    let (registry, event) = director();
    let untrusted = link(&registry, url.clone(), &server.token, LinkTrust::System);
    assert!(refused_for_its_certificate(
        &untrusted.hello().await.unwrap_err()
    ));

    let client = link(
        &registry,
        url.clone(),
        &server.token,
        LinkTrust::ExtraAuthorities { pem: cert.pem() },
    );
    assert_eq!(client.hello().await.unwrap().club.name, "My club");
    sync_all(&registry, &client, &mut Memory::default())
        .await
        .unwrap();
    assert!(held(&server, &event).unwrap() >= 1);

    // Another self-signed certificate for the same name is not that certificate.
    let other_key = KeyPair::generate().unwrap();
    let other = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .self_signed(&other_key)
        .unwrap();
    let wrong = link(
        &registry,
        url,
        &server.token,
        LinkTrust::ExtraAuthorities { pem: other.pem() },
    );
    assert!(refused_for_its_certificate(
        &wrong.hello().await.unwrap_err()
    ));
}

#[tokio::test]
async fn a_plain_http_server_is_not_spoken_to_at_an_https_address() {
    // The address says https and the thing there speaks plain HTTP. The Director does not
    // fall back, and the token is not sent in the clear.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let saw_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw = saw_token.clone();
    let task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let mut buf = vec![0u8; 4096];
            let n = tcp.read(&mut buf).await.unwrap_or(0);
            if String::from_utf8_lossy(&buf[..n]).contains("gfs_") {
                saw.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    });
    let (registry, _) = director();
    let client = link(
        &registry,
        format!("https://localhost:{}", addr.port()),
        "gfs_do-not-leak",
        LinkTrust::System,
    );
    assert!(matches!(
        client.hello().await.unwrap_err(),
        PushError::Server(_)
    ));
    assert!(
        !saw_token.load(std::sync::atomic::Ordering::SeqCst),
        "the token went out unencrypted"
    );
    task.abort();
}
