//! Pilots and the accounts that hold them, between a Director and a real server
//! (docs/primitives.md §13.7).
//!
//! Each test starts the server program's own router on a port of this computer, runs a
//! Director with its data in a folder, and talks over real HTTP.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_app::director::build_app;
use gridfpv_app::link::{Client, Memory, spawn, sync_all, sync_pilots};
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::link::{ClubSettings, LinkTrust};
use gridfpv_serverd::accounts::{AccountId, InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::ClubId;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const MARK: &str = "ZZSECRET";

fn club() -> ClubId {
    ClubId("club".into())
}

// ── The server ──────────────────────────────────────────────────────────────

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

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let token = state.book.new_token(&club(), "laptop").unwrap().1;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        _dir: dir,
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

    /// Someone with an account, who is a member of the club.
    fn member(&self, name: &str) -> AccountId {
        self.member_signed_in(name).0
    }

    /// A member, and the secret of a session they are signed in with.
    fn member_signed_in(&self, name: &str) -> (AccountId, String) {
        let (_, secret) = self
            .state
            .accounts
            .new_invite(
                NewInvite {
                    kind: InviteKind::Personal {
                        club: club(),
                        role: Role::Pilot,
                        pilot: None,
                    },
                    label: name.into(),
                    max_uses: 1,
                    expires_at: None,
                },
                None,
                1,
            )
            .unwrap();
        let subject = format!("subject-of-{name}");
        let sign_in = NewSignIn {
            provider: "discord",
            subject: &subject,
            label: name,
        };
        let (account, _, session) = self
            .state
            .accounts
            // At the real time: a session made in 1970 ran out long ago.
            .join_with_invite(&secret, &sign_in, name, gridfpv_serverd::people::now_ms())
            .unwrap();
        (account.id, session)
    }

    /// The club's pilots as the server has them: `(id, callsign, holder's name)`.
    fn pilots(&self) -> Vec<(String, String, Option<String>)> {
        let claims = self.state.accounts.claims_in(&club()).unwrap();
        self.state
            .accounts
            .club_pilots(&club())
            .unwrap()
            .into_iter()
            .map(|pilot| {
                let holder = claims
                    .iter()
                    .find(|claim| claim.pilot == pilot.pilot && claim.how.holds())
                    .map(|claim| claim.account.name.clone());
                (pilot.pilot, pilot.callsign, holder)
            })
            .collect()
    }

    /// The club has heard of a pilot: an event that names them was sent to it.
    fn knows(&self, pilot: &str, callsign: &str) {
        let known = gridfpv_serverd::accounts::ClubPilot {
            pilot: pilot.into(),
            callsign: callsign.into(),
            color: None,
        };
        self.state
            .accounts
            .note_pilots(&club(), &[known], 3)
            .unwrap();
    }

    fn holder_of(&self, pilot: &str) -> Option<String> {
        self.pilots()
            .into_iter()
            .find(|(id, _, _)| id == pilot)
            .and_then(|(_, _, holder)| holder)
    }
}

// ── The Director ────────────────────────────────────────────────────────────

struct Director {
    _dir: tempfile::TempDir,
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
        _dir: dir,
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

/// Add a pilot to the Director, with things about them a visitor must not see.
async fn pilot(d: &Director, callsign: &str) -> String {
    direct(
        d,
        Method::POST,
        "/pilots",
        json!({
            "callsign": callsign,
            "name": format!("{MARK}-realname"),
            "team": format!("{MARK}-team"),
            "color": "#FF0000",
            "multigp_id": format!("{MARK}-multigp")
        }),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn pilots_of(d: &Director) -> Vec<Value> {
    let (status, list) = ask(d, true, Method::GET, "/pilots", None).await;
    assert_eq!(status, StatusCode::OK);
    list.as_array().unwrap().clone()
}

async fn pilot_of(d: &Director, id: &str) -> Value {
    pilots_of(d)
        .await
        .into_iter()
        .find(|pilot| pilot["id"] == id)
        .unwrap_or_else(|| panic!("no pilot {id}"))
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

/// The Director sends to the server's club, and has an event open that goes to it: Grid
/// talks to the club of the open event, and to no other.
fn link(d: &Director, s: &Server) {
    if d.registry.active().is_none() {
        let meta = d
            .registry
            .create(&gridfpv_server::events::CreateEventRequest::named(
                "Race day",
            ))
            .unwrap();
        d.registry.set_active(&meta.id).unwrap();
    }
    link_to(
        &d.registry,
        &s.url(),
        Some(&s.token),
        true,
        LinkTrust::System,
    );
}

fn client(d: &Director) -> Client {
    client_of(&d.registry)
}

async fn until(what: &str, seconds: u64, mut check: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_secs(seconds);
    while !check() {
        assert!(tokio::time::Instant::now() < end, "never happened: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn set_account(d: &Director, pilot: &str, account: Value) -> (StatusCode, Value) {
    ask(
        d,
        true,
        Method::PUT,
        &format!("/pilots/{pilot}/server-account"),
        Some(json!({"account": account})),
    )
    .await
}

// ── Which pilots a club hears of ─────────────────────────────────────────────

#[tokio::test]
async fn a_club_hears_only_of_the_pilots_in_the_events_sent_to_it() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    let bee = pilot(&d, "Bee").await;
    link(&d, &s);
    let mut memory = Memory::default();

    // The Director's pilot list does not go up: a pilot who never flew for this club is
    // not put in its list.
    sync_all(&d.registry, &client(&d), &mut memory)
        .await
        .unwrap();
    sync_pilots(&d.registry, &client(&d), &mut memory, true)
        .await
        .unwrap();
    assert_eq!(s.pilots(), vec![]);

    // Ace is put in an event that goes to the club. Ace goes up with it, and Bee does not.
    let class = direct(
        &d,
        Method::POST,
        "/classes",
        json!({"name": "Open", "source": "Custom"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let event = d.registry.active().unwrap().id.0;
    direct(
        &d,
        Method::POST,
        &format!("/events/{event}/commands"),
        json!({"Record": {"RosterSet": {"class": class, "pilots": [ace]}}}),
    )
    .await;
    sync_all(&d.registry, &client(&d), &mut memory)
        .await
        .unwrap();
    sync_pilots(&d.registry, &client(&d), &mut memory, true)
        .await
        .unwrap();
    assert_eq!(s.pilots(), vec![(ace.clone(), "Ace".to_owned(), None)]);
    // A callsign and a colour went. Nothing else about a pilot did.
    let (status, sent) = {
        let request = Request::builder()
            .uri("/push/pilots")
            .header(header::AUTHORIZATION, format!("Bearer {}", s.token))
            .body(Body::empty())
            .unwrap();
        let response = router(s.state.clone()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice::<Value>(&bytes).unwrap())
    };
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sent,
        json!([{"id": ace, "callsign": "Ace", "color": "#FF0000"}])
    );
    assert!(!sent.to_string().contains(MARK), "{sent}");
    assert!(!sent.to_string().contains(&bee), "{sent}");

    // A renamed pilot goes up with the event's next look.
    direct(
        &d,
        Method::PUT,
        &format!("/pilots/{ace}"),
        json!({"callsign": "Ace Two"}),
    )
    .await;
    sync_all(&d.registry, &client(&d), &mut memory)
        .await
        .unwrap();
    assert_eq!(s.pilots(), vec![(ace, "Ace Two".to_owned(), None)]);
}

#[tokio::test]
async fn a_club_that_never_heard_of_a_pilot_does_not_say_who_holds_them() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    let bee = pilot(&d, "Bee").await;
    link(&d, &s);
    let pia = s.member("Pia");
    // Bee is tied to an account by another club. This club knows Ace only.
    d.registry
        .pilots()
        .set_server_account(
            &gridfpv_events::PilotId(bee.clone()),
            Some(gridfpv_server::pilots::PilotAccount {
                id: "acct_elsewhere".into(),
                name: "Bee at North".into(),
            }),
        )
        .unwrap();
    s.knows(&ace, "Ace");
    s.state
        .accounts
        .give_pilot(&club(), &ace, &pia, None, 5)
        .unwrap();

    sync_pilots(&d.registry, &client(&d), &mut Memory::default(), true)
        .await
        .unwrap();
    assert_eq!(pilot_of(&d, &ace).await["server_account"]["name"], "Pia");
    assert_eq!(
        pilot_of(&d, &bee).await["server_account"],
        json!({"id": "acct_elsewhere", "name": "Bee at North"}),
        "this club cannot say who holds a pilot it has never heard of"
    );
}

// ── Who holds a pilot comes down ────────────────────────────────────────────

/// One look, asking who holds what whether or not it is time.
async fn look(d: &Director, memory: &mut Memory) {
    sync_pilots(&d.registry, &client(d), memory, true)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_claim_made_on_the_server_shows_on_the_directors_pilot() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    let bee = pilot(&d, "Bee").await;
    link(&d, &s);
    s.knows(&ace, "Ace");
    s.knows(&bee, "Bee");
    let pia = s.member("Pia");
    let mut memory = Memory::default();
    look(&d, &mut memory).await;
    assert!(pilot_of(&d, &ace).await.get("server_account").is_none());

    // On the server, Pia comes to hold Ace (an invite, or an approver's yes).
    s.state
        .accounts
        .give_pilot(&club(), &ace, &pia, None, 5)
        .unwrap();
    look(&d, &mut memory).await;
    assert_eq!(
        pilot_of(&d, &ace).await["server_account"],
        json!({"id": pia.0, "name": "Pia"})
    );
    assert!(pilot_of(&d, &bee).await.get("server_account").is_none());

    // She changes her name there: the Director shows the new one.
    s.state.accounts.rename(&pia, "Pia P").unwrap();
    look(&d, &mut memory).await;
    assert_eq!(pilot_of(&d, &ace).await["server_account"]["name"], "Pia P");

    // A director of the club takes Ace back on the server's page.
    s.state.accounts.free_pilot(&club(), &ace).unwrap();
    look(&d, &mut memory).await;
    assert!(pilot_of(&d, &ace).await.get("server_account").is_none());
}

#[tokio::test]
async fn the_server_is_not_asked_who_holds_what_on_every_look() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    link(&d, &s);
    s.knows(&ace, "Ace");
    let pia = s.member("Pia");
    let mut memory = Memory::default();
    // The first look asks, because it never has.
    sync_pilots(&d.registry, &client(&d), &mut memory, false)
        .await
        .unwrap();
    s.state
        .accounts
        .give_pilot(&club(), &ace, &pia, None, 5)
        .unwrap();
    // A look straight after does not ask again, so it does not see the change yet.
    sync_pilots(&d.registry, &client(&d), &mut memory, false)
        .await
        .unwrap();
    assert!(pilot_of(&d, &ace).await.get("server_account").is_none());
    // Asked to, it does.
    sync_pilots(&d.registry, &client(&d), &mut memory, true)
        .await
        .unwrap();
    assert_eq!(pilot_of(&d, &ace).await["server_account"]["name"], "Pia");
}

#[tokio::test]
async fn which_account_a_pilot_is_never_reaches_a_visitor_or_an_events_header() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    link(&d, &s);
    let pia = s.member("Pia");
    let (status, _) = set_account(&d, &ace, json!(pia.0)).await;
    assert_eq!(status, StatusCode::OK);

    let class = direct(
        &d,
        Method::POST,
        "/classes",
        json!({"name": "Open", "source": "Custom"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let event = direct(&d, Method::POST, "/events", json!({"name": "Friday"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    direct(&d, Method::PUT, "/active-event", json!({"id": event})).await;
    direct(
        &d,
        Method::POST,
        &format!("/events/{event}/commands"),
        json!({"Record": {"RosterSet": {"class": class, "pilots": [ace]}}}),
    )
    .await;
    let (status, seen) = ask(&d, false, Method::GET, "/public/event/pilots", None).await;
    assert_eq!(status, StatusCode::OK, "{seen}");
    assert_eq!(seen[0]["callsign"], "Ace");
    assert!(!seen.to_string().contains(&pia.0), "{seen}");
    assert!(!seen.to_string().contains("server_account"), "{seen}");

    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    let copy = s
        .state
        .store
        .get(&club(), &EventId(event))
        .unwrap()
        .unwrap();
    let header = copy.header().unwrap().unwrap();
    assert!(header.contains("\"Ace\""));
    assert!(
        !header.contains(&pia.0) && !header.contains("server_account"),
        "{header}"
    );
}

// ── The director says who holds a pilot ─────────────────────────────────────

#[tokio::test]
async fn the_director_ties_a_pilot_to_a_member_and_unties_it() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    link(&d, &s);
    let pia = s.member("Pia");
    // The server has never heard of Ace: nothing was synced.
    assert_eq!(s.pilots(), vec![]);

    let (status, changed) = set_account(&d, &ace, json!(pia.0)).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(
        changed["server_account"],
        json!({"id": pia.0, "name": "Pia"})
    );
    assert_eq!(changed["callsign"], "Ace");
    // The server agrees, and learned the pilot on the way, by callsign only.
    assert_eq!(
        s.pilots(),
        vec![(ace.clone(), "Ace".to_owned(), Some("Pia".to_owned()))]
    );
    // It is kept: a Director started again still shows it.
    let again = EventRegistry::new(Some(d._dir.path().join("data"))).unwrap();
    let kept = again.pilots().list();
    assert_eq!(kept[0].server_account.as_ref().unwrap().name, "Pia");
    // The rest of the pilot is as it was.
    assert_eq!(
        kept[0].name.as_deref(),
        Some(format!("{MARK}-realname").as_str())
    );

    // To another member: one holder at most.
    let quin = s.member("Quin");
    let (status, changed) = set_account(&d, &ace, json!(quin.0)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["server_account"]["name"], "Quin");
    assert_eq!(s.holder_of(&ace), Some("Quin".to_owned()));

    // Nobody: `null`, and an empty string means the same.
    for nobody in [Value::Null, json!("")] {
        set_account(&d, &ace, json!(quin.0)).await;
        let (status, changed) = set_account(&d, &ace, nobody).await;
        assert_eq!(status, StatusCode::OK);
        assert!(changed.get("server_account").is_none(), "{changed}");
        assert_eq!(s.holder_of(&ace), None);
    }
}

#[tokio::test]
async fn nothing_changes_here_unless_the_server_agreed() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    let pia = s.member("Pia");

    // No event is open, so there is no club to ask.
    let not_sent = "The open event is not sent to a server";
    let (status, why) = set_account(&d, &ace, json!(pia.0)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(why["message"].as_str().unwrap().contains(not_sent), "{why}");
    let (status, _) = ask(&d, true, Method::GET, "/server-link/people", None).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A club is saved and an event is open, and the event goes to no club: the same.
    link(&d, &s);
    link_to(&d.registry, &s.url(), None, false, LinkTrust::System);
    let (status, why) = set_account(&d, &ace, json!(pia.0)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(why["message"].as_str().unwrap().contains(not_sent), "{why}");
    for (method, uri) in [
        (Method::GET, "/server-link/people"),
        (Method::POST, "/server-link/pull"),
    ] {
        let (status, why) = ask(&d, true, method, uri, Some(json!({}))).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}");
        assert!(why["message"].as_str().unwrap().contains(not_sent), "{why}");
    }
    assert_eq!(s.pilots(), vec![]);

    link(&d, &s);
    // An account that is not a member of the club, and one that does not exist.
    for stranger in ["acct_nobody", &s.url()] {
        let (status, why) = set_account(&d, &ace, json!(stranger)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{why}");
    }
    // A pilot this Director does not have.
    let (status, _) = set_account(&d, "nobody-9", json!(pia.0)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(pilot_of(&d, &ace).await.get("server_account").is_none());
    assert_eq!(s.holder_of(&ace), None);

    // The server stops answering: the pilot stays as it was, tied, and the words say why.
    set_account(&d, &ace, json!(pia.0)).await;
    s.task.abort();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (status, why) = set_account(&d, &ace, Value::Null).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        why["message"]
            .as_str()
            .unwrap()
            .contains("could not be reached"),
        "{why}"
    );
    assert_eq!(pilot_of(&d, &ace).await["server_account"]["name"], "Pia");
}

#[tokio::test]
async fn a_token_the_server_took_back_is_said_plainly() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    link(&d, &s);
    let pia = s.member("Pia");
    let id = s.state.book.tokens()[0].id.clone();
    s.state.book.revoke_token(&id).unwrap();
    let (status, why) = set_account(&d, &ace, json!(pia.0)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        why["message"].as_str().unwrap().contains("link token"),
        "{why}"
    );
    assert!(pilot_of(&d, &ace).await.get("server_account").is_none());
}

#[tokio::test]
async fn only_the_director_reaches_any_of_it() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    link(&d, &s);
    let pia = s.member("Pia");

    for (method, uri, body) in [
        (Method::GET, "/server-link/people".to_owned(), None),
        (
            Method::POST,
            "/server-link/pull".to_owned(),
            Some(json!({"members": [pia.0]})),
        ),
        (
            Method::PUT,
            format!("/pilots/{ace}/server-account"),
            Some(json!({"account": pia.0})),
        ),
    ] {
        let (status, _) = ask(&d, false, method.clone(), &uri, body).await;
        assert!(
            status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
            "{method} {uri}: {status}"
        );
    }
    assert_eq!(pilots_of(&d).await.len(), 1);
    assert_eq!(s.holder_of(&ace), None);
    assert_eq!(s.pilots(), vec![]);
}

// ── Bringing pilots in from the server ──────────────────────────────────────

#[tokio::test]
async fn the_director_sees_the_clubs_members_and_pilots() {
    let s = server().await;
    let d = director();
    link(&d, &s);
    let pia = s.member("Pia");
    let quin = s.member("Quin");
    // Another Director of the club sent these two, and Pia holds one.
    let other = |id: &str, callsign: &str| gridfpv_serverd::accounts::ClubPilot {
        pilot: id.into(),
        callsign: callsign.into(),
        color: Some("#00FF00".into()),
    };
    s.state
        .accounts
        .note_pilots(&club(), &[other("cat-1", "Cat"), other("dog-2", "Dog")], 3)
        .unwrap();
    s.state
        .accounts
        .give_pilot(&club(), "cat-1", &pia, None, 4)
        .unwrap();

    let (status, people) = ask(&d, true, Method::GET, "/server-link/people", None).await;
    assert_eq!(status, StatusCode::OK, "{people}");
    assert_eq!(
        people["members"],
        json!([{"id": pia.0, "name": "Pia"}, {"id": quin.0, "name": "Quin"}])
    );
    assert_eq!(
        people["pilots"],
        json!([
            {"id": "cat-1", "callsign": "Cat", "color": "#00FF00", "account": {"id": pia.0, "name": "Pia"}},
            {"id": "dog-2", "callsign": "Dog", "color": "#00FF00"}
        ])
    );
}

#[tokio::test]
async fn pilots_come_in_from_the_server_already_tied_to_their_accounts() {
    let s = server().await;
    let d = director();
    let mine = pilot(&d, "Mine").await;
    link(&d, &s);
    let pia = s.member("Pia");
    let quin = s.member("Quin");
    let other = |id: &str, callsign: &str| gridfpv_serverd::accounts::ClubPilot {
        pilot: id.into(),
        callsign: callsign.into(),
        color: Some("#00FF00".into()),
    };
    s.state
        .accounts
        .note_pilots(&club(), &[other("cat-1", "Cat"), other("dog-2", "Dog")], 3)
        .unwrap();
    s.state
        .accounts
        .give_pilot(&club(), "cat-1", &pia, None, 4)
        .unwrap();

    // Cat (held by Pia), Dog (held by nobody), and Quin, who has no pilot at all.
    let added = direct(
        &d,
        Method::POST,
        "/server-link/pull",
        json!({"pilots": ["cat-1", "dog-2"], "members": [quin.0]}),
    )
    .await;
    let added = added.as_array().unwrap();
    assert_eq!(added.len(), 3, "{added:?}");

    // Cat and Dog keep the ids they have on the server, so what they fly here is theirs
    // there.
    let cat = pilot_of(&d, "cat-1").await;
    assert_eq!(cat["callsign"], "Cat");
    assert_eq!(cat["color"], "#00FF00");
    assert_eq!(cat["server_account"], json!({"id": pia.0, "name": "Pia"}));
    let dog = pilot_of(&d, "dog-2").await;
    assert!(dog.get("server_account").is_none());

    // Quin got a new pilot, named as she is, and the server ties it to her.
    let quins = &added[2];
    assert_eq!(quins["callsign"], "Quin");
    assert_eq!(
        quins["server_account"],
        json!({"id": quin.0, "name": "Quin"})
    );
    let quins_id = quins["id"].as_str().unwrap();
    assert_eq!(s.holder_of(quins_id), Some("Quin".to_owned()));
    assert_eq!(pilots_of(&d).await.len(), 4);
    assert!(pilots_of(&d).await.iter().any(|p| p["id"] == mine));

    // Asking again for what is here adds nothing and changes nothing.
    direct(
        &d,
        Method::PUT,
        "/pilots/cat-1",
        json!({"callsign": "Cat Renamed"}),
    )
    .await;
    let again = direct(
        &d,
        Method::POST,
        "/server-link/pull",
        json!({"pilots": ["cat-1"]}),
    )
    .await;
    assert_eq!(again, json!([]));
    assert_eq!(pilot_of(&d, "cat-1").await["callsign"], "Cat Renamed");
    assert_eq!(pilots_of(&d).await.len(), 4);
}

#[tokio::test]
async fn asking_for_what_the_server_does_not_have_brings_nothing_in() {
    let s = server().await;
    let d = director();
    link(&d, &s);
    let pia = s.member("Pia");
    let other = gridfpv_serverd::accounts::ClubPilot {
        pilot: "cat-1".into(),
        callsign: "Cat".into(),
        color: None,
    };
    s.state.accounts.note_pilots(&club(), &[other], 3).unwrap();

    for body in [
        json!({"pilots": ["cat-1", "nobody-9"]}),
        json!({"pilots": ["cat-1"], "members": ["acct_nobody"]}),
        json!({"members": [pia.0, "acct_nobody"]}),
    ] {
        let (status, why) = ask(&d, true, Method::POST, "/server-link/pull", Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{why}");
    }
    // Not even the ones that were there.
    assert_eq!(pilots_of(&d).await, Vec::<Value>::new());
    assert_eq!(s.pilots().len(), 1);
    // Nothing asked for is nothing done.
    let none = direct(&d, Method::POST, "/server-link/pull", json!({})).await;
    assert_eq!(none, json!([]));
}

// ── The task, left to run ───────────────────────────────────────────────────

#[tokio::test]
async fn left_running_a_claim_comes_down_and_the_pilot_list_stays_here() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    let pia = s.member("Pia");
    // Before the Director is linked, another Director of the club sent Ace and the club
    // tied Ace to Pia.
    let known = gridfpv_serverd::accounts::ClubPilot {
        pilot: ace.clone(),
        callsign: "Ace".into(),
        color: None,
    };
    s.state.accounts.note_pilots(&club(), &[known], 3).unwrap();
    s.state
        .accounts
        .give_pilot(&club(), &ace, &pia, None, 4)
        .unwrap();

    let task = spawn(d.registry.clone());
    link(&d, &s);
    let registry = d.registry.clone();
    let id = gridfpv_events::PilotId(ace.clone());
    until("the claim shows on the Director's pilot", 10, || {
        registry
            .pilots()
            .get(&id)
            .and_then(|pilot| pilot.server_account)
            .is_some_and(|account| account.name == "Pia")
    })
    .await;

    // A pilot added while it runs is in no event of the club. They stay on the Director.
    let bee = pilot(&d, "Bee").await;
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert!(
        !s.pilots().iter().any(|(id, _, _)| id == &bee),
        "a pilot in none of the club's events is not put in its list"
    );
    task.abort();
}

// ── A result, from the gate to the account ──────────────────────────────────

/// Race one heat of a class to its end: `first` crosses four times and `second` twice.
async fn raced_event(d: &Director, first: &str, second: &str) -> String {
    let class = direct(
        d,
        Method::POST,
        "/classes",
        json!({"name": "Open", "source": "Custom"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let event = direct(d, Method::POST, "/events", json!({"name": "Claims Cup"})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    direct(d, Method::PUT, "/active-event", json!({"id": event})).await;
    let commands = format!("/events/{event}/commands");
    direct(
        d,
        Method::POST,
        &commands,
        json!({"Record": {"RosterSet": {"class": class, "pilots": [first, second]}}}),
    )
    .await;
    direct(
        d,
        Method::POST,
        &commands,
        json!({"Record": {"PhaseDefined": {"def": {
            "id": "heat-1",
            "label": "Heat 1",
            "class": class,
            "kind": {"Heat": {"lineup": [
                {"adapter": "bench", "seat": "seat-1", "source": {"Pilot": first}},
                {"adapter": "bench", "seat": "seat-2", "source": {"Pilot": second}}
            ]}},
            "format": {
                "name": "Open",
                "race": {"end": "Open", "staging_micros": 0, "first_lap": "HoleShot"},
                "ranking": {"sort": ["Laps"]},
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
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64;
    for seat in ["seat-1", "seat-2", "seat-1", "seat-2", "seat-1", "seat-1"] {
        direct(
            d,
            Method::POST,
            &format!("/events/{event}/passes"),
            json!({"adapter": "bench", "seat": seat, "at": at}),
        )
        .await;
        at += 1_500_000;
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
        json!({"SaveRace": {"race": "heat-1#1"}}),
    )
    .await;
    event
}

#[tokio::test]
async fn where_a_pilot_placed_reaches_the_account_the_director_tied_them_to() {
    let s = server().await;
    let d = director();
    let swift = pilot(&d, "Swift").await;
    let tardy = pilot(&d, "Tardy").await;
    raced_event(&d, &swift, &tardy).await;
    link(&d, &s);
    let (pia, pias_session) = s.member_signed_in("Pia");
    let (quin, quins_session) = s.member_signed_in("Quin");

    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    assert_eq!(
        set_account(&d, &swift, json!(pia.0)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        set_account(&d, &tardy, json!(quin.0)).await.0,
        StatusCode::OK
    );

    let results = |session: String| {
        let state = s.state.clone();
        async move {
            let request = Request::builder()
                .uri("/api/me/results")
                .header(header::COOKIE, format!("grid_session={session}"))
                .body(Body::empty())
                .unwrap();
            let response = router(state).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<Value>(&bytes).unwrap()
        }
    };
    let pias = results(pias_session.clone()).await;
    assert_eq!(pias.as_array().unwrap().len(), 1, "{pias}");
    assert_eq!(pias[0]["event"]["name"], "Claims Cup");
    assert_eq!(pias[0]["callsign"], "Swift");
    assert_eq!(
        pias[0]["classes"],
        json!([{"class": "Open", "position": 1, "of": 2, "placed": true}])
    );
    let quins = results(quins_session).await;
    assert_eq!(
        quins[0]["classes"],
        json!([{"class": "Open", "position": 2, "of": 2, "placed": true}])
    );
    // What a visitor may not see is not in a result either.
    assert!(!pias.to_string().contains(MARK), "{pias}");

    // Untied, the results are nobody's.
    assert_eq!(set_account(&d, &swift, Value::Null).await.0, StatusCode::OK);
    assert_eq!(results(pias_session).await, json!([]));
}

// ── The account wins where it exists (§13.7) ────────────────────────────────

fn profile(value: Value) -> gridfpv_server::remote::PilotProfile {
    serde_json::from_value(value).unwrap()
}

/// What the server's page shows a signed-in account of its own results.
async fn results_of(s: &Server, session: &str) -> Value {
    let request = Request::builder()
        .uri("/api/me/results")
        .header(header::COOKIE, format!("grid_session={session}"))
        .body(Body::empty())
        .unwrap();
    let response = router(s.state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn a_pilot_who_claims_after_the_race_is_their_account_on_the_server_and_then_in_grid() {
    let s = server().await;
    let d = director();
    // The event is run with the callsign the director had: Swift.
    let swift = pilot(&d, "Swift").await;
    let tardy = pilot(&d, "Tardy").await;
    let event = raced_event(&d, &swift, &tardy).await;
    link(&d, &s);
    sync_all(&d.registry, &client(&d), &mut Memory::default())
        .await
        .unwrap();
    let header_on_server = || {
        s.state
            .store
            .get(&club(), &EventId(event.clone()))
            .unwrap()
            .unwrap()
            .header()
            .unwrap()
            .unwrap()
    };
    assert!(header_on_server().contains("\"Swift\""));

    // Grid is shut off. Later the pilot makes her account, calls herself PiaFPV, and
    // comes to hold the results on the server.
    let (pia, session) = s.member_signed_in("Pia");
    let hers = json!({
        "callsign": "PiaFPV",
        "color": "#AA00FF",
        "multigp_id": "24680",
        "velocidrone_id": "pia.sim"
    });
    s.state
        .accounts
        .set_profile(&pia, profile(hers.clone()))
        .unwrap();
    s.state
        .accounts
        .give_pilot(&club(), &swift, &pia, None, 5)
        .unwrap();
    // The server shows the race as hers, under her account's callsign, at once.
    let results = results_of(&s, &session).await;
    assert_eq!(results[0]["event"]["name"], "Claims Cup");
    assert_eq!(results[0]["callsign"], "PiaFPV");
    assert_eq!(results[0]["classes"][0]["position"], 1);

    // Grid starts again on the same data, and pulls from the server.
    let again = EventRegistry::new(Some(d._dir.path().join("data"))).unwrap();
    let before = again
        .pilots()
        .get(&gridfpv_events::PilotId(swift.clone()))
        .unwrap();
    assert_eq!(before.callsign, "Swift");
    let client = client_of(&again);
    let mut memory = Memory::default();
    sync_pilots(&again, &client, &mut memory, true)
        .await
        .unwrap();

    // Grid's pilot is now the account: its callsign, colour and ids.
    let now = again
        .pilots()
        .get(&gridfpv_events::PilotId(swift.clone()))
        .unwrap();
    assert_eq!(now.callsign, "PiaFPV");
    assert_eq!(now.color.as_deref(), Some("#AA00FF"));
    assert_eq!(now.multigp_id.as_deref(), Some("24680"));
    assert_eq!(now.velocidrone_id.as_deref(), Some("pia.sim"));
    assert_eq!(now.server_account.as_ref().unwrap().name, "Pia");
    // What the server never held stays on the Director as it was.
    assert_eq!(
        now.name.as_deref(),
        Some(format!("{MARK}-realname").as_str())
    );
    assert_eq!(now.team.as_deref(), Some(format!("{MARK}-team").as_str()));
    // A pilot nobody holds is as the director had it.
    let other = again
        .pilots()
        .get(&gridfpv_events::PilotId(tardy.clone()))
        .unwrap();
    assert_eq!(other.callsign, "Tardy");
    assert_eq!(other.color.as_deref(), Some("#FF0000"));
    // It is kept: a Director started once more still has it, with no server in reach.
    let third = EventRegistry::new(Some(d._dir.path().join("data"))).unwrap();
    let kept = third
        .pilots()
        .get(&gridfpv_events::PilotId(swift.clone()))
        .unwrap();
    assert_eq!(kept.callsign, "PiaFPV");

    // The event goes up again under the new callsign. What a visitor may not see still
    // does not leave.
    sync_all(&again, &client, &mut memory).await.unwrap();
    let header = header_on_server();
    assert!(header.contains("\"PiaFPV\"") && !header.contains("\"Swift\""));
    assert!(
        !header.contains(MARK) && !header.contains("24680"),
        "{header}"
    );

    // She changes her callsign on the server. Grid follows at the next pull.
    s.state
        .accounts
        .set_profile(&pia, profile(json!({"callsign": "Pia Two"})))
        .unwrap();
    sync_pilots(&again, &client, &mut memory, true)
        .await
        .unwrap();
    let now = again
        .pilots()
        .get(&gridfpv_events::PilotId(swift.clone()))
        .unwrap();
    assert_eq!(now.callsign, "Pia Two");
    // A part she cleared on the server is not cleared here: the Director keeps what it has.
    assert_eq!(now.multigp_id.as_deref(), Some("24680"));
    assert_eq!(now.color.as_deref(), Some("#AA00FF"));
}

#[tokio::test]
async fn tying_or_bringing_in_a_pilot_takes_the_accounts_profile_at_once() {
    let s = server().await;
    let d = director();
    let ace = pilot(&d, "Ace").await;
    link(&d, &s);
    let pia = s.member("Pia");
    let quin = s.member("Quin");
    let ray = s.member("Ray");
    let accounts = &s.state.accounts;
    let pias = json!({"callsign": "PiaFPV", "color": "#00FF7F", "multigp_id": "24680"});
    accounts.set_profile(&pia, profile(pias)).unwrap();
    let quins = json!({"callsign": "Quinn", "velocidrone_id": "quin.sim"});
    accounts.set_profile(&quin, profile(quins)).unwrap();

    // The director says Ace is Pia. Ace takes her callsign, colour and id there and then.
    let (status, changed) = set_account(&d, &ace, json!(pia.0)).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["callsign"], "PiaFPV");
    assert_eq!(changed["color"], "#00FF7F");
    assert_eq!(changed["multigp_id"], "24680");
    assert_eq!(
        changed["server_account"],
        json!({"id": pia.0, "name": "Pia"})
    );
    // The real name the director typed is not the server's to change.
    assert_eq!(changed["name"], format!("{MARK}-realname"));

    // Members with no pilot are brought in. Each is named as they call themselves, or
    // else as their account, and comes with their ids.
    let added = direct(
        &d,
        Method::POST,
        "/server-link/pull",
        json!({"members": [quin.0, ray.0]}),
    )
    .await;
    let added = added.as_array().unwrap();
    assert_eq!(added.len(), 2, "{added:?}");
    assert_eq!(added[0]["callsign"], "Quinn");
    assert_eq!(added[0]["velocidrone_id"], "quin.sim");
    assert_eq!(added[0]["server_account"]["name"], "Quin");
    assert!(added[0].get("multigp_id").is_none());
    assert_eq!(added[1]["callsign"], "Ray");
    assert_eq!(added[1]["server_account"]["name"], "Ray");
    // The server holds each new pilot for its member.
    let quins_pilot = added[0]["id"].as_str().unwrap();
    assert_eq!(s.holder_of(quins_pilot), Some("Quin".to_owned()));

    // Untied, Ace keeps what it took: it is the pilot's now.
    let (status, changed) = set_account(&d, &ace, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(changed.get("server_account").is_none());
    assert_eq!(changed["callsign"], "PiaFPV");
}

#[tokio::test]
async fn a_pilot_of_the_club_held_by_an_account_comes_in_as_that_account() {
    let s = server().await;
    let d = director();
    link(&d, &s);
    let pia = s.member("Pia");
    // Another Director of the club sent Cat, and Pia came to hold it.
    s.knows("cat-1", "Cat");
    let accounts = &s.state.accounts;
    accounts
        .give_pilot(&club(), "cat-1", &pia, None, 4)
        .unwrap();
    let hers = json!({"callsign": "PiaFPV", "multigp_id": "24680"});
    accounts.set_profile(&pia, profile(hers)).unwrap();

    // What the director is offered is already under her callsign.
    let (_, people) = ask(&d, true, Method::GET, "/server-link/people", None).await;
    assert_eq!(people["pilots"][0]["callsign"], "PiaFPV");
    assert_eq!(people["pilots"][0]["id"], "cat-1");

    let added = direct(
        &d,
        Method::POST,
        "/server-link/pull",
        json!({"pilots": ["cat-1"]}),
    )
    .await;
    assert_eq!(added[0]["id"], "cat-1");
    assert_eq!(added[0]["callsign"], "PiaFPV");
    assert_eq!(added[0]["multigp_id"], "24680");
    assert_eq!(added[0]["server_account"]["name"], "Pia");
}
