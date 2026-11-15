//! Fetching ffmpeg, so nobody has to install it by hand.
//!
//! Drawing an overlay onto a recording needs ffmpeg, and Grid does not carry one: a build
//! of it is far bigger than Grid itself. When the director asks on the Video page, Grid
//! downloads a ready-made build and keeps it in the `tools` folder of its data folder,
//! where [`crate::render::find_ffmpeg`] looks.
//!
//! The download and the unpacking are done by `curl` and `tar`. Windows 10 and later carry
//! both, and so does nearly every Linux. That keeps a web client and an unpacker for two
//! archive kinds out of Grid.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// What ffmpeg's program file is called on this computer.
pub const FFMPEG_NAME: &str = if cfg!(windows) {
    "ffmpeg.exe"
} else {
    "ffmpeg"
};

/// Where a ready-made ffmpeg for this kind of computer is downloaded from, and the name to
/// keep the download under. `None` where there is no build Grid knows of: there the
/// director installs ffmpeg the usual way.
pub fn source() -> Option<(&'static str, &'static str)> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some((
            "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
            "ffmpeg.zip",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
            "ffmpeg.tar.xz",
        ))
    } else {
        None
    }
}

/// Where a fetched ffmpeg is kept.
pub fn kept_ffmpeg(tools_dir: &Path) -> PathBuf {
    tools_dir.join(FFMPEG_NAME)
}

/// The system's own copy of a program, where Windows keeps one. A `tar` from some other
/// toolkit earlier on the `PATH` may not open a zip; the one Windows ships does.
fn system(program: &str) -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        let own = PathBuf::from(root)
            .join("System32")
            .join(format!("{program}.exe"));
        if own.is_file() {
            return own;
        }
    }
    PathBuf::from(program)
}

/// Run a program to its end, and give its last words when it fails.
async fn run(program: &str, args: &[&std::ffi::OsStr], doing: &str) -> Result<(), String> {
    let mut command = Command::new(system(program));
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::render::quiet(&mut command);
    let output = command
        .output()
        .await
        .map_err(|e| format!("Could not start {program} to {doing}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let last = said.lines().rev().find(|line| !line.trim().is_empty());
    Err(match last {
        Some(line) => format!("Could not {doing}: {}", line.trim()),
        None => format!("Could not {doing}."),
    })
}

/// The first file called `name` under `dir`, however deep.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
    }
    None
}

/// Download ffmpeg from `url` and keep it in `tools_dir`. Answers with where it is.
pub async fn fetch_ffmpeg(tools_dir: &Path, url: &str, archive: &str) -> Result<PathBuf, String> {
    let scratch = tools_dir.join("fetching");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|e| format!("Could not make the folder {}: {e}", scratch.display()))?;
    let result = fetch_into(tools_dir, &scratch, url, archive).await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

async fn fetch_into(
    tools_dir: &Path,
    scratch: &Path,
    url: &str,
    archive: &str,
) -> Result<PathBuf, String> {
    let file = scratch.join(archive);
    run(
        "curl",
        &[
            "--location".as_ref(),
            "--fail".as_ref(),
            "--silent".as_ref(),
            "--show-error".as_ref(),
            "--output".as_ref(),
            file.as_os_str(),
            url.as_ref(),
        ],
        "download ffmpeg",
    )
    .await?;
    run(
        "tar",
        &[
            "-xf".as_ref(),
            file.as_os_str(),
            "-C".as_ref(),
            scratch.as_os_str(),
        ],
        "unpack ffmpeg",
    )
    .await?;
    let found = find_file(scratch, FFMPEG_NAME)
        .ok_or_else(|| "The download did not have ffmpeg in it.".to_string())?;
    let kept = kept_ffmpeg(tools_dir);
    let _ = std::fs::remove_file(&kept);
    std::fs::rename(&found, &kept).map_err(|e| format!("Could not keep ffmpeg: {e}"))?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_found_however_deep_it_sits() {
        let dir = std::env::temp_dir().join(format!("gridfpv-tools-{}", std::process::id()));
        let deep = dir.join("ffmpeg-7.1-essentials_build").join("bin");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("ffprobe"), b"").unwrap();
        assert_eq!(find_file(&dir, "ffmpeg"), None);
        std::fs::write(deep.join("ffmpeg"), b"").unwrap();
        assert_eq!(find_file(&dir, "ffmpeg"), Some(deep.join("ffmpeg")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The real download. Run by hand: `cargo test -p gridfpv-app tools -- --ignored`.
    #[tokio::test]
    #[ignore = "downloads ffmpeg from the internet"]
    async fn ffmpeg_is_fetched_and_runs() {
        let (url, archive) = source().expect("a build for this computer");
        let dir = std::env::temp_dir().join(format!("gridfpv-fetch-{}", std::process::id()));
        let kept = fetch_ffmpeg(&dir, url, archive).await.unwrap();
        assert_eq!(kept, kept_ffmpeg(&dir));
        assert!(!dir.join("fetching").exists());
        let ran = std::process::Command::new(&kept)
            .arg("-version")
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&ran.stdout).starts_with("ffmpeg version"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
