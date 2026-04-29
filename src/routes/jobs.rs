use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    middleware::AuthUser,
    models::{audit::AuditEvent, job::Job},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/jobs/:job_id", get(get_job))
        .route("/audit", get(list_audit))
}

async fn get_job(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(job_id): Path<Uuid>,
) -> Result<Json<Job>> {
    let job = sqlx::query_as::<_, Job>("SELECT * FROM provisioning_jobs WHERE id=$1")
        .bind(job_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(job))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    limit: Option<i64>,
}

async fn list_audit(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Query(q): Query<AuditQuery>,
) -> Result<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(100).min(500);
    let events = sqlx::query_as::<_, AuditEvent>(
        "SELECT * FROM audit_events ORDER BY created_at DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "events": events, "count": events.len() })))
}
