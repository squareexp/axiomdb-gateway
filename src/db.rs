use sqlx::{postgres::PgPoolOptions, PgPool};
use std::env;
use std::time::Duration;
use tracing::info;

/// Connect to Postgres and return a connection pool.
pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    info!("connecting to control-plane DB…");
    let min_connections = env_u32("AXIOMDB_DB_POOL_MIN", 2);
    let max_connections = env_u32("AXIOMDB_DB_POOL_MAX", 50);
    let acquire_timeout_secs = env_u64("AXIOMDB_DB_ACQUIRE_TIMEOUT_SECS", 3);

    let pool = PgPoolOptions::new()
        .min_connections(min_connections)
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
        .idle_timeout(Duration::from_secs(300))
        .connect(url)
        .await?;
    info!(
        "DB connection pool ready min={} max={} acquire_timeout={}s",
        min_connections, max_connections, acquire_timeout_secs
    );
    Ok(pool)
}

fn env_u32(key: &str, fallback: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

fn env_u64(key: &str, fallback: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

pub async fn assert_schema_ready(pool: &PgPool) -> anyhow::Result<()> {
    let required = [
        "schema_migrations",
        "users",
        "projects",
        "project_databases",
        "project_branches",
        "provisioning_jobs",
        "audit_events",
    ];

    let present: i64 = sqlx::query_scalar(
        "SELECT count(*)
         FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name = ANY($1)",
    )
    .bind(&required[..])
    .fetch_one(pool)
    .await?;

    if present != required.len() as i64 {
        anyhow::bail!(
            "Control-plane schema is not initialized. Run ./migrate.sh before starting axiomdb-gateway."
        );
    }

    Ok(())
}
