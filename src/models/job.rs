use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

/// Row model for the `provisioning_jobs` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Job {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub action: String,
    pub status: String,
    pub requested_by: Uuid,
    pub request_payload: Value,
    pub output: Value,
    pub error_text: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
