use sqlx::{postgres::PgPoolOptions, PgPool};
use std::time::Duration;
use tracing::info;

/// Connect to Postgres and return a connection pool.
pub async fn connect(url: &str) -> anyhow::Result<PgPool> {
    info!("connecting to control-plane DB…");
    let pool = PgPoolOptions::new()
        .min_connections(2)
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(300))
        .connect(url)
        .await?;
    info!("DB connection pool ready");
    Ok(pool)
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
