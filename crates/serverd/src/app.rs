//! The server's one router.

use std::path::Path;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::book::{Book, BookError};
use crate::push;
use crate::read;
use crate::store::Store;

/// The largest request body taken. A batch of log entries with signal in them is the big
/// one; a Director sends a long log in several batches.
const BODY_MAX: usize = 32 * 1024 * 1024;

/// What every route shares. Cloning shares it.
#[derive(Clone)]
pub struct AppState {
    pub book: Arc<Book>,
    pub store: Store,
}

impl AppState {
    /// The server's state over a data folder, made ready if the folder is new.
    pub fn open(dir: &Path) -> Result<Self, BookError> {
        Ok(Self {
            book: Arc::new(Book::open(dir)?),
            store: Store::new(dir),
        })
    }
}

/// `GET /health`: the server is up, and which build it is.
#[derive(Debug, Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
}

async fn get_health() -> Json<Health> {
    Json(Health {
        ok: true,
        version: gridfpv_server::BUILD_VERSION,
    })
}

/// The folder the web page is served from, when there is one: the built console
/// (`frontend/apps/rd-console/dist`). It is the same page a Director serves. Asked
/// `GET /public/hello`, this server says it is a server, and the page shows its events.
fn pages(assets: &Path) -> ServeDir<ServeFile> {
    // Any address that is not a file is the page itself: it reads where it is from the
    // part after the `#`.
    ServeDir::new(assets).fallback(ServeFile::new(assets.join("index.html")))
}

/// What an address that is no route and no file gets, when no page is served.
async fn nothing_here() -> push::ApiError {
    push::ApiError::new(
        axum::http::StatusCode::NOT_FOUND,
        gridfpv_server::error::ErrorCode::UnknownScope,
        "There is nothing at this address.",
    )
}

/// The first parts of an address that belong to the API. An address under one of these that
/// matches no route is a mistake to say so, and never the page: a program asking for JSON
/// must not be handed HTML.
const API_ROOTS: [&str; 3] = ["/api", "/push", "/public"];

fn is_api_path(path: &str) -> bool {
    API_ROOTS.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Every route, and the web page from `assets` when a folder is given.
pub fn router_with_pages(state: AppState, assets: Option<&Path>) -> Router {
    let api = router(state);
    let Some(assets) = assets else {
        return api.fallback(nothing_here);
    };
    let files = pages(assets);
    api.fallback(move |request: axum::extract::Request| {
        let files = files.clone();
        async move {
            if is_api_path(request.uri().path()) {
                return nothing_here().await.into_response();
            }
            match files.oneshot(request).await {
                Ok(response) => response.into_response(),
                Err(never) => match never {},
            }
        }
    })
}

/// Every route.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(get_health))
        .route("/public/hello", get(read::get_server_hello))
        .route("/push/hello", get(push::get_hello))
        .route(
            "/push/events/{id}",
            get(push::get_event)
                .put(push::put_event)
                .delete(push::delete_event),
        )
        .route("/push/events/{id}/log", axum::routing::post(push::post_log))
        .route("/api/clubs", get(read::get_clubs))
        .route("/api/clubs/{club}/events", get(read::get_events))
        .nest(
            "/api/clubs/{club}/events/{event}/public",
            Router::new()
                .route("/hello", get(read::get_hello))
                .route("/event", get(read::get_event))
                .route("/event/state", get(read::get_state))
                .route("/event/changes", get(read::get_changes))
                .route("/event/pilots", get(read::get_pilots))
                .route("/event/classes", get(read::get_classes))
                .route("/event/timers", get(read::get_timers))
                .route("/event/channels", get(read::get_channels)),
        )
        .layer(DefaultBodyLimit::max(BODY_MAX))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::is_api_path;

    #[test]
    fn an_api_path_is_one_under_an_api_root_and_nothing_that_only_starts_like_one() {
        for path in [
            "/api",
            "/api/",
            "/api/clubs",
            "/push",
            "/push/x/y",
            "/public",
            "/public/hello",
        ] {
            assert!(is_api_path(path), "{path}");
        }
        for path in [
            "/",
            "/apiary",
            "/pushed",
            "/publicity",
            "/assets/api",
            "/index.html",
            "",
        ] {
            assert!(!is_api_path(path), "{path}");
        }
    }
}
