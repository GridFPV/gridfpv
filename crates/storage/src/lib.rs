//! Storage for the append-only event log.
//!
//! This crate defines the backend-agnostic [`EventLog`] trait — the single
//! contract every storage backend honours — together with the [`StoredEvent`]
//! envelope it hands back, an [`InMemoryLog`] reference implementation, and a
//! durable [`SqliteLog`] backend.
//!
//! # The append-only contract
//!
//! The log is the single source of truth for a GridFPV event. It is strictly
//! append-only: entries are never mutated or deleted once written. Each append
//! assigns the entry a monotonically increasing [`Offset`], and reads always
//! replay entries in that same append order, so any projection (laps, results,
//! standings) can be recomputed deterministically by replaying from offset `0`.
//!
//! ## Offsets
//!
//! Offsets are `u64`, **contiguous and dense starting at `0`**: the first event
//! appended to an empty log is offset `0`, the next `1`, and so on with no gaps.
//! A reader can therefore treat [`EventLog::len`] as "one past the last offset"
//! and resume a replay from any offset it has already consumed.
//!
//! The trait lands in issue #5, the SQLite backend in #6; Cloud (later) reuses
//! the same trait over Postgres.
#![forbid(unsafe_code)]

mod memory;
mod sqlite;

pub use memory::InMemoryLog;
pub use sqlite::SqliteLog;

use gridfpv_events::Event;

/// A log position assigned to an appended event.
///
/// Dense and contiguous from `0`; see the crate docs for the offset contract.
pub type Offset = u64;

/// An [`Event`] as stored in the log: the canonical event plus the metadata the
/// log assigned to it.
///
/// This is what reads hand back. It is deliberately minimal — the assigned
/// [`Offset`] plus an optional caller-supplied arrival timestamp. The event's
/// own authoritative timing lives inside the [`Event`] (e.g. `SourceTime` on a
/// `Pass`); `recorded_at` is only the moment the log received the entry, when
/// the caller chose to record one.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    /// The position this event occupies in the append order.
    pub offset: Offset,
    /// Microseconds, on whatever clock the caller supplied at append time, for
    /// when the log received this entry. `None` when the caller did not record
    /// one. The log never invents a value of its own — keeping the type free of
    /// any wall-clock dependency.
    pub recorded_at: Option<i64>,
    /// The canonical event itself.
    pub event: Event,
}

/// Errors a storage backend can return.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// (De)serialising an event to/from its stored form failed.
    #[error("failed to (de)serialise a stored event: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The underlying SQLite backend reported an error.
    #[error("sqlite backend error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A stored row was structurally invalid (e.g. a negative offset that
    /// cannot be a `u64`). Indicates corruption or external tampering, which
    /// the append-only invariant forbids.
    #[error("corrupt log entry: {0}")]
    Corrupt(String),
}

/// Result alias for storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

/// A backend-agnostic, append-only event log.
///
/// Implementations assign each appended [`Event`] a dense, monotonically
/// increasing [`Offset`] (from `0`) and replay entries in that append order.
/// They never mutate or remove past entries.
///
/// The read methods return owned [`StoredEvent`] vectors. For v0.1 an event is
/// small and a full log fits comfortably in memory; a streaming/iterator API
/// can be layered on later without changing this contract.
pub trait EventLog {
    /// Append a single event, optionally tagging it with a caller-supplied
    /// arrival timestamp (microseconds, caller's clock). Returns the offset
    /// assigned to it.
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> Result<Offset>;

    /// Append a batch of events in order, each optionally tagged with its own
    /// arrival timestamp. Returns the offset assigned to the *first* event in
    /// the batch; the rest follow contiguously. Appending an empty batch is a
    /// no-op that returns the next offset that *would* be assigned (i.e.
    /// [`EventLog::len`]).
    ///
    /// Backends should make a batch atomic where they can (the SQLite backend
    /// wraps it in a transaction), so a failure leaves the log unchanged.
    fn append_batch(
        &mut self,
        events: impl IntoIterator<Item = (Event, Option<i64>)>,
    ) -> Result<Offset>;

    /// Read every entry, in append order.
    fn read_all(&self) -> Result<Vec<StoredEvent>> {
        self.read_from(0)
    }

    /// Read every entry from `start` (inclusive) to the end, in append order.
    /// A `start` at or beyond the end yields an empty vector.
    fn read_from(&self, start: Offset) -> Result<Vec<StoredEvent>>;

    /// Read entries in the half-open offset range `[start, end)`, in append
    /// order. If `start >= end` the result is empty; `end` beyond the log end
    /// is clamped.
    fn read_range(&self, start: Offset, end: Offset) -> Result<Vec<StoredEvent>>;

    /// The number of entries in the log, which equals the offset that the next
    /// appended event will receive.
    fn len(&self) -> Result<u64>;

    /// Whether the log has no entries.
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}
