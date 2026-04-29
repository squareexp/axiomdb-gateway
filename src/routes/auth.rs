use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{
    extract::State,
    routing::post,
    Json, Router,
};
use chrono::Utc;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::{AppError, Result}, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh_token))
}

// ---------------------------------------------------------------------------
// JWT claims
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub email: String,
    pub role: String,
    pub exp: usize,
    pub token_type: String,
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub user: UserOut,
}

#[derive(Serialize)]
pub struct UserOut {
    pub id: Uuid,
    pub role: String,
}

async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<LoginResponse>> {
    let user = sqlx::query_as::<_, crate::models::user::User>(
        "SELECT * FROM users WHERE email = $1 AND is_active = true",
    )
    .bind(&body.email)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    // Verify argon2 hash using the argon2 0.5 API
    let parsed_hash =
        PasswordHash::new(&user.password_hash).map_err(|_| AppError::Unauthorized)?;
    Argon2::default()
        .verify_password(body.password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::Unauthorized)?;

    let access = mint_token(
        &user.id,
        &user.email,
        &user.role,
        "access",
        state.cfg.jwt_access_ttl_secs,
        &state.cfg.jwt_secret,
    )?;
    let refresh = mint_token(
        &user.id,
        &user.email,
        &user.role,
        "refresh",
        state.cfg.jwt_refresh_ttl_secs,
        &state.cfg.jwt_secret,
    )?;

    Ok(Json(LoginResponse {
        access_token: access,
        refresh_token: refresh,
        user: UserOut { id: user.id, role: user.role },
    }))
}

// ---------------------------------------------------------------------------
// Refresh
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

async fn refresh_token(
    State(state): State<AppState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<serde_json::Value>> {
    let data = jsonwebtoken::decode::<Claims>(
        &body.refresh_token,
        &jsonwebtoken::DecodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;

    if data.claims.token_type != "refresh" {
        return Err(AppError::Unauthorized);
    }

    let claims = data.claims;
    let access = mint_token(
        &claims.sub,
        &claims.email,
        &claims.role,
        "access",
        state.cfg.jwt_access_ttl_secs,
        &state.cfg.jwt_secret,
    )?;

    Ok(Json(serde_json::json!({ "access_token": access })))
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn mint_token(
    user_id: &Uuid,
    email: &str,
    role: &str,
    token_type: &str,
    ttl_secs: u64,
    secret: &str,
) -> Result<String> {
    let exp = (Utc::now().timestamp() as u64 + ttl_secs) as usize;
    let claims = Claims {
        sub: *user_id,
        email: email.to_string(),
        role: role.to_string(),
        exp,
        token_type: token_type.to_string(),
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("jwt encode: {e}")))
}
