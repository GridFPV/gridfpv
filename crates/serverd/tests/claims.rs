//! A club's pilots and the accounts that hold them (docs/primitives.md §13.7): who may
//! ask, who may say yes, and what reaches an account.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use gridfpv_serverd::accounts::{AccountId, ClubPilot, InviteKind, NewInvite, NewSignIn, Role};
use gridfpv_serverd::app::{AppState, router};
use gridfpv_serverd::book::{BOOK_FILE, ClubId};
use gridfpv_serverd::people::{SESSION_COOKIE, now_ms};
use gridfpv_storage::Chain;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Rig {
    _dir: tempfile::TempDir,
    state: AppState,
    app: Router,
    /// A link token for each club: what its Director pushes with.
    north: String,
    south: String,
}

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

/// A server with two clubs. North has the pilots Ace, Bee and Cat.
fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(BOOK_FILE),
        r#"{"clubs":[{"id":"north","name":"North FPV"},{"id":"south","name":"South FPV"}]}"#,
    )
    .unwrap();
    let state = AppState::open(dir.path()).unwrap();
    let north = state.book.new_token(&club("north"), "laptop").unwrap().1;
    let south = state.book.new_token(&club("south"), "laptop").unwrap().1;
    let pilot = |id: &str, callsign: &str| ClubPilot {
        pilot: id.into(),
        callsign: callsign.into(),
        color: None,
    };
    state
        .accounts
        .note_pilots(
            &club("north"),
            &[
                pilot("ace-1", "Ace"),
                pilot("bee-2", "Bee"),
                pilot("cat-3", "Cat"),
            ],
            now_ms(),
        )
        .unwrap();
    Rig {
        app: router(state.clone()),
        state,
        north,
        south,
        _dir: dir,
    }
}

struct Person {
    id: AccountId,
    session: String,
}

impl Rig {
    fn person(&self, name: &str, kind: InviteKind) -> Person {
        let now = now_ms();
        let (_, secret) = self
            .state
            .accounts
            .new_invite(
                NewInvite {
                    kind,
                    label: name.into(),
                    max_uses: 1,
                    expires_at: None,
                },
                None,
                now,
            )
            .unwrap();
        self.join(name, &secret)
    }

    /// Sign in for the first time with an invite link's secret.
    fn join(&self, name: &str, secret: &str) -> Person {
        let subject = format!("subject-of-{name}");
        let sign_in = NewSignIn {
            provider: "discord",
            subject: &subject,
            label: name,
        };
        let (account, _, session) = self
            .state
            .accounts
            .join_with_invite(secret, &sign_in, name, now_ms())
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

enum By<'a> {
    Nobody,
    Person(&'a Person),
    Token(&'a str),
}

async fn send(
    rig: &Rig,
    by: By<'_>,
    method: Method,
    path: &str,
    body: Option<String>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "grid.example.org");
    match by {
        By::Nobody => {}
        By::Person(person) => {
            request = request.header(
                header::COOKIE,
                format!("{SESSION_COOKIE}={}", person.session),
            );
        }
        By::Token(token) => {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
    }
    let request = match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    };
    let response = rig.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn call(
    rig: &Rig,
    who: &Person,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send(
        rig,
        By::Person(who),
        method,
        path,
        body.map(|b| b.to_string()),
    )
    .await
}

async fn push(
    rig: &Rig,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send(
        rig,
        By::Token(token),
        method,
        path,
        body.map(|b| b.to_string()),
    )
    .await
}

/// The club's pilots as `who` is shown them, by callsign.
async fn pilots(rig: &Rig, who: &Person, club: &str) -> Value {
    let (status, list) = call(
        rig,
        who,
        Method::GET,
        &format!("/api/clubs/{club}/pilots"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    list
}

fn one<'a>(list: &'a Value, callsign: &str) -> &'a Value {
    list.as_array()
        .unwrap()
        .iter()
        .find(|pilot| pilot["callsign"] == callsign)
        .unwrap_or_else(|| panic!("no {callsign} in {list}"))
}

async fn ask(rig: &Rig, who: &Person, pilot: &str) -> StatusCode {
    call(
        rig,
        who,
        Method::POST,
        &format!("/api/clubs/north/pilots/{pilot}/claim"),
        None,
    )
    .await
    .0
}

async fn say_yes(rig: &Rig, who: &Person, pilot: &str, to: &Person) -> (StatusCode, Value) {
    call(
        rig,
        who,
        Method::PUT,
        &format!("/api/clubs/north/pilots/{pilot}/holder"),
        Some(json!({"account": to.id.0})),
    )
    .await
}

async fn my_pilots(rig: &Rig, who: &Person) -> Value {
    let (status, me) = call(rig, who, Method::GET, "/api/me", None).await;
    assert_eq!(status, StatusCode::OK);
    me["account"]["pilots"].clone()
}

// ── Seeing the club's pilots ────────────────────────────────────────────────

#[tokio::test]
async fn only_someone_in_the_club_sees_its_pilots() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let sam = rig.member("Sam", "south", Role::Director);
    let owner = rig.owner("Olive");

    let list = pilots(&rig, &pia, "north").await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["callsign"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Ace", "Bee", "Cat"]);
    assert_eq!(one(&list, "Ace")["id"], "ace-1");

    let (status, _) = send(
        &rig,
        By::Nobody,
        Method::GET,
        "/api/clubs/north/pilots",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A director of another club has no say here, and is not told the club exists.
    let (status, _) = call(&rig, &sam, Method::GET, "/api/clubs/north/pilots", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&rig, &sam, Method::GET, "/api/clubs/nowhere/pilots", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // An owner sees any club's, and is told when there is no such club.
    assert_eq!(
        pilots(&rig, &owner, "north")
            .await
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let (status, _) = call(&rig, &owner, Method::GET, "/api/clubs/nowhere/pilots", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // South has none of north's.
    assert_eq!(pilots(&rig, &sam, "south").await, json!([]));
}

// ── Asking ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn asking_for_a_pilot_gives_nothing_until_someone_says_yes() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let dee = rig.member("Dee", "north", Role::Director);

    assert_eq!(ask(&rig, &pia, "ace-1").await, StatusCode::NO_CONTENT);
    // Asking twice is asking once.
    assert_eq!(ask(&rig, &pia, "ace-1").await, StatusCode::NO_CONTENT);

    let list = pilots(&rig, &pia, "north").await;
    let ace = one(&list, "Ace");
    assert_eq!(ace["asked"], true);
    assert_eq!(ace["taken"], false);
    assert_eq!(ace["mine"], false);
    assert_eq!(
        my_pilots(&rig, &pia).await,
        json!([{"club": {"id": "north", "name": "North FPV"}, "id": "ace-1", "callsign": "Ace", "held": false}])
    );
    // No results come from asking.
    let (status, results) = call(&rig, &pia, Method::GET, "/api/me/results", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(results, json!([]));

    // The director sees one ask.
    let list = pilots(&rig, &dee, "north").await;
    assert_eq!(
        one(&list, "Ace")["asks"],
        json!([{"id": pia.id.0, "name": "Pia"}])
    );

    let (status, _) = say_yes(&rig, &dee, "ace-1", &pia).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let list = pilots(&rig, &pia, "north").await;
    let ace = one(&list, "Ace");
    assert_eq!(
        (
            ace["mine"].clone(),
            ace["taken"].clone(),
            ace["asked"].clone()
        ),
        (json!(true), json!(true), json!(false))
    );
    assert_eq!(my_pilots(&rig, &pia).await[0]["held"], true);
}

#[tokio::test]
async fn knowing_an_id_is_not_enough() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let sam = rig.member("Sam", "south", Role::Pilot);
    let owner = rig.owner("Olive");

    // A pilot the club does not have.
    assert_eq!(ask(&rig, &pia, "nobody-9").await, StatusCode::NOT_FOUND);
    // Someone outside the club, who knows the id.
    assert_eq!(ask(&rig, &sam, "ace-1").await, StatusCode::FORBIDDEN);
    // An owner may look at any club, but is not in this one.
    assert_eq!(ask(&rig, &owner, "ace-1").await, StatusCode::FORBIDDEN);
    // Nobody signed in.
    let (status, _) = send(
        &rig,
        By::Nobody,
        Method::POST,
        "/api/clubs/north/pilots/ace-1/claim",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A pilot cannot say yes to themselves, or to anyone.
    assert_eq!(ask(&rig, &pia, "ace-1").await, StatusCode::NO_CONTENT);
    let (status, _) = say_yes(&rig, &pia, "ace-1", &pia).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        rig.state
            .accounts
            .claims_in(&club("north"))
            .unwrap()
            .iter()
            .all(|c| !c.how.holds())
    );
}

#[tokio::test]
async fn a_plain_member_is_not_told_who_holds_or_asked() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let ann = rig.member("Ann", "north", Role::Approver);
    let dee = rig.member("Dee", "north", Role::Director);
    ask(&rig, &pia, "ace-1").await;
    say_yes(&rig, &dee, "bee-2", &pia).await;

    let list = pilots(&rig, &quin, "north").await;
    let text = list.to_string();
    assert!(!text.contains(&pia.id.0) && !text.contains("Pia"), "{text}");
    assert_eq!(one(&list, "Bee")["taken"], true);
    assert_eq!(one(&list, "Ace")["asked"], false);

    for sees in [&ann, &dee] {
        let list = pilots(&rig, sees, "north").await;
        assert_eq!(
            one(&list, "Bee")["holder"],
            json!({"id": pia.id.0, "name": "Pia"})
        );
        assert_eq!(one(&list, "Ace")["asks"][0]["name"], "Pia");
    }
}

// ── Saying yes ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn an_approver_says_yes_to_others_but_not_to_themselves() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let ann = rig.member("Ann", "north", Role::Approver);
    ask(&rig, &pia, "ace-1").await;
    ask(&rig, &quin, "ace-1").await;
    ask(&rig, &ann, "bee-2").await;

    // Her own ask: someone else must.
    let (status, why) = say_yes(&rig, &ann, "bee-2", &ann).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{why}");
    // Nobody asked for Cat, so an approver has nothing to say yes to.
    let (status, _) = say_yes(&rig, &ann, "cat-3", &pia).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = say_yes(&rig, &ann, "ace-1", &pia).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Quin, who also asked, is answered by that: no longer asking, and cannot be given it.
    let list = pilots(&rig, &ann, "north").await;
    assert_eq!(one(&list, "Ace")["asks"], json!([]));
    assert_eq!(one(&list, "Ace")["holder"]["name"], "Pia");
    let (status, _) = say_yes(&rig, &ann, "ace-1", &quin).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A taken pilot cannot be asked for.
    assert_eq!(ask(&rig, &quin, "ace-1").await, StatusCode::CONFLICT);
    // Asking for your own changes nothing.
    assert_eq!(ask(&rig, &pia, "ace-1").await, StatusCode::NO_CONTENT);
    assert_eq!(my_pilots(&rig, &pia).await.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_director_gives_a_pilot_with_no_ask_and_takes_it_back() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let ann = rig.member("Ann", "north", Role::Approver);
    let dee = rig.member("Dee", "north", Role::Director);
    let sam = rig.member("Sam", "south", Role::Director);
    let outsider = rig.member("Otto", "south", Role::Pilot);

    assert_eq!(
        say_yes(&rig, &dee, "ace-1", &pia).await.0,
        StatusCode::NO_CONTENT
    );
    // Giving it to someone else moves it: one holder at most.
    assert_eq!(
        say_yes(&rig, &dee, "ace-1", &quin).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(my_pilots(&rig, &pia).await, json!([]));
    assert_eq!(my_pilots(&rig, &quin).await[0]["callsign"], "Ace");
    // A director may hold a pilot themselves.
    assert_eq!(
        say_yes(&rig, &dee, "bee-2", &dee).await.0,
        StatusCode::NO_CONTENT
    );

    // Not to someone outside the club, not a pilot the club lacks, not an account that
    // is not there.
    assert_eq!(
        say_yes(&rig, &dee, "cat-3", &outsider).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        say_yes(&rig, &dee, "nobody-9", &pia).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = call(
        &rig,
        &dee,
        Method::PUT,
        "/api/clubs/north/pilots/cat-3/holder",
        Some(json!({"account": "acct_nobody"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Another club's director has no say.
    assert_eq!(
        say_yes(&rig, &sam, "cat-3", &pia).await.0,
        StatusCode::FORBIDDEN
    );

    // Only a director takes a pilot from its holder.
    let holder = "/api/clubs/north/pilots/ace-1/holder";
    for cannot in [&ann, &pia, &sam] {
        assert_eq!(
            call(&rig, cannot, Method::DELETE, holder, None).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(&rig, &dee, Method::DELETE, holder, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&rig, &dee, Method::DELETE, holder, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(my_pilots(&rig, &quin).await, json!([]));
}

#[tokio::test]
async fn an_ask_is_withdrawn_or_refused_and_a_pilot_is_let_go() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let ann = rig.member("Ann", "north", Role::Approver);
    let dee = rig.member("Dee", "north", Role::Director);
    let claim = "/api/clubs/north/pilots/ace-1/claim";

    // Withdrawn by the one who asked.
    ask(&rig, &pia, "ace-1").await;
    assert_eq!(
        call(&rig, &pia, Method::DELETE, claim, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&rig, &pia, Method::DELETE, claim, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(my_pilots(&rig, &pia).await, json!([]));

    // Refused by an approver. A pilot cannot refuse another's.
    ask(&rig, &pia, "ace-1").await;
    let refuse = format!("/api/clubs/north/pilots/ace-1/asks/{}", pia.id.0);
    assert_eq!(
        call(&rig, &quin, Method::DELETE, &refuse, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&rig, &ann, Method::DELETE, &refuse, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&rig, &ann, Method::DELETE, &refuse, None).await.0,
        StatusCode::NOT_FOUND
    );

    // Saying no to an ask is not a way to take a pilot from its holder.
    say_yes(&rig, &dee, "ace-1", &pia).await;
    assert_eq!(
        call(&rig, &ann, Method::DELETE, &refuse, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(my_pilots(&rig, &pia).await[0]["held"], true);
    // One person's "let go" does not touch another's pilot.
    assert_eq!(
        call(&rig, &quin, Method::DELETE, claim, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(my_pilots(&rig, &pia).await[0]["held"], true);
    // The holder lets go.
    assert_eq!(
        call(&rig, &pia, Method::DELETE, claim, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        one(&pilots(&rig, &pia, "north").await, "Ace")["taken"],
        false
    );
}

#[tokio::test]
async fn leaving_a_club_lets_go_of_its_pilots() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let dee = rig.member("Dee", "north", Role::Director);
    say_yes(&rig, &dee, "ace-1", &pia).await;
    ask(&rig, &pia, "bee-2").await;

    let (status, _) = call(
        &rig,
        &dee,
        Method::DELETE,
        &format!("/api/clubs/north/members/{}", pia.id.0),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        rig.state.accounts.claims_in(&club("north")).unwrap(),
        vec![]
    );
    assert_eq!(my_pilots(&rig, &pia).await, json!([]));
    // And an ask left behind by someone who was removed cannot be said yes to.
    let list = pilots(&rig, &dee, "north").await;
    assert_eq!(one(&list, "Bee")["asks"], json!([]));
}

// ── An invite made for a pilot ──────────────────────────────────────────────

async fn invite_for(rig: &Rig, who: &Person, body: Value) -> (StatusCode, Value) {
    call(
        rig,
        who,
        Method::POST,
        "/api/clubs/north/invites",
        Some(body),
    )
    .await
}

#[tokio::test]
async fn an_invite_made_for_a_pilot_gives_that_pilot() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);

    let (status, made) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Personal", "label": "Ace", "pilot": "ace-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{made}");
    assert_eq!(
        made["invite"]["pilot"],
        json!({"id": "ace-1", "callsign": "Ace"})
    );
    let secret = made["secret"].as_str().unwrap().to_owned();

    // The link says who it is for, to whoever opens it.
    let (status, seen) = send(
        &rig,
        By::Nobody,
        Method::GET,
        &format!("/api/invites/{secret}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["pilot"]["callsign"], "Ace");

    let ace = rig.join("AceInRealLife", &secret);
    assert_eq!(
        my_pilots(&rig, &ace).await,
        json!([{"club": {"id": "north", "name": "North FPV"}, "id": "ace-1", "callsign": "Ace", "held": true}])
    );
    assert_eq!(
        one(&pilots(&rig, &dee, "north").await, "Ace")["holder"]["name"],
        "AceInRealLife"
    );
}

#[tokio::test]
async fn an_invite_for_a_pilot_is_only_made_for_one_that_is_free() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let pia = rig.member("Pia", "north", Role::Pilot);
    say_yes(&rig, &dee, "bee-2", &pia).await;

    let (status, _) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Personal", "label": "x", "pilot": "nobody-9"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Personal", "label": "x", "pilot": "bee-2"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Group", "label": "x", "pilot": "ace-1", "max_uses": 5, "days": 5}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // None of those made an invite.
    let (_, invites) = call(&rig, &dee, Method::GET, "/api/clubs/north/invites", None).await;
    assert!(
        invites
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("pilot").is_none()),
        "{invites}"
    );
    // An empty pilot is no pilot.
    let (status, made) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Personal", "label": "x", "pilot": ""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(made["invite"].get("pilot").is_none());
}

#[tokio::test]
async fn an_invite_for_a_pilot_someone_came_to_hold_still_joins_the_club() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let pia = rig.member("Pia", "north", Role::Pilot);
    let (_, made) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Personal", "label": "Ace", "pilot": "ace-1"}),
    )
    .await;
    let secret = made["secret"].as_str().unwrap().to_owned();
    // After the invite was made, the director gave the pilot to someone else.
    say_yes(&rig, &dee, "ace-1", &pia).await;

    let late = rig.join("Late", &secret);
    let (_, me) = call(&rig, &late, Method::GET, "/api/me", None).await;
    assert_eq!(me["account"]["clubs"][0]["club"]["id"], "north");
    assert_eq!(me["account"]["pilots"], json!([]));
    assert_eq!(my_pilots(&rig, &pia).await[0]["held"], true);
}

#[tokio::test]
async fn someone_with_an_account_uses_an_invite_for_a_pilot() {
    let rig = rig();
    let dee = rig.member("Dee", "north", Role::Director);
    let sam = rig.member("Sam", "south", Role::Pilot);
    let (_, made) = invite_for(
        &rig,
        &dee,
        json!({"kind": "Personal", "label": "Cat", "pilot": "cat-3"}),
    )
    .await;
    let secret = made["secret"].as_str().unwrap();

    let (status, me) = call(
        &rig,
        &sam,
        Method::POST,
        &format!("/api/invites/{secret}/accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["account"]["pilots"][0]["callsign"], "Cat");
    assert_eq!(me["account"]["pilots"][0]["held"], true);
    // It worked once.
    let other = rig.member("Other", "south", Role::Pilot);
    let (status, _) = call(
        &rig,
        &other,
        Method::POST,
        &format!("/api/invites/{secret}/accept"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── What a Director does ────────────────────────────────────────────────────

#[tokio::test]
async fn a_director_needs_its_token_and_reaches_only_its_club() {
    let rig = rig();
    for (method, path) in [
        (Method::GET, "/push/pilots"),
        (Method::PUT, "/push/pilots"),
        (Method::GET, "/push/members"),
        (Method::PUT, "/push/pilots/ace-1/account"),
        (Method::DELETE, "/push/pilots/ace-1/account"),
    ] {
        let (status, _) = send(&rig, By::Nobody, method.clone(), path, Some("[]".into())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        let (status, _) = send(
            &rig,
            By::Token("gfs_not-a-token"),
            method.clone(),
            path,
            Some("[]".into()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    let pia = rig.member("Pia", "north", Role::Pilot);
    let sam = rig.member("Sam", "south", Role::Pilot);

    // Each token sees its own club's pilots and members.
    let (_, north) = push(&rig, &rig.north, Method::GET, "/push/pilots", None).await;
    assert_eq!(north.as_array().unwrap().len(), 3);
    let (_, south) = push(&rig, &rig.south, Method::GET, "/push/pilots", None).await;
    assert_eq!(south, json!([]));
    let (_, members) = push(&rig, &rig.north, Method::GET, "/push/members", None).await;
    assert_eq!(members, json!([{"id": pia.id.0, "name": "Pia"}]));

    // South's Director sends a pilot with the same id as one of north's. They are two
    // pilots, and south's Director cannot give north's to anyone.
    let (status, _) = push(
        &rig,
        &rig.south,
        Method::PUT,
        "/push/pilots",
        Some(json!([{"id": "ace-1", "callsign": "Other Ace"}])),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = push(
        &rig,
        &rig.south,
        Method::PUT,
        "/push/pilots/ace-1/account",
        Some(json!({"account": sam.id.0})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, north) = push(&rig, &rig.north, Method::GET, "/push/pilots", None).await;
    assert_eq!(north[0], json!({"id": "ace-1", "callsign": "Ace"}));
    // North's member is no member of south.
    let (status, _) = push(
        &rig,
        &rig.south,
        Method::PUT,
        "/push/pilots/ace-1/account",
        Some(json!({"account": pia.id.0})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_director_says_which_member_holds_a_pilot() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    ask(&rig, &quin, "ace-1").await;

    let account = "/push/pilots/ace-1/account";
    let (status, _) = push(
        &rig,
        &rig.north,
        Method::PUT,
        account,
        Some(json!({"account": pia.id.0})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Saying it again changes nothing.
    let (status, _) = push(
        &rig,
        &rig.north,
        Method::PUT,
        account,
        Some(json!({"account": pia.id.0})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, list) = push(&rig, &rig.north, Method::GET, "/push/pilots", None).await;
    assert_eq!(
        list[0],
        json!({"id": "ace-1", "callsign": "Ace", "account": {"id": pia.id.0, "name": "Pia"}})
    );
    assert_eq!(my_pilots(&rig, &pia).await[0]["held"], true);
    // Quin, who had asked, is answered by it.
    assert_eq!(my_pilots(&rig, &quin).await, json!([]));

    // A pilot the club does not know yet, an account that is not there, and an id that
    // is not an id.
    let (status, _) = push(
        &rig,
        &rig.north,
        Method::PUT,
        "/push/pilots/new-7/account",
        Some(json!({"account": pia.id.0})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = push(
        &rig,
        &rig.north,
        Method::PUT,
        account,
        Some(json!({"account": "acct_nobody"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = push(
        &rig,
        &rig.north,
        Method::PUT,
        "/push/pilots/a%20b/account",
        Some(json!({"account": pia.id.0})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(my_pilots(&rig, &pia).await.as_array().unwrap().len(), 1);

    // Taking it back, and taking back what nobody holds.
    for _ in 0..2 {
        let (status, _) = push(&rig, &rig.north, Method::DELETE, account, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    assert_eq!(my_pilots(&rig, &pia).await, json!([]));
}

#[tokio::test]
async fn the_pilots_a_director_sends_are_added_and_none_is_lost() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    push(
        &rig,
        &rig.north,
        Method::PUT,
        "/push/pilots/ace-1/account",
        Some(json!({"account": pia.id.0})),
    )
    .await;

    // A list with one new pilot, one renamed, one whose id will not do, one with no
    // callsign, and without Bee or Cat. It names an account for Dan, which is not read.
    let (status, _) = push(
        &rig,
        &rig.north,
        Method::PUT,
        "/push/pilots",
        Some(json!([
            {"id": "dan-4", "callsign": " Dan ", "color": "#00FF00", "account": {"id": pia.id.0, "name": "Pia"}},
            {"id": "ace-1", "callsign": "Ace of Spades"},
            {"id": "../etc", "callsign": "Sneaky"},
            {"id": "eve-5", "callsign": "   "}
        ])),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, list) = push(&rig, &rig.north, Method::GET, "/push/pilots", None).await;
    assert_eq!(
        list,
        json!([
            {"id": "ace-1", "callsign": "Ace of Spades", "account": {"id": pia.id.0, "name": "Pia"}},
            {"id": "bee-2", "callsign": "Bee"},
            {"id": "cat-3", "callsign": "Cat"},
            {"id": "dan-4", "callsign": "Dan", "color": "#00FF00"}
        ])
    );
}

// ── Results ─────────────────────────────────────────────────────────────────

/// Push an event to a club: its header, then a log that puts pilots in a class.
async fn push_event(
    rig: &Rig,
    token: &str,
    id: &str,
    name: &str,
    created_at: i64,
    pilots: &[(&str, &str)],
) {
    let header = json!({
        "meta": {"id": id, "name": name, "created_at": created_at, "persistent": true},
        "pilots": pilots.iter().map(|(id, callsign)| json!({"id": id, "callsign": callsign, "vtx_types": []})).collect::<Vec<_>>(),
        "classes": [{"id": "open", "name": "Open Class", "source": "Custom"}]
    });
    let (status, why) = push(
        rig,
        token,
        Method::PUT,
        &format!("/push/events/{id}"),
        Some(header),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{why}");
    let ids: Vec<&str> = pilots.iter().map(|(id, _)| *id).collect();
    let entry = json!({"RosterSet": {"class": "open", "pilots": ids}}).to_string();
    let body = format!(
        "{{\"from\":0,\"before\":\"{}\",\"entries\":[{{\"event\":{entry}}}]}}",
        Chain::start().to_hex()
    );
    let (status, why) = send(
        rig,
        By::Token(token),
        Method::POST,
        &format!("/push/events/{id}/log"),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{why}");
}

#[tokio::test]
async fn an_events_header_teaches_the_club_its_pilots() {
    let rig = rig();
    let sam = rig.member("Sam", "south", Role::Pilot);
    assert_eq!(pilots(&rig, &sam, "south").await, json!([]));
    push_event(
        &rig,
        &rig.south,
        "spring",
        "Spring Race",
        10,
        &[("zed-1", "Zed"), ("yak-2", "Yak")],
    )
    .await;
    let list = pilots(&rig, &sam, "south").await;
    assert_eq!(one(&list, "Zed")["id"], "zed-1");
    assert_eq!(one(&list, "Yak")["taken"], false);
}

#[tokio::test]
async fn results_reach_the_account_that_holds_the_pilot_and_no_other() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let dee = rig.member("Dee", "north", Role::Director);
    push_event(
        &rig,
        &rig.north,
        "spring",
        "Spring Race",
        10,
        &[("bee-2", "Bee"), ("ace-1", "Ace")],
    )
    .await;
    push_event(
        &rig,
        &rig.north,
        "summer",
        "Summer Race",
        20,
        &[("ace-1", "Acey"), ("bee-2", "Bee"), ("cat-3", "Cat")],
    )
    .await;
    push_event(
        &rig,
        &rig.north,
        "winter",
        "Winter Race",
        30,
        &[("bee-2", "Bee")],
    )
    .await;
    // Another club raced a pilot with the same id. It is not north's Ace.
    push_event(
        &rig,
        &rig.south,
        "south-day",
        "South Day",
        40,
        &[("ace-1", "Other Ace")],
    )
    .await;

    say_yes(&rig, &dee, "ace-1", &pia).await;
    ask(&rig, &quin, "bee-2").await;

    let (status, results) = call(&rig, &pia, Method::GET, "/api/me/results", None).await;
    assert_eq!(status, StatusCode::OK, "{results}");
    let rows = results.as_array().unwrap();
    // Newest first, only the events Ace was in, each under the callsign of that day.
    assert_eq!(rows.len(), 2, "{results}");
    assert_eq!(rows[0]["event"]["name"], "Summer Race");
    assert_eq!(rows[0]["callsign"], "Acey");
    assert_eq!(rows[0]["club"], json!({"id": "north", "name": "North FPV"}));
    assert_eq!(
        rows[0]["classes"],
        json!([{"class": "Open Class", "position": 1, "of": 3, "placed": false}])
    );
    assert_eq!(rows[1]["event"]["name"], "Spring Race");
    assert_eq!(rows[1]["callsign"], "Ace");
    assert_eq!(rows[1]["classes"][0]["position"], 2);
    assert_eq!(rows[1]["classes"][0]["of"], 2);

    // Quin has only asked for Bee, and Dee holds nobody.
    for nobody in [&quin, &dee] {
        let (_, results) = call(&rig, nobody, Method::GET, "/api/me/results", None).await;
        assert_eq!(results, json!([]));
    }
    let (status, _) = send(&rig, By::Nobody, Method::GET, "/api/me/results", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The results follow the pilot: given to Quin, they are Quin's and no longer Pia's.
    say_yes(&rig, &dee, "ace-1", &quin).await;
    let (_, results) = call(&rig, &pia, Method::GET, "/api/me/results", None).await;
    assert_eq!(results, json!([]));
    let (_, results) = call(&rig, &quin, Method::GET, "/api/me/results", None).await;
    assert_eq!(results.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_account_with_pilots_in_two_clubs_sees_both() {
    let rig = rig();
    let owner = rig.owner("Olive");
    let pia = rig.member("Pia", "north", Role::Pilot);
    // Pia joins south too.
    let (_, secret) = rig
        .state
        .accounts
        .new_invite(
            NewInvite {
                kind: InviteKind::Personal {
                    club: club("south"),
                    role: Role::Pilot,
                    pilot: None,
                },
                label: "Pia".into(),
                max_uses: 1,
                expires_at: None,
            },
            None,
            now_ms(),
        )
        .unwrap();
    rig.state
        .accounts
        .accept_invite(&secret, &pia.id, now_ms())
        .unwrap();
    push_event(
        &rig,
        &rig.north,
        "spring",
        "Spring Race",
        10,
        &[("ace-1", "Ace")],
    )
    .await;
    push_event(
        &rig,
        &rig.south,
        "south-day",
        "South Day",
        40,
        &[("pia-9", "Pia South")],
    )
    .await;

    for (club, pilot) in [("north", "ace-1"), ("south", "pia-9")] {
        let (status, why) = call(
            &rig,
            &owner,
            Method::PUT,
            &format!("/api/clubs/{club}/pilots/{pilot}/holder"),
            Some(json!({"account": pia.id.0})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{why}");
    }
    let (_, results) = call(&rig, &pia, Method::GET, "/api/me/results", None).await;
    let names: Vec<(&str, &str)> = results
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["club"]["name"].as_str().unwrap(),
                r["callsign"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(names, [("South FPV", "Pia South"), ("North FPV", "Ace")]);
}

#[tokio::test]
async fn the_claims_are_there_when_the_server_starts_again() {
    let dir = tempfile::tempdir().unwrap();
    let pia;
    {
        let state = AppState::open(dir.path()).unwrap();
        let rig = Rig {
            app: router(state.clone()),
            north: state.book.new_token(&club("club"), "laptop").unwrap().1,
            south: String::new(),
            state,
            _dir: tempfile::tempdir().unwrap(),
        };
        pia = rig.member("Pia", "club", Role::Pilot);
        push(
            &rig,
            &rig.north,
            Method::PUT,
            "/push/pilots",
            Some(json!([{"id": "ace-1", "callsign": "Ace"}])),
        )
        .await;
        let (status, _) = push(
            &rig,
            &rig.north,
            Method::PUT,
            "/push/pilots/ace-1/account",
            Some(json!({"account": pia.id.0})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let state = AppState::open(dir.path()).unwrap();
    let claims = state.accounts.claims_of(&pia.id).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].pilot, "ace-1");
    assert!(claims[0].how.holds());
}

// ── The account wins where it exists (§13.7) ────────────────────────────────

async fn set_profile(rig: &Rig, who: &Person, profile: Value) {
    let (status, body) = call(rig, who, Method::PUT, "/api/me/profile", Some(profile)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The pilots of an event as a visitor is shown them: `(id, callsign, colour)`.
async fn event_pilots(rig: &Rig, club: &str, event: &str) -> Vec<(String, String, Value)> {
    let path = format!("/api/clubs/{club}/events/{event}/public/event/pilots");
    let (status, list) = send(rig, By::Nobody, Method::GET, &path, None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let text = |value: &Value| value.as_str().unwrap().to_owned();
    list.as_array()
        .unwrap()
        .iter()
        .map(|pilot| {
            (
                text(&pilot["id"]),
                text(&pilot["callsign"]),
                pilot.get("color").cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

#[tokio::test]
async fn results_a_pilot_holds_are_shown_under_their_accounts_callsign_and_colour() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let dee = rig.member("Dee", "north", Role::Director);
    // The event was run with the callsigns the director had that day.
    let raced = [("ace-1", "Swift"), ("bee-2", "Bee"), ("cat-3", "Cat")];
    push_event(&rig, &rig.north, "spring", "Spring Race", 10, &raced).await;
    // The same pilot id raced in another club. It is not north's.
    let souths = [("ace-1", "Other Ace")];
    push_event(&rig, &rig.south, "south-day", "South Day", 40, &souths).await;
    let callsigns = |pilots: Vec<(String, String, Value)>| -> Vec<String> {
        let mut names: Vec<String> = pilots.into_iter().map(|(_, name, _)| name).collect();
        names.sort();
        names
    };
    assert_eq!(
        callsigns(event_pilots(&rig, "north", "spring").await),
        ["Bee", "Cat", "Swift"]
    );

    // Pia calls herself PiaFPV. Until she holds a pilot, that changes nothing shown.
    set_profile(
        &rig,
        &pia,
        json!({"callsign": "PiaFPV", "color": "#aa00ff"}),
    )
    .await;
    assert_eq!(
        callsigns(event_pilots(&rig, "north", "spring").await),
        ["Bee", "Cat", "Swift"]
    );
    // Asking for a pilot is not holding it.
    ask(&rig, &pia, "ace-1").await;
    assert_eq!(
        callsigns(event_pilots(&rig, "north", "spring").await),
        ["Bee", "Cat", "Swift"]
    );
    assert_eq!(my_pilots(&rig, &pia).await[0]["callsign"], "Swift");

    // She comes to hold the pilot. The results are shown as hers, under her account.
    say_yes(&rig, &dee, "ace-1", &pia).await;
    let shown = event_pilots(&rig, "north", "spring").await;
    let hers = shown.iter().find(|(id, _, _)| id == "ace-1").unwrap();
    assert_eq!(hers.1, "PiaFPV");
    assert_eq!(hers.2, json!("#AA00FF"));
    assert_eq!(callsigns(shown), ["Bee", "Cat", "PiaFPV"]);
    // The other club's pilot of the same id is not hers.
    assert_eq!(
        callsigns(event_pilots(&rig, "south", "south-day").await),
        ["Other Ace"]
    );
    // Her own pages say the same.
    assert_eq!(my_pilots(&rig, &pia).await[0]["callsign"], "PiaFPV");
    let (_, results) = call(&rig, &pia, Method::GET, "/api/me/results", None).await;
    assert_eq!(results[0]["callsign"], "PiaFPV");
    // And the club's list, for a member and for the director.
    for who in [&quin, &dee] {
        let list = pilots(&rig, who, "north").await;
        let row = one(&list, "PiaFPV");
        assert_eq!(row["id"], "ace-1");
        assert_eq!(row["color"], "#AA00FF");
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .all(|p| p["callsign"] != "Swift")
        );
    }

    // The event's own record is not rewritten: only how it is shown.
    let copy = rig
        .state
        .store
        .get(
            &club("north"),
            &gridfpv_server::events::EventId("spring".into()),
        )
        .unwrap()
        .unwrap();
    let header = copy.header().unwrap().unwrap();
    assert!(header.contains("\"Swift\"") && !header.contains("PiaFPV"));

    // She changes her callsign. It shows at once, with nothing sent again.
    set_profile(&rig, &pia, json!({"callsign": "Pia Two"})).await;
    let shown = event_pilots(&rig, "north", "spring").await;
    let hers = shown.iter().find(|(id, _, _)| id == "ace-1").unwrap();
    assert_eq!(hers.1, "Pia Two");
    // She gave no colour this time, so the colour of the day shows.
    assert_eq!(hers.2, Value::Null);

    // With no callsign in her profile, the callsign of the day shows again.
    set_profile(&rig, &pia, json!({"multigp_id": "24680"})).await;
    assert_eq!(
        callsigns(event_pilots(&rig, "north", "spring").await),
        ["Bee", "Cat", "Swift"]
    );
    let (_, results) = call(&rig, &pia, Method::GET, "/api/me/results", None).await;
    assert_eq!(results[0]["callsign"], "Swift");

    // She lets go of the pilot. It is the club's again, as the event had it.
    set_profile(&rig, &pia, json!({"callsign": "PiaFPV"})).await;
    let path = "/api/clubs/north/pilots/ace-1/claim";
    let (status, _) = call(&rig, &pia, Method::DELETE, path, None).await;
    assert!(status.is_success(), "{status}");
    assert_eq!(
        callsigns(event_pilots(&rig, "north", "spring").await),
        ["Bee", "Cat", "Swift"]
    );
}

#[tokio::test]
async fn a_director_is_sent_each_accounts_profile_with_the_pilot_it_holds() {
    let rig = rig();
    let pia = rig.member("Pia", "north", Role::Pilot);
    let quin = rig.member("Quin", "north", Role::Pilot);
    let dee = rig.member("Dee", "north", Role::Director);
    let raced = [("ace-1", "Swift"), ("bee-2", "Bee")];
    push_event(&rig, &rig.north, "spring", "Spring Race", 10, &raced).await;
    let full = json!({
        "callsign": "PiaFPV",
        "color": "#AA00FF",
        "multigp_id": "24680",
        "velocidrone_id": "pia.sim"
    });
    set_profile(&rig, &pia, full.clone()).await;
    say_yes(&rig, &dee, "ace-1", &pia).await;

    // The club's pilots: the held one comes as its account calls itself, with the profile.
    let (status, list) = push(&rig, &rig.north, Method::GET, "/push/pilots", None).await;
    assert_eq!(status, StatusCode::OK);
    let ace = list
        .as_array()
        .unwrap()
        .iter()
        .find(|pilot| pilot["id"] == "ace-1")
        .unwrap();
    assert_eq!(ace["callsign"], "PiaFPV");
    assert_eq!(ace["color"], "#AA00FF");
    assert_eq!(ace["account"]["id"], pia.id.0);
    assert_eq!(ace["account"]["name"], "Pia");
    assert_eq!(ace["account"]["profile"], full);
    let bee = list
        .as_array()
        .unwrap()
        .iter()
        .find(|pilot| pilot["id"] == "bee-2")
        .unwrap();
    assert_eq!(bee["callsign"], "Bee");
    assert!(bee.get("account").is_none());

    // The club's members: each with what they say of themselves. Nothing for one who
    // filled nothing in.
    let (status, members) = push(&rig, &rig.north, Method::GET, "/push/members", None).await;
    assert_eq!(status, StatusCode::OK);
    let by_name = |name: &str| -> Value {
        members
            .as_array()
            .unwrap()
            .iter()
            .find(|member| member["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(by_name("Pia")["profile"], full);
    assert!(by_name("Quin").get("profile").is_none());
    assert_eq!(by_name("Quin"), json!({"id": quin.id.0, "name": "Quin"}));

    // Another club's token is told nothing of north's members or their profiles.
    let (_, souths) = push(&rig, &rig.south, Method::GET, "/push/members", None).await;
    assert!(!souths.to_string().contains("PiaFPV"), "{souths}");
    // A visitor is never shown the ids: only the callsign and the colour.
    let path = "/api/clubs/north/events/spring/public/event/pilots";
    let (_, seen) = send(&rig, By::Nobody, Method::GET, path, None).await;
    assert!(seen.to_string().contains("PiaFPV"));
    assert!(!seen.to_string().contains("24680") && !seen.to_string().contains("pia.sim"));
    // Nor is a plain member of the club.
    let list = pilots(&rig, &quin, "north").await;
    assert!(!list.to_string().contains("24680"), "{list}");
}
