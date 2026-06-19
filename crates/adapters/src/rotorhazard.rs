//! RotorHazard adapter (#23).
//!
//! The first real-world target and the full-signal case. RotorHazard exposes pass
//! events over Socket.IO / RHAPI with per-node RSSI and EnterAt/ExitAt thresholds
//! plus RSSI history, and a race start/stop lifecycle. Each pass becomes a `Pass`
//! with signal context; node seats are reported via `CompetitorSeen`; server time
//! is the source clock.
//!
//! Capabilities: live passes ✓, signal ✓, calibration ✓, signal-based recovery ✓,
//! frequency mgmt ✓, source lifecycle ✓. Ref: `docs/timer-adapters.html` §8.
//!
//! Implemented in #23 — this module is the agreed home for it.
