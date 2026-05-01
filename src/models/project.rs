use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Row model for the `projects` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Project {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub app_key: String,
    pub env: String,
    pub status: String,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Row model for the `project_databases` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ProjectDatabase {
    pub id: Uuid,
    pub project_id: Uuid,
    pub database_name: String,
    pub owner_role: String,
    pub runtime_role: String,
    pub readonly_role: String,
    /// Env-var key name — never the actual connection string.
    pub runtime_key: String,
    /// Env-var key name — never the actual connection string.
    pub direct_key: String,
    pub created_at: DateTime<Utc>,
}

/// API response shape for project creation.
#[derive(Debug, Serialize)]
pub struct CreateProjectResponse {
    pub project: ProjectOut,
    pub main_branch: BranchOut,
    /// Backward-compatible metadata for older CLI/Web clients. This is key
    /// metadata only, never raw credentials.
    pub database: DatabaseOut,
}

#[derive(Debug, Serialize)]
pub struct ProjectOut {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub app_key: String,
    pub env: String,
}

#[derive(Debug, Serialize)]
pub struct DatabaseOut {
    pub database_name: String,
    pub runtime_key: String,
    pub direct_key: String,
}

#[derive(Debug, Serialize)]
pub struct BranchOut {
    pub id: Uuid,
    pub branch_name: String,
    pub database_name: String,
    pub source_database: String,
    pub lifespan: String,
    pub protected: bool,
    pub is_default: bool,
    pub status: String,
    pub expires_at: Option<DateTime<Utc>>,
}
