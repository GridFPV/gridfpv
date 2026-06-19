//! Reconnect + deduplication (#21).
//!
//! A source that drops mid-race reconnects and resumes appending. Deduplication
//! keys on source sequence numbers / timestamps so a reconnect can't double-count a
//! pass that was already recorded.
//!
//! Implemented in #21 — this module is the agreed home for it.
