use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Row model for the `project_branches` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Branch {
    pub id: Uuid,
    pub project_id: Uuid,
    pub parent_branch_id: Option<Uuid>,
    pub branch_name: String,
    pub database_name: String,
    pub source_database: String,
    pub status: String,
    pub is_default: bool,
    pub protected: bool,
    pub lifespan: String,
    pub expires_at: Option<DateTime<Utc>>,
    pub ttl_seconds: Option<i64>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
