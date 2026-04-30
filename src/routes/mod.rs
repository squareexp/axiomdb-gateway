pub mod auth;
pub mod backups;
pub mod branches;
pub mod health;
pub mod jobs;
pub mod monitoring;
pub mod network;
pub mod projects;
pub mod secrets;
pub mod tables;

use crate::state::AppState;
use axum::{middleware, Router};

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
        .merge(health::router())
        .merge(protected)
        .with_state(state)
}
