//! Looking at the certificate a server shows, so a director can accept it on purpose
//! (docs/primitives.md §13.5).
//!
//! A server with a normal certificate needs none of this. A club's own server, or one at
//! home, may have made its own. Grid will not trust that by itself, and it must not be
//! made to trust "any certificate". So the director is **shown the one certificate** the
//! server presents, by its fingerprint, checks it with whoever runs the server, and
//! accepts that one.
//!
//! | route | what |
//! |-------|------|
//! | `POST /server-link/certificate` | read the certificate the server at an address shows |
//!
//! **The look itself trusts nothing.** It connects without checking the certificate, only
//! to read it. It sends no link token and reads nothing the server says. Whether to trust
//! what it found is the director's choice, made after, and it is kept as the one
//! certificate added to what Grid trusts ([`LinkTrust::ExtraAuthorities`]).

use std::time::Duration;

use axum::Json;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::link::{LinkTrust, clean_trust, clean_url};

/// `POST /server-link/certificate`: the address to look at, and what is trusted now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CertificateRequest {
    /// The server's address, as on the Server page.
    pub url: String,
    /// The trust in the form now, so the answer can say whether the certificate is
    /// already taken. Left out for the trust this computer has by itself.
    #[serde(default)]
    pub trust: LinkTrust,
}

/// The certificate a server shows, for a director to check and accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CertificateLook {
    /// Its SHA-256 fingerprint, as pairs of letters and digits with colons between. This
    /// is what the director compares with whoever runs the server.
    pub fingerprint: String,
    /// The certificate itself, as PEM text: what is kept if the director accepts it.
    pub pem: String,
    /// Grid takes it already, with the trust that was sent. Nothing needs accepting.
    pub trusted: bool,
    /// Accepting this one certificate would be enough: the server made it for itself, for
    /// this address. When `false` and not `trusted`, it was signed by an authority this
    /// computer does not know, or is for another address, and accepting it would not help.
    pub stands_alone: bool,
}

/// A fingerprint as people read one out: `AB:CD:…`.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// A certificate as PEM text.
pub fn pem(der: &[u8]) -> String {
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let mut text = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in body.as_bytes().chunks(64) {
        text.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        text.push('\n');
    }
    text.push_str("-----END CERTIFICATE-----\n");
    text
}

const WITHIN: Duration = Duration::from_secs(10);

fn quick() -> reqwest::ClientBuilder {
    crate::http::builder()
        .connect_timeout(WITHIN)
        .timeout(WITHIN)
        .https_only(true)
        // The look is at the address given and nowhere it points to.
        .redirect(reqwest::redirect::Policy::none())
}

/// Whether a client built this way gets through the TLS handshake with the server. What
/// the server then answers does not matter.
async fn connects(builder: reqwest::ClientBuilder, url: &str) -> bool {
    match builder.build() {
        Ok(client) => client.get(format!("{url}/health")).send().await.is_ok(),
        Err(_) => false,
    }
}

/// Read the certificate the server at `url` shows. `Err` is a sentence for the director.
pub async fn look(url: &str, trust: &LinkTrust) -> Result<CertificateLook, String> {
    let url = clean_url(url).map_err(|e| e.to_string())?;
    if url.is_empty() {
        return Err("Type the server's address first.".into());
    }
    if !url.starts_with("https://") {
        return Err(
            "This address is plain HTTP on this same computer. There is no certificate to look at."
                .into(),
        );
    }
    // Read it without checking it. Nothing is sent but the request for the health page.
    let reader = quick()
        .tls_danger_accept_invalid_certs(true)
        .tls_info(true)
        .build()
        .map_err(|e| format!("Grid could not start a connection: {e}"))?;
    let response = reader
        .get(format!("{url}/health"))
        .send()
        .await
        .map_err(|e| {
            format!(
                "The server could not be reached: {}",
                crate::link_client::plain(e)
            )
        })?;
    let der = response
        .extensions()
        .get::<reqwest::tls::TlsInfo>()
        .and_then(|info| info.peer_certificate())
        .ok_or("The server showed no certificate.")?
        .to_vec();
    drop(response);

    // Would Grid take it as things are? Built the way the link's own client is.
    let mut as_set = quick();
    if let LinkTrust::ExtraAuthorities { pem } = trust
        && let Ok(certs) = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
    {
        as_set = as_set.tls_certs_merge(certs);
    }
    let trusted = connects(as_set, &url).await;

    // Would this one certificate be enough by itself, for this address?
    let stands_alone = match reqwest::Certificate::from_der(&der) {
        Ok(cert) => connects(quick().tls_certs_only([cert]), &url).await,
        Err(_) => false,
    };

    Ok(CertificateLook {
        fingerprint: fingerprint(&der),
        pem: pem(&der),
        trusted,
        stands_alone,
    })
}

/// `POST /server-link/certificate`.
pub async fn post_certificate(
    _auth: ControlAuth,
    Json(request): Json<CertificateRequest>,
) -> Result<Json<CertificateLook>, ProtocolError> {
    // A trust that is not certificates is said before anything is connected to.
    let trust = clean_trust(request.trust).map_err(ProtocolError::from)?;
    match look(&request.url, &trust).await {
        Ok(found) => Ok(Json(found)),
        Err(why) => Err(ProtocolError::new(ErrorCode::Refused, why)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fingerprint_is_read_out_in_pairs() {
        // SHA-256 of nothing, a value anyone can check.
        assert_eq!(
            fingerprint(b""),
            "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
        );
        assert_eq!(fingerprint(b"abc").len(), 32 * 3 - 1);
    }

    #[test]
    fn pem_text_is_wrapped_and_reads_back_as_the_same_certificate() {
        let der: Vec<u8> = (0..=255u8).cycle().take(700).collect();
        let text = pem(&der);
        assert!(text.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(text.ends_with("-----END CERTIFICATE-----\n"));
        assert!(text.lines().all(|line| line.len() <= 64));
        let body: String = text
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(body)
                .unwrap(),
            der
        );
    }

    #[tokio::test]
    async fn an_address_that_cannot_have_a_certificate_is_said_before_anything_is_tried() {
        for (url, words) in [
            ("", "Type the server's address first"),
            ("   ", "Type the server's address first"),
            ("http://localhost:8470", "no certificate to look at"),
            ("http://grid.example.org", "must start with https://"),
            ("grid.example.org", "not a web address"),
            ("https://grid.example.org/some/page", "address only"),
            ("https://someone:secret@grid.example.org", "address only"),
        ] {
            let why = look(url, &LinkTrust::System).await.unwrap_err();
            assert!(why.contains(words), "{url:?}: {why}");
        }
    }
}
