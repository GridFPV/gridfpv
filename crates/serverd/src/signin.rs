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
//! | `GET /api/server/sign-in-services` | an owner reads how each service is set up |
//! | `PUT /api/server/sign-in-services/{id}` | an owner sets up Discord or Google |
//! | `DELETE /api/server/sign-in-services/{id}` | an owner takes that back |
//!
//! Discord and Google are set up on the server's page by an owner, or in the environment.
//! What is set on the page wins, and is used from the next sign-in on, with no restart.
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
use crate::people::{MaybeSigned, Signed, now_ms, session_cookie};
use crate::push::ApiError;
use gridfpv_server::error::ErrorCode;

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

/// The two values of an app registered with a provider, as an owner gave them on the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppKeys {
    pub client_id: String,
    pub client_secret: String,
}

/// The providers an owner can set up on the page: the ones this build knows by name.
pub const PAGE_PROVIDERS: [(&str, &str); 2] = [("discord", "Discord"), ("google", "Google")];

impl Provider {
    /// One of the providers known by name, with an app's two values.
    fn named(id: &str, keys: AppKeys) -> Option<Self> {
        match id {
            "discord" => Some(Self::discord(keys.client_id, keys.client_secret)),
            "google" => Some(Self::google(keys.client_id, keys.client_secret)),
            _ => None,
        }
    }
}

/// The providers people can sign in with right now. One set up on the page is used in
/// place of the same one in the environment. The order is always Discord, Google, then a
/// service of the owner's own.
pub fn providers_now(state: &AppState) -> Vec<Provider> {
    let mut page = state.book.sign_in_apps();
    let mut found = Vec::new();
    for (id, _) in PAGE_PROVIDERS {
        let set = page
            .remove(id)
            .and_then(|keys| Provider::named(id, keys))
            .map(|made| match &state.provider_shape {
                Some(shape) => shape(made),
                None => made,
            });
        let env = || state.providers.iter().find(|p| p.id == id).cloned();
        if let Some(provider) = set.or_else(env) {
            found.push(provider);
        }
    }
    let named = |id: &str| PAGE_PROVIDERS.iter().any(|(known, _)| *known == id);
    found.extend(state.providers.iter().filter(|p| !named(&p.id)).cloned());
    found
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
        providers_now(&state)
            .into_iter()
            .map(|p| SignInProvider {
                id: p.id,
                name: p.name,
            })
            .collect(),
    )
}

// ── Setting a service up on the page ────────────────────────────────────────

/// Where a sign-in service's settings come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
#[serde(rename_all = "lowercase")]
pub enum ServiceSource {
    /// An owner set them on the page.
    Page,
    /// They are in the server's environment.
    Env,
    /// Nowhere: nobody signs in with this service here.
    Off,
}

/// A sign-in service, as an owner is shown it. The app's secret is never here: only
/// whether there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ServerSignInService {
    /// `discord`, `google`, or `oidc` for a service of the owner's own.
    pub id: String,
    /// The name on the button.
    pub name: String,
    pub source: ServiceSource,
    /// Whether it can be set up on the page. A service of the owner's own cannot: it is
    /// set in the environment.
    pub editable: bool,
    /// The app's client id. It is not a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub client_id: Option<String>,
    /// Whether the app's secret is kept.
    pub secret_set: bool,
    /// The address to give the provider as the app's redirect. Not there when the server
    /// does not know its own address (`GRIDFPV_SERVER_PUBLIC_URL`): then nobody can sign
    /// in with any service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub redirect: Option<String>,
}

/// The two values of an app, as an owner gives them. A secret left out keeps the one
/// that is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ServerSignInServiceChange {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub client_secret: Option<String>,
}

fn owner_only(standing: &crate::accounts::Standing) -> Result<(), ApiError> {
    if standing.may(crate::accounts::Act::RunServer) {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::FORBIDDEN,
        ErrorCode::Forbidden,
        "Only an owner of this server can do that.",
    ))
}

fn redirect_for(state: &AppState, id: &str) -> Option<String> {
    state
        .public_url
        .as_ref()
        .map(|base| format!("{base}/auth/{id}/callback"))
}

fn services(state: &AppState) -> Vec<ServerSignInService> {
    let page = state.book.sign_in_apps();
    let mut out = Vec::new();
    for (id, name) in PAGE_PROVIDERS {
        let env = state.providers.iter().find(|p| p.id == id);
        let (source, client_id) = match (page.get(id), env) {
            (Some(keys), _) => (ServiceSource::Page, Some(keys.client_id.clone())),
            (None, Some(env)) => (ServiceSource::Env, Some(env.client_id.clone())),
            (None, None) => (ServiceSource::Off, None),
        };
        out.push(ServerSignInService {
            id: id.to_owned(),
            name: name.to_owned(),
            source,
            editable: true,
            secret_set: source != ServiceSource::Off,
            client_id,
            redirect: redirect_for(state, id),
        });
    }
    let named = |id: &str| PAGE_PROVIDERS.iter().any(|(known, _)| *known == id);
    for other in state.providers.iter().filter(|p| !named(&p.id)) {
        out.push(ServerSignInService {
            id: other.id.clone(),
            name: other.name.clone(),
            source: ServiceSource::Env,
            editable: false,
            client_id: Some(other.client_id.clone()),
            secret_set: true,
            redirect: redirect_for(state, &other.id),
        });
    }
    out
}

/// The longest a client id or a secret may be. The real ones are far shorter.
const KEY_MAX: usize = 300;

/// One of an app's two values, as typed or pasted: no spaces round it, and nothing in it
/// that a value from a provider never has.
fn clean_key(value: &str, what: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ApiError::bad(format!("Give the {what}.")));
    }
    if value.len() > KEY_MAX || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::bad(format!(
            "That does not look like a {what}. Copy it again from the provider's page."
        )));
    }
    Ok(value.to_owned())
}

fn editable(id: &str) -> Result<(), ApiError> {
    if PAGE_PROVIDERS.iter().any(|(known, _)| *known == id) {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::NOT_FOUND,
        ErrorCode::UnknownScope,
        "This sign-in service cannot be set up on the page.",
    ))
}

/// `GET /api/server/sign-in-services`.
pub async fn get_server_services(
    State(state): State<AppState>,
    Signed(standing): Signed,
) -> Result<Json<Vec<ServerSignInService>>, ApiError> {
    owner_only(&standing)?;
    Ok(Json(services(&state)))
}

/// `PUT /api/server/sign-in-services/{id}`: set the app people sign in through. It is
/// used from the next sign-in on. Nothing is asked of the provider to check it.
pub async fn put_server_service(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(id): Path<String>,
    Json(change): Json<ServerSignInServiceChange>,
) -> Result<Json<Vec<ServerSignInService>>, ApiError> {
    owner_only(&standing)?;
    editable(&id)?;
    let client_id = clean_key(&change.client_id, "client ID")?;
    let given = change.client_secret.as_deref().map(str::trim).unwrap_or("");
    let client_secret = if given.is_empty() {
        // Nothing typed keeps the secret that was set on the page.
        match state.book.sign_in_apps().remove(&id) {
            Some(kept) => kept.client_secret,
            None => return Err(ApiError::bad("Give the client secret.")),
        }
    } else {
        clean_key(given, "client secret")?
    };
    state
        .book
        .set_sign_in_app(
            &id,
            Some(AppKeys {
                client_id,
                client_secret,
            }),
        )
        .map_err(crate::manage::book_error)?;
    Ok(Json(services(&state)))
}

/// `DELETE /api/server/sign-in-services/{id}`: forget what was set on the page. Settings
/// in the environment, if there are any, are used again.
///
/// With none there, nobody signs in with the service any more. An owner is not let to do
/// that when it is their only way in: they would have no way back but the command line.
pub async fn delete_server_service(
    State(state): State<AppState>,
    Signed(standing): Signed,
    Path(id): Path<String>,
) -> Result<Json<Vec<ServerSignInService>>, ApiError> {
    owner_only(&standing)?;
    editable(&id)?;
    let in_env = state.providers.iter().any(|p| p.id == id);
    if !in_env && state.book.sign_in_apps().contains_key(&id) {
        let mine = state.accounts.sign_ins(&standing.account)?;
        let password = state.book.password_sign_in();
        // A way in counts only when it would still work with this service gone.
        let left: Vec<String> = providers_now(&state)
            .into_iter()
            .filter(|p| p.id != id)
            .map(|p| p.id)
            .collect();
        let other_way = mine.iter().any(|held| {
            if held.provider == crate::accounts::PASSWORD_PROVIDER {
                password
            } else {
                left.contains(&held.provider)
            }
        });
        if !other_way {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                ErrorCode::Refused,
                "This is the only way you sign in. Add another way to sign in first, or you would lock yourself out.",
            ));
        }
    }
    state
        .book
        .set_sign_in_app(&id, None)
        .map_err(crate::manage::book_error)?;
    Ok(Json(services(&state)))
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
    let Some(provider) = providers_now(&state)
        .into_iter()
        .find(|p| p.id == provider)
        .filter(|_| state.public_url.is_some())
    else {
        return go_problem(&state, &headers, Problem::NotSetUp);
    };
    let provider = &provider;
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
    let Some(provider) = providers_now(&state)
        .into_iter()
        .find(|p| p.id == provider)
        .filter(|_| state.public_url.is_some())
    else {
        return go_problem(&state, &headers, Problem::NotSetUp);
    };
    let provider = &provider;
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
