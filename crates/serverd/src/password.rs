//! Signing in with a username and a password kept on this server (docs/primitives.md
//! §13.8).
//!
//! **Off until an owner switches it on.** It is for a server that should need nothing
//! outside itself: no Discord, no Google. A provider knows who someone is and keeps their
//! password; with this, the server keeps it.
//!
//! - **A password is never kept, only its hash** (Argon2id, with a salt of its own). It is
//!   never in an answer, a log or an error.
//! - **A username is a sign-in like any other.** An account may have one beside Discord or
//!   Google, keeps at least one way in, and an owner can take it off (§13.8).
//! - **Wrong guesses are slowed down.** After five wrong passwords in a row for one
//!   username, that username waits a minute, whoever is asking.
//! - **Nothing says whether a username exists.** A wrong password and a username nobody
//!   has get the same answer, after the same work.
//! - **An account is made the same ways as with a provider:** an invite, a re-connect link,
//!   or open sign-up when the owner allows it. A re-connect link is how a forgotten
//!   password is set again.
//!
//! | route | who | what |
//! |-------|-----|------|
//! | `PUT /api/server/password-sign-in` | an owner | switch it on or off |
//! | `POST /api/sign-in/password` | anyone | sign in |
//! | `POST /api/sign-up/password` | anyone | make an account, or get back into one with a link |
//! | `PUT /api/me/password` | an account | add a username and password, or change the password |

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::Json;
use axum::extract::State;
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use gridfpv_server::error::ErrorCode;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::{AccountId, Act, NewSignIn, PASSWORD_PROVIDER, RECONNECT_PREFIX};
use crate::app::AppState;
use crate::people::{Me, ServerInfo, Signed, me_of, now_ms, server_info, session_cookie};
use crate::push::ApiError;

/// The shortest and longest a username may be.
const USERNAME_MIN: usize = 3;
const USERNAME_MAX: usize = 32;

/// The shortest and longest a password may be. Long enough to be hard to guess; the top
/// is only there so nobody can make the server hash a book.
pub const PASSWORD_MIN: usize = 10;
const PASSWORD_MAX: usize = 200;

/// How many wrong passwords in a row a username gets before it waits.
const TRIES: u32 = 5;

/// How long a username waits after that.
const WAIT: Duration = Duration::from_secs(60);

/// How many usernames are watched for wrong guesses at once. Past that the oldest are
/// forgotten, so a flood of made-up usernames cannot fill the memory.
const WATCHED_MAX: usize = 10_000;

const OFF: &str = "Signing in with a password is switched off on this server.";
const NO_MATCH: &str = "That username and password do not match.";

/// The cost of hashing a password: 19 MiB of memory, two passes, one thread. It is what
/// OWASP gives as the least for Argon2id, and it takes a small server well under a second.
fn hasher() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, None).expect("fixed Argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// The hash to keep for a password. Each has a salt of its own, so two people with the
/// same password have different hashes.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("the operating system gives random bytes");
    let salt = SaltString::encode_b64(&salt).expect("sixteen bytes make a salt");
    hasher()
        .hash_password(password.as_bytes(), &salt)
        .expect("a password of an allowed length can be hashed")
        .to_string()
}

/// Whether a password is the one a hash was made from. A hash that cannot be read
/// matches nothing.
pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        hasher()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// The hash of a password nobody has. A sign-in for a username that does not exist is
/// checked against it, so it takes as long as one that does. Made once, when first needed.
fn decoy_hash() -> &'static str {
    static DECOY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DECOY.get_or_init(|| hash_password("a password nobody has"))
}

/// A username as it is kept: lower case, 3 to 32 of `a-z`, `0-9`, `.`, `_` and `-`.
/// Letters are told apart whatever their case, so `Ace` and `ace` are one username.
pub fn clean_username(text: &str) -> Result<String, ApiError> {
    let username = text.trim().to_ascii_lowercase();
    let count = username.chars().count();
    let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c);
    if !(USERNAME_MIN..=USERNAME_MAX).contains(&count) || !username.chars().all(allowed) {
        return Err(ApiError::bad(
            "A username is 3 to 32 letters and digits. A dot, a dash and an underscore are fine too.",
        ));
    }
    Ok(username)
}

/// A password that may be set. Any characters are allowed: only its length is checked.
fn check_password(password: &str) -> Result<(), ApiError> {
    let count = password.chars().count();
    if count < PASSWORD_MIN {
        return Err(ApiError::bad(
            "A password is at least 10 characters. A few words in a row is a good one.",
        ));
    }
    if count > PASSWORD_MAX {
        return Err(ApiError::bad("A password is 200 characters at most."));
    }
    Ok(())
}

/// Who has guessed wrong, and how often. Kept in memory: a restart forgets it, which costs
/// a guesser nothing they could not get by waiting a minute.
#[derive(Default)]
pub struct Guesses {
    by_username: Mutex<HashMap<String, Guess>>,
}

struct Guess {
    wrong: u32,
    /// Set once the tries are used up: no guess is looked at before this.
    wait_until: Option<Instant>,
    last: Instant,
}

impl Guesses {
    /// How long a username must still wait, if it must.
    fn waiting(&self, username: &str) -> Option<Duration> {
        let mut map = self.by_username.lock().unwrap_or_else(|e| e.into_inner());
        let guess = map.get_mut(username)?;
        let until = guess.wait_until?;
        let now = Instant::now();
        if now >= until {
            // The wait is over. They start again with all their tries.
            map.remove(username);
            return None;
        }
        Some(until - now)
    }

    /// A wrong password for a username.
    fn wrong(&self, username: &str) {
        let mut map = self.by_username.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if map.len() >= WATCHED_MAX && !map.contains_key(username) {
            // Forget the ones longest left alone, a tenth at a time.
            let mut by_age: Vec<(String, Instant)> =
                map.iter().map(|(k, v)| (k.clone(), v.last)).collect();
            by_age.sort_by_key(|(_, last)| *last);
            for (name, _) in by_age.into_iter().take(WATCHED_MAX / 10) {
                map.remove(&name);
            }
        }
        let guess = map.entry(username.to_owned()).or_insert(Guess {
            wrong: 0,
            wait_until: None,
            last: now,
        });
        guess.wrong += 1;
        guess.last = now;
        if guess.wrong >= TRIES {
            guess.wait_until = Some(now + WAIT);
        }
    }

    /// The right password: the count starts over.
    fn right(&self, username: &str) {
        self.by_username
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(username);
    }
}

fn off() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, OFF)
}

fn no_match() -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized, NO_MATCH)
}

fn wait(left: Duration) -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::Refused,
        format!(
            "Too many wrong passwords. Wait {} seconds and try again.",
            left.as_secs().max(1)
        ),
    )
}

/// Hash off the threads that answer requests: it is slow on purpose.
async fn hashed(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| internal())
}

async fn verified(password: String, hash: String) -> Result<bool, ApiError> {
    tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .map_err(|_| internal())
}

fn internal() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        ErrorCode::Internal,
        "The server could not check the password.",
    )
}

/// The answer that signs a browser in: the account, and its session cookie.
fn signed_in(
    state: &AppState,
    headers: &HeaderMap,
    account: &AccountId,
    session: &str,
) -> Result<Response, ApiError> {
    Ok((
        [(SET_COOKIE, session_cookie(state, headers, session))],
        Json(me_of(state, account)?),
    )
        .into_response())
}

// ── On the wire ─────────────────────────────────────────────────────────────

/// `PUT /api/server/password-sign-in`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PasswordSignInChange {
    pub on: bool,
}

/// `POST /api/sign-in/password`.
#[derive(Clone, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PasswordSignIn {
    pub username: String,
    pub password: String,
}

/// `POST /api/sign-up/password`.
#[derive(Clone, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PasswordSignUp {
    pub username: String,
    pub password: String,
    /// What the account is called. Left out, the username as it was typed stands in.
    #[serde(default)]
    #[ts(optional)]
    pub name: Option<String>,
    /// The invite, or the re-connect link, the person came with. Left out on a server
    /// that lets anyone make an account.
    #[serde(default)]
    #[ts(optional)]
    pub invite: Option<String>,
}

/// `PUT /api/me/password`.
#[derive(Clone, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PasswordChange {
    /// The username to sign in with, for an account that has none yet.
    #[serde(default)]
    #[ts(optional)]
    pub username: Option<String>,
    /// The password the account has now, to change it.
    #[serde(default)]
    #[ts(optional)]
    pub current: Option<String>,
    /// The new password.
    pub password: String,
}

// ── The routes ──────────────────────────────────────────────────────────────

/// `PUT /api/server/password-sign-in`: an owner switches it on or off.
///
/// Switching it off locks out everyone whose only way in is a password. An owner is not
/// let to do that to themselves from the page: they would have no way back but the
/// command line.
pub async fn put_setting(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(change): Json<PasswordSignInChange>,
) -> Result<Json<ServerInfo>, ApiError> {
    if !standing.may(Act::RunServer) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "Only an owner of this server can do that.",
        ));
    }
    if !change.on {
        let mine = state.accounts.sign_ins(&standing.account)?;
        if mine.iter().all(|held| held.provider == PASSWORD_PROVIDER) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                ErrorCode::Refused,
                "A password is the only way you sign in. Add another way to sign in first, or you would lock yourself out.",
            ));
        }
    }
    state
        .book
        .set_password_sign_in(change.on)
        .map_err(crate::manage::book_error)?;
    Ok(Json(server_info(&state)))
}

/// `POST /api/sign-in/password`.
pub async fn post_sign_in(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(given): Json<PasswordSignIn>,
) -> Result<Response, ApiError> {
    if !state.book.password_sign_in() {
        return Err(off());
    }
    // A username that could not be one matches nothing, and is not counted.
    let Ok(username) = clean_username(&given.username) else {
        return Err(no_match());
    };
    if let Some(left) = state.guesses.waiting(&username) {
        return Err(wait(left));
    }
    let found = state.accounts.password_of(&username)?;
    // The same work whether or not the username exists, so the time taken says nothing.
    let hash = match &found {
        Some((_, Some(hash))) => hash.clone(),
        _ => decoy_hash().to_owned(),
    };
    let right = verified(given.password, hash).await?;
    let account = match found {
        Some((account, Some(_))) if right => account,
        _ => {
            state.guesses.wrong(&username);
            return Err(no_match());
        }
    };
    state.guesses.right(&username);
    let session = state.accounts.new_session(&account, now_ms())?;
    signed_in(&state, &headers, &account, &session)
}

/// `POST /api/sign-up/password`: make an account with a username and a password.
///
/// - With an invite that works, the account gets what the invite gives.
/// - With a re-connect link, the username and password become a way in to the account the
///   link is for. This is how a forgotten password is set again.
/// - With neither, an account is made only when the owner lets anyone make one (§13.7).
pub async fn post_sign_up(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(given): Json<PasswordSignUp>,
) -> Result<Response, ApiError> {
    if !state.book.password_sign_in() {
        return Err(off());
    }
    let username = clean_username(&given.username)?;
    check_password(&given.password)?;
    let invite = given.invite.as_deref().filter(|invite| !invite.is_empty());
    if invite.is_none() && !state.book.open_sign_up() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "You need an invite to make an account here. Ask your club for a link.",
        ));
    }
    // Hashed before anything is made, so a slow hash never leaves half an account.
    let hash = hashed(given.password).await?;
    let now = now_ms();
    let sign_in = NewSignIn {
        provider: PASSWORD_PROVIDER,
        subject: &username,
        label: &username,
    };
    // The account is called what they typed, capitals and all. The username itself is
    // kept in lower case.
    let typed = given.username.trim().to_owned();
    let name = given
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&typed);
    let accounts = &state.accounts;
    let (account, session) = match invite {
        Some(link) if link.starts_with(RECONNECT_PREFIX) => {
            // The account may have had another username. It has one at a time.
            let old = accounts
                .peek_reconnect(link, now)?
                .and_then(|(_, account)| accounts.username_of(&account.id).transpose())
                .transpose()?;
            let (account, session) = accounts.reconnect(link, &sign_in, now)?;
            if let Some(old) = old.filter(|old| old != &username) {
                accounts.drop_sign_in(&account.id, PASSWORD_PROVIDER, &old)?;
                // Taking a sign-in off ends every session. This browser gets a new one.
                let session = accounts.new_session(&account.id, now)?;
                accounts.set_password_hash(&account.id, &hash, now)?;
                return signed_in(&state, &headers, &account.id, &session);
            }
            (account.id, session)
        }
        Some(link) => {
            let (account, _, session) = accounts.join_with_invite(link, &sign_in, name, now)?;
            (account.id, session)
        }
        None => {
            let (account, session) = accounts.join_open(&sign_in, name, now)?;
            (account.id, session)
        }
    };
    accounts.set_password_hash(&account, &hash, now)?;
    signed_in(&state, &headers, &account, &session)
}

/// `PUT /api/me/password`: add a username and password to the signed-in account, or
/// change its password.
///
/// Changing needs the password the account has now. Everyone else signed in to the
/// account is signed out.
pub async fn put_my_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Signed(standing): Signed,
    Json(change): Json<PasswordChange>,
) -> Result<Json<Me>, ApiError> {
    if !state.book.password_sign_in() {
        return Err(off());
    }
    check_password(&change.password)?;
    let accounts = &state.accounts;
    let account = &standing.account;
    let now = now_ms();
    match accounts.username_of(account)? {
        // Changing the password. The username stays.
        Some(username) => {
            if let Some(left) = state.guesses.waiting(&username) {
                return Err(wait(left));
            }
            let held = accounts.password_of(&username)?.and_then(|(_, hash)| hash);
            // An account whose password was never kept has nothing to match: it sets one.
            if let Some(held) = held {
                let current = change.current.clone().unwrap_or_default();
                if !verified(current, held).await? {
                    state.guesses.wrong(&username);
                    return Err(ApiError::new(
                        StatusCode::FORBIDDEN,
                        ErrorCode::Forbidden,
                        "That is not the password you have now.",
                    ));
                }
                state.guesses.right(&username);
            }
            let hash = hashed(change.password).await?;
            accounts.set_password_hash(account, &hash, now)?;
        }
        // Adding one.
        None => {
            let username = clean_username(change.username.as_deref().unwrap_or_default())?;
            let hash = hashed(change.password).await?;
            let sign_in = NewSignIn {
                provider: PASSWORD_PROVIDER,
                subject: &username,
                label: &username,
            };
            accounts.add_sign_in(account, &sign_in, now)?;
            accounts.set_password_hash(account, &hash, now)?;
        }
    }
    if let Some(mine) = crate::people::session_secret(&headers) {
        accounts.end_other_sessions(account, mine)?;
    }
    Ok(Json(me_of(&state, account)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_is_not_the_password_and_two_of_one_password_differ() {
        let one = hash_password("correct horse battery");
        let two = hash_password("correct horse battery");
        assert_ne!(one, two, "each has a salt of its own");
        assert!(!one.contains("correct horse"));
        assert!(one.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{one}");
        assert!(verify_password("correct horse battery", &one));
        assert!(verify_password("correct horse battery", &two));
        assert!(!verify_password("correct horse batter", &one));
        assert!(!verify_password("Correct horse battery", &one));
        assert!(!verify_password("", &one));
    }

    #[test]
    fn a_hash_that_cannot_be_read_matches_nothing() {
        for junk in ["", "hunter2", "$argon2id$", "$2b$12$abcdefghijklmnopqrstuv"] {
            assert!(!verify_password("hunter2", junk), "{junk:?}");
        }
    }

    #[test]
    fn a_username_is_tidied_and_what_is_not_one_is_refused() {
        for (given, kept) in [
            ("ace", "ace"),
            ("  Ace_FPV ", "ace_fpv"),
            ("a.b-c_9", "a.b-c_9"),
            ("ABC", "abc"),
            (&"x".repeat(32), &"x".repeat(32)),
        ] {
            assert_eq!(clean_username(given).ok().as_deref(), Some(kept), "{given}");
        }
        for bad in [
            "",
            "ab",
            &"x".repeat(33),
            "two words",
            "ace@example.org",
            "ace/../x",
            "åce",
            "a\nce",
            "gfr_abc!",
        ] {
            assert!(clean_username(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_password_is_judged_by_its_length_alone() {
        assert!(check_password("123456789").is_err());
        assert!(check_password("1234567890").is_ok());
        assert!(check_password("ten spaces").is_ok());
        assert!(check_password(&"é".repeat(10)).is_ok());
        assert!(check_password(&"x".repeat(200)).is_ok());
        assert!(check_password(&"x".repeat(201)).is_err());
        assert!(check_password("").is_err());
    }

    #[test]
    fn five_wrong_guesses_make_a_username_wait_and_a_right_one_starts_over() {
        let guesses = Guesses::default();
        for _ in 0..4 {
            guesses.wrong("ace");
            assert!(guesses.waiting("ace").is_none());
        }
        // A right password starts the count over.
        guesses.right("ace");
        for _ in 0..4 {
            guesses.wrong("ace");
        }
        assert!(guesses.waiting("ace").is_none());
        guesses.wrong("ace");
        let left = guesses.waiting("ace").expect("the fifth makes it wait");
        assert!(left <= WAIT && left > WAIT - Duration::from_secs(5));
        // Another username is not held up by it.
        assert!(guesses.waiting("bee").is_none());
    }

    #[test]
    fn the_list_of_guessers_cannot_grow_without_end() {
        let guesses = Guesses::default();
        for n in 0..(WATCHED_MAX + 500) {
            guesses.wrong(&format!("made-up-{n}"));
        }
        let held = guesses.by_username.lock().unwrap().len();
        assert!(held <= WATCHED_MAX, "{held}");
    }
}
