//! Canonical event model for GridFPV — the schema of the append-only log.
//!
//! Everything in the spine appends and folds over these types. The concrete
//! event model (Pass and friends, with serde) lands in issue #3; this crate is
//! the skeleton it slots into.
#![forbid(unsafe_code)]
