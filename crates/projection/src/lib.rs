//! Projection engine — folds the append-only log into derived read models.
//!
//! Projections are recomputable from the log with no hidden state. The first
//! projection (a lap list) lands in #7; this crate is where it and later
//! projections (standings, brackets, stats) live.
#![forbid(unsafe_code)]
