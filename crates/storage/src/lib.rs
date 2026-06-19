//! Storage for the append-only event log.
//!
//! Defines the backend-agnostic storage trait (append + ordered range-read)
//! and its SQLite implementation. The trait lands in #5, the SQLite backend in
//! #6; Cloud later reuses the same trait over Postgres.
#![forbid(unsafe_code)]
