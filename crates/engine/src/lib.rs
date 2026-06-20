//! GridFPV race engine — the heat-loop state machine, scoring, marshaling, and
//! format generators that turn canonical passes + a format into a run race.
//!
//! The engine is a **pure function of the log** (race-engine.html §6): it consumes
//! canonical `Pass`/lifecycle events and appends race-state events; laps, results,
//! and standings remain projections. Time/RNG are injected, never read, so a
//! recorded session always replays identically.
#![forbid(unsafe_code)]

pub mod heat;
