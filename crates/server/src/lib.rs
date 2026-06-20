//! GridFPV protocol server — the one read/realtime contract (snapshot + WS change
//! stream) plus the RD control path, served over axum on the Director (and reused
//! by the Cloud). The wire types are defined here once and generated to TypeScript
//! (ts-rs), so clients never hand-write a wire type. See docs/protocol.html.
#![forbid(unsafe_code)]
