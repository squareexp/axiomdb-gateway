use sqlx::{postgres::PgPoolOptions, PgPool};
use std::time::Duration;
use tracing::info;

/// Connect to Postgres and return a connection pool.
pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    info!("connecting to control-plane DB…");
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await?;
    info!("DB connection pool ready");
    Ok(pool)
}
