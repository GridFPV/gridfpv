//! Signing in with a username and a password kept on the server (docs/primitives.md
//! §13.8): off until an owner switches it on, never the password itself kept or shown,
//! wrong guesses slowed, and a re-connect link to set a forgotten one again.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_serverd::accounts::{AccountId, InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
}

/// A server with two clubs, so that what one club's director may do in the other can be
/// asked.
fn rig() -> Rig {
    rig_at(None)
}

fn rig_at(public_url: Option<&str>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"},{"id":"south","name":"South FPV"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path())
        .unwrap()
        .at(public_url.map(str::to_owned));
    Rig {
        app: router(state.clone()),
        state,
        _dir: dir,
    }
}

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

/// Someone with an account, and a session for it.
struct Person {
    id: AccountId,
    session: String,
}

impl Rig {
    /// Make an account the way one is made: an invite, used by someone who signed in.
    fn person(&self, name: &str, kind: InviteKind) -> Person {
        let now = now_ms();
        let expires_at = matches!(kind, InviteKind::Group { .. }).then_some(now + 86_400_000);
        let (_, secret) = self
            .state
            .accounts
            .new_invite(
                NewInvite {
                    kind,
                    label: name.into(),
                    max_uses: 5,
                    expires_at,
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
        let (account, _, session) = self
            .state
            .accounts
            .join_with_invite(&secret, &sign_in, name, now)
            .unwrap();
        Person {
            id: account.id,
            session,
        }
    }

    fn member(&self, name: &str, club_id: &str, role: Role) -> Person {
        self.person(
            name,
            InviteKind::Personal {
                club: club(club_id),
                role,
                pilot: None,
            },
        )
    }

    fn owner(&self, name: &str) -> Person {
        self.person(name, InviteKind::Owner)
    }
}

/// One request. `who` signs it with that person's session cookie.
async fn call(
    rig: &Rig,
    who: Option<&Person>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_with(
        rig,
        who.map(|p| p.session.as_str()),
        method,
        path,
        body,
        &[],
    )
    .await
    .0
}

async fn call_with(
    rig: &Rig,
    session: Option<&str>,
    method: Method,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> ((StatusCode, Value), Vec<String>) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "grid.example.org");
    if let Some(session) = session {
        request = request.header(
            header::COOKIE,
            format!("other=1; {SESSION_COOKIE}={session}; more=2"),
        );
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
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
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ),
        cookies,
    )
}

// ── Helpers ─────────────────────────────────────────────────────────────────

const PW: &str = "correct horse battery";
const PW2: &str = "another long password";
const OFF: &str = "switched off";

impl Rig {
    fn on(&self) {
        self.state.book.set_password_sign_in(true).unwrap();
    }

    /// An invite to North as a pilot.
    fn invite(&self) -> String {
        let invite = NewInvite {
            kind: InviteKind::Personal {
                club: club("north"),
                role: Role::Pilot,
                pilot: None,
            },
            label: "someone".into(),
            max_uses: 1,
            expires_at: None,
        };
        self.state
            .accounts
            .new_invite(invite, None, now_ms())
            .unwrap()
            .1
    }

    fn accounts(&self) -> usize {
        self.state.accounts.all_accounts().unwrap().len()
    }
}

/// The session a `Set-Cookie` answer gives, when it gives one.
fn session_in(cookies: &[String]) -> Option<String> {
    cookies.iter().find_map(|line| {
        let value = line.strip_prefix(&format!("{SESSION_COOKIE}="))?;
        let value = value.split(';').next()?;
        (!value.is_empty()).then(|| value.to_owned())
    })
}

async fn post(
    rig: &Rig,
    session: Option<&str>,
    path: &str,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    let ((status, answer), cookies) =
        call_with(rig, session, Method::POST, path, Some(body), &[]).await;
    (status, answer, session_in(&cookies))
}

async fn sign_in(rig: &Rig, username: &str, password: &str) -> (StatusCode, Value, Option<String>) {
    let body = json!({"username": username, "password": password});
    post(rig, None, "/api/sign-in/password", body).await
}

async fn sign_up(rig: &Rig, body: Value) -> (StatusCode, Value, Option<String>) {
    post(rig, None, "/api/sign-up/password", body).await
}

/// Make an account with a username and password, by invite. Returns its id and session.
async fn made(rig: &Rig, username: &str, password: &str) -> (String, String) {
    let body = json!({"username": username, "password": password, "invite": rig.invite()});
    let (status, me, session) = sign_up(rig, body).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    (
        me["account"]["id"].as_str().unwrap().to_owned(),
        session.expect("signing up signs in"),
    )
}

async fn me_as(rig: &Rig, session: &str) -> Value {
    call_with(rig, Some(session), Method::GET, "/api/me", None, &[])
        .await
        .0
        .1
}

async fn set_password(rig: &Rig, session: &str, body: Value) -> (StatusCode, Value) {
    call_with(
        rig,
        Some(session),
        Method::PUT,
        "/api/me/password",
        Some(body),
        &[],
    )
    .await
    .0
}

// ── Off until an owner says ─────────────────────────────────────────────────

#[tokio::test]
async fn it_is_off_on_a_new_server_and_nothing_about_passwords_works() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let (_, info) = call(&rig, None, Method::GET, "/api/server", None).await;
    assert_eq!(info["password_sign_in"], false);

    let (status, body, session) = sign_in(&rig, "ace", PW).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["message"].as_str().unwrap().contains(OFF), "{body}");
    assert!(session.is_none());
    let up = json!({"username": "ace", "password": PW, "invite": rig.invite()});
    let (status, body, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["message"].as_str().unwrap().contains(OFF));
    assert!(session.is_none());
    let add = json!({"username": "ace", "password": PW});
    let (status, _) = set_password(&rig, &ace.session, add).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Nothing was made, and the invite was not used.
    assert_eq!(rig.accounts(), 1);
    assert!(rig.state.accounts.password_of("ace").unwrap().is_none());
}

#[tokio::test]
async fn only_an_owner_switches_it_and_not_so_as_to_lock_themselves_out() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let dee = rig.member("Dee", "north", Role::Director);
    let path = "/api/server/password-sign-in";
    let change = |on: bool| Some(json!({"on": on}));

    let (status, _) = call(&rig, None, Method::PUT, path, change(true)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&rig, Some(&dee), Method::PUT, path, change(true)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!rig.state.book.password_sign_in());

    let (status, info) = call(&rig, Some(&owner), Method::PUT, path, change(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        info,
        json!({"open_sign_up": false, "password_sign_in": true})
    );
    // Kept: a server started again on the same data still has it on.
    assert!(
        AppState::open(rig._dir.path())
            .unwrap()
            .book
            .password_sign_in()
    );
    // The other setting in the same answer is not touched by it.
    assert!(!rig.state.book.open_sign_up());

    // An owner whose only way in is a password may not switch passwords off.
    let setup = NewInvite {
        kind: InviteKind::Owner,
        label: "boss".into(),
        max_uses: 1,
        expires_at: None,
    };
    let link = rig
        .state
        .accounts
        .new_invite(setup, None, now_ms())
        .unwrap()
        .1;
    let up = json!({"username": "boss", "password": PW, "invite": link});
    let (status, me, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    // The setup link makes an owner, whichever way they sign in.
    assert_eq!(me["account"]["owner"], true);
    let session = session.unwrap();
    let ((status, body), _) =
        call_with(&rig, Some(&session), Method::PUT, path, change(false), &[]).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("lock yourself out")
    );
    assert!(rig.state.book.password_sign_in());
    // An owner who signs in another way may.
    let (status, info) = call(&rig, Some(&owner), Method::PUT, path, change(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["password_sign_in"], false);
}

// ── Making an account ───────────────────────────────────────────────────────

#[tokio::test]
async fn an_invite_makes_an_account_with_a_username_and_the_password_is_never_kept_or_shown() {
    let rig = rig();
    rig.on();
    let up =
        json!({"username": " Ace_FPV ", "password": PW, "name": "Ace", "invite": rig.invite()});
    let ((status, me), cookies) = call_with(
        &rig,
        None,
        Method::POST,
        "/api/sign-up/password",
        Some(up),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    // Signed in, with the same kind of cookie any sign-in gives.
    let session = session_in(&cookies).expect("a session");
    assert!(
        cookies
            .iter()
            .any(|c| c.contains("HttpOnly") && c.contains("SameSite=Lax"))
    );
    assert_eq!(me["account"]["name"], "Ace");
    assert_eq!(me["account"]["clubs"][0]["role"], "Pilot");
    // The username is how the account signs in. It is kept in lower case.
    assert_eq!(
        me["account"]["sign_ins"],
        json!([{"provider": "password", "subject": "ace_fpv", "label": "ace_fpv"}])
    );
    // Neither the password nor its hash is in any answer.
    let later = me_as(&rig, &session).await;
    for answer in [&me, &later] {
        let text = answer.to_string();
        assert!(!text.contains(PW) && !text.contains("argon2"), "{text}");
    }
    // What is kept is a hash, and not the password.
    let (_, hash) = rig.state.accounts.password_of("ace_fpv").unwrap().unwrap();
    let hash = hash.unwrap();
    assert!(hash.starts_with("$argon2id$"), "{hash}");
    assert!(!hash.contains(PW));
    let on_disk = std::fs::read(rig._dir.path().join("accounts.sqlite")).unwrap();
    assert!(!String::from_utf8_lossy(&on_disk).contains(PW));

    // Signing in: the username in any case, the password exactly.
    for username in ["ace_fpv", "ACE_FPV", "  Ace_Fpv "] {
        let (status, me, session) = sign_in(&rig, username, PW).await;
        assert_eq!(status, StatusCode::OK, "{username}: {me}");
        assert_eq!(me["account"]["name"], "Ace");
        assert!(session.is_some());
    }
    let (status, _, session) = sign_in(&rig, "ace_fpv", &PW.to_uppercase()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(session.is_none());
}

#[tokio::test]
async fn a_name_left_out_is_the_username_as_it_was_typed() {
    let rig = rig();
    rig.on();
    let up = json!({"username": " Quin_FPV ", "password": PW, "invite": rig.invite()});
    let (_, me, _) = sign_up(&rig, up).await;
    // Called as it was typed. Signed in with in lower case.
    assert_eq!(me["account"]["name"], "Quin_FPV");
    assert_eq!(me["account"]["sign_ins"][0]["subject"], "quin_fpv");
    // A name that is only spaces is no name.
    let up = json!({"username": "ray", "name": "   ", "password": PW, "invite": rig.invite()});
    let (_, me, _) = sign_up(&rig, up).await;
    assert_eq!(me["account"]["name"], "ray");
}

#[tokio::test]
async fn with_no_invite_an_account_is_made_only_when_the_owner_lets_anyone_in() {
    let rig = rig();
    rig.on();
    let up = json!({"username": "pia", "password": PW});
    let (status, body, session) = sign_up(&rig, up.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["message"].as_str().unwrap().contains("need an invite"));
    assert!(session.is_none());
    assert_eq!(rig.accounts(), 0);
    // An empty invite is no invite.
    let empty = json!({"username": "pia", "password": PW, "invite": ""});
    assert_eq!(sign_up(&rig, empty).await.0, StatusCode::FORBIDDEN);

    rig.state.book.set_open_sign_up(true).unwrap();
    let (status, me, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert!(session.is_some());
    // In no club, an owner of nothing.
    assert_eq!(me["account"]["clubs"], json!([]));
    assert_eq!(me["account"]["owner"], false);
    assert_eq!(rig.accounts(), 1);
}

#[tokio::test]
async fn what_is_not_a_username_or_a_password_makes_nothing_and_uses_no_invite() {
    let rig = rig();
    rig.on();
    let link = rig.invite();
    for (username, password, word) in [
        ("ab", PW, "username"),
        ("two words", PW, "username"),
        ("ace@example.org", PW, "username"),
        ("", PW, "username"),
        ("ace", "short", "at least 10"),
        ("ace", "", "at least 10"),
        ("ace", &"x".repeat(201), "200"),
    ] {
        let up = json!({"username": username, "password": password, "invite": link});
        let (status, body, session) = sign_up(&rig, up).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{username:?}");
        assert!(body["message"].as_str().unwrap().contains(word), "{body}");
        assert!(session.is_none());
    }
    assert_eq!(rig.accounts(), 0);
    // The invite still works: nothing above used it.
    let up = json!({"username": "ace", "password": PW, "invite": link});
    assert_eq!(sign_up(&rig, up).await.0, StatusCode::OK);
    // And works once.
    let again = json!({"username": "bee", "password": PW, "invite": link});
    let (status, _, _) = sign_up(&rig, again).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(rig.accounts(), 1);
    // Something that is not a sign-up at all.
    let (status, _, _) = sign_up(&rig, json!({"username": 7})).await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn a_username_someone_has_cannot_be_taken_and_their_password_is_not_changed() {
    let rig = rig();
    rig.on();
    let (first, _) = made(&rig, "ace", PW).await;
    // Someone else tries the same username, in another case, with their own password.
    let link = rig.invite();
    let up = json!({"username": "ACE", "password": PW2, "invite": link});
    let (status, body, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(session.is_none());
    assert_eq!(rig.accounts(), 1);
    // The first account still signs in with its own password, and not with theirs.
    let (status, me, _) = sign_in(&rig, "ace", PW).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["account"]["id"], first);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::UNAUTHORIZED);
    // Their invite was not used up by the refusal.
    let up = json!({"username": "bee", "password": PW2, "invite": link});
    assert_eq!(sign_up(&rig, up).await.0, StatusCode::OK);
}

// ── Signing in ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_wrong_password_and_a_username_nobody_has_get_the_same_answer() {
    let rig = rig();
    rig.on();
    made(&rig, "ace", PW).await;
    let (wrong_status, wrong, _) = sign_in(&rig, "ace", "not the password").await;
    let (nobody_status, nobody, _) = sign_in(&rig, "nobody", "not the password").await;
    assert_eq!(wrong_status, StatusCode::UNAUTHORIZED);
    assert_eq!((nobody_status, &nobody), (wrong_status, &wrong));
    // A username that could not be one says the same again.
    let (status, odd, _) = sign_in(&rig, "not a username!", PW).await;
    assert_eq!((status, &odd), (wrong_status, &wrong));
    assert_eq!(wrong["message"], "That username and password do not match.");
}

#[tokio::test]
async fn five_wrong_passwords_make_a_username_wait_even_for_the_right_one() {
    let rig = rig();
    rig.on();
    made(&rig, "ace", PW).await;
    made(&rig, "bee", PW).await;
    for _ in 0..5 {
        assert_eq!(
            sign_in(&rig, "ace", "a wrong password").await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // The sixth is not looked at, right or wrong.
    let (status, body, session) = sign_in(&rig, "ace", PW).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(body["message"].as_str().unwrap().contains("Wait"), "{body}");
    assert!(session.is_none());
    // In any case of the username.
    assert_eq!(
        sign_in(&rig, "ACE", PW).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Someone else is not held up.
    assert_eq!(sign_in(&rig, "bee", PW).await.0, StatusCode::OK);
    // A username nobody has waits the same way, so it cannot be told from a real one.
    for _ in 0..5 {
        sign_in(&rig, "nobody", "a wrong password").await;
    }
    assert_eq!(
        sign_in(&rig, "nobody", PW).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn four_wrong_and_then_right_signs_in_and_starts_the_count_over() {
    let rig = rig();
    rig.on();
    made(&rig, "ace", PW).await;
    for _ in 0..2 {
        for _ in 0..4 {
            sign_in(&rig, "ace", "a wrong password").await;
        }
        assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::OK);
    }
}

#[tokio::test]
async fn a_sign_in_asked_for_by_another_website_is_refused() {
    let rig = rig();
    rig.on();
    made(&rig, "ace", PW).await;
    let body = Some(json!({"username": "ace", "password": PW}));
    let other = [("origin", "https://evil.example")];
    let ((status, _), cookies) = call_with(
        &rig,
        None,
        Method::POST,
        "/api/sign-in/password",
        body.clone(),
        &other,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(session_in(&cookies).is_none());
    let ours = [("origin", "https://grid.example.org")];
    let ((status, _), _) = call_with(
        &rig,
        None,
        Method::POST,
        "/api/sign-in/password",
        body,
        &ours,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ── Adding, changing, taking off ────────────────────────────────────────────

#[tokio::test]
async fn an_account_that_signs_in_another_way_adds_a_username_and_password() {
    let rig = rig();
    rig.on();
    let ace = rig.member("Ace", "north", Role::Pilot);
    // A username is needed to add one.
    let (status, _) = set_password(&rig, &ace.session, json!({"password": PW})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = set_password(
        &rig,
        &ace.session,
        json!({"username": "ace", "password": "short"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let add = json!({"username": "Ace", "password": PW});
    let (status, me) = set_password(&rig, &ace.session, add).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    let ways: Vec<&str> = me["account"]["sign_ins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|held| held["provider"].as_str().unwrap())
        .collect();
    assert_eq!(ways.len(), 2);
    assert!(ways.contains(&"discord") && ways.contains(&"password"));
    // It signs in to the same account.
    let (status, me, _) = sign_in(&rig, "ace", PW).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["account"]["id"], ace.id.0);

    // Nobody signed in adds nothing. A username someone has cannot be added by another.
    let ((status, _), _) = call_with(
        &rig,
        None,
        Method::PUT,
        "/api/me/password",
        Some(json!({"username": "x-ray", "password": PW})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let bee = rig.member("Bee", "north", Role::Pilot);
    let take = json!({"username": "ace", "password": PW2});
    let (status, _) = set_password(&rig, &bee.session, take).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn changing_a_password_needs_the_one_there_is_and_signs_everyone_else_out() {
    let rig = rig();
    rig.on();
    let (_, here) = made(&rig, "ace", PW).await;
    let (_, _, elsewhere) = sign_in(&rig, "ace", PW).await;
    let elsewhere = elsewhere.unwrap();

    // Without the password there is, or with a wrong one, nothing changes.
    for body in [
        json!({"password": PW2}),
        json!({"current": "not the password", "password": PW2}),
        json!({"current": "", "password": PW2}),
    ] {
        let (status, why) = set_password(&rig, &here, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
    }
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::OK);
    // A new password that is too short is refused before anything else.
    let short = json!({"current": PW, "password": "short"});
    assert_eq!(
        set_password(&rig, &here, short).await.0,
        StatusCode::BAD_REQUEST
    );
    // A username given with a change does not rename the account's username.
    let (status, me) = set_password(
        &rig,
        &here,
        json!({"current": PW, "password": PW2, "username": "other"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["account"]["sign_ins"][0]["subject"], "ace");
    assert!(rig.state.accounts.password_of("other").unwrap().is_none());

    // The new one works and the old one does not.
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::OK);
    // This browser stays signed in. The other one is signed out.
    assert_eq!(
        me_as(&rig, &here).await["account"]["sign_ins"][0]["subject"],
        "ace"
    );
    assert_eq!(me_as(&rig, &elsewhere).await, json!({}));
}

#[tokio::test]
async fn wrong_guesses_at_the_password_there_is_are_slowed_too() {
    let rig = rig();
    rig.on();
    let (_, session) = made(&rig, "ace", PW).await;
    for _ in 0..5 {
        let body = json!({"current": "a wrong password", "password": PW2});
        assert_eq!(
            set_password(&rig, &session, body).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let body = json!({"current": PW, "password": PW2});
    assert_eq!(
        set_password(&rig, &session, body).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // The password was not changed by any of it.
    let hash = rig.state.accounts.password_of("ace").unwrap().unwrap().1;
    assert!(gridfpv_serverd::password::verify_password(
        PW,
        &hash.unwrap()
    ));
}

#[tokio::test]
async fn a_username_is_taken_off_like_any_sign_in_and_the_last_way_in_stays() {
    let rig = rig();
    rig.on();
    let ace = rig.member("Ace", "north", Role::Pilot);
    set_password(
        &rig,
        &ace.session,
        json!({"username": "ace", "password": PW}),
    )
    .await;

    let off = "/api/me/sign-ins/password/ace";
    let (status, me) = call(&rig, Some(&ace), Method::DELETE, off, None).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["account"]["sign_ins"].as_array().unwrap().len(), 1);
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::UNAUTHORIZED);
    // The hash is gone with it, and the username is free again.
    assert!(rig.state.accounts.password_of("ace").unwrap().is_none());
    let bee = rig.member("Bee", "north", Role::Pilot);
    let take = json!({"username": "ace", "password": PW2});
    assert_eq!(
        set_password(&rig, &bee.session, take).await.0,
        StatusCode::OK
    );
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::OK);

    // An account whose only way in is a password keeps it.
    let (_, session) = made(&rig, "solo", PW).await;
    let ((status, _), _) = call_with(
        &rig,
        Some(&session),
        Method::DELETE,
        "/api/me/sign-ins/password/solo",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(sign_in(&rig, "solo", PW).await.0, StatusCode::OK);
}

// ── A forgotten password ────────────────────────────────────────────────────

#[tokio::test]
async fn an_owners_re_connect_link_sets_a_forgotten_password_again() {
    let rig = rig();
    rig.on();
    let (id, old_session) = made(&rig, "ace", PW).await;
    let account = AccountId(id.clone());
    let link = rig
        .state
        .accounts
        .new_reconnect(&account, None, now_ms())
        .unwrap()
        .1;

    // She opens the link and gives a password. The same username, and a new password.
    let up = json!({"username": "ace", "password": PW2, "invite": link});
    let (status, me, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    // It is her own account, with its club, and she is signed in.
    assert_eq!(me["account"]["id"], id);
    assert_eq!(me["account"]["clubs"][0]["role"], "Pilot");
    assert!(session.is_some());
    assert_eq!(rig.accounts(), 1);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::OK);
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::UNAUTHORIZED);
    // Whoever was signed in before is still signed in: only the password changed.
    assert_eq!(me_as(&rig, &old_session).await["account"]["id"], id);

    // The link works once.
    let again = json!({"username": "ace", "password": PW, "invite": link});
    assert_eq!(sign_up(&rig, again).await.0, StatusCode::NOT_FOUND);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_link_can_give_an_account_a_new_username_and_the_old_one_stops_working() {
    let rig = rig();
    rig.on();
    let (id, old_session) = made(&rig, "ace", PW).await;
    let account = AccountId(id.clone());
    let link = rig
        .state
        .accounts
        .new_reconnect(&account, None, now_ms())
        .unwrap()
        .1;
    let up = json!({"username": "ace2", "password": PW2, "invite": link});
    let (status, me, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["account"]["id"], id);
    // One username at a time.
    assert_eq!(
        me["account"]["sign_ins"],
        json!([{"provider": "password", "subject": "ace2", "label": "ace2"}])
    );
    assert_eq!(sign_in(&rig, "ace2", PW2).await.0, StatusCode::OK);
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(sign_in(&rig, "ace", PW2).await.0, StatusCode::UNAUTHORIZED);
    // The old username went with the old sign-in, and so did everyone signed in by it.
    assert_eq!(me_as(&rig, &old_session).await, json!({}));
    assert_eq!(me_as(&rig, &session.unwrap()).await["account"]["id"], id);
}

#[tokio::test]
async fn a_link_cannot_take_a_username_another_account_has() {
    let rig = rig();
    rig.on();
    let (ace, _) = made(&rig, "ace", PW).await;
    made(&rig, "bee", PW).await;
    let link = rig
        .state
        .accounts
        .new_reconnect(&AccountId(ace), None, now_ms())
        .unwrap()
        .1;
    let up = json!({"username": "bee", "password": PW2, "invite": link});
    let (status, _, session) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(session.is_none());
    // Bee's password is Bee's, and the link is left to use.
    assert_eq!(sign_in(&rig, "bee", PW).await.0, StatusCode::OK);
    assert_eq!(sign_in(&rig, "bee", PW2).await.0, StatusCode::UNAUTHORIZED);
    let up = json!({"username": "ace", "password": PW2, "invite": link});
    assert_eq!(sign_up(&rig, up).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_link_gives_an_account_that_signed_in_another_way_a_password() {
    let rig = rig();
    rig.on();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let link = rig
        .state
        .accounts
        .new_reconnect(&ace.id, None, now_ms())
        .unwrap()
        .1;
    let up = json!({"username": "ace", "password": PW, "invite": link});
    let (status, me, _) = sign_up(&rig, up).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["account"]["id"], ace.id.0);
    // The Discord sign-in is still there beside it.
    assert_eq!(me["account"]["sign_ins"].as_array().unwrap().len(), 2);
    // The name she had is kept: a link does not rename an account.
    assert_eq!(me["account"]["name"], "Ace");
}

// ── Switched off again ──────────────────────────────────────────────────────

#[tokio::test]
async fn switched_off_again_passwords_stop_working_and_are_kept_for_when_it_is_back() {
    let rig = rig();
    rig.on();
    let (_, session) = made(&rig, "ace", PW).await;
    rig.state.book.set_password_sign_in(false).unwrap();

    let (status, body, _) = sign_in(&rig, "ace", PW).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["message"].as_str().unwrap().contains(OFF));
    // Whoever is signed in already stays signed in, and cannot change the password.
    assert_eq!(me_as(&rig, &session).await["account"]["name"], "ace");
    let change = json!({"current": PW, "password": PW2});
    assert_eq!(
        set_password(&rig, &session, change).await.0,
        StatusCode::FORBIDDEN
    );

    rig.on();
    assert_eq!(sign_in(&rig, "ace", PW).await.0, StatusCode::OK);
}
