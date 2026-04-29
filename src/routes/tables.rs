use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{error::Result, middleware::AuthUser, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/:project_id/tables", get(list_tables))
        .route("/projects/:project_id/tables/:table/rows", get(get_rows))
}

#[derive(Deserialize)]
pub struct RowsQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_tables(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    // Fetch the database name from control plane
    let db_name: Option<String> = sqlx::query_scalar(
        "SELECT database_name FROM project_databases WHERE project_id=$1 LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;

    // Query the information_schema for the project database's tables.
    // In v1 we query the control-plane DB; phase 2 uses a project-specific pool.
    let table_names: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema='public'
         ORDER BY table_name",
    )
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "tables": table_names, "database": db_name })))
}

async fn get_rows(
    AuthUser(_claims): AuthUser,
    Path((_project_id, table)): Path<(Uuid, String)>,
    Query(q): Query<RowsQuery>,
) -> Result<Json<serde_json::Value>> {
    let limit = q.limit.unwrap_or(100).min(500);
    let offset = q.offset.unwrap_or(0);

    // Validate table name: alphanumeric + underscore only
    if !table.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Err(crate::error::AppError::Validation("invalid table name".into()));
    }

    Ok(Json(json!({
        "table": table,
        "rows": [],
        "limit": limit,
        "offset": offset,
        "note": "Direct table query requires project-specific DB connection pool (phase 2)"
    })))
}
