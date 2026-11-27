//! The server's one router.

use std::path::Path;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::accounts::Accounts;
use crate::asks::{self, Asks};
use crate::book::{Book, BookError};
use crate::claims;
use crate::manage;
use crate::page::Page;
use crate::people;
use crate::push;
use crate::read;
use crate::signin::{self, Provider};
use crate::store::Store;

/// The largest request body taken. A batch of log entries with signal in them is the big
/// one; a Director sends a long log in several batches.
const BODY_MAX: usize = 32 * 1024 * 1024;

/// What every route shares. Cloning shares it.
#[derive(Clone)]
pub struct AppState {
    pub book: Arc<Book>,
    pub store: Store,
    pub accounts: Arc<Accounts>,
    /// The address people reach this server at, such as `https://grid.example.org`, when
    /// it is set (`GRIDFPV_SERVER_PUBLIC_URL`). Sign-in needs it; a link printed on the
    /// command line uses it.
    pub public_url: Option<String>,
    /// The providers people can sign in with here (`signin`).
    pub providers: Arc<Vec<Provider>>,
    /// What pilots have asked for that a Director has yet to answer (`asks`).
    pub asks: Arc<Asks>,
}

impl AppState {
    /// Say where people reach this server. A slash on the end is dropped.
    #[must_use]
    pub fn at(mut self, public_url: Option<String>) -> Self {
        self.public_url = public_url
            .map(|url| url.trim().trim_end_matches('/').to_owned())
            .filter(|url| !url.is_empty());
        self
    }

    /// Say which providers people can sign in with.
    #[must_use]
    pub fn with_providers(mut self, providers: Vec<Provider>) -> Self {
        self.providers = Arc::new(providers);
        self
    }

    /// The server's state over a data folder, made ready if the folder is new.
    pub fn open(dir: &Path) -> Result<Self, BookError> {
        let accounts =
            Accounts::open(dir).map_err(|e| BookError::Io(std::io::Error::other(e.to_string())))?;
        Ok(Self {
            book: Arc::new(Book::open(dir)?),
            store: Store::new(dir),
            accounts: Arc::new(accounts),
            public_url: None,
            providers: Arc::default(),
            asks: Arc::default(),
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
const API_ROOTS: [&str; 4] = ["/api", "/push", "/public", "/auth"];

fn is_api_path(path: &str) -> bool {
    API_ROOTS.iter().any(|root| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// Every route, and the web page (see [`Page`]).
pub fn router_with_page(state: AppState, page: Page) -> Router {
    router(state).fallback(move |request: axum::extract::Request| {
        let page = page.clone();
        async move {
            if is_api_path(request.uri().path()) {
                return nothing_here().await.into_response();
            }
            match page.serve(request).await {
                Some(response) => response,
                None => nothing_here().await.into_response(),
            }
        }
    })
}

/// Every route, and the web page from `assets` when a folder is given.
pub fn router_with_pages(state: AppState, assets: Option<&Path>) -> Router {
    let page = assets.map_or(Page::None, |dir| Page::Folder(dir.to_owned()));
    router_with_page(state, page)
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
        .route("/push/events", get(push::get_events))
        .route("/api/clubs", get(read::get_clubs))
        .route(
            "/api/clubs/{club}",
            axum::routing::patch(manage::patch_club),
        )
        .route(
            "/api/clubs/{club}/tokens",
            get(manage::get_tokens).post(manage::post_token),
        )
        .route(
            "/api/clubs/{club}/tokens/{id}",
            axum::routing::delete(manage::delete_token),
        )
        .route(
            "/api/clubs/{club}/events/{event}/listing",
            axum::routing::put(manage::put_listing),
        )
        .route(
            "/api/clubs/{club}/events/{event}",
            get(read::get_one_event).delete(manage::delete_event),
        )
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
                .route("/event/channels", get(read::get_channels))
                // A signed-in pilot asks the Director for a spot (§13.4).
                .route("/pilot/signup/take", axum::routing::post(asks::post_take))
                .route("/pilot/signup/next", axum::routing::post(asks::post_next))
                .route("/pilot/signup/leave", axum::routing::post(asks::post_leave))
                // And for an open seat in a race that is under way (§13.13).
                .route(
                    "/pilot/checkin/take",
                    axum::routing::post(asks::post_take_seat),
                )
                .route(
                    "/pilot/checkin/drop",
                    axum::routing::post(asks::post_drop_seat),
                ),
        )
        .route("/api/sign-in/providers", get(signin::get_providers))
        .route("/auth/{provider}/start", get(signin::get_start))
        .route("/auth/{provider}/callback", get(signin::get_callback))
        .route("/api/me", get(people::get_me).patch(people::patch_me))
        .route(
            "/api/me/sign-ins/{provider}/{subject}",
            axum::routing::delete(people::delete_my_sign_in),
        )
        .route("/api/sign-out", axum::routing::post(people::post_sign_out))
        .route("/api/invites/{secret}", get(people::get_invite))
        .route(
            "/api/invites/{secret}/accept",
            axum::routing::post(people::post_accept_invite),
        )
        .route(
            "/api/clubs/{club}/invites",
            get(people::get_club_invites).post(people::post_club_invite),
        )
        .route(
            "/api/clubs/{club}/invites/{id}",
            axum::routing::delete(people::delete_club_invite),
        )
        .route("/api/clubs/{club}/members", get(people::get_members))
        .route(
            "/api/clubs/{club}/members/{account}",
            axum::routing::put(people::put_member).delete(people::delete_member),
        )
        .route("/api/clubs/{club}/pilots", get(claims::get_pilots))
        .route(
            "/api/clubs/{club}/pilots/{pilot}/claim",
            axum::routing::post(claims::post_claim).delete(claims::delete_claim),
        )
        .route(
            "/api/clubs/{club}/pilots/{pilot}/holder",
            axum::routing::put(claims::put_holder).delete(claims::delete_holder),
        )
        .route(
            "/api/clubs/{club}/pilots/{pilot}/asks/{account}",
            axum::routing::delete(claims::delete_ask),
        )
        .route("/api/me/results", get(claims::get_my_results))
        .route(
            "/push/pilots",
            get(claims::get_push_pilots).put(claims::put_push_pilots),
        )
        .route("/push/members", get(claims::get_push_members))
        .route("/push/listen", get(asks::get_listen))
        .route("/push/requests", get(asks::get_requests))
        .route(
            "/push/requests/{id}",
            axum::routing::post(asks::post_answer),
        )
        .route(
            "/push/pilots/{pilot}/account",
            axum::routing::put(claims::put_push_account).delete(claims::delete_push_account),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            people::same_site,
        ))
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
