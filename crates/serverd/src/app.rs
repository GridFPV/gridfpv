//! The server's one router.

use std::path::Path;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::book::{Book, BookError};
use crate::push;
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

/// Every route.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(get_health))
        .route("/push/hello", get(push::get_hello))
        .route(
            "/push/events/{id}",
            get(push::get_event)
                .put(push::put_event)
                .delete(push::delete_event),
        )
        .route("/push/events/{id}/log", axum::routing::post(push::post_log))
        .layer(DefaultBodyLimit::max(BODY_MAX))
        .with_state(state)
}
