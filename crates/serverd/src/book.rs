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
//!   run to make or take back a token. So it is read again whenever it has changed on disk.

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
    pub id: ClubId,
    pub name: String,
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

/// When the file was last seen: enough to tell that it has changed.
type Stamp = Option<(SystemTime, u64)>;

struct Held {
    pages: Pages,
    stamp: Stamp,
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

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
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
                stamp: None,
            }),
        };
        if book.path.exists() {
            let pages = book.read_file()?;
            let mut held = book.held.write().expect("book lock");
            held.pages = pages;
            held.stamp = stamp(&book.path);
        } else {
            let mut held = book.held.write().expect("book lock");
            held.pages.clubs.push(Club {
                id: ClubId("club".into()),
                name: "My club".into(),
            });
            book.write_file(&mut held)?;
        }
        Ok(book)
    }

    fn read_file(&self) -> Result<Pages, BookError> {
        let text = std::fs::read_to_string(&self.path)?;
        serde_json::from_str(&text).map_err(|e| BookError::Unreadable {
            path: self.path.display().to_string(),
            why: e.to_string(),
        })
    }

    /// Write the whole book: to a new file first, then moved over the old one, so a crash
    /// leaves either the old book or the new one and never half of one.
    fn write_file(&self, held: &mut Held) -> Result<(), BookError> {
        let text = serde_json::to_string_pretty(&held.pages).expect("the book is plain data");
        let part = self.path.with_extension("json.part");
        std::fs::write(&part, text)?;
        std::fs::rename(&part, &self.path)?;
        held.stamp = stamp(&self.path);
        Ok(())
    }

    /// Read the file again if another process has changed it. A file that has become
    /// unreadable keeps what was last read: a half-saved edit must not lock every Director
    /// out.
    fn refresh(&self) {
        let seen = stamp(&self.path);
        if self.held.read().expect("book lock").stamp == seen {
            return;
        }
        let mut held = self.held.write().expect("book lock");
        if let Ok(pages) = self.read_file() {
            held.pages = pages;
        }
        held.stamp = seen;
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

    /// Give a club another name.
    pub fn rename_club(&self, id: &ClubId, name: &str) -> Result<Club, BookError> {
        let name = clean(
            name,
            "A club needs a name.",
            "A club's name is 80 characters at most, on one line.",
        )?;
        self.change(|pages| {
            let club = pages
                .clubs
                .iter_mut()
                .find(|club| &club.id == id)
                .ok_or_else(|| BookError::NoSuchClub(id.0.clone()))?;
            club.name = name;
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
                name: "My club".into()
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
