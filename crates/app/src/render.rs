//! Drawing a race's overlay onto its clean recording, after the fact.
//!
//! A race recorded clean (the camera, no overlay) can have the overlay drawn onto it at any
//! time, with the laps as they stand now. That is what makes a marshal's change show in the
//! video: the laps are fixed, and the overlay is drawn again.
//!
//! Two programs on the Director's machine do the work:
//!
//! - **A browser** (Chrome, Edge or Chromium), run with no window. It opens the console's
//!   replay page for the race (`#/overlay/replay/‹event›/‹race›`), which draws the overlay
//!   as it stood at any moment of the recording. The renderer steps that page through the
//!   recording one frame at a time and takes a see-through picture of each. The browser is
//!   driven over its DevTools connection ([`Cdp`]).
//! - **ffmpeg**, which gets the pictures on its standard input, lays them over the clean
//!   recording, and writes the finished video beside it.
//!
//! One render runs at a time. Each is a [`RenderJob`] on the video desk, which is where the
//! console reads its progress from. A finished render is logged into the event as
//! [`Event::VideoRendered`], with the mark of the laps it shows.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use gridfpv_events::{Event, RaceId};
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::video::{
    RenderJob, RenderSource, RenderState, RenderTools, RenderWhen, log_video, races_needing_render,
    render_source,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// The most pictures drawn for each second of video. The overlay's fastest-moving part is
/// the clock's tenths, so more would only take longer.
const MAX_FPS: f64 = 30.0;
/// How long the browser is given to start, and the replay page to load its race.
const BROWSER_START: Duration = Duration::from_secs(30);
/// How long the browser is given to answer one request.
const BROWSER_ANSWER: Duration = Duration::from_secs(30);
/// How long ffmpeg is given to finish the file once the last picture is in.
const FFMPEG_FINISH: Duration = Duration::from_secs(600);
/// How often the renderer looks for work when nobody has asked.
const LOOK: Duration = Duration::from_secs(3);

/// Run the renderer until its task is aborted. `base` is where this Director serves the
/// console, as the browser on this machine reaches it: `http://127.0.0.1:8080`.
pub fn spawn(registry: EventRegistry, base: String) -> JoinHandle<()> {
    tokio::spawn(async move {
        let desk = registry.video();
        let mut asked = desk.watch_renders();
        // What "render every race by itself" has already tried: a race is tried once for
        // each state of its laps, not every few seconds for as long as something is wrong.
        let mut tried: Vec<(EventId, RaceId, String)> = Vec::new();
        let tools_dir = desk.tools_dir();
        let mut fetch_problem: Option<String> = None;
        loop {
            let settings = desk.stored();
            let tools = |fetching: bool, fetch_problem: &Option<String>| RenderTools {
                browser: find_browser(&settings.browser_path),
                ffmpeg: find_ffmpeg(&settings.ffmpeg_path, tools_dir.as_deref()),
                can_fetch: tools_dir.is_some() && crate::tools::source().is_some(),
                fetching,
                fetch_problem: fetch_problem.clone(),
            };
            desk.report_tools(tools(false, &fetch_problem));
            if desk.take_ffmpeg_ask()
                && let (Some(dir), Some((url, archive))) = (&tools_dir, crate::tools::source())
            {
                fetch_problem = None;
                desk.report_tools(tools(true, &fetch_problem));
                if let Err(why) = crate::tools::fetch_ffmpeg(dir, url, archive).await {
                    eprintln!("WARNING: ffmpeg was not downloaded: {why}");
                    fetch_problem = Some(why);
                }
                desk.report_tools(tools(false, &fetch_problem));
            }
            if settings.render_when == RenderWhen::Always
                && let Some(event) = registry.active()
            {
                for (race, name) in races_needing_render(&registry, &event.id) {
                    let Ok(source) = render_source(&registry, &event.id, &race) else {
                        continue;
                    };
                    let key = (event.id.clone(), race.clone(), source.laps_key);
                    if !tried.contains(&key) {
                        tried.push(key);
                        desk.ask_render(&event.id, &race, &name);
                    }
                }
            }
            while let Some(job) = desk.next_render() {
                run(&registry, &base, &job).await;
            }
            tokio::select! {
                changed = asked.changed() => if changed.is_err() { return },
                () = tokio::time::sleep(LOOK) => {}
            }
        }
    })
}

/// Do one job, and say on the desk how it went.
async fn run(registry: &EventRegistry, base: &str, job: &RenderJob) {
    let desk = registry.video();
    desk.report_render(
        &job.event,
        &job.race,
        RenderState::Rendering { percent: 0 },
        None,
    );
    let outcome = render(registry, base, job).await;
    match outcome {
        Ok(()) => desk.report_render(&job.event, &job.race, RenderState::Done, None),
        Err(why) => {
            eprintln!("WARNING: the overlay for {} was not drawn: {why}", job.name);
            desk.report_render(&job.event, &job.race, RenderState::Failed, Some(why));
        }
    }
}

async fn render(registry: &EventRegistry, base: &str, job: &RenderJob) -> Result<(), String> {
    let desk = registry.video();
    let settings = desk.stored();
    let source = render_source(registry, &job.event, &job.race).map_err(|e| e.to_string())?;
    if !Path::new(&source.file).is_file() {
        return Err(format!(
            "The clean recording is not on this machine: {}. The overlay can only be drawn where the recording is.",
            source.file
        ));
    }
    let ffmpeg = find_ffmpeg(&settings.ffmpeg_path, desk.tools_dir().as_deref())
        .ok_or("Grid could not find ffmpeg. Install it, or say where it is on the Video page.")?;
    let browser = find_browser(&settings.browser_path).ok_or(
        "Grid could not find Chrome, Edge or Chromium. Install one, or say where it is on the Video page.",
    )?;
    let film = probe(&ffmpeg, &source.file).await?;
    let fps = film.fps.unwrap_or(MAX_FPS).clamp(1.0, MAX_FPS).round();
    let frames = (film.seconds * fps).ceil() as u64;
    if frames == 0 {
        return Err("The clean recording is empty.".into());
    }

    let out = overlaid_path(&source.file);
    let part = PathBuf::from(format!("{}.part", out.display()));
    let mut browser = Browser::launch(&browser).await?;
    let drawn = draw(
        registry,
        &mut browser,
        base,
        &source,
        &ffmpeg,
        &part,
        fps,
        frames,
    )
    .await;
    browser.close().await;
    if let Err(why) = drawn {
        let _ = std::fs::remove_file(&part);
        return Err(why);
    }
    std::fs::rename(&part, &out)
        .map_err(|e| format!("The finished video could not be put in place: {e}"))?;
    log_video(
        registry,
        &job.event,
        Event::VideoRendered {
            race: job.race.clone(),
            file: out.to_string_lossy().into_owned(),
            laps_key: source.laps_key.clone(),
        },
    );
    Ok(())
}

/// Step the replay page through the recording, and feed ffmpeg a picture for every frame.
#[allow(clippy::too_many_arguments)]
async fn draw(
    registry: &EventRegistry,
    browser: &mut Browser,
    base: &str,
    source: &RenderSource,
    ffmpeg: &str,
    part: &Path,
    fps: f64,
    frames: u64,
) -> Result<(), String> {
    let desk = registry.video();
    let url = format!(
        "{base}/#/overlay/replay/{}/{}",
        url_part(&source.event.0),
        url_part(&source.race.0)
    );
    browser.open(&url).await?;

    let mut command = Command::new(ffmpeg);
    command
        .args(composite_args(&source.file, part, fps))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    quiet(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("ffmpeg could not be started: {e}"))?;
    let mut pictures = child.stdin.take().expect("stdin is piped");
    let mut errors = child.stderr.take().expect("stderr is piped");
    let said = tokio::spawn(async move {
        let mut text = String::new();
        let _ = errors.read_to_string(&mut text).await;
        text
    });
    // Why ffmpeg stopped, in its own last words.
    let ffmpeg_said = |text: String| {
        let last = text.lines().rev().find(|line| !line.trim().is_empty());
        format!(
            "ffmpeg could not make the video: {}",
            last.unwrap_or("it gave no reason")
        )
    };

    let mut last_key: Option<String> = None;
    let mut picture: Vec<u8> = Vec::new();
    let mut percent = 0u8;
    for frame in 0..frames {
        let at_ms = frame as f64 * 1000.0 / fps;
        // The page answers with a key for what it shows. The same key as the last frame
        // means the same picture, which is sent again without being taken again.
        let key = browser.seek(at_ms).await?;
        if last_key.as_deref() != Some(key.as_str()) {
            picture = browser.picture().await?;
            last_key = Some(key);
        }
        if pictures.write_all(&picture).await.is_err() {
            drop(pictures);
            let _ = child.wait().await;
            return Err(ffmpeg_said(said.await.unwrap_or_default()));
        }
        let now = (frame * 100 / frames) as u8;
        if now != percent {
            percent = now;
            desk.report_render(
                &source.event,
                &source.race,
                RenderState::Rendering { percent },
                None,
            );
        }
    }
    let _ = pictures.shutdown().await;
    drop(pictures);
    let status = tokio::time::timeout(FFMPEG_FINISH, child.wait())
        .await
        .map_err(|_| "ffmpeg did not finish the video.".to_owned())?
        .map_err(|e| format!("ffmpeg could not be waited for: {e}"))?;
    if !status.success() {
        return Err(ffmpeg_said(said.await.unwrap_or_default()));
    }
    Ok(())
}

// ── Names and paths ──────────────────────────────────────────────────────────

/// Where the finished video goes: beside the clean recording, under the race's name.
/// `Night race 1 - Clean.mkv` gives `Night race 1 - Overlay (redrawn).mp4`. The name is not
/// the one a recorded overlay scene has, so neither ever replaces the other.
pub fn overlaid_path(clean: &str) -> PathBuf {
    let path = Path::new(clean);
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = stem.strip_suffix(" - Clean").unwrap_or(&stem);
    path.with_file_name(format!("{name} - Overlay (redrawn).mp4"))
}

/// One part of a URL's hash, percent-encoded the way the console's router reads it back.
fn url_part(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ── Finding the two programs ─────────────────────────────────────────────────

/// A program by name on the `PATH`, as a full path.
fn on_path(name: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) && !name.to_lowercase().ends_with(".exe") {
        vec![format!("{name}.exe"), name.to_owned()]
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// What the director typed for a program: a path to it, or its name on the `PATH`.
fn as_typed(setting: &str) -> Option<String> {
    let typed = setting.trim().trim_matches('"');
    if Path::new(typed).is_file() {
        return Some(typed.to_owned());
    }
    on_path(typed).map(|found| found.to_string_lossy().into_owned())
}

/// The browser to draw with: the one named in the settings, else the first of Chrome, Edge
/// and Chromium found where each is usually installed.
pub fn find_browser(setting: &str) -> Option<String> {
    if !setting.trim().is_empty() {
        return as_typed(setting);
    }
    let mut places: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for root in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
            let Some(root) = std::env::var_os(root).map(PathBuf::from) else {
                continue;
            };
            places.push(root.join("Google/Chrome/Application/chrome.exe"));
            places.push(root.join("Microsoft/Edge/Application/msedge.exe"));
            places.push(root.join("Chromium/Application/chrome.exe"));
        }
    } else if cfg!(target_os = "macos") {
        for app in [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Chromium.app/Contents/MacOS/Chromium",
        ] {
            places.push(Path::new("/Applications").join(app));
        }
    }
    places
        .into_iter()
        .find(|place| place.is_file())
        .or_else(|| {
            [
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
                "microsoft-edge",
                "chrome",
                "msedge",
            ]
            .into_iter()
            .find_map(on_path)
        })
        .map(|found| found.to_string_lossy().into_owned())
}

/// ffmpeg: the one named in the settings, else one beside the Director, else the one Grid
/// downloaded into `tools_dir`, else one on the `PATH`.
pub fn find_ffmpeg(setting: &str, tools_dir: Option<&Path>) -> Option<String> {
    if !setting.trim().is_empty() {
        return as_typed(setting);
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(name)))
        .filter(|beside| beside.is_file())
        .or_else(|| {
            tools_dir
                .map(crate::tools::kept_ffmpeg)
                .filter(|kept| kept.is_file())
        })
        .or_else(|| on_path("ffmpeg"))
        .map(|found| found.to_string_lossy().into_owned())
}

/// Keep a program started from the desktop app from opening a console window of its own.
pub(crate) fn quiet(command: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

// ── ffmpeg ───────────────────────────────────────────────────────────────────

/// What ffmpeg says about a recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Film {
    pub seconds: f64,
    /// Frames each second, when ffmpeg says.
    pub fps: Option<f64>,
}

/// Read a recording's length and frame rate. ffmpeg prints both when it is given a file
/// and nothing to do with it.
async fn probe(ffmpeg: &str, file: &str) -> Result<Film, String> {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-i", file])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    quiet(&mut command);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .map_err(|_| "ffmpeg did not answer.".to_owned())?
        .map_err(|e| format!("ffmpeg could not be started: {e}"))?;
    parse_probe(&String::from_utf8_lossy(&output.stderr)).ok_or_else(|| {
        "ffmpeg could not read the clean recording. It may be damaged or still being written."
            .to_owned()
    })
}

/// Pick the length and frame rate out of what ffmpeg printed about a file.
pub fn parse_probe(printed: &str) -> Option<Film> {
    let clock = printed.split("Duration: ").nth(1)?.split(',').next()?;
    let mut parts = clock.trim().split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    // "Stream #0:0: Video: h264 (High), yuv420p, 1280x720, 30 fps, 30 tbr, 1k tbn"
    let fps = printed
        .lines()
        .find(|line| line.contains("Video:"))
        .and_then(|line| {
            line.split(',')
                .find_map(|part| part.trim().strip_suffix(" fps")?.parse::<f64>().ok())
        })
        .filter(|fps| *fps > 0.0);
    Some(Film {
        seconds: hours * 3600.0 + minutes * 60.0 + seconds,
        fps,
    })
}

/// sRGB's curve taken off and put back on, as ffmpeg `lutrgb` expressions over 16-bit
/// values. See [`composite_filter`].
const LIN: &str =
    "if(lt(val/65535,0.04045),val/65535/12.92,pow((val/65535+0.055)/1.055,2.4))*65535";
const ENC: &str =
    "if(lt(val/65535,0.0031308),val/65535*12.92,1.055*pow(val/65535,1/2.4)-0.055)*65535";

fn lut(expression: &str) -> String {
    format!("lutrgb=r='{expression}':g='{expression}':b='{expression}'")
}

/// The filter graph that lays the pictures over the recording.
///
/// OBS blends a browser source in linear light: the picture is taken out of sRGB, blended,
/// and put back. Through a dark see-through panel that lets clearly more of the video show
/// than a blend on the stored values does. This graph does the same sum at 16 bits
/// (`out = lin(premultiplied overlay) + lin(video) × (1 − alpha)`), so a redrawn video
/// looks like one filmed live with the overlay on. The pictures are scaled to the
/// recording's size when they differ.
pub fn composite_filter() -> String {
    [
        format!("[0:v]format=gbrp16le,{}[base]", lut(LIN)),
        "[1:v]format=rgba[o8]".to_owned(),
        "[o8][base]scale2ref=flags=bicubic[os][base2]".to_owned(),
        "[os]split[o1][o2]".to_owned(),
        format!(
            "[o1]format=gbrap16le,premultiply=inplace=1,format=gbrp16le,{}[src]",
            lut(LIN)
        ),
        "[o2]alphaextract,negate,format=gray16le,split=3[a1][a2][a3]".to_owned(),
        "[a1][a2][a3]mergeplanes=map0s=0:map0p=0:map1s=1:map1p=0:map2s=2:map2p=0:format=gbrp16le[inv]"
            .to_owned(),
        "[base2][inv]blend=all_mode=multiply:repeatlast=1[dst]".to_owned(),
        "[dst][src]blend=all_mode=addition:repeatlast=1[lin]".to_owned(),
        format!("[lin]{},format=yuv420p[v]", lut(ENC)),
    ]
    .join(";")
}

/// ffmpeg's arguments: the recording, `fps` PNG pictures a second on standard input, and
/// the finished MP4 at `out` (whatever `out` is called).
pub fn composite_args(recording: &str, out: &Path, fps: f64) -> Vec<String> {
    let mut args: Vec<String> = [
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        recording,
        "-f",
        "image2pipe",
        "-framerate",
        &fps.to_string(),
        "-i",
        "pipe:0",
        // One thread for the graph. Split across threads, a 16-bit stage of ffmpeg 7 and 8
        // leaves the row at each split black, seen as lines through the overlay's text. The
        // encoder keeps its own threads.
        "-filter_complex_threads",
        "1",
        "-filter_complex",
        &composite_filter(),
        "-map",
        "[v]",
        "-map",
        "0:a?",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "20",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-movflags",
        "+faststart",
        "-shortest",
        "-f",
        "mp4",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.push(out.to_string_lossy().into_owned());
    args
}

// ── The browser ──────────────────────────────────────────────────────────────

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A browser's DevTools connection: requests by name, one at a time.
struct Cdp {
    socket: Socket,
    next: u64,
}

impl Cdp {
    async fn connect(url: &str) -> Result<Self, String> {
        let (socket, _) =
            tokio::time::timeout(BROWSER_ANSWER, tokio_tungstenite::connect_async(url))
                .await
                .map_err(|_| "The browser did not answer.".to_owned())?
                .map_err(|e| format!("The browser could not be reached: {e}"))?;
        Ok(Self { socket, next: 0 })
    }

    /// Make one request and wait for its answer. `session` names the page it is for; none
    /// means the browser itself.
    async fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.next += 1;
        let mut request = json!({ "id": self.next, "method": method, "params": params });
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        let gone = || "The browser closed.".to_owned();
        self.socket
            .send(Message::text(request.to_string()))
            .await
            .map_err(|_| gone())?;
        let id = self.next;
        tokio::time::timeout(BROWSER_ANSWER, async {
            loop {
                let message = self
                    .socket
                    .next()
                    .await
                    .ok_or_else(gone)?
                    .map_err(|_| gone())?;
                let Message::Text(text) = message else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                // Anything else is an event, or the answer to a request given up on.
                if value["id"].as_u64() != Some(id) {
                    continue;
                }
                return match value.get("error") {
                    Some(error) => Err(format!(
                        "The browser refused {method}: {}",
                        error["message"].as_str().unwrap_or("no reason given")
                    )),
                    None => Ok(value["result"].clone()),
                };
            }
        })
        .await
        .map_err(|_| format!("The browser did not answer {method} in time."))?
    }
}

/// A browser with no window, started for one render, with one page open.
struct Browser {
    child: Child,
    cdp: Cdp,
    /// The page's session, once one is open.
    page: Option<String>,
    /// The browser's own folder for this run, removed afterwards.
    profile: PathBuf,
}

impl Browser {
    async fn launch(path: &str) -> Result<Self, String> {
        let profile = std::env::temp_dir().join(format!(
            "gridfpv-render-{}-{}",
            std::process::id(),
            gridfpv_server::app::now_micros()
        ));
        let mut command = Command::new(path);
        command
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-extensions",
                "--hide-scrollbars",
                "--mute-audio",
                "--force-device-scale-factor=1",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        quiet(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| format!("The browser could not be started: {e}"))?;
        // The browser prints where its DevTools connection is as it starts.
        let mut lines = BufReader::new(child.stderr.take().expect("stderr is piped")).lines();
        let found = tokio::time::timeout(BROWSER_START, async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(url) = line.split("DevTools listening on ").nth(1) {
                    return Some(url.trim().to_owned());
                }
            }
            None
        })
        .await
        .ok()
        .flatten();
        let Some(url) = found else {
            let _ = child.kill().await;
            let _ = std::fs::remove_dir_all(&profile);
            return Err("The browser did not start. Check its path on the Video page.".into());
        };
        // Keep reading what it prints, so it never waits on a full pipe.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        let cdp = match Cdp::connect(&url).await {
            Ok(cdp) => cdp,
            Err(e) => {
                let _ = child.kill().await;
                let _ = std::fs::remove_dir_all(&profile);
                return Err(e);
            }
        };
        Ok(Self {
            child,
            cdp,
            page: None,
            profile,
        })
    }

    /// Open the replay page at 1920 by 1080 on a see-through background, and wait for it to
    /// have its race.
    async fn open(&mut self, url: &str) -> Result<(), String> {
        let target = self
            .cdp
            .call(None, "Target.createTarget", json!({ "url": "about:blank" }))
            .await?;
        let attached = self
            .cdp
            .call(
                None,
                "Target.attachToTarget",
                json!({ "targetId": target["targetId"], "flatten": true }),
            )
            .await?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or("The browser did not open a page.")?
            .to_owned();
        self.page = Some(session.clone());
        let page = Some(session.as_str());
        self.cdp
            .call(
                page,
                "Emulation.setDeviceMetricsOverride",
                json!({ "width": 1920, "height": 1080, "deviceScaleFactor": 1, "mobile": false }),
            )
            .await?;
        self.cdp
            .call(
                page,
                "Emulation.setDefaultBackgroundColorOverride",
                json!({ "color": { "r": 0, "g": 0, "b": 0, "a": 0 } }),
            )
            .await?;
        self.cdp
            .call(page, "Page.navigate", json!({ "url": url }))
            .await?;
        // The page puts its handle on the window once it is running.
        let deadline = tokio::time::Instant::now() + BROWSER_START;
        loop {
            let there = self
                .evaluate("typeof window.gridfpvReplay === 'object'")
                .await
                .unwrap_or(Value::Bool(false));
            if there == true {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(
                    "The replay page did not load. Check that this machine can open the console."
                        .into(),
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // `ready()` answers with an empty string, or with what is wrong.
        let problem = self.evaluate("window.gridfpvReplay.ready()").await?;
        match problem.as_str() {
            Some("") => Ok(()),
            Some(problem) => Err(problem.to_owned()),
            None => Err("The replay page did not say it was ready.".into()),
        }
    }

    async fn evaluate(&mut self, expression: &str) -> Result<Value, String> {
        let page = self.page.clone();
        let answer = self
            .cdp
            .call(
                page.as_deref(),
                "Runtime.evaluate",
                json!({ "expression": expression, "awaitPromise": true, "returnByValue": true }),
            )
            .await?;
        if let Some(thrown) = answer.get("exceptionDetails") {
            return Err(format!(
                "The replay page failed: {}",
                thrown["exception"]["description"]
                    .as_str()
                    .or(thrown["text"].as_str())
                    .unwrap_or("no reason given")
            ));
        }
        Ok(answer["result"]["value"].clone())
    }

    /// Show the overlay as it stood `at_ms` into the recording. The answer is a key for what
    /// is on the page: the same key means the same picture.
    async fn seek(&mut self, at_ms: f64) -> Result<String, String> {
        let key = self
            .evaluate(&format!("window.gridfpvReplay.seek({at_ms})"))
            .await?;
        key.as_str()
            .map(str::to_owned)
            .ok_or_else(|| "The replay page did not answer.".to_owned())
    }

    /// The page as a see-through PNG.
    async fn picture(&mut self) -> Result<Vec<u8>, String> {
        let page = self.page.clone();
        let shot = self
            .cdp
            .call(
                page.as_deref(),
                "Page.captureScreenshot",
                json!({ "format": "png", "optimizeForSpeed": true }),
            )
            .await?;
        base64::engine::general_purpose::STANDARD
            .decode(shot["data"].as_str().unwrap_or_default())
            .map_err(|_| "The browser sent a picture that could not be read.".to_owned())
    }

    /// Ask the browser to close, make sure it has, and remove its folder.
    async fn close(mut self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            self.cdp.call(None, "Browser.close", json!({})),
        )
        .await;
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_finished_video_is_named_for_the_race_beside_the_clean_one() {
        assert_eq!(
            overlaid_path("/rec/Night race 1 - Clean.mkv"),
            Path::new("/rec/Night race 1 - Overlay (redrawn).mp4")
        );
        // A clean recording that was not named by Grid still gets a name of its own.
        assert_eq!(
            overlaid_path("/rec/camera.mkv"),
            Path::new("/rec/camera - Overlay (redrawn).mp4")
        );
    }

    #[test]
    fn ids_are_written_into_the_url_the_way_the_router_reads_them() {
        assert_eq!(url_part("heat#1"), "heat%231");
        assert_eq!(url_part("a/b c"), "a%2Fb%20c");
        assert_eq!(url_part("Plain-id_1.2~"), "Plain-id_1.2~");
    }

    #[test]
    fn the_length_and_frame_rate_are_read_from_what_ffmpeg_prints() {
        let printed = "Input #0, matroska,webm, from 'a.mkv':\n  Metadata:\n    ENCODER         : Lavf61.7.100\n  Duration: 00:01:05.50, start: 0.000000, bitrate: 2905 kb/s\n  Stream #0:0: Video: h264 (High), yuv420p(tv, bt709, progressive), 1280x720 [SAR 1:1 DAR 16:9], 30 fps, 30 tbr, 1k tbn\n  Stream #0:1: Audio: aac (LC), 48000 Hz, stereo, fltp\nAt least one output file must be specified\n";
        assert_eq!(
            parse_probe(printed),
            Some(Film {
                seconds: 65.5,
                fps: Some(30.0)
            })
        );
        let fractional = printed.replace("30 fps", "59.94 fps");
        assert_eq!(parse_probe(&fractional).unwrap().fps, Some(59.94));
        // A file still being written has no length yet.
        assert_eq!(parse_probe(&printed.replace("00:01:05.50", "N/A")), None);
        assert_eq!(parse_probe("a.mkv: No such file or directory"), None);
    }

    #[test]
    fn ffmpeg_is_given_the_recording_the_pictures_and_one_graph_thread() {
        let args = composite_args("/rec/a - Clean.mkv", Path::new("/rec/a.mp4.part"), 30.0);
        let at = |flag: &str| {
            let index = args.iter().position(|arg| arg == flag).unwrap();
            args[index + 1].as_str()
        };
        assert_eq!(at("-framerate"), "30");
        assert_eq!(at("-filter_complex_threads"), "1");
        assert_eq!(at("-filter_complex"), composite_filter());
        // The name ends in `.part`, so the kind of file is said outright.
        assert_eq!(at("-f"), "image2pipe");
        assert_eq!(&args[args.len() - 3..], ["-f", "mp4", "/rec/a.mp4.part"]);
        assert_eq!(args.iter().filter(|arg| *arg == "-i").count(), 2);
    }

    #[test]
    fn a_program_that_is_not_there_is_not_found() {
        assert_eq!(find_browser("/nowhere/at/all/chrome"), None);
        assert_eq!(find_ffmpeg("/nowhere/at/all/ffmpeg", None), None);
    }
}
