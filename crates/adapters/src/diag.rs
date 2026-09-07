//! Diagnostic output for adapters, routed to whatever the host process logs to (#380).
//!
//! Adapters sit **below** `gridfpv-app`, so they cannot call its logging module directly. But
//! their diagnostics are exactly the ones an operator needs in the field — the pass-source
//! selection line, the "plugin advertised `live_pass` but never delivered lap N" fallback
//! warning, and the per-heat pass summary (#389). Writing those to `stderr` loses them on the
//! shipped Windows desktop build, which is GUI-subsystem and therefore has no stderr at all —
//! the precise failure #380 exists to fix.
//!
//! So the host installs a sink once at startup and every adapter diagnostic goes through it.
//! With no sink installed (tests, the dev loop, any library consumer) it falls back to
//! `eprintln!`, so nothing is lost where stderr *does* work.
use std::sync::OnceLock;

/// Where adapter diagnostics go. Installed once by the host; `None` until then.
static SINK: OnceLock<fn(&str)> = OnceLock::new();

/// Install the process-wide diagnostic sink. First caller wins — later calls are ignored rather
/// than racing, since a second sink would silently split the log. Returns whether it took effect.
pub fn set_sink(sink: fn(&str)) -> bool {
    SINK.set(sink).is_ok()
}

/// Emit one diagnostic line. Goes to the installed sink, or `stderr` when none is installed.
pub fn emit(line: &str) {
    match SINK.get() {
        Some(sink) => sink(line),
        None => eprintln!("{line}"),
    }
}

/// `eprintln!`-shaped diagnostics that survive a console-less process. Use this in adapter code
/// instead of `eprintln!` — see the module docs for why.
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::diag::emit(&format!($($arg)*))
    };
}
