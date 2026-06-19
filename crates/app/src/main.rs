//! GridFPV Director binary — the walking-skeleton entry point.
//!
//! For now this just confirms the workspace wires together. The vertical slice
//! (synthetic source → append → project → read) is filled in across #8/#9.
#![forbid(unsafe_code)]

fn main() {
    println!("GridFPV {} — walking skeleton", env!("CARGO_PKG_VERSION"));
}
