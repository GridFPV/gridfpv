//! The rules of accounts, sign-ins, sessions, invites and roles (docs/primitives.md §13.2,
//! §13.7, §13.8).

use gridfpv_serverd::accounts::{
    AccountError, AccountId, Accounts, Act, INVITE_PREFIX, Invite, InviteKind, NewInvite,
    NewSignIn, Role, SESSION_MS, Standing,
};
use gridfpv_serverd::book::ClubId;

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 24 * 60 * 60 * 1000;

fn club(id: &str) -> ClubId {
    ClubId(id.into())
}

fn discord(subject: &str) -> NewSignIn<'_> {
    NewSignIn {
        provider: "discord",
        subject,
        label: "someone#0001",
    }
}

fn google(subject: &str) -> NewSignIn<'_> {
    NewSignIn {
        provider: "google",
        subject,
        label: "someone@example.org",
    }
}

fn personal(club_id: &str, role: Role) -> NewInvite {
    NewInvite {
        kind: InviteKind::Personal {
            club: club(club_id),
            role,
            pilot: None,
        },
        label: "Ace".into(),
        max_uses: 1,
        expires_at: None,
    }
}

fn group(club_id: &str, max_uses: u32) -> NewInvite {
    NewInvite {
        kind: InviteKind::Group {
            club: club(club_id),
        },
        label: "Club Discord".into(),
        max_uses,
        expires_at: Some(NOW + 7 * DAY),
    }
}

fn owner_invite() -> NewInvite {
    NewInvite {
        kind: InviteKind::Owner,
        label: "First owner".into(),
        max_uses: 1,
        expires_at: None,
    }
}

/// Make an account with a fresh invite of the kind given. Returns its id.
fn join(accounts: &Accounts, spec: NewInvite, sign_in: &NewSignIn<'_>, name: &str) -> AccountId {
    let (_, secret) = accounts.new_invite(spec, None, NOW).unwrap();
    accounts
        .join_with_invite(&secret, sign_in, name, NOW)
        .unwrap()
        .0
        .id
}

fn standing(accounts: &Accounts, id: &AccountId) -> Standing {
    accounts.standing(id).unwrap().unwrap()
}

// ── An account is made only with an invite ──────────────────────────────────

#[test]
fn an_invite_makes_an_account_signs_it_in_and_gives_what_it_is_for() {
    let accounts = Accounts::in_memory().unwrap();
    let (invite, secret) = accounts
        .new_invite(personal("north", Role::Pilot), None, NOW)
        .unwrap();
    assert!(secret.starts_with(INVITE_PREFIX) && secret.len() > 30);
    assert_eq!(invite.uses, 0);

    let (account, used, session) = accounts
        .join_with_invite(&secret, &discord("111"), "  Ace  ", NOW)
        .unwrap();
    assert_eq!(account.name, "Ace");
    assert_eq!(used.id, invite.id);

    // Signed in at once, as that account, a pilot of that club and nothing more.
    let who = accounts.session(&session, NOW).unwrap().unwrap();
    assert_eq!(who.account, account.id);
    assert!(!who.owner);
    assert_eq!(who.role_in(&club("north")), Some(Role::Pilot));
    assert_eq!(who.clubs.len(), 1);

    // And it signs in that way from now on.
    assert_eq!(
        accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .unwrap()
            .id,
        account.id
    );
    let kept = accounts.sign_ins(&account.id).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(
        (kept[0].provider.as_str(), kept[0].subject.as_str()),
        ("discord", "111")
    );
}

#[test]
fn a_sign_in_with_no_account_is_nobody_however_much_like_someone_it_looks() {
    let accounts = Accounts::in_memory().unwrap();
    join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    // Another provider's id that happens to be the same text, the same label, another id.
    assert!(
        accounts
            .account_for_sign_in("google", "111")
            .unwrap()
            .is_none()
    );
    assert!(
        accounts
            .account_for_sign_in("discord", "112")
            .unwrap()
            .is_none()
    );
    assert!(
        accounts
            .account_for_sign_in("discord", "")
            .unwrap()
            .is_none()
    );
    assert!(
        accounts
            .account_for_sign_in("Discord", "111")
            .unwrap()
            .is_none()
    );
}

#[test]
fn what_is_not_an_invite_makes_no_account_and_leaves_nothing_behind() {
    let accounts = Accounts::in_memory().unwrap();
    let (invite, secret) = accounts
        .new_invite(personal("north", Role::Pilot), None, NOW)
        .unwrap();
    for wrong in [
        "",
        INVITE_PREFIX,
        "gfi_not-an-invite",
        invite.id.as_str(),
        &secret[..secret.len() - 1],
        &format!("{secret}x"),
        &secret.to_uppercase(),
        &secret[INVITE_PREFIX.len()..],
    ] {
        assert_eq!(
            accounts
                .join_with_invite(wrong, &discord("111"), "Ace", NOW)
                .unwrap_err(),
            AccountError::InviteDoesNotWork,
            "{wrong:?}"
        );
        assert!(
            accounts.peek_invite(wrong, NOW).unwrap().is_none(),
            "{wrong:?}"
        );
    }
    assert!(
        accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .is_none()
    );
    assert!(accounts.members(&club("north")).unwrap().is_empty());
    // The real one still works after all that.
    assert!(
        accounts
            .join_with_invite(&secret, &discord("111"), "Ace", NOW)
            .is_ok()
    );
}

#[test]
fn the_secret_of_an_invite_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::open(dir.path()).unwrap();
    let (_, secret) = accounts
        .new_invite(personal("north", Role::Pilot), None, NOW)
        .unwrap();
    let (_, _, session) = accounts
        .join_with_invite(&secret, &discord("111"), "Ace", NOW)
        .unwrap();
    drop(accounts);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(&secret) && !text.contains(&secret[INVITE_PREFIX.len()..]));
        assert!(
            !text.contains(&session),
            "a session's secret is in the file"
        );
    }
}

#[test]
fn a_personal_invite_works_once() {
    let accounts = Accounts::in_memory().unwrap();
    let (_, secret) = accounts
        .new_invite(personal("north", Role::Pilot), None, NOW)
        .unwrap();
    accounts
        .join_with_invite(&secret, &discord("111"), "Ace", NOW)
        .unwrap();
    assert_eq!(
        accounts
            .join_with_invite(&secret, &discord("222"), "Bee", NOW)
            .unwrap_err(),
        AccountError::InviteDoesNotWork
    );
    assert!(accounts.peek_invite(&secret, NOW).unwrap().is_none());
    assert!(
        accounts
            .account_for_sign_in("discord", "222")
            .unwrap()
            .is_none()
    );
    assert_eq!(accounts.members(&club("north")).unwrap().len(), 1);
}

#[test]
fn a_personal_invite_asked_to_work_many_times_still_works_once() {
    let accounts = Accounts::in_memory().unwrap();
    let mut spec = personal("north", Role::Director);
    spec.max_uses = 50;
    let (invite, secret) = accounts.new_invite(spec, None, NOW).unwrap();
    assert_eq!(invite.max_uses, 1);
    accounts
        .join_with_invite(&secret, &discord("111"), "Ace", NOW)
        .unwrap();
    assert!(
        accounts
            .join_with_invite(&secret, &discord("222"), "Bee", NOW)
            .is_err()
    );
    let mut owner = owner_invite();
    owner.max_uses = 50;
    assert_eq!(accounts.new_invite(owner, None, NOW).unwrap().0.max_uses, 1);
}

#[test]
fn a_group_invite_works_up_to_its_limit_and_until_its_end() {
    let accounts = Accounts::in_memory().unwrap();
    let (_, secret) = accounts.new_invite(group("north", 3), None, NOW).unwrap();
    for n in 0..3 {
        let (account, _, _) = accounts
            .join_with_invite(&secret, &discord(&format!("user-{n}")), "Pilot", NOW + n)
            .unwrap();
        assert_eq!(
            standing(&accounts, &account.id).role_in(&club("north")),
            Some(Role::Pilot)
        );
    }
    assert_eq!(
        accounts
            .join_with_invite(&secret, &discord("user-3"), "Late", NOW)
            .unwrap_err(),
        AccountError::InviteDoesNotWork
    );
    assert_eq!(accounts.members(&club("north")).unwrap().len(), 3);

    // Another, with room left, stops at its end date. The moment itself is too late.
    let (_, secret) = accounts.new_invite(group("north", 100), None, NOW).unwrap();
    let end = NOW + 7 * DAY;
    assert!(accounts.peek_invite(&secret, end - 1).unwrap().is_some());
    assert!(accounts.peek_invite(&secret, end).unwrap().is_none());
    assert_eq!(
        accounts
            .join_with_invite(&secret, &discord("late"), "Late", end)
            .unwrap_err(),
        AccountError::InviteDoesNotWork
    );
}

#[test]
fn a_group_invite_must_have_an_end_and_a_limit() {
    let accounts = Accounts::in_memory().unwrap();
    let mut no_end = group("north", 10);
    no_end.expires_at = None;
    assert!(matches!(
        accounts.new_invite(no_end, None, NOW),
        Err(AccountError::Invalid(_))
    ));
    assert!(matches!(
        accounts.new_invite(group("north", 0), None, NOW),
        Err(AccountError::Invalid(_))
    ));
    let mut past = group("north", 10);
    past.expires_at = Some(NOW);
    assert!(matches!(
        accounts.new_invite(past, None, NOW),
        Err(AccountError::Invalid(_))
    ));
    let mut unnamed = group("north", 10);
    unnamed.label = "   ".into();
    assert!(matches!(
        accounts.new_invite(unnamed, None, NOW),
        Err(AccountError::Invalid(_))
    ));
    assert!(accounts.all_invites().unwrap().is_empty());
}

#[test]
fn an_invite_taken_back_stops_working_and_stays_on_the_list() {
    let accounts = Accounts::in_memory().unwrap();
    let (invite, secret) = accounts.new_invite(group("north", 10), None, NOW).unwrap();
    accounts
        .join_with_invite(&secret, &discord("111"), "Ace", NOW)
        .unwrap();
    assert!(accounts.revoke_invite(&invite.id, NOW + 5).unwrap());
    assert!(!accounts.revoke_invite(&invite.id, NOW + 6).unwrap());
    assert!(!accounts.revoke_invite("no-such-id", NOW).unwrap());
    assert!(accounts.peek_invite(&secret, NOW + 6).unwrap().is_none());
    assert_eq!(
        accounts
            .join_with_invite(&secret, &discord("222"), "Bee", NOW + 6)
            .unwrap_err(),
        AccountError::InviteDoesNotWork
    );
    // Who already joined with it keeps their account.
    assert!(
        accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .is_some()
    );
    let listed: Vec<Invite> = accounts.invites_of(&club("north")).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].uses, listed[0].revoked_at), (1, Some(NOW + 5)));
    assert!(!listed[0].stands(NOW + 6));
}

#[test]
fn looking_at_an_invite_does_not_use_it() {
    let accounts = Accounts::in_memory().unwrap();
    let (_, secret) = accounts
        .new_invite(personal("north", Role::Pilot), None, NOW)
        .unwrap();
    for _ in 0..5 {
        let seen = accounts.peek_invite(&secret, NOW).unwrap().unwrap();
        assert_eq!(seen.uses, 0);
        assert_eq!(seen.kind.club(), Some(&club("north")));
    }
    assert!(
        accounts
            .join_with_invite(&secret, &discord("111"), "Ace", NOW)
            .is_ok()
    );
}

#[test]
fn a_join_that_fails_uses_nothing_and_makes_nothing() {
    let accounts = Accounts::in_memory().unwrap();
    join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    // The second person's invite is good, but the sign-in they came with is Ace's.
    let (invite, secret) = accounts
        .new_invite(personal("north", Role::Director), None, NOW)
        .unwrap();
    assert_eq!(
        accounts
            .join_with_invite(&secret, &discord("111"), "Impostor", NOW)
            .unwrap_err(),
        AccountError::SignInTaken
    );
    // The invite is still whole, nobody new is a member, and Ace is still only a pilot.
    assert_eq!(accounts.peek_invite(&secret, NOW).unwrap().unwrap().uses, 0);
    let members = accounts.members(&club("north")).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(
        (members[0].0.name.as_str(), members[0].1),
        ("Ace", Role::Pilot)
    );
    assert_eq!(
        accounts
            .invites_of(&club("north"))
            .unwrap()
            .iter()
            .find(|i| i.id == invite.id)
            .unwrap()
            .uses,
        0
    );
}

#[test]
fn many_people_racing_for_the_last_place_on_an_invite_get_exactly_that_many_accounts() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::open(dir.path()).unwrap();
    let (_, secret) = accounts.new_invite(group("north", 5), None, NOW).unwrap();
    let joined = std::sync::atomic::AtomicU32::new(0);
    std::thread::scope(|scope| {
        for n in 0..24 {
            let (accounts, secret, joined) = (&accounts, &secret, &joined);
            scope.spawn(move || {
                let subject = format!("user-{n}");
                if accounts
                    .join_with_invite(secret, &discord(&subject), "Pilot", NOW)
                    .is_ok()
                {
                    joined.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(joined.load(std::sync::atomic::Ordering::SeqCst), 5);
    assert_eq!(accounts.members(&club("north")).unwrap().len(), 5);
    assert_eq!(accounts.invites_of(&club("north")).unwrap()[0].uses, 5);
}

// ── An account is not its sign-in ───────────────────────────────────────────

#[test]
fn an_account_adds_a_second_sign_in_and_then_signs_in_either_way() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    accounts.add_sign_in(&ace, &google("g-1"), NOW + 1).unwrap();
    assert_eq!(
        accounts
            .account_for_sign_in("google", "g-1")
            .unwrap()
            .unwrap()
            .id,
        ace
    );
    assert_eq!(
        accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .unwrap()
            .id,
        ace
    );
    assert_eq!(accounts.sign_ins(&ace).unwrap().len(), 2);
    // Adding it again changes nothing.
    accounts.add_sign_in(&ace, &google("g-1"), NOW + 2).unwrap();
    assert_eq!(accounts.sign_ins(&ace).unwrap().len(), 2);
}

#[test]
fn a_sign_in_that_belongs_to_one_account_cannot_be_added_to_another() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    let bee = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("222"),
        "Bee",
    );
    assert_eq!(
        accounts
            .add_sign_in(&bee, &discord("111"), NOW)
            .unwrap_err(),
        AccountError::SignInTaken
    );
    assert_eq!(
        accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .unwrap()
            .id,
        ace
    );
    assert_eq!(accounts.sign_ins(&bee).unwrap().len(), 1);
    assert_eq!(
        accounts
            .add_sign_in(&AccountId("acct_nobody".into()), &google("x"), NOW)
            .unwrap_err(),
        AccountError::NoSuchAccount
    );
}

#[test]
fn the_same_label_on_two_sign_ins_joins_nothing() {
    // Two people whose providers both call them the same thing are two people.
    let accounts = Accounts::in_memory().unwrap();
    let same = "ace@example.org";
    let one = join(
        &accounts,
        personal("north", Role::Pilot),
        &NewSignIn {
            provider: "discord",
            subject: "111",
            label: same,
        },
        "Ace",
    );
    let two = join(
        &accounts,
        personal("north", Role::Pilot),
        &NewSignIn {
            provider: "google",
            subject: "g-9",
            label: same,
        },
        "Ace",
    );
    assert_ne!(one, two);
    assert_eq!(accounts.sign_ins(&one).unwrap().len(), 1);
    assert_eq!(accounts.sign_ins(&two).unwrap().len(), 1);
}

#[test]
fn an_account_keeps_at_least_one_sign_in() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    assert_eq!(
        accounts.remove_sign_in(&ace, "discord", "111").unwrap_err(),
        AccountError::LastSignIn
    );
    accounts.add_sign_in(&ace, &google("g-1"), NOW).unwrap();
    accounts.remove_sign_in(&ace, "discord", "111").unwrap();
    assert!(
        accounts
            .account_for_sign_in("discord", "111")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        accounts.remove_sign_in(&ace, "google", "g-1").unwrap_err(),
        AccountError::LastSignIn
    );
    // Removing what the account does not have is nothing, and never someone else's.
    let bee = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("222"),
        "Bee",
    );
    accounts.remove_sign_in(&ace, "discord", "222").unwrap();
    assert_eq!(
        accounts
            .account_for_sign_in("discord", "222")
            .unwrap()
            .unwrap()
            .id,
        bee
    );
}

#[test]
fn a_label_can_be_brought_up_to_date_and_a_name_changed() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "",
    );
    // A provider that gave no usable name still makes an account.
    assert_eq!(accounts.account(&ace).unwrap().unwrap().name, "Pilot");
    accounts
        .refresh_label("discord", "111", " ace_fpv ")
        .unwrap();
    assert_eq!(accounts.sign_ins(&ace).unwrap()[0].label, "ace_fpv");
    assert_eq!(accounts.rename(&ace, " Ace ").unwrap().name, "Ace");
    for bad in ["", "  ", "two\nlines", &"x".repeat(81)] {
        assert!(
            matches!(accounts.rename(&ace, bad), Err(AccountError::Invalid(_))),
            "{bad:?}"
        );
    }
    assert_eq!(accounts.account(&ace).unwrap().unwrap().name, "Ace");
    assert_eq!(
        accounts
            .rename(&AccountId("acct_nobody".into()), "X")
            .unwrap_err(),
        AccountError::NoSuchAccount
    );
}

// ── Sessions ────────────────────────────────────────────────────────────────

#[test]
fn a_session_is_good_until_it_runs_out_or_is_ended() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    let session = accounts.new_session(&ace, NOW).unwrap();
    assert_eq!(
        accounts.session(&session, NOW).unwrap().unwrap().account,
        ace
    );
    assert!(
        accounts
            .session(&session, NOW + SESSION_MS - 1)
            .unwrap()
            .is_some()
    );
    assert!(
        accounts
            .session(&session, NOW + SESSION_MS)
            .unwrap()
            .is_none()
    );

    for wrong in [
        "",
        "nope",
        &session[..session.len() - 1],
        &format!("{session}x"),
    ] {
        assert!(accounts.session(wrong, NOW).unwrap().is_none(), "{wrong:?}");
    }

    accounts.end_session(&session).unwrap();
    assert!(accounts.session(&session, NOW).unwrap().is_none());
    accounts.end_session(&session).unwrap();
    assert_eq!(
        accounts
            .new_session(&AccountId("acct_nobody".into()), NOW)
            .unwrap_err(),
        AccountError::NoSuchAccount
    );
}

#[test]
fn each_session_is_its_own_and_all_can_be_ended_at_once() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("111"),
        "Ace",
    );
    let bee = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("222"),
        "Bee",
    );
    let phone = accounts.new_session(&ace, NOW).unwrap();
    let laptop = accounts.new_session(&ace, NOW).unwrap();
    let other = accounts.new_session(&bee, NOW).unwrap();
    assert_ne!(phone, laptop);

    accounts.end_session(&phone).unwrap();
    assert!(accounts.session(&phone, NOW).unwrap().is_none());
    assert!(accounts.session(&laptop, NOW).unwrap().is_some());

    accounts.end_sessions_of(&ace).unwrap();
    assert!(accounts.session(&laptop, NOW).unwrap().is_none());
    assert_eq!(accounts.session(&other, NOW).unwrap().unwrap().account, bee);
}

#[test]
fn a_session_shows_the_roles_as_they_are_now_not_as_they_were() {
    let accounts = Accounts::in_memory().unwrap();
    let boss = join(
        &accounts,
        personal("north", Role::Director),
        &discord("1"),
        "Boss",
    );
    let ace = join(
        &accounts,
        personal("north", Role::Director),
        &discord("2"),
        "Ace",
    );
    let session = accounts.new_session(&ace, NOW).unwrap();
    assert!(
        accounts
            .session(&session, NOW)
            .unwrap()
            .unwrap()
            .may(Act::SetRole(&club("north")))
    );
    accounts
        .set_role(&club("north"), &ace, Role::Pilot)
        .unwrap();
    assert!(
        !accounts
            .session(&session, NOW)
            .unwrap()
            .unwrap()
            .may(Act::SetRole(&club("north")))
    );
    accounts.remove_member(&club("north"), &ace).unwrap();
    assert!(
        accounts
            .session(&session, NOW)
            .unwrap()
            .unwrap()
            .clubs
            .is_empty()
    );
    let _ = boss;
}

// ── Roles are per club ──────────────────────────────────────────────────────

#[test]
fn one_account_has_a_different_role_in_each_club() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Director),
        &discord("111"),
        "Ace",
    );
    let (_, south) = accounts.new_invite(group("south", 10), None, NOW).unwrap();
    accounts.accept_invite(&south, &ace, NOW).unwrap();
    let (_, east) = accounts
        .new_invite(personal("east", Role::Approver), None, NOW)
        .unwrap();
    accounts.accept_invite(&east, &ace, NOW).unwrap();

    let who = standing(&accounts, &ace);
    assert_eq!(who.role_in(&club("north")), Some(Role::Director));
    assert_eq!(who.role_in(&club("south")), Some(Role::Pilot));
    assert_eq!(who.role_in(&club("east")), Some(Role::Approver));
    assert_eq!(who.role_in(&club("west")), None);
    // One account, not three.
    assert_eq!(accounts.sign_ins(&ace).unwrap().len(), 1);
}

#[test]
fn what_each_role_may_do_and_only_in_its_own_club() {
    let accounts = Accounts::in_memory().unwrap();
    let director = join(
        &accounts,
        personal("north", Role::Director),
        &discord("1"),
        "Director",
    );
    let approver = join(
        &accounts,
        personal("north", Role::Approver),
        &discord("2"),
        "Approver",
    );
    let pilot = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("3"),
        "Pilot",
    );
    let owner = join(&accounts, owner_invite(), &discord("4"), "Owner");
    let (north, south) = (club("north"), club("south"));

    // In order: invite, see members, set a role, approve a claim.
    fn may_in(s: &Standing, c: &ClubId) -> [bool; 4] {
        [
            s.may(Act::Invite(c, Role::Pilot)),
            s.may(Act::SeeMembers(c)),
            s.may(Act::SetRole(c)),
            s.may(Act::ApproveClaim(c)),
        ]
    }
    let table: [(&AccountId, [bool; 4]); 4] = [
        (&director, [true, true, true, true]),
        (&approver, [false, false, false, true]),
        (&pilot, [false, false, false, false]),
        (&owner, [true, true, true, true]),
    ];
    for (who, allowed) in table {
        let s = standing(&accounts, who);
        assert_eq!(may_in(&s, &north), allowed, "{} in north", who.0);
        // In a club they have no role in, only the owner may do anything.
        assert_eq!(may_in(&s, &south), [s.owner; 4], "{} in south", who.0);
        assert_eq!(s.may(Act::RunServer), s.owner);
    }
}

#[test]
fn a_director_changes_roles_and_a_club_is_never_left_with_no_director() {
    let accounts = Accounts::in_memory().unwrap();
    let north = club("north");
    let boss = join(
        &accounts,
        personal("north", Role::Director),
        &discord("1"),
        "Boss",
    );
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("2"),
        "Ace",
    );

    // The only director cannot step down or be removed.
    assert_eq!(
        accounts.set_role(&north, &boss, Role::Pilot).unwrap_err(),
        AccountError::LastDirector
    );
    assert_eq!(
        accounts
            .set_role(&north, &boss, Role::Approver)
            .unwrap_err(),
        AccountError::LastDirector
    );
    assert_eq!(
        accounts.remove_member(&north, &boss).unwrap_err(),
        AccountError::LastDirector
    );
    assert_eq!(
        standing(&accounts, &boss).role_in(&north),
        Some(Role::Director)
    );

    // With a second director, either may.
    accounts.set_role(&north, &ace, Role::Director).unwrap();
    accounts.set_role(&north, &boss, Role::Pilot).unwrap();
    assert_eq!(
        accounts.set_role(&north, &ace, Role::Pilot).unwrap_err(),
        AccountError::LastDirector
    );
    accounts.remove_member(&north, &boss).unwrap();
    assert_eq!(accounts.members(&north).unwrap().len(), 1);

    // Someone who is not a member has no role to change.
    assert_eq!(
        accounts.set_role(&north, &boss, Role::Pilot).unwrap_err(),
        AccountError::NotAMember
    );
    assert_eq!(
        accounts.remove_member(&north, &boss).unwrap_err(),
        AccountError::NotAMember
    );
    assert_eq!(
        accounts
            .set_role(&club("south"), &ace, Role::Pilot)
            .unwrap_err(),
        AccountError::NotAMember
    );
    // Their account is still there, with its sign-in.
    assert!(
        accounts
            .account_for_sign_in("discord", "1")
            .unwrap()
            .is_some()
    );
}

#[test]
fn the_last_director_rule_is_per_club() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Director),
        &discord("1"),
        "Ace",
    );
    let (_, south) = accounts
        .new_invite(personal("south", Role::Director), None, NOW)
        .unwrap();
    accounts.accept_invite(&south, &ace, NOW).unwrap();
    join(
        &accounts,
        personal("south", Role::Director),
        &discord("2"),
        "Bee",
    );
    // South has two directors, north has one: Ace may step down in south only.
    accounts
        .set_role(&club("south"), &ace, Role::Pilot)
        .unwrap();
    assert_eq!(
        accounts
            .set_role(&club("north"), &ace, Role::Pilot)
            .unwrap_err(),
        AccountError::LastDirector
    );
}

#[test]
fn an_invite_never_takes_a_role_away() {
    let accounts = Accounts::in_memory().unwrap();
    let boss = join(
        &accounts,
        personal("north", Role::Director),
        &discord("1"),
        "Boss",
    );
    // The director opens the club's own group link.
    let (_, secret) = accounts
        .new_invite(group("north", 10), Some(&boss), NOW)
        .unwrap();
    accounts.accept_invite(&secret, &boss, NOW).unwrap();
    assert_eq!(
        standing(&accounts, &boss).role_in(&club("north")),
        Some(Role::Director)
    );

    // And one does raise a role.
    let ace = join(&accounts, group("north", 10), &discord("2"), "Ace");
    let (_, up) = accounts
        .new_invite(personal("north", Role::Approver), Some(&boss), NOW)
        .unwrap();
    accounts.accept_invite(&up, &ace, NOW).unwrap();
    assert_eq!(
        standing(&accounts, &ace).role_in(&club("north")),
        Some(Role::Approver)
    );
}

#[test]
fn an_existing_account_cannot_use_an_invite_that_does_not_work() {
    let accounts = Accounts::in_memory().unwrap();
    let ace = join(
        &accounts,
        personal("north", Role::Pilot),
        &discord("1"),
        "Ace",
    );
    let (invite, secret) = accounts
        .new_invite(personal("south", Role::Director), None, NOW)
        .unwrap();
    accounts.revoke_invite(&invite.id, NOW).unwrap();
    assert_eq!(
        accounts.accept_invite(&secret, &ace, NOW).unwrap_err(),
        AccountError::InviteDoesNotWork
    );
    assert_eq!(standing(&accounts, &ace).role_in(&club("south")), None);
    let (_, good) = accounts
        .new_invite(personal("south", Role::Pilot), None, NOW)
        .unwrap();
    assert_eq!(
        accounts
            .accept_invite(&good, &AccountId("acct_nobody".into()), NOW)
            .unwrap_err(),
        AccountError::NoSuchAccount
    );
    assert_eq!(accounts.peek_invite(&good, NOW).unwrap().unwrap().uses, 0);
}

// ── Owners ──────────────────────────────────────────────────────────────────

#[test]
fn the_first_owner_is_made_with_an_invite_like_anyone_else() {
    let accounts = Accounts::in_memory().unwrap();
    assert!(!accounts.has_owner().unwrap());
    let (_, secret) = accounts.new_invite(owner_invite(), None, NOW).unwrap();
    assert!(!accounts.has_owner().unwrap(), "an invite is not an owner");
    let (owner, _, _) = accounts
        .join_with_invite(&secret, &discord("1"), "Ryan", NOW)
        .unwrap();
    assert!(accounts.has_owner().unwrap());
    let who = standing(&accounts, &owner.id);
    assert!(who.owner && who.clubs.is_empty());
    assert!(who.may(Act::RunServer) && who.may(Act::SetRole(&club("any-club"))));
    // The setup link is spent.
    assert!(
        accounts
            .join_with_invite(&secret, &discord("2"), "Second", NOW)
            .is_err()
    );
}

#[test]
fn a_pilot_invite_never_makes_an_owner() {
    let accounts = Accounts::in_memory().unwrap();
    for spec in [
        personal("north", Role::Pilot),
        personal("north", Role::Director),
        group("north", 5),
    ] {
        let id = join(
            &accounts,
            spec,
            &discord(&format!("u{}", accounts.all_invites().unwrap().len())),
            "X",
        );
        assert!(!standing(&accounts, &id).owner);
        assert!(!standing(&accounts, &id).may(Act::RunServer));
    }
    assert!(!accounts.has_owner().unwrap());
}

// ── Kept ────────────────────────────────────────────────────────────────────

#[test]
fn everything_is_there_when_the_file_is_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let (ace, session, open_invite) = {
        let accounts = Accounts::open(dir.path()).unwrap();
        let (_, secret) = accounts
            .new_invite(personal("north", Role::Director), None, NOW)
            .unwrap();
        let (ace, _, session) = accounts
            .join_with_invite(&secret, &discord("111"), "Ace", NOW)
            .unwrap();
        accounts.add_sign_in(&ace.id, &google("g-1"), NOW).unwrap();
        let (_, open_invite) = accounts
            .new_invite(group("north", 4), Some(&ace.id), NOW)
            .unwrap();
        (ace.id, session, open_invite)
    };
    let accounts = Accounts::open(dir.path()).unwrap();
    assert_eq!(
        accounts
            .session(&session, NOW + DAY)
            .unwrap()
            .unwrap()
            .account,
        ace
    );
    assert_eq!(accounts.sign_ins(&ace).unwrap().len(), 2);
    assert_eq!(
        standing(&accounts, &ace).role_in(&club("north")),
        Some(Role::Director)
    );
    let invite = accounts
        .peek_invite(&open_invite, NOW + DAY)
        .unwrap()
        .unwrap();
    assert_eq!(invite.created_by, Some(ace.clone()));
    assert_eq!(accounts.invites_of(&club("north")).unwrap().len(), 2);
    assert!(accounts.invites_of(&club("south")).unwrap().is_empty());
}

#[test]
fn members_are_listed_by_name_whatever_the_case() {
    let accounts = Accounts::in_memory().unwrap();
    for (n, name) in ["charlie", "Alpha", "bravo"].iter().enumerate() {
        join(
            &accounts,
            personal("north", Role::Pilot),
            &discord(&n.to_string()),
            name,
        );
    }
    let names: Vec<String> = accounts
        .members(&club("north"))
        .unwrap()
        .into_iter()
        .map(|(a, _)| a.name)
        .collect();
    assert_eq!(names, ["Alpha", "bravo", "charlie"]);
}
