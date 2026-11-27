//! The clubs on this server, and the link tokens Directors push with (§13.2, §13.6).
//!
//! All of it is one small file, `server.json`, in the data folder. It is read whole and
//! written whole.
//!
//! - **Every token belongs to one club.** A Director with a club's token pushes into that
//!   club and no other.
//! - **A token is shown once,** when it is made. Only its hash is kept, so the file does not
//!   let whoever reads it push.
//! - **The file may be changed by another process** while the server runs: the same program,
//!   run to make or take back a token. So the file is read every time the book is asked
//!   anything, and taken in again when its contents differ. It is a small file. Its time and
//!   size are not trusted to say it changed: two changes in one tick of the clock can leave
//!   both the same, and a token that was taken back would go on working.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The file the book is kept in, under the data folder.
pub const BOOK_FILE: &str = "server.json";

/// What every link token starts with, so one can be told from other secrets at a glance.
pub const TOKEN_PREFIX: &str = "gfs_";

/// The longest a club's name or a token's label may be.
const NAME_MAX: usize = 80;

/// Identifies a club on this server. Made by the server, and safe in a file path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClubId(pub String);

/// A club: the owner of events, tokens and, later, invites and pilot lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Club {
    /// Never changes. The site's own links use it.
    pub id: ClubId,
    pub name: String,
    /// The club's friendly address: a short word made from its name, for handing out. It
    /// follows the name when the club is renamed.
    #[serde(default)]
    pub word: String,
    /// Friendly addresses the club had before a rename. Each still leads to the club,
    /// until another club takes the word.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub old_words: Vec<String>,
    /// When an owner archived the club. An archived club is read-only and off the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
    /// A public club: anyone with an account may join it (§13.7). When `false` the club is
    /// private, and a person asks to join, uses an invite, or is added by a director. A
    /// club starts private.
    #[serde(default)]
    pub open: bool,
}

/// The longest a friendly address may be.
const WORD_MAX: usize = 40;

/// The friendly address a name makes: its letters and digits in lower case, with one `-`
/// wherever anything else was. A name with none of either makes `club`.
pub fn word_of(name: &str) -> String {
    let mut word = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        } else if !word.is_empty() && !word.ends_with('-') {
            word.push('-');
        }
        if word.chars().count() >= WORD_MAX {
            break;
        }
    }
    let word = word.trim_end_matches('-');
    if word.is_empty() {
        "club".to_owned()
    } else {
        word.to_owned()
    }
}

/// `word`, or `word-2`, `word-3`…: the first that no club in `taken` has as its address now.
fn free_word(word: &str, taken: &[&str]) -> String {
    if !taken.contains(&word) {
        return word.to_owned();
    }
    (2..)
        .map(|n| format!("{word}-{n}"))
        .find(|tried| !taken.contains(&tried.as_str()))
        .expect("there is always another number")
}

/// Give every club that has no friendly address one. A book written before clubs had them
/// is read this way.
fn give_words(clubs: &mut [Club]) {
    for at in 0..clubs.len() {
        if !clubs[at].word.is_empty() {
            continue;
        }
        let word = {
            let taken: Vec<&str> = clubs.iter().map(|club| club.word.as_str()).collect();
            free_word(&word_of(&clubs[at].name), &taken)
        };
        clubs[at].word = word;
    }
}

/// A link token, as it is kept. The token itself is not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkToken {
    /// A short id to name the token by, when listing or taking it back. Not a secret.
    pub id: String,
    pub club: ClubId,
    /// What the server's owner called it, such as "Ryan's laptop".
    pub label: String,
    /// SHA-256 of the token, as hex.
    pub hash: String,
    /// When it was made, in milliseconds since 1970.
    pub created_at: i64,
    /// When it was taken back. A token taken back no longer works.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Pages {
    #[serde(default)]
    clubs: Vec<Club>,
    #[serde(default)]
    tokens: Vec<LinkToken>,
    /// What is noted about an event, apart from the event itself. An event with nothing
    /// noted has no entry.
    #[serde(default)]
    events: Vec<EventNote>,
    /// Whether someone with no invite may make an account (§13.7). The owner's choice.
    /// Off until they switch it on.
    #[serde(default)]
    open_sign_up: bool,
    /// Whether people may sign in with a username and a password kept here (§13.8). The
    /// owner's choice. Off until they switch it on.
    #[serde(default)]
    password_sign_in: bool,
}

/// What the server notes about one event of a club, apart from the event itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventNote {
    pub club: ClubId,
    /// The event's id, as the Director gave it.
    pub event: String,
    /// Left out of the club's list for everyone but those who run the club (§13.9). It can
    /// still be opened by its address.
    #[serde(default)]
    pub hidden: bool,
    /// When someone who runs the club removed it from the server. A Director is not let
    /// send it again until its director says so.
    #[serde(default)]
    pub removed_at: Option<i64>,
}

/// What can go wrong with the book.
#[derive(Debug, thiserror::Error)]
pub enum BookError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// The file is there but is not a book. It is left alone: the owner must look at it.
    #[error("{path} could not be read as the server's records: {why}")]
    Unreadable { path: String, why: String },
    #[error("there is no club with the id {0:?}")]
    NoSuchClub(String),
    #[error("{0}")]
    Invalid(&'static str),
}

struct Held {
    pages: Pages,
    /// The file's contents when it was last taken in. `None` before the first read.
    seen: Option<Vec<u8>>,
}

/// The server's book. See the module docs.
pub struct Book {
    path: PathBuf,
    held: RwLock<Held>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn random(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the operating system gives random bytes");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// A name or label, trimmed, or why it will not do.
fn clean(text: &str, empty: &'static str, long: &'static str) -> Result<String, BookError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(BookError::Invalid(empty));
    }
    if text.chars().count() > NAME_MAX || text.chars().any(char::is_control) {
        return Err(BookError::Invalid(long));
    }
    Ok(text.to_owned())
}

impl Book {
    /// Open the book in a data folder. A folder with no book gets one, with one club
    /// (§13.2: the first build has one club, made when the server is set up).
    pub fn open(dir: &Path) -> Result<Self, BookError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(BOOK_FILE);
        let book = Self {
            path,
            held: RwLock::new(Held {
                pages: Pages::default(),
                seen: None,
            }),
        };
        if book.path.exists() {
            let bytes = std::fs::read(&book.path)?;
            let pages = book.parse(&bytes)?;
            let mut held = book.held.write().expect("book lock");
            held.pages = pages;
            held.seen = Some(bytes);
        } else {
            let mut held = book.held.write().expect("book lock");
            held.pages.clubs.push(Club {
                id: ClubId("club".into()),
                name: "My club".into(),
                word: word_of("My club"),
                old_words: Vec::new(),
                archived_at: None,
                open: false,
            });
            book.write_file(&mut held)?;
        }
        Ok(book)
    }

    fn parse(&self, bytes: &[u8]) -> Result<Pages, BookError> {
        let mut pages: Pages =
            serde_json::from_slice(bytes).map_err(|e| BookError::Unreadable {
                path: self.path.display().to_string(),
                why: e.to_string(),
            })?;
        give_words(&mut pages.clubs);
        Ok(pages)
    }

    /// Write the whole book: to a new file first, then moved over the old one, so a crash
    /// leaves either the old book or the new one and never half of one.
    fn write_file(&self, held: &mut Held) -> Result<(), BookError> {
        let text = serde_json::to_string_pretty(&held.pages).expect("the book is plain data");
        let part = self.path.with_extension("json.part");
        std::fs::write(&part, &text)?;
        std::fs::rename(&part, &self.path)?;
        held.seen = Some(text.into_bytes());
        Ok(())
    }

    /// Take the file in again if another process has changed it. A file that cannot be
    /// read, or has become unreadable, keeps what was last taken in: a half-saved edit must
    /// not lock every Director out.
    fn refresh(&self) {
        let Ok(bytes) = std::fs::read(&self.path) else {
            return;
        };
        if self.held.read().expect("book lock").seen.as_deref() == Some(bytes.as_slice()) {
            return;
        }
        let mut held = self.held.write().expect("book lock");
        if let Ok(pages) = self.parse(&bytes) {
            held.pages = pages;
        }
        // Remembered either way, so a spoiled file is not parsed again on every request.
        held.seen = Some(bytes);
    }

    /// Change the book and save it. The change is not kept if it cannot be saved.
    fn change<R>(
        &self,
        f: impl FnOnce(&mut Pages) -> Result<R, BookError>,
    ) -> Result<R, BookError> {
        self.refresh();
        let mut held = self.held.write().expect("book lock");
        let before = held.pages.clone();
        let out = f(&mut held.pages)?;
        if let Err(e) = self.write_file(&mut held) {
            held.pages = before;
            return Err(e);
        }
        Ok(out)
    }

    /// Every club, in the order they were made.
    pub fn clubs(&self) -> Vec<Club> {
        self.refresh();
        self.held.read().expect("book lock").pages.clubs.clone()
    }

    pub fn club(&self, id: &ClubId) -> Option<Club> {
        self.clubs().into_iter().find(|club| &club.id == id)
    }

    /// The club a friendly address leads to: the club that has the word now, or else the
    /// one that had it before a rename.
    pub fn club_by_word(&self, word: &str) -> Option<Club> {
        let word = word.trim().to_lowercase();
        let clubs = self.clubs();
        clubs
            .iter()
            .find(|club| club.word == word)
            .or_else(|| clubs.iter().find(|club| club.old_words.contains(&word)))
            .cloned()
    }

    /// Make a club. Its id is random and never changes; its friendly address comes from
    /// its name.
    pub fn new_club(&self, name: &str) -> Result<Club, BookError> {
        let name = clean(
            name,
            "A club needs a name.",
            "A club's name is 80 characters at most, on one line.",
        )?;
        self.change(|pages| {
            let id = loop {
                // Lower-case letters and digits only: safe as a folder name everywhere.
                let mut buf = [0u8; 6];
                getrandom::fill(&mut buf).expect("the operating system gives random bytes");
                let id = ClubId(format!(
                    "c{}",
                    buf.iter().map(|b| format!("{b:02x}")).collect::<String>()
                ));
                if !pages.clubs.iter().any(|club| club.id == id) {
                    break id;
                }
            };
            let word = Self::take_word(pages, &id, &word_of(&name));
            let club = Club {
                id,
                name,
                word,
                old_words: Vec::new(),
                archived_at: None,
                open: false,
            };
            pages.clubs.push(club.clone());
            Ok(club)
        })
    }

    /// The friendly address a club gets for `wanted`: that word, or it with a number when
    /// another club has it now. Whoever only *had* the word before a rename loses it.
    fn take_word(pages: &mut Pages, id: &ClubId, wanted: &str) -> String {
        let word = {
            let taken: Vec<&str> = pages
                .clubs
                .iter()
                .filter(|club| &club.id != id)
                .map(|club| club.word.as_str())
                .collect();
            free_word(wanted, &taken)
        };
        for club in &mut pages.clubs {
            club.old_words.retain(|old| old != &word);
        }
        word
    }

    /// Give a club another name. Its friendly address follows, and the old one still leads
    /// to it.
    pub fn rename_club(&self, id: &ClubId, name: &str) -> Result<Club, BookError> {
        let name = clean(
            name,
            "A club needs a name.",
            "A club's name is 80 characters at most, on one line.",
        )?;
        self.change(|pages| {
            let before = pages
                .clubs
                .iter()
                .find(|club| &club.id == id)
                .ok_or_else(|| BookError::NoSuchClub(id.0.clone()))?
                .word
                .clone();
            let wanted = word_of(&name);
            // A new name that makes the same word, or the same word with the number it
            // already has, keeps the address.
            let word = if wanted == before
                || before
                    .strip_prefix(&format!("{wanted}-"))
                    .is_some_and(|n| n.parse::<u32>().is_ok())
            {
                before.clone()
            } else {
                Self::take_word(pages, id, &wanted)
            };
            let club = pages
                .clubs
                .iter_mut()
                .find(|club| &club.id == id)
                .expect("found above");
            club.name = name;
            if word != before {
                club.old_words.push(before);
                club.word = word;
            }
            Ok(club.clone())
        })
    }

    /// Whether someone with no invite may make an account on this server (§13.7).
    pub fn open_sign_up(&self) -> bool {
        self.refresh();
        self.held.read().expect("book lock").pages.open_sign_up
    }

    /// Whether people may sign in with a username and password kept on this server.
    pub fn password_sign_in(&self) -> bool {
        self.refresh();
        self.held.read().expect("book lock").pages.password_sign_in
    }

    /// Switch signing in with a username and password on or off.
    pub fn set_password_sign_in(&self, on: bool) -> Result<bool, BookError> {
        self.change(|pages| {
            pages.password_sign_in = on;
            Ok(on)
        })
    }

    /// Let anyone who can sign in make an account, or only those with an invite.
    pub fn set_open_sign_up(&self, open: bool) -> Result<bool, BookError> {
        self.change(|pages| {
            pages.open_sign_up = open;
            Ok(open)
        })
    }

    /// Make a club public (anyone with an account may join) or private (§13.7).
    pub fn set_club_open(&self, id: &ClubId, open: bool) -> Result<Club, BookError> {
        self.change(|pages| {
            let club = pages
                .clubs
                .iter_mut()
                .find(|club| &club.id == id)
                .ok_or_else(|| BookError::NoSuchClub(id.0.clone()))?;
            club.open = open;
            Ok(club.clone())
        })
    }

    /// Archive a club, or bring it back. An archived club is read-only and off the list.
    pub fn archive_club(&self, id: &ClubId, archived: bool) -> Result<Club, BookError> {
        self.change(|pages| {
            let club = pages
                .clubs
                .iter_mut()
                .find(|club| &club.id == id)
                .ok_or_else(|| BookError::NoSuchClub(id.0.clone()))?;
            match (archived, club.archived_at) {
                (true, None) => club.archived_at = Some(now_ms()),
                (false, Some(_)) => club.archived_at = None,
                _ => {}
            }
            Ok(club.clone())
        })
    }

    /// Make a link token for a club. Returns its record and **the token itself, which is
    /// not kept** and cannot be shown again.
    pub fn new_token(&self, club: &ClubId, label: &str) -> Result<(LinkToken, String), BookError> {
        let label = clean(
            label,
            "A token needs a label, so it can be told from the others.",
            "A token's label is 80 characters at most, on one line.",
        )?;
        let secret = format!("{TOKEN_PREFIX}{}", random(32));
        let record = self.change(|pages| {
            if !pages.clubs.iter().any(|c| &c.id == club) {
                return Err(BookError::NoSuchClub(club.0.clone()));
            }
            let id = loop {
                let id = random(6);
                if !pages.tokens.iter().any(|t| t.id == id) {
                    break id;
                }
            };
            let record = LinkToken {
                id,
                club: club.clone(),
                label,
                hash: hash(&secret),
                created_at: now_ms(),
                revoked_at: None,
            };
            pages.tokens.push(record.clone());
            Ok(record)
        })?;
        Ok((record, secret))
    }

    /// Take a token back, by its id. `false` when there is no such token, or it was already
    /// taken back.
    pub fn revoke_token(&self, id: &str) -> Result<bool, BookError> {
        self.change(|pages| {
            match pages
                .tokens
                .iter_mut()
                .find(|t| t.id == id && t.revoked_at.is_none())
            {
                Some(token) => {
                    token.revoked_at = Some(now_ms());
                    Ok(true)
                }
                None => Ok(false),
            }
        })
    }

    /// Take back a token of one club, by its id. `false` when the club has no such token,
    /// or it was already taken back.
    pub fn revoke_club_token(&self, club: &ClubId, id: &str) -> Result<bool, BookError> {
        self.change(|pages| {
            match pages
                .tokens
                .iter_mut()
                .find(|t| t.id == id && &t.club == club && t.revoked_at.is_none())
            {
                Some(token) => {
                    token.revoked_at = Some(now_ms());
                    Ok(true)
                }
                None => Ok(false),
            }
        })
    }

    /// What is noted about an event. Nothing noted reads as not hidden and not removed.
    pub fn event_note(&self, club: &ClubId, event: &str) -> EventNote {
        self.refresh();
        self.held
            .read()
            .expect("book lock")
            .pages
            .events
            .iter()
            .find(|note| &note.club == club && note.event == event)
            .cloned()
            .unwrap_or_else(|| EventNote {
                club: club.clone(),
                event: event.to_owned(),
                hidden: false,
                removed_at: None,
            })
    }

    /// Every event of a club that has something noted.
    pub fn event_notes(&self, club: &ClubId) -> Vec<EventNote> {
        self.refresh();
        self.held
            .read()
            .expect("book lock")
            .pages
            .events
            .iter()
            .filter(|note| &note.club == club)
            .cloned()
            .collect()
    }

    /// Change what is noted about an event. A note that says nothing is not kept.
    pub fn note_event(
        &self,
        club: &ClubId,
        event: &str,
        change: impl FnOnce(&mut EventNote),
    ) -> Result<EventNote, BookError> {
        self.change(|pages| {
            if !pages.clubs.iter().any(|c| &c.id == club) {
                return Err(BookError::NoSuchClub(club.0.clone()));
            }
            let at = pages
                .events
                .iter()
                .position(|note| &note.club == club && note.event == event);
            let mut note = match at {
                Some(at) => pages.events.remove(at),
                None => EventNote {
                    club: club.clone(),
                    event: event.to_owned(),
                    hidden: false,
                    removed_at: None,
                },
            };
            change(&mut note);
            if note.hidden || note.removed_at.is_some() {
                pages.events.push(note.clone());
            }
            Ok(note)
        })
    }

    /// Every token ever made, those taken back too.
    pub fn tokens(&self) -> Vec<LinkToken> {
        self.refresh();
        self.held.read().expect("book lock").pages.tokens.clone()
    }

    /// The club a token pushes into. `None` for a token that was never made, was taken back,
    /// or whose club is gone.
    pub fn club_for_token(&self, token: &str) -> Option<Club> {
        if !token.starts_with(TOKEN_PREFIX) {
            return None;
        }
        self.refresh();
        let hash = hash(token);
        let held = self.held.read().expect("book lock");
        let record = held
            .pages
            .tokens
            .iter()
            .find(|t| t.hash == hash && t.revoked_at.is_none())?;
        held.pages
            .clubs
            .iter()
            .find(|club| club.id == record.club)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn club() -> ClubId {
        ClubId("club".into())
    }

    #[test]
    fn a_new_folder_gets_a_book_with_one_club() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(&dir.path().join("deep/er")).unwrap();
        assert_eq!(
            book.clubs(),
            vec![Club {
                id: club(),
                name: "My club".into(),
                word: "my-club".into(),
                old_words: vec![],
                archived_at: None,
                open: false,
            }]
        );
        assert!(book.tokens().is_empty());
        assert!(dir.path().join("deep/er").join(BOOK_FILE).is_file());
    }

    #[test]
    fn a_token_names_its_club_and_only_while_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        let (record, secret) = book.new_token(&club(), "  Ryan's laptop ").unwrap();
        assert_eq!(record.label, "Ryan's laptop");
        assert!(secret.starts_with(TOKEN_PREFIX));
        assert!(secret.len() > 40);

        assert_eq!(book.club_for_token(&secret).unwrap().id, club());
        // Not the token: its id, its hash, a near miss, nothing, and the prefix alone.
        for wrong in [
            record.id.as_str(),
            record.hash.as_str(),
            &secret[..secret.len() - 1],
            &format!("{secret}x"),
            &secret.to_uppercase(),
            "",
            TOKEN_PREFIX,
        ] {
            assert!(book.club_for_token(wrong).is_none(), "{wrong:?}");
        }

        assert!(book.revoke_token(&record.id).unwrap());
        assert!(book.club_for_token(&secret).is_none());
        // Taking it back again, or taking back what never was, changes nothing.
        assert!(!book.revoke_token(&record.id).unwrap());
        assert!(!book.revoke_token("nope").unwrap());
        assert_eq!(book.tokens().len(), 1);
        assert!(book.tokens()[0].revoked_at.is_some());
    }

    #[test]
    fn the_token_itself_is_not_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        let (_, secret) = book.new_token(&club(), "laptop").unwrap();
        let text = std::fs::read_to_string(dir.path().join(BOOK_FILE)).unwrap();
        assert!(!text.contains(&secret));
        assert!(!text.contains(&secret[TOKEN_PREFIX.len()..]));
    }

    #[test]
    fn two_tokens_are_two_secrets_and_each_stands_alone() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        let (a, secret_a) = book.new_token(&club(), "a").unwrap();
        let (b, secret_b) = book.new_token(&club(), "a").unwrap();
        assert_ne!(secret_a, secret_b);
        assert_ne!(a.id, b.id);
        book.revoke_token(&a.id).unwrap();
        assert!(book.club_for_token(&secret_a).is_none());
        assert!(book.club_for_token(&secret_b).is_some());
    }

    #[test]
    fn the_book_is_there_when_opened_again() {
        let dir = tempfile::tempdir().unwrap();
        let secret = {
            let book = Book::open(dir.path()).unwrap();
            book.rename_club(&club(), "Recon FPV").unwrap();
            book.new_token(&club(), "laptop").unwrap().1
        };
        let book = Book::open(dir.path()).unwrap();
        assert_eq!(book.clubs()[0].name, "Recon FPV");
        assert_eq!(book.club_for_token(&secret).unwrap().name, "Recon FPV");
    }

    #[test]
    fn a_change_made_by_another_process_is_seen() {
        // The running server, and the same program run again to make a token.
        let dir = tempfile::tempdir().unwrap();
        let server = Book::open(dir.path()).unwrap();
        let cli = Book::open(dir.path()).unwrap();
        let (record, secret) = cli.new_token(&club(), "laptop").unwrap();
        assert!(server.club_for_token(&secret).is_some());
        assert_eq!(server.tokens().len(), 1);

        cli.revoke_token(&record.id).unwrap();
        assert!(server.club_for_token(&secret).is_none());

        // And a change by the server is not lost when the other one writes next.
        server.rename_club(&club(), "Renamed").unwrap();
        cli.new_token(&club(), "second").unwrap();
        assert_eq!(server.clubs()[0].name, "Renamed");
        assert_eq!(server.tokens().len(), 2);
    }

    #[test]
    fn two_changes_of_the_same_size_at_the_same_moment_are_both_seen() {
        // What a check by time and size missed: one process renames the club to a name of
        // the same length, and the other writes straight after. With the change missed, the
        // second wrote its old copy back over the first.
        let dir = tempfile::tempdir().unwrap();
        let server = Book::open(dir.path()).unwrap();
        let cli = Book::open(dir.path()).unwrap();
        for round in 0..200 {
            let name = if round % 2 == 0 { "Renamed" } else { "My club" };
            assert_eq!(name.len(), 7);
            server.rename_club(&club(), name).unwrap();
            assert_eq!(
                cli.clubs()[0].name,
                name,
                "round {round}: the other process saw it"
            );
            cli.rename_club(&club(), "Another").unwrap();
            assert_eq!(server.clubs()[0].name, "Another", "round {round}");
        }
    }

    #[test]
    fn a_token_taken_back_by_another_process_stops_working_however_fast() {
        let dir = tempfile::tempdir().unwrap();
        let server = Book::open(dir.path()).unwrap();
        let cli = Book::open(dir.path()).unwrap();
        for _ in 0..50 {
            let (record, secret) = cli.new_token(&club(), "laptop").unwrap();
            assert!(server.club_for_token(&secret).is_some());
            cli.revoke_token(&record.id).unwrap();
            assert!(server.club_for_token(&secret).is_none());
        }
    }

    #[test]
    fn names_and_labels_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        for bad in ["", "   ", "two\nlines", &"x".repeat(81)] {
            assert!(book.new_token(&club(), bad).is_err(), "{bad:?}");
            assert!(book.rename_club(&club(), bad).is_err(), "{bad:?}");
        }
        assert!(book.new_token(&club(), &"é".repeat(80)).is_ok());
        assert!(matches!(
            book.new_token(&ClubId("other".into()), "x"),
            Err(BookError::NoSuchClub(_))
        ));
        assert!(matches!(
            book.rename_club(&ClubId("other".into()), "x"),
            Err(BookError::NoSuchClub(_))
        ));
        // Nothing was saved by the refused ones.
        assert_eq!(book.tokens().len(), 1);
        assert_eq!(book.clubs()[0].name, "My club");
    }

    #[test]
    fn a_file_that_is_not_a_book_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(BOOK_FILE);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            Book::open(dir.path()),
            Err(BookError::Unreadable { .. })
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn a_book_spoiled_while_running_keeps_what_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        let (_, secret) = book.new_token(&club(), "laptop").unwrap();
        std::fs::write(dir.path().join(BOOK_FILE), "half an edi").unwrap();
        assert!(book.club_for_token(&secret).is_some());
        assert_eq!(book.clubs().len(), 1);
    }

    #[test]
    fn a_book_from_an_older_build_with_less_in_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(BOOK_FILE),
            r#"{"clubs":[{"id":"club","name":"Old","gone":1}],"unknown":true}"#,
        )
        .unwrap();
        let book = Book::open(dir.path()).unwrap();
        assert_eq!(book.clubs()[0].name, "Old");
        assert!(book.tokens().is_empty());
    }

    #[test]
    fn a_token_whose_club_is_gone_does_not_work() {
        let dir = tempfile::tempdir().unwrap();
        let book = Book::open(dir.path()).unwrap();
        let (_, secret) = book.new_token(&club(), "laptop").unwrap();
        let path = dir.path().join(BOOK_FILE);
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\"id\": \"club\"", "\"id\": \"renamed-by-hand\"");
        std::fs::write(&path, text).unwrap();
        assert!(book.club_for_token(&secret).is_none());
    }
}
