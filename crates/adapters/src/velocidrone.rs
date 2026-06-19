//! Velocidrone adapter (#22).
//!
//! Built first, on purpose — the simulator has no RSSI, no thresholds, no
//! frequencies, and a clock that isn't ours, so making the canonical model fit it
//! cleanly keeps the abstraction honest. Velocidrone exposes a WebSocket feed
//! carrying gate passes, lap splits, lap times and totals from the game engine;
//! each gate crossing becomes a `Pass` (with split/gate index), the sim player name
//! is reported via `CompetitorSeen`, and the game's lap times/totals are advisory
//! cross-checks (the engine derives laps from the pass stream).
//!
//! Capabilities: live passes ✓, splits ✓, source lifecycle ✓; signal ✗,
//! calibration ✗, frequency ✗. Ref: `docs/timer-adapters.html` §7.
//!
//! Implemented in #22 — this module is the agreed home for it.
