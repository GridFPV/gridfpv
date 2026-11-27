//! The Director's end of the wire to a remote server (docs/primitives.md §13.4): one
//! [`Client`] per server and link token.
//!
//! It only ever connects outward, over HTTPS (plain HTTP is let through for this same
//! computer alone, §13.5). Every failure comes back as words a director can read.

use std::time::Duration;

use gridfpv_events::PilotId;
use gridfpv_storage::{Chain, RawEntry};

use crate::events::EventId;
use crate::link::{LinkStored, LinkTrust};
use crate::remote::{
    PushAccount, PushAnswer, PushHeader, PushHeld, PushHello, PushPilot, PushPilotAccount,
    PushRefusal, PushRefused, PushRequest, PushStatus,
};

/// What went wrong talking to the server, in words for the director.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    /// The server could not be reached, or what it said made no sense. Worth trying again.
    Server(String),
    /// The server did not take the token.
    Token,
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(why) => f.write_str(why),
            Self::Token => f.write_str(
                "The server did not take the link token. It may have been taken back, or it is for another server.",
            ),
        }
    }
}

/// What a server says it holds of one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// None of it.
    Nothing,
    Some(PushStatus),
    /// It was removed on the server, which takes none of it until told to.
    Removed,
}

/// A connection to one server, with one token.
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Client {
    /// A client for the server in the settings. Fails only when the settings cannot make
    /// one (extra authorities that are not certificates).
    pub fn new(stored: &LinkStored) -> Result<Self, PushError> {
        let mut builder = crate::http::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            // The desk let through plain HTTP only for this same computer (§13.5).
            .https_only(stored.url.starts_with("https://"));
        match &stored.trust {
            LinkTrust::System => {}
            LinkTrust::ExtraAuthorities { pem } => {
                let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
                    .ok()
                    .filter(|certs| !certs.is_empty())
                    .ok_or_else(|| {
                        PushError::Server(
                            "The extra certificate authorities could not be read.".into(),
                        )
                    })?;
                builder = builder.tls_certs_merge(certs);
            }
        }
        let http = builder
            .build()
            .map_err(|e| PushError::Server(format!("The connection could not be set up: {e}")))?;
        Ok(Self {
            http,
            base: stored.url.clone(),
            token: stored.token.clone(),
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, PushError> {
        let response = request.send().await.map_err(|e| {
            PushError::Server(format!("The server could not be reached: {}", plain(e)))
        })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(PushError::Token);
        }
        Ok(response)
    }

    /// Whatever else the server answered, as a reason.
    async fn unexpected(response: reqwest::Response) -> PushError {
        let status = response.status();
        let said = response
            .json::<crate::error::ProtocolError>()
            .await
            .map(|e| e.message)
            .unwrap_or_default();
        PushError::Server(if said.is_empty() {
            format!("The server answered {status}.")
        } else {
            format!("The server said: {said}")
        })
    }

    async fn read<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, PushError> {
        response.json::<T>().await.map_err(|e| {
            PushError::Server(format!(
                "The server's answer could not be read. Is this a Grid server? ({})",
                plain(e)
            ))
        })
    }

    /// Which club the token pushes into.
    pub async fn hello(&self) -> Result<PushHello, PushError> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/push/hello"))
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await
    }

    /// How much of an event the server holds.
    pub async fn held(&self, event: &EventId) -> Result<Held, PushError> {
        let path = format!("/push/events/{}", event.0);
        let response = self.send(self.request(reqwest::Method::GET, &path)).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Held::Nothing);
        }
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Self::refused(response).await.map(|()| Held::Removed);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await.map(Held::Some)
    }

    /// A `409` that is not about a batch can only be the event having been removed on the
    /// server. Anything else is an answer this Director does not know.
    async fn refused(response: reqwest::Response) -> Result<(), PushError> {
        let refused: PushRefused = Self::read(response).await?;
        match refused.reason {
            PushRefusal::Removed => Ok(()),
            other => Err(PushError::Server(format!(
                "The server gave an answer that was not expected ({other:?})."
            ))),
        }
    }

    /// What the server knows of every event of the club. `None` from a server too old to
    /// say.
    pub async fn events(&self) -> Result<Option<Vec<PushHeld>>, PushError> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/push/events"))
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await.map(Some)
    }

    /// Send an event's header. `false` when the event was removed on the server, which
    /// then takes nothing.
    pub async fn put_header(
        &self,
        event: &EventId,
        header: &PushHeader,
    ) -> Result<bool, PushError> {
        let path = format!("/push/events/{}", event.0);
        let response = self
            .send(self.request(reqwest::Method::PUT, &path).json(header))
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Self::refused(response).await.map(|()| false);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Ok(true)
    }

    /// Send a run of entries that starts at `from`, where `before` is the chain of what is
    /// ahead of them. `Ok(Err(..))` is the server saying it did not take them, and why.
    pub async fn post_log(
        &self,
        event: &EventId,
        from: u64,
        before: Chain,
        entries: &[RawEntry],
    ) -> Result<Result<PushStatus, PushRefused>, PushError> {
        let path = format!("/push/events/{}/log", event.0);
        let response = self
            .send(
                self.request(reqwest::Method::POST, &path)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(log_body(from, before, entries, now_micros())),
            )
            .await?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            return Self::read(response).await.map(Err);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await.map(Ok)
    }

    /// Remove the server's copy of an event. Holding none is as good.
    pub async fn delete(&self, event: &EventId) -> Result<(), PushError> {
        let path = format!("/push/events/{}", event.0);
        let response = self
            .send(self.request(reqwest::Method::DELETE, &path))
            .await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(Self::unexpected(response).await)
    }

    /// Open the stream the server tells this Director things on (`GET /push/listen`). It
    /// stays open: read it a piece at a time with [`reqwest::Response::chunk`]. The server
    /// writes to it every few seconds, so a long silence means the connection is dead.
    pub async fn listen(&self) -> Result<reqwest::Response, PushError> {
        let response = self
            .send(
                self.request(reqwest::Method::GET, "/push/listen")
                    // No end is set for the whole answer: it never ends by itself.
                    .timeout(Duration::from_secs(60 * 60 * 24 * 365)),
            )
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Ok(response)
    }

    /// What pilots have asked for on the server and nobody has answered.
    pub async fn requests(&self) -> Result<Vec<PushRequest>, PushError> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/push/requests"))
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await
    }

    /// Answer one ask. `false` when the pilot is no longer waiting for it.
    pub async fn answer(&self, id: &str, answer: &PushAnswer) -> Result<bool, PushError> {
        let path = format!("/push/requests/{id}");
        let response = self
            .send(self.request(reqwest::Method::POST, &path).json(answer))
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Ok(true)
    }

    /// The club's pilots as the server knows them, each with the account that holds it.
    pub async fn pilots(&self) -> Result<Vec<PushPilot>, PushError> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/push/pilots"))
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await
    }

    /// Tell the server the pilots this Director has. It adds them to what the club knows.
    pub async fn put_pilots(&self, pilots: &[PushPilot]) -> Result<(), PushError> {
        let response = self
            .send(
                self.request(reqwest::Method::PUT, "/push/pilots")
                    .json(pilots),
            )
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Ok(())
    }

    /// The club's members.
    pub async fn members(&self) -> Result<Vec<PushAccount>, PushError> {
        let response = self
            .send(self.request(reqwest::Method::GET, "/push/members"))
            .await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Self::read(response).await
    }

    /// Say which member holds a pilot, or with `None` that nobody does.
    pub async fn set_account(
        &self,
        pilot: &PilotId,
        account: Option<&str>,
    ) -> Result<(), PushError> {
        let path = format!("/push/pilots/{}/account", pilot.0);
        let request = match account {
            Some(account) => self
                .request(reqwest::Method::PUT, &path)
                .json(&PushPilotAccount {
                    account: account.to_owned(),
                }),
            None => self.request(reqwest::Method::DELETE, &path),
        };
        let response = self.send(request).await?;
        if !response.status().is_success() {
            return Err(Self::unexpected(response).await);
        }
        Ok(())
    }
}

/// A network error without the address in it twice, and with its cause.
pub(crate) fn plain(error: reqwest::Error) -> String {
    use std::error::Error;
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text = format!("{text}: {cause}");
        source = cause.source();
    }
    text
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// The body of a log push. It is built as text so each entry goes as it is stored:
/// passing an entry through a JSON value would write its keys in another order, and the
/// server would hold other text than the chain was worked out from.
pub fn log_body(from: u64, before: Chain, entries: &[RawEntry], sent_at: i64) -> String {
    let mut body = format!(
        "{{\"from\":{from},\"before\":\"{}\",\"entries\":[",
        before.to_hex()
    );
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        match entry.recorded_at {
            Some(at) => body.push_str(&format!("{{\"recorded_at\":{at},\"event\":")),
            None => body.push_str("{\"event\":"),
        }
        body.push_str(&entry.json);
        body.push('}');
    }
    body.push_str(&format!("],\"sent_at\":{sent_at}}}"));
    body
}
