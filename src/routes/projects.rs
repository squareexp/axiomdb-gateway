use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sqlx::FromRow;
use tokio::time::{sleep, Duration};
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    executor,
    middleware::{require_role, AuthUser},
    models::{
        branch::Branch,
        project::{
            BranchOut, CreateProjectResponse, DatabaseOut, Project, ProjectDatabase, ProjectOut,
        },
    },
    state::AppState,
};

const CANONICAL_DB_HOST: &str = "db.squareexp.com";
const RUNTIME_DB_PORT: u16 = 6432;
const DIRECT_DB_PORT: u16 = 5432;
const PROJECT_CREATE_LOCK_WAIT_ATTEMPTS: usize = 20;
const PROJECT_CREATE_LOCK_WAIT_MS: u64 = 500;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/:project_id", get(get_project))
        .route(
            "/projects/:project_id/credentials",
            get(get_project_credentials),
        )
}

// ---------------------------------------------------------------------------
// GET /projects
// ---------------------------------------------------------------------------

async fn list_projects(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
) -> Result<Json<serde_json::Value>> {
    let projects = sqlx::query_as::<_, Project>(
        "SELECT * FROM projects WHERE status = 'active' ORDER BY created_at DESC",
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "projects": projects })))
}

// ---------------------------------------------------------------------------
// POST /projects  — the E2E guaranteed route from the spec
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    pub app_key: String,
    pub env: String,
}

async fn create_project(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Json(body): Json<CreateProjectRequest>,
) -> Result<(StatusCode, Json<CreateProjectResponse>)> {
    require_role(&claims.role, &["owner", "admin", "operator"])?;

    let app = normalize_name("app_key", &body.app_key)?;
    let env = normalize_name("env", &body.env)?;
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::Validation("name is required".into()));
    }

    let slug = format!("{app}-{env}");
    let lock_key = format!("project:create:{slug}");

    let mut lock = state.db.acquire().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtext($1))")
        .bind(&lock_key)
        .fetch_one(&mut *lock)
        .await?;

    if !acquired {
        drop(lock);
        for _ in 0..PROJECT_CREATE_LOCK_WAIT_ATTEMPTS {
            sleep(Duration::from_millis(PROJECT_CREATE_LOCK_WAIT_MS)).await;
            if let Some(existing) = load_create_project_response(&state, &app, &env).await? {
                return Ok((StatusCode::OK, Json(existing)));
            }
        }

        return Err(AppError::Conflict(format!(
            "project {slug} is already provisioning; refresh in a few seconds"
        )));
    }

    let result = create_project_locked(&state, &claims, name, &app, &env, &slug).await;
    if let Err(error) = sqlx::query("SELECT pg_advisory_unlock(hashtext($1))")
        .bind(&lock_key)
        .execute(&mut *lock)
        .await
    {
        tracing::error!("failed to release project create lock for {slug}: {error}");
    }

    result
}

async fn create_project_locked(
    state: &AppState,
    claims: &crate::routes::auth::Claims,
    name: &str,
    app: &str,
    env: &str,
    slug: &str,
) -> Result<(StatusCode, Json<CreateProjectResponse>)> {
    if let Some(existing) = load_create_project_response(state, app, env).await? {
        return Ok((StatusCode::OK, Json(existing)));
    }

    // Step 1: Insert pending provisioning job
    let job_id: Uuid = sqlx::query_scalar(
        "INSERT INTO provisioning_jobs
             (action, status, requested_by, project_id, request_payload)
         VALUES ('provision', 'pending', $1, NULL, $2)
         RETURNING id",
    )
    .bind(claims.sub)
    .bind(json!({ "app": app, "env": env, "name": name }))
    .fetch_one(&state.db)
    .await?;

    // Mark running
    sqlx::query("UPDATE provisioning_jobs SET status='running', started_at=now() WHERE id=$1")
        .bind(job_id)
        .execute(&state.db)
        .await?;

    // Step 2: Execute real VPS provision via square-dbctl
    let prov = match executor::run_provision(&state.cfg.dbctl_bin, app, env).await {
        Ok(p) => p,
        Err(e) => {
            sqlx::query(
                "UPDATE provisioning_jobs
                 SET status='failed', finished_at=now(), error_text=$1
                 WHERE id=$2",
            )
            .bind(e.to_string())
            .bind(job_id)
            .execute(&state.db)
            .await?;
            return Err(AppError::Executor(e.to_string()));
        }
    };

    // Step 3: Persist in a transaction
    let mut tx = state.db.begin().await?;

    let project: Project = sqlx::query_as(
        "INSERT INTO projects (slug, name, app_key, env, status, created_by)
         VALUES ($1, $2, $3, $4, 'active', $5)
         ON CONFLICT (app_key, env) DO UPDATE
           SET slug = EXCLUDED.slug,
               name = EXCLUDED.name,
               status = 'active',
               updated_at = now()
         RETURNING *",
    )
    .bind(slug)
    .bind(name)
    .bind(app)
    .bind(env)
    .bind(claims.sub)
    .fetch_one(&mut *tx)
    .await?;

    let db_row: ProjectDatabase = sqlx::query_as(
        "INSERT INTO project_databases
             (project_id, database_name, owner_role, runtime_role, readonly_role, runtime_key, direct_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (database_name) DO UPDATE
           SET project_id = EXCLUDED.project_id,
               owner_role = EXCLUDED.owner_role,
               runtime_role = EXCLUDED.runtime_role,
               readonly_role = EXCLUDED.readonly_role,
               runtime_key = EXCLUDED.runtime_key,
               direct_key = EXCLUDED.direct_key
         RETURNING *",
    )
    .bind(project.id)
    .bind(&prov.database)
    .bind(&prov.owner_role)
    .bind(&prov.runtime_role)
    .bind(&prov.readonly_role)
    .bind(&prov.runtime_key)
    .bind(&prov.direct_key)
    .fetch_one(&mut *tx)
    .await?;

    let main_branch: Branch = sqlx::query_as(
        "INSERT INTO project_branches
             (project_id, branch_name, database_name, source_database, status, created_by,
              is_default, protected, lifespan)
         VALUES ($1, 'main', $2, $2, 'active', $3, true, true, 'forever')
         ON CONFLICT (project_id, branch_name) DO UPDATE
           SET database_name = EXCLUDED.database_name,
               source_database = EXCLUDED.source_database,
               status = 'active',
               is_default = true,
               protected = true,
               lifespan = 'forever',
               expires_at = NULL,
               deleted_at = NULL,
               updated_at = now()
         RETURNING *",
    )
    .bind(project.id)
    .bind(&prov.database)
    .bind(claims.sub)
    .fetch_one(&mut *tx)
    .await?;

    // Mark job succeeded + link to project
    sqlx::query(
        "UPDATE provisioning_jobs
         SET status='succeeded', finished_at=now(), project_id=$1, output=$2
         WHERE id=$3",
    )
    .bind(project.id)
    .bind(json!({
        "database": prov.database,
        "runtime_key": prov.runtime_key,
        "direct_key": prov.direct_key,
        "stdout": prov.raw_stdout,
    }))
    .bind(job_id)
    .execute(&mut *tx)
    .await?;

    // Audit
    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'project.created', 'project', $2, $3)",
    )
    .bind(claims.sub)
    .bind(project.id.to_string())
    .bind(json!({ "slug": slug, "database": &prov.database }))
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'branch.created', 'branch', $2, $3)",
    )
    .bind(claims.sub)
    .bind(main_branch.id.to_string())
    .bind(json!({
        "branch_name": "main",
        "database": &prov.database,
        "protected": true,
        "lifespan": "forever"
    }))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // Step 4: Return — key names only, never raw URLs
    Ok((
        StatusCode::CREATED,
        Json(create_project_response(project, db_row, main_branch)),
    ))
}

fn normalize_name(label: &str, value: &str) -> Result<String> {
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty()
        || !normalized
            .chars()
            .enumerate()
            .all(|(idx, c)| c.is_ascii_lowercase() || c.is_ascii_digit() && idx > 0 || c == '_')
        || !normalized
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
    {
        return Err(AppError::Validation(format!(
            "{label} must start with a lowercase letter and use only lowercase letters, digits, or underscores"
        )));
    }
    Ok(normalized)
}

async fn load_create_project_response(
    state: &AppState,
    app: &str,
    env: &str,
) -> Result<Option<CreateProjectResponse>> {
    let row = sqlx::query_as::<_, CreateProjectJoinedRow>(
        "SELECT
           p.id AS project_id,
           p.slug AS project_slug,
           p.name AS project_name,
           p.app_key AS project_app_key,
           p.env AS project_env,
           d.database_name AS database_name,
           d.runtime_key AS runtime_key,
           d.direct_key AS direct_key,
           b.id AS branch_id,
           b.branch_name AS branch_name,
           b.database_name AS branch_database_name,
           b.source_database AS branch_source_database,
           b.lifespan AS branch_lifespan,
           b.protected AS branch_protected,
           b.is_default AS branch_is_default,
           b.status AS branch_status,
           b.expires_at AS branch_expires_at
         FROM projects p
         JOIN project_databases d ON d.project_id = p.id
         JOIN project_branches b
           ON b.project_id = p.id
          AND b.branch_name = 'main'
          AND b.status = 'active'
          AND b.deleted_at IS NULL
         WHERE p.app_key=$1
           AND p.env=$2
           AND p.status='active'
         ORDER BY p.created_at ASC
         LIMIT 1",
    )
    .bind(app)
    .bind(env)
    .fetch_optional(&state.db)
    .await?;

    Ok(row.map(CreateProjectJoinedRow::into_response))
}

#[derive(FromRow)]
struct CreateProjectJoinedRow {
    project_id: Uuid,
    project_slug: String,
    project_name: String,
    project_app_key: String,
    project_env: String,
    database_name: String,
    runtime_key: String,
    direct_key: String,
    branch_id: Uuid,
    branch_name: String,
    branch_database_name: String,
    branch_source_database: String,
    branch_lifespan: String,
    branch_protected: bool,
    branch_is_default: bool,
    branch_status: String,
    branch_expires_at: Option<DateTime<Utc>>,
}

impl CreateProjectJoinedRow {
    fn into_response(self) -> CreateProjectResponse {
        CreateProjectResponse {
            project: ProjectOut {
                id: self.project_id,
                slug: self.project_slug,
                name: self.project_name,
                app_key: self.project_app_key,
                env: self.project_env,
            },
            main_branch: BranchOut {
                id: self.branch_id,
                branch_name: self.branch_name,
                database_name: self.branch_database_name,
                source_database: self.branch_source_database,
                lifespan: self.branch_lifespan,
                protected: self.branch_protected,
                is_default: self.branch_is_default,
                status: self.branch_status,
                expires_at: self.branch_expires_at,
            },
            database: DatabaseOut {
                database_name: self.database_name,
                runtime_key: self.runtime_key,
                direct_key: self.direct_key,
            },
        }
    }
}

fn create_project_response(
    project: Project,
    db_row: ProjectDatabase,
    main_branch: Branch,
) -> CreateProjectResponse {
    CreateProjectResponse {
        project: ProjectOut {
            id: project.id,
            slug: project.slug,
            name: project.name,
            app_key: project.app_key,
            env: project.env,
        },
        main_branch: BranchOut {
            id: main_branch.id,
            branch_name: main_branch.branch_name,
            database_name: main_branch.database_name,
            source_database: main_branch.source_database,
            lifespan: main_branch.lifespan,
            protected: main_branch.protected,
            is_default: main_branch.is_default,
            status: main_branch.status,
            expires_at: main_branch.expires_at,
        },
        database: DatabaseOut {
            database_name: db_row.database_name,
            runtime_key: db_row.runtime_key,
            direct_key: db_row.direct_key,
        },
    }
}

// ---------------------------------------------------------------------------
// GET /projects/:project_id
// ---------------------------------------------------------------------------

async fn get_project(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let project = sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;

    let databases =
        sqlx::query_as::<_, ProjectDatabase>("SELECT * FROM project_databases WHERE project_id=$1")
            .bind(project.id)
            .fetch_all(&state.db)
            .await?;

    let branches = sqlx::query_as::<_, Branch>(
        "SELECT * FROM project_branches
         WHERE project_id=$1 AND status <> 'deleted'
         ORDER BY is_default DESC, created_at ASC",
    )
    .bind(project.id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        json!({ "project": project, "databases": databases, "branches": branches }),
    ))
}

async fn get_project_credentials(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let db_row = sqlx::query_as::<_, ProjectDatabase>(
        "SELECT * FROM project_databases WHERE project_id=$1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;

    let runtime_url = credential_value_from_store(&state.cfg.secret_file, &db_row.runtime_key)?;
    let direct_url = credential_value_from_store(&state.cfg.secret_file, &db_row.direct_key)?;
    let runtime_url = canonical_project_url(&runtime_url, RUNTIME_DB_PORT)?;
    let direct_url = canonical_project_url(&direct_url, DIRECT_DB_PORT)?;

    Ok(Json(json!({
        "project_id": project_id,
        "database": db_row.database_name,
        "runtime_key": "DATABASE_URL",
        "direct_key": "DIRECT_URL",
        "database_url": runtime_url,
        "direct_url": direct_url
    })))
}

pub(crate) fn credential_value_from_store(secret_file: &str, key: &str) -> Result<String> {
    let env_contents = std::fs::read_to_string(secret_file)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("failed reading env store: {e}")))?;

    env_contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(|v| v.to_string()))
        .ok_or(AppError::NotFound)
}

fn canonical_project_url(raw: &str, port: u16) -> Result<String> {
    canonical_project_url_with_database(raw, port, None)
}

pub(crate) fn canonical_project_url_for_database(
    raw: &str,
    port: u16,
    database: &str,
) -> Result<String> {
    canonical_project_url_with_database(raw, port, Some(database))
}

fn canonical_project_url_with_database(
    raw: &str,
    port: u16,
    database_override: Option<&str>,
) -> Result<String> {
    let url = raw.trim().trim_matches('"').trim_matches('\'');
    let body = url.strip_prefix("postgresql://").ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "stored Postgres URL must use postgresql://"
        ))
    })?;
    let (userinfo, host_path) = body.split_once('@').ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "stored Postgres URL is missing credentials"
        ))
    })?;
    let (_, db_and_query) = host_path.split_once('/').ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!(
            "stored Postgres URL is missing database name"
        ))
    })?;
    let (database, query) = db_and_query
        .split_once('?')
        .map_or((db_and_query, ""), |(database, query)| (database, query));

    let database = database_override.unwrap_or(database);

    if userinfo.is_empty() || database.is_empty() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "stored Postgres URL is incomplete"
        )));
    }

    let mut params = vec!["sslmode=require".to_string()];
    params.extend(
        query
            .split('&')
            .filter(|part| !part.is_empty())
            .filter(|part| !part.starts_with("sslmode="))
            .map(str::to_string),
    );

    Ok(format!(
        "postgresql://{userinfo}@{CANONICAL_DB_HOST}:{port}/{database}?{}",
        params.join("&")
    ))
}

#[cfg(test)]
mod tests {
    use super::{canonical_project_url, DIRECT_DB_PORT, RUNTIME_DB_PORT};

    #[test]
    fn canonicalizes_runtime_url_to_public_pooler() {
        let got = canonical_project_url(
            "postgresql://app:pass@127.0.0.1:5432/sq_app_dev?sslmode=disable",
            RUNTIME_DB_PORT,
        )
        .unwrap();

        assert_eq!(
            got,
            "postgresql://app:pass@db.squareexp.com:6432/sq_app_dev?sslmode=require"
        );
    }

    #[test]
    fn canonicalizes_direct_url_and_preserves_extra_params() {
        let got = canonical_project_url(
            "\"postgresql://owner:pass@internal:6543/sq_app_dev?connect_timeout=10\"",
            DIRECT_DB_PORT,
        )
        .unwrap();

        assert_eq!(
            got,
            "postgresql://owner:pass@db.squareexp.com:5432/sq_app_dev?sslmode=require&connect_timeout=10"
        );
    }
}
