//! Signing in with a provider (docs/primitives.md §13.8), against a stand-in provider that
//! speaks as Discord and Google do, with the test acting as the person's browser.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use gridfpv_serverd::accounts::{InviteKind, NewInvite, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
use gridfpv_serverd::signin::{Provider, SIGNIN_COOKIE, challenge_of};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const PUBLIC: &str = "https://grid.example.org";

// ── The stand-in provider ───────────────────────────────────────────────────

#[derive(Default)]
struct Books {
    /// A code the provider gave a browser: the challenge it was started with, and who.
    codes: HashMap<String, (String, Value)>,
    /// An access token, and who it is for.
    tokens: HashMap<String, Value>,
    /// Every token request, as the form that was sent.
    asked: Vec<HashMap<String, String>>,
}

#[derive(Clone)]
struct Fake {
    books: Arc<Mutex<Books>>,
    addr: SocketAddr,
}

async fn token(
    State(books): State<Arc<Mutex<Books>>>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let mut books = books.lock().unwrap();
    books.asked.push(form.clone());
    let get = |key: &str| form.get(key).map(String::as_str).unwrap_or("");
    let refuse = |why: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": why})));
    if get("client_id") != "the-client-id" || get("client_secret") != "the-client-secret" {
        return refuse("invalid_client");
    }
    if get("grant_type") != "authorization_code" {
        return refuse("unsupported_grant_type");
    }
    // A code works once, as a real provider's does.
    let Some((challenge, user)) = books.codes.remove(get("code")) else {
        return refuse("invalid_grant");
    };
    if challenge_of(get("code_verifier")) != challenge {
        return refuse("invalid_grant: the verifier is not the one the challenge was made from");
    }
    if !get("redirect_uri").ends_with("/callback") || !get("redirect_uri").starts_with(PUBLIC) {
        return refuse("redirect_uri_mismatch");
    }
    let access = format!("access-{}", books.tokens.len());
    books.tokens.insert(access.clone(), user);
    (
        StatusCode::OK,
        Json(json!({"access_token": access, "token_type": "Bearer", "expires_in": 604800})),
    )
}

async fn user(
    State(books): State<Arc<Mutex<Books>>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match bearer.and_then(|token| books.lock().unwrap().tokens.get(token).cloned()) {
        Some(user) => (StatusCode::OK, Json(user)),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "401: Unauthorized"})),
        ),
    }
}

async fn fake() -> Fake {
    let books = Arc::new(Mutex::new(Books::default()));
    let app = Router::new()
        .route("/token", post(token))
        .route("/user", get(user))
        .with_state(books.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fake { books, addr }
}

impl Fake {
    /// A provider that is this stand-in, shaped like the real one of that name.
    fn provider(&self, like: Provider) -> Provider {
        Provider {
            authorize_url: format!("http://{}/authorize", self.addr),
            token_url: format!("http://{}/token", self.addr),
            user_url: format!("http://{}/user", self.addr),
            ..like
        }
    }
    fn asked(&self) -> Vec<HashMap<String, String>> {
        self.books.lock().unwrap().asked.clone()
    }
}

// ── The server, and a browser ───────────────────────────────────────────────

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    fake: Fake,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"},{"id":"south","name":"South FPV"}]}"#,
    )
    .unwrap();
    let fake = fake().await;
    let providers = vec![
        fake.provider(Provider::discord(
            "the-client-id".into(),
            "the-client-secret".into(),
        )),
        fake.provider(Provider::google(
            "the-client-id".into(),
            "the-client-secret".into(),
        )),
    ];
    let state = AppState::open(dir.path())
        .unwrap()
        .at(Some(PUBLIC.into()))
        .with_providers(providers);
    Rig {
        app: router(state.clone()),
        state,
        fake,
        _dir: dir,
    }
}

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

impl Rig {
    fn invite(&self, kind: InviteKind) -> (String, String) {
        let now = now_ms();
        let expires_at = matches!(kind, InviteKind::Group { .. }).then_some(now + 86_400_000);
        let (invite, secret) = self
            .state
            .accounts
            .new_invite(
                NewInvite {
                    kind,
                    label: "x".into(),
                    max_uses: 5,
                    expires_at,
                },
                None,
                now,
            )
            .unwrap();
        (invite.id, secret)
    }
    fn personal(&self, club_id: &str, role: Role) -> String {
        self.invite(InviteKind::Personal {
            club: club(club_id),
            role,
            pilot: None,
        })
        .1
    }
    fn accounts_made(&self) -> usize {
        ["north", "south"]
            .iter()
            .map(|c| self.state.accounts.members(&club(c)).unwrap().len())
            .sum()
    }
}

/// A person's browser: the cookies it holds.
#[derive(Default, Clone)]
struct Browser {
    cookies: HashMap<String, String>,
}

struct Went {
    status: StatusCode,
    /// Where the browser was sent.
    location: String,
    /// The `Set-Cookie` lines of the answer.
    set: Vec<String>,
    body: Value,
}

impl Browser {
    async fn get(&mut self, rig: &Rig, path: &str) -> Went {
        let mut request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header(header::HOST, "grid.example.org");
        if !self.cookies.is_empty() {
            let line: Vec<String> = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            request = request.header(header::COOKIE, line.join("; "));
        }
        let response = rig
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let location = response
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let set: Vec<String> = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect();
        // Keep cookies as a browser does: a cookie set empty, or with no time left, is gone.
        for line in &set {
            let (pair, _) = line.split_once(';').unwrap_or((line, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() || line.contains("Max-Age=0") {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        Went {
            status,
            location,
            set,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    async fn me(&mut self, rig: &Rig) -> Value {
        self.get(rig, "/api/me").await.body
    }

    /// Start a sign-in. Gives the address the browser was sent to at the provider.
    async fn start(&mut self, rig: &Rig, provider: &str, query: &str) -> Went {
        self.get(rig, &format!("/auth/{provider}/start{query}"))
            .await
    }

    /// Start, approve at the provider as `user`, and come back. Gives where the browser
    /// ended up.
    async fn sign_in(&mut self, rig: &Rig, provider: &str, query: &str, user: Value) -> Went {
        let started = self.start(rig, provider, query).await;
        assert_eq!(
            started.status,
            StatusCode::SEE_OTHER,
            "{}",
            started.location
        );
        let params = query_of(&started.location);
        let code = approve(rig, &params, user);
        self.get(
            rig,
            &format!(
                "/auth/{provider}/callback?code={code}&state={}",
                params["state"]
            ),
        )
        .await
    }
}

fn query_of(url: &str) -> HashMap<String, String> {
    let (_, query) = url.split_once('?').unwrap_or((url, ""));
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_owned(), percent_decode(v)))
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

/// The person says yes at the provider: it makes a code for the browser to carry back.
fn approve(rig: &Rig, params: &HashMap<String, String>, user: Value) -> String {
    let mut books = rig.fake.books.lock().unwrap();
    let code = format!("code-{}", books.codes.len() + books.asked.len() + 1000);
    books
        .codes
        .insert(code.clone(), (params["code_challenge"].clone(), user));
    code
}

fn discord_user(id: &str, name: &str) -> Value {
    json!({"id": id, "username": name.to_lowercase(), "global_name": name, "discriminator": "0"})
}

fn google_user(sub: &str, name: &str) -> Value {
    json!({"sub": sub, "name": name, "picture": "https://example.org/p.png"})
}

fn signed_in(went: &Went) -> bool {
    went.set
        .iter()
        .any(|line| line.starts_with(&format!("{SESSION_COOKIE}=")) && !line.contains("Max-Age=0"))
}

// ── What is set up ──────────────────────────────────────────────────────────

#[tokio::test]
async fn the_real_providers_are_asked_at_their_documented_addresses_for_no_more_than_who_someone_is()
 {
    let discord = Provider::discord("id".into(), "secret".into());
    assert_eq!(
        discord.authorize_url,
        "https://discord.com/oauth2/authorize"
    );
    assert_eq!(discord.token_url, "https://discord.com/api/oauth2/token");
    assert_eq!(discord.user_url, "https://discord.com/api/v10/users/@me");
    assert_eq!(
        discord.scope, "identify",
        "the id and username, and no email"
    );
    assert_eq!(discord.subject_field, "id");

    let google = Provider::google("id".into(), "secret".into());
    assert_eq!(
        google.authorize_url,
        "https://accounts.google.com/o/oauth2/v2/auth"
    );
    assert_eq!(google.token_url, "https://oauth2.googleapis.com/token");
    assert_eq!(
        google.user_url,
        "https://openidconnect.googleapis.com/v1/userinfo"
    );
    assert_eq!(google.scope, "openid profile", "no email");
    assert_eq!(google.subject_field, "sub", "never the email");
}

#[tokio::test]
async fn the_page_is_told_which_providers_are_set_up_and_none_when_the_server_has_no_address() {
    let rig = rig().await;
    let mut browser = Browser::default();
    let list = browser.get(&rig, "/api/sign-in/providers").await.body;
    assert_eq!(
        list,
        json!([{"id": "discord", "name": "Discord"}, {"id": "google", "name": "Google"}])
    );
    assert!(!list.to_string().contains("the-client-secret"));

    // With no address to be sent back to, signing in cannot work, so nothing is offered.
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path())
        .unwrap()
        .with_providers(vec![Provider::discord("a".into(), "b".into())]);
    let bare = Rig {
        app: router(state.clone()),
        state,
        fake: rig.fake.clone(),
        _dir: dir,
    };
    assert_eq!(
        browser.get(&bare, "/api/sign-in/providers").await.body,
        json!([])
    );
    let went = browser.start(&bare, "discord", "").await;
    assert!(
        went.location.ends_with("/#/sign-in?problem=not-set-up"),
        "{}",
        went.location
    );
    // A provider nobody set up.
    let went = browser.start(&rig, "facebook", "").await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=not-set-up")
    );
}

#[tokio::test]
async fn the_browser_is_sent_to_the_provider_with_everything_the_round_trip_needs() {
    let rig = rig().await;
    let mut browser = Browser::default();
    let went = browser.start(&rig, "discord", "").await;
    assert_eq!(went.status, StatusCode::SEE_OTHER);
    assert!(
        went.location
            .starts_with(&format!("http://{}/authorize?", rig.fake.addr))
    );
    let params = query_of(&went.location);
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], "the-client-id");
    assert_eq!(
        params["redirect_uri"],
        format!("{PUBLIC}/auth/discord/callback")
    );
    assert_eq!(params["scope"], "identify");
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(params["state"].len() >= 30 && params["code_challenge"].len() >= 40);
    // The secret never goes out with the browser.
    assert!(!went.location.contains("the-client-secret"));

    // The cookie that ties the trip to this browser.
    let cookie = went
        .set
        .iter()
        .find(|line| line.starts_with(SIGNIN_COOKIE))
        .unwrap();
    assert!(cookie.starts_with(&format!("{SIGNIN_COOKIE}={};", params["state"])));
    for part in [
        "HttpOnly",
        "SameSite=Lax",
        "Path=/auth",
        "Secure",
        "Max-Age=600",
    ] {
        assert!(cookie.contains(part), "{cookie} lacks {part}");
    }
    // Each start is its own.
    let again = query_of(&browser.start(&rig, "discord", "").await.location);
    assert_ne!(again["state"], params["state"]);
    assert_ne!(again["code_challenge"], params["code_challenge"]);
    // Google is asked for its own scope, at its own way back.
    let google = query_of(&browser.start(&rig, "google", "").await.location);
    assert_eq!(google["scope"], "openid profile");
    assert_eq!(
        google["redirect_uri"],
        format!("{PUBLIC}/auth/google/callback")
    );
}

// ── An account is made only with an invite ──────────────────────────────────

#[tokio::test]
async fn someone_new_with_an_invite_gets_an_account_and_is_signed_in() {
    let rig = rig().await;
    let secret = rig.personal("north", Role::Pilot);
    let mut browser = Browser::default();
    let went = browser
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={secret}"),
            discord_user("80351110224678912", "Ace"),
        )
        .await;
    assert_eq!(went.status, StatusCode::SEE_OTHER);
    assert_eq!(went.location, format!("{PUBLIC}/#/"));
    assert!(signed_in(&went));

    let me = browser.me(&rig).await;
    assert_eq!(me["account"]["name"], "Ace");
    assert_eq!(
        me["account"]["sign_ins"],
        json!([{"provider": "discord", "subject": "80351110224678912", "label": "Ace"}])
    );
    assert_eq!(
        me["account"]["clubs"],
        json!([{"club": {"id": "north", "name": "North FPV"}, "role": "Pilot"}])
    );
    // The invite is used up, and the trip's cookie is gone.
    assert!(
        rig.state
            .accounts
            .peek_invite(&secret, now_ms())
            .unwrap()
            .is_none()
    );
    assert!(!browser.cookies.contains_key(SIGNIN_COOKIE));

    // The provider was given the app's secret, the same way back, and the right proof.
    let asked = rig.fake.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["client_secret"], "the-client-secret");
    assert_eq!(
        asked[0]["redirect_uri"],
        format!("{PUBLIC}/auth/discord/callback")
    );
    assert!(asked[0]["code_verifier"].len() >= 40);
}

#[tokio::test]
async fn someone_new_with_no_invite_gets_nothing() {
    let rig = rig().await;
    let mut browser = Browser::default();
    let went = browser
        .sign_in(&rig, "discord", "", discord_user("111", "Stranger"))
        .await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=need-invite")
    );
    assert!(!signed_in(&went));
    assert_eq!(browser.me(&rig).await, json!({}));
    assert!(
        rig.state
            .accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.accounts_made(), 0);
}

#[tokio::test]
async fn an_invite_that_does_not_work_makes_no_account() {
    let rig = rig().await;
    let (id, secret) = rig.invite(InviteKind::Personal {
        club: club("north"),
        role: Role::Director,
        pilot: None,
    });
    rig.state.accounts.revoke_invite(&id, now_ms()).unwrap();
    for invite in [secret.as_str(), "gfi_made-up", "anything"] {
        let mut browser = Browser::default();
        let went = browser
            .sign_in(
                &rig,
                "discord",
                &format!("?invite={invite}"),
                discord_user("111", "Ace"),
            )
            .await;
        assert_eq!(
            went.location,
            format!("{PUBLIC}/#/sign-in?problem=invite"),
            "{invite}"
        );
        assert!(!signed_in(&went));
    }
    assert!(
        rig.state
            .accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.accounts_made(), 0);
}

#[tokio::test]
async fn coming_back_later_signs_in_to_the_same_account_with_no_invite() {
    let rig = rig().await;
    let secret = rig.personal("north", Role::Pilot);
    let mut phone = Browser::default();
    phone
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={secret}"),
            discord_user("111", "Ace"),
        )
        .await;
    let id = phone.me(&rig).await["account"]["id"].clone();

    // Another device, another day, and the person has since changed their Discord name.
    let mut laptop = Browser::default();
    let went = laptop
        .sign_in(&rig, "discord", "", discord_user("111", "Ace of Spades"))
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/"));
    let me = laptop.me(&rig).await;
    assert_eq!(me["account"]["id"], id);
    assert_eq!(
        me["account"]["sign_ins"][0]["label"], "Ace of Spades",
        "what it is shown as follows the provider"
    );
    assert_eq!(
        me["account"]["name"], "Ace",
        "the name they chose here does not"
    );
    assert_eq!(rig.accounts_made(), 1);
    // Both devices are signed in.
    assert_eq!(phone.me(&rig).await["account"]["id"], id);
}

#[tokio::test]
async fn someone_with_an_account_who_comes_by_another_clubs_invite_joins_that_club() {
    let rig = rig().await;
    let mut browser = Browser::default();
    browser
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={}", rig.personal("north", Role::Director)),
            discord_user("111", "Ace"),
        )
        .await;
    let south = rig.personal("south", Role::Pilot);
    // Signed out, they open the other club's link and sign in.
    let mut fresh = Browser::default();
    let went = fresh
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={south}"),
            discord_user("111", "Ace"),
        )
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/"));
    let clubs = fresh.me(&rig).await["account"]["clubs"].clone();
    assert_eq!(clubs.as_array().unwrap().len(), 2);
    assert_eq!(rig.state.accounts.members(&club("south")).unwrap().len(), 1);
    assert!(
        rig.state
            .accounts
            .peek_invite(&south, now_ms())
            .unwrap()
            .is_none(),
        "used"
    );

    // With an invite that no longer works they are still signed in, and told.
    let mut again = Browser::default();
    let went = again
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={south}"),
            discord_user("111", "Ace"),
        )
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=invite"));
    assert!(signed_in(&went));
}

// ── Who someone is ──────────────────────────────────────────────────────────

#[tokio::test]
async fn the_same_id_at_another_provider_is_another_person() {
    let rig = rig().await;
    let mut ace = Browser::default();
    ace.sign_in(
        &rig,
        "discord",
        &format!("?invite={}", rig.personal("north", Role::Director)),
        discord_user("42", "Ace"),
    )
    .await;
    // Someone whose Google id happens to be the same text, with the same name.
    let mut other = Browser::default();
    let went = other
        .sign_in(&rig, "google", "", google_user("42", "Ace"))
        .await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=need-invite")
    );
    assert!(!signed_in(&went));
}

#[tokio::test]
async fn nothing_but_the_providers_id_says_who_someone_is() {
    let rig = rig().await;
    let mut ace = Browser::default();
    ace.sign_in(
        &rig,
        "discord",
        &format!("?invite={}", rig.personal("north", Role::Director)),
        discord_user("111", "Ace"),
    )
    .await;
    // Same username, same display name, and an email field for good measure: another id.
    let mut impostor = Browser::default();
    let user =
        json!({"id": "222", "username": "ace", "global_name": "Ace", "email": "ace@example.org"});
    let went = impostor.sign_in(&rig, "discord", "", user).await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=need-invite")
    );
    assert!(!signed_in(&went));
}

#[tokio::test]
async fn what_a_provider_says_is_tidied_and_an_answer_with_no_id_is_no_answer() {
    let rig = rig().await;
    // A number for an id is taken as its digits. No display name: the username.
    let mut a = Browser::default();
    a.sign_in(
        &rig,
        "discord",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        json!({"id": 80351110224678912u64, "username": "nelly", "global_name": null}),
    )
    .await;
    let me = a.me(&rig).await;
    assert_eq!(me["account"]["sign_ins"][0]["subject"], "80351110224678912");
    assert_eq!(me["account"]["name"], "nelly");

    // No name at all: the account still gets made, with a name to change later.
    let mut b = Browser::default();
    b.sign_in(
        &rig,
        "google",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        json!({"sub": "g-1"}),
    )
    .await;
    assert_eq!(b.me(&rig).await["account"]["name"], "Pilot");

    // A long name with a line break in it.
    let mut c = Browser::default();
    let long = format!("Line one\nline two {}", "x".repeat(200));
    c.sign_in(
        &rig,
        "google",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        google_user("g-2", &long),
    )
    .await;
    let name = c.me(&rig).await["account"]["name"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        name.chars().count() <= 80 && !name.contains('\n'),
        "{name:?}"
    );

    // No id, an empty id, an id that is not text: nobody.
    for user in [
        json!({"username": "ghost"}),
        json!({"id": ""}),
        json!({"id": "  "}),
        json!({"id": ["1"]}),
        json!({"id": null}),
    ] {
        let secret = rig.personal("north", Role::Pilot);
        let mut d = Browser::default();
        let went = d
            .sign_in(&rig, "discord", &format!("?invite={secret}"), user.clone())
            .await;
        assert_eq!(
            went.location,
            format!("{PUBLIC}/#/sign-in?problem=provider"),
            "{user}"
        );
        assert!(!signed_in(&went));
        assert!(
            rig.state
                .accounts
                .peek_invite(&secret, now_ms())
                .unwrap()
                .is_some(),
            "the invite is not used"
        );
    }
    assert_eq!(rig.accounts_made(), 3);
}

// ── Adding a second way to sign in ──────────────────────────────────────────

#[tokio::test]
async fn a_signed_in_account_adds_google_and_then_signs_in_either_way() {
    let rig = rig().await;
    let mut browser = Browser::default();
    browser
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={}", rig.personal("north", Role::Pilot)),
            discord_user("111", "Ace"),
        )
        .await;
    let id = browser.me(&rig).await["account"]["id"].clone();

    let went = browser
        .sign_in(&rig, "google", "?add=1", google_user("g-ace", "Ace G"))
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/account"));
    let me = browser.me(&rig).await;
    assert_eq!(me["account"]["id"], id);
    assert_eq!(me["account"]["sign_ins"].as_array().unwrap().len(), 2);

    let mut later = Browser::default();
    let went = later
        .sign_in(&rig, "google", "", google_user("g-ace", "Ace G"))
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/"));
    assert_eq!(later.me(&rig).await["account"]["id"], id);
    assert_eq!(rig.accounts_made(), 1);
}

#[tokio::test]
async fn adding_needs_someone_signed_in() {
    let rig = rig().await;
    let mut browser = Browser::default();
    let went = browser.start(&rig, "google", "?add=1").await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=not-signed-in")
    );
    assert!(rig.fake.asked().is_empty());
}

#[tokio::test]
async fn a_sign_in_that_is_someone_elses_cannot_be_added() {
    let rig = rig().await;
    let mut ace = Browser::default();
    ace.sign_in(
        &rig,
        "discord",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        discord_user("111", "Ace"),
    )
    .await;
    let mut bee = Browser::default();
    bee.sign_in(
        &rig,
        "google",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        google_user("g-bee", "Bee"),
    )
    .await;
    let bee_id = bee.me(&rig).await["account"]["id"].clone();

    // Ace tries to add Bee's Google account to their own.
    let went = ace
        .sign_in(&rig, "google", "?add=1", google_user("g-bee", "Bee"))
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=taken"));
    assert_eq!(
        ace.me(&rig).await["account"]["sign_ins"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Bee still signs in as Bee, and Ace is still Ace.
    let mut check = Browser::default();
    check
        .sign_in(&rig, "google", "", google_user("g-bee", "Bee"))
        .await;
    assert_eq!(check.me(&rig).await["account"]["id"], bee_id);
    assert_eq!(ace.me(&rig).await["account"]["name"], "Ace");
}

#[tokio::test]
async fn an_add_started_by_one_account_cannot_be_finished_as_another_or_as_nobody() {
    let rig = rig().await;
    let mut ace = Browser::default();
    ace.sign_in(
        &rig,
        "discord",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        discord_user("111", "Ace"),
    )
    .await;
    let mut bee = Browser::default();
    bee.sign_in(
        &rig,
        "discord",
        &format!("?invite={}", rig.personal("north", Role::Pilot)),
        discord_user("222", "Bee"),
    )
    .await;

    for finisher in ["bee", "nobody"] {
        let started = ace.start(&rig, "google", "?add=1").await;
        let params = query_of(&started.location);
        let code = approve(&rig, &params, google_user("g-x", "X"));
        // The trip's own cookie is Ace's; the session in the browser that finishes is not.
        let mut other = ace.clone();
        match finisher {
            "bee" => {
                other
                    .cookies
                    .insert(SESSION_COOKIE.into(), bee.cookies[SESSION_COOKIE].clone());
            }
            _ => {
                other.cookies.remove(SESSION_COOKIE);
            }
        }
        let went = other
            .get(
                &rig,
                &format!(
                    "/auth/google/callback?code={code}&state={}",
                    params["state"]
                ),
            )
            .await;
        assert_eq!(
            went.location,
            format!("{PUBLIC}/#/sign-in?problem=not-signed-in"),
            "{finisher}"
        );
    }
    assert!(
        rig.state
            .accounts
            .account_for_sign_in("google", "g-x")
            .unwrap()
            .is_none()
    );
    for who in [&mut ace, &mut bee] {
        assert_eq!(
            who.me(&rig).await["account"]["sign_ins"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

// ── The round trip cannot be bent ───────────────────────────────────────────

#[tokio::test]
async fn a_way_back_that_this_browser_did_not_start_does_nothing() {
    let rig = rig().await;
    let secret = rig.personal("north", Role::Director);

    // Made-up state, no state, an empty one.
    for query in ["?code=x&state=made-up", "?code=x", "?code=x&state="] {
        let mut browser = Browser::default();
        let went = browser
            .get(&rig, &format!("/auth/discord/callback{query}"))
            .await;
        assert_eq!(
            went.location,
            format!("{PUBLIC}/#/sign-in?problem=expired"),
            "{query}"
        );
        assert!(!signed_in(&went));
    }

    // An attacker starts a sign-in as themselves and sends the victim the way back, hoping
    // the victim ends up signed in to the attacker's account. The victim's browser never
    // started it, so it has no cookie for it.
    let mut attacker = Browser::default();
    let started = attacker
        .start(&rig, "discord", &format!("?invite={secret}"))
        .await;
    let params = query_of(&started.location);
    let code = approve(&rig, &params, discord_user("666", "Attacker"));
    let link = format!(
        "/auth/discord/callback?code={code}&state={}",
        params["state"]
    );
    let mut victim = Browser::default();
    let went = victim.get(&rig, &link).await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=expired"));
    assert!(!signed_in(&went));
    assert_eq!(victim.me(&rig).await, json!({}));
    // And the state is spent: the attacker cannot finish it either.
    let went = attacker.get(&rig, &link).await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=expired"));
    assert_eq!(rig.accounts_made(), 0);
    assert!(
        rig.state
            .accounts
            .peek_invite(&secret, now_ms())
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_way_back_works_once() {
    let rig = rig().await;
    let secret = rig
        .invite(InviteKind::Group {
            club: club("north"),
        })
        .1;
    let mut browser = Browser::default();
    let started = browser
        .start(&rig, "discord", &format!("?invite={secret}"))
        .await;
    let params = query_of(&started.location);
    let code = approve(&rig, &params, discord_user("111", "Ace"));
    let link = format!(
        "/auth/discord/callback?code={code}&state={}",
        params["state"]
    );
    let mut replayer = browser.clone();

    assert!(signed_in(&browser.get(&rig, &link).await));
    // The same address again, from a browser that still holds the trip's cookie.
    let went = replayer.get(&rig, &link).await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=expired"));
    assert!(!signed_in(&went));
    assert_eq!(
        rig.fake.asked().len(),
        1,
        "the provider was not asked a second time"
    );
    assert_eq!(
        rig.state.accounts.invites_of(&club("north")).unwrap()[0].uses,
        1
    );
}

#[tokio::test]
async fn a_way_back_from_one_provider_does_not_work_at_another() {
    let rig = rig().await;
    let mut browser = Browser::default();
    let started = browser
        .start(
            &rig,
            "discord",
            &format!("?invite={}", rig.personal("north", Role::Pilot)),
        )
        .await;
    let params = query_of(&started.location);
    let code = approve(
        &rig,
        &params,
        json!({"sub": "g-1", "id": "111", "name": "Ace"}),
    );
    let went = browser
        .get(
            &rig,
            &format!(
                "/auth/google/callback?code={code}&state={}",
                params["state"]
            ),
        )
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=expired"));
    assert!(rig.fake.asked().is_empty());
    assert_eq!(rig.accounts_made(), 0);
}

#[tokio::test]
async fn saying_no_at_the_provider_or_a_code_it_will_not_take_makes_nothing() {
    let rig = rig().await;
    let secret = rig.personal("north", Role::Pilot);
    // The person pressed Cancel.
    let mut browser = Browser::default();
    let started = browser
        .start(&rig, "discord", &format!("?invite={secret}"))
        .await;
    let state = &query_of(&started.location)["state"];
    let went = browser
        .get(
            &rig,
            &format!("/auth/discord/callback?error=access_denied&state={state}"),
        )
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=refused"));
    // A code the provider never gave.
    let mut browser = Browser::default();
    let started = browser
        .start(&rig, "discord", &format!("?invite={secret}"))
        .await;
    let state = &query_of(&started.location)["state"];
    let went = browser
        .get(
            &rig,
            &format!("/auth/discord/callback?code=forged&state={state}"),
        )
        .await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=provider")
    );
    assert!(!signed_in(&went));
    // Come back with no code at all.
    let mut browser = Browser::default();
    let started = browser
        .start(&rig, "discord", &format!("?invite={secret}"))
        .await;
    let state = &query_of(&started.location)["state"];
    let went = browser
        .get(&rig, &format!("/auth/discord/callback?state={state}"))
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=refused"));

    assert_eq!(rig.accounts_made(), 0);
    assert_eq!(
        rig.state
            .accounts
            .peek_invite(&secret, now_ms())
            .unwrap()
            .unwrap()
            .uses,
        0
    );
}

#[tokio::test]
async fn a_code_meant_for_another_trip_is_refused_by_the_proof() {
    // The code is real, but it was given for another start (another challenge). The server
    // sends its own trip's secret, and the provider sees it does not fit.
    let rig = rig().await;
    let mut one = Browser::default();
    let first = query_of(
        &one.start(
            &rig,
            "discord",
            &format!("?invite={}", rig.personal("north", Role::Pilot)),
        )
        .await
        .location,
    );
    let mut two = Browser::default();
    let second = query_of(
        &two.start(
            &rig,
            "discord",
            &format!("?invite={}", rig.personal("north", Role::Pilot)),
        )
        .await
        .location,
    );
    let stolen = approve(&rig, &first, discord_user("111", "Ace"));
    let went = two
        .get(
            &rig,
            &format!(
                "/auth/discord/callback?code={stolen}&state={}",
                second["state"]
            ),
        )
        .await;
    assert_eq!(
        went.location,
        format!("{PUBLIC}/#/sign-in?problem=provider")
    );
    assert!(!signed_in(&went));
    assert_eq!(rig.accounts_made(), 0);
}

#[tokio::test]
async fn the_browser_is_only_ever_sent_to_this_servers_own_pages_afterwards() {
    let rig = rig().await;
    let secret = rig.personal("north", Role::Pilot);
    let mut browser = Browser::default();
    // Things a link might add in the hope of sending the person somewhere else.
    let query = format!(
        "?invite={secret}&next=https://evil.example&redirect=//evil.example&return_to=%2F%2Fevil.example"
    );
    let went = browser
        .sign_in(&rig, "discord", &query, discord_user("111", "Ace"))
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/"));
    // And every way a trip can end is one of this server's own pages.
    let mut other = Browser::default();
    for query in [
        "?code=x&state=y",
        "?error=access_denied&state=y",
        "?state=y&next=https://evil.example",
    ] {
        let went = other
            .get(&rig, &format!("/auth/discord/callback{query}"))
            .await;
        assert!(
            went.location.starts_with(&format!("{PUBLIC}/#/")),
            "{}",
            went.location
        );
    }
}

#[tokio::test]
async fn the_invite_a_person_came_with_is_never_sent_to_the_provider() {
    let rig = rig().await;
    let secret = rig.personal("north", Role::Pilot);
    let mut browser = Browser::default();
    let started = browser
        .start(&rig, "discord", &format!("?invite={secret}"))
        .await;
    assert!(!started.location.contains(&secret) && !started.location.contains(&secret[4..]));
    let params = query_of(&started.location);
    let code = approve(&rig, &params, discord_user("111", "Ace"));
    browser
        .get(
            &rig,
            &format!(
                "/auth/discord/callback?code={code}&state={}",
                params["state"]
            ),
        )
        .await;
    for form in rig.fake.asked() {
        assert!(!form.values().any(|v| v.contains(&secret)), "{form:?}");
    }
}

#[tokio::test]
async fn the_first_owner_is_made_by_signing_in_with_the_setup_link() {
    let rig = rig().await;
    let secret = rig.invite(InviteKind::Owner).1;
    assert!(!rig.state.accounts.has_owner().unwrap());
    let mut browser = Browser::default();
    let went = browser
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={secret}"),
            discord_user("1", "Ryan"),
        )
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/"));
    let me = browser.me(&rig).await;
    assert_eq!(me["account"]["owner"], true);
    assert!(rig.state.accounts.has_owner().unwrap());
    // The link is spent: a second person does not become an owner with it.
    let mut second = Browser::default();
    let went = second
        .sign_in(
            &rig,
            "discord",
            &format!("?invite={secret}"),
            discord_user("2", "Second"),
        )
        .await;
    assert_eq!(went.location, format!("{PUBLIC}/#/sign-in?problem=invite"));
    assert!(
        rig.state
            .accounts
            .account_for_sign_in("discord", "2")
            .unwrap()
            .is_none()
    );
}
