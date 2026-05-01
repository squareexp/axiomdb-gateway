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
    models::{branch::Branch, project::Project},
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
        .route(
            "/projects/:project_id/branches/:branch_ref/credentials",
            get(get_branch_credentials),
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

async fn get_branch_credentials(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path((project_id, branch_ref)): Path<(Uuid, String)>,
) -> Result<Json<serde_json::Value>> {
    let branch = if let Ok(branch_id) = Uuid::parse_str(&branch_ref) {
        sqlx::query_as::<_, Branch>(
            "SELECT * FROM project_branches WHERE id=$1 AND project_id=$2 AND status='active'",
        )
        .bind(branch_id)
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, Branch>(
            "SELECT * FROM project_branches WHERE branch_name=$1 AND project_id=$2 AND status='active'",
        )
        .bind(branch_ref.trim().to_lowercase())
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
    }
    .ok_or(AppError::NotFound)?;

    let project =
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id=$1 AND status='active'")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;

    let db_row = sqlx::query_as::<_, crate::models::project::ProjectDatabase>(
        "SELECT * FROM project_databases WHERE project_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let runtime_url = crate::routes::projects::credential_value_from_store(
        &state.cfg.secret_file,
        &db_row.runtime_key,
    )?;
    let direct_url = crate::routes::projects::credential_value_from_store(
        &state.cfg.secret_file,
        &db_row.direct_key,
    )?;
    let runtime_url = crate::routes::projects::canonical_project_url_for_database(
        &runtime_url,
        6432,
        &branch.database_name,
    )?;
    let direct_url = crate::routes::projects::canonical_project_url_for_database(
        &direct_url,
        5432,
        &branch.database_name,
    )?;

    let runtime_key = branch_env_key(
        "DATABASE_URL",
        &project.app_key,
        &project.env,
        &branch.branch_name,
    );
    let direct_key = branch_env_key(
        "DIRECT_URL",
        &project.app_key,
        &project.env,
        &branch.branch_name,
    );

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'branch.credentials.viewed', 'branch', $2, $3)",
    )
    .bind(claims.sub)
    .bind(branch.id.to_string())
    .bind(json!({
        "project_id": project_id,
        "branch_name": &branch.branch_name,
        "database": &branch.database_name
    }))
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "project_id": project_id,
        "branch_id": branch.id,
        "branch_name": branch.branch_name,
        "database": branch.database_name,
        "runtime_key": runtime_key,
        "direct_key": direct_key,
        "database_url": runtime_url,
        "direct_url": direct_url
    })))
}

pub(crate) fn branch_env_key(prefix: &str, app: &str, env: &str, branch: &str) -> String {
    format!(
        "{}_{}_{}_BR_{}",
        prefix,
        key_safe_slug(app, "APP"),
        key_safe_slug(env, "ENV"),
        key_safe_slug(branch, "BRANCH")
    )
}

fn key_safe_slug(value: &str, fallback: &str) -> String {
    let mut output = String::new();
    let mut last_was_separator = false;

    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_uppercase());
            last_was_separator = false;
        } else if !last_was_separator {
            output.push('_');
            last_was_separator = true;
        }
    }

    let output = output.trim_matches('_');
    if output.is_empty() {
        fallback.to_string()
    } else {
        output.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::branch_env_key;
    use crate::routes::projects::canonical_project_url_for_database;

    #[test]
    fn builds_deterministic_branch_env_keys() {
        assert_eq!(
            branch_env_key("DATABASE_URL", "admin4", "dev", "feature-x"),
            "DATABASE_URL_ADMIN4_DEV_BR_FEATURE_X"
        );
        assert_eq!(
            branch_env_key("DIRECT_URL", "square api", "stage-env", "feature 141754"),
            "DIRECT_URL_SQUARE_API_STAGE_ENV_BR_FEATURE_141754"
        );
    }

    #[test]
    fn canonicalizes_branch_urls_to_public_prisma_contract() {
        let runtime = canonical_project_url_for_database(
            "postgresql://app:secret@localhost:5432/sq_admin4_dev?sslmode=disable",
            6432,
            "sq_admin4_dev_br_feature-x",
        )
        .unwrap();
        let direct = canonical_project_url_for_database(
            "postgresql://owner:secret@10.0.0.10:6543/sq_admin4_dev",
            5432,
            "sq_admin4_dev_br_feature-x",
        )
        .unwrap();

        assert!(runtime.contains("@db.squareexp.com:6432/sq_admin4_dev_br_feature-x"));
        assert!(direct.contains("@db.squareexp.com:5432/sq_admin4_dev_br_feature-x"));
        assert!(runtime.contains("sslmode=require"));
        assert!(direct.contains("sslmode=require"));
        assert!(!runtime.contains("localhost"));
        assert!(!direct.contains("10.0.0.10"));
    }
}
