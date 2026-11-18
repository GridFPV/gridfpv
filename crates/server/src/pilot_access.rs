//! How a pilot says who they are, at the race (docs/primitives.md §11.3).
//!
//! The Director's pilot list is who people are at a race. The director adds every pilot.
//! To act for themselves from a phone, a pilot signs in with a short **code** that belongs
//! to them for this one event. The director shows it from the console, as letters and as a
//! QR code that holds the same letters in a link.
//!
//! - A code is made the first time the director looks at an event's codes.
//! - It works for **that event only**, and only while the pilot is in the event.
//! - **One device.** Signing in again with the same code, on another phone, signs the
//!   first one out.
//! - The director can give a pilot a new code (the old one stops working and the pilot is
//!   signed out), or just sign their device out.
//!
//! Signing in sets a cookie the browser sends by itself. It says who the pilot is and
//! nothing more: a signed-in pilot is still a visitor to every part of the console.
//!
//! The codes and the sign-ins are kept in `pilot-access.json` in the data folder, so
//! closing and opening Grid at the field does not sign everyone out. They are not part of
//! a copy made on the Data page, and they never go into an event's log.
//!
//! **The kiosk** (§11.4) is a shared tablet. A pilot proves who they are there with the
//! same code, but it must not sign their phone out, and it must not leave them signed in
//! for the next person. So a code typed at a kiosk gives a **kiosk pass**: a random token
//! the page keeps in memory and sends in the [`KIOSK_HEADER`] header. It is never written
//! down, it stops working [`KIOSK_IDLE`] after it was last used, and a new code or the
//! director's "sign out" ends it.
//!
//! **Only the director makes a device a kiosk.** A kiosk is a tablet the director sets up
//! at the race director's stand; pilots are never given a way to make one. The director
//! makes a **kiosk code** on the console's Network page and types it on the tablet. A code
//! works **once**: it is used up by the tablet that takes it, and the director makes
//! another for the next tablet. The tablet is given the [`KIOSK_COOKIE`] cookie, and is
//! then on the director's list of kiosks until the director removes it. Without that a
//! device cannot use a pilot's code the kiosk way, so a pilot cannot get round "one
//! device" by opening the kiosk page on a second phone. Being a kiosk opens nothing else.
//!
//! Wrong codes are counted by address like wrong access tokens: after
//! [`crate::access::MAX_FAILURES`] in a minute, that device has to wait.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, Extension, Path, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, SET_COOKIE,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use gridfpv_events::PilotId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventId, EventRegistry};
use crate::view::EventView;

/// The file the codes and sign-ins are kept in.
pub const ACCESS_FILE: &str = "pilot-access.json";
/// How many letters and digits a code has.
pub const CODE_LENGTH: usize = 6;
/// What a code is written in: capitals and digits, without the ones that are easy to
/// mistake for each other (`0` and `O`, `1` and `I`). 32 of them, so a random byte picks
/// one evenly.
pub const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// The cookie that carries a pilot's sign-in.
pub const PILOT_COOKIE: &str = "gridfpv_pilot";
/// How long the cookie lasts: two days, which covers a weekend event. The sign-in also
/// ends when the director says so, or the pilot leaves the event.
const COOKIE_SECONDS: u32 = 2 * 24 * 60 * 60;

/// The header a kiosk sends its pass in.
pub const KIOSK_HEADER: &str = "x-gridfpv-kiosk";
/// How long a kiosk pass lasts after it was last used. The page signs the pilot out
/// sooner by itself; this is the backstop for a tablet that was closed or lost its page.
pub const KIOSK_IDLE: Duration = Duration::from_secs(120);
/// The most kiosk passes kept at once. The oldest goes when one more is made.
const KIOSK_MAX: usize = 64;

/// The cookie that marks a device as a kiosk. It holds a token only that tablet has.
pub const KIOSK_COOKIE: &str = "gridfpv_kiosk";
/// How long a tablet stays a kiosk: thirty days, or until the director removes it.
const KIOSK_COOKIE_SECONDS: u32 = 30 * 24 * 60 * 60;
/// The most kiosk codes that can be waiting to be used at once.
pub const KIOSK_CODES_MAX: usize = 12;
/// The most kiosks at once. The oldest makes room for one more.
pub const KIOSK_DEVICES_MAX: usize = 24;

/// A tablet the director has set up as a kiosk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct KioskDevice {
    /// Its number on the director's list: "Kiosk 2".
    number: u32,
    /// What its cookie holds.
    token: String,
    /// When it was set up, in seconds since 1970.
    set_up_at: u64,
    /// What the director calls it ("By the gate"), so two tablets can be told apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

/// The longest name a kiosk can have, in characters.
pub const KIOSK_NAME_MAX: usize = 40;

/// A pilot at a kiosk.
struct KioskPass {
    event: EventId,
    pilot: PilotId,
    used: Instant,
}

/// One pilot's code, and the device signed in with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

/// Everything kept, by event.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    events: BTreeMap<EventId, BTreeMap<PilotId, Entry>>,
    /// Kiosk codes the director has made that no tablet has used yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    kiosk_codes: Vec<String>,
    /// The tablets set up as kiosks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    kiosks: Vec<KioskDevice>,
    /// The number the last kiosk got. Numbers are not used twice.
    #[serde(default)]
    last_kiosk: u32,
}

struct Inner {
    file: Option<PathBuf>,
    stored: Stored,
    /// Kiosk passes, by token. Kept in memory only.
    kiosk: HashMap<String, KioskPass>,
}

/// One pilot's code, as the director's console shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PilotCodeView {
    pub pilot: PilotId,
    /// The code, six letters and digits.
    pub code: String,
    /// Whether a device is signed in with it.
    pub signed_in: bool,
}

/// `GET /events/{id}/access`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PilotAccessView {
    /// Every pilot in the event, with their code.
    pub pilots: Vec<PilotCodeView>,
    /// The address a pilot's phone opens, when other devices can reach Grid. A code's QR
    /// is this plus `#/join/CODE`. Absent while the network is off or has no address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub address: Option<String>,
}

/// `POST /public/pilot/session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PilotSignInRequest {
    pub code: String,
}

/// A pilot, as far as their own phone is told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PublicPilot {
    pub id: PilotId,
    pub callsign: String,
}

/// The codes and sign-ins, shared by the API.
#[derive(Clone)]
pub struct PilotAccess {
    inner: Arc<RwLock<Inner>>,
}

/// A code as typed: capitals, with spaces and dashes taken out. `None` when what is left
/// is not a code at all (the wrong length, or a character no code has).
///
/// A whole sign-in link is taken too (`…#/join/CODE`), which is what a handheld scanner
/// types when it reads a pilot's QR code.
pub fn normalize(typed: &str) -> Option<String> {
    let typed = typed.rsplit_once("/join/").map_or(typed, |(_, code)| code);
    let code: String = typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == CODE_LENGTH && code.bytes().all(|b| CODE_ALPHABET.contains(&b))).then_some(code)
}

/// How many letters and digits a kiosk code has. Longer than a pilot's code: it is typed
/// once, by the director.
pub const KIOSK_KEY_LENGTH: usize = 8;

/// A kiosk key as typed: capitals, with spaces and dashes taken out.
pub fn normalize_key(typed: &str) -> String {
    typed
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A fresh kiosk key, in the letters and digits codes are written in.
fn random_key() -> String {
    let mut bytes = [0u8; KIOSK_KEY_LENGTH];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    bytes
        .iter()
        .map(|byte| CODE_ALPHABET[usize::from(byte % 32)] as char)
        .collect()
}

/// A fresh code, drawn from the system's source of randomness.
fn random_code() -> String {
    let mut bytes = [0u8; CODE_LENGTH];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    bytes
        .iter()
        .map(|byte| CODE_ALPHABET[usize::from(byte % 32)] as char)
        .collect()
}

/// Compare two codes in the same time whatever they hold, so how long a wrong guess takes
/// says nothing about how close it was.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |diff, (x, y)| diff | (x ^ y))
            == 0
}

impl PilotAccess {
    /// The codes kept under `data_dir`. A file that is missing or cannot be read means
    /// nobody has a code yet.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let file = data_dir.map(|dir| dir.join(ACCESS_FILE));
        let stored = file
            .as_ref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                file,
                stored,
                kiosk: HashMap::new(),
            })),
        }
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Write what is kept. A failure is not passed on: the codes still work until Grid is
    /// closed, and the next change tries again.
    fn save(inner: &Inner) {
        if let Some(file) = &inner.file {
            let bytes = serde_json::to_vec_pretty(&inner.stored).unwrap_or_default();
            if let Err(err) = std::fs::write(file, bytes) {
                eprintln!("WARNING: could not save the pilots' sign-in codes: {err}");
            }
        }
    }

    /// Bring an event's codes in line with who is in it: a pilot with no code gets one, and
    /// a pilot who has left the event loses theirs (and is signed out). Gives every pilot's
    /// code, in `pilots` order.
    pub fn codes(&self, event: &EventId, pilots: &BTreeSet<PilotId>) -> Vec<PilotCodeView> {
        let mut inner = self.write();
        let entries = inner.stored.events.entry(event.clone()).or_default();
        let before = entries.clone();
        entries.retain(|pilot, _| pilots.contains(pilot));
        for pilot in pilots {
            if !entries.contains_key(pilot) {
                // Two pilots must never share a code: draw again on the rare clash.
                let code = loop {
                    let code = random_code();
                    if !entries.values().any(|entry| entry.code == code) {
                        break code;
                    }
                };
                entries.insert(
                    pilot.clone(),
                    Entry {
                        code,
                        session: None,
                    },
                );
            }
        }
        let view = pilots
            .iter()
            .map(|pilot| {
                let entry = &entries[pilot];
                PilotCodeView {
                    pilot: pilot.clone(),
                    code: entry.code.clone(),
                    signed_in: entry.session.is_some(),
                }
            })
            .collect();
        if *entries != before {
            Self::save(&inner);
        }
        view
    }

    /// Give a pilot a new code. The old one stops working and their device is signed out.
    /// `None` when the pilot has no code (they are not in the event).
    pub fn reset(&self, event: &EventId, pilot: &PilotId) -> Option<PilotCodeView> {
        let mut inner = self.write();
        let entries = inner.stored.events.get_mut(event)?;
        if !entries.contains_key(pilot) {
            return None;
        }
        let code = loop {
            let code = random_code();
            if !entries.values().any(|entry| entry.code == code) {
                break code;
            }
        };
        entries.insert(
            pilot.clone(),
            Entry {
                code: code.clone(),
                session: None,
            },
        );
        inner
            .kiosk
            .retain(|_, pass| !(pass.event == *event && pass.pilot == *pilot));
        Self::save(&inner);
        Some(PilotCodeView {
            pilot: pilot.clone(),
            code,
            signed_in: false,
        })
    }

    /// Sign a pilot's device out. Their code still works. Whether there was a sign-in.
    pub fn sign_out(&self, event: &EventId, pilot: &PilotId) -> bool {
        let mut inner = self.write();
        // A kiosk they are standing at is signed out too, though it does not count as a
        // device: it never shows as "signed in".
        inner
            .kiosk
            .retain(|_, pass| !(pass.event == *event && pass.pilot == *pilot));
        let ended = inner
            .stored
            .events
            .get_mut(event)
            .and_then(|entries| entries.get_mut(pilot))
            .and_then(|entry| entry.session.take())
            .is_some();
        if ended {
            Self::save(&inner);
        }
        ended
    }

    /// Sign in with a code, for `event`. `pilots` is who is in the event right now: a code
    /// of a pilot who has left it does not work. Gives the pilot and the new sign-in, which
    /// replaces any earlier one for that pilot.
    pub fn sign_in(
        &self,
        event: &EventId,
        pilots: &BTreeSet<PilotId>,
        typed: &str,
    ) -> Option<(PilotId, String)> {
        let mut inner = self.write();
        let pilot = Self::pilot_of(&inner, event, pilots, typed)?;
        let session = crate::auth::random_token();
        inner.stored.events.get_mut(event)?.get_mut(&pilot)?.session = Some(session.clone());
        Self::save(&inner);
        Some((pilot, session))
    }

    /// The pilot a code belongs to, in `event`, if they are in it right now.
    fn pilot_of(
        inner: &Inner,
        event: &EventId,
        pilots: &BTreeSet<PilotId>,
        typed: &str,
    ) -> Option<PilotId> {
        let code = normalize(typed)?;
        let entries = inner.stored.events.get(event)?;
        // Look at every entry, so the time taken does not depend on which one matched.
        let mut found = None;
        for (pilot, entry) in entries.iter() {
            if same(&entry.code, &code) && pilots.contains(pilot) {
                found = Some(pilot.clone());
            }
        }
        found
    }

    /// The kiosk codes waiting to be used and the kiosks set up, for the director.
    pub fn kiosk_view(&self) -> KioskView {
        let inner = self.read();
        KioskView {
            codes: inner.stored.kiosk_codes.clone(),
            kiosks: inner
                .stored
                .kiosks
                .iter()
                .map(|device| KioskDeviceView {
                    number: device.number,
                    set_up_at: device.set_up_at,
                    name: device.name.clone(),
                })
                .collect(),
        }
    }

    /// Make one more kiosk code. `None` when [`KIOSK_CODES_MAX`] are already waiting.
    pub fn kiosk_new_code(&self) -> Option<String> {
        let mut inner = self.write();
        if inner.stored.kiosk_codes.len() >= KIOSK_CODES_MAX {
            return None;
        }
        let code = loop {
            let code = random_key();
            if !inner.stored.kiosk_codes.contains(&code) {
                break code;
            }
        };
        inner.stored.kiosk_codes.push(code.clone());
        Self::save(&inner);
        Some(code)
    }

    /// Throw away a kiosk code that has not been used. Whether there was one.
    pub fn kiosk_cancel_code(&self, typed: &str) -> bool {
        let code = normalize_key(typed);
        let mut inner = self.write();
        let before = inner.stored.kiosk_codes.len();
        inner.stored.kiosk_codes.retain(|kept| *kept != code);
        let found = inner.stored.kiosk_codes.len() != before;
        if found {
            Self::save(&inner);
        }
        found
    }

    /// Use a kiosk code up, making the device that typed it a kiosk. Gives the token for
    /// its cookie. `None` for a code that is wrong or already used. `now` is seconds since
    /// 1970.
    pub fn kiosk_set_up(&self, typed: &str, now: u64) -> Option<String> {
        let code = normalize_key(typed);
        if code.is_empty() {
            return None;
        }
        let mut inner = self.write();
        // Look at every code, so the time taken does not say which one matched.
        let mut found = None;
        for (index, kept) in inner.stored.kiosk_codes.iter().enumerate() {
            if same(kept, &code) {
                found = Some(index);
            }
        }
        let index = found?;
        inner.stored.kiosk_codes.remove(index);
        while inner.stored.kiosks.len() >= KIOSK_DEVICES_MAX {
            inner.stored.kiosks.remove(0);
        }
        inner.stored.last_kiosk += 1;
        let token = crate::auth::random_token();
        let number = inner.stored.last_kiosk;
        inner.stored.kiosks.push(KioskDevice {
            number,
            token: token.clone(),
            set_up_at: now,
            name: None,
        });
        Self::save(&inner);
        Some(token)
    }

    /// Name a kiosk, or take its name away with an empty one. The name is trimmed, and cut
    /// to [`KIOSK_NAME_MAX`] characters. Whether the kiosk was there.
    pub fn kiosk_rename(&self, number: u32, name: &str) -> bool {
        let name: String = name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(KIOSK_NAME_MAX)
            .collect();
        let mut inner = self.write();
        let Some(device) = inner
            .stored
            .kiosks
            .iter_mut()
            .find(|device| device.number == number)
        else {
            return false;
        };
        device.name = (!name.trim().is_empty()).then(|| name.trim().to_string());
        Self::save(&inner);
        true
    }

    /// What the kiosk whose cookie holds `token` is called: its name, or "Kiosk 2".
    pub fn kiosk_label(&self, token: &str) -> Option<String> {
        if token.is_empty() {
            return None;
        }
        self.read()
            .stored
            .kiosks
            .iter()
            .find(|device| same(&device.token, token))
            .map(|device| {
                device
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Kiosk {}", device.number))
            })
    }

    /// Remove a kiosk from the list. Its tablet stops being one. Whether it was there.
    pub fn kiosk_remove(&self, number: u32) -> bool {
        let mut inner = self.write();
        let before = inner.stored.kiosks.len();
        inner.stored.kiosks.retain(|device| device.number != number);
        let found = inner.stored.kiosks.len() != before;
        if found {
            Self::save(&inner);
        }
        found
    }

    /// Stop the kiosk whose cookie holds `token`. Whether there was one.
    pub fn kiosk_forget(&self, token: &str) -> bool {
        let mut inner = self.write();
        let before = inner.stored.kiosks.len();
        inner
            .stored
            .kiosks
            .retain(|device| !same(&device.token, token));
        let found = inner.stored.kiosks.len() != before;
        if found {
            Self::save(&inner);
        }
        found
    }

    /// Whether `token` is the cookie of a kiosk on the director's list.
    pub fn is_kiosk(&self, token: &str) -> bool {
        !token.is_empty()
            && self
                .read()
                .stored
                .kiosks
                .iter()
                .fold(false, |found, device| found | same(&device.token, token))
    }

    /// Prove who a pilot is at a kiosk, with their code. Gives the pilot and a pass.
    /// Unlike [`Self::sign_in`] it leaves the pilot's own device signed in, and nothing is
    /// written down.
    pub fn kiosk_in(
        &self,
        event: &EventId,
        pilots: &BTreeSet<PilotId>,
        typed: &str,
        now: Instant,
    ) -> Option<(PilotId, String)> {
        let mut inner = self.write();
        let pilot = Self::pilot_of(&inner, event, pilots, typed)?;
        inner
            .kiosk
            .retain(|_, pass| now.saturating_duration_since(pass.used) < KIOSK_IDLE);
        while inner.kiosk.len() >= KIOSK_MAX {
            let Some(oldest) = inner
                .kiosk
                .iter()
                .min_by_key(|(_, pass)| pass.used)
                .map(|(token, _)| token.clone())
            else {
                break;
            };
            inner.kiosk.remove(&oldest);
        }
        let token = crate::auth::random_token();
        inner.kiosk.insert(
            token.clone(),
            KioskPass {
                event: event.clone(),
                pilot: pilot.clone(),
                used: now,
            },
        );
        Some((pilot, token))
    }

    /// Who a kiosk pass belongs to, in `event`. Using a pass keeps it going. `None` for an
    /// unknown or ended pass, one that has sat unused too long, one for another event, or
    /// a pilot who has left the event.
    pub fn kiosk_who(
        &self,
        event: &EventId,
        pilots: &BTreeSet<PilotId>,
        token: &str,
        now: Instant,
    ) -> Option<PilotId> {
        let mut inner = self.write();
        let pass = inner.kiosk.get_mut(token)?;
        if now.saturating_duration_since(pass.used) >= KIOSK_IDLE {
            inner.kiosk.remove(token);
            return None;
        }
        if pass.event != *event || !pilots.contains(&pass.pilot) {
            return None;
        }
        pass.used = now;
        Some(pass.pilot.clone())
    }

    /// End a kiosk pass. Whether there was one.
    pub fn kiosk_end(&self, token: &str) -> bool {
        self.write().kiosk.remove(token).is_some()
    }

    /// Who a sign-in belongs to, in `event`. `None` for an unknown or ended sign-in, one
    /// for another event, or a pilot who has left the event.
    pub fn who(
        &self,
        event: &EventId,
        pilots: &BTreeSet<PilotId>,
        session: &str,
    ) -> Option<PilotId> {
        if session.is_empty() {
            return None;
        }
        let inner = self.read();
        inner
            .stored
            .events
            .get(event)?
            .iter()
            .find(|(pilot, entry)| {
                pilots.contains(*pilot)
                    && entry
                        .session
                        .as_deref()
                        .is_some_and(|kept| same(kept, session))
            })
            .map(|(pilot, _)| pilot.clone())
    }

    /// End a sign-in, whichever event it is for. Whether there was one.
    pub fn end(&self, session: &str) -> bool {
        if session.is_empty() {
            return false;
        }
        let mut inner = self.write();
        let mut ended = false;
        for entries in inner.stored.events.values_mut() {
            for entry in entries.values_mut() {
                if entry
                    .session
                    .as_deref()
                    .is_some_and(|kept| same(kept, session))
                {
                    entry.session = None;
                    ended = true;
                }
            }
        }
        if ended {
            Self::save(&inner);
        }
        ended
    }

    /// Drop everything kept for an event that is gone.
    pub fn forget(&self, event: &EventId) {
        let mut inner = self.write();
        inner.kiosk.retain(|_, pass| pass.event != *event);
        if inner.stored.events.remove(event).is_some() {
            Self::save(&inner);
        }
    }
}

/// The pilot sign-in a request carries, if it has one.
pub fn pilot_cookie(headers: &HeaderMap) -> Option<String> {
    cookie_named(headers, PILOT_COOKIE)
}

/// What the kiosk a request comes from is called. `None` for a device that is not one.
pub fn kiosk_label(registry: &EventRegistry, headers: &HeaderMap) -> Option<String> {
    cookie_named(headers, KIOSK_COOKIE)
        .and_then(|token| registry.pilot_access().kiosk_label(&token))
}

/// Whether a request comes from a device the director set up as a kiosk.
pub fn is_kiosk_device(registry: &EventRegistry, headers: &HeaderMap) -> bool {
    cookie_named(headers, KIOSK_COOKIE)
        .is_some_and(|token| registry.pilot_access().is_kiosk(&token))
}

fn cookie_named(headers: &HeaderMap, wanted: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, value)| *name == wanted && !value.is_empty())
        .map(|(_, value)| value.to_string())
}

fn cookie(value: &str, max_age: u32) -> String {
    format!("{PILOT_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict")
}

fn unknown_event(id: &EventId) -> ProtocolError {
    ProtocolError::new(
        ErrorCode::UnknownScope,
        format!("no event with id {:?}", id.0),
    )
}

/// Everyone in an event right now: the rosters, the fields and the lineups.
fn pilots_in(
    registry: &EventRegistry,
    event: &EventId,
) -> Result<BTreeSet<PilotId>, ProtocolError> {
    crate::control::with_event(registry, event, |session| {
        let view = EventView::build(session.state(), session.now);
        Ok(crate::spectate::mentioned(&view).pilots)
    })
}

/// The pilot a request is signed in as, for the event that is open. This is the one way
/// any route learns who a pilot is.
///
/// A request from a kiosk carries a pass in [`KIOSK_HEADER`], and then the pass alone
/// decides: a tablet whose pass has run out must not fall back to whoever once signed in
/// on it the ordinary way.
pub fn signed_in_pilot(registry: &EventRegistry, headers: &HeaderMap) -> Option<PublicPilot> {
    let event = registry.active()?.id;
    let id = if let Some(pass) = headers.get(KIOSK_HEADER) {
        // A pass is only any use on a device that is still a kiosk.
        if !is_kiosk_device(registry, headers) {
            return None;
        }
        let pass = pass.to_str().ok()?;
        let pilots = pilots_in(registry, &event).ok()?;
        registry
            .pilot_access()
            .kiosk_who(&event, &pilots, pass, Instant::now())?
    } else {
        let session = pilot_cookie(headers)?;
        let pilots = pilots_in(registry, &event).ok()?;
        registry.pilot_access().who(&event, &pilots, &session)?
    };
    let callsign = registry
        .pilots()
        .list()
        .into_iter()
        .find(|pilot| pilot.id == id)?
        .callsign;
    Some(PublicPilot { id, callsign })
}

/// `GET /events/{event_id}/access` — every pilot in the event with their sign-in code.
/// For the director only: the gate keeps visitors out of everything under `/events`.
pub async fn get_access(
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Result<Response, ProtocolError> {
    if registry.meta_of(&event_id).is_none() {
        return Err(unknown_event(&event_id));
    }
    let pilots = pilots_in(&registry, &event_id)?;
    Ok((
        // The codes are secrets: a browser must not keep this answer.
        [(CACHE_CONTROL, "no-store")],
        Json(PilotAccessView {
            pilots: registry.pilot_access().codes(&event_id, &pilots),
            address: registry.network().public_address(),
        }),
    )
        .into_response())
}

fn not_in_event() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::UnknownScope,
        "That pilot is not in this event, so they have no sign-in code.",
    )
}

/// `POST /events/{event_id}/access/{pilot_id}/reset` — give a pilot a new code. The old
/// one stops working and their device is signed out.
pub async fn post_reset(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, pilot_id)): Path<(EventId, PilotId)>,
) -> Result<Json<PilotCodeView>, ProtocolError> {
    if registry.meta_of(&event_id).is_none() {
        return Err(unknown_event(&event_id));
    }
    let pilots = pilots_in(&registry, &event_id)?;
    if !pilots.contains(&pilot_id) {
        return Err(not_in_event());
    }
    let access = registry.pilot_access();
    // A pilot who joined after the list was last read has no code yet.
    access.codes(&event_id, &pilots);
    access
        .reset(&event_id, &pilot_id)
        .map(Json)
        .ok_or_else(not_in_event)
}

/// `DELETE /events/{event_id}/access/{pilot_id}/session` — sign a pilot's device out.
/// Their code still works. Always `204`.
pub async fn delete_pilot_session(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((event_id, pilot_id)): Path<(EventId, PilotId)>,
) -> Result<StatusCode, ProtocolError> {
    if registry.meta_of(&event_id).is_none() {
        return Err(unknown_event(&event_id));
    }
    registry.pilot_access().sign_out(&event_id, &pilot_id);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /events/{event_id}/access/{pilot_id}/qr` — the pilot's code as a QR picture. It
/// holds a link to Grid on the network with the code in it, so scanning it signs in.
/// `409` while other devices cannot reach Grid: there is no address to put in it.
pub async fn get_qr(
    State(registry): State<EventRegistry>,
    Path((event_id, pilot_id)): Path<(EventId, PilotId)>,
) -> Result<Response, ProtocolError> {
    if registry.meta_of(&event_id).is_none() {
        return Err(unknown_event(&event_id));
    }
    let pilots = pilots_in(&registry, &event_id)?;
    let code = registry
        .pilot_access()
        .codes(&event_id, &pilots)
        .into_iter()
        .find(|entry| entry.pilot == pilot_id)
        .ok_or_else(not_in_event)?
        .code;
    let Some(address) = registry.network().public_address() else {
        return Err(ProtocolError::new(
            ErrorCode::Refused,
            "Other devices cannot reach Grid yet. Switch on Public, on the Network page, to get a QR code.",
        ));
    };
    let svg = qr_svg(&join_link(&address, &code)).map_err(|err| {
        ProtocolError::new(
            ErrorCode::Internal,
            format!("The QR code could not be drawn: {err}"),
        )
    })?;
    Ok((
        [
            (CONTENT_TYPE, "image/svg+xml"),
            // A new code must show at once.
            (CACHE_CONTROL, "no-store"),
            // An SVG is a document: it may draw, and nothing more.
            (
                CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
        ],
        svg,
    )
        .into_response())
}

/// The link a pilot's QR code holds: Grid's address on the network, with the code after
/// the `#`, where it is never sent to anyone but the page itself.
pub fn join_link(address: &str, code: &str) -> String {
    format!("{}/#/join/{code}", address.trim_end_matches('/'))
}

/// A QR code for `text`, as an SVG picture: dark squares on white, with the quiet border
/// a camera needs.
pub fn qr_svg(text: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|err| err.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .quiet_zone(true)
        .build())
}

/// The dark squares of a QR code drawn in Grid's colours: the console's own dark ground.
pub const QR_DARK: &str = "#0a0d12";
/// Its light ground: a pale tint of Grid's green. A camera needs dark squares on a light
/// ground, so the colours are this way round whatever the page behind looks like.
pub const QR_LIGHT: &str = "#dffcef";

/// A QR code for `text` in Grid's colours ([`QR_DARK`] on [`QR_LIGHT`]), as an SVG picture
/// with the quiet border a camera needs.
pub fn qr_svg_themed(text: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|err| err.to_string())?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .dark_color(qrcode::render::svg::Color(QR_DARK))
        .light_color(qrcode::render::svg::Color(QR_LIGHT))
        .quiet_zone(true)
        .build())
}

/// The address a phone should open to reach the public pages, for a caller who asked at
/// `host`.
///
/// The name the caller itself used comes first: it reached Grid that way, so a phone on the
/// same network can too, and on a computer with two networks it is the right one of the two.
/// A caller on the Grid computer (`localhost`) used a name no phone can, so for it, and for
/// a host that is not plainly a name and a port, the Public door's own address is given.
pub fn public_link(host: Option<&str>, public_address: Option<String>) -> Option<String> {
    let plain = |host: &&str| {
        !host.is_empty()
            && host.len() <= 255
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
            && !crate::access::is_loopback_host(host)
    };
    match host.map(str::trim).filter(plain) {
        Some(host) => Some(format!("http://{host}/")),
        None => public_address,
    }
}

/// `GET /public/qr` — a QR code that opens Grid's public pages, in Grid's colours. A kiosk
/// shows it, so a pilot with a phone can carry on there. It holds an address and nothing
/// else: no code, and nothing about who is asking.
pub async fn get_public_qr(
    State(registry): State<EventRegistry>,
    headers: axum::http::HeaderMap,
) -> Result<Response, ProtocolError> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|value| value.to_str().ok());
    let Some(address) = public_link(host, registry.network().public_address()) else {
        return Err(ProtocolError::new(
            ErrorCode::Refused,
            "Other devices cannot reach Grid yet. Switch on Public, on the Network page, to get a QR code.",
        ));
    };
    let svg = qr_svg_themed(&address).map_err(|err| {
        ProtocolError::new(
            ErrorCode::Internal,
            format!("The QR code could not be drawn: {err}"),
        )
    })?;
    Ok((
        [
            (CONTENT_TYPE, "image/svg+xml"),
            // The address changes when the computer changes network.
            (CACHE_CONTROL, "no-store"),
            // An SVG is a document: it may draw, and nothing more.
            (
                CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
        ],
        svg,
    )
        .into_response())
}

/// The address wrong codes are counted against: the caller's, or one shared count when it
/// is not known.
fn guesser(peer: Option<ConnectInfo<SocketAddr>>) -> IpAddr {
    peer.map_or(IpAddr::from([0, 0, 0, 0]), |ConnectInfo(peer)| {
        peer.ip().to_canonical()
    })
}

/// `POST /public/pilot/session` — sign in as a pilot with their code, for the open event.
///
/// A right code answers the pilot and sets a cookie. It replaces any earlier sign-in for
/// that pilot: one device at a time. A wrong code answers `401`. After too many wrong
/// codes from one device, `429` until a minute has passed.
pub async fn post_session(
    State(registry): State<EventRegistry>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<PilotSignInRequest>,
) -> Result<Response, ProtocolError> {
    let (pilot, session) = with_code(&registry, peer, |access, event, pilots| {
        access.sign_in(event, pilots, &body.code)
    })?;
    Ok((
        [(SET_COOKIE, cookie(&session, COOKIE_SECONDS))],
        Json(pilot),
    )
        .into_response())
}

/// Check a code for the open event, counting wrong ones by address. `check` gives the
/// pilot and whatever token the right code earns.
fn with_code(
    registry: &EventRegistry,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    check: impl FnOnce(&PilotAccess, &EventId, &BTreeSet<PilotId>) -> Option<(PilotId, String)>,
) -> Result<(PublicPilot, String), ProtocolError> {
    let ip = guesser(peer.map(|Extension(peer)| peer));
    let guesses = registry.pilot_guesses();
    let now = Instant::now();
    if guesses.blocked(ip, now) {
        return Err(ProtocolError::new(
            ErrorCode::TooManyRequests,
            "Too many wrong codes from this device. Wait a minute and try again.",
        ));
    }
    let Some(event) = registry.active().map(|event| event.id) else {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            "No event is open.",
        ));
    };
    let pilots = pilots_in(registry, &event)?;
    let Some((id, token)) = check(&registry.pilot_access(), &event, &pilots) else {
        guesses.failed(ip, now);
        return Err(ProtocolError::new(
            ErrorCode::Unauthorized,
            "That code is not right. Check it with the director.",
        ));
    };
    guesses.cleared(ip);
    let callsign = registry
        .pilots()
        .list()
        .into_iter()
        .find(|pilot| pilot.id == id)
        .map(|pilot| pilot.callsign)
        .unwrap_or_default();
    Ok((PublicPilot { id, callsign }, token))
}

/// What a kiosk is told when a pilot's code is right.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct KioskPassView {
    pub pilot: PublicPilot,
    /// What the kiosk sends in the `x-gridfpv-kiosk` header to act as this pilot.
    pub pass: String,
    /// How many seconds the pass lasts after it was last used.
    pub idle_seconds: u32,
}

/// `POST /public/pilot/kiosk` — prove who a pilot is at a kiosk, with their code.
///
/// A right code answers the pilot and a pass. It sets no cookie and leaves the pilot's own
/// phone signed in. Wrong codes are counted and answered as on [`post_session`].
pub async fn post_kiosk(
    State(registry): State<EventRegistry>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<PilotSignInRequest>,
) -> Result<Response, ProtocolError> {
    if !is_kiosk_device(&registry, &headers) {
        return Err(not_a_kiosk());
    }
    let (pilot, pass) = with_code(&registry, peer, |access, event, pilots| {
        access.kiosk_in(event, pilots, &body.code, Instant::now())
    })?;
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(KioskPassView {
            pilot,
            pass,
            idle_seconds: KIOSK_IDLE.as_secs() as u32,
        }),
    )
        .into_response())
}

fn not_a_kiosk() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::Forbidden,
        "This device is not set up as a kiosk. The director sets a kiosk up.",
    )
}

/// One kiosk on the director's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct KioskDeviceView {
    /// "Kiosk 2".
    pub number: u32,
    /// When it was set up, in seconds since 1970.
    #[ts(type = "number")]
    pub set_up_at: u64,
    /// What the director calls it. Absent until it is given a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
}

/// `PUT /kiosk/devices/{number}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct KioskNameRequest {
    /// The name, or an empty one to take it away.
    pub name: String,
}

/// `GET /kiosk`: what the director sees about kiosks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct KioskView {
    /// Codes that no tablet has used yet. Each sets up one kiosk, once.
    pub codes: Vec<String>,
    /// The tablets set up as kiosks, oldest first.
    pub kiosks: Vec<KioskDeviceView>,
}

/// `POST /public/pilot/kiosk/device`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct KioskDeviceRequest {
    /// A kiosk code the director made.
    pub code: String,
}

fn kiosk_answer(registry: &EventRegistry) -> Response {
    (
        // The codes are secrets: a browser must not keep this answer.
        [(CACHE_CONTROL, "no-store")],
        Json(registry.pilot_access().kiosk_view()),
    )
        .into_response()
}

/// `GET /kiosk` — the kiosk codes and the kiosks, for the director only.
pub async fn get_kiosks(State(registry): State<EventRegistry>) -> Response {
    kiosk_answer(&registry)
}

/// `POST /kiosk/codes` — make one more kiosk code, for the director only.
pub async fn post_kiosk_code(
    State(registry): State<EventRegistry>,
) -> Result<Response, ProtocolError> {
    if registry.pilot_access().kiosk_new_code().is_none() {
        return Err(ProtocolError::new(
            ErrorCode::Refused,
            "There are already a lot of kiosk codes waiting. Use one or cancel one first.",
        ));
    }
    Ok(kiosk_answer(&registry))
}

/// `DELETE /kiosk/codes/{code}` — cancel a kiosk code that has not been used.
pub async fn delete_kiosk_code(
    State(registry): State<EventRegistry>,
    Path(code): Path<String>,
) -> Response {
    registry.pilot_access().kiosk_cancel_code(&code);
    kiosk_answer(&registry)
}

/// `PUT /kiosk/devices/{number}` — name a kiosk, so two tablets can be told apart.
pub async fn put_kiosk_name(
    State(registry): State<EventRegistry>,
    Path(number): Path<u32>,
    Json(body): Json<KioskNameRequest>,
) -> Result<Response, ProtocolError> {
    if !registry.pilot_access().kiosk_rename(number, &body.name) {
        return Err(ProtocolError::new(
            ErrorCode::UnknownScope,
            "That kiosk is not on the list any more.",
        ));
    }
    Ok(kiosk_answer(&registry))
}

/// `DELETE /kiosk/devices/{number}` — remove a kiosk. Its tablet stops being one.
pub async fn delete_kiosk_number(
    State(registry): State<EventRegistry>,
    Path(number): Path<u32>,
) -> Response {
    registry.pilot_access().kiosk_remove(number);
    kiosk_answer(&registry)
}

/// `POST /public/pilot/kiosk/device` — make this device a kiosk, with a kiosk code the
/// director typed on it. A right code is used up, sets a cookie and answers `204`. A wrong
/// or used one answers `401`, and wrong codes are counted like wrong pilot codes.
pub async fn post_kiosk_device(
    State(registry): State<EventRegistry>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Json(body): Json<KioskDeviceRequest>,
) -> Result<Response, ProtocolError> {
    let ip = guesser(peer.map(|Extension(peer)| peer));
    let guesses = registry.pilot_guesses();
    let now = Instant::now();
    if guesses.blocked(ip, now) {
        return Err(ProtocolError::new(
            ErrorCode::TooManyRequests,
            "Too many wrong tries from this device. Wait a minute and try again.",
        ));
    }
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let Some(token) = registry.pilot_access().kiosk_set_up(&body.code, wall) else {
        guesses.failed(ip, now);
        return Err(ProtocolError::new(
            ErrorCode::Unauthorized,
            "That kiosk code is not right, or it has been used. Make a new one on the Network page.",
        ));
    };
    guesses.cleared(ip);
    Ok((
        StatusCode::NO_CONTENT,
        [(
            SET_COOKIE,
            format!(
                "{KIOSK_COOKIE}={token}; Path=/; Max-Age={KIOSK_COOKIE_SECONDS}; HttpOnly; SameSite=Strict"
            ),
        )],
    )
        .into_response())
}

/// `DELETE /public/pilot/kiosk/device` — this device stops being a kiosk, and comes off the
/// director's list. Always `204`.
pub async fn delete_kiosk_device(
    State(registry): State<EventRegistry>,
    headers: HeaderMap,
) -> Response {
    if let Some(token) = cookie_named(&headers, KIOSK_COOKIE) {
        registry.pilot_access().kiosk_forget(&token);
    }
    (
        StatusCode::NO_CONTENT,
        [(
            SET_COOKIE,
            format!("{KIOSK_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict"),
        )],
    )
        .into_response()
}

/// `DELETE /public/pilot/kiosk` — the pilot is done at the kiosk. Always `204`.
pub async fn delete_kiosk(State(registry): State<EventRegistry>, headers: HeaderMap) -> Response {
    if let Some(pass) = headers.get(KIOSK_HEADER).and_then(|v| v.to_str().ok()) {
        registry.pilot_access().kiosk_end(pass);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `DELETE /public/pilot/session` — sign out. Always `204`, signed in or not.
pub async fn delete_session(State(registry): State<EventRegistry>, headers: HeaderMap) -> Response {
    if let Some(session) = pilot_cookie(&headers) {
        registry.pilot_access().end(&session);
    }
    (StatusCode::NO_CONTENT, [(SET_COOKIE, cookie("", 0))]).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: &str) -> EventId {
        EventId(id.to_string())
    }
    fn pilot(id: &str) -> PilotId {
        PilotId(id.to_string())
    }
    fn field(ids: &[&str]) -> BTreeSet<PilotId> {
        ids.iter().map(|id| pilot(id)).collect()
    }
    fn code_of(
        access: &PilotAccess,
        event_id: &str,
        pilots: &BTreeSet<PilotId>,
        id: &str,
    ) -> String {
        access
            .codes(&event(event_id), pilots)
            .into_iter()
            .find(|entry| entry.pilot == pilot(id))
            .unwrap()
            .code
    }

    // ── The kiosk ────────────────────────────────────────────────────────────

    /// An event with Ace and Bee in it, their codes made.
    fn kiosk_rig() -> (PilotAccess, BTreeSet<PilotId>, String, String) {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace", "bee"]);
        let ace = code_of(&access, "fri", &pilots, "ace");
        let bee = code_of(&access, "fri", &pilots, "bee");
        (access, pilots, ace, bee)
    }

    #[test]
    fn a_kiosk_code_is_used_up_by_the_first_device_that_types_it() {
        let access = PilotAccess::new(None);
        assert!(access.kiosk_view().codes.is_empty());
        let code = access.kiosk_new_code().unwrap();
        assert_eq!(code.len(), KIOSK_KEY_LENGTH);
        assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        // Typed loosely, it still works.
        let loose = format!(" {}-{} ", code[..4].to_lowercase(), &code[4..]);
        let token = access.kiosk_set_up(&loose, 1000).unwrap();
        assert!(access.is_kiosk(&token));
        assert!(access.kiosk_set_up(&code, 1001).is_none());
        let view = access.kiosk_view();
        assert!(view.codes.is_empty());
        assert_eq!(
            view.kiosks,
            [KioskDeviceView {
                number: 1,
                set_up_at: 1000,
                name: None
            }]
        );
        // The code is not the cookie, and nothing but the token makes a kiosk.
        for not in ["", code.as_str(), "x", &token[1..], &format!("{token}x")] {
            assert!(!access.is_kiosk(not), "{not:?}");
        }
        assert!(access.kiosk_set_up("", 1002).is_none());
        assert!(access.kiosk_set_up("   ", 1002).is_none());
    }

    #[test]
    fn kiosk_numbers_are_never_used_twice_and_removing_one_stops_only_it() {
        let access = PilotAccess::new(None);
        let mut tokens = Vec::new();
        for n in 0..3 {
            let code = access.kiosk_new_code().unwrap();
            tokens.push(access.kiosk_set_up(&code, n).unwrap());
        }
        assert!(access.kiosk_remove(2));
        assert!(!access.kiosk_remove(2));
        assert!(!access.kiosk_remove(99));
        assert_eq!(
            [
                access.is_kiosk(&tokens[0]),
                access.is_kiosk(&tokens[1]),
                access.is_kiosk(&tokens[2])
            ],
            [true, false, true]
        );
        // The tablet itself can stop being one.
        assert!(access.kiosk_forget(&tokens[0]));
        assert!(!access.kiosk_forget(&tokens[0]));
        assert!(!access.kiosk_forget(""));
        let code = access.kiosk_new_code().unwrap();
        access.kiosk_set_up(&code, 9).unwrap();
        let numbers: Vec<u32> = access
            .kiosk_view()
            .kiosks
            .iter()
            .map(|k| k.number)
            .collect();
        assert_eq!(numbers, [3, 4]);
    }

    #[test]
    fn a_kiosk_can_be_named_and_the_name_is_tidied() {
        let access = PilotAccess::new(None);
        let code = access.kiosk_new_code().unwrap();
        let token = access.kiosk_set_up(&code, 1).unwrap();
        assert_eq!(access.kiosk_label(&token).as_deref(), Some("Kiosk 1"));
        assert_eq!(access.kiosk_view().kiosks[0].name, None);
        assert!(access.kiosk_rename(1, "  By   the\tgate\n "));
        assert_eq!(
            access.kiosk_view().kiosks[0].name.as_deref(),
            Some("By the gate")
        );
        assert_eq!(access.kiosk_label(&token).as_deref(), Some("By the gate"));
        // Cut at the limit, by characters and not bytes.
        assert!(access.kiosk_rename(1, &"é".repeat(100)));
        assert_eq!(
            access.kiosk_view().kiosks[0]
                .name
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            KIOSK_NAME_MAX
        );
        // An empty name, or one of spaces, takes the name away.
        for empty in ["", "   ", "\n\t"] {
            assert!(access.kiosk_rename(1, "x"));
            assert!(access.kiosk_rename(1, empty));
            assert_eq!(access.kiosk_view().kiosks[0].name, None, "{empty:?}");
            assert_eq!(access.kiosk_label(&token).as_deref(), Some("Kiosk 1"));
        }
        // A kiosk that is not there, and a token that is not a kiosk's.
        assert!(!access.kiosk_rename(2, "x"));
        assert_eq!(access.kiosk_label(""), None);
        assert_eq!(access.kiosk_label("nope"), None);
        // The name has no power: it is not the token and does not make a kiosk.
        access.kiosk_rename(1, &token);
        assert!(access.is_kiosk(&token));
        access.kiosk_rename(1, "Front desk");
        assert!(!access.is_kiosk("Front desk"));
    }

    #[test]
    fn a_kiosk_name_is_kept_and_an_old_file_without_one_loads() {
        let dir =
            std::env::temp_dir().join(format!("gridfpv-kiosk-{}", crate::auth::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ACCESS_FILE),
            r#"{"kiosks":[{"number":4,"token":"tok-en","set_up_at":9}],"last_kiosk":4}"#,
        )
        .unwrap();
        let access = PilotAccess::new(Some(dir.clone()));
        assert!(access.is_kiosk("tok-en"));
        assert_eq!(access.kiosk_label("tok-en").as_deref(), Some("Kiosk 4"));
        assert!(access.kiosk_rename(4, "Pit lane"));
        let reopened = PilotAccess::new(Some(dir.clone()));
        assert_eq!(reopened.kiosk_label("tok-en").as_deref(), Some("Pit lane"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn only_so_many_codes_wait_and_only_so_many_kiosks_are_kept() {
        let access = PilotAccess::new(None);
        let mut codes = std::collections::BTreeSet::new();
        for _ in 0..KIOSK_CODES_MAX {
            assert!(
                codes.insert(access.kiosk_new_code().unwrap()),
                "a code came up twice"
            );
        }
        assert!(access.kiosk_new_code().is_none());
        assert!(access.kiosk_cancel_code(&codes.first().unwrap().to_lowercase()));
        assert!(!access.kiosk_cancel_code("NOPE"));
        assert!(access.kiosk_new_code().is_some());
        // One more kiosk than the list holds: the oldest makes room.
        let mut first = None;
        for n in 0..=KIOSK_DEVICES_MAX {
            let code = match access.kiosk_new_code() {
                Some(code) => code,
                None => access.kiosk_view().codes[0].clone(),
            };
            let token = access.kiosk_set_up(&code, n as u64).unwrap();
            first.get_or_insert(token);
        }
        let view = access.kiosk_view();
        assert_eq!(view.kiosks.len(), KIOSK_DEVICES_MAX);
        assert_eq!(view.kiosks[0].number, 2);
        assert!(!access.is_kiosk(&first.unwrap()));
    }

    #[test]
    fn an_old_file_with_no_kiosks_still_loads_and_kiosks_are_saved() {
        let dir =
            std::env::temp_dir().join(format!("gridfpv-kiosk-{}", crate::auth::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        // What an earlier build wrote: codes only, and a key this build does not know.
        std::fs::write(
            dir.join(ACCESS_FILE),
            r#"{"events":{"fri":{"ace":{"code":"ABC234"}}},"kiosk_key":"OLDOLDOL"}"#,
        )
        .unwrap();
        let access = PilotAccess::new(Some(dir.clone()));
        assert_eq!(code_of(&access, "fri", &field(&["ace"]), "ace"), "ABC234");
        assert!(access.kiosk_view().codes.is_empty());
        assert!(!access.is_kiosk("OLDOLDOL"));
        let used = access.kiosk_new_code().unwrap();
        let waiting = access.kiosk_new_code().unwrap();
        let token = access.kiosk_set_up(&used, 5).unwrap();
        let reopened = PilotAccess::new(Some(dir.clone()));
        assert_eq!(reopened.kiosk_view().codes, [waiting]);
        assert!(reopened.is_kiosk(&token));
        assert_eq!(code_of(&reopened, "fri", &field(&["ace"]), "ace"), "ABC234");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_scanned_link_is_read_as_its_code() {
        assert_eq!(
            normalize("http://192.168.1.5:8081/#/join/abc-234").as_deref(),
            Some("ABC234")
        );
        assert_eq!(normalize("#/join/ABC234").as_deref(), Some("ABC234"));
        // The last one counts, and what follows it must be a code and nothing more.
        assert_eq!(
            normalize("http://x/join/ZZZZZZ/#/join/ABC234").as_deref(),
            Some("ABC234")
        );
        assert_eq!(normalize("http://x/#/join/"), None);
        assert_eq!(normalize("http://x/#/join/ABC234/more"), None);
        assert_eq!(normalize("http://x/#/join/ABC234?x=1"), None);
        assert_eq!(normalize("http://x/#/other/ABC234"), None);
    }

    #[test]
    fn a_kiosk_pass_is_its_pilots_and_using_it_keeps_it_going() {
        let (access, pilots, ace, bee) = kiosk_rig();
        let start = Instant::now();
        let (who, pass) = access
            .kiosk_in(&event("fri"), &pilots, &ace, start)
            .unwrap();
        assert_eq!(who, pilot("ace"));
        let (_, other) = access
            .kiosk_in(&event("fri"), &pilots, &bee, start)
            .unwrap();
        assert_ne!(pass, other);
        // Used every 100 seconds, it lasts well past the 120 it would sit unused.
        let mut now = start;
        for _ in 0..10 {
            now += Duration::from_secs(100);
            assert_eq!(
                access.kiosk_who(&event("fri"), &pilots, &pass, now),
                Some(pilot("ace"))
            );
        }
        // Bee's was never used, and is gone.
        assert_eq!(access.kiosk_who(&event("fri"), &pilots, &other, now), None);
    }

    #[test]
    fn a_kiosk_pass_stops_exactly_when_it_has_sat_unused_too_long() {
        let (access, pilots, ace, _) = kiosk_rig();
        let start = Instant::now();
        let (_, pass) = access
            .kiosk_in(&event("fri"), &pilots, &ace, start)
            .unwrap();
        let just_before = start + KIOSK_IDLE - Duration::from_millis(1);
        assert!(
            access
                .kiosk_who(&event("fri"), &pilots, &pass, just_before)
                .is_some()
        );
        // That use moved the clock on.
        let at = just_before + KIOSK_IDLE;
        assert_eq!(access.kiosk_who(&event("fri"), &pilots, &pass, at), None);
        // Once it has stopped it stays stopped, even asked "earlier".
        assert_eq!(
            access.kiosk_who(&event("fri"), &pilots, &pass, just_before),
            None
        );
    }

    #[test]
    fn a_clock_that_goes_backwards_does_not_break_a_kiosk_pass() {
        let (access, pilots, ace, _) = kiosk_rig();
        let later = Instant::now() + Duration::from_secs(1000);
        let (_, pass) = access
            .kiosk_in(&event("fri"), &pilots, &ace, later)
            .unwrap();
        let earlier = later - Duration::from_secs(900);
        assert!(
            access
                .kiosk_who(&event("fri"), &pilots, &pass, earlier)
                .is_some()
        );
    }

    #[test]
    fn a_kiosk_pass_does_not_touch_the_pilots_own_sign_in_or_the_file() {
        let dir =
            std::env::temp_dir().join(format!("gridfpv-kiosk-{}", crate::auth::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let access = PilotAccess::new(Some(dir.clone()));
        let pilots = field(&["ace"]);
        let code = code_of(&access, "fri", &pilots, "ace");
        let (_, session) = access.sign_in(&event("fri"), &pilots, &code).unwrap();
        let before = std::fs::read_to_string(dir.join(ACCESS_FILE)).unwrap();
        let now = Instant::now();
        let (_, pass) = access.kiosk_in(&event("fri"), &pilots, &code, now).unwrap();
        assert_eq!(
            access.who(&event("fri"), &pilots, &session),
            Some(pilot("ace"))
        );
        assert!(access.codes(&event("fri"), &pilots)[0].signed_in);
        assert_eq!(
            std::fs::read_to_string(dir.join(ACCESS_FILE)).unwrap(),
            before
        );
        assert!(!before.contains(&pass));
        // A pass is not a sign-in, and a sign-in is not a pass.
        assert_eq!(access.who(&event("fri"), &pilots, &pass), None);
        assert_eq!(
            access.kiosk_who(&event("fri"), &pilots, &session, now),
            None
        );
        // Opened again, the sign-in is there and the pass is not.
        let reopened = PilotAccess::new(Some(dir.clone()));
        assert!(reopened.who(&event("fri"), &pilots, &session).is_some());
        assert_eq!(reopened.kiosk_who(&event("fri"), &pilots, &pass, now), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_kiosk_pass_ends_with_done_a_new_code_a_sign_out_or_the_event() {
        let (access, pilots, ace, bee) = kiosk_rig();
        let now = Instant::now();
        let fri = event("fri");
        let pass = |code: &str| access.kiosk_in(&fri, &pilots, code, now).unwrap().1;
        let works = |pass: &str| access.kiosk_who(&fri, &pilots, pass, now).is_some();

        let done = pass(&ace);
        assert!(access.kiosk_end(&done));
        assert!(!access.kiosk_end(&done));
        assert!(!works(&done));

        let (a, b) = (pass(&ace), pass(&bee));
        access.reset(&fri, &pilot("ace")).unwrap();
        assert!(!works(&a));
        assert!(works(&b));
        // The old code is no good at the kiosk either.
        assert!(access.kiosk_in(&fri, &pilots, &ace, now).is_none());

        // The director's sign-out ends it, though no device was signed in.
        assert!(!access.sign_out(&fri, &pilot("bee")));
        assert!(!works(&b));

        let b = pass(&bee);
        access.forget(&fri);
        assert!(!works(&b));
    }

    #[test]
    fn a_kiosk_pass_is_for_its_event_and_a_pilot_still_in_it() {
        let (access, pilots, ace, _) = kiosk_rig();
        let now = Instant::now();
        let (_, pass) = access.kiosk_in(&event("fri"), &pilots, &ace, now).unwrap();
        assert_eq!(access.kiosk_who(&event("sat"), &pilots, &pass, now), None);
        assert_eq!(
            access.kiosk_who(&event("fri"), &field(&["bee"]), &pass, now),
            None
        );
        // Neither of those ended it: back in the event, it works.
        assert!(
            access
                .kiosk_who(&event("fri"), &pilots, &pass, now)
                .is_some()
        );
        // A code does nothing in another event, or for a pilot who has left.
        assert!(access.kiosk_in(&event("sat"), &pilots, &ace, now).is_none());
        assert!(
            access
                .kiosk_in(&event("fri"), &field(&["bee"]), &ace, now)
                .is_none()
        );
        assert!(access.kiosk_in(&event("fri"), &pilots, "", now).is_none());
    }

    #[test]
    fn only_so_many_kiosk_passes_are_kept_and_the_oldest_goes_first() {
        let (access, pilots, ace, _) = kiosk_rig();
        let start = Instant::now();
        let fri = event("fri");
        let mut passes = Vec::new();
        for n in 0..(KIOSK_MAX as u64 + 3) {
            let at = start + Duration::from_millis(n);
            passes.push(access.kiosk_in(&fri, &pilots, &ace, at).unwrap().1);
        }
        let now = start + Duration::from_secs(1);
        let alive: Vec<bool> = passes
            .iter()
            .map(|pass| access.kiosk_who(&fri, &pilots, pass, now).is_some())
            .collect();
        assert_eq!(alive.iter().filter(|on| **on).count(), KIOSK_MAX);
        assert_eq!(alive[..3], [false, false, false]);
        // Passes that have run out make room without pushing a live one out.
        let much_later = now + KIOSK_IDLE * 2;
        let fresh = access.kiosk_in(&fri, &pilots, &ace, much_later).unwrap().1;
        assert!(
            access
                .kiosk_who(&fri, &pilots, &fresh, much_later)
                .is_some()
        );
        assert_eq!(access.read().kiosk.len(), 1);
    }

    #[test]
    fn a_code_is_six_characters_no_camera_or_eye_confuses() {
        for _ in 0..500 {
            let code = random_code();
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)), "{code}");
            for confusing in ['0', 'O', '1', 'I'] {
                assert!(!code.contains(confusing), "{code}");
            }
        }
        // Every character of the alphabet is used, and none twice.
        let unique: BTreeSet<u8> = CODE_ALPHABET.iter().copied().collect();
        assert_eq!(unique.len(), 32);
    }

    #[test]
    fn codes_are_not_the_same_every_time() {
        let drawn: BTreeSet<String> = (0..200).map(|_| random_code()).collect();
        assert!(drawn.len() > 195, "{} different codes in 200", drawn.len());
    }

    #[test]
    fn a_typed_code_is_tidied_before_it_is_checked() {
        for typed in [
            "ABC234",
            "abc234",
            " abc234 ",
            "ABC-234",
            "abc 234",
            "a b c 2 3 4",
            "ABC\t234\n",
        ] {
            assert_eq!(normalize(typed).as_deref(), Some("ABC234"), "{typed:?}");
        }
    }

    #[test]
    fn what_is_not_a_code_is_refused_before_it_is_looked_up() {
        for typed in [
            "",
            " ",
            "ABC23",
            "ABC2345",
            "ABC23O",
            "ABC230",
            "ABC231",
            "ABC23I",
            "ABC23!",
            "ABC23é",
            "ＡＢＣ２３４",
            "ABC_234",
            "ABC.234",
            "------",
            "ABC234ABC234",
        ] {
            assert_eq!(normalize(typed), None, "{typed:?}");
        }
    }

    #[test]
    fn two_codes_are_compared_whole() {
        assert!(same("ABC234", "ABC234"));
        assert!(!same("ABC234", "ABC235"));
        assert!(!same("ABC234", "BBC234"));
        assert!(!same("ABC234", "ABC23"));
        assert!(!same("", "ABC234"));
        assert!(same("", ""));
    }

    #[test]
    fn every_pilot_in_the_event_gets_a_code_of_their_own_and_keeps_it() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace", "bee", "cat"]);
        let first = access.codes(&event("night"), &pilots);
        assert_eq!(first.len(), 3);
        let codes: BTreeSet<&str> = first.iter().map(|entry| entry.code.as_str()).collect();
        assert_eq!(codes.len(), 3, "no two pilots share a code");
        assert!(first.iter().all(|entry| !entry.signed_in));
        // Asking again changes nothing.
        assert_eq!(access.codes(&event("night"), &pilots), first);
    }

    #[test]
    fn many_pilots_never_share_a_code() {
        let access = PilotAccess::new(None);
        let ids: Vec<String> = (0..400).map(|n| format!("pilot-{n}")).collect();
        let pilots: BTreeSet<PilotId> = ids.iter().map(|id| pilot(id)).collect();
        let codes: BTreeSet<String> = access
            .codes(&event("big"), &pilots)
            .into_iter()
            .map(|entry| entry.code)
            .collect();
        assert_eq!(codes.len(), 400);
    }

    #[test]
    fn a_right_code_signs_the_pilot_in_however_it_is_typed() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace", "bee"]);
        let code = code_of(&access, "night", &pilots, "ace");
        let typed = format!(" {}-{} ", code[..3].to_lowercase(), &code[3..]);
        let (who, session) = access.sign_in(&event("night"), &pilots, &typed).unwrap();
        assert_eq!(who, pilot("ace"));
        assert!(session.len() >= 32);
        assert_eq!(
            access.who(&event("night"), &pilots, &session),
            Some(pilot("ace"))
        );
        let listed = access.codes(&event("night"), &pilots);
        assert!(
            listed
                .iter()
                .find(|e| e.pilot == pilot("ace"))
                .unwrap()
                .signed_in
        );
        assert!(
            !listed
                .iter()
                .find(|e| e.pilot == pilot("bee"))
                .unwrap()
                .signed_in
        );
    }

    #[test]
    fn a_wrong_code_signs_nobody_in() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        let code = code_of(&access, "night", &pilots, "ace");
        let mut wrong = code.clone().into_bytes();
        wrong[0] = if wrong[0] == b'A' { b'B' } else { b'A' };
        let wrong = String::from_utf8(wrong).unwrap();
        for typed in [
            wrong.as_str(),
            "",
            "AAAAAA",
            &code[..5],
            &format!("{code}A"),
        ] {
            if typed == code {
                continue;
            }
            assert_eq!(
                access.sign_in(&event("night"), &pilots, typed),
                None,
                "{typed:?}"
            );
        }
        assert!(!access.codes(&event("night"), &pilots)[0].signed_in);
    }

    #[test]
    fn nobody_has_a_code_until_the_director_has_looked() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        for typed in ["AAAAAA", "ABC234", ""] {
            assert_eq!(access.sign_in(&event("night"), &pilots, typed), None);
        }
    }

    #[test]
    fn a_second_sign_in_replaces_the_first() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        let code = code_of(&access, "night", &pilots, "ace");
        let (_, phone) = access.sign_in(&event("night"), &pilots, &code).unwrap();
        let (_, tablet) = access.sign_in(&event("night"), &pilots, &code).unwrap();
        assert_ne!(phone, tablet);
        assert_eq!(
            access.who(&event("night"), &pilots, &phone),
            None,
            "the first is signed out"
        );
        assert_eq!(
            access.who(&event("night"), &pilots, &tablet),
            Some(pilot("ace"))
        );
    }

    #[test]
    fn one_pilot_signing_in_does_not_touch_another() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace", "bee"]);
        let ace = code_of(&access, "night", &pilots, "ace");
        let bee = code_of(&access, "night", &pilots, "bee");
        let (_, ace_phone) = access.sign_in(&event("night"), &pilots, &ace).unwrap();
        let (_, bee_phone) = access.sign_in(&event("night"), &pilots, &bee).unwrap();
        assert_eq!(
            access.who(&event("night"), &pilots, &ace_phone),
            Some(pilot("ace"))
        );
        assert_eq!(
            access.who(&event("night"), &pilots, &bee_phone),
            Some(pilot("bee"))
        );
    }

    #[test]
    fn a_code_and_a_sign_in_are_for_one_event_only() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        let friday = code_of(&access, "friday", &pilots, "ace");
        let sunday = code_of(&access, "sunday", &pilots, "ace");
        assert_ne!(
            friday, sunday,
            "the same pilot has another code at another event"
        );
        assert_eq!(access.sign_in(&event("sunday"), &pilots, &friday), None);
        let (_, session) = access.sign_in(&event("friday"), &pilots, &friday).unwrap();
        assert_eq!(access.who(&event("sunday"), &pilots, &session), None);
        assert_eq!(
            access.who(&event("friday"), &pilots, &session),
            Some(pilot("ace"))
        );
        // Signing in at one event does not sign the pilot out of the other.
        let (_, other) = access.sign_in(&event("sunday"), &pilots, &sunday).unwrap();
        assert_eq!(
            access.who(&event("friday"), &pilots, &session),
            Some(pilot("ace"))
        );
        assert_eq!(
            access.who(&event("sunday"), &pilots, &other),
            Some(pilot("ace"))
        );
    }

    #[test]
    fn a_pilot_who_leaves_the_event_is_signed_out_and_their_code_stops_working() {
        let access = PilotAccess::new(None);
        let both = field(&["ace", "bee"]);
        let code = code_of(&access, "night", &both, "ace");
        let (_, session) = access.sign_in(&event("night"), &both, &code).unwrap();
        // Ace is taken out of the event. Even before the director looks at the codes again,
        // neither the sign-in nor the code counts.
        let only_bee = field(&["bee"]);
        assert_eq!(access.who(&event("night"), &only_bee, &session), None);
        assert_eq!(access.sign_in(&event("night"), &only_bee, &code), None);
        // Once the director looks, the code is gone for good.
        assert_eq!(access.codes(&event("night"), &only_bee).len(), 1);
        // Back in the event: a new code, and the old sign-in stays dead.
        let again = code_of(&access, "night", &both, "ace");
        assert_ne!(again, code);
        assert_eq!(access.who(&event("night"), &both, &session), None);
    }

    #[test]
    fn a_new_code_ends_the_old_one_and_the_sign_in() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace", "bee"]);
        let old = code_of(&access, "night", &pilots, "ace");
        let bee = code_of(&access, "night", &pilots, "bee");
        let (_, session) = access.sign_in(&event("night"), &pilots, &old).unwrap();
        let fresh = access.reset(&event("night"), &pilot("ace")).unwrap();
        assert_ne!(fresh.code, old);
        assert!(!fresh.signed_in);
        assert_eq!(access.sign_in(&event("night"), &pilots, &old), None);
        assert_eq!(access.who(&event("night"), &pilots, &session), None);
        assert!(
            access
                .sign_in(&event("night"), &pilots, &fresh.code)
                .is_some()
        );
        // The other pilot's code is as it was.
        assert_eq!(code_of(&access, "night", &pilots, "bee"), bee);
        // A pilot with no code, or an event with none, has nothing to reset.
        assert_eq!(access.reset(&event("night"), &pilot("nobody")), None);
        assert_eq!(access.reset(&event("other"), &pilot("ace")), None);
    }

    #[test]
    fn the_director_can_sign_a_device_out_and_the_code_still_works() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        let code = code_of(&access, "night", &pilots, "ace");
        let (_, session) = access.sign_in(&event("night"), &pilots, &code).unwrap();
        assert!(access.sign_out(&event("night"), &pilot("ace")));
        assert_eq!(access.who(&event("night"), &pilots, &session), None);
        assert!(
            !access.sign_out(&event("night"), &pilot("ace")),
            "nothing to sign out twice"
        );
        assert!(!access.sign_out(&event("night"), &pilot("nobody")));
        assert!(!access.sign_out(&event("other"), &pilot("ace")));
        assert!(access.sign_in(&event("night"), &pilots, &code).is_some());
    }

    #[test]
    fn a_pilot_can_sign_themselves_out() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace", "bee"]);
        let ace = code_of(&access, "night", &pilots, "ace");
        let bee = code_of(&access, "night", &pilots, "bee");
        let (_, ace_phone) = access.sign_in(&event("night"), &pilots, &ace).unwrap();
        let (_, bee_phone) = access.sign_in(&event("night"), &pilots, &bee).unwrap();
        assert!(access.end(&ace_phone));
        assert_eq!(access.who(&event("night"), &pilots, &ace_phone), None);
        assert_eq!(
            access.who(&event("night"), &pilots, &bee_phone),
            Some(pilot("bee"))
        );
        assert!(!access.end(&ace_phone));
        assert!(!access.end("unknown"));
        assert!(!access.end(""));
    }

    #[test]
    fn an_empty_or_made_up_sign_in_is_nobody() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        code_of(&access, "night", &pilots, "ace");
        // Nobody is signed in: an empty value must not match "no sign-in".
        for session in ["", " ", "null", "undefined", "x"] {
            assert_eq!(
                access.who(&event("night"), &pilots, session),
                None,
                "{session:?}"
            );
        }
    }

    #[test]
    fn an_event_that_is_gone_takes_its_codes_with_it() {
        let access = PilotAccess::new(None);
        let pilots = field(&["ace"]);
        let code = code_of(&access, "night", &pilots, "ace");
        let (_, session) = access.sign_in(&event("night"), &pilots, &code).unwrap();
        access.forget(&event("night"));
        assert_eq!(access.who(&event("night"), &pilots, &session), None);
        assert_eq!(access.sign_in(&event("night"), &pilots, &code), None);
        access.forget(&event("never-existed"));
    }

    #[test]
    fn codes_and_sign_ins_outlast_a_restart() {
        let dir = tempdir("restart");
        let pilots = field(&["ace", "bee"]);
        let (code, session) = {
            let access = PilotAccess::new(Some(dir.clone()));
            let code = code_of(&access, "night", &pilots, "ace");
            let (_, session) = access.sign_in(&event("night"), &pilots, &code).unwrap();
            (code, session)
        };
        let again = PilotAccess::new(Some(dir.clone()));
        assert_eq!(code_of(&again, "night", &pilots, "ace"), code);
        assert_eq!(
            again.who(&event("night"), &pilots, &session),
            Some(pilot("ace"))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_broken_file_means_nobody_has_a_code_and_never_an_error() {
        let dir = tempdir("broken");
        for text in [
            "",
            "not json",
            "[]",
            "null",
            "{\"events\":7}",
            "{\"events\":{\"e\":[]}}",
        ] {
            std::fs::write(dir.join(ACCESS_FILE), text).unwrap();
            let access = PilotAccess::new(Some(dir.clone()));
            let pilots = field(&["ace"]);
            assert_eq!(
                access.sign_in(&event("e"), &pilots, "ABC234"),
                None,
                "{text}"
            );
            // And it can start afresh.
            assert_eq!(access.codes(&event("e"), &pilots).len(), 1);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_folder_that_cannot_be_written_still_works_until_grid_is_closed() {
        let dir = tempdir("gone");
        let access = PilotAccess::new(Some(dir.join("no-such-folder")));
        let pilots = field(&["ace"]);
        let code = code_of(&access, "night", &pilots, "ace");
        assert!(access.sign_in(&event("night"), &pilots, &code).is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_sign_in_cookie_is_read_from_among_others_and_cannot_be_read_by_a_script() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(COOKIE, value.parse().unwrap());
            headers
        };
        assert_eq!(
            pilot_cookie(&with("gridfpv_pilot=abc")).as_deref(),
            Some("abc")
        );
        assert_eq!(
            pilot_cookie(&with("gridfpv_director=d; gridfpv_pilot=abc; x=1")).as_deref(),
            Some("abc")
        );
        for none in [
            "gridfpv_pilot=",
            "gridfpv_pilotx=abc",
            "xgridfpv_pilot=abc",
            "gridfpv_director=abc",
        ] {
            assert_eq!(pilot_cookie(&with(none)), None, "{none}");
        }
        assert_eq!(pilot_cookie(&HeaderMap::new()), None);
        let set = cookie("abc", COOKIE_SECONDS);
        for part in [
            "gridfpv_pilot=abc",
            "Path=/",
            "HttpOnly",
            "SameSite=Strict",
            "Max-Age=172800",
        ] {
            assert!(set.contains(part), "{part} in {set}");
        }
        assert!(cookie("", 0).contains("Max-Age=0"));
    }

    #[test]
    fn the_qr_link_puts_the_code_after_the_hash() {
        assert_eq!(
            join_link("http://192.168.1.20:8081/", "ABC234"),
            "http://192.168.1.20:8081/#/join/ABC234"
        );
        assert_eq!(
            join_link("http://192.168.1.20:8081", "ABC234"),
            "http://192.168.1.20:8081/#/join/ABC234"
        );
    }

    #[test]
    fn a_themed_qr_code_is_dark_on_light_in_grids_colours_and_never_the_other_way() {
        let svg = qr_svg_themed("http://192.168.1.20/").unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"), "{svg}");
        assert!(svg.contains(QR_DARK) && svg.contains(QR_LIGHT));
        assert!(!svg.contains("#000000") && !svg.contains("#ffffff"));
        assert!(!svg.contains("<script"));
        // Dark squares on a light ground: a camera cannot read the reverse. The check is on
        // the colours themselves, so a change of palette cannot swap them by mistake.
        let brightness = |hex: &str| {
            let part = |at: usize| u32::from_str_radix(&hex[at..at + 2], 16).unwrap();
            (299 * part(1) + 587 * part(3) + 114 * part(5)) / 1000
        };
        assert!(brightness(QR_DARK) < 40, "the squares are dark");
        assert!(brightness(QR_LIGHT) > 215, "the ground is light");
        // The same squares as the plain code: only the colours differ.
        let plain = qr_svg("http://192.168.1.20/").unwrap();
        assert_eq!(
            svg.replace(QR_DARK, "#000000").replace(QR_LIGHT, "#ffffff"),
            plain
        );
    }

    #[test]
    fn the_public_link_is_the_name_the_caller_used_unless_no_phone_could_use_it() {
        let door = || Some("http://192.168.1.20:8081/".to_string());
        // The name the caller reached Grid by.
        for host in [
            "192.168.1.20",
            "192.168.1.20:8081",
            "10.42.0.1:80",
            "grid.race.lan",
            "grid.race.lan:8081",
            "[fd00::7]:8081",
        ] {
            assert_eq!(
                public_link(Some(host), door()),
                Some(format!("http://{host}/")),
                "{host}"
            );
        }
        // On the Grid computer, or with no usable name: the Public door's address.
        for host in [
            "localhost",
            "localhost:8080",
            "127.0.0.1:8080",
            "[::1]:8080",
            "",
            "  ",
        ] {
            assert_eq!(public_link(Some(host), door()), door(), "{host:?}");
        }
        assert_eq!(public_link(None, door()), door());
        // A host that is not plainly a name and a port is never put in a picture.
        for host in [
            "evil.example/path",
            "a b",
            "x@y",
            "grid.lan?x=1",
            "grid.lan#/join/ABC234",
            "<svg>",
            "grid.lan\"onload=",
        ] {
            assert_eq!(public_link(Some(host), door()), door(), "{host}");
            assert_eq!(public_link(Some(host), None), None, "{host}");
        }
        let long = "a".repeat(300);
        assert_eq!(public_link(Some(&long), None), None);
        assert_eq!(public_link(Some("localhost"), None), None);
    }

    #[test]
    fn a_qr_code_is_an_svg_picture_with_a_white_border() {
        let svg = qr_svg("http://192.168.1.20:8081/#/join/ABC234").unwrap();
        assert!(
            svg.starts_with("<?xml") || svg.starts_with("<svg"),
            "{}",
            &svg[..40]
        );
        assert!(svg.contains("<svg"));
        assert!(svg.contains("#ffffff") && svg.contains("#000000"));
        assert!(!svg.contains("<script"));
        // Another link gives another picture.
        assert_ne!(
            svg,
            qr_svg("http://192.168.1.20:8081/#/join/XYZ789").unwrap()
        );
        // A link far too long for a QR code is an error, not a panic.
        assert!(qr_svg(&"x".repeat(10_000)).is_err());
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-pilot-access-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
