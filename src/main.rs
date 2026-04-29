mod config;
mod db;
mod error;
mod executor;
mod middleware;
mod models;
mod routes;
mod state;

use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load env vars
    dotenvy::dotenv().ok();

    // Init tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "astra_db=debug,tower_http=info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cfg = config::Config::from_env()?;
    info!("astradb-core starting on {}", cfg.bind_addr);

    // Connect to control-plane DB
    let pool = db::connect(&cfg.database_url).await?;

    let app_state = state::AppState::new(pool, cfg.clone());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .nest("/api/v1", routes::build(app_state))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr).await?;
    info!("listening on {}", cfg.bind_addr);

    axum::serve(listener, app).await?;
    Ok(())
}
