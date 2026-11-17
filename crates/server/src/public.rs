//! What a visitor on the network may ask for (docs/primitives.md §11).
//!
//! Everything under `/public` is written for someone who is not the director: it gives out
//! only what a spectator may see, and it never takes a change. The one exception is
//! `/public/session`, which is how a director on another device shows their access token
//! and stops being a visitor.
//!
//! - `GET /public/hello` — who the Director takes the caller for, and what is on.
//! - `POST /public/session` — sign in as the director from another device.
//! - `DELETE /public/session` — sign out again.

use axum::Json;
use axum::extract::{Extension, State};
use axum::http::header::{AUTHORIZATION, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::access::{Access, Origin, SESSION_COOKIE, cookie_token};
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// Who the Director takes a caller for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Caller {
    /// On the Grid computer itself. The whole console.
    Local,
    /// On another device, with the director's access token. The whole console.
    Director,
    /// On another device, with no token. The public pages only.
    Visitor,
}

/// `GET /public/hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PublicHello {
    /// The Grid build that is answering.
    pub version: String,
    pub caller: Caller,
    /// The name of the event that is open, if one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub event: Option<String>,
    /// Whether a director can sign in from another device at all: an access token is set
    /// on this Director. With none set, the console is used on the Grid computer only.
    pub director_sign_in: bool,
    /// The pilot this browser is signed in as, for the open event. Absent when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pilot: Option<crate::pilot_access::PublicPilot>,
    /// Whether the director has set this device up as a kiosk.
    #[serde(default)]
    pub kiosk: bool,
    /// What the director calls this kiosk: its name, or "Kiosk 2". Absent off a kiosk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kiosk_name: Option<String>,
}

/// How long a director stays signed in on another device: half a day, which covers a race
/// day. It also ends when the Director is closed.
const SESSION_SECONDS: u32 = 12 * 60 * 60;

/// Who a request is from. A router with no gate around it (a test) has no [`Access`]; that
/// is the Grid computer talking to itself.
fn caller(access: Option<Access>) -> Caller {
    match access {
        None => Caller::Local,
        Some(access) if access.origin == Origin::Local => Caller::Local,
        Some(access) if access.director => Caller::Director,
        Some(_) => Caller::Visitor,
    }
}

/// `GET /public/hello` — who the caller is taken for, and the open event's name.
pub async fn get_hello(
    State(registry): State<EventRegistry>,
    access: Option<Extension<Access>>,
    headers: HeaderMap,
) -> Json<PublicHello> {
    Json(PublicHello {
        version: crate::BUILD_VERSION.to_string(),
        caller: caller(access.map(|Extension(access)| access)),
        event: registry.active().map(|event| event.name),
        director_sign_in: registry.tokens().has_control_credential(),
        pilot: crate::pilot_access::signed_in_pilot(&registry, &headers),
        kiosk: crate::pilot_access::is_kiosk_device(&registry, &headers),
        kiosk_name: crate::pilot_access::kiosk_label(&registry, &headers),
    })
}

fn cookie(value: &str, max_age: u32) -> String {
    // HttpOnly: no script can read it. SameSite=Strict: no other site's page can send it.
    format!("{SESSION_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict")
}

/// The token in a `Bearer` header. The session cookie does not count here: signing in
/// takes the director's own token, not an earlier sign-in.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let (scheme, token) = headers.get(AUTHORIZATION)?.to_str().ok()?.split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty()).then_some(token)
}

/// `POST /public/session` — sign in as the director from a browser.
///
/// Send the director's access token as `Authorization: Bearer …`. A right token answers
/// `204` and sets a cookie the browser then sends by itself, so the pages that cannot add a
/// header (the change feed, a video) work too. The cookie holds a sign-in of its own, not
/// the access token. A wrong token answers `401`.
pub async fn post_session(
    State(registry): State<EventRegistry>,
    access: Option<Extension<Access>>,
    headers: HeaderMap,
) -> Result<Response, ProtocolError> {
    let tokens = registry.tokens();
    let wrong = || {
        ProtocolError::new(
            ErrorCode::Unauthorized,
            "That is not the director's access token.",
        )
    };
    let Some(token) = bearer(&headers) else {
        return Err(wrong());
    };
    // A browser sign-in cannot be used to make another one.
    let known = tokens
        .session(token)
        .is_some_and(|session| session.role.can_control() && !session.browser);
    // For a visitor the gate has already checked the token, and counted a wrong one. Its
    // answer is the one that stands: an address that has guessed too often is refused even
    // when it finally sends the right token.
    let access = access.map(|Extension(access)| access);
    // Say that it is a wait, and not that the token is wrong: a director with the right
    // token must not be told it is the wrong one.
    if access.is_some_and(|access| access.blocked) {
        return Err(ProtocolError::new(
            ErrorCode::TooManyRequests,
            "Too many wrong access tokens from this device. Wait a minute and try again.",
        ));
    }
    let allowed = match access {
        Some(access) if access.origin == Origin::Network => access.director && known,
        _ => known,
    };
    if !allowed {
        return Err(wrong());
    }
    let session = tokens.issue_browser_session();
    Ok((
        StatusCode::NO_CONTENT,
        [(SET_COOKIE, cookie(&session, SESSION_SECONDS))],
    )
        .into_response())
}

/// `DELETE /public/session` — sign out. The cookie's sign-in stops working at once, here
/// and in every other tab that has it. Always `204`, signed in or not.
pub async fn delete_session(State(registry): State<EventRegistry>, headers: HeaderMap) -> Response {
    if let Some(session) = cookie_token(&headers) {
        registry.tokens().end_browser_session(&session);
    }
    (StatusCode::NO_CONTENT, [(SET_COOKIE, cookie("", 0))]).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_is_named_from_what_the_gate_found() {
        assert_eq!(caller(None), Caller::Local);
        let access = |origin, director| {
            Some(Access {
                origin,
                director,
                blocked: false,
            })
        };
        assert_eq!(caller(access(Origin::Local, false)), Caller::Local);
        // A token shown from the Grid computer changes nothing: it is local either way.
        assert_eq!(caller(access(Origin::Local, true)), Caller::Local);
        assert_eq!(caller(access(Origin::Network, true)), Caller::Director);
        assert_eq!(caller(access(Origin::Network, false)), Caller::Visitor);
    }

    #[test]
    fn the_cookie_cannot_be_read_by_a_script_or_sent_by_another_site() {
        let set = cookie("abc", SESSION_SECONDS);
        assert!(set.starts_with("gridfpv_director=abc; "));
        for part in ["Path=/", "Max-Age=43200", "HttpOnly", "SameSite=Strict"] {
            assert!(set.contains(part), "{part} in {set}");
        }
        // Signing out sends an empty one that ends at once.
        assert!(cookie("", 0).contains("gridfpv_director=; "));
        assert!(cookie("", 0).contains("Max-Age=0"));
    }

    #[test]
    fn only_a_bearer_header_is_a_sign_in_token() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, value.parse().unwrap());
            headers
        };
        assert_eq!(bearer(&with("Bearer abc")), Some("abc"));
        assert_eq!(bearer(&with("bearer  abc ")), Some("abc"));
        assert_eq!(bearer(&with("Basic abc")), None);
        assert_eq!(bearer(&with("Bearer ")), None);
        assert_eq!(bearer(&with("abc")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }
}
