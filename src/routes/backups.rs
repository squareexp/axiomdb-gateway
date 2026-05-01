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
            "/projects/:project_id/backups/restore-plan",
            post(plan_restore),
        )
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

    // Run the backup catalog path. This intentionally does not call `list`,
    // which returns database inventory rather than backup state.
    let output = tokio::process::Command::new(&state.cfg.dbctl_bin)
        .arg("backups")
        .output()
        .await
        .map_err(|e| AppError::Executor(e.to_string()))?;

    if !output.status.success() {
        return Err(AppError::Executor(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

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
    pub restore_plan_id: Option<Uuid>,
    pub target_branch_name: Option<String>,
    pub point_in_time: Option<String>,
    pub snapshot_id: Option<String>,
    pub confirm: Option<String>,
}

async fn plan_restore(
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

    let mut command = tokio::process::Command::new(&state.cfg.dbctl_bin);
    command.args([
        "backup-restore",
        "--app",
        &row.app_key,
        "--env",
        &row.env,
        "--confirm",
        "plan",
    ]);
    if let Some(point_in_time) = body
        .point_in_time
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        command.args(["--point-in-time", point_in_time]);
    }

    let output = command
        .output()
        .await
        .map_err(|e| AppError::Executor(e.to_string()))?;

    if !output.status.success() {
        return Err(AppError::Executor(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let plan = serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or(json!({ "raw": stdout.trim() }));

    let restore_plan_id: Uuid = sqlx::query_scalar(
        "INSERT INTO provisioning_jobs (action, status, requested_by, project_id, request_payload, output)
         VALUES ('restore_plan', 'succeeded', $1, $2, $3, $4)
         RETURNING id",
    )
    .bind(claims.sub)
    .bind(project_id)
    .bind(json!({
        "app_key": row.app_key,
        "env": row.env,
        "point_in_time": body.point_in_time,
        "snapshot_id": body.snapshot_id,
        "target_branch_name": body.target_branch_name
    }))
    .bind(plan)
    .fetch_one(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'backup.restore_planned', 'project', $2, $3)",
    )
    .bind(claims.sub)
    .bind(project_id.to_string())
    .bind(json!({ "restore_plan_id": restore_plan_id }))
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "restore_plan_id": restore_plan_id,
            "status": "planned",
            "plan": serde_json::from_str::<serde_json::Value>(&stdout)
                .unwrap_or(json!({ "raw": stdout.trim() }))
        })),
    ))
}

async fn restore_backup(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    crate::middleware::require_role(&claims.role, &["owner", "admin"])?;
    if body.confirm.as_deref() != Some("restore backup") {
        return Err(AppError::Validation(
            "confirm must be exactly: restore backup".into(),
        ));
    }

    let row = sqlx::query_as::<_, AppEnvRow>("SELECT app_key, env FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let job_id: Uuid = sqlx::query_scalar(
        "INSERT INTO provisioning_jobs (action, status, requested_by, project_id, request_payload)
         VALUES ('restore', 'pending', $1, $2, $3)
         RETURNING id",
    )
    .bind(claims.sub)
    .bind(project_id)
    .bind(json!({
        "app_key": row.app_key,
        "env": row.env,
        "restore_plan_id": body.restore_plan_id,
        "target_branch_name": body.target_branch_name,
        "point_in_time": body.point_in_time,
        "snapshot_id": body.snapshot_id
    }))
    .fetch_one(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'backup.restore_started', 'project', $2, $3)",
    )
    .bind(claims.sub)
    .bind(project_id.to_string())
    .bind(json!({ "job_id": job_id, "restore_plan_id": body.restore_plan_id }))
    .execute(&state.db)
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
