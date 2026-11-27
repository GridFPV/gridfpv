//! Signing in with a provider (docs/primitives.md §13.8): Discord and Google.
//!
//! A person proves who they are to a provider they already have an account with, and the
//! provider tells this server. Grid never sees a password.
//!
//! | route | what |
//! |-------|------|
//! | `GET /api/sign-in/providers` | which providers this server is set up for |
//! | `GET /auth/{provider}/start` | send the browser to the provider |
//! | `GET /auth/{provider}/callback` | the provider sends the browser back here |
//!
//! `start` takes `?invite=<secret>` when the person came by an invite link, and `?add=1`
//! when a signed-in account is adding another way to sign in.
//!
//! # What comes of coming back
//!
//! - **The sign-in belongs to an account:** the browser is signed in as it. An invite the
//!   person came with is used as that account.
//! - **It belongs to none, and the person came with an invite that works:** an account is
//!   made, with what the invite gives.
//! - **It belongs to none, and there is no invite:** nothing is made, unless the server's
//!   owner switched open sign-up on (§13.7). Then an account is made, in no club.
//! - **Adding:** it is attached to the account that asked, if that account is still the
//!   one signed in and no other account has it.
//!
//! A sign-in is matched by the provider's own id for the person and by nothing else. Not
//! by a name, and not by an email.
//!
//! # How the round trip is kept honest
//!
//! - **`state`:** a value made here, sent out with the browser, and expected back. It works
//!   once and for ten minutes.
//! - **The browser must be the one that left.** `start` gives it a cookie that only the
//!   same browser brings back. A link to `callback` sent to someone else does nothing.
//! - **PKCE:** the provider is given the hash of a secret at the start, and the secret
//!   itself only when the code is exchanged, so a stolen code is no use.
//! - **The browser is only ever sent to a fixed place on this server afterwards.**

use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::accounts::{AccountError, NewSignIn, PendingSignIn, RECONNECT_PREFIX};
use crate::app::AppState;
use crate::people::{MaybeSigned, now_ms, session_cookie};

/// The cookie that says which browser started a sign-in.
pub const SIGNIN_COOKIE: &str = "grid_signin";

/// A provider this server can ask who someone is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// The name in addresses and in the records: `discord`, `google`.
    pub id: String,
    /// The name on the button.
    pub name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub user_url: String,
    pub scope: String,
    pub client_id: String,
    pub client_secret: String,
    /// The field of the provider's answer that is its own id for the person.
    pub subject_field: String,
    /// The fields to show the person by, the first that is there.
    pub label_fields: Vec<String>,
}

impl Provider {
    /// Discord, with the app's two values. The `identify` scope gives the id and the
    /// username, and no email.
    pub fn discord(client_id: String, client_secret: String) -> Self {
        Self {
            id: "discord".into(),
            name: "Discord".into(),
            authorize_url: "https://discord.com/oauth2/authorize".into(),
            token_url: "https://discord.com/api/oauth2/token".into(),
            user_url: "https://discord.com/api/v10/users/@me".into(),
            scope: "identify".into(),
            client_id,
            client_secret,
            subject_field: "id".into(),
            label_fields: vec!["global_name".into(), "username".into()],
        }
    }

    /// Google, with the app's two values. `openid profile` gives the id (`sub`) and a
    /// name, and no email.
    pub fn google(client_id: String, client_secret: String) -> Self {
        Self {
            id: "google".into(),
            name: "Google".into(),
            authorize_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            user_url: "https://openidconnect.googleapis.com/v1/userinfo".into(),
            scope: "openid profile".into(),
            client_id,
            client_secret,
            subject_field: "sub".into(),
            label_fields: vec!["name".into()],
        }
    }

    /// A sign-in service of the server owner's own choosing that speaks OpenID Connect: a
    /// club's own, or one this build has no name for. `openid profile` is asked for unless
    /// another scope is given. The person's id is `sub`.
    pub fn custom(custom: Custom) -> Self {
        Self {
            id: "oidc".into(),
            name: custom.name,
            authorize_url: custom.authorize_url,
            token_url: custom.token_url,
            user_url: custom.userinfo_url,
            scope: custom.scope.unwrap_or_else(|| "openid profile".into()),
            client_id: custom.client_id,
            client_secret: custom.client_secret,
            subject_field: "sub".into(),
            label_fields: vec!["name".into(), "preferred_username".into()],
        }
    }

    /// The providers set up in the environment. Each needs all of its settings, or it is
    /// left off.
    ///
    /// - `GRIDFPV_SERVER_DISCORD_CLIENT_ID` and `GRIDFPV_SERVER_DISCORD_CLIENT_SECRET`
    /// - `GRIDFPV_SERVER_GOOGLE_CLIENT_ID` and `GRIDFPV_SERVER_GOOGLE_CLIENT_SECRET`
    /// - `GRIDFPV_SERVER_OIDC_NAME`, `…_OIDC_AUTHORIZE_URL`, `…_OIDC_TOKEN_URL`,
    ///   `…_OIDC_USERINFO_URL`, `…_OIDC_CLIENT_ID`, `…_OIDC_CLIENT_SECRET`, and
    ///   optionally `…_OIDC_SCOPE`
    pub fn from_env() -> Vec<Self> {
        Self::from_settings(|key| std::env::var(key).ok())
    }

    /// [`from_env`](Self::from_env), reading settings from anywhere.
    pub fn from_settings(read: impl Fn(&str) -> Option<String>) -> Vec<Self> {
        let get = |key: &str| {
            read(&format!("GRIDFPV_SERVER_{key}"))
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let mut found = Vec::new();
        if let (Some(id), Some(secret)) = (get("DISCORD_CLIENT_ID"), get("DISCORD_CLIENT_SECRET")) {
            found.push(Self::discord(id, secret));
        }
        if let (Some(id), Some(secret)) = (get("GOOGLE_CLIENT_ID"), get("GOOGLE_CLIENT_SECRET")) {
            found.push(Self::google(id, secret));
        }
        // An address for the provider's own pages must be a web address. HTTPS, or this
        // same computer.
        let address = |key: &str| {
            get(key).filter(|url| {
                url.starts_with("https://")
                    || url.starts_with("http://127.0.0.1")
                    || url.starts_with("http://localhost")
                    || url.starts_with("http://[::1]")
            })
        };
        let custom = (|| {
            Some(Custom {
                name: get("OIDC_NAME")?
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(40)
                    .collect(),
                authorize_url: address("OIDC_AUTHORIZE_URL")?,
                token_url: address("OIDC_TOKEN_URL")?,
                userinfo_url: address("OIDC_USERINFO_URL")?,
                client_id: get("OIDC_CLIENT_ID")?,
                client_secret: get("OIDC_CLIENT_SECRET")?,
                scope: get("OIDC_SCOPE"),
            })
        })();
        if let Some(custom) = custom {
            found.push(Self::custom(custom));
        }
        found
    }
}

/// The settings of a sign-in service of the server owner's own choosing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Custom {
    /// The name on the button.
    pub name: String,
    pub authorize_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: Option<String>,
}

/// A provider, for the page's buttons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SignInProvider {
    pub id: String,
    pub name: String,
}

/// `GET /api/sign-in/providers`. Empty when signing in is not set up: no provider, or the
/// server does not know its own address to be sent back to.
pub async fn get_providers(State(state): State<AppState>) -> Json<Vec<SignInProvider>> {
    if state.public_url.is_none() {
        return Json(Vec::new());
    }
    Json(
        state
            .providers
            .iter()
            .map(|p| SignInProvider {
                id: p.id.clone(),
                name: p.name.clone(),
            })
            .collect(),
    )
}

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the operating system gives random bytes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// The value a provider is shown at the start for a PKCE secret: its SHA-256, in base64url.
pub fn challenge_of(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Why a sign-in did not go through, as the page is told in the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Signing in with this provider is not set up here.
    NotSetUp,
    /// The person said no at the provider, or the provider refused.
    Refused,
    /// The round trip did not hold together: too slow, another browser, done twice.
    Expired,
    /// The provider could not be asked, or its answer made no sense.
    Provider,
    /// There is no account for this sign-in, and no invite to make one with.
    NeedInvite,
    /// The invite the person came with does not work.
    Invite,
    /// The re-connect link the person came with does not work.
    Link,
    /// The sign-in being added already belongs to another account.
    Taken,
    /// Adding was asked for, and nobody is signed in to add to.
    NotSignedIn,
}

impl Problem {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotSetUp => "not-set-up",
            Self::Refused => "refused",
            Self::Expired => "expired",
            Self::Provider => "provider",
            Self::NeedInvite => "need-invite",
            Self::Invite => "invite",
            Self::Link => "link",
            Self::Taken => "taken",
            Self::NotSignedIn => "not-signed-in",
        }
    }
}

fn base(state: &AppState) -> &str {
    state.public_url.as_deref().unwrap_or("")
}

/// Send the browser to a page of this server. Only ever one of these fixed places.
fn go(state: &AppState, hash: &str) -> Response {
    let to = format!("{}/#/{hash}", base(state));
    (StatusCode::SEE_OTHER, [(LOCATION, to)]).into_response()
}

fn go_problem(state: &AppState, headers: &HeaderMap, problem: Problem) -> Response {
    let mut response = go(state, &format!("sign-in?problem={}", problem.code()));
    response
        .headers_mut()
        .append(SET_COOKIE, signin_cookie(state, headers, None));
    response
}

fn secure(state: &AppState, headers: &HeaderMap) -> &'static str {
    let https = match &state.public_url {
        Some(url) => url.starts_with("https://"),
        None => headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|proto| proto.eq_ignore_ascii_case("https")),
    };
    if https { "; Secure" } else { "" }
}

/// The cookie that ties a sign-in to the browser that started it. `None` clears it.
fn signin_cookie(state: &AppState, headers: &HeaderMap, value: Option<&str>) -> HeaderValue {
    let secure = secure(state, headers);
    let text = match value {
        Some(value) => format!(
            "{SIGNIN_COOKIE}={value}; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=600{secure}"
        ),
        None => format!("{SIGNIN_COOKIE}=; Path=/auth; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
    };
    HeaderValue::from_str(&text).expect("plain text")
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(found, _)| *found == name)
        .map(|(_, value)| value)
        .filter(|value| !value.is_empty())
}

fn redirect_uri(state: &AppState, provider: &Provider) -> String {
    format!("{}/auth/{}/callback", base(state), provider.id)
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    invite: Option<String>,
    #[serde(default)]
    add: Option<String>,
}

/// `GET /auth/{provider}/start`.
pub async fn get_start(
    State(state): State<AppState>,
    MaybeSigned(standing): MaybeSigned,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(provider) = state
        .providers
        .iter()
        .find(|p| p.id == provider)
        .filter(|_| state.public_url.is_some())
    else {
        return go_problem(&state, &headers, Problem::NotSetUp);
    };
    let adding = query.add.is_some();
    let add_to = match (adding, standing) {
        (true, Some(standing)) => Some(standing.account),
        (true, None) => return go_problem(&state, &headers, Problem::NotSignedIn),
        (false, _) => None,
    };
    let state_value = random(24);
    let verifier = random(32);
    let pending = PendingSignIn {
        provider: provider.id.clone(),
        verifier: verifier.clone(),
        invite: query.invite.filter(|invite| !invite.is_empty()),
        add_to,
    };
    if state
        .accounts
        .start_sign_in(&state_value, &pending, now_ms())
        .is_err()
    {
        return go_problem(&state, &headers, Problem::Provider);
    }
    let to = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        provider.authorize_url,
        encode(&provider.client_id),
        encode(&redirect_uri(&state, provider)),
        encode(&provider.scope),
        encode(&state_value),
        encode(&challenge_of(&verifier)),
    );
    (
        StatusCode::SEE_OTHER,
        [
            (
                LOCATION,
                HeaderValue::from_str(&to).expect("an encoded address"),
            ),
            (
                SET_COOKIE,
                signin_cookie(&state, &headers, Some(&state_value)),
            ),
        ],
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Who a provider says someone is.
struct Vouched {
    subject: String,
    label: String,
}

/// Exchange the code for a token, and ask the provider who it is for.
async fn ask_provider(
    provider: &Provider,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Option<Vouched> {
    let http = gridfpv_server::http::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .ok()?;
    let token: serde_json::Value = http
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", provider.client_id.as_str()),
            ("client_secret", provider.client_secret.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let access = token.get("access_token")?.as_str()?;
    let user: serde_json::Value = http
        .get(&provider.user_url)
        .bearer_auth(access)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    // The id is text for both providers today. A number is taken as its digits; anything
    // else is not an id.
    let subject = match user.get(&provider.subject_field)? {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => number.to_string(),
        _ => return None,
    };
    if subject.trim().is_empty() {
        return None;
    }
    let label = provider
        .label_fields
        .iter()
        .filter_map(|field| user.get(field)?.as_str())
        .map(str::trim)
        .find(|text| !text.is_empty())
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    Some(Vouched { subject, label })
}

/// `GET /auth/{provider}/callback`.
pub async fn get_callback(
    State(state): State<AppState>,
    MaybeSigned(standing): MaybeSigned,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
    headers: HeaderMap,
) -> Response {
    let now = now_ms();
    let Some(provider) = state
        .providers
        .iter()
        .find(|p| p.id == provider)
        .filter(|_| state.public_url.is_some())
    else {
        return go_problem(&state, &headers, Problem::NotSetUp);
    };
    // Whatever else is true, a `state` is used up by being shown here.
    let given = query.state.as_deref().unwrap_or("");
    let pending = state.accounts.take_sign_in(given, now).ok().flatten();
    let same_browser = cookie(&headers, SIGNIN_COOKIE).is_some_and(|mine| mine == given);
    let Some(pending) = pending.filter(|p| same_browser && p.provider == provider.id) else {
        return go_problem(&state, &headers, Problem::Expired);
    };
    if query.error.is_some() {
        return go_problem(&state, &headers, Problem::Refused);
    }
    let Some(code) = query.code.as_deref().filter(|code| !code.is_empty()) else {
        return go_problem(&state, &headers, Problem::Refused);
    };
    let Some(vouched) = ask_provider(
        provider,
        &redirect_uri(&state, provider),
        code,
        &pending.verifier,
    )
    .await
    else {
        return go_problem(&state, &headers, Problem::Provider);
    };
    let sign_in = NewSignIn {
        provider: &provider.id,
        subject: &vouched.subject,
        label: &vouched.label,
    };
    let accounts = &state.accounts;
    let done = |hash: &str, session: Option<&str>| -> Response {
        let mut response = go(&state, hash);
        let out = response.headers_mut();
        out.append(SET_COOKIE, signin_cookie(&state, &headers, None));
        if let Some(session) = session {
            out.append(SET_COOKIE, session_cookie(&state, &headers, session));
        }
        response
    };

    // Adding a sign-in to the account that asked.
    if let Some(account) = &pending.add_to {
        if standing.as_ref().map(|s| &s.account) != Some(account) {
            return go_problem(&state, &headers, Problem::NotSignedIn);
        }
        return match accounts.add_sign_in(account, &sign_in, now) {
            // Back on the page they added it from: how the account signs in.
            Ok(()) => done("account/sign-in", None),
            Err(AccountError::SignInTaken) => go_problem(&state, &headers, Problem::Taken),
            Err(_) => go_problem(&state, &headers, Problem::Provider),
        };
    }

    // Getting back in with a re-connect link an owner made (§13.8): the sign-in goes on
    // that link's account. One that another account holds is refused, and nobody is
    // signed in: the person meant to bring a sign-in they are not already using here.
    if let Some(link) = pending
        .invite
        .as_deref()
        .filter(|secret| secret.starts_with(RECONNECT_PREFIX))
    {
        return match accounts.reconnect(link, &sign_in, now) {
            Ok((_, session)) => done("account/sign-in", Some(&session)),
            Err(AccountError::SignInTaken) => go_problem(&state, &headers, Problem::Taken),
            Err(AccountError::ReconnectDoesNotWork) => go_problem(&state, &headers, Problem::Link),
            Err(_) => go_problem(&state, &headers, Problem::Provider),
        };
    }

    let Ok(known) = accounts.account_for_sign_in(&provider.id, &vouched.subject) else {
        return go_problem(&state, &headers, Problem::Provider);
    };
    match (known, pending.invite.as_deref()) {
        // An account signs in. An invite it came with is used as that account.
        (Some(account), invite) => {
            let _ = accounts.refresh_label(&provider.id, &vouched.subject, &vouched.label);
            let Ok(session) = accounts.new_session(&account.id, now) else {
                return go_problem(&state, &headers, Problem::Provider);
            };
            match invite.map(|invite| accounts.accept_invite(invite, &account.id, now)) {
                // Signed in all the same, and told the invite did nothing.
                Some(Err(_)) => done("sign-in?problem=invite", Some(&session)),
                _ => done("", Some(&session)),
            }
        }
        // Nobody yet: an invite makes the account.
        (None, Some(invite)) => {
            let name = if vouched.label.is_empty() {
                "Pilot"
            } else {
                &vouched.label
            };
            match accounts.join_with_invite(invite, &sign_in, name, now) {
                Ok((_, _, session)) => done("", Some(&session)),
                Err(AccountError::InviteDoesNotWork) => {
                    go_problem(&state, &headers, Problem::Invite)
                }
                Err(_) => go_problem(&state, &headers, Problem::Provider),
            }
        }
        // Nobody yet, and no invite. The owner says whether that is enough (§13.7).
        (None, None) if state.book.open_sign_up() => {
            let name = if vouched.label.is_empty() {
                "Pilot"
            } else {
                &vouched.label
            };
            match accounts.join_open(&sign_in, name, now) {
                // A new account has a profile to fill in: they land on their account page.
                Ok((_, session)) => done("account", Some(&session)),
                Err(_) => go_problem(&state, &headers, Problem::Provider),
            }
        }
        (None, None) => go_problem(&state, &headers, Problem::NeedInvite),
    }
}
