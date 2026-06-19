//! In-memory [`EventLog`] — the reference implementation.
//!
//! A plain `Vec` of [`StoredEvent`]s whose index *is* the offset. It carries no
//! durability, but it defines the canonical behaviour every other backend is
//! tested against, and is the obvious choice for unit tests and replay scratch
//! space.

use crate::{EventLog, Offset, Result, StoredEvent};
use gridfpv_events::Event;

/// An append-only event log held entirely in memory.
///
/// The vector index doubles as the [`Offset`], which keeps the dense-from-`0`
/// contract trivially true.
#[derive(Debug, Default, Clone)]
pub struct InMemoryLog {
    entries: Vec<StoredEvent>,
}

impl InMemoryLog {
    /// Create an empty in-memory log.
    pub fn new() -> Self {
        Self::default()
    }
}

impl EventLog for InMemoryLog {
    fn append(&mut self, event: Event, recorded_at: Option<i64>) -> Result<Offset> {
        let offset = self.entries.len() as Offset;
        self.entries.push(StoredEvent {
            offset,
            recorded_at,
            event,
        });
        Ok(offset)
    }

    fn append_batch(
        &mut self,
        events: impl IntoIterator<Item = (Event, Option<i64>)>,
    ) -> Result<Offset> {
        let first = self.entries.len() as Offset;
        for (event, recorded_at) in events {
            // Reuse the single-append path so the offset rule lives in one place.
            self.append(event, recorded_at)?;
        }
        Ok(first)
    }

    fn read_from(&self, start: Offset) -> Result<Vec<StoredEvent>> {
        let start = (start as usize).min(self.entries.len());
        Ok(self.entries[start..].to_vec())
    }

    fn read_range(&self, start: Offset, end: Offset) -> Result<Vec<StoredEvent>> {
        let len = self.entries.len();
        let start = (start as usize).min(len);
        let end = (end as usize).min(len);
        if start >= end {
            return Ok(Vec::new());
        }
        Ok(self.entries[start..end].to_vec())
    }

    fn len(&self) -> Result<u64> {
        Ok(self.entries.len() as u64)
    }
}
