//! Getting into a club (docs/primitives.md §13.7): public and private clubs, asking to
//! join, a director saying yes or no, and a director adding someone.
//!
//! Each test drives the server's own router with a session cookie, as a browser would.

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

impl Rig {
    /// Someone who signed up with no invite: an account in no club.
    fn newcomer(&self, name: &str) -> Person {
        let subject = format!("subject-of-{name}");
        let sign_in = NewSignIn {
            provider: "discord",
            subject: &subject,
            label: name,
        };
        let (account, session) = self
            .state
            .accounts
            .join_open(&sign_in, name, now_ms())
            .unwrap();
        Person {
            id: account.id,
            session,
        }
    }

    fn open(&self, club_id: &str) {
        self.state.book.set_club_open(&club(club_id), true).unwrap();
    }

    fn role(&self, who: &Person, club_id: &str) -> Option<Role> {
        self.state
            .accounts
            .standing(&who.id)
            .unwrap()
            .unwrap()
            .role_in(&club(club_id))
    }

    fn asked(&self, club_id: &str) -> Vec<String> {
        self.state
            .accounts
            .join_asks(&club(club_id))
            .unwrap()
            .into_iter()
            .map(|(account, _)| account.name)
            .collect()
    }
}

async fn join(rig: &Rig, who: Option<&Person>, club_id: &str) -> (StatusCode, Value) {
    let path = format!("/api/clubs/{club_id}/join");
    call(rig, who, Method::POST, &path, None).await
}

async fn unjoin(rig: &Rig, who: Option<&Person>, club_id: &str) -> (StatusCode, Value) {
    let path = format!("/api/clubs/{club_id}/join");
    call(rig, who, Method::DELETE, &path, None).await
}

/// The ids of the clubs an answer says the account is in, and those it asked to join.
fn clubs_of(me: &Value) -> (Vec<String>, Vec<String>) {
    let text = |value: &Value| value.as_str().unwrap().to_owned();
    (
        me["account"]["clubs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| text(&entry["club"]["id"]))
            .collect(),
        me["account"]["join_asks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| text(&entry["id"]))
            .collect(),
    )
}

fn north() -> Vec<String> {
    vec!["north".to_owned()]
}

fn none() -> Vec<String> {
    Vec::new()
}

// ── Public or private ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_club_starts_private_and_those_who_run_it_make_it_public() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let dee = rig.member("Dee", "north", Role::Director);
    let sam = rig.member("Sam", "south", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let app = rig.member("App", "north", Role::Approver);
    let path = "/api/clubs/north/open";

    let (_, clubs) = call(&rig, None, Method::GET, "/api/clubs", None).await;
    assert_eq!(clubs[0]["open"], false);
    assert_eq!(clubs[1]["open"], false);

    let change = |open: bool| Some(json!({"open": open}));
    let (status, _) = call(&rig, None, Method::PUT, path, change(true)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A pilot, an approver, and the director of another club have no say.
    for who in [&ace, &app, &sam] {
        let (status, _) = call(&rig, Some(who), Method::PUT, path, change(true)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert!(!rig.state.book.club(&club("north")).unwrap().open);

    let (status, changed) = call(&rig, Some(&dee), Method::PUT, path, change(true)).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["open"], true);
    assert_eq!(changed["name"], "North FPV");
    // Anyone can see which it is, and the other club is as it was.
    let (_, clubs) = call(&rig, None, Method::GET, "/api/clubs", None).await;
    assert_eq!(clubs[0]["open"], true);
    assert_eq!(clubs[1]["open"], false);
    let (_, one) = call(&rig, None, Method::GET, "/api/clubs/north", None).await;
    assert_eq!(one["open"], true);
    // Kept: a server started again on the same data still has it public.
    let again = AppState::open(rig._dir.path()).unwrap();
    assert!(again.book.club(&club("north")).unwrap().open);

    // An owner may, in any club. And back again.
    let (status, changed) = call(&rig, Some(&owner), Method::PUT, path, change(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["open"], false);
    let nowhere = "/api/clubs/nowhere/open";
    let (status, _) = call(&rig, Some(&owner), Method::PUT, nowhere, change(true)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── A public club ───────────────────────────────────────────────────────────

#[tokio::test]
async fn anyone_with_an_account_joins_a_public_club_as_a_pilot() {
    let rig = rig();
    rig.open("north");
    let pia = rig.newcomer("Pia");

    let (status, _) = join(&rig, None, "north").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, me) = join(&rig, Some(&pia), "north").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    // The answer is the account as it now stands: in the club, with nothing asked.
    assert_eq!(
        me["account"]["clubs"],
        json!([{"club": {"id": "north", "name": "North FPV"}, "role": "Pilot"}])
    );
    assert_eq!(me["account"]["join_asks"], json!([]));
    assert_eq!(me["account"]["primary"], "north");
    assert_eq!(rig.role(&pia, "north"), Some(Role::Pilot));
    assert!(rig.asked("north").is_empty());

    // Joining again changes nothing.
    let (status, me) = join(&rig, Some(&pia), "north").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clubs_of(&me), (north(), none()));
    assert_eq!(rig.state.accounts.members(&club("north")).unwrap().len(), 1);

    // She is a pilot and no more: she cannot see the members.
    let members = "/api/clubs/north/members";
    let (status, _) = call(&rig, Some(&pia), Method::GET, members, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A club that is not there.
    let (status, _) = join(&rig, Some(&pia), "nowhere").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn joining_never_takes_a_role_away() {
    let rig = rig();
    rig.open("north");
    let dee = rig.member("Dee", "north", Role::Director);
    let app = rig.member("App", "north", Role::Approver);
    for (who, role) in [(&dee, Role::Director), (&app, Role::Approver)] {
        let (status, _) = join(&rig, Some(who), "north").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rig.role(who, "north"), Some(role));
    }
}

// ── A private club ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_private_club_is_asked_and_a_director_says_yes_or_no() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let sam = rig.member("Sam", "south", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let pia = rig.newcomer("Pia");
    let quin = rig.newcomer("Quin");
    let callsign = serde_json::from_value(json!({"callsign": "PiaFPV"})).unwrap();
    rig.state.accounts.set_profile(&pia.id, callsign).unwrap();

    // Asking puts nobody in the club.
    let (status, me) = join(&rig, Some(&pia), "north").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(clubs_of(&me), (none(), north()));
    assert_eq!(
        me["account"]["join_asks"],
        json!([{"id": "north", "name": "North FPV"}])
    );
    assert_eq!(rig.role(&pia, "north"), None);
    // Asking twice is asking once.
    join(&rig, Some(&pia), "north").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    join(&rig, Some(&quin), "north").await;
    assert_eq!(rig.asked("north"), ["Pia", "Quin"]);
    // What she asked shows when she looks later, and gives her nothing meanwhile.
    let (_, me) = call(&rig, Some(&pia), Method::GET, "/api/me", None).await;
    assert_eq!(clubs_of(&me), (none(), north()));
    let pilots = "/api/clubs/north/pilots";
    let (status, _) = call(&rig, Some(&pia), Method::GET, pilots, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Only who runs the club sees who asked.
    let asks = "/api/clubs/north/join-asks";
    let (status, _) = call(&rig, None, Method::GET, asks, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for who in [&sam, &ace, &pia] {
        let (status, _) = call(&rig, Some(who), Method::GET, asks, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (status, list) = call(&rig, Some(&dee), Method::GET, asks, None).await;
    assert_eq!(status, StatusCode::OK);
    // The one who asked first is at the top. A callsign shows when there is one.
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["id"], pia.id.0);
    assert_eq!(list[0]["name"], "Pia");
    assert_eq!(list[0]["callsign"], "PiaFPV");
    assert!(list[0]["asked_at"].as_i64().unwrap() > 0);
    assert_eq!(list[1]["name"], "Quin");
    assert!(list[1].get("callsign").is_none());
    // The other club was asked nothing.
    let souths = "/api/clubs/south/join-asks";
    let (_, souths) = call(&rig, Some(&sam), Method::GET, souths, None).await;
    assert_eq!(souths, json!([]));

    // Nobody but who runs the club answers.
    let pias = format!("{asks}/{}", pia.id.0);
    for who in [&sam, &ace, &pia] {
        for method in [Method::PUT, Method::DELETE] {
            let (status, _) = call(&rig, Some(who), method, &pias, None).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
        }
    }
    assert_eq!(rig.asked("north"), ["Pia", "Quin"]);

    // Yes: she is in the club as a pilot, and the ask is done with.
    let (status, _) = call(&rig, Some(&dee), Method::PUT, &pias, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(rig.role(&pia, "north"), Some(Role::Pilot));
    assert_eq!(rig.asked("north"), ["Quin"]);
    let (_, me) = call(&rig, Some(&pia), Method::GET, "/api/me", None).await;
    assert_eq!(clubs_of(&me), (north(), none()));
    // Saying yes again is saying it to nobody.
    let (status, _) = call(&rig, Some(&dee), Method::PUT, &pias, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No: he is not in the club, and the ask is done with.
    let quins = format!("{asks}/{}", quin.id.0);
    let (status, _) = call(&rig, Some(&dee), Method::DELETE, &quins, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(rig.role(&quin, "north"), None);
    assert!(rig.asked("north").is_empty());
    let (_, me) = call(&rig, Some(&quin), Method::GET, "/api/me", None).await;
    assert_eq!(clubs_of(&me), (none(), none()));
    let (status, _) = call(&rig, Some(&dee), Method::DELETE, &quins, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // He may ask again.
    join(&rig, Some(&quin), "north").await;
    assert_eq!(rig.asked("north"), ["Quin"]);

    // Someone who never asked cannot be let in by a yes: an account's id is not an ask.
    let sams = format!("{asks}/{}", sam.id.0);
    let (status, _) = call(&rig, Some(&dee), Method::PUT, &sams, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(rig.role(&sam, "north"), None);
}

#[tokio::test]
async fn an_ask_to_one_club_cannot_be_answered_through_another() {
    let rig = rig();
    let sam = rig.member("Sam", "south", Role::Director);
    let pia = rig.newcomer("Pia");
    join(&rig, Some(&pia), "north").await;
    // South's director names her under their own club: there is no such ask there.
    let path = format!("/api/clubs/south/join-asks/{}", pia.id.0);
    let (status, _) = call(&rig, Some(&sam), Method::PUT, &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(rig.role(&pia, "south"), None);
    assert_eq!(rig.role(&pia, "north"), None);
    assert_eq!(rig.asked("north"), ["Pia"]);
}

#[tokio::test]
async fn someone_in_the_club_has_nothing_to_ask_for() {
    let rig = rig();
    let ace = rig.member("Ace", "north", Role::Pilot);
    let (status, me) = join(&rig, Some(&ace), "north").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clubs_of(&me), (north(), none()));
    assert!(rig.asked("north").is_empty());
}

#[tokio::test]
async fn an_ask_is_taken_back_and_a_member_leaves() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let pia = rig.newcomer("Pia");

    // Nothing to take back, and no club to leave.
    let (status, _) = unjoin(&rig, Some(&pia), "north").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = unjoin(&rig, None, "north").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    join(&rig, Some(&pia), "north").await;
    let (status, me) = unjoin(&rig, Some(&pia), "north").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(clubs_of(&me), (none(), none()));
    assert!(rig.asked("north").is_empty());

    // In the club, she leaves it. What she held there is let go.
    rig.open("north");
    join(&rig, Some(&pia), "north").await;
    let known = gridfpv_serverd::accounts::ClubPilot {
        pilot: "p-pia".into(),
        callsign: "Pia".into(),
        color: None,
    };
    let accounts = &rig.state.accounts;
    accounts.note_pilots(&club("north"), &[known], 3).unwrap();
    accounts
        .give_pilot(&club("north"), "p-pia", &pia.id, None, 4)
        .unwrap();
    let (status, me) = unjoin(&rig, Some(&pia), "north").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clubs_of(&me), (none(), none()));
    assert_eq!(me["account"]["pilots"], json!([]));
    assert_eq!(rig.role(&pia, "north"), None);
    // Her account is still hers.
    assert_eq!(me["account"]["name"], "Pia");

    // The only director of a club cannot walk out of it.
    let (status, body) = unjoin(&rig, Some(&dee), "north").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["message"].as_str().unwrap().contains("director"));
    assert_eq!(rig.role(&dee, "north"), Some(Role::Director));
}

#[tokio::test]
async fn an_invite_or_the_club_going_public_answers_an_ask() {
    let rig = rig();
    let pia = rig.newcomer("Pia");
    let quin = rig.newcomer("Quin");
    join(&rig, Some(&pia), "north").await;
    join(&rig, Some(&quin), "north").await;

    // Pia is sent an invite and uses it. Her ask is done with.
    let invite = NewInvite {
        kind: InviteKind::Personal {
            club: club("north"),
            role: Role::Approver,
            pilot: None,
        },
        label: "Pia".into(),
        max_uses: 1,
        expires_at: None,
    };
    let (_, link) = rig
        .state
        .accounts
        .new_invite(invite, None, now_ms())
        .unwrap();
    let path = format!("/api/invites/{link}/accept");
    let (status, me) = call(&rig, Some(&pia), Method::POST, &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clubs_of(&me), (north(), none()));
    assert_eq!(rig.role(&pia, "north"), Some(Role::Approver));
    assert_eq!(rig.asked("north"), ["Quin"]);

    // The club goes public. Quin's ask still waits. He presses again, and is in.
    rig.open("north");
    assert_eq!(rig.asked("north"), ["Quin"]);
    let (status, me) = join(&rig, Some(&quin), "north").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clubs_of(&me), (north(), none()));
    assert!(rig.asked("north").is_empty());
}

// ── A director adds someone ─────────────────────────────────────────────────

async fn find(rig: &Rig, who: &Person, club_id: &str, q: &str) -> (StatusCode, Vec<String>) {
    let path = format!("/api/clubs/{club_id}/people?q={q}");
    let (status, list) = call(rig, Some(who), Method::GET, &path, None).await;
    let names = list
        .as_array()
        .map(|list| {
            list.iter()
                .map(|p| p["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    (status, names)
}

async fn found(rig: &Rig, who: &Person, q: &str) -> Vec<String> {
    find(rig, who, "north", q).await.1
}

#[tokio::test]
async fn a_director_finds_people_by_name_or_callsign_and_never_the_whole_list() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let sam = rig.member("Sam", "south", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let pia = rig.newcomer("Pia Parker");
    let _pip = rig.newcomer("pip");
    let quin = rig.newcomer("Quin");
    let _odd = rig.newcomer("100% Rotor_Head");
    let callsign = serde_json::from_value(json!({"callsign": "Piston"})).unwrap();
    rig.state.accounts.set_profile(&quin.id, callsign).unwrap();

    // By name, whatever the case, and by callsign. In name order.
    assert_eq!(found(&rig, &dee, "pi").await, ["Pia Parker", "pip", "Quin"]);
    assert_eq!(found(&rig, &dee, "PARK").await, ["Pia Parker"]);
    assert_eq!(found(&rig, &dee, "piston").await, ["Quin"]);
    // The answer says who it is, with the callsign when there is one. Nothing else.
    let path = "/api/clubs/north/people?q=piston";
    let (_, list) = call(&rig, Some(&dee), Method::GET, path, None).await;
    assert_eq!(
        list,
        json!([{"id": quin.id.0, "name": "Quin", "callsign": "Piston"}])
    );
    // Someone already in the club is not offered. In the other club they are.
    assert_eq!(found(&rig, &dee, "ace").await, none());
    assert_eq!(find(&rig, &sam, "south", "ace").await.1, ["Ace"]);
    assert_eq!(found(&rig, &dee, "dee").await, none());

    // Too little to go on gives nobody: the accounts are not listed whole.
    for little in ["", "p", "%20p%20", "%25", "_"] {
        assert_eq!(
            find(&rig, &dee, "north", little).await,
            (StatusCode::OK, none()),
            "{little:?}"
        );
    }
    let bare = "/api/clubs/north/people";
    let (status, list) = call(&rig, Some(&dee), Method::GET, bare, None).await;
    assert_eq!((status, list), (StatusCode::OK, json!([])));
    // A % or a _ is only itself: it does not stand for any letters.
    assert_eq!(found(&rig, &dee, "%25%25").await, none());
    assert_eq!(found(&rig, &dee, "__").await, none());
    assert_eq!(found(&rig, &dee, "0%25").await, ["100% Rotor_Head"]);
    assert_eq!(found(&rig, &dee, "r_h").await, ["100% Rotor_Head"]);

    // Nobody but who runs the club looks.
    for who in [&ace, &pia, &sam] {
        assert_eq!(
            find(&rig, who, "north", "pi").await.0,
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn a_look_gives_twenty_people_at_most() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    for n in 0..25 {
        rig.newcomer(&format!("Racer {n:02}"));
    }
    let names = found(&rig, &dee, "racer").await;
    assert_eq!(names.len(), 20);
    assert_eq!(names[0], "Racer 00");
    assert_eq!(names[19], "Racer 19");
}

async fn add(rig: &Rig, who: &Person, to: &str, account: &AccountId) -> StatusCode {
    let path = format!("/api/clubs/{to}/members");
    let body = Some(json!({"account": account.0}));
    call(rig, Some(who), Method::POST, &path, body).await.0
}

#[tokio::test]
async fn a_director_adds_someone_to_their_own_club_as_a_pilot() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let sam = rig.member("Sam", "south", Role::Director);
    let ace = rig.member("Ace", "north", Role::Pilot);
    let pia = rig.newcomer("Pia");

    // Not a pilot of the club, not another club's director, and not into another club.
    for (who, to) in [(&ace, "north"), (&sam, "north"), (&dee, "south")] {
        assert_eq!(add(&rig, who, to, &pia.id).await, StatusCode::FORBIDDEN);
    }
    assert_eq!(rig.role(&pia, "north"), None);

    // She had asked. Being added answers it.
    join(&rig, Some(&pia), "north").await;
    assert_eq!(
        add(&rig, &dee, "north", &pia.id).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(rig.role(&pia, "north"), Some(Role::Pilot));
    assert!(rig.asked("north").is_empty());
    let (_, me) = call(&rig, Some(&pia), Method::GET, "/api/me", None).await;
    assert_eq!(clubs_of(&me), (north(), none()));

    // Adding someone who is in the club keeps the role they have. Even oneself.
    for who in [&pia, &dee] {
        assert_eq!(
            add(&rig, &dee, "north", &who.id).await,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(rig.role(&dee, "north"), Some(Role::Director));
    assert_eq!(rig.role(&pia, "north"), Some(Role::Pilot));
    assert_eq!(rig.state.accounts.members(&club("north")).unwrap().len(), 3);

    // An account that is not there.
    let nobody = AccountId("acct_nobody".into());
    assert_eq!(
        add(&rig, &dee, "north", &nobody).await,
        StatusCode::NOT_FOUND
    );
    // A role in one club is not carried into another by being added there.
    assert_eq!(
        add(&rig, &sam, "south", &dee.id).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(rig.role(&dee, "south"), Some(Role::Pilot));
}

// ── An archived club ────────────────────────────────────────────────────────

#[tokio::test]
async fn an_archived_club_is_not_joined_or_asked_and_asks_to_it_are_done_with() {
    let rig = rig();
    let owner = rig.owner("Oz");
    let pia = rig.newcomer("Pia");
    let quin = rig.newcomer("Quin");
    join(&rig, Some(&pia), "north").await;
    assert_eq!(rig.asked("north"), ["Pia"]);

    let archive = Some(json!({"archived": true}));
    let path = "/api/clubs/north/archived";
    let (status, _) = call(&rig, Some(&owner), Method::PUT, path, archive).await;
    assert_eq!(status, StatusCode::OK);
    // Her ask has no answer coming, and she is not shown waiting.
    assert!(rig.asked("north").is_empty());
    let (_, me) = call(&rig, Some(&pia), Method::GET, "/api/me", None).await;
    assert_eq!(clubs_of(&me), (none(), none()));

    // Nobody joins or asks, public or not, and nobody is added.
    for open in [false, true] {
        rig.state.book.set_club_open(&club("north"), open).unwrap();
        let (status, body) = join(&rig, Some(&quin), "north").await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body["message"].as_str().unwrap().contains("archived"));
    }
    assert_eq!(
        add(&rig, &owner, "north", &quin.id).await,
        StatusCode::CONFLICT
    );
    let open = "/api/clubs/north/open";
    let private = Some(json!({"open": false}));
    let (status, _) = call(&rig, Some(&owner), Method::PUT, open, private).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(rig.role(&quin, "north"), None);
    assert!(rig.asked("north").is_empty());
}

#[tokio::test]
async fn asks_are_kept_per_club_and_only_for_accounts_that_are_there() {
    let rig = rig();
    let pia = rig.newcomer("Pia");
    join(&rig, Some(&pia), "north").await;
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    join(&rig, Some(&pia), "south").await;
    assert_eq!(
        rig.state.accounts.my_join_asks(&pia.id).unwrap(),
        [club("north"), club("south")]
    );
    // Taking one back leaves the other.
    unjoin(&rig, Some(&pia), "north").await;
    assert_eq!(
        rig.state.accounts.my_join_asks(&pia.id).unwrap(),
        [club("south")]
    );
    // Asking for an account that is not there keeps nothing.
    let nobody = AccountId("acct_nobody".into());
    let accounts = &rig.state.accounts;
    assert!(accounts.ask_to_join(&club("north"), &nobody, 1).is_err());
    assert!(accounts.add_member(&club("north"), &nobody, 1).is_err());
    assert!(rig.asked("north").is_empty());
}
