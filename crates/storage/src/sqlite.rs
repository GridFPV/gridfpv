//! SQLite-backed [`EventLog`] (issue #6).
//!
//! Durable storage over `rusqlite` with the `bundled` SQLite, so no system
//! SQLite library is needed — it compiles from source at build time.
//!
//! # Schema
//!
//! A single table holds the log:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS log (
//!     offset      INTEGER PRIMARY KEY,  -- dense, assigned from 0
//!     recorded_at INTEGER,             -- caller micros, nullable
//!     event       TEXT NOT NULL        -- the canonical Event as JSON
//! );
//! ```
//!
//! The offset is assigned explicitly as the current row count (rather than via
//! `AUTOINCREMENT`, which starts at 1 and may leave gaps) so it stays dense and
//! contiguous from `0`, matching [`InMemoryLog`](crate::InMemoryLog). Because
//! `offset` is the `INTEGER PRIMARY KEY` it is the table's rowid: ordered scans
//! and range reads are index lookups, and a duplicate offset would be rejected
//! by the primary-key constraint — a structural guard for the append-only rule.

use crate::{EventLog, Offset, Result, StorageError, StoredEvent};
use gridfpv_events::Event;
use rusqlite::{Connection, OptionalExtension, params};

/// An append-only event log persisted in SQLite.
pub struct SqliteLog {
    conn: Connection,
}

impl SqliteLog {
    /// Open (creating if absent) a log at `path`, ensuring the schema exists.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Open a private, in-memory database — handy for tests and ephemeral
    /// replay scratch space. Nothing is persisted.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    /// Wrap an existing connection, applying pragmas and the schema. Lets a
    /// caller (or a future Cloud backend) supply a tuned connection.
    pub fn from_connection(conn: Connection) -> Result<Self> {
        // Durability + concurrency defaults that suit an append-only log: WAL
        // lets readers replay while a writer appends; FULL sync keeps each
        // committed append crash-safe.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS log (
                offset      INTEGER PRIMARY KEY,
                recorded_at INTEGER,
                event       TEXT NOT NULL
            )",
            [],
        )?;
        Ok(Self { conn })
    }

    /// The next offset to assign = one past the current maximum, or `0` when
    /// empty. Equivalent to the row count given the dense-from-`0` invariant.
    fn next_offset(conn: &Connection) -> Result<Offset> {
        let max: Option<i64> = conn
            .query_row("SELECT MAX(offset) FROM log", [], |row| row.get(0))
            .optional()?
            .flatten();
        Ok(match max {
            Some(m) => (m as u64) + 1,
            None => 0,
        })
    }

    /// Insert one already-serialised event at `offset` using `conn` (which may
    /// be a transaction).
    fn insert(
        conn: &Connection,
        offset: Offset,
        recorded_at: Option<i64>,
        json: &str,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO log (offset, recorded_at, event) VALUES (?1, ?2, ?3)",
            params![offset as i64, recorded_at, json],
        )?;
        Ok(())
    }

    /// Map a `(offset, recorded_at, event_json)` row into a [`StoredEvent`].
    fn row_to_stored(offset: i64, recorded_at: Option<i64>, json: String) -> Result<StoredEvent> {
        if offset < 0 {
            return Err(StorageError::Corrupt(format!(
                "negative offset {offset} in log table"
            )));
        }
        let event: Event = serde_json::from_str(&json)?;
        Ok(StoredEvent {
            offset: offset as u64,
            recorded_at,
            event,
        })
    }

    /// Shared ordered-scan helper for the range reads. `end` of `None` means
    /// "to the end of the log".
    fn select_range(&self, start: Offset, end: Option<Offset>) -> Result<Vec<StoredEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT offset, recorded_at, event
             FROM log
             WHERE offset >= ?1 AND (?2 IS NULL OR offset < ?2)
             ORDER BY offset ASC",
        )?;
        let end_param = end.map(|e| e as i64);
        let rows = stmt.query_map(params![start as i64, end_param], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (offset, recorded_at, json) = row?;
            out.push(Self::row_to_stored(offset, recorded_at, json)?);
        }
        Ok(out)
    }
}

impl EventLog for SqliteLog {
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> Result<Offset> {
        let offset = Self::next_offset(&self.conn)?;
        let json = serde_json::to_string(&event)?;
        Self::insert(&self.conn, offset, recorded_at, &json)?;
        Ok(offset)
    }

    fn append_batch(
        &mut self,
        events: impl IntoIterator<Item = (Event, Option<i64>)>,
    ) -> Result<Offset> {
        // One transaction makes the whole batch atomic: any failure rolls back
        // and leaves the log untouched.
        let tx = self.conn.transaction()?;
        let first = Self::next_offset(&tx)?;
        for (offset, (event, recorded_at)) in (first..).zip(events) {
            let json = serde_json::to_string(&event)?;
            Self::insert(&tx, offset, recorded_at, &json)?;
        }
        tx.commit()?;
        Ok(first)
    }

    fn read_from(&self, start: Offset) -> Result<Vec<StoredEvent>> {
        self.select_range(start, None)
    }

    fn read_range(&self, start: Offset, end: Offset) -> Result<Vec<StoredEvent>> {
        if start >= end {
            return Ok(Vec::new());
        }
        self.select_range(start, Some(end))
    }

    fn len(&self) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM log", [], |row| row.get(0))?;
        Ok(count as u64)
    }
}
