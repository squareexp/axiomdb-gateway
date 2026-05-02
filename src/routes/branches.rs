use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use chrono::{Duration, Utc};
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
        .route(
            "/projects/:project_id/branches/:branch_ref/metrics/summary",
            get(get_branch_metrics),
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
        "SELECT * FROM project_branches
         WHERE project_id=$1 AND status <> 'deleted'
         ORDER BY is_default DESC, created_at",
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
    pub source_branch_id: Option<Uuid>,
    pub lifespan: Option<String>,
}

async fn create_branch(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<CreateBranchRequest>,
) -> Result<(StatusCode, Json<Branch>)> {
    let branch_name = body.branch_name.trim().to_lowercase();
    if branch_name.is_empty()
        || !branch_name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(AppError::Validation(
            "branch_name must use lowercase letters, digits, hyphens, or underscores".into(),
        ));
    }
    if branch_name == "main" {
        return Err(AppError::Validation(
            "main is reserved and already created with each project".into(),
        ));
    }

    let lifespan = normalize_lifespan(body.lifespan.as_deref())?;
    let (ttl_seconds, expires_at) = lifespan_window(&lifespan);

    let active_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM project_branches WHERE project_id=$1 AND status='active'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await?;
    if active_count >= 10 {
        return Err(AppError::BranchLimitExceeded);
    }

    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM project_branches WHERE project_id=$1 AND branch_name=$2 AND status='active'",
    )
    .bind(project_id)
    .bind(&branch_name)
    .fetch_optional(&state.db)
    .await?;
    if existing.is_some() {
        return Err(AppError::Conflict(format!(
            "branch already exists: {branch_name}"
        )));
    }

    let project =
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id=$1 AND status='active'")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;

    let usage = crate::executor::run_dbctl_json(
        &state.cfg.dbctl_bin,
        &[
            "project-usage",
            "--app",
            &project.app_key,
            "--env",
            &project.env,
        ],
        30,
    )
    .await
    .map_err(|error| AppError::Executor(error.to_string()))?;
    let storage_used = usage
        .get("storageUsedBytes")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let storage_limit = usage
        .get("storageLimitBytes")
        .and_then(|value| value.as_i64())
        .unwrap_or(i64::MAX);
    if storage_used >= storage_limit {
        return Err(AppError::Conflict(
            "project storage quota is full; extend storage before creating another branch".into(),
        ));
    }

    // Look up source database (prod / default branch)
    let db_row = sqlx::query_as::<_, crate::models::project::ProjectDatabase>(
        "SELECT * FROM project_databases WHERE project_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let source_branch = match body.source_branch_id {
        Some(source_branch_id) => sqlx::query_as::<_, Branch>(
            "SELECT * FROM project_branches
             WHERE id=$1 AND project_id=$2 AND status='active' AND deleted_at IS NULL",
        )
        .bind(source_branch_id)
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?,
        None => sqlx::query_as::<_, Branch>(
            "SELECT * FROM project_branches
             WHERE project_id=$1 AND is_default=true AND status='active' AND deleted_at IS NULL
             ORDER BY created_at ASC LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?,
    };

    // Derive branch DB name: <source>_br_<branch_name>
    let branch_db = format!("{}_br_{branch_name}", db_row.database_name);

    crate::executor::run_branch_create(
        &state.cfg.dbctl_bin,
        &source_branch.database_name,
        &branch_db,
        &db_row.owner_role,
        &db_row.runtime_role,
        &db_row.readonly_role,
    )
    .await
    .map_err(|e| AppError::Executor(e.to_string()))?;

    // Insert — DB trigger will raise if >= 10 active branches
    // The error type maps to 409 BRANCH_LIMIT_EXCEEDED in AppError
    let branch: Branch = sqlx::query_as(
        "INSERT INTO project_branches
             (project_id, parent_branch_id, branch_name, database_name, source_database, status,
              created_by, lifespan, expires_at, ttl_seconds, is_default, protected)
         VALUES ($1, $2, $3, $4, $5, 'active', $6, $7, $8, $9, false, false)
         RETURNING *",
    )
    .bind(project_id)
    .bind(source_branch.id)
    .bind(&branch_name)
    .bind(&branch_db)
    .bind(&source_branch.database_name)
    .bind(claims.sub)
    .bind(&lifespan)
    .bind(expires_at)
    .bind(ttl_seconds)
    .fetch_one(&state.db)
    .await?;

    // Audit
    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'branch.created', 'branch', $2, $3)",
    )
    .bind(claims.sub)
    .bind(branch.id.to_string())
    .bind(json!({
        "branch_name": &branch_name,
        "database": &branch_db,
        "source_branch_id": source_branch.id,
        "source_database": source_branch.database_name,
        "lifespan": &lifespan,
        "expires_at": expires_at
    }))
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

    if branch.protected || branch.is_default || branch.branch_name == "main" {
        return Err(AppError::Forbidden);
    }

    sqlx::query(
        "UPDATE project_branches
         SET status='deleted', deleted_at=now(), updated_at=now()
         WHERE id=$1",
    )
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

fn normalize_lifespan(value: Option<&str>) -> Result<String> {
    let normalized = value.unwrap_or("7d").trim().to_lowercase();
    match normalized.as_str() {
        "7d" | "7days" | "7 days" => Ok("7d".to_string()),
        "1m" | "1month" | "1 month" => Ok("1m".to_string()),
        "6m" | "6months" | "6 months" => Ok("6m".to_string()),
        "1y" | "1year" | "1 year" => Ok("1y".to_string()),
        "forever" | "permanent" => Ok("forever".to_string()),
        _ => Err(AppError::Validation(
            "lifespan must be one of 7d, 1m, 6m, 1y, forever".into(),
        )),
    }
}

fn lifespan_window(lifespan: &str) -> (Option<i64>, Option<chrono::DateTime<Utc>>) {
    let seconds = match lifespan {
        "7d" => Some(7 * 24 * 60 * 60),
        "1m" => Some(30 * 24 * 60 * 60),
        "6m" => Some(183 * 24 * 60 * 60),
        "1y" => Some(365 * 24 * 60 * 60),
        _ => None,
    };

    let expires_at = seconds.map(|seconds| Utc::now() + Duration::seconds(seconds));
    (seconds, expires_at)
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

async fn get_branch_metrics(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
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

    let metrics = crate::executor::run_dbctl_json(
        &state.cfg.dbctl_bin,
        &["database-metrics", "--database", &branch.database_name],
        30,
    )
    .await
    .map_err(|error| AppError::Executor(error.to_string()))?;
    let usage = crate::executor::run_dbctl_json(
        &state.cfg.dbctl_bin,
        &[
            "project-usage",
            "--app",
            &project.app_key,
            "--env",
            &project.env,
        ],
        30,
    )
    .await
    .map_err(|error| AppError::Executor(error.to_string()))?;

    Ok(Json(json!({
        "project_id": project_id,
        "branch_id": branch.id,
        "branch_name": branch.branch_name,
        "database": branch.database_name,
        "metrics": metrics,
        "usage": usage
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
    use super::{branch_env_key, lifespan_window, normalize_lifespan};
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

    #[test]
    fn normalizes_branch_lifespan_values() {
        assert_eq!(normalize_lifespan(None).unwrap(), "7d");
        assert_eq!(normalize_lifespan(Some("1 month")).unwrap(), "1m");
        assert_eq!(normalize_lifespan(Some("permanent")).unwrap(), "forever");
        assert!(normalize_lifespan(Some("3d")).is_err());
    }

    #[test]
    fn forever_lifespan_has_no_expiry() {
        let (ttl, expires_at) = lifespan_window("forever");
        assert!(ttl.is_none());
        assert!(expires_at.is_none());
    }
}
