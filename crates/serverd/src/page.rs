//! The web page the server shows: the built console.
//!
//! It comes from one of two places:
//!
//! - **A folder** (`GRIDFPV_SERVER_ASSETS`), as the container image has it.
//! - **Inside the program itself**, when it was built with the `embed-page` feature. That
//!   build is one file with nothing beside it, for a server installed without Docker.
//!
//! Either way a path that is no file gets the page: the page reads where it is from the
//! part of the address after the `#`.

use std::path::{Path, PathBuf};

use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

/// Where the page comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    /// There is none. The server takes pushes and answers the API.
    None,
    /// The files in this folder.
    Folder(PathBuf),
    /// The page built into this program.
    Embedded,
}

/// Whether this program was built with the page inside it.
pub const EMBEDDED: bool = cfg!(feature = "embed-page");

impl Page {
    /// Pick the page: the folder that was asked for, else the one inside the program, else
    /// the usual folder when it holds a page, else none.
    pub fn pick(asked: Option<PathBuf>, usual: &Path) -> Self {
        let has_page = |dir: &Path| dir.join("index.html").is_file();
        match asked {
            Some(dir) if has_page(&dir) => Self::Folder(dir),
            // A folder asked for by name and not there is said so by the caller: it must
            // not quietly become some other page.
            Some(_) => Self::None,
            None if EMBEDDED => Self::Embedded,
            None if has_page(usual) => Self::Folder(usual.to_owned()),
            None => Self::None,
        }
    }

    /// Answer a request for the page or one of its files. `None` when there is no page.
    pub async fn serve(&self, request: Request) -> Option<Response> {
        match self {
            Self::None => None,
            Self::Folder(dir) => {
                let files = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
                Some(match files.oneshot(request).await {
                    Ok(response) => response.into_response(),
                    Err(never) => match never {},
                })
            }
            Self::Embedded => Some(embedded::serve(request)),
        }
    }
}

#[cfg(not(feature = "embed-page"))]
mod embedded {
    use super::*;

    /// Never reached: [`Page::pick`] gives `Embedded` only in a build that has it.
    pub fn serve(_request: Request) -> Response {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(feature = "embed-page")]
mod embedded {
    use axum::body::Body;
    use axum::http::{Method, StatusCode, header};
    use rust_embed::RustEmbed;

    use super::*;

    /// The built console, taken into the program when it is compiled. The folder must exist
    /// then: build the frontend first.
    #[derive(RustEmbed)]
    #[folder = "../../frontend/apps/rd-console/dist"]
    struct Files;

    pub fn serve(request: Request) -> Response {
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let asked = request.uri().path().trim_start_matches('/');
        // A path that is no file is the page itself.
        let (path, file) = match Files::get(asked) {
            Some(file) if !asked.is_empty() => (asked, file),
            _ => match Files::get("index.html") {
                Some(index) => ("index.html", index),
                None => return StatusCode::NOT_FOUND.into_response(),
            },
        };
        let kind = mime_guess::from_path(path).first_or_octet_stream();
        let body = if request.method() == Method::HEAD {
            Body::empty()
        } else {
            Body::from(file.data.into_owned())
        };
        ([(header::CONTENT_TYPE, kind.as_ref().to_owned())], body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_asked_for_is_used_only_when_it_holds_a_page() {
        let dir = tempfile::tempdir().unwrap();
        let usual = dir.path().join("usual");
        let asked = dir.path().join("asked");
        std::fs::create_dir_all(&usual).unwrap();
        std::fs::create_dir_all(&asked).unwrap();
        std::fs::write(usual.join("index.html"), "usual").unwrap();

        // Asked for and empty: no page. Not the usual one, and not the one inside.
        assert_eq!(Page::pick(Some(asked.clone()), &usual), Page::None);
        std::fs::write(asked.join("index.html"), "asked").unwrap();
        assert_eq!(Page::pick(Some(asked.clone()), &usual), Page::Folder(asked));
    }

    #[test]
    fn with_nothing_asked_for_the_page_inside_comes_before_the_usual_folder() {
        let dir = tempfile::tempdir().unwrap();
        let usual = dir.path().join("usual");
        let picked = Page::pick(None, &usual);
        if EMBEDDED {
            assert_eq!(picked, Page::Embedded);
        } else {
            assert_eq!(picked, Page::None);
            std::fs::create_dir_all(&usual).unwrap();
            std::fs::write(usual.join("index.html"), "usual").unwrap();
            assert_eq!(Page::pick(None, &usual), Page::Folder(usual));
        }
    }
}
