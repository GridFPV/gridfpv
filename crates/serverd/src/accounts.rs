//! Accounts, how they sign in, their roles, and invites (docs/primitives.md §13.2, §13.7,
//! §13.8). All of it is one SQLite file, `accounts.sqlite`, in the data folder.
//!
//! The rules this module keeps, whatever asks:
//!
//! - **An account is not its sign-in.** An account is this server's own record. A sign-in
//!   (a Discord or Google identity) is a key attached to it, and an account can hold several.
//!   A sign-in belongs to one account only, and an account always keeps at least one.
//! - **An account is made only with an invite.** Nobody gets one by turning up.
//! - **Two sign-ins are never joined because something about them matches.** A sign-in is
//!   attached to an account only by that account, signed in, adding it.
//! - **Roles are per club.** The same account can be a director in one club and a pilot in
//!   another. A server owner is separate, and may do anything in any club.
//! - **A club is never left with no director** by a change of role.
//! - **Secrets are shown once.** A session and an invite are kept only as a hash.
//! - **A club's pilot has one holder at most.** A pilot of a club (the id a Director gave
//!   them) is held by one account, which is how results reach an account (§13.7). An
//!   account that asks for a pilot does not hold it until someone allowed says yes.
//!   **Knowing a pilot's id is never enough.**
//! - **Only a member of a club holds or asks for one of its pilots.** Leaving the club
//!   lets go of them.
//!
//! Who may do what is in [`Standing::may`]. Nothing here speaks HTTP.

use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::book::ClubId;

/// The file the accounts are kept in, under the data folder.
pub const ACCOUNTS_FILE: &str = "accounts.sqlite";

/// How long a session lasts from when it is made, in milliseconds: thirty days.
pub const SESSION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// How long a browser has to come back from a provider, in milliseconds: ten minutes.
pub const PENDING_MS: i64 = 10 * 60 * 1000;

/// A sign-in that is under way: a browser has been sent to a provider and has not come
/// back yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSignIn {
    pub provider: String,
    /// The secret half of the proof that the browser that comes back is the one that left
    /// (PKCE).
    pub verifier: String,
    /// The invite the person came with, if any.
    pub invite: Option<String>,
    /// The account this sign-in is being added to, when that is what is being done.
    pub add_to: Option<AccountId>,
}

/// What an invite link's secret starts with.
pub const INVITE_PREFIX: &str = "gfi_";

/// What a re-connect link's secret starts with, so it is told from an invite's.
pub const RECONNECT_PREFIX: &str = "gfr_";

/// How long a re-connect link works, in milliseconds: seven days.
pub const RECONNECT_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The longest a name or label may be.
const NAME_MAX: usize = 80;

/// Identifies an account on this server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

/// What a person may do in one club (§13.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    /// Flies with the club.
    Pilot,
    /// May also approve a pilot's claim to results.
    Approver,
    /// Runs the club: pushes events, invites, approves claims, hands out roles.
    Director,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pilot => "pilot",
            Self::Approver => "approver",
            Self::Director => "director",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "pilot" => Self::Pilot,
            "approver" => Self::Approver,
            "director" => Self::Director,
            _ => return None,
        })
    }
}

/// An account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    /// What the person is called here. They choose it.
    pub name: String,
    pub created_at: i64,
}

/// One way an account signs in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignIn {
    /// Which provider: `discord`, `google`.
    pub provider: String,
    /// Who the provider says this is. Its own id for the person, which does not change.
    pub subject: String,
    /// What to show for it, such as the Discord username. May change; never used to match.
    pub label: String,
    pub added_at: i64,
}

/// An account's place in one club.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    pub club: ClubId,
    pub role: Role,
}

/// What an account may do, worked out once for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub account: AccountId,
    pub owner: bool,
    pub clubs: Vec<Membership>,
}

/// Something a person may ask to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act<'a> {
    /// Make or take back an invite to a club, for the role given.
    Invite(&'a ClubId, Role),
    /// See a club's members and invites.
    SeeMembers(&'a ClubId),
    /// Give a member of a club a role, or remove them from it.
    SetRole(&'a ClubId),
    /// Approve a pilot's claim to results in a club.
    ApproveClaim(&'a ClubId),
    /// See a club's pilots, and ask for one of them.
    SeePilots(&'a ClubId),
    /// Say which account holds a pilot of a club, with nobody having asked.
    GivePilot(&'a ClubId),
    /// Run a club's own things on the server: its name, its link tokens, and which of its
    /// events are listed or removed (§13.6, §13.9).
    RunClub(&'a ClubId),
    /// Look after the server itself: make clubs and owners.
    RunServer,
}

impl Standing {
    pub fn role_in(&self, club: &ClubId) -> Option<Role> {
        self.clubs.iter().find(|m| &m.club == club).map(|m| m.role)
    }

    /// Whether this account may do it. An owner may do everything (§13.2).
    pub fn may(&self, act: Act<'_>) -> bool {
        if self.owner {
            return true;
        }
        let director = |club| self.role_in(club) == Some(Role::Director);
        match act {
            // Only a club's directors invite to it and hand out its roles.
            Act::Invite(club, _)
            | Act::SeeMembers(club)
            | Act::SetRole(club)
            | Act::GivePilot(club)
            | Act::RunClub(club) => director(club),
            Act::SeePilots(club) => self.role_in(club).is_some(),
            Act::ApproveClaim(club) => {
                matches!(self.role_in(club), Some(Role::Director | Role::Approver))
            }
            Act::RunServer => false,
        }
    }
}

/// What an invite is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InviteKind {
    /// Makes the account an owner of this server. Works once.
    Owner,
    /// Joins a club with a role. Works once. `pilot` is the club's pilot the invite is
    /// for, when it is for one: using it will claim that pilot's results (§13.8).
    Personal {
        club: ClubId,
        role: Role,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pilot: Option<String>,
    },
    /// Joins a club as a pilot. One link for many people, with a limit and an end.
    Group { club: ClubId },
}

impl InviteKind {
    pub fn club(&self) -> Option<&ClubId> {
        match self {
            Self::Owner => None,
            Self::Personal { club, .. } | Self::Group { club } => Some(club),
        }
    }
    /// The role the invite gives in its club.
    pub fn role(&self) -> Option<Role> {
        match self {
            Self::Owner => None,
            Self::Personal { role, .. } => Some(*role),
            Self::Group { .. } => Some(Role::Pilot),
        }
    }
}

/// An invite, as it is kept. The link's secret is not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// A short id to name the invite by. Not a secret.
    pub id: String,
    pub kind: InviteKind,
    /// What whoever made it called it, such as the pilot's callsign or "Club Discord".
    pub label: String,
    pub max_uses: u32,
    pub uses: u32,
    /// When it stops working, in milliseconds since 1970. `None` for no end.
    pub expires_at: Option<i64>,
    pub created_by: Option<AccountId>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

impl Invite {
    /// Whether it can still be used at `now`.
    pub fn stands(&self, now: i64) -> bool {
        self.revoked_at.is_none()
            && self.uses < self.max_uses
            && self.expires_at.is_none_or(|end| now < end)
    }
}

/// What to make an invite with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInvite {
    pub kind: InviteKind,
    pub label: String,
    /// For a group invite. A personal or owner invite always works once.
    pub max_uses: u32,
    pub expires_at: Option<i64>,
}

/// A pilot of a club, as the server knows them: the id a Director gave them and the
/// callsign it sent. Kept from every event header and every list of pilots a Director
/// sends, and never removed, so results from long ago still have a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClubPilot {
    pub pilot: String,
    pub callsign: String,
    pub color: Option<String>,
}

/// How an account came to hold a pilot, or that it has only asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimHow {
    /// The account asked. Nobody has said yes.
    Asked,
    /// A director said so.
    Director,
    /// The account used a personal invite made for this pilot.
    Invite,
    /// The account asked and someone allowed said yes.
    Approved,
}

impl ClaimHow {
    fn as_str(self) -> &'static str {
        match self {
            Self::Asked => "asked",
            Self::Director => "director",
            Self::Invite => "invite",
            Self::Approved => "approved",
        }
    }
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "asked" => Self::Asked,
            "director" => Self::Director,
            "invite" => Self::Invite,
            "approved" => Self::Approved,
            _ => return None,
        })
    }
    /// Whether the account holds the pilot, rather than having asked for it.
    pub fn holds(self) -> bool {
        self != Self::Asked
    }
}

/// A re-connect link, as it is kept: a link an owner makes for someone who can no longer
/// sign in (§13.8). Whoever opens it and signs in gets that sign-in put on the account.
/// The link's secret is not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconnect {
    /// A short id to name it by. Not a secret.
    pub id: String,
    pub account: AccountId,
    pub created_by: Option<AccountId>,
    pub created_at: i64,
    /// When it stops working, in milliseconds since 1970.
    pub expires_at: i64,
}

/// An account's tie to a pilot of a club: it holds the pilot, or has asked to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub club: ClubId,
    pub pilot: String,
    pub account: Account,
    pub how: ClaimHow,
    pub at: i64,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccountError {
    #[error("This club has no such pilot.")]
    NoSuchPilot,
    #[error("That pilot already belongs to an account.")]
    PilotTaken,
    #[error("Nobody has asked for that.")]
    NoSuchClaim,
    #[error("That invite does not work. It may have been used, taken back, or have run out.")]
    InviteDoesNotWork,
    #[error(
        "That re-connect link does not work. It may have been used, taken back, or have run out."
    )]
    ReconnectDoesNotWork,
    #[error("That sign-in already belongs to an account.")]
    SignInTaken,
    #[error("An account must keep at least one way to sign in.")]
    LastSignIn,
    #[error("A club must keep at least one director.")]
    LastDirector,
    #[error("There is no such account.")]
    NoSuchAccount,
    #[error("That account is not a member of the club.")]
    NotAMember,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the accounts could not be read or written: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for AccountError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

type Result<T> = std::result::Result<T, AccountError>;

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the operating system gives random bytes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A name or label, trimmed, or why it will not do.
fn clean(text: &str, empty: &'static str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AccountError::Invalid(empty));
    }
    if text.chars().count() > NAME_MAX || text.chars().any(char::is_control) {
        return Err(AccountError::Invalid(
            "A name is 80 characters at most, on one line.",
        ));
    }
    Ok(text.to_owned())
}

/// Every account on this server. See the module docs.
pub struct Accounts {
    conn: Mutex<Connection>,
}

impl Accounts {
    /// Open the accounts in a data folder, making the file if it is new.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| AccountError::Storage(e.to_string()))?;
        Self::with(Connection::open(dir.join(ACCOUNTS_FILE))?)
    }

    /// Accounts kept in memory only, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS accounts (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sign_ins (
                provider TEXT NOT NULL,
                subject  TEXT NOT NULL,
                account  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                label    TEXT NOT NULL,
                added_at INTEGER NOT NULL,
                PRIMARY KEY (provider, subject)
            );
            CREATE TABLE IF NOT EXISTS sessions (
                hash       TEXT PRIMARY KEY,
                account    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS owners (
                account TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS members (
                account  TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                club     TEXT NOT NULL,
                role     TEXT NOT NULL,
                added_at INTEGER NOT NULL,
                PRIMARY KEY (account, club)
            );
            CREATE TABLE IF NOT EXISTS pending_sign_ins (
                hash       TEXT PRIMARY KEY,
                provider   TEXT NOT NULL,
                verifier   TEXT NOT NULL,
                invite     TEXT,
                add_to     TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS invites (
                id         TEXT PRIMARY KEY,
                hash       TEXT NOT NULL UNIQUE,
                kind       TEXT NOT NULL,
                label      TEXT NOT NULL,
                max_uses   INTEGER NOT NULL,
                uses       INTEGER NOT NULL DEFAULT 0,
                expires_at INTEGER,
                created_by TEXT,
                created_at INTEGER NOT NULL,
                revoked_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS reconnects (
                id         TEXT PRIMARY KEY,
                hash       TEXT NOT NULL UNIQUE,
                account    TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                created_by TEXT,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                used_at    INTEGER,
                revoked_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS primary_clubs (
                account TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
                club    TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS club_pilots (
                club     TEXT NOT NULL,
                pilot    TEXT NOT NULL,
                callsign TEXT NOT NULL,
                color    TEXT,
                seen_at  INTEGER NOT NULL,
                PRIMARY KEY (club, pilot)
            );
            CREATE TABLE IF NOT EXISTS claims (
                club    TEXT NOT NULL,
                pilot   TEXT NOT NULL,
                account TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
                held    INTEGER NOT NULL,
                how     TEXT NOT NULL,
                at      INTEGER NOT NULL,
                by      TEXT,
                PRIMARY KEY (club, pilot, account)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS claims_one_holder
                ON claims (club, pilot) WHERE held = 1;",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── Reading ─────────────────────────────────────────────────────────────

    fn account_in(conn: &Connection, id: &AccountId) -> Result<Option<Account>> {
        Ok(conn
            .query_row(
                "SELECT id, name, created_at FROM accounts WHERE id = ?1",
                params![id.0],
                |row| {
                    Ok(Account {
                        id: AccountId(row.get(0)?),
                        name: row.get(1)?,
                        created_at: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn account(&self, id: &AccountId) -> Result<Option<Account>> {
        Self::account_in(&self.lock(), id)
    }

    /// How an account signs in, oldest first.
    pub fn sign_ins(&self, id: &AccountId) -> Result<Vec<SignIn>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT provider, subject, label, added_at FROM sign_ins
             WHERE account = ?1 ORDER BY added_at, provider, subject",
        )?;
        let rows = stmt.query_map(params![id.0], |row| {
            Ok(SignIn {
                provider: row.get(0)?,
                subject: row.get(1)?,
                label: row.get(2)?,
                added_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn standing_in(conn: &Connection, id: &AccountId) -> Result<Option<Standing>> {
        if Self::account_in(conn, id)?.is_none() {
            return Ok(None);
        }
        let owner: bool = conn
            .query_row(
                "SELECT 1 FROM owners WHERE account = ?1",
                params![id.0],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let mut stmt =
            conn.prepare("SELECT club, role FROM members WHERE account = ?1 ORDER BY club")?;
        let rows = stmt.query_map(params![id.0], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut clubs = Vec::new();
        for row in rows {
            let (club, role) = row?;
            // A role this build does not know gives nothing, rather than something.
            if let Some(role) = Role::parse(&role) {
                clubs.push(Membership {
                    club: ClubId(club),
                    role,
                });
            }
        }
        Ok(Some(Standing {
            account: id.clone(),
            owner,
            clubs,
        }))
    }

    /// What an account may do. `None` when there is no such account.
    pub fn standing(&self, id: &AccountId) -> Result<Option<Standing>> {
        Self::standing_in(&self.lock(), id)
    }

    /// Mark a club as the one an account's front page opens on (§13.2). Whether the
    /// account may pick it is the caller's to check.
    pub fn set_primary_club(&self, account: &AccountId, club: &ClubId) -> Result<()> {
        let conn = self.lock();
        if Self::account_in(&conn, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        conn.execute(
            "INSERT INTO primary_clubs (account, club) VALUES (?1, ?2)
             ON CONFLICT(account) DO UPDATE SET club = excluded.club",
            params![account.0, club.0],
        )?;
        Ok(())
    }

    /// The club an account marked as primary, if it marked one. It may no longer be a
    /// club the account is in: the caller checks.
    pub fn primary_club(&self, account: &AccountId) -> Result<Option<ClubId>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT club FROM primary_clubs WHERE account = ?1",
                params![account.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(ClubId))
    }

    /// The clubs an account is in, the one it joined first coming first.
    pub fn clubs_joined(&self, account: &AccountId) -> Result<Vec<ClubId>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT club FROM members WHERE account = ?1 ORDER BY added_at, rowid")?;
        let rows = stmt.query_map(params![account.0], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(ClubId)
            .collect())
    }

    /// Whether this server has an owner yet.
    pub fn has_owner(&self) -> Result<bool> {
        Ok(self
            .lock()
            .query_row("SELECT 1 FROM owners LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// A club's members and their roles, by name.
    pub fn members(&self, club: &ClubId) -> Result<Vec<(Account, Role)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, a.created_at, m.role FROM members m
             JOIN accounts a ON a.id = m.account
             WHERE m.club = ?1 ORDER BY a.name COLLATE NOCASE, a.id",
        )?;
        let rows = stmt.query_map(params![club.0], |row| {
            Ok((
                Account {
                    id: AccountId(row.get(0)?),
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                },
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (account, role) = row?;
            if let Some(role) = Role::parse(&role) {
                out.push((account, role));
            }
        }
        Ok(out)
    }

    // ── Sessions ────────────────────────────────────────────────────────────

    /// Start a session for an account. Returns the session's secret, which is not kept.
    pub fn new_session(&self, account: &AccountId, now: i64) -> Result<String> {
        let conn = self.lock();
        Self::new_session_in(&conn, account, now)
    }

    fn new_session_in(conn: &Connection, account: &AccountId, now: i64) -> Result<String> {
        if Self::account_in(conn, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        let secret = random(32);
        conn.execute(
            "INSERT INTO sessions (hash, account, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![hash(&secret), account.0, now, now + SESSION_MS],
        )?;
        // Sessions that ran out are of no use to anyone.
        conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])?;
        Ok(secret)
    }

    /// Whose session a secret is, if it is one that has not run out or been ended.
    pub fn session(&self, secret: &str, now: i64) -> Result<Option<Standing>> {
        if secret.is_empty() {
            return Ok(None);
        }
        let conn = self.lock();
        let account: Option<String> = conn
            .query_row(
                "SELECT account FROM sessions WHERE hash = ?1 AND expires_at > ?2",
                params![hash(secret), now],
                |row| row.get(0),
            )
            .optional()?;
        match account {
            Some(id) => Self::standing_in(&conn, &AccountId(id)),
            None => Ok(None),
        }
    }

    /// End a session. Ending one that is not there is fine.
    pub fn end_session(&self, secret: &str) -> Result<()> {
        self.lock().execute(
            "DELETE FROM sessions WHERE hash = ?1",
            params![hash(secret)],
        )?;
        Ok(())
    }

    /// End every session of an account: signed out everywhere.
    pub fn end_sessions_of(&self, account: &AccountId) -> Result<()> {
        self.lock().execute(
            "DELETE FROM sessions WHERE account = ?1",
            params![account.0],
        )?;
        Ok(())
    }

    // ── A sign-in that is under way ─────────────────────────────────────────

    /// Note that a browser has been sent to a provider. `state` is the value the provider
    /// will hand back; it is kept only as a hash.
    pub fn start_sign_in(&self, state: &str, pending: &PendingSignIn, now: i64) -> Result<()> {
        let conn = self.lock();
        // One that was never finished is of no use after a while.
        conn.execute(
            "DELETE FROM pending_sign_ins WHERE created_at <= ?1",
            params![now - PENDING_MS],
        )?;
        conn.execute(
            "INSERT INTO pending_sign_ins (hash, provider, verifier, invite, add_to, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                hash(state),
                pending.provider,
                pending.verifier,
                pending.invite,
                pending.add_to.as_ref().map(|a| a.0.clone()),
                now
            ],
        )?;
        Ok(())
    }

    /// Take the sign-in a `state` belongs to. It is given once: a second ask, a `state`
    /// that was never made, and one from too long ago all get `None`.
    pub fn take_sign_in(&self, state: &str, now: i64) -> Result<Option<PendingSignIn>> {
        if state.is_empty() {
            return Ok(None);
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let found = tx
            .query_row(
                "SELECT provider, verifier, invite, add_to, created_at FROM pending_sign_ins
                 WHERE hash = ?1",
                params![hash(state)],
                |row| {
                    Ok((
                        PendingSignIn {
                            provider: row.get(0)?,
                            verifier: row.get(1)?,
                            invite: row.get(2)?,
                            add_to: row.get::<_, Option<String>>(3)?.map(AccountId),
                        },
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?;
        tx.execute(
            "DELETE FROM pending_sign_ins WHERE hash = ?1",
            params![hash(state)],
        )?;
        tx.commit()?;
        Ok(found
            .filter(|(_, made)| now - made < PENDING_MS)
            .map(|(pending, _)| pending))
    }

    // ── Signing in ──────────────────────────────────────────────────────────

    /// The account a sign-in belongs to. `None` when no account has it: that person has
    /// no account here, whatever else about them is known.
    pub fn account_for_sign_in(&self, provider: &str, subject: &str) -> Result<Option<Account>> {
        let conn = self.lock();
        let id: Option<String> = conn
            .query_row(
                "SELECT account FROM sign_ins WHERE provider = ?1 AND subject = ?2",
                params![provider, subject],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => Self::account_in(&conn, &AccountId(id)),
            None => Ok(None),
        }
    }

    /// Note what a sign-in is shown as now. The provider may have changed it.
    pub fn refresh_label(&self, provider: &str, subject: &str, label: &str) -> Result<()> {
        self.lock().execute(
            "UPDATE sign_ins SET label = ?3 WHERE provider = ?1 AND subject = ?2",
            params![provider, subject, label.trim()],
        )?;
        Ok(())
    }

    /// Attach another sign-in to an account. This is the only way two sign-ins come to
    /// belong to one account: the account, signed in, adds it.
    pub fn add_sign_in(
        &self,
        account: &AccountId,
        sign_in: &NewSignIn<'_>,
        now: i64,
    ) -> Result<()> {
        let conn = self.lock();
        if Self::account_in(&conn, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        Self::add_sign_in_in(&conn, account, sign_in, now)
    }

    fn add_sign_in_in(
        conn: &Connection,
        account: &AccountId,
        sign_in: &NewSignIn<'_>,
        now: i64,
    ) -> Result<()> {
        if sign_in.provider.trim().is_empty() || sign_in.subject.is_empty() {
            return Err(AccountError::Invalid(
                "A sign-in needs a provider and an id.",
            ));
        }
        let taken: Option<String> = conn
            .query_row(
                "SELECT account FROM sign_ins WHERE provider = ?1 AND subject = ?2",
                params![sign_in.provider, sign_in.subject],
                |row| row.get(0),
            )
            .optional()?;
        match taken {
            // Adding what the account already has changes nothing.
            Some(owner) if owner == account.0 => Ok(()),
            Some(_) => Err(AccountError::SignInTaken),
            None => {
                conn.execute(
                    "INSERT INTO sign_ins (provider, subject, account, label, added_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        sign_in.provider,
                        sign_in.subject,
                        account.0,
                        sign_in.label.trim(),
                        now
                    ],
                )?;
                Ok(())
            }
        }
    }

    /// Take a sign-in off an account. The last one stays.
    pub fn remove_sign_in(&self, account: &AccountId, provider: &str, subject: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sign_ins WHERE account = ?1",
            params![account.0],
            |row| row.get(0),
        )?;
        let has: bool = tx
            .query_row(
                "SELECT 1 FROM sign_ins WHERE account = ?1 AND provider = ?2 AND subject = ?3",
                params![account.0, provider, subject],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !has {
            return Ok(());
        }
        if count <= 1 {
            return Err(AccountError::LastSignIn);
        }
        tx.execute(
            "DELETE FROM sign_ins WHERE account = ?1 AND provider = ?2 AND subject = ?3",
            params![account.0, provider, subject],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Getting back in ─────────────────────────────────────────────────────

    /// Every account, by name. For the command line, where an owner picks one to help.
    pub fn all_accounts(&self) -> Result<Vec<Account>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, created_at FROM accounts ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Account {
                id: AccountId(row.get(0)?),
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Take a sign-in off someone else's account, as an owner does for a person who lost
    /// it or had it stolen (§13.8). Unlike [`Self::remove_sign_in`], the last one may go:
    /// the owner then sends a re-connect link. The account is signed out everywhere, so
    /// whoever held the sign-in is out at once. `false` when the account has no such
    /// sign-in.
    pub fn drop_sign_in(&self, account: &AccountId, provider: &str, subject: &str) -> Result<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if Self::account_in(&tx, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        let gone = tx.execute(
            "DELETE FROM sign_ins WHERE account = ?1 AND provider = ?2 AND subject = ?3",
            params![account.0, provider, subject],
        )? > 0;
        if gone {
            tx.execute(
                "DELETE FROM sessions WHERE account = ?1",
                params![account.0],
            )?;
        }
        tx.commit()?;
        Ok(gone)
    }

    fn reconnect_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<Reconnect> {
        Ok(Reconnect {
            id: row.get(0)?,
            account: AccountId(row.get(1)?),
            created_by: row.get::<_, Option<String>>(2)?.map(AccountId),
            created_at: row.get(3)?,
            expires_at: row.get(4)?,
        })
    }

    const RECONNECT_COLUMNS: &'static str = "id, account, created_by, created_at, expires_at";
    /// What makes a kept link one that still works at `?now`.
    const RECONNECT_STANDS: &'static str =
        "used_at IS NULL AND revoked_at IS NULL AND expires_at >";

    /// Make a re-connect link for an account. Returns its record and **the link's secret,
    /// which is not kept**. An account has one such link at a time: making one takes back
    /// any before it.
    ///
    /// Whether the maker may is the caller's to check ([`Standing::may`]).
    pub fn new_reconnect(
        &self,
        account: &AccountId,
        by: Option<&AccountId>,
        now: i64,
    ) -> Result<(Reconnect, String)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if Self::account_in(&tx, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        tx.execute(
            "UPDATE reconnects SET revoked_at = ?2
             WHERE account = ?1 AND used_at IS NULL AND revoked_at IS NULL",
            params![account.0, now],
        )?;
        // Old links are of no use to anyone once they have long run out.
        tx.execute(
            "DELETE FROM reconnects WHERE expires_at <= ?1",
            params![now - RECONNECT_MS],
        )?;
        let id = loop {
            let id = random(6);
            let taken = tx
                .query_row(
                    "SELECT 1 FROM reconnects WHERE id = ?1",
                    params![id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !taken {
                break id;
            }
        };
        let secret = format!("{RECONNECT_PREFIX}{}", random(24));
        let link = Reconnect {
            id,
            account: account.clone(),
            created_by: by.cloned(),
            created_at: now,
            expires_at: now + RECONNECT_MS,
        };
        tx.execute(
            "INSERT INTO reconnects (id, hash, account, created_by, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                link.id,
                hash(&secret),
                link.account.0,
                link.created_by.as_ref().map(|a| a.0.clone()),
                link.created_at,
                link.expires_at
            ],
        )?;
        tx.commit()?;
        Ok((link, secret))
    }

    fn reconnect_by_secret(conn: &Connection, secret: &str, now: i64) -> Result<Option<Reconnect>> {
        if !secret.starts_with(RECONNECT_PREFIX) {
            return Ok(None);
        }
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {} FROM reconnects WHERE hash = ?1 AND {} ?2",
                    Self::RECONNECT_COLUMNS,
                    Self::RECONNECT_STANDS
                ),
                params![hash(secret), now],
                Self::reconnect_from,
            )
            .optional()?)
    }

    /// The account a re-connect link is for, when the link still works. Looking does not
    /// use it.
    pub fn peek_reconnect(&self, secret: &str, now: i64) -> Result<Option<(Reconnect, Account)>> {
        let conn = self.lock();
        let Some(link) = Self::reconnect_by_secret(&conn, secret, now)? else {
            return Ok(None);
        };
        Ok(Self::account_in(&conn, &link.account)?.map(|account| (link, account)))
    }

    /// The re-connect link an account has waiting, if it has one that still works.
    pub fn reconnect_of(&self, account: &AccountId, now: i64) -> Result<Option<Reconnect>> {
        Ok(self
            .lock()
            .query_row(
                &format!(
                    "SELECT {} FROM reconnects WHERE account = ?1 AND {} ?2",
                    Self::RECONNECT_COLUMNS,
                    Self::RECONNECT_STANDS
                ),
                params![account.0, now],
                Self::reconnect_from,
            )
            .optional()?)
    }

    /// Take back an account's re-connect link. `false` when it has none that still works.
    pub fn revoke_reconnect(&self, account: &AccountId, now: i64) -> Result<bool> {
        Ok(self.lock().execute(
            &format!(
                "UPDATE reconnects SET revoked_at = ?2 WHERE account = ?1 AND {} ?2",
                Self::RECONNECT_STANDS
            ),
            params![account.0, now],
        )? > 0)
    }

    /// Use a re-connect link. The person has just signed in with `sign_in`: it is put on
    /// the link's account, and a session is started. Returns the account and the session's
    /// secret.
    ///
    /// The link works once. A sign-in another account holds is refused and the link is
    /// left to use: the person picked the wrong one. All of it happens or none of it.
    pub fn reconnect(
        &self,
        secret: &str,
        sign_in: &NewSignIn<'_>,
        now: i64,
    ) -> Result<(Account, String)> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let link = Self::reconnect_by_secret(&tx, secret, now)?
            .ok_or(AccountError::ReconnectDoesNotWork)?;
        let account =
            Self::account_in(&tx, &link.account)?.ok_or(AccountError::ReconnectDoesNotWork)?;
        Self::add_sign_in_in(&tx, &account.id, sign_in, now)?;
        let used = tx.execute(
            "UPDATE reconnects SET used_at = ?2 WHERE id = ?1 AND used_at IS NULL",
            params![link.id, now],
        )?;
        if used == 0 {
            return Err(AccountError::ReconnectDoesNotWork);
        }
        let session = Self::new_session_in(&tx, &account.id, now)?;
        tx.commit()?;
        Ok((account, session))
    }

    /// Change what an account is called.
    pub fn rename(&self, account: &AccountId, name: &str) -> Result<Account> {
        let name = clean(name, "An account needs a name.")?;
        let conn = self.lock();
        let changed = conn.execute(
            "UPDATE accounts SET name = ?2 WHERE id = ?1",
            params![account.0, name],
        )?;
        if changed == 0 {
            return Err(AccountError::NoSuchAccount);
        }
        Ok(Self::account_in(&conn, account)?.expect("just updated"))
    }

    // ── Invites ─────────────────────────────────────────────────────────────

    fn invite_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Invite, bool)> {
        let kind: String = row.get(1)?;
        // A kind this build cannot read gives nothing: the flag beside the invite says so,
        // and every reader drops it.
        let parsed = serde_json::from_str::<InviteKind>(&kind).ok();
        let readable = parsed.is_some();
        Ok((
            Invite {
                id: row.get(0)?,
                kind: parsed.unwrap_or(InviteKind::Owner),
                label: row.get(2)?,
                max_uses: row.get(3)?,
                uses: row.get(4)?,
                expires_at: row.get(5)?,
                created_by: row.get::<_, Option<String>>(6)?.map(AccountId),
                created_at: row.get(7)?,
                revoked_at: row.get(8)?,
            },
            readable,
        ))
    }

    const INVITE_COLUMNS: &'static str =
        "id, kind, label, max_uses, uses, expires_at, created_by, created_at, revoked_at";

    /// Make an invite. Returns its record and **the link's secret, which is not kept**.
    ///
    /// Whether the maker may is the caller's to check ([`Standing::may`]).
    pub fn new_invite(
        &self,
        spec: NewInvite,
        by: Option<&AccountId>,
        now: i64,
    ) -> Result<(Invite, String)> {
        let label = clean(
            &spec.label,
            "An invite needs a label, so it can be told from the others.",
        )?;
        let max_uses = match spec.kind {
            InviteKind::Group { .. } => spec.max_uses,
            InviteKind::Owner | InviteKind::Personal { .. } => 1,
        };
        if max_uses == 0 {
            return Err(AccountError::Invalid("An invite must work at least once."));
        }
        if let InviteKind::Group { .. } = spec.kind
            && spec.expires_at.is_none()
        {
            return Err(AccountError::Invalid("A group invite needs an end date."));
        }
        if spec.expires_at.is_some_and(|end| end <= now) {
            return Err(AccountError::Invalid("That end date has already passed."));
        }
        let secret = format!("{INVITE_PREFIX}{}", random(24));
        let conn = self.lock();
        // An invite made for a pilot is for one the club has and nobody holds.
        if let InviteKind::Personal {
            club,
            pilot: Some(pilot),
            ..
        } = &spec.kind
        {
            if Self::club_pilot_in(&conn, club, pilot)?.is_none() {
                return Err(AccountError::NoSuchPilot);
            }
            if Self::holder_in(&conn, club, pilot)?.is_some() {
                return Err(AccountError::PilotTaken);
            }
        }
        let id = loop {
            let id = random(6);
            let taken = conn
                .query_row("SELECT 1 FROM invites WHERE id = ?1", params![id], |_| {
                    Ok(())
                })
                .optional()?
                .is_some();
            if !taken {
                break id;
            }
        };
        let invite = Invite {
            id,
            kind: spec.kind,
            label,
            max_uses,
            uses: 0,
            expires_at: spec.expires_at,
            created_by: by.cloned(),
            created_at: now,
            revoked_at: None,
        };
        conn.execute(
            "INSERT INTO invites (id, hash, kind, label, max_uses, uses, expires_at, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8)",
            params![
                invite.id,
                hash(&secret),
                serde_json::to_string(&invite.kind).expect("an invite kind is plain data"),
                invite.label,
                invite.max_uses,
                invite.expires_at,
                invite.created_by.as_ref().map(|a| a.0.clone()),
                now
            ],
        )?;
        Ok((invite, secret))
    }

    fn invite_by_secret(conn: &Connection, secret: &str, now: i64) -> Result<Option<Invite>> {
        if !secret.starts_with(INVITE_PREFIX) {
            return Ok(None);
        }
        let found = conn
            .query_row(
                &format!(
                    "SELECT {} FROM invites WHERE hash = ?1",
                    Self::INVITE_COLUMNS
                ),
                params![hash(secret)],
                Self::invite_from,
            )
            .optional()?;
        Ok(found
            // An invite of a kind this build cannot read is no invite.
            .filter(|(invite, readable)| *readable && invite.stands(now))
            .map(|(invite, _)| invite))
    }

    /// What an invite link is for, when it still works. Looking does not use it.
    pub fn peek_invite(&self, secret: &str, now: i64) -> Result<Option<Invite>> {
        Self::invite_by_secret(&self.lock(), secret, now)
    }

    /// A club's invites, newest first: those that work and those that no longer do.
    pub fn invites_of(&self, club: &ClubId) -> Result<Vec<Invite>> {
        Ok(self
            .all_invites()?
            .into_iter()
            .filter(|invite| invite.kind.club() == Some(club))
            .collect())
    }

    /// Every invite, newest first.
    pub fn all_invites(&self) -> Result<Vec<Invite>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM invites ORDER BY created_at DESC, id",
            Self::INVITE_COLUMNS
        ))?;
        let rows = stmt.query_map([], Self::invite_from)?;
        let mut out = Vec::new();
        for row in rows {
            let (invite, readable) = row?;
            if readable {
                out.push(invite);
            }
        }
        Ok(out)
    }

    /// Take back every invite to a club that still stands. Returns how many.
    pub fn revoke_invites_of(&self, club: &ClubId, now: i64) -> Result<usize> {
        let mut gone = 0;
        for invite in self.invites_of(club)? {
            if invite.stands(now) && self.revoke_invite(&invite.id, now)? {
                gone += 1;
            }
        }
        Ok(gone)
    }

    /// Take an invite back, by its id. `false` when there is none that still stands.
    pub fn revoke_invite(&self, id: &str, now: i64) -> Result<bool> {
        Ok(self.lock().execute(
            "UPDATE invites SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, now],
        )? > 0)
    }

    /// Give what an invite gives, to an account, inside a transaction that is already open.
    fn grant(tx: &Connection, account: &AccountId, invite: &Invite, now: i64) -> Result<()> {
        match &invite.kind {
            InviteKind::Owner => {
                tx.execute(
                    "INSERT OR IGNORE INTO owners (account) VALUES (?1)",
                    params![account.0],
                )?;
            }
            kind => {
                let club = kind.club().expect("not an owner invite");
                let role = kind.role().expect("not an owner invite");
                let current: Option<String> = tx
                    .query_row(
                        "SELECT role FROM members WHERE account = ?1 AND club = ?2",
                        params![account.0, club.0],
                        |row| row.get(0),
                    )
                    .optional()?;
                // An invite never takes a role away: a director who opens the club's
                // group link stays a director.
                let keep = current
                    .as_deref()
                    .and_then(Role::parse)
                    .is_some_and(|have| have >= role);
                if !keep {
                    tx.execute(
                        "INSERT INTO members (account, club, role, added_at) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(account, club) DO UPDATE SET role = excluded.role",
                        params![account.0, club.0, role.as_str(), now],
                    )?;
                }
                // An invite made for a pilot gives that pilot, unless someone came to
                // hold it since. The person still joins the club.
                if let InviteKind::Personal {
                    pilot: Some(pilot), ..
                } = kind
                    && Self::holder_in(tx, club, pilot)?.is_none()
                {
                    Self::hold_in(
                        tx,
                        club,
                        pilot,
                        account,
                        ClaimHow::Invite,
                        invite.created_by.as_ref(),
                        now,
                    )?;
                }
            }
        }
        let used = tx.execute(
            "UPDATE invites SET uses = uses + 1 WHERE id = ?1 AND uses < max_uses AND revoked_at IS NULL",
            params![invite.id],
        )?;
        if used == 0 {
            return Err(AccountError::InviteDoesNotWork);
        }
        Ok(())
    }

    /// Make an account with an invite: the only way one is made. The person has just
    /// signed in with `sign_in`, which no account has yet. Returns the account, the invite
    /// that was used, and a session's secret.
    ///
    /// All of it happens or none of it: no account without the invite being used, and no
    /// invite used without the account.
    pub fn join_with_invite(
        &self,
        secret: &str,
        sign_in: &NewSignIn<'_>,
        name: &str,
        now: i64,
    ) -> Result<(Account, Invite, String)> {
        // A name the provider gave may be empty or odd. The person can change it after.
        let name = clean(name, "An account needs a name.").unwrap_or_else(|_| "Pilot".to_owned());
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let invite =
            Self::invite_by_secret(&tx, secret, now)?.ok_or(AccountError::InviteDoesNotWork)?;
        let account = Account {
            id: AccountId(format!("acct_{}", random(9))),
            name,
            created_at: now,
        };
        tx.execute(
            "INSERT INTO accounts (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![account.id.0, account.name, now],
        )?;
        Self::add_sign_in_in(&tx, &account.id, sign_in, now)?;
        Self::grant(&tx, &account.id, &invite, now)?;
        let session = Self::new_session_in(&tx, &account.id, now)?;
        tx.commit()?;
        Ok((account, invite, session))
    }

    /// Use an invite as an account that already exists: someone with an account is invited
    /// to another club, or to be an owner.
    pub fn accept_invite(&self, secret: &str, account: &AccountId, now: i64) -> Result<Invite> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if Self::account_in(&tx, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        let invite =
            Self::invite_by_secret(&tx, secret, now)?.ok_or(AccountError::InviteDoesNotWork)?;
        Self::grant(&tx, account, &invite, now)?;
        tx.commit()?;
        Ok(invite)
    }

    // ── Roles ───────────────────────────────────────────────────────────────

    fn directors_in(conn: &Connection, club: &ClubId) -> Result<i64> {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM members WHERE club = ?1 AND role = 'director'",
            params![club.0],
            |row| row.get(0),
        )?)
    }

    /// Give a member of a club another role. Whether the asker may is the caller's to
    /// check. The club's last director cannot be made anything else.
    pub fn set_role(&self, club: &ClubId, account: &AccountId, role: Role) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let current: Option<String> = tx
            .query_row(
                "SELECT role FROM members WHERE account = ?1 AND club = ?2",
                params![account.0, club.0],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(AccountError::NotAMember);
        };
        if current == "director" && role != Role::Director && Self::directors_in(&tx, club)? <= 1 {
            return Err(AccountError::LastDirector);
        }
        tx.execute(
            "UPDATE members SET role = ?3 WHERE account = ?1 AND club = ?2",
            params![account.0, club.0, role.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Remove a member from a club. Their account, and their place in other clubs, stay.
    pub fn remove_member(&self, club: &ClubId, account: &AccountId) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let current: Option<String> = tx
            .query_row(
                "SELECT role FROM members WHERE account = ?1 AND club = ?2",
                params![account.0, club.0],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(AccountError::NotAMember);
        };
        if current == "director" && Self::directors_in(&tx, club)? <= 1 {
            return Err(AccountError::LastDirector);
        }
        tx.execute(
            "DELETE FROM members WHERE account = ?1 AND club = ?2",
            params![account.0, club.0],
        )?;
        // Only a member holds or asks for a club's pilots.
        tx.execute(
            "DELETE FROM claims WHERE account = ?1 AND club = ?2",
            params![account.0, club.0],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── A club's pilots, and who holds them ─────────────────────────────────

    /// Note pilots a Director sent for a club: new ones are added, and a known one takes
    /// the callsign and colour it has now. None is ever removed.
    pub fn note_pilots(&self, club: &ClubId, pilots: &[ClubPilot], now: i64) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for pilot in pilots {
            if !valid_pilot_id(&pilot.pilot) {
                continue;
            }
            // A callsign that will not do is not worth refusing an event over.
            let Ok(callsign) = clean(&pilot.callsign, "") else {
                continue;
            };
            tx.execute(
                "INSERT INTO club_pilots (club, pilot, callsign, color, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(club, pilot) DO UPDATE SET
                    callsign = excluded.callsign, color = excluded.color, seen_at = excluded.seen_at",
                params![club.0, pilot.pilot, callsign, pilot.color, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn club_pilot_in(conn: &Connection, club: &ClubId, pilot: &str) -> Result<Option<ClubPilot>> {
        Ok(conn
            .query_row(
                "SELECT pilot, callsign, color FROM club_pilots WHERE club = ?1 AND pilot = ?2",
                params![club.0, pilot],
                |row| {
                    Ok(ClubPilot {
                        pilot: row.get(0)?,
                        callsign: row.get(1)?,
                        color: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// One pilot of a club, if the club has them.
    pub fn club_pilot(&self, club: &ClubId, pilot: &str) -> Result<Option<ClubPilot>> {
        Self::club_pilot_in(&self.lock(), club, pilot)
    }

    /// A club's pilots, by callsign.
    pub fn club_pilots(&self, club: &ClubId) -> Result<Vec<ClubPilot>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT pilot, callsign, color FROM club_pilots WHERE club = ?1
             ORDER BY callsign COLLATE NOCASE, pilot",
        )?;
        let rows = stmt.query_map(params![club.0], |row| {
            Ok(ClubPilot {
                pilot: row.get(0)?,
                callsign: row.get(1)?,
                color: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn holder_in(conn: &Connection, club: &ClubId, pilot: &str) -> Result<Option<AccountId>> {
        Ok(conn
            .query_row(
                "SELECT account FROM claims WHERE club = ?1 AND pilot = ?2 AND held = 1",
                params![club.0, pilot],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(AccountId))
    }

    fn is_member_in(conn: &Connection, club: &ClubId, account: &AccountId) -> Result<bool> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM members WHERE account = ?1 AND club = ?2",
                params![account.0, club.0],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Make `account` the holder of a pilot, inside a transaction: whoever held it lets
    /// go, and everyone who had asked for it is answered by this.
    fn hold_in(
        tx: &Connection,
        club: &ClubId,
        pilot: &str,
        account: &AccountId,
        how: ClaimHow,
        by: Option<&AccountId>,
        now: i64,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM claims WHERE club = ?1 AND pilot = ?2",
            params![club.0, pilot],
        )?;
        tx.execute(
            "INSERT INTO claims (club, pilot, account, held, how, at, by)
             VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6)",
            params![
                club.0,
                pilot,
                account.0,
                how.as_str(),
                now,
                by.map(|a| a.0.clone())
            ],
        )?;
        Ok(())
    }

    /// Ask to hold a pilot of a club. The account must be a member, and the pilot must
    /// be one nobody holds. Asking twice is asking once. Nothing is held until someone
    /// allowed says yes ([`Accounts::approve_claim`]).
    pub fn ask_claim(
        &self,
        club: &ClubId,
        pilot: &str,
        account: &AccountId,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if !Self::is_member_in(&tx, club, account)? {
            return Err(AccountError::NotAMember);
        }
        if Self::club_pilot_in(&tx, club, pilot)?.is_none() {
            return Err(AccountError::NoSuchPilot);
        }
        match Self::holder_in(&tx, club, pilot)? {
            // Asking for what is already yours changes nothing.
            Some(holder) if &holder == account => return Ok(()),
            Some(_) => return Err(AccountError::PilotTaken),
            None => {}
        }
        tx.execute(
            "INSERT OR IGNORE INTO claims (club, pilot, account, held, how, at)
             VALUES (?1, ?2, ?3, 0, 'asked', ?4)",
            params![club.0, pilot, account.0, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Say yes to an account that asked for a pilot. Whether the one saying so may is the
    /// caller's to check. Anyone else who asked for that pilot is answered by this.
    pub fn approve_claim(
        &self,
        club: &ClubId,
        pilot: &str,
        account: &AccountId,
        by: &AccountId,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let asked = tx
            .query_row(
                "SELECT 1 FROM claims
                 WHERE club = ?1 AND pilot = ?2 AND account = ?3 AND held = 0",
                params![club.0, pilot, account.0],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !asked {
            return Err(AccountError::NoSuchClaim);
        }
        if Self::holder_in(&tx, club, pilot)?.is_some() {
            return Err(AccountError::PilotTaken);
        }
        // Someone removed from the club since they asked holds nothing in it.
        if !Self::is_member_in(&tx, club, account)? {
            return Err(AccountError::NotAMember);
        }
        Self::hold_in(&tx, club, pilot, account, ClaimHow::Approved, Some(by), now)?;
        tx.commit()?;
        Ok(())
    }

    /// Say which account holds a pilot, with nobody having asked: what a director does.
    /// Whoever held it lets go. The account must be a member of the club.
    pub fn give_pilot(
        &self,
        club: &ClubId,
        pilot: &str,
        account: &AccountId,
        by: Option<&AccountId>,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if Self::club_pilot_in(&tx, club, pilot)?.is_none() {
            return Err(AccountError::NoSuchPilot);
        }
        if Self::account_in(&tx, account)?.is_none() {
            return Err(AccountError::NoSuchAccount);
        }
        if !Self::is_member_in(&tx, club, account)? {
            return Err(AccountError::NotAMember);
        }
        if Self::holder_in(&tx, club, pilot)?.as_ref() != Some(account) {
            Self::hold_in(&tx, club, pilot, account, ClaimHow::Director, by, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Take a pilot from whoever holds it. `false` when nobody did. Those who asked for
    /// it are still asking.
    pub fn free_pilot(&self, club: &ClubId, pilot: &str) -> Result<bool> {
        Ok(self.lock().execute(
            "DELETE FROM claims WHERE club = ?1 AND pilot = ?2 AND held = 1",
            params![club.0, pilot],
        )? > 0)
    }

    /// End one account's tie to a pilot: an ask is withdrawn or refused, a pilot held is
    /// let go. `false` when there was none.
    pub fn drop_claim(&self, club: &ClubId, pilot: &str, account: &AccountId) -> Result<bool> {
        Ok(self.lock().execute(
            "DELETE FROM claims WHERE club = ?1 AND pilot = ?2 AND account = ?3",
            params![club.0, pilot, account.0],
        )? > 0)
    }

    fn claims_where(&self, clause: &str, value: &str) -> Result<Vec<Claim>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.club, c.pilot, c.how, c.at, a.id, a.name, a.created_at FROM claims c
             JOIN accounts a ON a.id = c.account
             WHERE {clause} ORDER BY c.at, c.club, c.pilot, a.id"
        ))?;
        let rows = stmt.query_map(params![value], |row| {
            Ok((
                row.get::<_, String>(2)?,
                Claim {
                    club: ClubId(row.get(0)?),
                    pilot: row.get(1)?,
                    account: Account {
                        id: AccountId(row.get(4)?),
                        name: row.get(5)?,
                        created_at: row.get(6)?,
                    },
                    how: ClaimHow::Asked,
                    at: row.get(3)?,
                },
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (how, claim) = row?;
            // A kind this build does not know gives nothing.
            if let Some(how) = ClaimHow::parse(&how) {
                out.push(Claim { how, ..claim });
            }
        }
        Ok(out)
    }

    /// Every tie between an account and a pilot of a club: held and asked, oldest first.
    pub fn claims_in(&self, club: &ClubId) -> Result<Vec<Claim>> {
        self.claims_where("c.club = ?1", &club.0)
    }

    /// Every pilot an account holds or has asked for, in any club.
    pub fn claims_of(&self, account: &AccountId) -> Result<Vec<Claim>> {
        self.claims_where("c.account = ?1", &account.0)
    }
}

/// Whether text can be a pilot's id: what a Director makes, and nothing that could be
/// mistaken for part of an address.
pub fn valid_pilot_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 120
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A sign-in a provider has just vouched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewSignIn<'a> {
    pub provider: &'a str,
    pub subject: &'a str,
    pub label: &'a str,
}
