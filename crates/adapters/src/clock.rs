//! Clock alignment + sequence handling (#20).
//!
//! Every source stamps in its own clock (game-engine, server, device RTC, wall).
//! At session start an adapter captures an offset mapping its source clock onto the
//! Director's session timeline, so events from different sources share one axis;
//! source time stays authoritative for intervals. Where a source exposes a
//! monotonic sequence counter, the adapter carries it through.
//!
//! Implemented in #20 — this module is the agreed home for it.
