//! `gridfpv-server`: the Grid remote server (docs/primitives.md §13).
//!
//! ```text
//! gridfpv-server                       run the server
//! gridfpv-server token new <label>     make a link token for a Director, and show it once
//! gridfpv-server token list            the tokens that were made
//! gridfpv-server token revoke <id>     take a token back
//! gridfpv-server club name <name>      name the club
//! ```
//!
//! Set from the environment:
//!
//! - `GRIDFPV_SERVER_DATA_DIR`: where everything is kept. Default `./gridfpv-server-data`.
//! - `GRIDFPV_SERVER_ADDR`: the address to listen on. Default `127.0.0.1:8470`.
//! - `GRIDFPV_SERVER_ASSETS`: the folder with the web page (the built console). Default
//!   `frontend/apps/rd-console/dist`. With no page there, the server still takes pushes and
//!   answers the API.
//!
//! The server speaks plain HTTP and listens on this computer only by default. It is meant
//! to sit behind a web server that gives it HTTPS (§13.5).
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use gridfpv_serverd::app::{AppState, router_with_pages};
use gridfpv_serverd::book::Book;

const DEFAULT_ADDR: &str = "127.0.0.1:8470";
const DEFAULT_DATA_DIR: &str = "gridfpv-server-data";
const DEFAULT_ASSETS: &str = "frontend/apps/rd-console/dist";

const USAGE: &str = "usage:
  gridfpv-server                       run the server
  gridfpv-server token new <label>     make a link token for a Director
  gridfpv-server token list            list the tokens
  gridfpv-server token revoke <id>     take a token back
  gridfpv-server club name <name>      name the club";

fn data_dir() -> PathBuf {
    std::env::var_os("GRIDFPV_SERVER_DATA_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let done = match args.as_slice() {
        [] | ["serve"] => serve(),
        ["token", "new", label] => token_new(label),
        ["token", "list"] => token_list(),
        ["token", "revoke", id] => token_revoke(id),
        ["club", "name", name] => club_name(name),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("gridfpv-server: {why}");
            ExitCode::FAILURE
        }
    }
}

type Done = Result<(), Box<dyn std::error::Error>>;

fn serve() -> Done {
    let addr: SocketAddr = std::env::var("GRIDFPV_SERVER_ADDR")
        .ok()
        .filter(|addr| !addr.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ADDR.to_owned())
        .trim()
        .parse()
        .map_err(|e| format!("GRIDFPV_SERVER_ADDR is not an address like {DEFAULT_ADDR}: {e}"))?;
    let dir = data_dir();
    let state = AppState::open(&dir)?;
    let assets = std::env::var_os("GRIDFPV_SERVER_ASSETS")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ASSETS));
    let has_page = assets.join("index.html").is_file();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        println!("gridfpv-server {}", gridfpv_server::BUILD_VERSION);
        println!("  listening on http://{}", listener.local_addr()?);
        println!("  data in {}", dir.display());
        for club in state.book.clubs() {
            println!("  club: {}", club.name);
        }
        if state.book.tokens().iter().all(|t| t.revoked_at.is_some()) {
            println!(
                "  No Director can push yet. Make a link token: gridfpv-server token new <label>"
            );
        }
        if has_page {
            println!("  web page from {}", assets.display());
        } else {
            println!(
                "  No web page: {} has no index.html. Pushes and the API still work.",
                assets.display()
            );
        }
        let app = router_with_pages(state, has_page.then_some(assets.as_path()));
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

fn token_new(label: &str) -> Done {
    let book = Book::open(&data_dir())?;
    // The first build has one club (§13.2).
    let club = book
        .clubs()
        .into_iter()
        .next()
        .ok_or("this server has no club")?;
    let (record, secret) = book.new_token(&club.id, label)?;
    println!(
        "Link token for {} ({}), id {}:",
        club.name, record.label, record.id
    );
    println!();
    println!("  {secret}");
    println!();
    println!("Put it in the Director. It is not kept here and cannot be shown again.");
    Ok(())
}

fn token_list() -> Done {
    let book = Book::open(&data_dir())?;
    let tokens = book.tokens();
    if tokens.is_empty() {
        println!("No tokens yet.");
    }
    for token in tokens {
        let club = book
            .club(&token.club)
            .map_or_else(|| "a club that is gone".to_owned(), |club| club.name);
        let state = if token.revoked_at.is_some() {
            "taken back"
        } else {
            "in use"
        };
        println!("{}  {}  {}  ({club})", token.id, state, token.label);
    }
    Ok(())
}

fn token_revoke(id: &str) -> Done {
    if Book::open(&data_dir())?.revoke_token(id)? {
        println!("Token {id} no longer works.");
        Ok(())
    } else {
        Err(format!("there is no token in use with the id {id:?}").into())
    }
}

fn club_name(name: &str) -> Done {
    let book = Book::open(&data_dir())?;
    let club = book
        .clubs()
        .into_iter()
        .next()
        .ok_or("this server has no club")?;
    let club = book.rename_club(&club.id, name)?;
    println!("The club is now called {}.", club.name);
    Ok(())
}
