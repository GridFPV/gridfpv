//! `gridfpv-server`: the Grid remote server (docs/primitives.md §13).
//!
//! ```text
//! gridfpv-server                       run the server
//! gridfpv-server token new <label>     make a link token for a Director, and show it once
//! gridfpv-server token list            the tokens that were made
//! gridfpv-server token revoke <id>     take a token back
//! gridfpv-server club name <name>      name the club
//! gridfpv-server club list             the clubs, with each one's address
//! gridfpv-server club new <name>       make another club
//! gridfpv-server club archive <club>   make a club read-only and take it off the list
//! gridfpv-server club restore <club>   bring an archived club back
//! ```
//!
//! With more than one club, `token new` and `club name` take the club's address (the word
//! `club list` shows) as one more word at the end: `token new "Ryan's laptop" recon-fpv`.
//!
//! ```text
//! gridfpv-server owner invite          make a link that makes whoever opens it an owner
//! gridfpv-server account list          the accounts, and how each signs in
//! gridfpv-server account reconnect <id>   make a link that lets someone back in to an account
//! ```
//!
//! Set from the environment:
//!
//! - `GRIDFPV_SERVER_DATA_DIR`: where everything is kept. Default `./gridfpv-server-data`.
//! - `GRIDFPV_SERVER_ADDR`: the address to listen on. Default `127.0.0.1:8470`.
//! - `GRIDFPV_SERVER_PUBLIC_URL`: the address people open, such as
//!   `https://grid.example.org`. Signing in needs it.
//! - `GRIDFPV_SERVER_DISCORD_CLIENT_ID` and `…_DISCORD_CLIENT_SECRET`, and the same with
//!   `GOOGLE`: the app registered with each provider, to sign in with it.
//! - `GRIDFPV_SERVER_ASSETS`: a folder with the web page (the built console). Not needed in
//!   a build with the page inside it (`embed-page`). With no page, the server still takes
//!   pushes and answers the API.
//!
//! The server speaks plain HTTP and listens on this computer only by default. It is meant
//! to sit behind a web server that gives it HTTPS (§13.5).
#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use gridfpv_serverd::app::{AppState, router_with_page};
use gridfpv_serverd::book::Book;
use gridfpv_serverd::page::Page;

const DEFAULT_ADDR: &str = "127.0.0.1:8470";
const DEFAULT_DATA_DIR: &str = "gridfpv-server-data";
const DEFAULT_ASSETS: &str = "frontend/apps/rd-console/dist";

const USAGE: &str = "usage:
  gridfpv-server                       run the server
  gridfpv-server token new <label>     make a link token for a Director
  gridfpv-server token list            list the tokens
  gridfpv-server token revoke <id>     take a token back
  gridfpv-server club name <name>      name the club
  gridfpv-server club list             list the clubs, with each one's address
  gridfpv-server club new <name>       make another club
  gridfpv-server club archive <club>   make a club read-only and take it off the list
  gridfpv-server club restore <club>   bring an archived club back
  gridfpv-server owner invite          make a link that makes its user an owner
  gridfpv-server email test <address>  send a test email with the email settings
  gridfpv-server password              say whether people may sign in with a password
  gridfpv-server password on           let people sign in with a username and password
  gridfpv-server password off          sign in with Discord, Google and the like only
  gridfpv-server sign-up               say who may make an account
  gridfpv-server sign-up open          let anyone who can sign in make an account
  gridfpv-server sign-up invite        an account needs an invite (how a server starts)
  gridfpv-server account list          list the accounts, and how each signs in
  gridfpv-server account reconnect <id>   make a link that lets someone back in";

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
        ["token", "new", label] => token_new(label, None),
        ["token", "new", label, club] => token_new(label, Some(club)),
        ["token", "list"] => token_list(),
        ["token", "revoke", id] => token_revoke(id),
        ["club", "name", name] => club_name(name, None),
        ["club", "name", name, club] => club_name(name, Some(club)),
        ["club", "list"] => club_list(),
        ["club", "new", name] => club_new(name),
        ["club", "archive", club] => club_archive(club, true),
        ["club", "restore", club] => club_archive(club, false),
        ["owner", "invite"] => owner_invite(),
        ["email", "test", to] => email_test(to),
        ["password"] => password(None),
        ["password", "on"] => password(Some(true)),
        ["password", "off"] => password(Some(false)),
        ["sign-up"] => sign_up(None),
        ["sign-up", "open"] => sign_up(Some(true)),
        ["sign-up", "invite"] => sign_up(Some(false)),
        ["account", "list"] => account_list(),
        ["account", "reconnect", id] => account_reconnect(id),
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
    // Settings for email that do not make sense stop the server from starting: a server
    // that silently sent nothing would be found out only when someone needed a link.
    let mailer = match gridfpv_serverd::email::MailSettings::from_env()? {
        Some(settings) => Some(gridfpv_serverd::email::Mailer::smtp(&settings)?),
        None => None,
    };
    let state = AppState::open(&dir)?
        .at(public_url())
        .with_providers(gridfpv_serverd::signin::Provider::from_env())
        .with_mailer(mailer);
    let asked = std::env::var_os("GRIDFPV_SERVER_ASSETS")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let page = Page::pick(asked.clone(), std::path::Path::new(DEFAULT_ASSETS));
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
        match (&state.public_url, state.providers.is_empty()) {
            (Some(url), false) => {
                let names: Vec<&str> = state.providers.iter().map(|p| p.name.as_str()).collect();
                println!("  sign-in with {} at {url}", names.join(" and "));
            }
            (None, false) => println!(
                "  Sign-in is off: set GRIDFPV_SERVER_PUBLIC_URL to the address people open."
            ),
            (_, true) if state.book.password_sign_in() => {}
            (_, true) => println!(
                "  Sign-in is off: no provider is set up, and passwords are off (gridfpv-server password on)."
            ),
        }
        if state.book.password_sign_in() {
            println!("  sign-in with a username and password");
        }
        match (&state.mailer, &state.public_url) {
            (Some(_), Some(_)) => println!("  email is set up"),
            (Some(_), None) => println!(
                "  Email is off: set GRIDFPV_SERVER_PUBLIC_URL, so a link in a mail can be opened."
            ),
            (None, _) => {}
        }
        if state.book.open_sign_up() {
            println!("  Anyone who can sign in may make an account.");
        }
        if state.book.tokens().iter().all(|t| t.revoked_at.is_some()) {
            println!(
                "  No Director can push yet. Make a link token: gridfpv-server token new <label>"
            );
        }
        match (&page, &asked) {
            (Page::Embedded, _) => println!("  web page: built in"),
            (Page::Folder(dir), _) => println!("  web page from {}", dir.display()),
            (Page::None, Some(dir)) => println!(
                "  No web page: {} has no index.html. Pushes and the API still work.",
                dir.display()
            ),
            (Page::None, None) => {
                println!("  No web page. Pushes and the API still work.")
            }
        }
        let app = router_with_page(state, page);
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

/// The club a command is about: the one named by its address, or the only one there is.
fn pick_club(book: &Book, word: Option<&str>) -> Result<gridfpv_serverd::book::Club, String> {
    let clubs = book.clubs();
    match word {
        Some(word) => book.club_by_word(word).ok_or_else(|| {
            format!("there is no club with the address {word:?}. See them with: gridfpv-server club list")
        }),
        None => match clubs.as_slice() {
            [] => Err("this server has no club".to_owned()),
            [only] => Ok(only.clone()),
            _ => Err(
                "this server has more than one club. Say which, by its address, as the last word. See them with: gridfpv-server club list"
                    .to_owned(),
            ),
        },
    }
}

fn token_new(label: &str, club: Option<&str>) -> Done {
    let book = Book::open(&data_dir())?;
    let club = pick_club(&book, club)?;
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

fn public_url() -> Option<String> {
    std::env::var("GRIDFPV_SERVER_PUBLIC_URL").ok()
}

/// A link that makes whoever opens it and signs in an owner of this server. It works once
/// and for a day. This is how the first owner is made (§13.8), and how another is added.
fn owner_invite() -> Done {
    use gridfpv_serverd::accounts::{Accounts, InviteKind, NewInvite};
    let accounts = Accounts::open(&data_dir())?;
    let now = gridfpv_serverd::people::now_ms();
    let (_, secret) = accounts.new_invite(
        NewInvite {
            kind: InviteKind::Owner,
            label: "Owner setup".to_owned(),
            max_uses: 1,
            expires_at: Some(now + 24 * 60 * 60 * 1000),
        },
        None,
        now,
    )?;
    let base = public_url()
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "https://<your server>".to_owned());
    println!("Open this link and sign in. Whoever does becomes an owner of this server:");
    println!();
    println!("  {base}/#/invite/{secret}");
    println!();
    println!("It works once, and for one day. It is not kept here and cannot be shown again.");
    Ok(())
}

/// The address people open, or a stand-in to fill in by hand.
fn link_base() -> String {
    public_url()
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| "https://<your server>".to_owned())
}

/// Every account: its name, its id, and how it signs in.
fn account_list() -> Done {
    use gridfpv_serverd::accounts::Accounts;
    let accounts = Accounts::open(&data_dir())?;
    let all = accounts.all_accounts()?;
    if all.is_empty() {
        println!("No accounts yet. Make the first owner: gridfpv-server owner invite");
        return Ok(());
    }
    for account in all {
        let owner = accounts
            .standing(&account.id)?
            .is_some_and(|standing| standing.owner);
        let keys: Vec<String> = accounts
            .sign_ins(&account.id)?
            .into_iter()
            .map(|key| match key.label.as_str() {
                "" => key.provider,
                label => format!("{} ({label})", key.provider),
            })
            .collect();
        let keys = if keys.is_empty() {
            "no way to sign in".to_owned()
        } else {
            keys.join(", ")
        };
        let owner = if owner { "  owner" } else { "" };
        println!("{}  {}{owner}  {keys}", account.id.0, account.name);
    }
    Ok(())
}

/// A link that lets whoever opens it and signs in back in to an account (§13.8). This is
/// how an owner who has lost their own sign-in gets back in.
fn account_reconnect(id: &str) -> Done {
    use gridfpv_serverd::accounts::{AccountId, Accounts};
    let accounts = Accounts::open(&data_dir())?;
    let id = AccountId(id.to_owned());
    let Some(account) = accounts.account(&id)? else {
        return Err(format!(
            "there is no account with the id {:?}. See them with: gridfpv-server account list",
            id.0
        )
        .into());
    };
    let (_, secret) = accounts.new_reconnect(&id, None, gridfpv_serverd::people::now_ms())?;
    println!(
        "Send this link to {}. Whoever opens it and signs in gets into that account:",
        account.name
    );
    println!();
    println!("  {}/#/reconnect/{secret}", link_base());
    println!();
    println!("It works once, and for seven days. It is not kept here and cannot be shown again.");
    Ok(())
}

/// Send one email with the settings in the environment, to check they work.
fn email_test(to: &str) -> Done {
    use gridfpv_serverd::email::{Mail, MailSettings, Mailer};
    let Some(settings) = MailSettings::from_env()? else {
        return Err("Email is not set up: GRIDFPV_SERVER_SMTP_HOST is not set.".into());
    };
    let mailer = Mailer::smtp(&settings)?;
    let mail = Mail {
        to: to.to_owned(),
        subject: "A test from your Grid server".into(),
        body: "This is a test. If you can read it, your Grid server can send email.\n".into(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(mailer.send(mail))?;
    println!(
        "Sent to {to}, through {} on port {}. Look for it in the inbox.",
        settings.host, settings.port
    );
    Ok(())
}

/// Say whether people may sign in with a username and password, after changing it when
/// asked to.
fn password(on: Option<bool>) -> Done {
    let book = Book::open(&data_dir())?;
    let on = match on {
        Some(on) => book.set_password_sign_in(on)?,
        None => book.password_sign_in(),
    };
    if on {
        println!("People may sign in with a username and a password kept on this server.");
        println!("To switch it off: gridfpv-server password off");
    } else {
        println!("Signing in with a password is off.");
        println!("To switch it on: gridfpv-server password on");
        println!("Anyone whose only way in is a password cannot sign in while it is off.");
    }
    Ok(())
}

/// Say who may make an account, after changing it when asked to.
fn sign_up(open: Option<bool>) -> Done {
    let book = Book::open(&data_dir())?;
    let open = match open {
        Some(open) => book.set_open_sign_up(open)?,
        None => book.open_sign_up(),
    };
    if open {
        println!("Anyone who can sign in may make an account.");
        println!("To ask for an invite again: gridfpv-server sign-up invite");
    } else {
        println!("An account needs an invite.");
        println!("To let anyone make one: gridfpv-server sign-up open");
    }
    Ok(())
}

fn club_name(name: &str, club: Option<&str>) -> Done {
    let book = Book::open(&data_dir())?;
    let club = pick_club(&book, club)?;
    let club = book.rename_club(&club.id, name)?;
    println!("The club is now called {}.", club.name);
    println!("Its address is {}/#/c/{}", link_base(), club.word);
    Ok(())
}

fn club_list() -> Done {
    let book = Book::open(&data_dir())?;
    for club in book.clubs() {
        let archived = if club.archived_at.is_some() {
            "  archived"
        } else {
            ""
        };
        println!("{}  {}{archived}", club.word, club.name);
    }
    Ok(())
}

fn club_new(name: &str) -> Done {
    let book = Book::open(&data_dir())?;
    let club = book.new_club(name)?;
    println!("Made the club {}.", club.name);
    println!("Its address is {}/#/c/{}", link_base(), club.word);
    println!(
        "Make a link token for its director: gridfpv-server token new <label> {}",
        club.word
    );
    Ok(())
}

fn club_archive(word: &str, archived: bool) -> Done {
    let book = Book::open(&data_dir())?;
    let club = pick_club(&book, Some(word))?;
    let club = book.archive_club(&club.id, archived)?;
    if archived {
        use gridfpv_serverd::accounts::Accounts;
        Accounts::open(&data_dir())?
            .revoke_invites_of(&club.id, gridfpv_serverd::people::now_ms())?;
        println!(
            "{} is archived. It is off the list and nothing in it can be changed. Its events still open by their links.",
            club.name
        );
    } else {
        println!("{} is back.", club.name);
    }
    Ok(())
}
