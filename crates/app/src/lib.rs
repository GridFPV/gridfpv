//! The GridFPV **Director** as a library: the server wiring ([`director`]) and the always-on
//! diagnostics log ([`logging`]).
//!
//! The `gridfpv` binary (`main.rs`) and the Tauri desktop shell (`src-tauri`) both run the
//! same [`director::run_director`], so the two never drift.
#![forbid(unsafe_code)]

// **Declared first, and with `#[macro_use]`, on purpose** (#380).
//
// `logging` defines a `macro_rules! eprintln` that shadows the std prelude's, routing every
// diagnostic in this crate into an always-on **log file** as well as stderr. `macro_rules!`
// scope is *textual*, so the shadow only reaches modules declared **after** this line — which
// is why `director` comes below it. Moving this declaration down would silently un-file those
// messages on the shipped desktop build.
#[macro_use]
pub mod logging;
pub mod discovery;
pub mod links;
pub mod oft;
pub mod rotorhazard;

pub mod director;
