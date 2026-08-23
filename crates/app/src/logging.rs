//! **Always-on file logging** for the Director (#380).
//!
//! # Why this exists (and why it is not `tauri-plugin-log`)
//!
//! Every diagnostic this crate produces used to go to **stderr only**. That is fine for the
//! hosted dev loop (`cargo run`, a terminal attached) and *completely useless* for the
//! shipped desktop build: `src-tauri/src/main.rs` sets
//! `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` so the Windows
//! release is a **GUI-subsystem process with no console**. Such a process has no stderr to
//! write to — the writes are discarded, and even shell redirection
//! (`gridfpv-desktop.exe > log.txt 2>&1`) produces an *empty file*. That is exactly what
//! happened in the field on 2026-08-24: a RotorHazard timer showed `Error` and the carefully
//! built connect **error chain** in [`crate::source::rotorhazard`] — the one line that tells
//! a refused TCP connect apart from a TLS fault or an engine.io handshake reject — was
//! thrown away.
//!
//! So this module **does not depend on stderr in any way**. It opens a real file itself with
//! `OpenOptions` and writes to it with `write_all` + `flush`. Whether a console exists is
//! irrelevant to whether the log gets written. Echoing to stderr as well is a *best-effort*
//! extra so the terminal dev loop reads exactly as it did before; if that echo fails it is
//! ignored.
//!
//! `tauri-plugin-log` was considered and rejected for this job:
//!
//! - It captures the **`log` crate facade**, not `eprintln!`. Every existing diagnostic in
//!   this crate is an `eprintln!`, so the plugin would file exactly *none* of them without
//!   rewriting every call site — including the RH error chain, the whole point of the issue.
//! - It lives in `src-tauri`, a **separate workspace** deliberately excluded from the root one
//!   (CI runners have no webkit2gtk), so it is never compiled by `cargo xtask ci`. Putting
//!   the field-diagnostics lifeline behind a dependency nothing in CI compiles is the same
//!   class of mistake the issue is about.
//! - It would leave the **hosted** `gridfpv` Director with no log file at all.
//!
//! Living in `gridfpv-app` instead means the hosted binary and the native desktop app write
//! the *same* file, from the *same* code, checked by the *same* `cargo check --workspace`.
//!
//! # How diagnostics get here
//!
//! `lib.rs` declares this module with `#[macro_use]`, which puts the `macro_rules!`
//! `eprintln!` defined at the bottom of this file into textual scope for **every module
//! declared after it** — `director`, `source`, `source::rotorhazard`, and so on. That local
//! macro **shadows the std prelude's `eprintln!`**, so every existing and *future*
//! `eprintln!` in this crate is routed through [`record`] and lands in the file, with no
//! call-site churn and no way for a new diagnostic to silently regress the fix. The
//! `gridfpv` binary and the desktop shell (`src-tauri/src/lib.rs`) declare the same shadows
//! at their own crate roots for the same reason.
//!
//! # Where the file lives
//!
//! The platform per-user app-data/log directory (see [`resolve_log_dir`]), overridable with
//! `GRIDFPV_LOG_DIR`. Deliberately **not** the portable `gridfpv-data/` folder beside the
//! executable: the log must still be written when the exe sits on a read-only mount, and an
//! RD asked to "send me the log" needs one predictable place per platform.
//!
//! The file is **appended across runs** (with a session banner per start) rather than
//! truncated, so a crash-relaunch-crash loop — the exact field scenario — keeps the earlier
//! sessions. It rotates by **size** ([`MAX_BYTES`], keeping [`MAX_FILES`] generations), so it
//! can never grow without bound.

use std::backtrace::Backtrace;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate once the active log file passes this size (5 MiB). Generous enough that a whole
/// race day fits in the live file, small enough to email.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// How many generations to keep: the live `gridfpv.log` plus `gridfpv.log.1 ..= .4`.
pub const MAX_FILES: u32 = 5;

/// The log file's name inside the resolved log directory.
pub const LOG_FILE_NAME: &str = "gridfpv.log";

/// Env var that overrides the log directory (tests, and anyone who wants the log somewhere
/// specific). Unset ⇒ the platform app-data/log dir.
pub const LOG_DIR_ENV: &str = "GRIDFPV_LOG_DIR";

/// The one process-wide sink. `None` ⇒ no writable log directory could be resolved at all
/// (every fallback failed); diagnostics then still reach stderr exactly as they did before
/// this module existed, so logging can never end up *worse* than the old behavior.
static SINK: OnceLock<Option<Sink>> = OnceLock::new();

/// The resolved log destination: where it is, and the open handle behind a mutex.
struct Sink {
    path: PathBuf,
    dir: PathBuf,
    state: Mutex<Open>,
}

/// The open file plus the byte count that drives size rotation.
struct Open {
    file: File,
    written: u64,
}

/// Open the log file (creating its directory), write the session banner, and install the
/// panic hook. Idempotent: safe to call from every entry point, and called lazily by
/// [`record`] so a diagnostic can never be lost to a forgotten `init()`.
///
/// Returns the active log file's path, or `None` when no writable directory could be found.
pub fn init() -> Option<&'static Path> {
    sink().map(|s| s.path.as_path())
}

/// The active log file's path, if logging is running. Served to the console over
/// `GET /diagnostics` (see [`crate::director::build_app`]) and printed in the startup banner,
/// so an RD can find the file without being told where to look.
pub fn log_file() -> Option<&'static Path> {
    sink().map(|s| s.path.as_path())
}

/// The directory holding the log file and its rotated generations.
pub fn log_dir() -> Option<&'static Path> {
    sink().map(|s| s.dir.as_path())
}

/// Record one diagnostic line: timestamped into the log file, and echoed to **stderr** as a
/// best effort so an attached terminal reads exactly as it did before.
///
/// This is what the `eprintln!` shadow macro expands to. It takes `fmt::Arguments` so call
/// sites keep `format!`-style syntax.
pub fn record(args: fmt::Arguments<'_>) {
    emit(args, Stream::Stderr);
}

/// Like [`record`], but the console echo goes to **stdout** — used by the `println!` shadow
/// in the `gridfpv` binary so the startup banner keeps landing on stdout while *also* being
/// filed. The banner carries the bound address, the RD-token state, the data dir and the
/// active lap source: precisely the context a field log needs above the errors.
pub fn record_stdout(args: fmt::Arguments<'_>) {
    emit(args, Stream::Stdout);
}

enum Stream {
    Stdout,
    Stderr,
}

fn emit(args: fmt::Arguments<'_>, stream: Stream) {
    // Format once; the same text goes to the file (timestamped) and the console (bare).
    let message = fmt::format(args);

    // The FILE first, and unconditionally — it is the only sink that survives a
    // GUI-subsystem build. A failure below must not cost us it.
    if let Some(sink) = sink() {
        sink.write_line(&message);
    }

    // Best-effort console echo. On a Windows GUI-subsystem process these writes go nowhere;
    // that is fine, and is exactly why the file above is not conditional on them.
    match stream {
        Stream::Stdout => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{message}");
            let _ = out.flush();
        }
        Stream::Stderr => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "{message}");
        }
    }
}

impl Sink {
    /// Append one timestamped line, rotating first if this write would push the file past
    /// [`MAX_BYTES`]. Every failure is swallowed: logging must never take the Director down.
    fn write_line(&self, message: &str) {
        let line = format!("{} {message}\n", timestamp(SystemTime::now()));
        let mut open = match self.state.lock() {
            Ok(open) => open,
            // A previous writer panicked mid-write. Drop the line rather than poison the
            // whole Director.
            Err(_) => return,
        };

        if open.written.saturating_add(line.len() as u64) > MAX_BYTES {
            self.rotate(&mut open);
        }

        if open.file.write_all(line.as_bytes()).is_ok() {
            open.written = open.written.saturating_add(line.len() as u64);
        }
        // `File` is unbuffered, so `write_all` already reached the OS; the flush is belt and
        // braces so a hard kill loses nothing.
        let _ = open.file.flush();
    }

    /// Append a line from the **panic hook**, which must never block.
    ///
    /// A panic raised *inside* [`Sink::write_line`] would still be holding `state`, and a
    /// plain `lock()` on the same thread would then deadlock the process instead of letting
    /// it die. `try_lock` turns that into a dropped line, and skips rotation (a panic report
    /// is worth a few bytes over the cap).
    fn panic_line(&self, message: &str) {
        if let Ok(mut open) = self.state.try_lock() {
            let line = format!("{} {message}\n", timestamp(SystemTime::now()));
            let _ = open.file.write_all(line.as_bytes());
            let _ = open.file.flush();
            open.written = open.written.saturating_add(line.len() as u64);
        }
    }

    /// Shift `gridfpv.log.{n}` → `.{n+1}` (dropping the oldest), move the live file to `.1`,
    /// and reopen a fresh live file. On any failure we keep writing to the file we already
    /// have — an over-long log beats a lost log.
    fn rotate(&self, open: &mut Open) {
        for n in (1..MAX_FILES).rev() {
            let from = self.dir.join(format!("{LOG_FILE_NAME}.{n}"));
            let to = self.dir.join(format!("{LOG_FILE_NAME}.{}", n + 1));
            if from.exists() {
                if n + 1 >= MAX_FILES {
                    let _ = fs::remove_file(&to);
                }
                let _ = fs::rename(&from, &to);
            }
        }
        let first = self.dir.join(format!("{LOG_FILE_NAME}.1"));
        let _ = fs::remove_file(&first);
        if fs::rename(&self.path, &first).is_err() {
            // Windows can refuse a rename while a handle is open; keep appending in place.
            return;
        }
        if let Ok(file) = open_append(&self.path) {
            open.file = file;
            open.written = 0;
        }
    }
}

/// Resolve (once) the process-wide sink.
fn sink() -> Option<&'static Sink> {
    SINK.get_or_init(open_sink).as_ref()
}

fn open_sink() -> Option<Sink> {
    let dir = resolve_log_dir()?;
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(LOG_FILE_NAME);
    let file = open_append(&path).ok()?;
    let written = file.metadata().map(|m| m.len()).unwrap_or(0);

    let sink = Sink {
        path,
        dir,
        state: Mutex::new(Open { file, written }),
    };

    // Session banner: which build, when, and from where. A field log that does not say which
    // binary produced it is half a log.
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    sink.write_line(&format!(
        "==== GridFPV {} — session start (pid {}, {} {}, exe {exe}) ====",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ));

    install_panic_hook();
    Some(sink)
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// File the panic message **and** a backtrace before the default hook runs.
///
/// On a GUI-subsystem build a panic is otherwise perfectly silent — the window vanishes and
/// nothing anywhere says why. `force_capture` ignores `RUST_BACKTRACE` (no RD is going to set
/// it) and only costs anything on the way down.
fn install_panic_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(sink) = SINK.get().and_then(Option::as_ref) {
                sink.panic_line(&format!("PANIC: {info}"));
                sink.panic_line(&format!("backtrace:\n{}", Backtrace::force_capture()));
            }
            previous(info);
        }));
    });
}

/// The per-platform per-user log directory, honoring `GRIDFPV_LOG_DIR` first.
///
/// The layout matches the convention Tauri's own app-data/log resolution uses, so the file
/// sits where a Windows/macOS user would look for it:
///
/// - **Windows** — `%LOCALAPPDATA%\GridFPV\logs`
/// - **macOS** — `~/Library/Logs/GridFPV`
/// - **Linux/other** — `$XDG_DATA_HOME/GridFPV/logs`, else `~/.local/share/GridFPV/logs`
///
/// with the system temp dir as a last resort, so this returns `None` only if even that is
/// unusable. Resolved from environment variables rather than a `dirs`-style crate so
/// `gridfpv-app` picks up **no new dependency** for logging.
fn resolve_log_dir() -> Option<PathBuf> {
    if let Some(dir) = env_path(LOG_DIR_ENV) {
        return Some(dir);
    }

    let from_platform = if cfg!(windows) {
        env_path("LOCALAPPDATA")
            .or_else(|| env_path("APPDATA"))
            .or_else(|| env_path("USERPROFILE").map(|p| p.join("AppData").join("Local")))
            .map(|base| base.join("GridFPV").join("logs"))
    } else if cfg!(target_os = "macos") {
        env_path("HOME").map(|home| home.join("Library").join("Logs").join("GridFPV"))
    } else {
        env_path("XDG_DATA_HOME")
            .or_else(|| env_path("HOME").map(|home| home.join(".local").join("share")))
            .map(|base| base.join("GridFPV").join("logs"))
    };

    from_platform.or_else(|| Some(std::env::temp_dir().join("GridFPV").join("logs")))
}

fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Some(PathBuf::from(value)),
        _ => None,
    }
}

/// `YYYY-MM-DDThh:mm:ss.mmmZ` (UTC) — hand-rolled so logging adds no date-time dependency.
///
/// UTC, not local time: a log emailed from a field laptop set to some other timezone is read
/// against event timestamps that are already epoch-based.
fn timestamp(now: SystemTime) -> String {
    let since_epoch = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return "0000-00-00T00:00:00.000Z".to_string(),
    };
    let secs = since_epoch.as_secs() as i64;
    let millis = since_epoch.subsec_millis();

    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (sod / 3600, (sod % 3600) / 60, sod % 60);

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's `civil_from_days`: days-since-1970-01-01 → (year, month, day), proleptic
/// Gregorian. Exact for every date we could ever stamp.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Route **every** `eprintln!` in this crate to the log file (and still to stderr).
///
/// This deliberately shadows the std prelude's `eprintln!` for every module declared after
/// `#[macro_use] pub mod logging;` in `lib.rs`. It is the whole reason the RotorHazard
/// connect error chain — written as a plain `eprintln!` — reaches the file with no change to
/// `source/rotorhazard.rs`, and the reason a *future* `eprintln!` cannot silently re-break
/// #380. Use `std::eprintln!` explicitly if you ever want a console-only write.
macro_rules! eprintln {
    ($($arg:tt)*) => {
        $crate::logging::record(::std::format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_renders_a_known_instant() {
        // 2026-08-23T12:34:56.789Z
        let t = UNIX_EPOCH + std::time::Duration::new(1_787_488_496, 789_000_000);
        assert_eq!(timestamp(t), "2026-08-23T12:34:56.789Z");
    }

    #[test]
    fn timestamp_renders_the_epoch_and_a_leap_day() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + std::time::Duration::from_secs(1_709_164_800); // 2024-02-29
        assert!(timestamp(leap).starts_with("2024-02-29T"));
    }

    #[test]
    fn civil_from_days_matches_known_boundaries() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
    }

    /// A log directory **always** resolves — the temp-dir fallback makes `None` unreachable
    /// in practice, which is the guarantee "the Director always writes a log file" rests on.
    #[test]
    fn a_log_dir_always_resolves() {
        let resolved = resolve_log_dir().expect("the temp-dir fallback makes this infallible");
        assert!(!resolved.as_os_str().is_empty());
    }

    /// Rotation shifts generations and truncates the live file, and the live file is always
    /// the one named [`LOG_FILE_NAME`].
    #[test]
    fn rotation_shifts_generations_and_reopens_the_live_file() {
        let dir = std::env::temp_dir().join(format!("gridfpv-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir is creatable");
        let path = dir.join(LOG_FILE_NAME);

        let file = open_append(&path).expect("log file opens");
        let sink = Sink {
            path: path.clone(),
            dir: dir.clone(),
            state: Mutex::new(Open { file, written: 0 }),
        };
        sink.write_line("first session line");

        {
            let mut open = sink.state.lock().expect("uncontended");
            sink.rotate(&mut open);
        }
        sink.write_line("after rotation");

        let rotated = fs::read_to_string(dir.join(format!("{LOG_FILE_NAME}.1"))).expect("gen 1");
        assert!(rotated.contains("first session line"));
        let live = fs::read_to_string(&path).expect("live file");
        assert!(live.contains("after rotation"));
        assert!(!live.contains("first session line"));

        let _ = fs::remove_dir_all(&dir);
    }
}
