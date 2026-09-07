//! `cargo xtask vd-mock` — the **interactive Velocidrone mock harness** (#484).
//!
//! Velocidrone is closed source and cannot be containerized, so unlike `rh-mock` there is
//! no real server to feed: [`gridfpv_testkit::vd_mock`] *is* the server — a wire-faithful
//! reimplementation of the game's WebSocket interface, decompiled from VelociDrone
//! 1.17.13 (binary frames, string scalars, host gating, newest-connection-wins; the full
//! receipts live in the RE workspace's `ws-spec.md`). This command runs it on a real
//! port so the maintainer can point a Director (or any consumer) at it:
//!
//! - **`cargo xtask vd-mock feed [scenario] [--port P] [--bind ADDR] [--speed X]`** —
//!   serve a scenario and stay running, printing every command a client sends (with its
//!   authorization outcome) until Ctrl-C. Command-driven scenarios (e.g. `heat`) wait
//!   for `startrace` exactly like the real game; replay/autostart scenarios begin on the
//!   first connection.
//! - **`cargo xtask vd-mock list`** — print the scenario menu.
//!
//! Defaults mirror the game: port **60003**, service path `/velocidrone`. `--bind
//! 0.0.0.0` accepts connections from another machine (the bench laptop); the real game
//! binds its LAN IP, so a consumer that works against `--bind 0.0.0.0` here must still
//! be pointed at the LAN IP for the real thing.

use std::time::Duration;

use gridfpv_testkit::vd_mock::{VdMock, scenarios};

/// Dispatch. `args` is everything after `vd-mock` on the command line.
pub fn run(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("feed") => feed(&args[1..]),
        Some("list") | None => {
            print_menu();
            true
        }
        Some(other) => {
            eprintln!("unknown vd-mock subcommand: {other}");
            usage();
            false
        }
    }
}

fn usage() {
    eprintln!(
        "\nusage: cargo xtask vd-mock <feed|list>\n\
         \n\
         feed [scenario] [--port P] [--bind ADDR] [--speed X]\n\
         \x20                       serve the wire-faithful Velocidrone mock (default\n\
         \x20                       scenario: heat; port 60003; bind 127.0.0.1;\n\
         \x20                       --speed 2 runs the race twice as fast)\n\
         list                      show the scenario menu\n"
    );
}

fn print_menu() {
    println!("vd-mock scenarios (cargo xtask vd-mock feed <name>):\n");
    for s in scenarios() {
        println!("  {:<14} {}", s.name, s.blurb);
    }
    println!(
        "\nThe mock serves ws://<bind>:<port>/velocidrone with the game's exact wire behavior."
    );
}

fn feed(args: &[String]) -> bool {
    let mut scenario_name = "heat".to_string();
    let mut port: u16 = 60003;
    let mut bind = "127.0.0.1".to_string();
    let mut speed: f64 = 1.0;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--port" => match it.next().and_then(|v| v.parse().ok()) {
                Some(p) => port = p,
                None => {
                    eprintln!("--port needs a number");
                    return false;
                }
            },
            "--bind" => match it.next() {
                Some(a) => bind = a.clone(),
                None => {
                    eprintln!("--bind needs an address");
                    return false;
                }
            },
            "--speed" => match it.next().and_then(|v| v.parse().ok()) {
                Some(x) if x > 0.0 => speed = x,
                _ => {
                    eprintln!("--speed needs a positive number");
                    return false;
                }
            },
            name if !name.starts_with("--") => scenario_name = name.to_string(),
            other => {
                eprintln!("unknown flag: {other}");
                usage();
                return false;
            }
        }
    }

    let Some(scenario) = scenarios().into_iter().find(|s| s.name == scenario_name) else {
        eprintln!("unknown scenario: {scenario_name}");
        print_menu();
        return false;
    };

    let mut cfg = (scenario.build)();
    cfg.bind = bind;
    cfg.port = port;
    // `--speed X` runs X times faster: the config's scale multiplies delays.
    cfg.time_scale /= speed;

    let mock = VdMock::start(cfg);
    println!(
        "vd-mock: serving scenario '{}' at {}",
        scenario.name,
        mock.url()
    );
    println!("         {}", scenario.blurb);
    println!("vd-mock: Ctrl-C to stop; incoming commands are printed below.\n");

    // Tail the command log until Ctrl-C.
    let mut seen = 0usize;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let commands = mock.commands();
        for cmd in &commands[seen..] {
            let gate = if cmd.authorized {
                "ok"
            } else {
                "DROPPED (not authorized)"
            };
            println!("vd-mock: <- {} [{}]  {}", cmd.command, gate, cmd.raw);
        }
        seen = commands.len();
    }
}
