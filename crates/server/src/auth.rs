//! Auth & authorization — the policy gate in front of the HTTP API.
//!
//! The **read** half is open on the LAN, while every **write** requires the RD's
//! authenticated role once a token is configured. The pieces:
//!
//! - **Opaque bearer tokens** back a server-side [`Session`] carrying a [`Role`]. They
//!   are opaque random strings (not self-describing JWTs) so the server is the only
//!   authority and a token is **revoked** by deleting its session — no key rotation, no
//!   token blacklist, instant effect ([`TokenStore::revoke`]).
//! - A lightweight **read-only join-token** (QR/URL-friendly) authenticates a
//!   [`Role::ReadOnly`] session but is **never** accepted on a write — a spectator who
//!   scans the venue QR can watch, never run the race ([`Role::can_control`]).
//!
//! # How writes are gated (the one chokepoint)
//!
//! [`ControlAuth`] is the single extractor every RD-gated route demands first. Its body
//! calls [`TokenStore::authenticate_control`] with the request's `Authorization: Bearer …`:
//! a valid **RD-role** token yields the marker, anything else (no header, a read-only/join
//! token, a revoked or unknown token) is [`ProtocolError`] of [`ErrorCode::Unauthorized`]
//! → HTTP 401. The handlers never see the credential.
//!
//! With **no RD token configured** the gate is open (full trust — the loopback / trusted
//! LAN default); see [`TokenStore::has_control_credential`].
//!
//! # How reads are treated
//!
//! Reads stay **open on the LAN**: no `GET` requires a token, matching the "a phone on the
//! venue Wi-Fi just watches" use case. [`TokenStore::authenticate_read`] is the read-side
//! policy (a token is optional, but a present one must be valid); no route in the skeleton
//! applies it yet.
//!
//! # Deferred (noted, not built)
//!
//! - **Persistence.** The [`TokenStore`] is in-memory: tokens live for the process and a
//!   restart invalidates every session (the RD re-issues).
//! - **Real signing.** The join-token is an opaque random string, not an HMAC/JWT-signed
//!   capability — it is validated by store lookup like the bearer token.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// The body of a successful `POST /events/{event_id}/auth/join-token`.
///
/// A freshly-minted **read-only** join token, returned to an authenticated RD so it can
/// hand it (e.g. as a venue QR / share URL) to a spectator. The token authenticates LAN
/// **reads** but is rejected on control ([`Role::can_control`]). The single-field wire
/// shape leaves room to grow additively (an expiry, a scope) without breaking an older
/// client that reads only `token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct JoinTokenResponse {
    /// The opaque, URL/QR-safe read-only join token (see [`random_token`]).
    pub token: String,
}

/// The authority a [`Session`] grants (protocol.html §5).
///
/// Two levels are all the contract needs today: the privileged **RD** role that may
/// drive control, and a **read-only** role (the join-token's level) that may watch but
/// never control. The distinction is enforced at the one control chokepoint via
/// [`Role::can_control`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The Race Director: authenticated, control-authorized (§5 "the RD's authenticated
    /// role"). Minted by [`TokenStore::issue_rd_token`].
    Rd,
    /// A read-only viewer: may read the contract, never control. The level a join-token
    /// authenticates (§9.4). Minted by [`TokenStore::issue_join_token`].
    ReadOnly,
}

impl Role {
    /// Whether this role may drive the privileged control path (protocol.html §5).
    /// Only [`Role::Rd`] can; [`Role::ReadOnly`] never — the join-token "never grants
    /// control" rule (§9.4).
    pub fn can_control(self) -> bool {
        matches!(self, Role::Rd)
    }
}

/// A server-side session a token resolves to (protocol.html §5, §9.4).
///
/// The token is opaque; the *session* is where the authority lives, so revoking a token
/// is just dropping its session — no token is self-describing, so none can outlive the
/// server's decision to revoke it.
#[derive(Debug, Clone)]
pub struct Session {
    /// The authority this session grants.
    pub role: Role,
}

/// The in-memory token → [`Session`] store (protocol.html §5, §9.4) — the auth authority.
///
/// Opaque random tokens map to sessions carrying a [`Role`]. Mintable
/// ([`issue_rd_token`](TokenStore::issue_rd_token),
/// [`issue_join_token`](TokenStore::issue_join_token)) and **revocable**
/// ([`revoke`](TokenStore::revoke)) — revocation is instant because the token carries no
/// authority of its own, only a key into this map. Cloning shares the one store (it is an
/// `Arc<RwLock<…>>`), so every axum handler consults the same sessions.
///
/// In-memory by design for v0.4 (persistence deferred — see the module docs).
#[derive(Clone, Default)]
pub struct TokenStore {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
}

impl TokenStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh **RD-role** bearer token, register its control-authorized session, and
    /// return the opaque token string (protocol.html §5). The RD console presents it as
    /// `Authorization: Bearer <token>` on the control path.
    pub fn issue_rd_token(&self) -> String {
        self.issue(Role::Rd)
    }

    /// Mint a fresh **read-only join-token** and register its read-only session
    /// (protocol.html §9.4). QR/URL-friendly (an opaque URL-safe string); it authenticates
    /// reads but is rejected on control by [`Role::can_control`].
    pub fn issue_join_token(&self) -> String {
        self.issue(Role::ReadOnly)
    }

    /// Register an **RD-role** session under a caller-supplied `token`, returning whether it
    /// was registered. Unlike [`issue_rd_token`](TokenStore::issue_rd_token) (which mints a
    /// random opaque token), this pins a *known* control token so a deployment can configure
    /// a fixed credential — the Director uses it to honour a `GRIDFPV_RD_TOKEN` env, letting
    /// an automated client (the e2e harness, the Tauri app under test) log in with a known
    /// value. A blank token is rejected (`false`) so an empty env never registers an empty,
    /// trivially-guessable key; otherwise the session is registered and `true` returned.
    pub fn register_rd_token(&self, token: &str) -> bool {
        if token.trim().is_empty() {
            return false;
        }
        self.sessions
            .write()
            .expect("token store lock poisoned")
            .insert(token.to_string(), Session { role: Role::Rd });
        true
    }

    /// Register a session for `role` under a fresh opaque token and return the token.
    fn issue(&self, role: Role) -> String {
        let token = random_token();
        self.sessions
            .write()
            .expect("token store lock poisoned")
            .insert(token.clone(), Session { role });
        token
    }

    /// **Revoke** a token by dropping its session (protocol.html §9.4): instant and
    /// total — the token authenticates nothing afterwards. Returns whether a session was
    /// actually removed (a no-op for an already-unknown token).
    pub fn revoke(&self, token: &str) -> bool {
        self.sessions
            .write()
            .expect("token store lock poisoned")
            .remove(token)
            .is_some()
    }

    /// Resolve a token to its [`Session`], or `None` if it is unknown/revoked.
    pub fn session(&self, token: &str) -> Option<Session> {
        self.sessions
            .read()
            .expect("token store lock poisoned")
            .get(token)
            .cloned()
    }

    /// Whether **any control credential is configured** — i.e. at least one registered
    /// session may drive control ([`Role::can_control`]).
    ///
    /// This is the switch behind the **full-trust (open-by-default) control posture**
    /// (issue #72, Slice 1b): the Director only registers an RD token when one is
    /// *configured* (a `GRIDFPV_RD_TOKEN` env, or an RD minting one), so an unconfigured
    /// Director has **no** control credential and [`authenticate_control`](Self::authenticate_control)
    /// admits an anonymous caller. The moment a token is configured, control is gated again.
    ///
    /// Read-only/join tokens do **not** count — they can never control, so issuing a venue
    /// QR never accidentally locks the control path.
    ///
    /// This is the **dev/local-trust** posture (safe on loopback / a trusted LAN). The proper
    /// loopback-trust + remote-passphrase split (open on `127.0.0.1`, gated for a remote
    /// binding) is tracked separately as #80 and is **not** built here.
    pub fn has_control_credential(&self) -> bool {
        self.sessions
            .read()
            .expect("token store lock poisoned")
            .values()
            .any(|s| s.role.can_control())
    }

    /// Authenticate a control caller (issue #72, Slice 1b — full-trust by default).
    ///
    /// Policy, in order:
    /// - **No control credential configured** ([`has_control_credential`](Self::has_control_credential)
    ///   is `false`) ⇒ control is **OPEN (full trust)**: admit as [`Role::Rd`] regardless of
    ///   any token. A present token — even a stale/unknown or read-only one — is **ignored**:
    ///   there is nothing configured to validate it against, and "full trust" means a leftover
    ///   token (e.g. from when the Director was previously run gated) must not break it.
    /// - **A control credential IS configured** ⇒ control is **gated**: a present control token
    ///   is validated (unknown/revoked or read-only ⇒ rejected); no token ⇒ rejected.
    ///
    /// This is the whole control policy in one place; [`ControlAuth`] is the only caller.
    /// The loopback/remote split is #80 (not built here).
    pub fn authenticate_control(&self, token: Option<&str>) -> Result<Session, ProtocolError> {
        // Full-trust first: when no control credential is configured, control is OPEN —
        // admit as RD regardless of any token presented. A leftover/stale token (e.g. from
        // when the Director was previously run gated) must NOT break an open Director;
        // there is nothing to validate it against, so it is simply ignored.
        if !self.has_control_credential() {
            return Ok(Session { role: Role::Rd });
        }
        // Gated: a credential IS configured, so a valid control token is required.
        match token {
            Some(token) => match self.session(token) {
                Some(session) if session.role.can_control() => Ok(session),
                Some(_) => Err(ProtocolError::new(
                    ErrorCode::Unauthorized,
                    "this token is read-only and may not drive control",
                )),
                None => Err(ProtocolError::new(
                    ErrorCode::Unauthorized,
                    "unknown or revoked control token",
                )),
            },
            None => Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                "control requires an Authorization: Bearer <RD token> header",
            )),
        }
    }

    /// Authenticate a **read** caller (protocol.html §5): reads are open on the LAN, so a
    /// missing token is allowed (`Ok(None)`); a *present* token must be valid (an
    /// unknown/revoked token is [`ErrorCode::Unauthorized`] rather than silently treated as
    /// anonymous). A valid token of either role authenticates a read. The join-token path
    /// uses this.
    pub fn authenticate_read(&self, token: Option<&str>) -> Result<Option<Session>, ProtocolError> {
        match token {
            None => Ok(None),
            Some(token) => self.session(token).map(Some).ok_or_else(|| {
                ProtocolError::new(ErrorCode::Unauthorized, "unknown or revoked read token")
            }),
        }
    }
}

/// Draw an opaque, URL-safe token from the OS CSPRNG (protocol.html §9.4 "opaque token").
///
/// 32 random bytes (256 bits — unguessable) rendered as URL-safe base64 without padding,
/// so the string drops straight into a `Bearer` header, a query parameter, or a QR/URL
/// for the join-token. Random + opaque means the token carries no authority itself; the
/// [`TokenStore`] is the sole authority (so revocation is instant). Real signed tokens are
/// deferred (see the module docs).
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    url_safe_base64(&bytes)
}

/// URL-safe base64 (RFC 4648 §5) without padding — enough to render a random token as a
/// compact, header/URL/QR-safe string. Hand-rolled to avoid pulling a base64 dependency
/// for one tiny use.
fn url_safe_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6 & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// Read a bearer token from an `Authorization: Bearer <token>` header on a request's
/// [`Parts`], if present and well-formed.
///
/// Returns `None` for a missing header or one that is not the `Bearer` scheme — the
/// caller (`authenticate_*`) decides whether absence is an error (control) or allowed
/// (reads). The scheme match is case-insensitive per RFC 7235.
pub fn bearer_token(parts: &Parts) -> Option<String> {
    let header = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = header.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("Bearer") {
        let token = token.trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    } else {
        None
    }
}

/// The **auth chokepoint** for every RD-gated write.
///
/// An axum extractor a gated route demands *before* its handler runs: it reads the
/// caller's `Authorization: Bearer <token>` and requires a token resolving to a
/// **control-authorized** ([`Role::Rd`]) session in the shared [`TokenStore`]. A valid RD
/// token yields the marker; anything else — no header, a read-only/join token, an unknown
/// or revoked token — is rejected with [`ErrorCode::Unauthorized`] (HTTP 401). The whole
/// policy lives in [`TokenStore::authenticate_control`]; this extractor is just where it is
/// applied.
#[derive(Debug, Clone, Copy)]
pub struct ControlAuth {
    // A private field so `ControlAuth` can only be minted by the extractor (the auth
    // chokepoint), never constructed ad hoc by a handler that wants to skip the check.
    _private: (),
}

impl FromRequestParts<EventRegistry> for ControlAuth {
    type Rejection = ProtocolError;

    async fn from_request_parts(
        parts: &mut Parts,
        registry: &EventRegistry,
    ) -> Result<Self, Self::Rejection> {
        // The token store is Director-wide, so one RD token authorizes every write.
        let token = bearer_token(parts);
        registry.tokens().authenticate_control(token.as_deref())?;
        Ok(ControlAuth { _private: () })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rd_token_authenticates_control_and_round_trips() {
        let store = TokenStore::new();
        let token = store.issue_rd_token();
        let session = store.authenticate_control(Some(&token)).unwrap();
        assert_eq!(session.role, Role::Rd);
    }

    #[test]
    fn join_token_is_read_only_and_rejected_on_control_when_gated() {
        let store = TokenStore::new();
        store.issue_rd_token(); // gate control so a presented token is actually validated
        let token = store.issue_join_token();
        // Authenticates a read…
        let read = store.authenticate_read(Some(&token)).unwrap().unwrap();
        assert_eq!(read.role, Role::ReadOnly);
        // …but never control (on a gated Director).
        let err = store.authenticate_control(Some(&token)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
    }

    #[test]
    fn no_token_is_open_on_control_when_nothing_configured_and_always_open_on_read() {
        // Full-trust (#72, Slice 1b): an unconfigured store has no control credential, so a
        // no-token control caller is admitted as an RD; reads are open regardless.
        let store = TokenStore::new();
        assert!(!store.has_control_credential());
        let session = store.authenticate_control(None).unwrap();
        assert_eq!(session.role, Role::Rd);
        assert!(store.authenticate_read(None).unwrap().is_none());
    }

    #[test]
    fn no_token_is_rejected_on_control_once_a_control_token_is_configured() {
        // The moment an RD token is configured the open posture closes: a no-token control
        // caller is rejected, but the configured token still works.
        let store = TokenStore::new();
        let token = store.issue_rd_token();
        assert!(store.has_control_credential());
        assert_eq!(
            store.authenticate_control(None).unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert!(store.authenticate_control(Some(&token)).is_ok());
    }

    #[test]
    fn a_join_token_alone_does_not_configure_control_so_control_stays_open() {
        // A read-only join token can never control, so issuing one (a venue QR) must not
        // accidentally lock the open control path.
        let store = TokenStore::new();
        let _join = store.issue_join_token();
        assert!(!store.has_control_credential());
        assert!(store.authenticate_control(None).is_ok());
    }

    #[test]
    fn a_present_token_is_ignored_on_an_open_director() {
        // Full-trust (#72): with nothing configured (open), control admits as RD regardless of
        // any token — a stale/unknown token, or even a read-only one, is IGNORED, not rejected.
        // A leftover token from a previously-gated run must never break an open Director.
        let store = TokenStore::new();
        assert_eq!(
            store.authenticate_control(Some("stale")).unwrap().role,
            Role::Rd
        );
        let join = store.issue_join_token(); // a join token still does not gate control
        assert!(!store.has_control_credential());
        assert_eq!(
            store.authenticate_control(Some(&join)).unwrap().role,
            Role::Rd
        );
    }

    #[test]
    fn revoked_token_stops_working_while_control_stays_gated() {
        let store = TokenStore::new();
        let keep = store.issue_rd_token(); // a second credential keeps control gated post-revoke
        let token = store.issue_rd_token();
        assert!(store.authenticate_control(Some(&token)).is_ok());
        assert!(store.revoke(&token));
        // Still gated (the `keep` credential remains), so the revoked token is now rejected.
        assert!(store.has_control_credential());
        assert_eq!(
            store.authenticate_control(Some(&token)).unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert!(store.authenticate_control(Some(&keep)).is_ok());
        // A second revoke is a harmless no-op.
        assert!(!store.revoke(&token));
    }

    #[test]
    fn unknown_token_is_unauthorized_on_both_paths_when_gated() {
        let store = TokenStore::new();
        store.issue_rd_token(); // gate control so an unknown token is validated + rejected
        assert_eq!(
            store.authenticate_control(Some("nope")).unwrap_err().code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            store.authenticate_read(Some("nope")).unwrap_err().code,
            ErrorCode::Unauthorized
        );
    }

    #[test]
    fn tokens_are_distinct_and_url_safe() {
        let store = TokenStore::new();
        let a = store.issue_rd_token();
        let b = store.issue_rd_token();
        assert_ne!(a, b, "each token is freshly random");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token is URL/QR-safe: {a}"
        );
    }

    #[test]
    fn base64_matches_a_known_vector() {
        // "Man" -> "TWFu" in standard/url-safe base64 (no padding needed, length 3).
        assert_eq!(url_safe_base64(b"Man"), "TWFu");
        // A length that needs the 2-char tail: "M" -> "TQ".
        assert_eq!(url_safe_base64(b"M"), "TQ");
    }
}
