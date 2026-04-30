use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    middleware::AuthUser,
    models::branch::Branch,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/:project_id/branches",
            get(list_branches).post(create_branch),
        )
        .route(
            "/projects/:project_id/branches/:branch_id",
            delete(delete_branch),
        )
}

// ---------------------------------------------------------------------------
// GET /projects/:project_id/branches
// ---------------------------------------------------------------------------

async fn list_branches(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let branches = sqlx::query_as::<_, Branch>(
        "SELECT * FROM project_branches WHERE project_id=$1 AND status='active' ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "branches": branches })))
}

// ---------------------------------------------------------------------------
// POST /projects/:project_id/branches
// Branch cap is enforced by DB trigger (returns BRANCH_LIMIT_EXCEEDED on 409)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateBranchRequest {
    pub branch_name: String,
}

async fn create_branch(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<CreateBranchRequest>,
) -> Result<(StatusCode, Json<Branch>)> {
    let branch_name = body.branch_name.trim().to_lowercase();

    // Look up source database (prod / default branch)
    let source_db: String = sqlx::query_scalar(
        "SELECT database_name FROM project_databases WHERE project_id=$1 LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    // Derive branch DB name: <source>_br_<branch_name>
    let branch_db = format!("{source_db}_br_{branch_name}");

    // Insert — DB trigger will raise if >= 10 active branches
    // The error type maps to 409 BRANCH_LIMIT_EXCEEDED in AppError
    let branch: Branch = sqlx::query_as(
        "INSERT INTO project_branches
             (project_id, branch_name, database_name, source_database, status, created_by)
         VALUES ($1, $2, $3, $4, 'active', $5)
         RETURNING *",
    )
    .bind(project_id)
    .bind(&branch_name)
    .bind(&branch_db)
    .bind(&source_db)
    .bind(claims.sub)
    .fetch_one(&state.db)
    .await?;

    // Audit
    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'branch.created', 'branch', $2, $3)",
    )
    .bind(claims.sub)
    .bind(branch.id.to_string())
    .bind(json!({ "branch_name": &branch_name, "database": &branch_db }))
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(branch)))
}

// ---------------------------------------------------------------------------
// DELETE /projects/:project_id/branches/:branch_id
// ---------------------------------------------------------------------------

async fn delete_branch(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path((project_id, branch_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let branch = sqlx::query_as::<_, Branch>(
        "SELECT * FROM project_branches WHERE id=$1 AND project_id=$2 AND status='active'",
    )
    .bind(branch_id)
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    sqlx::query("UPDATE project_branches SET status='deleted' WHERE id=$1")
        .bind(branch.id)
        .execute(&state.db)
        .await?;

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'branch.deleted', 'branch', $2, $3)",
    )
    .bind(claims.sub)
    .bind(branch.id.to_string())
    .bind(json!({ "branch_name": branch.branch_name }))
    .execute(&state.db)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}
