use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Row model for the `users` table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    /// Never serialised to API responses.
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Subset returned to callers (no password_hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct UserPublic {
    pub id: Uuid,
    pub email: String,
    pub role: String,
}

impl From<User> for UserPublic {
    fn from(u: User) -> Self {
        Self { id: u.id, email: u.email, role: u.role }
    }
}
