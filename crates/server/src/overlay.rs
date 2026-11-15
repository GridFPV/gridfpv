//! How the race overlay looks: where its panel sits on the video, how big it is, how solid
//! its background is, and the two logos on its top line.
//!
//! There is one look for the whole Director. The live page in OBS, the replay page, and so
//! every overlay the Director redraws onto a recording all read it, which is what makes a
//! redrawn video match one filmed live.
//!
//! The placement is kept in `overlay.json` in the data folder. The logos are the files
//! `left` and `right` in its `branding` folder.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;

/// File name of the overlay's look under the data directory.
pub const OVERLAY_FILE: &str = "overlay.json";
/// The folder of the data directory the logos are kept in.
pub const BRANDING_DIR: &str = "branding";
/// The biggest logo taken, in bytes.
pub const MAX_LOGO_BYTES: usize = 4 << 20;

/// The farthest the panel can be pushed in from an edge, in pixels.
pub const OFFSET_MAX: u32 = 4000;
/// The panel's size, as a percentage: the smallest and the biggest.
pub const SCALE_MIN: u32 = 25;
pub const SCALE_MAX: u32 = 300;

/// The corner of the video the panel hangs from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum OverlayCorner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

fn default_offset() -> u32 {
    24
}
fn default_scale() -> u32 {
    100
}
fn default_opacity() -> u32 {
    90
}

/// Where the panel sits. This is what is stored, and what `PUT /overlay/look` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct OverlayPlacement {
    #[serde(default)]
    pub corner: OverlayCorner,
    /// Pixels in from the corner's side edge.
    #[serde(default = "default_offset")]
    pub x: u32,
    /// Pixels in from the corner's top or bottom edge.
    #[serde(default = "default_offset")]
    pub y: u32,
    /// The panel's size, as a percentage of its natural size.
    #[serde(default = "default_scale")]
    pub scale: u32,
    /// The panel's background, percent opaque: 0 is text alone over the video, 100 a solid
    /// sheet.
    #[serde(default = "default_opacity")]
    pub opacity: u32,
}

impl Default for OverlayPlacement {
    fn default() -> Self {
        Self {
            corner: OverlayCorner::TopLeft,
            x: default_offset(),
            y: default_offset(),
            scale: default_scale(),
            opacity: default_opacity(),
        }
    }
}

/// The overlay's look as a client reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct OverlayLook {
    pub placement: OverlayPlacement,
    /// Whether there is a logo for the left of the top line (`GET /overlay/logo/left`).
    pub logo_left: bool,
    /// Whether there is a logo for the right of the top line.
    pub logo_right: bool,
}

/// What can go wrong changing the look.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("The overlay can be moved at most {OFFSET_MAX} pixels in from an edge.")]
    TooFar,
    #[error("The overlay's size is between {SCALE_MIN} and {SCALE_MAX} percent.")]
    BadScale,
    #[error("The background is between 0 and 100 percent solid.")]
    BadOpacity,
    #[error("A logo goes on the left or on the right.")]
    NoSuchSide,
    #[error("A logo is a PNG, JPEG, WebP or SVG picture.")]
    NotAPicture,
    #[error("That picture is too big for a logo. Keep it under 4 MB.")]
    TooBig,
    #[error("This Director keeps no data, so it has nowhere to keep a logo.")]
    NoFolder,
    #[error("Could not save the overlay's look: {0}")]
    Io(String),
}

impl From<OverlayError> for ProtocolError {
    fn from(error: OverlayError) -> Self {
        let code = match error {
            OverlayError::TooFar
            | OverlayError::BadScale
            | OverlayError::BadOpacity
            | OverlayError::NotAPicture
            | OverlayError::TooBig => ErrorCode::BadRequest,
            OverlayError::NoSuchSide => ErrorCode::UnknownScope,
            OverlayError::NoFolder => ErrorCode::Refused,
            OverlayError::Io(_) => ErrorCode::Internal,
        };
        ProtocolError::new(code, error.to_string())
    }
}

/// The side of the top line a logo sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoSide {
    Left,
    Right,
}

impl LogoSide {
    fn parse(side: &str) -> Result<Self, OverlayError> {
        match side {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            _ => Err(OverlayError::NoSuchSide),
        }
    }
    fn file(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// The media type of a picture, from how its bytes begin. `None`: not a picture we take.
fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        // An SVG is text: allow a little white space or an XML line before its tag.
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
        let head = head.trim_start();
        (head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")))
            .then_some("image/svg+xml")
    }
}

struct Inner {
    placement: OverlayPlacement,
    data_dir: Option<PathBuf>,
}

/// The overlay's look. Cloning shares the one desk.
#[derive(Clone)]
pub struct OverlayDesk {
    inner: Arc<RwLock<Inner>>,
}

impl OverlayDesk {
    /// The desk kept under `data_dir`. A file that cannot be read gives the usual look.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let placement = data_dir
            .as_ref()
            .and_then(|dir| std::fs::read(dir.join(OVERLAY_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                placement,
                data_dir,
            })),
        }
    }

    fn logo_path(&self, side: LogoSide) -> Option<PathBuf> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        Some(
            inner
                .data_dir
                .as_ref()?
                .join(BRANDING_DIR)
                .join(side.file()),
        )
    }

    /// The look as it is now.
    pub fn look(&self) -> OverlayLook {
        let placement = self
            .inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .placement;
        let has = |side| self.logo_path(side).is_some_and(|path| path.is_file());
        OverlayLook {
            placement,
            logo_left: has(LogoSide::Left),
            logo_right: has(LogoSide::Right),
        }
    }

    /// Move or resize the panel.
    pub fn place(&self, placement: OverlayPlacement) -> Result<OverlayLook, OverlayError> {
        if placement.x > OFFSET_MAX || placement.y > OFFSET_MAX {
            return Err(OverlayError::TooFar);
        }
        if !(SCALE_MIN..=SCALE_MAX).contains(&placement.scale) {
            return Err(OverlayError::BadScale);
        }
        if placement.opacity > 100 {
            return Err(OverlayError::BadOpacity);
        }
        {
            let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
            if let Some(dir) = &inner.data_dir {
                let io = |e: std::io::Error| OverlayError::Io(e.to_string());
                std::fs::create_dir_all(dir).map_err(io)?;
                let bytes = serde_json::to_vec_pretty(&placement)
                    .map_err(|e| OverlayError::Io(e.to_string()))?;
                std::fs::write(dir.join(OVERLAY_FILE), bytes).map_err(io)?;
            }
            inner.placement = placement;
        }
        Ok(self.look())
    }

    /// Keep a picture as the logo of one side.
    pub fn set_logo(&self, side: LogoSide, bytes: &[u8]) -> Result<OverlayLook, OverlayError> {
        if bytes.len() > MAX_LOGO_BYTES {
            return Err(OverlayError::TooBig);
        }
        if picture_type(bytes).is_none() {
            return Err(OverlayError::NotAPicture);
        }
        let path = self.logo_path(side).ok_or(OverlayError::NoFolder)?;
        let io = |e: std::io::Error| OverlayError::Io(e.to_string());
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(io)?;
        }
        std::fs::write(&path, bytes).map_err(io)?;
        Ok(self.look())
    }

    /// Take one side's logo away. Fine when there was none.
    pub fn clear_logo(&self, side: LogoSide) -> Result<OverlayLook, OverlayError> {
        if let Some(path) = self.logo_path(side)
            && path.is_file()
        {
            std::fs::remove_file(&path).map_err(|e| OverlayError::Io(e.to_string()))?;
        }
        Ok(self.look())
    }

    /// One side's logo: its media type and its bytes.
    pub fn logo(&self, side: LogoSide) -> Option<(&'static str, Vec<u8>)> {
        let bytes = std::fs::read(self.logo_path(side)?).ok()?;
        Some((picture_type(&bytes)?, bytes))
    }
}

/// `GET /overlay/look` — where the overlay sits and which logos it has. An open read: the
/// overlay page in OBS asks with no login.
pub async fn get_look(State(registry): State<EventRegistry>) -> Json<OverlayLook> {
    Json(registry.overlay().look())
}

/// `PUT /overlay/look` — move or resize the overlay, RD-gated.
pub async fn put_look(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<OverlayPlacement>,
) -> Result<Json<OverlayLook>, ProtocolError> {
    Ok(Json(registry.overlay().place(body)?))
}

/// `GET /overlay/logo/{side}` — the logo of `left` or `right`. `404` when there is none.
pub async fn get_logo(State(registry): State<EventRegistry>, Path(side): Path<String>) -> Response {
    let found = LogoSide::parse(&side)
        .ok()
        .and_then(|side| registry.overlay().logo(side));
    match found {
        Some((kind, bytes)) => (
            [
                (header::CONTENT_TYPE, kind),
                // A new logo must show at once, on a page that has been open all evening.
                (header::CACHE_CONTROL, "no-cache"),
                // An SVG is a document: it may draw, and nothing more.
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; style-src 'unsafe-inline'; sandbox",
                ),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `PUT /overlay/logo/{side}` — keep the picture in the body as that side's logo, RD-gated.
pub async fn put_logo(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(side): Path<String>,
    body: Bytes,
) -> Result<Json<OverlayLook>, ProtocolError> {
    let side = LogoSide::parse(&side)?;
    Ok(Json(registry.overlay().set_logo(side, &body)?))
}

/// `DELETE /overlay/logo/{side}` — take that side's logo away, RD-gated.
pub async fn delete_logo(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(side): Path<String>,
) -> Result<Json<OverlayLook>, ProtocolError> {
    let side = LogoSide::parse(&side)?;
    Ok(Json(registry.overlay().clear_logo(side)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-overlay-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    #[test]
    fn the_look_starts_top_left_and_is_kept_in_the_data_folder() {
        let dir = scratch("kept");
        let desk = OverlayDesk::new(Some(dir.clone()));
        assert_eq!(
            desk.look(),
            OverlayLook {
                placement: OverlayPlacement {
                    corner: OverlayCorner::TopLeft,
                    x: 24,
                    y: 24,
                    scale: 100,
                    opacity: 90
                },
                logo_left: false,
                logo_right: false,
            }
        );
        let moved = OverlayPlacement {
            corner: OverlayCorner::BottomRight,
            x: 40,
            y: 0,
            scale: 150,
            opacity: 0,
        };
        assert_eq!(desk.place(moved).unwrap().placement, moved);
        // A Director started again reads the same look.
        assert_eq!(OverlayDesk::new(Some(dir.clone())).look().placement, moved);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_placement_out_of_range_is_refused_in_words() {
        let desk = OverlayDesk::new(None);
        let with = |change: fn(&mut OverlayPlacement)| {
            let mut placement = OverlayPlacement::default();
            change(&mut placement);
            desk.place(placement)
        };
        assert!(matches!(with(|p| p.x = 4001), Err(OverlayError::TooFar)));
        assert!(matches!(
            with(|p| p.scale = 24),
            Err(OverlayError::BadScale)
        ));
        assert!(matches!(
            with(|p| p.scale = 301),
            Err(OverlayError::BadScale)
        ));
        assert!(matches!(
            with(|p| p.opacity = 101),
            Err(OverlayError::BadOpacity)
        ));
        // Nothing moved.
        assert_eq!(desk.look().placement, OverlayPlacement::default());
    }

    #[test]
    fn an_old_file_with_fields_missing_still_loads() {
        let dir = scratch("old");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OVERLAY_FILE), br#"{"corner":"TopRight","was":1}"#).unwrap();
        let look = OverlayDesk::new(Some(dir.clone())).look();
        assert_eq!(look.placement.corner, OverlayCorner::TopRight);
        assert_eq!(look.placement.scale, 100);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_logo_is_kept_served_and_taken_away() {
        let dir = scratch("logo");
        let desk = OverlayDesk::new(Some(dir.clone()));
        assert!(desk.logo(LogoSide::Left).is_none());
        assert!(matches!(
            desk.set_logo(LogoSide::Left, b"not a picture"),
            Err(OverlayError::NotAPicture)
        ));
        let look = desk.set_logo(LogoSide::Left, PNG).unwrap();
        assert!(look.logo_left && !look.logo_right);
        assert_eq!(desk.logo(LogoSide::Left), Some(("image/png", PNG.to_vec())));
        let svg = b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        desk.set_logo(LogoSide::Right, svg).unwrap();
        assert_eq!(desk.logo(LogoSide::Right).unwrap().0, "image/svg+xml");
        let look = desk.clear_logo(LogoSide::Left).unwrap();
        assert!(!look.logo_left && look.logo_right);
        // Clearing what is not there is fine.
        desk.clear_logo(LogoSide::Left).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_director_with_no_data_folder_has_nowhere_for_a_logo() {
        let desk = OverlayDesk::new(None);
        assert!(matches!(
            desk.set_logo(LogoSide::Left, PNG),
            Err(OverlayError::NoFolder)
        ));
        assert!(matches!(
            LogoSide::parse("middle"),
            Err(OverlayError::NoSuchSide)
        ));
    }
}
