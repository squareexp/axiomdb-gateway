use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum AppError {
    #[error("not found")]
    NotFound,

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("validation error: {0}")]
    Validation(String),

    #[error("branch limit exceeded: max 10 active branches per project")]
    BranchLimitExceeded,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("executor error: {0}")]
    Executor(String),

    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "NOT_FOUND", self.to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", self.to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "FORBIDDEN", self.to_string()),
            AppError::Validation(m) => (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", m.clone()),
            AppError::BranchLimitExceeded => (
                StatusCode::CONFLICT,
                "BRANCH_LIMIT_EXCEEDED",
                self.to_string(),
            ),
            AppError::Conflict(m) => (StatusCode::CONFLICT, "CONFLICT", m.clone()),
            AppError::Executor(m) => {
                tracing::error!("executor: {m}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "EXECUTOR_ERROR",
                    "operation failed".into(),
                )
            }
            AppError::Sqlx(e) => {
                // Detect branch cap trigger
                if e.to_string().contains("branch limit exceeded") {
                    return AppError::BranchLimitExceeded.into_response();
                }
                tracing::error!("sqlx: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DB_ERROR",
                    "database error".into(),
                )
            }
            AppError::Internal(e) => {
                tracing::error!("internal: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "internal server error".into(),
                )
            }
        };

        let body = json!({ "error": { "code": code, "message": message } });
        (status, Json(body)).into_response()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
