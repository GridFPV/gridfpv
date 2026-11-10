//! The framing RotorHazard's socket uses: Socket.IO over Engine.IO, as text frames on one
//! WebSocket.
//!
//! Only the small part RotorHazard needs is here. Every frame is a short text:
//!
//! | Text | Means |
//! |---|---|
//! | `0{…}` | Engine.IO open (first frame from the server) |
//! | `2` / `3` | Engine.IO ping from the server / the pong it wants back |
//! | `40` / `40{…}` | Socket.IO connect: the client asks, the server confirms |
//! | `41` | Socket.IO disconnect |
//! | `42["name",arg…]` | an event |
//! | `42N["name",arg…]` | an event that wants an answer, numbered N |
//! | `43N[arg…]` | the answer to event N |
//!
//! Checked on 2026-11-10 against RotorHazard 4.3.0 and 4.4.0: a WebSocket opened at
//! `/socket.io/?EIO=4&transport=websocket` needs no polling step and no login.
//!
//! Pure: this parses and builds text and nothing else.

use serde_json::Value;

/// The path and query a RotorHazard WebSocket is opened at.
pub const SOCKET_PATH: &str = "/socket.io/?EIO=4&transport=websocket";

/// One frame from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// The first frame: the link is open. The client now sends [`connect`].
    Open,
    /// The server wants a [`pong`] back, or it will drop the link.
    Ping,
    /// The server accepted the connect: events can flow.
    Connected,
    /// The server closed the Socket.IO session.
    Disconnected,
    /// An event: its name and its arguments.
    Event { name: String, args: Vec<Value> },
    /// The answer to an event the client numbered `id`.
    Ack { id: u64, args: Vec<Value> },
    /// Anything else. Ignored.
    Other,
}

/// Parse one text frame. Never fails: a frame that cannot be read is [`Frame::Other`].
pub fn parse(frame: &str) -> Frame {
    match frame.as_bytes() {
        [b'0', ..] => Frame::Open,
        [b'2'] => Frame::Ping,
        [b'4', b'0', ..] => Frame::Connected,
        [b'4', b'1', ..] => Frame::Disconnected,
        [b'4', b'2', ..] => {
            // An event from the server may carry an ack number too; skip it.
            let body = frame[2..].trim_start_matches(|c: char| c.is_ascii_digit());
            match serde_json::from_str::<Vec<Value>>(body) {
                Ok(mut items) if !items.is_empty() => match items.remove(0) {
                    Value::String(name) => Frame::Event { name, args: items },
                    _ => Frame::Other,
                },
                _ => Frame::Other,
            }
        }
        [b'4', b'3', ..] => {
            let rest = &frame[2..];
            let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            match (
                rest[..digits].parse::<u64>(),
                serde_json::from_str::<Vec<Value>>(&rest[digits..]),
            ) {
                (Ok(id), Ok(args)) => Frame::Ack { id, args },
                _ => Frame::Other,
            }
        }
        _ => Frame::Other,
    }
}

/// The frame that asks for a Socket.IO session. Sent once, after [`Frame::Open`].
pub fn connect() -> String {
    "40".to_owned()
}

/// The answer to a [`Frame::Ping`].
pub fn pong() -> String {
    "3".to_owned()
}

/// An event with no answer wanted. `arg` is the one argument RotorHazard's handlers take;
/// `None` sends none.
pub fn event(name: &str, arg: Option<&Value>) -> String {
    format!("42{}", body(name, arg))
}

/// An event that wants an answer. The answer comes back as [`Frame::Ack`] with this `id`.
pub fn event_with_ack(id: u64, name: &str, arg: Option<&Value>) -> String {
    format!("42{id}{}", body(name, arg))
}

fn body(name: &str, arg: Option<&Value>) -> String {
    let mut items = vec![Value::String(name.to_owned())];
    items.extend(arg.cloned());
    Value::Array(items).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_frames_a_real_rotorhazard_sent_are_read() {
        // Captured from RotorHazard 4.4.0 on 2026-11-10.
        assert_eq!(
            parse(
                r#"0{"sid":"RI8_uFGvfBzbUjKDAAAA","upgrades":[],"pingTimeout":20000,"pingInterval":25000}"#
            ),
            Frame::Open
        );
        assert_eq!(
            parse(r#"40{"sid":"J78ttOmU0QdaIkMgAAAB"}"#),
            Frame::Connected
        );
        assert_eq!(parse("2"), Frame::Ping);
        assert_eq!(
            parse(r#"430[{"server_time_s":1904518.493188328}]"#),
            Frame::Ack {
                id: 0,
                args: vec![json!({"server_time_s": 1904518.493188328})]
            }
        );
        assert_eq!(
            parse(
                r#"42["enter_and_exit_at_levels",{"enter_at_levels":[90,90],"exit_at_levels":[80,80]}]"#
            ),
            Frame::Event {
                name: "enter_and_exit_at_levels".into(),
                args: vec![json!({"enter_at_levels": [90, 90], "exit_at_levels": [80, 80]})]
            }
        );
        assert_eq!(parse("41"), Frame::Disconnected);
    }

    #[test]
    fn an_ack_number_can_have_several_digits() {
        assert_eq!(
            parse("4312[1,2]"),
            Frame::Ack {
                id: 12,
                args: vec![json!(1), json!(2)]
            }
        );
    }

    #[test]
    fn a_frame_that_cannot_be_read_is_ignored_not_an_error() {
        for frame in [
            "", "9", "42", "42{}", "42[]", "42[7]", "43x[1]", "43[1]x", "6", "4",
        ] {
            assert_eq!(parse(frame), Frame::Other, "{frame:?}");
        }
    }

    #[test]
    fn frames_are_built_the_way_rotorhazard_reads_them() {
        assert_eq!(connect(), "40");
        assert_eq!(pong(), "3");
        assert_eq!(event("stop_race", None), r#"42["stop_race"]"#);
        assert_eq!(
            event(
                "set_frequency",
                Some(&json!({"node": 0, "frequency": 5658}))
            ),
            r#"42["set_frequency",{"frequency":5658,"node":0}]"#
        );
        assert_eq!(
            event_with_ack(3, "get_server_time", None),
            r#"423["get_server_time"]"#
        );
        // What is built reads back.
        assert_eq!(
            parse(&event("x", Some(&json!([1])))),
            Frame::Event {
                name: "x".into(),
                args: vec![json!([1])]
            }
        );
    }
}
