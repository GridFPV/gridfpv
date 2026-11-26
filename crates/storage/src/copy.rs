//! Copying a log to another machine, and telling whether two copies are the same log.
//!
//! A Director sends an event's log to a remote server (docs/primitives.md §13.3). Two things
//! are needed for that, and both are here so the two ends cannot disagree about them:
//!
//! - [`RawEntry`]: an entry **as it is stored**, its JSON text untouched. A copy must be
//!   made from the stored text. Parsing an entry and writing it out again is not safe: a
//!   newer build writes fields an older entry never had, and the two ends would then
//!   disagree about what entry 12 is.
//! - [`Chain`]: one short value that stands for everything in a log up to some entry. Each
//!   entry is hashed together with the value before it, so two logs have the same value at
//!   entry N only when every entry before N is the same in both.
//!
//! The chain is how a server knows that what a Director sends continues the log it holds,
//! and is not some other log with the same name (an event restored from an old backup, or
//! made again from nothing).

use sha2::{Digest, Sha256};

use crate::Offset;

/// One log entry as it is stored: the JSON text of its event, not the event parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// Where the entry sits in the log.
    pub offset: Offset,
    /// When the log received it, if the writer said.
    pub recorded_at: Option<i64>,
    /// The event's JSON, exactly as stored.
    pub json: String,
}

/// What stands for a log up to some entry. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chain([u8; 32]);

impl Chain {
    /// The value of an empty log. Every log starts from it.
    pub fn start() -> Self {
        Self(Sha256::digest(b"gridfpv log chain v1").into())
    }

    /// The value after one more entry.
    ///
    /// The length goes in before the text, and the time is marked as there or not, so no
    /// two different entries can hash the same bytes.
    #[must_use]
    pub fn then(self, json: &str, recorded_at: Option<i64>) -> Self {
        let mut hash = Sha256::new();
        hash.update(self.0);
        hash.update((json.len() as u64).to_be_bytes());
        hash.update(json.as_bytes());
        match recorded_at {
            Some(at) => {
                hash.update([1]);
                hash.update(at.to_be_bytes());
            }
            None => hash.update([0]),
        }
        Self(hash.finalize().into())
    }

    /// The value after a run of entries, in order.
    #[must_use]
    pub fn then_all<'a>(self, entries: impl IntoIterator<Item = &'a RawEntry>) -> Self {
        entries.into_iter().fold(self, |chain, entry| {
            chain.then(&entry.json, entry.recorded_at)
        })
    }

    /// The value as 64 hex digits, for the wire.
    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Read a value back from [`to_hex`](Self::to_hex). `None` for anything else.
    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.is_ascii() {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
        }
        // `from_str_radix` takes a leading `+`; hex digits only.
        text.bytes()
            .all(|b| b.is_ascii_hexdigit())
            .then_some(Self(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(offset: Offset, json: &str, recorded_at: Option<i64>) -> RawEntry {
        RawEntry {
            offset,
            recorded_at,
            json: json.to_owned(),
        }
    }

    #[test]
    fn the_same_entries_give_the_same_value() {
        let a = Chain::start().then("{\"a\":1}", Some(5)).then("{}", None);
        let b = Chain::start().then("{\"a\":1}", Some(5)).then("{}", None);
        assert_eq!(a, b);
        assert_ne!(a, Chain::start());
    }

    #[test]
    fn any_difference_changes_the_value() {
        let base = Chain::start().then("{\"a\":1}", Some(5));
        // Another text, another time, a time against none, and none against zero.
        assert_ne!(base, Chain::start().then("{\"a\":2}", Some(5)));
        assert_ne!(base, Chain::start().then("{\"a\":1}", Some(6)));
        assert_ne!(base, Chain::start().then("{\"a\":1}", None));
        assert_ne!(
            Chain::start().then("{}", None),
            Chain::start().then("{}", Some(0))
        );
        // The text is not re-read as JSON: spacing counts.
        assert_ne!(base, Chain::start().then("{\"a\": 1}", Some(5)));
    }

    #[test]
    fn order_counts() {
        let ab = Chain::start().then("a", None).then("b", None);
        let ba = Chain::start().then("b", None).then("a", None);
        assert_ne!(ab, ba);
    }

    #[test]
    fn where_one_entry_ends_and_the_next_begins_counts() {
        // "ab" then "c" is not "a" then "bc", and neither is "abc" alone.
        let one = Chain::start().then("ab", None).then("c", None);
        let two = Chain::start().then("a", None).then("bc", None);
        let three = Chain::start().then("abc", None);
        assert_ne!(one, two);
        assert_ne!(one, three);
        assert_ne!(two, three);
    }

    #[test]
    fn then_all_is_then_in_order() {
        let entries = [
            entry(0, "x", Some(1)),
            entry(1, "y", None),
            entry(2, "z", Some(-3)),
        ];
        let by_hand = Chain::start()
            .then("x", Some(1))
            .then("y", None)
            .then("z", Some(-3));
        assert_eq!(Chain::start().then_all(&entries), by_hand);
        assert_eq!(Chain::start().then_all(&[]), Chain::start());
    }

    #[test]
    fn hex_goes_there_and_back() {
        let chain = Chain::start().then("{}", Some(i64::MIN));
        let hex = chain.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(Chain::from_hex(&hex), Some(chain));
        assert_eq!(Chain::from_hex(&hex.to_uppercase()), Some(chain));
    }

    #[test]
    fn what_is_not_a_value_is_not_read_as_one() {
        let hex = Chain::start().to_hex();
        assert_eq!(Chain::from_hex(""), None);
        assert_eq!(Chain::from_hex(&hex[..63]), None);
        assert_eq!(Chain::from_hex(&format!("{hex}0")), None);
        assert_eq!(Chain::from_hex(&format!("{}g", &hex[..63])), None);
        assert_eq!(Chain::from_hex(&format!("+{}", &hex[..63])), None);
        assert_eq!(Chain::from_hex(&format!("{}é", &hex[..62])), None);
    }
}
