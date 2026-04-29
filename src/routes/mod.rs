pub mod auth;
pub mod projects;
pub mod branches;
pub mod tables;
pub mod monitoring;
pub mod backups;
pub mod network;
pub mod secrets;
pub mod jobs;

use axum::{middleware, Router};
use crate::state::AppState;

/// Build the full /api/v1 router.
pub fn build(state: AppState) -> Router {
    let protected = Router::new()
        .merge(projects::router())
        .merge(branches::router())
        .merge(tables::router())
        .merge(monitoring::router())
        .merge(backups::router())
        .merge(network::router())
        .merge(secrets::router())
        .merge(jobs::router())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::require_auth,
        ));

    Router::new()
        .merge(auth::router())
        .merge(protected)
        .with_state(state)
}
