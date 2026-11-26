//! Accounts over HTTP: who may ask for what, the session cookie, and requests another site
//! makes (docs/primitives.md §13.2, §13.7, §13.8).

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

// ── Who am I ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_browser_that_is_not_signed_in_is_told_so_and_nothing_else() {
    let rig = rig();
    let (status, me) = call(&rig, None, Method::GET, "/api/me", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me, json!({}));
    // A cookie that is no session is the same as none.
    for junk in ["", "nonsense", "gfi_looks-like-an-invite"] {
        let ((status, me), _) =
            call_with(&rig, Some(junk), Method::GET, "/api/me", None, &[]).await;
        assert_eq!((status, me), (StatusCode::OK, json!({})), "{junk:?}");
    }
}

#[tokio::test]
async fn a_signed_in_browser_is_told_who_it_is_and_what_it_may_do() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Director);
    // Ace also flies with the other club.
    let (_, link) = rig
        .state
        .accounts
        .new_invite(
            NewInvite {
                kind: InviteKind::Personal {
                    club: club("south"),
                    role: Role::Pilot,
                    pilot: None,
                },
                label: "Ace".into(),
                max_uses: 1,
                expires_at: None,
            },
            None,
            now_ms(),
        )
        .unwrap();
    let (status, me) = call(
        &rig,
        Some(&ace),
        Method::POST,
        &format!("/api/invites/{link}/accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        me["account"]["clubs"],
        json!([
            {"club": {"id": "north", "name": "North FPV"}, "role": "Director"},
            {"club": {"id": "south", "name": "South FPV"}, "role": "Pilot"}
        ])
    );
    let (_, me) = call(&rig, Some(&ace), Method::GET, "/api/me", None).await;
    assert_eq!(me["account"]["id"], ace.id.0);
    assert_eq!(me["account"]["name"], "Ace");
    assert_eq!(me["account"]["owner"], false);
    assert_eq!(
        me["account"]["sign_ins"],
        json!([{"provider": "discord", "subject": "subject-of-Ace", "label": "Ace"}])
    );
    // The session's secret is never in an answer.
    assert!(!me.to_string().contains(&ace.session));
}

#[tokio::test]
async fn a_name_is_changed_by_its_own_account_only() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let (status, me) = call(
        &rig,
        Some(&ace),
        Method::PATCH,
        "/api/me",
        Some(json!({"name": " Ace of Spades "})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["account"]["name"], "Ace of Spades");
    for bad in ["", "   ", "two\nlines"] {
        let (status, body) = call(
            &rig,
            Some(&ace),
            Method::PATCH,
            "/api/me",
            Some(json!({"name": bad})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}");
        assert_eq!(body["code"], "BadRequest");
    }
    let (status, _) = call(
        &rig,
        None,
        Method::PATCH,
        "/api/me",
        Some(json!({"name": "X"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_last_sign_in_cannot_be_taken_off() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let path = "/api/me/sign-ins/discord/subject-of-Ace";
    let (status, body) = call(&rig, Some(&ace), Method::DELETE, path, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["message"].as_str().unwrap().contains("at least one"));
    rig.state
        .accounts
        .add_sign_in(
            &ace.id,
            &NewSignIn {
                provider: "google",
                subject: "g-1",
                label: "ace@x",
            },
            now_ms(),
        )
        .unwrap();
    let (status, me) = call(&rig, Some(&ace), Method::DELETE, path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["account"]["sign_ins"].as_array().unwrap().len(), 1);
    assert_eq!(me["account"]["sign_ins"][0]["provider"], "google");
    // Another account's sign-in is not this one's to take off.
    let bee = rig.member("Bee", "north", Role::Pilot);
    let (status, _) = call(
        &rig,
        Some(&ace),
        Method::DELETE,
        "/api/me/sign-ins/discord/subject-of-Bee",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.state.accounts.sign_ins(&bee.id).unwrap().len(), 1);
}

// ── Signing out ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn signing_out_ends_the_session_and_clears_the_cookie() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let other_device = rig.state.accounts.new_session(&ace.id, now_ms()).unwrap();
    let ((status, me), cookies) = call_with(
        &rig,
        Some(&ace.session),
        Method::POST,
        "/api/sign-out",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me, json!({}));
    assert_eq!(cookies.len(), 1);
    assert!(
        cookies[0].starts_with(&format!("{SESSION_COOKIE}=;")),
        "{cookies:?}"
    );
    assert!(cookies[0].contains("Max-Age=0"));

    let (_, me) = call(&rig, Some(&ace), Method::GET, "/api/me", None).await;
    assert_eq!(me, json!({}), "the old cookie is no longer a session");
    // Only this browser was signed out.
    let ((_, me), _) =
        call_with(&rig, Some(&other_device), Method::GET, "/api/me", None, &[]).await;
    assert_eq!(me["account"]["name"], "Ace");
    // Signing out when not signed in is fine.
    let (status, _) = call(&rig, None, Method::POST, "/api/sign-out", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_cookie_is_hidden_from_scripts_and_marked_secure_over_https() {
    let plain = rig();
    let cookie = gridfpv_serverd::people::session_cookie(&plain.state, &Default::default(), "abc");
    let cookie = cookie.to_str().unwrap();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=abc;")));
    for part in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age=2592000"] {
        assert!(cookie.contains(part), "{cookie} lacks {part}");
    }
    assert!(
        !cookie.contains("Secure"),
        "no HTTPS, so not marked for HTTPS only"
    );

    // Set to be reached over HTTPS.
    let https = rig_at(Some("https://grid.example.org/"));
    let cookie = gridfpv_serverd::people::session_cookie(&https.state, &Default::default(), "abc");
    assert!(cookie.to_str().unwrap().contains("; Secure"));
    // Or told so by the web server in front.
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-forwarded-proto", "https".parse().unwrap());
    let cookie = gridfpv_serverd::people::session_cookie(&plain.state, &headers, "abc");
    assert!(cookie.to_str().unwrap().contains("; Secure"));
    // Set to plain http wins over what a header claims.
    let http = rig_at(Some("http://localhost:8470"));
    let cookie = gridfpv_serverd::people::session_cookie(&http.state, &headers, "abc");
    assert!(!cookie.to_str().unwrap().contains("Secure"));
}

// ── Invites ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn anyone_can_see_what_a_link_is_for_and_looking_does_not_use_it() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let (status, made) = call(
        &rig,
        Some(&boss),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({
            "kind": "Group", "label": "Club Discord", "max_uses": 20, "days": 7
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    let secret = made["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("gfi_"));
    assert_eq!(made["invite"]["kind"], "Group");
    assert_eq!(made["invite"]["role"], "Pilot");
    assert_eq!(
        made["invite"]["club"],
        json!({"id": "north", "name": "North FPV"})
    );
    assert_eq!(made["invite"]["works"], true);

    for _ in 0..3 {
        let (status, seen) = call(
            &rig,
            None,
            Method::GET,
            &format!("/api/invites/{secret}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seen["uses"], 0);
        assert_eq!(seen["label"], "Club Discord");
        assert!(
            !seen.to_string().contains(&secret),
            "the link is not echoed back"
        );
    }
    for wrong in ["gfi_nope", "nope", made["invite"]["id"].as_str().unwrap()] {
        let (status, body) = call(
            &rig,
            None,
            Method::GET,
            &format!("/api/invites/{wrong}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{wrong}");
        assert_eq!(body["code"], "UnknownScope");
    }
}

#[tokio::test]
async fn the_link_of_an_invite_is_given_once_and_never_listed() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let (_, made) = call(
        &rig,
        Some(&boss),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({
            "kind": "Personal", "label": "Ace", "role": "Approver"
        })),
    )
    .await;
    let secret = made["secret"].as_str().unwrap();
    let (status, list) = call(
        &rig,
        Some(&boss),
        Method::GET,
        "/api/clubs/north/invites",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let text = list.to_string();
    assert!(
        !text.contains(secret) && !text.contains(&secret[4..]),
        "{text}"
    );
    // Both are there: the one Boss joined with, and the new one.
    assert_eq!(list.as_array().unwrap().len(), 2);
    let new = list
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "Ace")
        .unwrap();
    assert_eq!(
        (
            new["kind"].as_str(),
            new["role"].as_str(),
            new["max_uses"].as_u64()
        ),
        (Some("Personal"), Some("Approver"), Some(1))
    );
}

#[tokio::test]
async fn only_a_director_of_that_club_or_an_owner_makes_sees_and_takes_back_its_invites() {
    let rig = rig();
    let north_boss = rig.member("North boss", "north", Role::Director);
    let north_approver = rig.member("Approver", "north", Role::Approver);
    let north_pilot = rig.member("Pilot", "north", Role::Pilot);
    let south_boss = rig.member("South boss", "south", Role::Director);
    let owner = rig.owner("Owner");
    let body = json!({"kind": "Personal", "label": "Someone"});

    let (_, made) = call(
        &rig,
        Some(&north_boss),
        Method::POST,
        "/api/clubs/north/invites",
        Some(body.clone()),
    )
    .await;
    let id = made["invite"]["id"].as_str().unwrap().to_owned();

    for (who, name) in [
        (&north_approver, "approver"),
        (&north_pilot, "pilot"),
        (&south_boss, "another club's director"),
    ] {
        for (method, path, send) in [
            (
                Method::POST,
                "/api/clubs/north/invites".to_owned(),
                Some(body.clone()),
            ),
            (Method::GET, "/api/clubs/north/invites".to_owned(), None),
            (
                Method::DELETE,
                format!("/api/clubs/north/invites/{id}"),
                None,
            ),
        ] {
            let (status, answer) = call(&rig, Some(who), method.clone(), &path, send).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {method} {path}");
            assert_eq!(answer["code"], "Forbidden");
        }
    }
    // Not signed in at all.
    let (status, _) = call(
        &rig,
        None,
        Method::POST,
        "/api/clubs/north/invites",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&rig, None, Method::GET, "/api/clubs/north/invites", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Nothing was taken back by those who may not.
    assert!(
        rig.state
            .accounts
            .invites_of(&club("north"))
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .unwrap()
            .revoked_at
            .is_none()
    );

    // An owner may, in any club.
    let (status, _) = call(
        &rig,
        Some(&owner),
        Method::POST,
        "/api/clubs/south/invites",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &rig,
        Some(&owner),
        Method::DELETE,
        &format!("/api/clubs/north/invites/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &rig,
        Some(&owner),
        Method::DELETE,
        &format!("/api/clubs/north/invites/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "already taken back");
}

#[tokio::test]
async fn a_director_cannot_take_back_another_clubs_invite_through_their_own_club() {
    let rig = rig();
    let north_boss = rig.member("North boss", "north", Role::Director);
    let south_boss = rig.member("South boss", "south", Role::Director);
    let (_, made) = call(
        &rig,
        Some(&south_boss),
        Method::POST,
        "/api/clubs/south/invites",
        Some(json!({"kind": "Personal", "label": "X"})),
    )
    .await;
    let id = made["invite"]["id"].as_str().unwrap();
    // The id is real, but it is not one of north's.
    let (status, _) = call(
        &rig,
        Some(&north_boss),
        Method::DELETE,
        &format!("/api/clubs/north/invites/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        rig.state
            .accounts
            .peek_invite(made["secret"].as_str().unwrap(), now_ms())
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn an_invite_that_is_asked_for_wrongly_is_refused_and_not_made() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let before = rig.state.accounts.all_invites().unwrap().len();
    for (bad, word) in [
        (json!({"kind": "Owner", "label": "Sneaky"}), "owner"),
        (
            json!({"kind": "Group", "label": "No limit", "days": 7}),
            "1 to 500",
        ),
        (
            json!({"kind": "Group", "label": "Too many", "max_uses": 501, "days": 7}),
            "1 to 500",
        ),
        (
            json!({"kind": "Group", "label": "No end", "max_uses": 10}),
            "1 to 90",
        ),
        (
            json!({"kind": "Group", "label": "Too long", "max_uses": 10, "days": 91}),
            "1 to 90",
        ),
        (
            json!({"kind": "Group", "label": "Directors for all", "max_uses": 10, "days": 7, "role": "Director"}),
            "pilot",
        ),
        (json!({"kind": "Personal", "label": "   "}), "label"),
    ] {
        let (status, body) = call(
            &rig,
            Some(&boss),
            Method::POST,
            "/api/clubs/north/invites",
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains(word),
            "{bad}: {body}"
        );
    }
    assert_eq!(rig.state.accounts.all_invites().unwrap().len(), before);
    // A club that is not there. A director of north is not told whether it exists.
    let (status, _) = call(
        &rig,
        Some(&boss),
        Method::POST,
        "/api/clubs/nowhere/invites",
        Some(json!({"kind": "Personal", "label": "X"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let owner = rig.owner("Owner");
    let (status, _) = call(
        &rig,
        Some(&owner),
        Method::POST,
        "/api/clubs/nowhere/invites",
        Some(json!({"kind": "Personal", "label": "X"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn using_a_link_needs_an_account_and_a_link_that_works() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let ace = rig.member("Ace", "south", Role::Pilot);
    let (_, made) = call(
        &rig,
        Some(&boss),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({"kind": "Personal", "label": "Ace", "role": "Director"})),
    )
    .await;
    let secret = made["secret"].as_str().unwrap();
    let path = format!("/api/invites/{secret}/accept");

    let (status, _) = call(&rig, None, Method::POST, &path, None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "there is no account to give it to"
    );
    assert_eq!(
        rig.state
            .accounts
            .peek_invite(secret, now_ms())
            .unwrap()
            .unwrap()
            .uses,
        0
    );

    let (status, me) = call(&rig, Some(&ace), Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    let clubs = me["account"]["clubs"].as_array().unwrap();
    assert!(
        clubs
            .iter()
            .any(|c| c["club"]["id"] == "north" && c["role"] == "Director")
    );
    // It worked once.
    let (status, _) = call(&rig, Some(&boss), Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── Members and roles ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_director_sees_and_changes_the_members_of_their_own_club_only() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let south_boss = rig.member("South boss", "south", Role::Director);
    let south_pilot = rig.member("Southerner", "south", Role::Pilot);

    let (status, list) = call(
        &rig,
        Some(&boss),
        Method::GET,
        "/api/clubs/north/members",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list,
        json!([
            {"id": ace.id.0, "name": "Ace", "role": "Pilot"},
            {"id": boss.id.0, "name": "Boss", "role": "Director"}
        ])
    );

    let path = format!("/api/clubs/north/members/{}", ace.id.0);
    let (status, _) = call(
        &rig,
        Some(&boss),
        Method::PUT,
        &path,
        Some(json!({"role": "Approver"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        rig.state
            .accounts
            .standing(&ace.id)
            .unwrap()
            .unwrap()
            .role_in(&club("north")),
        Some(Role::Approver)
    );

    // Not in the other club: neither to look, nor to change, nor to remove.
    let theirs = format!("/api/clubs/south/members/{}", south_pilot.id.0);
    for (method, path, body) in [
        (Method::GET, "/api/clubs/south/members".to_owned(), None),
        (
            Method::PUT,
            theirs.clone(),
            Some(json!({"role": "Director"})),
        ),
        (Method::DELETE, theirs.clone(), None),
    ] {
        let (status, _) = call(&rig, Some(&boss), method.clone(), &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    assert_eq!(
        rig.state
            .accounts
            .standing(&south_pilot.id)
            .unwrap()
            .unwrap()
            .role_in(&club("south")),
        Some(Role::Pilot)
    );

    // A member who is not a director cannot, even on themselves.
    let (status, _) = call(
        &rig,
        Some(&ace),
        Method::PUT,
        &path,
        Some(json!({"role": "Director"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &rig,
        Some(&ace),
        Method::GET,
        "/api/clubs/north/members",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&rig, None, Method::GET, "/api/clubs/north/members", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = south_boss;
}

#[tokio::test]
async fn a_member_of_another_club_cannot_be_given_a_role_here_by_their_id() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let southerner = rig.member("Southerner", "south", Role::Pilot);
    // Boss knows the id, but the person is not a member of north. A role is not handed to
    // someone who never joined: they join by invite.
    let path = format!("/api/clubs/north/members/{}", southerner.id.0);
    let (status, _) = call(
        &rig,
        Some(&boss),
        Method::PUT,
        &path,
        Some(json!({"role": "Director"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        rig.state
            .accounts
            .standing(&southerner.id)
            .unwrap()
            .unwrap()
            .role_in(&club("north")),
        None
    );
    let (status, _) = call(
        &rig,
        Some(&boss),
        Method::PUT,
        "/api/clubs/north/members/acct_nobody",
        Some(json!({"role": "Pilot"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_last_director_cannot_be_demoted_or_removed_even_by_an_owner() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let owner = rig.owner("Owner");
    let path = format!("/api/clubs/north/members/{}", boss.id.0);
    for who in [&boss, &owner] {
        let (status, body) = call(
            &rig,
            Some(who),
            Method::PUT,
            &path,
            Some(json!({"role": "Pilot"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("at least one director")
        );
        let (status, _) = call(&rig, Some(who), Method::DELETE, &path, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    // With a second director, the first may be removed, and loses the right at once.
    let second = rig.member("Second", "north", Role::Director);
    let (status, _) = call(&rig, Some(&second), Method::DELETE, &path, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &rig,
        Some(&boss),
        Method::GET,
        "/api/clubs/north/members",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Their account is still there.
    let (_, me) = call(&rig, Some(&boss), Method::GET, "/api/me", None).await;
    assert_eq!(me["account"]["name"], "Boss");
    assert_eq!(me["account"]["clubs"], json!([]));
}

#[tokio::test]
async fn a_role_that_is_not_a_role_is_refused() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let path = format!("/api/clubs/north/members/{}", ace.id.0);
    for bad in [
        json!({"role": "Owner"}),
        json!({"role": "director"}),
        json!({}),
        json!({"role": 3}),
    ] {
        let (status, _) = call(&rig, Some(&boss), Method::PUT, &path, Some(bad.clone())).await;
        assert!(status.is_client_error(), "{bad}: {status}");
    }
    assert_eq!(
        rig.state
            .accounts
            .standing(&ace.id)
            .unwrap()
            .unwrap()
            .role_in(&club("north")),
        Some(Role::Pilot)
    );
}

// ── A request another site makes ────────────────────────────────────────────

#[tokio::test]
async fn a_change_asked_for_by_another_website_is_refused_whoever_is_signed_in() {
    let rig = rig();
    let boss = rig.member("Boss", "north", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let path = format!("/api/clubs/north/members/{}", ace.id.0);
    for origin in [
        "https://evil.example",
        "https://grid.example.org.evil.example",
        "http://localhost:3000",
        "null",
    ] {
        let ((status, body), _) = call_with(
            &rig,
            Some(&boss.session),
            Method::PUT,
            &path,
            Some(json!({"role": "Director"})),
            &[("origin", origin)],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}");
        assert_eq!(body["code"], "Forbidden");
        let ((status, _), _) = call_with(
            &rig,
            Some(&boss.session),
            Method::POST,
            "/api/sign-out",
            None,
            &[("origin", origin)],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}: even signing out");
    }
    assert_eq!(
        rig.state
            .accounts
            .standing(&ace.id)
            .unwrap()
            .unwrap()
            .role_in(&club("north")),
        Some(Role::Pilot)
    );

    // The server's own page may, whichever scheme the proxy in front spoke.
    for origin in [
        "https://grid.example.org",
        "http://grid.example.org",
        "https://GRID.example.org",
    ] {
        let ((status, _), _) = call_with(
            &rig,
            Some(&boss.session),
            Method::PUT,
            &path,
            Some(json!({"role": "Pilot"})),
            &[("origin", origin)],
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{origin}");
    }
    // Reading is not a change: another site can ask, and the browser will not let it read.
    let ((status, _), _) = call_with(
        &rig,
        Some(&boss.session),
        Method::GET,
        "/api/me",
        None,
        &[("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_address_the_server_is_set_to_is_its_own_site_too() {
    // Behind a proxy that passes another Host on, the set address is what counts.
    let rig = rig_at(Some("https://races.example.net"));
    let boss = rig.member("Boss", "north", Role::Director);
    let ((status, _), _) = call_with(
        &rig,
        Some(&boss.session),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({"kind": "Personal", "label": "X"})),
        &[("origin", "https://races.example.net")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ((status, _), _) = call_with(
        &rig,
        Some(&boss.session),
        Method::POST,
        "/api/clubs/north/invites",
        Some(json!({"kind": "Personal", "label": "X"})),
        &[("origin", "https://races.example.net.evil.example")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_director_pushing_with_a_link_token_is_not_stopped_by_the_site_check() {
    // The push routes are a program's, not a page's: no cookie, no Origin.
    let rig = rig();
    let token = rig
        .state
        .book
        .new_token(&club("north"), "laptop")
        .unwrap()
        .1;
    let request = Request::builder()
        .method(Method::PUT)
        .uri("/push/events/e")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"meta":{"id":"e","name":"E","created_at":1,"persistent":true}}"#,
        ))
        .unwrap();
    assert_eq!(
        rig.app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_session_is_not_a_link_token_and_a_link_token_is_not_a_session() {
    let rig = rig();
    let owner = rig.owner("Owner");
    let token = rig
        .state
        .book
        .new_token(&club("north"), "laptop")
        .unwrap()
        .1;
    // The owner's session does not push.
    let request = Request::builder()
        .uri("/push/hello")
        .header("authorization", format!("Bearer {}", owner.session))
        .header(
            header::COOKIE,
            format!("{SESSION_COOKIE}={}", owner.session),
        )
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        rig.app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    // A link token signs nobody in.
    let ((_, me), _) = call_with(&rig, Some(&token), Method::GET, "/api/me", None, &[]).await;
    assert_eq!(me, json!({}));
}
