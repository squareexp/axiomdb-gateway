use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

/// Row model for the `audit_events` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AuditEvent {
    pub id: Uuid,
    pub actor_user_id: Option<Uuid>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}
