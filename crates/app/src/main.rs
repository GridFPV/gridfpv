//! GridFPV Director binary — the walking-skeleton entry point.
//!
//! Runs the whole spine for a synthetic session and prints the derived lap list:
//! synthetic source → append to the SQLite log → read back → project → render.
#![forbid(unsafe_code)]

use gridfpv_app::{SyntheticPilot, append_and_project, render_lap_list, synthetic_session};
use gridfpv_storage::SqliteLog;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("GridFPV {} — walking skeleton\n", env!("CARGO_PKG_VERSION"));

    let events = synthetic_session(
        "sim",
        &[
            SyntheticPilot {
                name: "Ace",
                lap_micros: &[30_000_000, 31_000_000, 29_500_000],
            },
            SyntheticPilot {
                name: "Bee",
                lap_micros: &[33_000_000, 32_250_000],
            },
        ],
    );

    // Append to a real (in-memory) SQLite log, then derive the read model from it.
    let mut log = SqliteLog::open_in_memory()?;
    let laps = append_and_project(&mut log, &events)?;

    print!("{}", render_lap_list(&laps));
    Ok(())
}
