//! OBS over its WebSocket server (obs-websocket 5, built into OBS 28 and later: Tools →
//! WebSocket Server Settings).
//!
//! [`connect`] opens the connection and does the handshake, with the password when OBS asks
//! for one. The [`Obs`] it gives back makes requests ([`Obs::call`]) and hands out OBS's
//! events ([`Obs::events`]). One task owns the socket; it ends when OBS closes the
//! connection or the last `Obs` is dropped, and every request still waiting then fails.
//!
//! Only what the recorder needs is here: requests by name with JSON in and out. Nothing in
//! this module knows what a race is.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// How long OBS is given to answer the handshake or one request.
const ANSWER: Duration = Duration::from_secs(5);

/// The event group asked for: outputs (a recording starting and stopping).
const SUBSCRIBE_OUTPUTS: u32 = 1 << 6;

/// One event from OBS: its type ("RecordStateChanged") and its data.
#[derive(Debug, Clone, PartialEq)]
pub struct ObsEvent {
    pub kind: String,
    pub data: Value,
}

struct Call {
    kind: String,
    data: Value,
    answer: oneshot::Sender<Result<Value, String>>,
}

/// A live connection to OBS. Cloning shares it.
#[derive(Clone)]
pub struct Obs {
    calls: mpsc::Sender<Call>,
    events: broadcast::Sender<ObsEvent>,
}

/// What the password handshake sends: `base64(sha256(base64(sha256(password + salt)) +
/// challenge))` (obs-websocket protocol, "Creating an authentication string").
pub fn authentication(password: &str, salt: &str, challenge: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let secret = b64.encode(Sha256::digest(format!("{password}{salt}")));
    b64.encode(Sha256::digest(format!("{secret}{challenge}")))
}

/// Connect to OBS at `host:port` and identify. The error is a sentence for the director.
pub async fn connect(address: &str, password: &str) -> Result<Obs, String> {
    let unreachable = || {
        format!(
            "Could not reach OBS at {address}. Check that OBS is open and its WebSocket server is on (Tools → WebSocket Server Settings)."
        )
    };
    let url = format!("ws://{address}");
    let (mut socket, _) = tokio::time::timeout(ANSWER, tokio_tungstenite::connect_async(&url))
        .await
        .map_err(|_| unreachable())?
        .map_err(|_| unreachable())?;

    // Hello, then Identify, then Identified.
    let hello = next_json(&mut socket).await.ok_or_else(unreachable)?;
    let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": SUBSCRIBE_OUTPUTS });
    if let Some(auth) = hello["d"].get("authentication") {
        if password.is_empty() {
            return Err(
                "OBS asks for a password. Type the one from its WebSocket Server Settings.".into(),
            );
        }
        identify["authentication"] = json!(authentication(
            password,
            auth["salt"].as_str().unwrap_or_default(),
            auth["challenge"].as_str().unwrap_or_default(),
        ));
    }
    socket
        .send(Message::text(json!({ "op": 1, "d": identify }).to_string()))
        .await
        .map_err(|_| unreachable())?;
    match next_json(&mut socket).await {
        Some(identified) if identified["op"] == 2 => {}
        // OBS closes the connection on a wrong password (close code 4009).
        _ => {
            return Err(
                "OBS did not take the password. Check it against OBS's WebSocket Server Settings."
                    .into(),
            );
        }
    }

    let (calls, mut calls_in) = mpsc::channel::<Call>(16);
    let events = broadcast::Sender::new(64);
    let events_out = events.clone();
    tokio::spawn(async move {
        let mut waiting: BTreeMap<String, oneshot::Sender<Result<Value, String>>> = BTreeMap::new();
        let mut next_id: u64 = 0;
        loop {
            tokio::select! {
                call = calls_in.recv() => {
                    // Every `Obs` is gone: nobody is left to ask.
                    let Some(call) = call else { break };
                    next_id += 1;
                    let id = next_id.to_string();
                    let request = json!({ "op": 6, "d": {
                        "requestType": call.kind, "requestId": id, "requestData": call.data,
                    }});
                    if socket.send(Message::text(request.to_string())).await.is_err() {
                        let _ = call.answer.send(Err("OBS closed the connection.".into()));
                        break;
                    }
                    waiting.insert(id, call.answer);
                }
                message = socket.next() => {
                    let Some(Ok(message)) = message else { break };
                    let Message::Text(text) = message else { continue };
                    let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                    match value["op"].as_u64() {
                        // An event.
                        Some(5) => {
                            let _ = events_out.send(ObsEvent {
                                kind: value["d"]["eventType"].as_str().unwrap_or_default().to_owned(),
                                data: value["d"]["eventData"].clone(),
                            });
                        }
                        // The answer to a request.
                        Some(7) => {
                            let d = &value["d"];
                            let Some(answer) = d["requestId"].as_str().and_then(|id| waiting.remove(id))
                            else { continue };
                            let status = &d["requestStatus"];
                            let _ = answer.send(if status["result"] == true {
                                Ok(d["responseData"].clone())
                            } else {
                                Err(status["comment"]
                                    .as_str()
                                    .map_or_else(
                                        || format!("OBS refused {} (code {}).", d["requestType"], status["code"]),
                                        str::to_owned,
                                    ))
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        // Dropping `waiting` fails every request still open.
    });
    Ok(Obs { calls, events })
}

/// The next JSON message, within the answer time. `None` on anything else.
async fn next_json<S>(socket: &mut S) -> Option<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match tokio::time::timeout(ANSWER, socket.next()).await.ok()?? {
            Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            _ => return None,
        }
    }
}

impl Obs {
    /// Make one request and wait for its answer. `Err` is OBS's own reason, or a sentence
    /// saying the connection is gone.
    pub async fn call(&self, kind: &str, data: Value) -> Result<Value, String> {
        let (answer, answered) = oneshot::channel();
        let call = Call {
            kind: kind.to_owned(),
            data,
            answer,
        };
        let gone = || "OBS closed the connection.".to_owned();
        self.calls.send(call).await.map_err(|_| gone())?;
        match tokio::time::timeout(ANSWER, answered).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(gone()),
            Err(_) => Err(format!("OBS did not answer {kind} in time.")),
        }
    }

    /// Listen to OBS's events from now on.
    pub fn events(&self) -> broadcast::Receiver<ObsEvent> {
        self.events.subscribe()
    }

    /// Whether the connection is still up.
    pub fn is_open(&self) -> bool {
        !self.calls.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_handshake_matches_the_protocols_own_example() {
        // The worked example in the obs-websocket protocol document.
        assert_eq!(
            authentication(
                "supersecretpassword",
                "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI=",
                "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY=",
            ),
            "1Ct943GAT+6YQUUX47Ia/ncufilbe6+oD6lY+5kaCu4="
        );
    }
}
