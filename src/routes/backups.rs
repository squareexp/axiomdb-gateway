use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    middleware::AuthUser,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/:project_id/backups", get(list_backups))
        .route(
            "/projects/:project_id/backups/restore",
            post(restore_backup),
        )
}

#[derive(sqlx::FromRow)]
struct AppEnvRow {
    app_key: String,
    env: String,
}

async fn list_backups(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let row = sqlx::query_as::<_, AppEnvRow>("SELECT app_key, env FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    // Run `square-dbctl list --app <app> --env <env> --json`
    let output = tokio::process::Command::new(&state.cfg.dbctl_bin)
        .args(["list", "--app", &row.app_key, "--env", &row.env, "--json"])
        .output()
        .await
        .map_err(|e| AppError::Executor(e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(Json(json!({
        "app_key": row.app_key,
        "env": row.env,
        "catalog": serde_json::from_str::<serde_json::Value>(&stdout)
            .unwrap_or(json!({ "raw": stdout.trim() })),
    })))
}

#[derive(Deserialize)]
pub struct RestoreRequest {
    pub point_in_time: Option<String>,
    pub snapshot_id: Option<String>,
}

async fn restore_backup(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    crate::middleware::require_role(&claims.role, &["owner", "admin"])?;

    let row = sqlx::query_as::<_, AppEnvRow>("SELECT app_key, env FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let job_id: Uuid = sqlx::query_scalar(
        "INSERT INTO provisioning_jobs (action, status, requested_by, project_id, request_payload)
         VALUES ('deprovision', 'pending', $1, $2, $3)
         RETURNING id",
    )
    .bind(claims.sub)
    .bind(project_id)
    .bind(json!({
        "app_key": row.app_key,
        "env": row.env,
        "point_in_time": body.point_in_time,
        "snapshot_id": body.snapshot_id
    }))
    .fetch_one(&state.db)
    .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "job_id": job_id,
            "status": "pending",
            "message": "Restore job queued. Poll /jobs/:job_id for status."
        })),
    ))
}
