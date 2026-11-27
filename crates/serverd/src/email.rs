//! Email a server sends (docs/primitives.md §13.8).
//!
//! **A server sends email only when its owner sets it up**, with the settings of an email
//! account they have (`GRIDFPV_SERVER_SMTP_*`). With none, nothing here does anything, and
//! a forgotten password is set again with a link an owner hands over.
//!
//! It is used for two things, and no more:
//!
//! - **To show an address is its account's.** A person gives their address on their account
//!   page. A link is sent to it, and opening the link confirms it.
//! - **To let someone set a forgotten password** without asking an owner. A link is sent to
//!   the confirmed address of the account. It is the same re-connect link an owner makes,
//!   with an hour to use it.
//!
//! An address is never shown to anyone but its own account, and never used to say two
//! accounts are one person (§13.8).
//!
//! | route | who | what |
//! |-------|-----|------|
//! | `PUT /api/me/email` | an account | give an address; a link is sent to confirm it |
//! | `DELETE /api/me/email` | an account | take the address off |
//! | `POST /api/email/confirm` | whoever has the link | confirm an address |
//! | `POST /api/password/forgot` | anyone | ask for a link to set a new password |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use gridfpv_server::error::ErrorCode;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::accounts::AccountId;
use crate::app::AppState;
use crate::people::{Me, Signed, me_of, now_ms};
use crate::push::ApiError;

/// How long a link to set a new password works when it is sent by email.
pub const RESET_MS: i64 = 60 * 60 * 1000;

/// How long before the same account is sent another link.
const RESET_EVERY: Duration = Duration::from_secs(5 * 60);

/// How long before the same account is sent another link to confirm its address.
const CONFIRM_EVERY: Duration = Duration::from_secs(60);

/// The longest an email address may be.
const ADDRESS_MAX: usize = 254;

/// How the connection to the email server is kept private.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailTls {
    /// Start in the clear, then switch to TLS before anything is said. The usual way, on
    /// port 587.
    StartTls,
    /// TLS from the first byte. Port 465.
    Tls,
    /// No TLS. Only for an email server on this same computer.
    None,
}

/// The settings of the email account a server sends from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailSettings {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
    /// Who the mail is from, such as `Grid <grid@example.org>`.
    pub from: String,
    pub tls: MailTls,
}

impl MailSettings {
    /// The settings in the environment, when they are all there and make sense. `Err`
    /// says what is wrong, for the owner to read when the server starts.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_settings(|key| std::env::var(key).ok())
    }

    /// [`from_env`](Self::from_env), reading settings from anywhere.
    pub fn from_settings(read: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let get = |key: &str| {
            read(&format!("GRIDFPV_SERVER_SMTP_{key}"))
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let Some(host) = get("HOST") else {
            return Ok(None);
        };
        let from =
            get("FROM").ok_or("GRIDFPV_SERVER_SMTP_FROM is not set: who is the mail from?")?;
        from.parse::<Mailbox>().map_err(|_| {
            "GRIDFPV_SERVER_SMTP_FROM is not an address. It looks like: Grid <grid@example.org>"
        })?;
        let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]");
        let tls = match get("TLS")
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("starttls") => MailTls::StartTls,
            Some("tls") => MailTls::Tls,
            Some("none") if local => MailTls::None,
            Some("none") => {
                return Err(
                    "GRIDFPV_SERVER_SMTP_TLS=none is only for an email server on this same computer."
                        .into(),
                );
            }
            Some(_) => return Err("GRIDFPV_SERVER_SMTP_TLS is starttls, tls or none.".into()),
        };
        let port = match get("PORT") {
            Some(port) => port
                .parse()
                .map_err(|_| "GRIDFPV_SERVER_SMTP_PORT is not a port number.")?,
            None if tls == MailTls::Tls => 465,
            None => 587,
        };
        Ok(Some(Self {
            host,
            port,
            user: get("USER"),
            password: get("PASSWORD"),
            from,
            tls,
        }))
    }
}

/// One email: who it is to, its subject and its words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// What sends a server's email.
pub enum Mailer {
    /// An email server, reached with the owner's settings.
    Smtp {
        transport: Box<AsyncSmtpTransport<Tokio1Executor>>,
        from: Mailbox,
    },
    /// Nowhere: each mail is kept here to be looked at. For tests.
    Outbox(Mutex<Vec<Mail>>),
}

impl Mailer {
    /// A mailer for the owner's settings. Nothing is connected to until a mail is sent.
    pub fn smtp(settings: &MailSettings) -> Result<Self, String> {
        gridfpv_server::http::tls_ready();
        let from: Mailbox = settings
            .from
            .parse()
            .map_err(|_| "The address the mail is from is not an address.".to_owned())?;
        let secure = || {
            TlsParameters::new(settings.host.clone())
                .map_err(|e| format!("TLS for the email server could not be set up: {e}"))
        };
        let tls = match settings.tls {
            MailTls::StartTls => Tls::Required(secure()?),
            MailTls::Tls => Tls::Wrapper(secure()?),
            MailTls::None => Tls::None,
        };
        let mut builder =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(settings.host.clone())
                .port(settings.port)
                .tls(tls)
                .timeout(Some(Duration::from_secs(20)));
        if let (Some(user), Some(password)) = (&settings.user, &settings.password) {
            builder = builder.credentials(Credentials::new(user.clone(), password.clone()));
        }
        Ok(Self::Smtp {
            transport: Box::new(builder.build()),
            from,
        })
    }

    /// A mailer that sends nowhere and keeps what it was given.
    pub fn outbox() -> Self {
        Self::Outbox(Mutex::new(Vec::new()))
    }

    /// What an outbox was given, oldest first. Empty for a real mailer.
    pub fn sent(&self) -> Vec<Mail> {
        match self {
            Self::Smtp { .. } => Vec::new(),
            Self::Outbox(kept) => kept.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }

    /// Send one mail. `Err` is in words for an owner: why the email server did not take it.
    pub async fn send(&self, mail: Mail) -> Result<(), String> {
        match self {
            Self::Outbox(kept) => {
                kept.lock().unwrap_or_else(|e| e.into_inner()).push(mail);
                Ok(())
            }
            Self::Smtp { transport, from } => {
                let to: Mailbox = mail
                    .to
                    .parse()
                    .map_err(|_| format!("{} is not an email address.", mail.to))?;
                let message = Message::builder()
                    .from(from.clone())
                    .to(to)
                    .subject(mail.subject)
                    .body(mail.body)
                    .map_err(|e| format!("The mail could not be put together: {e}"))?;
                transport
                    .send(message)
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("The email server did not take the mail: {e}"))
            }
        }
    }
}

/// When each account was last sent a mail of each kind, so nobody's inbox is filled by
/// someone pressing a button again and again.
#[derive(Default)]
pub struct Sent {
    last: Mutex<HashMap<(String, &'static str), Instant>>,
}

impl Sent {
    /// Whether a mail of this kind may go to this account now. Saying yes counts as one.
    fn may(&self, account: &AccountId, kind: &'static str, every: Duration) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        last.retain(|_, at| now.duration_since(*at) < Duration::from_secs(3600));
        let key = (account.0.clone(), kind);
        if last
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < every)
        {
            return false;
        }
        last.insert(key, now);
        true
    }
}

/// An address as it is kept: trimmed, in lower case, and shaped like one.
pub fn clean_address(text: &str) -> Result<String, ApiError> {
    let address = text.trim().to_ascii_lowercase();
    let not_one = || ApiError::bad("That is not an email address. It looks like you@example.org");
    if address.len() > ADDRESS_MAX || address.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(not_one());
    }
    let parsed: Address = address.parse().map_err(|_| not_one())?;
    // A name with no dot in it after the @ is a computer on some local network, not
    // somewhere mail from the internet goes.
    if !parsed.domain().contains('.') {
        return Err(not_one());
    }
    Ok(address)
}

fn no_mail() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        ErrorCode::Refused,
        "This server does not send email. Ask an owner of the server for help.",
    )
}

/// The server's own name in a mail: its address without the `https://`.
fn server_name(state: &AppState) -> String {
    state
        .public_url
        .as_deref()
        .map(|url| {
            url.trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_owned()
        })
        .unwrap_or_else(|| "a Grid server".to_owned())
}

/// The mailer and the server's own address, or why no mail can be sent.
fn ready(state: &AppState) -> Result<(Arc<Mailer>, String), ApiError> {
    match (&state.mailer, &state.public_url) {
        (Some(mailer), Some(url)) => Ok((mailer.clone(), url.clone())),
        _ => Err(no_mail()),
    }
}

// ── On the wire ─────────────────────────────────────────────────────────────

/// An account's email address, as its own account is shown it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MyEmail {
    pub address: String,
    /// Whether they opened the link sent to it. Until then nothing is sent to it but that
    /// link.
    pub confirmed: bool,
}

/// `PUT /api/me/email`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EmailChange {
    pub address: String,
}

/// `POST /api/email/confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EmailConfirm {
    pub secret: String,
}

/// The answer to `POST /api/email/confirm`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EmailConfirmed {
    pub address: String,
}

/// `POST /api/password/forgot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Forgot {
    /// A username, or an email address.
    pub who: String,
}

/// An account's address, for its own page.
pub(crate) fn my_email(state: &AppState, account: &AccountId) -> Result<Option<MyEmail>, ApiError> {
    Ok(state
        .accounts
        .email_of(account)?
        .map(|(address, confirmed)| MyEmail { address, confirmed }))
}

// ── The routes ──────────────────────────────────────────────────────────────

/// `PUT /api/me/email`: give an address. A link is sent to it, and it is not used for
/// anything until the link is opened.
pub async fn put_my_email(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Json(change): Json<EmailChange>,
) -> Result<Json<Me>, ApiError> {
    let (mailer, url) = ready(&state)?;
    let address = clean_address(&change.address)?;
    let account = &standing.account;
    let already = state
        .accounts
        .email_of(account)?
        .is_some_and(|(held, confirmed)| held == address && confirmed);
    if already {
        // Nothing to confirm again.
        return Ok(Json(me_of(&state, account)?));
    }
    if !state.sent.may(account, "confirm", CONFIRM_EVERY) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::Refused,
            "A link was sent a moment ago. Wait a minute before asking for another.",
        ));
    }
    let secret = state.accounts.set_email(account, &address, now_ms())?;
    let name = server_name(&state);
    let mail = Mail {
        to: address,
        subject: format!("Confirm your email for {name}"),
        body: format!(
            "Someone gave this email address for an account on {name}.\n\
             \n\
             If that was you, open this link to confirm it:\n\
             \n\
             {url}/#/confirm-email/{secret}\n\
             \n\
             The link works once, and for one day.\n\
             \n\
             If it was not you, do nothing. The address is not used until it is confirmed.\n"
        ),
    };
    if let Err(why) = mailer.send(mail).await {
        eprintln!("gridfpv-server: {why}");
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            ErrorCode::Refused,
            "The email could not be sent. Check the address, or try again later.",
        ));
    }
    Ok(Json(me_of(&state, account)?))
}

/// `DELETE /api/me/email`.
pub async fn delete_my_email(
    State(state): State<AppState>,
    Signed(standing): Signed,
) -> Result<Json<Me>, ApiError> {
    state.accounts.clear_email(&standing.account)?;
    Ok(Json(me_of(&state, &standing.account)?))
}

/// `POST /api/email/confirm`: the link sent to an address was opened.
pub async fn post_confirm(
    State(state): State<AppState>,
    Json(given): Json<EmailConfirm>,
) -> Result<Json<EmailConfirmed>, ApiError> {
    match state.accounts.verify_email(&given.secret, now_ms())? {
        Some((_, address)) => Ok(Json(EmailConfirmed { address })),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            ErrorCode::UnknownScope,
            "That link does not work. It may have been used already, or have run out.",
        )),
    }
}

/// `POST /api/password/forgot`: send a link to set a new password.
///
/// **The answer is the same whoever is asked about**, so it cannot be used to find out
/// which usernames or addresses have accounts. A link goes only to an address the account
/// confirmed, and only to an account that signs in with a password.
pub async fn post_forgot(
    State(state): State<AppState>,
    Json(given): Json<Forgot>,
) -> Result<StatusCode, ApiError> {
    let (mailer, url) = ready(&state)?;
    if !state.book.password_sign_in() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "Signing in with a password is switched off on this server.",
        ));
    }
    let who = given.who.trim();
    let accounts = &state.accounts;
    // A username names one account. An address may be several accounts' own.
    let asked: Vec<AccountId> = if who.contains('@') {
        match clean_address(who) {
            Ok(address) => accounts.accounts_with_email(&address)?,
            Err(_) => Vec::new(),
        }
    } else {
        match crate::password::clean_username(who) {
            Ok(username) => accounts
                .password_of(&username)?
                .map(|(account, _)| account)
                .into_iter()
                .collect(),
            Err(_) => Vec::new(),
        }
    };
    let name = server_name(&state);
    let now = now_ms();
    for account in asked {
        let Some((address, true)) = accounts.email_of(&account)? else {
            continue;
        };
        let Some(username) = accounts.username_of(&account)? else {
            continue;
        };
        if !state.sent.may(&account, "reset", RESET_EVERY) {
            continue;
        }
        let (_, secret) = accounts.new_reconnect_for(&account, None, now, RESET_MS)?;
        let mail = Mail {
            to: address,
            subject: format!("Set a new password for {name}"),
            body: format!(
                "Someone asked to set a new password for the account \"{username}\" on {name}.\n\
                 \n\
                 If that was you, open this link, type your username and a new password:\n\
                 \n\
                 {url}/#/reconnect/{secret}\n\
                 \n\
                 The link works once, and for one hour.\n\
                 \n\
                 If it was not you, do nothing. Your password has not been changed.\n"
            ),
        };
        // The person asking is not told whether it went: only the owner's log is.
        if let Err(why) = mailer.send(mail).await {
            eprintln!("gridfpv-server: {why}");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> Result<Option<MailSettings>, String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (format!("GRIDFPV_SERVER_SMTP_{k}"), (*v).to_owned()))
            .collect();
        MailSettings::from_settings(|key| map.get(key).cloned())
    }

    #[test]
    fn with_no_host_set_there_is_no_email() {
        assert_eq!(settings(&[]), Ok(None));
        assert_eq!(settings(&[("FROM", "Grid <grid@example.org>")]), Ok(None));
        assert_eq!(settings(&[("HOST", "  ")]), Ok(None));
    }

    #[test]
    fn the_usual_settings_are_read_with_the_usual_port() {
        let got = settings(&[
            ("HOST", " smtp.example.org "),
            ("USER", "grid@example.org"),
            ("PASSWORD", "app-password"),
            ("FROM", "Grid <grid@example.org>"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(got.host, "smtp.example.org");
        assert_eq!((got.port, got.tls), (587, MailTls::StartTls));
        assert_eq!(got.user.as_deref(), Some("grid@example.org"));
        // TLS from the first byte has a port of its own.
        let tls = settings(&[
            ("HOST", "smtp.example.org"),
            ("FROM", "g@example.org"),
            ("TLS", "TLS"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!((tls.port, tls.tls), (465, MailTls::Tls));
        let port = settings(&[
            ("HOST", "smtp.example.org"),
            ("FROM", "g@example.org"),
            ("PORT", "2525"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(port.port, 2525);
    }

    #[test]
    fn settings_that_do_not_make_sense_say_what_is_wrong() {
        for (pairs, word) in [
            (vec![("HOST", "smtp.example.org")], "FROM"),
            (
                vec![("HOST", "smtp.example.org"), ("FROM", "not an address")],
                "FROM",
            ),
            (
                vec![
                    ("HOST", "smtp.example.org"),
                    ("FROM", "g@example.org"),
                    ("PORT", "many"),
                ],
                "PORT",
            ),
            (
                vec![
                    ("HOST", "smtp.example.org"),
                    ("FROM", "g@example.org"),
                    ("TLS", "maybe"),
                ],
                "TLS",
            ),
            // No TLS only to this same computer.
            (
                vec![
                    ("HOST", "smtp.example.org"),
                    ("FROM", "g@example.org"),
                    ("TLS", "none"),
                ],
                "same computer",
            ),
        ] {
            let why = settings(&pairs).unwrap_err();
            assert!(why.contains(word), "{pairs:?}: {why}");
        }
        let local = settings(&[
            ("HOST", "127.0.0.1"),
            ("FROM", "g@example.org"),
            ("TLS", "none"),
        ]);
        assert_eq!(local.unwrap().unwrap().tls, MailTls::None);
    }

    #[test]
    fn an_address_is_tidied_and_what_is_not_one_is_refused() {
        assert_eq!(
            clean_address("  Pia@Example.ORG ").ok().as_deref(),
            Some("pia@example.org")
        );
        assert!(clean_address("pia+grid@mail.example.org").is_ok());
        for bad in [
            "",
            "pia",
            "pia@",
            "@example.org",
            "pia@localhost",
            "pia @example.org",
            "pia@example.org\nBcc: x@example.org",
            "Pia <pia@example.org>",
            &format!("{}@example.org", "x".repeat(250)),
        ] {
            assert!(clean_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_same_account_is_not_sent_the_same_kind_of_mail_twice_in_a_row() {
        let sent = Sent::default();
        let (ace, bee) = (AccountId("a".into()), AccountId("b".into()));
        let every = Duration::from_secs(60);
        assert!(sent.may(&ace, "reset", every));
        assert!(!sent.may(&ace, "reset", every));
        // Another kind, and another account, are counted apart.
        assert!(sent.may(&ace, "confirm", every));
        assert!(sent.may(&bee, "reset", every));
        // Once the wait is over, it may again.
        assert!(sent.may(&ace, "reset", Duration::ZERO));
    }

    #[tokio::test]
    async fn an_outbox_keeps_what_it_is_given_in_order() {
        let mailer = Mailer::outbox();
        for n in 0..3 {
            let mail = Mail {
                to: "pia@example.org".into(),
                subject: format!("mail {n}"),
                body: "x".into(),
            };
            mailer.send(mail).await.unwrap();
        }
        let subjects: Vec<String> = mailer.sent().into_iter().map(|m| m.subject).collect();
        assert_eq!(subjects, ["mail 0", "mail 1", "mail 2"]);
    }
}
