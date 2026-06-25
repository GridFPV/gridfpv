//! `cargo xtask rh-mock` — the **interactive RotorHazard mock-signal harness** for marshaling
//! testing.
//!
//! The `mock_data` mechanism (emulated node-output CSVs → a *real* dockerized RotorHazard that
//! detects + records laps through its genuine pipeline) previously lived only inside the
//! `#[ignore]`d `rh_signal` e2e test. This promotes it to a runnable tool the maintainer drives by
//! hand:
//!
//! - **`cargo xtask rh-mock feed [scenario] [--port P] [--tick T]`** — generate a chosen mock
//!   signal stream (reusing the [`gridfpv_testkit`] scenario library), feed it to a fresh
//!   disposable RotorHazard container, and **stay running** while the maintainer points a Director
//!   at the timer and runs a heat. The container is removed on Ctrl-C (RAII).
//! - **`cargo xtask rh-mock dump <director-base-url> <event> <heat>`** — after a heat has run, hit
//!   the Director's `?projection=signal` endpoint and **pretty-print the captured RSSI samples +
//!   enter/exit thresholds** for every competitor (plus the lap list), so the maintainer sees
//!   exactly what signal values flowed end-to-end. Read-only — it never touches the adapter or the
//!   projection code.
//! - **`cargo xtask rh-mock list`** — print the scenario menu.
//!
//! This module only generates CSVs ([`gridfpv_testkit`]), drives Docker through the testkit's
//! [`RhContainer`], and parses the *served* signal JSON. It modifies **no** adapter / projection
//! code; the dump is a plain HTTP read of the existing `?projection=signal` route.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use gridfpv_testkit::{NodeCsv, NodePlan, RhContainer, node_csv, scenarios};

/// Default host port for the harness-spun RotorHazard (distinct from the persistent dev rigs:
/// `gridfpv-rh` :5000, `gridfpv-demo-rh` :5099, and the `*_live` test ports 5031/5040-5042).
const DEFAULT_PORT: u16 = 5055;
/// Default CSV tick interval (seconds). A brisk pace so passes land quickly while a heat runs.
const DEFAULT_TICK: &str = "0.1";

/// The dispatch entry point. `args` is everything after `rh-mock` on the command line.
pub fn run(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("feed") => feed(&args[1..]),
        Some("dump") => dump(&args[1..]),
        Some("list") | None => {
            print_menu();
            true
        }
        Some(other) => {
            eprintln!("unknown rh-mock subcommand: {other}");
            usage();
            false
        }
    }
}

fn usage() {
    eprintln!(
        "\nusage: cargo xtask rh-mock <feed|dump|list>\n\
         \n\
         feed [scenario] [--port P] [--tick T]   spin up a real RotorHazard fed a mock signal\n\
         dump <director-url> <event> <heat>       print the captured ?projection=signal values\n\
         list                                     show the scenario menu\n\
         \n\
         Run `cargo xtask rh-mock list` for the scenarios."
    );
}

// ---------------------------------------------------------------------------------------------
// Scenario menu — maps a CLI name onto a testkit CSV set (one or more `(node_index, csv)` pairs).
// ---------------------------------------------------------------------------------------------

/// One selectable scenario: a name, a one-line description, and a builder that yields the per-node
/// `(node_index, csv)` list the [`RhContainer`] mounts.
struct Scenario {
    name: &'static str,
    blurb: &'static str,
    build: fn() -> Vec<(usize, String)>,
}

/// The scenario menu. Each entry reuses the [`gridfpv_testkit`] generators so the harness drives
/// exactly the same emulated streams the e2e suite asserts on.
fn menu() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "clean",
            blurb: "one node, clean metronomic laps, strong signal (the happy path)",
            build: || vec![(0, node_csv(&NodeCsv::default()))],
        },
        Scenario {
            name: "missed-lap",
            blurb: "one node that SKIPS a crossing (gate miss) — 3 laps, a long dead gap, 3 more",
            build: || vec![(0, plan(scenarios::missed_crossing(3, 3, 8, 4)))],
        },
        Scenario {
            name: "false-pass",
            blurb: "one node with a fast double-crossing (a false/extra pass) among normal laps",
            build: || {
                // Two crossings only ~2 ticks apart simulate a bounce/double-trigger: RH may record
                // a too-fast extra lap the marshal must void. Normal pace is 12 ticks.
                let plan = scenarios::varied_pace(&[12, 12, 2, 12, 12]);
                vec![(0, gridfpv_testkit::plan_csv(&plan))]
            },
        },
        Scenario {
            name: "noisy",
            blurb: "one node at a MARGINAL peak (just over the enter threshold) — dirty RSSI",
            build: || vec![(0, plan(scenarios::marginal(6, 10)))],
        },
        Scenario {
            name: "strong-vs-weak",
            blurb: "two nodes, one strong + one weak signal — compare cached RSSI context",
            build: || gridfpv_testkit::race(&[scenarios::strong(6, 8), scenarios::weak(6, 12)]),
        },
        Scenario {
            name: "dnf",
            blurb: "two nodes — node 0 flies on, node 1 DNFs after 2 laps (per-node independence)",
            build: || gridfpv_testkit::race(&[scenarios::uniform(8, 8), scenarios::dnf(2, 8)]),
        },
        Scenario {
            name: "pack",
            blurb: "three nodes crossing the gate SIMULTANEOUSLY — the dedup/ordering stress case",
            build: || gridfpv_testkit::simultaneous(3, 6, 12),
        },
    ]
}

/// Render a single-node [`NodePlan`] to its CSV (thin wrapper so the menu reads cleanly).
fn plan(p: NodePlan) -> String {
    gridfpv_testkit::plan_csv(&p)
}

fn print_menu() {
    println!("\n\x1b[1mRH mock-signal scenarios\x1b[0m (cargo xtask rh-mock feed <name>):\n");
    for s in menu() {
        println!("  \x1b[1m{:<16}\x1b[0m {}", s.name, s.blurb);
    }
    println!(
        "\nDefault is `clean`. Each feeds a REAL dockerized RotorHazard the chosen emulated\n\
         node-output stream, so RH detects + records laps through its genuine pipeline."
    );
}

// ---------------------------------------------------------------------------------------------
// feed: spin up RH with the chosen scenario and keep it alive for manual heat-running.
// ---------------------------------------------------------------------------------------------

fn feed(args: &[String]) -> bool {
    let mut scenario_name = "clean".to_string();
    let mut port = DEFAULT_PORT;
    let mut tick = DEFAULT_TICK.to_string();

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => match it.next().and_then(|v| v.parse::<u16>().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("--port needs a number");
                    return false;
                }
            },
            "--tick" => match it.next() {
                Some(t) => tick = t.clone(),
                None => {
                    eprintln!("--tick needs a value (seconds, e.g. 0.1)");
                    return false;
                }
            },
            other if other.starts_with("--") => {
                eprintln!("unknown flag: {other}");
                return false;
            }
            name => scenario_name = name.to_string(),
        }
    }

    let menu = menu();
    let Some(scenario) = menu.iter().find(|s| s.name == scenario_name) else {
        eprintln!("unknown scenario: {scenario_name}");
        print_menu();
        return false;
    };

    if !docker_present() {
        eprintln!(
            "\x1b[31mdocker not found on PATH.\x1b[0m The harness drives a REAL dockerized \
             RotorHazard, so Docker must be installed and running."
        );
        return false;
    }

    let csvs = (scenario.build)();
    let node_count = csvs.len();
    println!(
        "\n\x1b[1mStarting RotorHazard\x1b[0m with scenario \x1b[1m{}\x1b[0m on port {port} \
         ({node_count} emulated node(s), tick={tick}s)...",
        scenario.name
    );
    println!("  {}", scenario.blurb);

    // RAII: removed on drop (Ctrl-C below drops it). Blocks until the HTTP port is up.
    let rh = RhContainer::start(port, &tick, &csvs);
    let url = rh.url().to_string();

    println!(
        "\n\x1b[32mRotorHazard is up at {url}\x1b[0m (container `{}`).\n",
        rh.name()
    );
    print_run_instructions(&url, node_count);

    // Stay alive so the maintainer can run a heat against the live timer. The container is
    // RAII-owned by `rh`: dropping it (a clean exit, or `Drop` on unwind) runs `docker rm -f`.
    // On Ctrl-C the process is killed before `Drop` runs, so we print the exact cleanup command;
    // `RhContainer::start` also `docker rm -f`s any leftover of the same name on the next run.
    let container = rh.name().to_string();
    println!(
        "\n\x1b[2mLeaving RotorHazard running. Press Ctrl-C here to stop the harness; then remove \
         the container with:\x1b[0m\n  docker rm -f {container}"
    );
    // Park forever; the maintainer ends the session with Ctrl-C.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Print exactly what the maintainer does next: point a Director at this RH timer and run a heat.
fn print_run_instructions(url: &str, node_count: usize) {
    println!("\x1b[1mNext steps (run a heat against this timer):\x1b[0m");
    println!(
        "  1. Start a Director: \x1b[1mcargo run -p gridfpv-app\x1b[0m (serves on its own port, \
         usually http://localhost:3000)."
    );
    println!(
        "  2. In the console, add a RotorHazard timer pointing at \x1b[1m{url}\x1b[0m and select \
         it for your event."
    );
    println!(
        "  3. Build a heat with {node_count} seat(s) (node-0..node-{}), stage it, and start the \
         race. The emulated CSV drives the crossings — no manual lap injection.",
        node_count.saturating_sub(1)
    );
    println!(
        "  4. Let it run a few seconds (laps record as the mock signal crosses), then finish the \
         heat."
    );
    println!(
        "  5. \x1b[1mSee the captured values\x1b[0m:\n     \
         cargo xtask rh-mock dump <director-url> <event-id> <heat-id>\n     \
         e.g. cargo xtask rh-mock dump http://localhost:3000 practice q-1"
    );
    println!(
        "\n  \x1b[2mTip: RH's default 10s lap minimum can swallow the brisk mock laps. The \
         Director/adapter set it to 0 for mock runs; if you drive RH directly, set MIN_LAP_TIME=0.\x1b[0m"
    );
}

// ---------------------------------------------------------------------------------------------
// dump: read ?projection=signal (and ?projection=laps) and pretty-print the captured values.
// ---------------------------------------------------------------------------------------------

fn dump(args: &[String]) -> bool {
    let [base, event, heat] = args else {
        eprintln!(
            "usage: cargo xtask rh-mock dump <director-base-url> <event-id> <heat-id>\n\
             e.g.   cargo xtask rh-mock dump http://localhost:3000 practice q-1"
        );
        return false;
    };
    let base = base.trim_end_matches('/');

    // --- signal trace ---
    let signal_path = format!("/events/{event}/snapshot/heat/{heat}?projection=signal");
    let signal = match http_get_json(base, &signal_path) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("\x1b[31mfailed to fetch signal projection:\x1b[0m {err}");
            eprintln!("  GET {base}{signal_path}");
            return false;
        }
    };
    print_signal(&signal);

    // --- lap list (best-effort; the signal dump is the primary view) ---
    let laps_path = format!("/events/{event}/snapshot/heat/{heat}?projection=laps");
    match http_get_json(base, &laps_path) {
        Ok(v) => print_laps(&v),
        Err(err) => eprintln!("(lap list unavailable: {err})"),
    }
    true
}

/// One competitor's signal trace, extracted from the served `?projection=signal` JSON. The pure
/// extraction lives here (rather than inline in the printer) so the load-bearing JSON pointers —
/// the contract with the server's `SignalTrace` body — are unit-tested without Docker or a server.
struct TraceDump {
    adapter: String,
    competitor: String,
    from: Option<i64>,
    period_micros: i64,
    enter: Option<i64>,
    exit: Option<i64>,
    samples: Vec<i64>,
}

/// Parse the `SignalTrace` body of a snapshot into per-competitor [`TraceDump`]s. Returns `None`
/// when the body is some other projection (so the caller can report the actual kind).
fn parse_signal(snapshot: &serde_json::Value) -> Option<Vec<TraceDump>> {
    let view = snapshot.pointer("/body/SignalTrace")?;
    let competitors = view.get("competitors").and_then(|c| c.as_array());
    Some(
        competitors
            .map(|arr| {
                arr.iter()
                    .map(|c| TraceDump {
                        adapter: c
                            .pointer("/competitor/adapter")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        competitor: c
                            .pointer("/competitor/competitor")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        from: c.get("from").and_then(|v| v.as_i64()),
                        period_micros: c.get("period_micros").and_then(|v| v.as_i64()).unwrap_or(0),
                        enter: c.get("enter").and_then(|v| v.as_i64()),
                        exit: c.get("exit").and_then(|v| v.as_i64()),
                        samples: c
                            .get("samples")
                            .and_then(|v| v.as_array())
                            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Pretty-print the `SignalTrace` body: per-competitor samples + thresholds + simple stats.
fn print_signal(snapshot: &serde_json::Value) {
    println!("\n\x1b[1m=== Captured signal trace (?projection=signal) ===\x1b[0m");
    let Some(traces) = parse_signal(snapshot) else {
        eprintln!("  (no SignalTrace body — got: {})", body_kind(snapshot));
        return;
    };
    if traces.is_empty() {
        println!("  (no competitors captured any signal — did a heat run and finish?)");
        return;
    }
    for t in &traces {
        let TraceDump {
            adapter,
            competitor: key,
            from,
            period_micros: period,
            enter,
            exit,
            samples,
        } = t;
        let (from, enter, exit) = (*from, *enter, *exit);

        println!("\n  \x1b[1m{adapter} / {key}\x1b[0m");
        println!("    thresholds: enter={} exit={}", opt(enter), opt(exit));
        println!(
            "    time base : from={}µs  period={}µs/sample  ({} samples)",
            opt(from),
            period,
            samples.len()
        );
        if let (Some(&min), Some(&max)) = (samples.iter().min(), samples.iter().max()) {
            let sum: i64 = samples.iter().sum();
            let mean = sum / samples.len().max(1) as i64;
            println!("    rssi      : min={min} max={max} mean≈{mean}");
        }
        print_sparkline(samples, enter);
        // Raw values, wrapped, so the maintainer sees EXACTLY what flowed.
        print!("    samples   : ");
        for (i, s) in samples.iter().enumerate() {
            if i > 0 && i % 20 == 0 {
                print!("\n                ");
            }
            print!("{s} ");
        }
        println!();
    }
}

/// A compact ASCII sparkline of the samples, with the enter threshold marked, so peaks
/// (crossings) are visible at a glance.
fn print_sparkline(samples: &[i64], enter: Option<i64>) {
    if samples.is_empty() {
        return;
    }
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let min = *samples.iter().min().unwrap();
    let max = *samples.iter().max().unwrap();
    let span = (max - min).max(1);
    let line: String = samples
        .iter()
        .map(|&s| {
            let idx = ((s - min) * (BARS.len() as i64 - 1) / span) as usize;
            BARS[idx]
        })
        .collect();
    println!("    trace     : {line}");
    if let Some(e) = enter {
        let marker = if e >= min && e <= max {
            "(enter threshold falls within the captured range — crossings clear it)"
        } else if e > max {
            "(enter threshold ABOVE every sample — nothing would have crossed!)"
        } else {
            "(enter threshold BELOW the baseline — every sample clears it)"
        };
        println!("    \x1b[2m{marker}\x1b[0m");
    }
}

/// One competitor's recorded laps, extracted from the served `?projection=laps` JSON.
struct LapsDump {
    competitor: String,
    /// `(lap_number, duration_micros)` in recorded order.
    laps: Vec<(i64, i64)>,
}

/// Parse the `LapList` body of a snapshot into per-competitor [`LapsDump`]s, or `None` for a
/// non-lap-list body. The competitor ref nests one level deeper than in the signal body
/// (`LapList`'s `competitor` is a `CompetitorKey` object, whose `.competitor` is the ref string).
fn parse_laps(snapshot: &serde_json::Value) -> Option<Vec<LapsDump>> {
    let list = snapshot.pointer("/body/LapList")?;
    let competitors = list.get("competitors").and_then(|c| c.as_array());
    Some(
        competitors
            .map(|arr| {
                arr.iter()
                    .map(|c| LapsDump {
                        competitor: c
                            .pointer("/competitor/competitor")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        laps: c
                            .get("laps")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .map(|l| {
                                        (
                                            l.get("number").and_then(|v| v.as_i64()).unwrap_or(0),
                                            l.get("duration_micros")
                                                .and_then(|v| v.as_i64())
                                                .unwrap_or(0),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Pretty-print the `LapList` body: per-competitor recorded laps.
fn print_laps(snapshot: &serde_json::Value) {
    println!("\n\x1b[1m=== Recorded laps (?projection=laps) ===\x1b[0m");
    let Some(competitors) = parse_laps(snapshot) else {
        eprintln!("  (no LapList body — got: {})", body_kind(snapshot));
        return;
    };
    if competitors.is_empty() {
        println!("  (no laps recorded)");
        return;
    }
    for c in &competitors {
        let times: Vec<String> = c
            .laps
            .iter()
            .map(|(num, micros)| format!("#{num}={:.3}s", *micros as f64 / 1_000_000.0))
            .collect();
        println!(
            "  \x1b[1m{}\x1b[0m: {} lap(s)  {}",
            c.competitor,
            c.laps.len(),
            times.join("  ")
        );
    }
}

/// Name the projection body variant present (for clearer errors when it isn't what we expect).
fn body_kind(snapshot: &serde_json::Value) -> String {
    snapshot
        .get("body")
        .and_then(|b| b.as_object())
        .and_then(|o| o.keys().next().cloned())
        .unwrap_or_else(|| "<none>".to_string())
}

fn opt(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "—".to_string())
}

// ---------------------------------------------------------------------------------------------
// Minimal std-only HTTP GET (no async stack). The projection JSON is small; this is plenty.
// ---------------------------------------------------------------------------------------------

/// Issue a blocking `GET base+path` and parse the body as JSON. Supports `http://host:port` only
/// (the Director is loopback in this harness) — no TLS, no redirects, no chunked encoding.
fn http_get_json(base: &str, path: &str) -> Result<serde_json::Value, String> {
    let rest = base
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// base URLs are supported, got `{base}`"))?;
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| format!("bad port in `{base}`"))?,
        ),
        None => (rest, 80u16),
    };
    let host = host.trim_end_matches('/');

    let mut stream =
        TcpStream::connect((host, port)).map_err(|e| format!("connect {host}:{port}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write request: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read response: {e}"))?;
    let text = String::from_utf8_lossy(&raw);

    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response (no header/body split)".to_string())?;
    let status_line = head.lines().next().unwrap_or_default();
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    if code != 200 {
        return Err(format!("HTTP {status_line} — body: {}", body.trim()));
    }
    serde_json::from_str(body.trim())
        .map_err(|e| format!("parse JSON: {e} — body: {}", body.trim()))
}

// ---------------------------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------------------------

fn docker_present() -> bool {
    std::process::Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Tests — on the JSON-pointer extraction (the contract with the served projection bodies) and the
// scenario menu. No Docker, no server: the fixtures mirror exactly what the server serializes
// (externally-tagged `body`; `AdapterId`/`CompetitorRef`/`SourceTime` are `#[serde(transparent)]`
// so they wire as bare scalars). These run in the core `cargo xtask ci` suite.
// ---------------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `?projection=signal` snapshot exactly as the server's externally-tagged `Snapshot`
    /// serializes it: `{cursor, body: {SignalTrace: {competitors: [...]}}}`.
    fn signal_snapshot() -> serde_json::Value {
        json!({
            "cursor": 12,
            "body": {
                "SignalTrace": {
                    "competitors": [
                        {
                            "competitor": { "adapter": "rotorhazard", "competitor": "node-0" },
                            "from": 0,
                            "period_micros": 100_000,
                            "samples": [70, 70, 150, 148, 71, 70],
                            "enter": 90,
                            "exit": 80
                        }
                    ]
                }
            }
        })
    }

    #[test]
    fn parses_signal_trace_fields() {
        let traces = parse_signal(&signal_snapshot()).expect("signal body");
        assert_eq!(traces.len(), 1);
        let t = &traces[0];
        assert_eq!(t.adapter, "rotorhazard");
        assert_eq!(t.competitor, "node-0");
        assert_eq!(t.from, Some(0));
        assert_eq!(t.period_micros, 100_000);
        assert_eq!(t.enter, Some(90));
        assert_eq!(t.exit, Some(80));
        assert_eq!(t.samples, vec![70, 70, 150, 148, 71, 70]);
    }

    #[test]
    fn parse_signal_rejects_other_bodies() {
        let laps = json!({ "cursor": 1, "body": { "LapList": { "competitors": [] } } });
        assert!(parse_signal(&laps).is_none());
        assert_eq!(body_kind(&laps), "LapList");
    }

    #[test]
    fn parses_lap_list_with_nested_competitor_key() {
        // `LapList`'s competitor is a `CompetitorKey` object (adapter + ref), one level deeper than
        // the signal body's bare ref.
        let snap = json!({
            "cursor": 3,
            "body": {
                "LapList": {
                    "competitors": [
                        {
                            "competitor": { "adapter": "rotorhazard", "competitor": "node-0" },
                            "laps": [
                                { "number": 1, "duration_micros": 1_500_000 },
                                { "number": 2, "duration_micros": 1_480_000 }
                            ]
                        }
                    ]
                }
            }
        });
        let laps = parse_laps(&snap).expect("lap body");
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].competitor, "node-0");
        assert_eq!(laps[0].laps, vec![(1, 1_500_000), (2, 1_480_000)]);
    }

    #[test]
    fn empty_signal_view_yields_no_traces() {
        let snap = json!({ "cursor": 0, "body": { "SignalTrace": { "competitors": [] } } });
        assert_eq!(parse_signal(&snap).expect("body").len(), 0);
    }

    #[test]
    fn menu_names_are_unique_and_build() {
        let menu = menu();
        let mut names: Vec<&str> = menu.iter().map(|s| s.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "scenario names must be unique");
        // Every builder produces at least one non-empty node CSV.
        for s in &menu {
            let csvs = (s.build)();
            assert!(!csvs.is_empty(), "scenario {} built no nodes", s.name);
            for (_, csv) in &csvs {
                assert!(!csv.is_empty(), "scenario {} produced an empty CSV", s.name);
            }
        }
    }

    #[test]
    fn http_get_rejects_non_http_base() {
        assert!(http_get_json("https://example.com", "/x").is_err());
    }
}
