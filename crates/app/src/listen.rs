//! Hearing from the linked server (docs/primitives.md §13.4).
//!
//! A pilot who signs up on the server is asking the Director, the only writer of an
//! event. The Director must hear of it at once, and nothing may reach in to the laptop. So
//! the Director opens one stream to the server and keeps it open. The server says on it
//! when something is waiting. The Director then reads what is waiting, decides each ask
//! as that pilot's own, and answers.
//!
//! - **It is safe to lose the stream.** It is opened again, and the server says what is
//!   waiting as soon as it is.
//! - **A silent stream is a dead one.** The server writes every few seconds. A long quiet
//!   is taken as a lost connection.
//! - **Racing never waits on this.** An ask that cannot be answered is left; the pilot is
//!   told there was no answer.

use std::time::Duration;

use gridfpv_server::events::EventRegistry;
use gridfpv_server::link_client::{Client, PushError};
use tokio::task::JoinHandle;

/// How long the stream may say nothing before it is taken as dead. The server writes at
/// least every 15 seconds.
const QUIET_FOR: Duration = Duration::from_secs(45);

/// How long to wait before opening the stream again.
const AGAIN_AFTER: Duration = Duration::from_secs(2);

/// Read what is waiting on the server, decide each ask, and answer. Returns how many were
/// answered.
pub async fn answer_waiting(registry: &EventRegistry, client: &Client) -> Result<usize, PushError> {
    let mut answered = 0;
    for request in client.requests().await? {
        // An event this Director does not have is another Director's to answer.
        if registry.meta_of(&request.event).is_none() {
            continue;
        }
        // And so is one this Director no longer sends to this club.
        if registry.link().club_id_of(&request.event).as_deref() != Some(client.club()) {
            continue;
        }
        let answer = gridfpv_server::signup::decide(registry, &request);
        // The pilot may have stopped waiting. The log says what was done either way, and
        // it goes up with the next push.
        if client.answer(&request.id, &answer).await? {
            answered += 1;
        }
    }
    Ok(answered)
}

/// Whether a piece of the stream says something is waiting.
fn says_waiting(text: &str) -> bool {
    text.lines().any(|line| line.trim() == "event: asks")
}

/// Listen until the stream ends, answering as asks come in.
async fn listen_once(registry: &EventRegistry, client: &Client) -> Result<(), PushError> {
    let mut stream = client.listen().await?;
    // Whatever was asked while nobody was listening.
    answer_waiting(registry, client).await?;
    let mut held = String::new();
    loop {
        let piece = tokio::time::timeout(QUIET_FOR, stream.chunk())
            .await
            .map_err(|_| PushError::Server("The server went quiet.".into()))?
            .map_err(|e| PushError::Server(format!("The stream from the server broke: {e}")))?;
        let Some(piece) = piece else {
            return Ok(());
        };
        held.push_str(&String::from_utf8_lossy(&piece));
        // One message ends with an empty line. Keep what comes after the last one.
        let Some(end) = held.rfind("\n\n") else {
            continue;
        };
        let whole: String = held.drain(..end + 2).collect();
        if says_waiting(&whole) {
            answer_waiting(registry, client).await?;
        }
    }
}

/// Start listening to the server this client is for. Runs until aborted.
pub fn spawn(registry: EventRegistry, client: Client) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Why it ended does not matter here: the task that pushes events is the one
            // that says on the Server page when the server is away.
            listen_once(&registry, &client).await.ok();
            tokio::time::sleep(AGAIN_AFTER).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_asks_message_says_something_is_waiting() {
        assert!(says_waiting("event: asks\ndata: 2\n\n"));
        assert!(says_waiting(": keep-alive\n\nevent: asks\ndata: 1\n\n"));
        assert!(!says_waiting("event: listening\ndata:\n\n"));
        assert!(!says_waiting(": keep-alive\n\n"));
        // The word in a message's data is not a message of that kind.
        assert!(!says_waiting("event: listening\ndata: event: asks?\n\n"));
    }
}
