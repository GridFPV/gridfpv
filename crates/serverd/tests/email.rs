//! Email a server sends (docs/primitives.md §13.8): confirming an address, and a link to
//! set a forgotten password. Most tests use a mailer that keeps what it is given. One
//! talks to an email server over a real connection.

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
    rig_at(Some("https://grid.example.org"))
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
        .at(public_url.map(str::to_owned))
        .with_mailer(Some(gridfpv_serverd::email::Mailer::outbox()));
    state.book.set_password_sign_in(true).unwrap();
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

use gridfpv_serverd::email::{Mail, MailSettings, MailTls, Mailer};

const PW: &str = "correct horse battery";
const PW2: &str = "another long password";
const HOUR: i64 = 60 * 60 * 1000;

impl Rig {
    fn mails(&self) -> Vec<Mail> {
        self.state.mailer.as_ref().unwrap().sent()
    }

    /// Someone who signs in with a username and a password, in North.
    async fn with_password(&self, username: &str) -> Person {
        let invite = NewInvite {
            kind: InviteKind::Personal {
                club: club("north"),
                role: Role::Pilot,
                pilot: None,
            },
            label: username.into(),
            max_uses: 1,
            expires_at: None,
        };
        let link = self
            .state
            .accounts
            .new_invite(invite, None, now_ms())
            .unwrap()
            .1;
        let body = json!({"username": username, "password": PW, "invite": link});
        let ((status, me), cookies) = call_with(
            self,
            None,
            Method::POST,
            "/api/sign-up/password",
            Some(body),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{me}");
        let session = cookies
            .iter()
            .find_map(|line| line.strip_prefix(&format!("{SESSION_COOKIE}=")))
            .and_then(|rest| rest.split(';').next())
            .unwrap()
            .to_owned();
        Person {
            id: AccountId(me["account"]["id"].as_str().unwrap().to_owned()),
            session,
        }
    }

    /// Give an account an address it has already confirmed.
    fn confirmed(&self, who: &Person, address: &str) {
        let accounts = &self.state.accounts;
        let secret = accounts.set_email(&who.id, address, now_ms()).unwrap();
        assert!(accounts.verify_email(&secret, now_ms()).unwrap().is_some());
    }
}

async fn give(rig: &Rig, who: Option<&Person>, address: &str) -> (StatusCode, Value) {
    let body = Some(json!({"address": address}));
    call(rig, who, Method::PUT, "/api/me/email", body).await
}

async fn confirm(rig: &Rig, secret: &str) -> (StatusCode, Value) {
    let body = Some(json!({"secret": secret}));
    call(rig, None, Method::POST, "/api/email/confirm", body).await
}

async fn forgot(rig: &Rig, who: &str) -> StatusCode {
    let body = Some(json!({"who": who}));
    call(rig, None, Method::POST, "/api/password/forgot", body)
        .await
        .0
}

async fn email_of(rig: &Rig, who: &Person) -> Value {
    let (_, me) = call(rig, Some(who), Method::GET, "/api/me", None).await;
    me["account"].get("email").cloned().unwrap_or(Value::Null)
}

/// The secret in the link a mail carries, after `marker` (`confirm-email/`, `reconnect/`).
fn secret_in(mail: &Mail, marker: &str) -> String {
    let at = mail
        .body
        .find(marker)
        .unwrap_or_else(|| panic!("no {marker} in {}", mail.body));
    mail.body[at + marker.len()..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

async fn sign_in(rig: &Rig, username: &str, password: &str) -> StatusCode {
    let body = Some(json!({"username": username, "password": password}));
    call(rig, None, Method::POST, "/api/sign-in/password", body)
        .await
        .0
}

// ── A server that sends no email ────────────────────────────────────────────

#[tokio::test]
async fn a_server_with_no_email_set_up_says_so_and_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path())
        .unwrap()
        .at(Some("https://grid.example.org".into()));
    state.book.set_password_sign_in(true).unwrap();
    let rig = Rig {
        app: router(state.clone()),
        state,
        _dir: dir,
    };
    let (_, info) = call(&rig, None, Method::GET, "/api/server", None).await;
    assert_eq!(info["email"], false);
    let ace = rig.owner("Ace");
    let (status, why) = give(&rig, Some(&ace), "ace@example.org").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        why["message"]
            .as_str()
            .unwrap()
            .contains("does not send email")
    );
    assert_eq!(forgot(&rig, "ace").await, StatusCode::CONFLICT);
    assert!(rig.state.accounts.email_of(&ace.id).unwrap().is_none());
}

#[tokio::test]
async fn email_needs_the_servers_own_address_to_put_in_a_link() {
    let rig = rig_at(None);
    let (_, info) = call(&rig, None, Method::GET, "/api/server", None).await;
    assert_eq!(info["email"], false);
    let ace = rig.member("Ace", "north", Role::Pilot);
    assert_eq!(
        give(&rig, Some(&ace), "ace@example.org").await.0,
        StatusCode::CONFLICT
    );
    assert!(rig.mails().is_empty());
}

// ── Confirming an address ───────────────────────────────────────────────────

#[tokio::test]
async fn an_address_is_confirmed_by_the_link_sent_to_it_and_by_nothing_else() {
    let rig = rig();
    let (_, info) = call(&rig, None, Method::GET, "/api/server", None).await;
    assert_eq!(info["email"], true);
    let ace = rig.member("Ace", "north", Role::Pilot);
    assert_eq!(email_of(&rig, &ace).await, Value::Null);

    let (status, me) = give(&rig, Some(&ace), "  Ace@Example.ORG ").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    // Kept tidied, and not confirmed yet.
    let waiting = json!({"address": "ace@example.org", "confirmed": false});
    assert_eq!(me["account"]["email"], waiting);
    // One mail, to that address, with a link on this server.
    let mails = rig.mails();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, "ace@example.org");
    assert!(
        mails[0].subject.contains("grid.example.org"),
        "{}",
        mails[0].subject
    );
    assert!(
        mails[0]
            .body
            .contains("https://grid.example.org/#/confirm-email/gfe_")
    );
    let secret = secret_in(&mails[0], "confirm-email/");
    // The link is not in any answer: only the inbox has it.
    assert!(!me.to_string().contains(&secret));

    // Something that is not the link confirms nothing.
    for wrong in [
        "",
        "gfe_",
        "gfe_not-the-link",
        "gfr_something",
        &secret[..secret.len() - 1],
    ] {
        assert_eq!(
            confirm(&rig, wrong).await.0,
            StatusCode::NOT_FOUND,
            "{wrong:?}"
        );
    }
    assert_eq!(email_of(&rig, &ace).await, waiting);

    // Whoever opens the link confirms it. They need not be signed in.
    let (status, answer) = confirm(&rig, &secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer, json!({"address": "ace@example.org"}));
    assert_eq!(
        email_of(&rig, &ace).await,
        json!({"address": "ace@example.org", "confirmed": true})
    );
    // The link works once.
    assert_eq!(confirm(&rig, &secret).await.0, StatusCode::NOT_FOUND);
    // It is still there when the server starts again.
    let again = AppState::open(rig._dir.path()).unwrap();
    assert_eq!(
        again.accounts.email_of(&ace.id).unwrap(),
        Some(("ace@example.org".to_owned(), true))
    );

    // Giving the same confirmed address again sends nothing and changes nothing.
    let (status, _) = give(&rig, Some(&ace), "ace@example.org").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.mails().len(), 1);
    assert_eq!(email_of(&rig, &ace).await["confirmed"], true);
}

#[tokio::test]
async fn a_new_address_is_not_confirmed_and_the_link_to_the_old_one_stops_working() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let accounts = &rig.state.accounts;
    let now = now_ms();
    let first = accounts.set_email(&ace.id, "old@example.org", now).unwrap();
    let second = accounts.set_email(&ace.id, "new@example.org", now).unwrap();
    // The link sent to the old address does not confirm the new one.
    assert!(accounts.verify_email(&first, now).unwrap().is_none());
    assert_eq!(
        accounts.email_of(&ace.id).unwrap(),
        Some(("new@example.org".to_owned(), false))
    );
    assert!(accounts.verify_email(&second, now).unwrap().is_some());
    // A confirmed address, changed, is not confirmed any more.
    accounts
        .set_email(&ace.id, "third@example.org", now)
        .unwrap();
    assert_eq!(
        accounts.email_of(&ace.id).unwrap(),
        Some(("third@example.org".to_owned(), false))
    );
}

#[tokio::test]
async fn a_link_to_confirm_runs_out_after_a_day() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let accounts = &rig.state.accounts;
    let now = now_ms();
    let secret = accounts.set_email(&ace.id, "ace@example.org", now).unwrap();
    assert!(
        accounts
            .verify_email(&secret, now + 25 * HOUR)
            .unwrap()
            .is_none()
    );
    assert!(
        accounts
            .verify_email(&secret, now + 23 * HOUR)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn what_is_not_an_address_is_refused_and_nobody_else_gives_one_for_an_account() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    for bad in [
        "",
        "ace",
        "ace@",
        "ace@localhost",
        "Ace <ace@example.org>",
        "ace@example.org\nBcc: other@example.org",
        "ace @example.org",
    ] {
        let (status, why) = give(&rig, Some(&ace), bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {why}");
    }
    assert_eq!(
        give(&rig, None, "ace@example.org").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(rig.mails().is_empty());
    assert!(rig.state.accounts.email_of(&ace.id).unwrap().is_none());
}

#[tokio::test]
async fn asking_again_straight_away_sends_no_second_mail() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    assert_eq!(
        give(&rig, Some(&ace), "ace@example.org").await.0,
        StatusCode::OK
    );
    let (status, why) = give(&rig, Some(&ace), "ace@example.org").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(why["message"].as_str().unwrap().contains("Wait a minute"));
    assert_eq!(rig.mails().len(), 1);
    // Someone else is not held up by it.
    let bee = rig.member("Bee", "north", Role::Pilot);
    assert_eq!(
        give(&rig, Some(&bee), "bee@example.org").await.0,
        StatusCode::OK
    );
    assert_eq!(rig.mails().len(), 2);
}

#[tokio::test]
async fn an_address_is_taken_off_and_is_shown_to_nobody_but_its_own_account() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let dee = rig.member("Dee", "north", Role::Director);
    let owner = rig.owner("Oz");
    rig.confirmed(&ace, "ace-secret@example.org");

    // Not to the club's director, nor to an owner, wherever accounts are listed.
    for (who, path) in [
        (&dee, "/api/clubs/north/members".to_owned()),
        (&dee, "/api/clubs/south/people?q=ace".to_owned()),
        (&owner, "/api/clubs/south/people?q=ace".to_owned()),
        (&owner, format!("/api/accounts/{}/sign-ins", ace.id.0)),
        (&dee, "/api/me".to_owned()),
    ] {
        let (_, answer) = call(&rig, Some(who), Method::GET, &path, None).await;
        assert!(
            !answer.to_string().contains("ace-secret"),
            "{path}: {answer}"
        );
    }

    let (status, me) = call(&rig, Some(&ace), Method::DELETE, "/api/me/email", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(me["account"].get("email").is_none());
    assert!(rig.state.accounts.email_of(&ace.id).unwrap().is_none());
    // Taking off what is not there is nothing.
    let (status, _) = call(&rig, Some(&ace), Method::DELETE, "/api/me/email", None).await;
    assert_eq!(status, StatusCode::OK);
}

// ── A forgotten password ────────────────────────────────────────────────────

#[tokio::test]
async fn someone_who_forgot_their_password_is_sent_a_link_and_sets_a_new_one() {
    let rig = rig();
    let ace = rig.with_password("ace").await;
    rig.confirmed(&ace, "ace@example.org");

    assert_eq!(forgot(&rig, " ACE ").await, StatusCode::NO_CONTENT);
    let mails = rig.mails();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].to, "ace@example.org");
    // It names the username, since that is the other thing people forget.
    assert!(mails[0].body.contains("\"ace\""), "{}", mails[0].body);
    assert!(
        mails[0]
            .body
            .contains("https://grid.example.org/#/reconnect/gfr_")
    );
    // The password is not changed by asking.
    assert_eq!(sign_in(&rig, "ace", PW).await, StatusCode::OK);

    // The link sets a new password, for the same account.
    let secret = secret_in(&mails[0], "reconnect/");
    let body = json!({"username": "ace", "password": PW2, "invite": secret});
    let (status, me) = call(
        &rig,
        None,
        Method::POST,
        "/api/sign-up/password",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["account"]["id"], ace.id.0);
    assert_eq!(sign_in(&rig, "ace", PW2).await, StatusCode::OK);
    assert_eq!(sign_in(&rig, "ace", PW).await, StatusCode::UNAUTHORIZED);
    // The link worked once.
    let again = json!({"username": "ace", "password": PW, "invite": secret});
    let (status, _) = call(
        &rig,
        None,
        Method::POST,
        "/api/sign-up/password",
        Some(again),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_link_sent_by_email_has_an_hour() {
    let rig = rig();
    let ace = rig.with_password("ace").await;
    rig.confirmed(&ace, "ace@example.org");
    forgot(&rig, "ace").await;
    let secret = secret_in(&rig.mails()[0], "reconnect/");
    let accounts = &rig.state.accounts;
    let now = now_ms();
    assert!(
        accounts
            .peek_reconnect(&secret, now + 59 * 60 * 1000)
            .unwrap()
            .is_some()
    );
    assert!(
        accounts
            .peek_reconnect(&secret, now + 61 * 60 * 1000)
            .unwrap()
            .is_none()
    );
    // A link an owner hands over still has its seven days.
    let (_, handed) = accounts.new_reconnect(&ace.id, None, now).unwrap();
    assert!(
        accounts
            .peek_reconnect(&handed, now + 6 * 24 * HOUR)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_answer_is_the_same_whoever_is_asked_about_and_a_link_goes_only_where_it_should() {
    let rig = rig();
    // Confirmed, with a password: the one case a link is sent.
    let ace = rig.with_password("ace").await;
    rig.confirmed(&ace, "ace@example.org");
    // An address given and never confirmed.
    let bee = rig.with_password("bee").await;
    rig.state
        .accounts
        .set_email(&bee.id, "bee@example.org", now_ms())
        .unwrap();
    // A password and no address at all.
    rig.with_password("cat").await;
    // A confirmed address, and no password: they sign in with Discord.
    let dan = rig.member("Dan", "north", Role::Pilot);
    rig.confirmed(&dan, "dan@example.org");

    for who in [
        "bee",
        "bee@example.org",
        "cat",
        "dan@example.org",
        "nobody",
        "nobody@example.org",
        "not a username!",
        "not@an@address",
        "",
    ] {
        assert_eq!(forgot(&rig, who).await, StatusCode::NO_CONTENT, "{who:?}");
    }
    assert!(rig.mails().is_empty(), "{:?}", rig.mails());

    // By address works as by username.
    assert_eq!(
        forgot(&rig, "Ace@Example.org").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(rig.mails().len(), 1);
    // Asked again straight away: the same answer, and no second mail.
    assert_eq!(forgot(&rig, "ace").await, StatusCode::NO_CONTENT);
    assert_eq!(rig.mails().len(), 1);
}

#[tokio::test]
async fn two_accounts_with_one_address_each_get_their_own_link() {
    let rig = rig();
    let ace = rig.with_password("ace").await;
    let alt = rig.with_password("ace-alt").await;
    rig.confirmed(&ace, "shared@example.org");
    rig.confirmed(&alt, "shared@example.org");
    assert_eq!(
        forgot(&rig, "shared@example.org").await,
        StatusCode::NO_CONTENT
    );
    let mails = rig.mails();
    assert_eq!(mails.len(), 2);
    assert!(mails[0].body.contains("\"ace\""));
    assert!(mails[1].body.contains("\"ace-alt\""));
    // Each link is for its own account, and the two are not joined by the address.
    let first = secret_in(&mails[0], "reconnect/");
    let now = now_ms();
    let (_, account) = rig
        .state
        .accounts
        .peek_reconnect(&first, now)
        .unwrap()
        .unwrap();
    assert_eq!(account.id, ace.id);
    // By username, only that one account is sent to.
    let bee = rig.with_password("bee").await;
    rig.confirmed(&bee, "shared@example.org");
    assert_eq!(forgot(&rig, "bee").await, StatusCode::NO_CONTENT);
    assert_eq!(rig.mails().len(), 3);
}

#[tokio::test]
async fn with_passwords_off_no_link_is_sent() {
    let rig = rig();
    let ace = rig.with_password("ace").await;
    rig.confirmed(&ace, "ace@example.org");
    rig.state.book.set_password_sign_in(false).unwrap();
    assert_eq!(forgot(&rig, "ace").await, StatusCode::FORBIDDEN);
    assert!(rig.mails().is_empty());
}

#[tokio::test]
async fn asking_from_another_website_is_refused() {
    let rig = rig();
    let ace = rig.with_password("ace").await;
    rig.confirmed(&ace, "ace@example.org");
    let other = [("origin", "https://evil.example")];
    let body = Some(json!({"who": "ace"}));
    let ((status, _), _) = call_with(
        &rig,
        None,
        Method::POST,
        "/api/password/forgot",
        body,
        &other,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(rig.mails().is_empty());
}

// ── A real connection to an email server ────────────────────────────────────

/// An email server that takes one mail and says what it was given: who from, who to, and
/// the mail itself.
async fn smtp_server() -> (u16, tokio::task::JoinHandle<(String, String, String)>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        write.write_all(b"220 test ESMTP\r\n").await.unwrap();
        let (mut from, mut to, mut data) = (String::new(), String::new(), String::new());
        while let Some(line) = lines.next_line().await.unwrap() {
            let upper = line.to_ascii_uppercase();
            if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                write.write_all(b"250 test\r\n").await.unwrap();
            } else if upper.starts_with("MAIL FROM") {
                from = line;
                write.write_all(b"250 OK\r\n").await.unwrap();
            } else if upper.starts_with("RCPT TO") {
                to = line;
                write.write_all(b"250 OK\r\n").await.unwrap();
            } else if upper == "DATA" {
                write.write_all(b"354 go on\r\n").await.unwrap();
                while let Some(line) = lines.next_line().await.unwrap() {
                    if line == "." {
                        break;
                    }
                    data.push_str(&line);
                    data.push('\n');
                }
                write.write_all(b"250 taken\r\n").await.unwrap();
            } else if upper == "QUIT" {
                write.write_all(b"221 bye\r\n").await.unwrap();
                break;
            } else {
                write.write_all(b"250 OK\r\n").await.unwrap();
            }
        }
        (from, to, data)
    });
    (port, task)
}

fn local_settings(port: u16) -> MailSettings {
    MailSettings {
        host: "127.0.0.1".into(),
        port,
        user: None,
        password: None,
        from: "Grid Test <grid@example.org>".into(),
        tls: MailTls::None,
    }
}

#[tokio::test]
async fn a_mail_really_goes_to_an_email_server_as_it_was_written() {
    let (port, server) = smtp_server().await;
    let mailer = Mailer::smtp(&local_settings(port)).unwrap();
    let link = "https://grid.example.org/#/reconnect/gfr_abcDEF-123_xyz";
    let mail = Mail {
        to: "pia@example.org".into(),
        subject: "Set a new password for grid.example.org".into(),
        body: format!("Open this link:\n\n{link}\n\nIt works once.\n"),
    };
    mailer.send(mail).await.unwrap();
    drop(mailer);
    let (from, to, data) = server.await.unwrap();
    assert!(from.contains("<grid@example.org>"), "{from}");
    assert!(to.contains("<pia@example.org>"), "{to}");
    assert!(
        data.contains("Subject: Set a new password for grid.example.org"),
        "{data}"
    );
    assert!(
        // The name the mail is from is in quotes on the wire.
        data.contains("From: \"Grid Test\" <grid@example.org>"),
        "{data}"
    );
    assert!(data.contains("To: pia@example.org"), "{data}");
    // The link arrives whole, on one line, as it was written.
    assert!(data.lines().any(|line| line == link), "{data}");
}

#[tokio::test]
async fn an_email_server_that_is_not_there_is_an_error_in_words_and_not_a_hang() {
    // A port nothing listens on.
    let port = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let mailer = Mailer::smtp(&local_settings(port)).unwrap();
    let mail = Mail {
        to: "pia@example.org".into(),
        subject: "x".into(),
        body: "x".into(),
    };
    let started = std::time::Instant::now();
    let why = mailer.send(mail).await.unwrap_err();
    assert!(why.contains("did not take the mail"), "{why}");
    assert!(started.elapsed() < std::time::Duration::from_secs(25));
}

#[tokio::test]
async fn the_whole_way_a_forgotten_password_link_goes_through_a_real_email_server() {
    let (port, server) = smtp_server().await;
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path())
        .unwrap()
        .at(Some("https://grid.example.org".into()))
        .with_mailer(Some(Mailer::smtp(&local_settings(port)).unwrap()));
    state.book.set_password_sign_in(true).unwrap();
    state.book.set_open_sign_up(true).unwrap();
    let rig = Rig {
        app: router(state.clone()),
        state,
        _dir: dir,
    };
    let up = json!({"username": "pia", "password": PW});
    let (status, me) = call(&rig, None, Method::POST, "/api/sign-up/password", Some(up)).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    let pia = AccountId(me["account"]["id"].as_str().unwrap().to_owned());
    let accounts = &rig.state.accounts;
    let secret = accounts
        .set_email(&pia, "pia@example.org", now_ms())
        .unwrap();
    accounts.verify_email(&secret, now_ms()).unwrap().unwrap();

    assert_eq!(forgot(&rig, "pia").await, StatusCode::NO_CONTENT);
    let (_, to, data) = server.await.unwrap();
    assert!(to.contains("<pia@example.org>"), "{to}");
    // The link in the mail the server got is one that works.
    let line = data
        .lines()
        .find(|line| line.starts_with("https://grid.example.org/#/reconnect/gfr_"))
        .unwrap_or_else(|| panic!("no link in {data}"));
    let secret = line.rsplit('/').next().unwrap();
    let body = json!({"username": "pia", "password": PW2, "invite": secret});
    let (status, _) = call(
        &rig,
        None,
        Method::POST,
        "/api/sign-up/password",
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sign_in(&rig, "pia", PW2).await, StatusCode::OK);
}
