use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::convert::Infallible;
use std::time::Duration;
use sysinfo::System;
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    executor,
    middleware::AuthUser,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/:project_id/monitoring/summary",
            get(monitoring_summary),
        )
        .route(
            "/projects/:project_id/monitoring/stream",
            get(monitoring_stream),
        )
}

// ---------------------------------------------------------------------------
// GET /projects/:project_id/monitoring/summary
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct ProjectRow {
    app_key: String,
    env: String,
    database_name: String,
}

async fn monitoring_summary(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let row = sqlx::query_as::<_, ProjectRow>(
        "SELECT p.app_key, p.env, pd.database_name
         FROM projects p
         JOIN project_databases pd ON pd.project_id = p.id
         WHERE p.id = $1 LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    // Refresh system metrics
    let (cpu_pct, mem_used_mb, mem_total_mb) = {
        let mut sys = state.sys.lock().await;
        sys.refresh_all();
        sys.refresh_memory();
        let cpu: f32 =
            sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>() / sys.cpus().len().max(1) as f32;
        (
            cpu,
            sys.used_memory() / 1024 / 1024,
            sys.total_memory() / 1024 / 1024,
        )
    };

    // Active PG connections (from control plane)
    let pg_connections: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM pg_stat_activity WHERE state='active'")
            .fetch_one(&state.db)
            .await
            .unwrap_or(0);

    // Smoke test
    let smoke_ok = executor::run_smoke(&state.cfg.dbctl_bin, &row.app_key, &row.env)
        .await
        .is_ok();

    Ok(Json(json!({
        "database": row.database_name,
        "cpu_percent": cpu_pct,
        "mem_used_mb": mem_used_mb,
        "mem_total_mb": mem_total_mb,
        "pg_active_connections": pg_connections,
        "smoke_ok": smoke_ok,
    })))
}

// ---------------------------------------------------------------------------
// GET /projects/:project_id/monitoring/stream  (SSE)
// ---------------------------------------------------------------------------

async fn monitoring_stream(
    State(_state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(_project_id): Path<Uuid>,
) -> axum::response::Sse<
    impl futures::Stream<Item = std::result::Result<axum::response::sse::Event, Infallible>>,
> {
    use tokio_stream::StreamExt;

    let stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)))
            .map(move |_| {
                let mut sys = System::new();
                sys.refresh_all();
                sys.refresh_memory();
                let cpu: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
                    / sys.cpus().len().max(1) as f32;
                let payload = json!({
                    "cpu_percent": cpu,
                    "mem_used_mb": sys.used_memory() / 1024 / 1024,
                    "ts": chrono::Utc::now().to_rfc3339(),
                });
                Ok(axum::response::sse::Event::default().data(payload.to_string()))
            });

    axum::response::Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
