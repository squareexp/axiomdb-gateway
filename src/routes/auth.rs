use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::{extract::State, routing::post, Json, Router};
use chrono::Utc;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use totp_rs::{Algorithm, Secret, TOTP};
use tracing::info;
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh_token))
        .route("/auth/2fa/setup", post(setup_2fa))
        .route("/auth/2fa/verify-setup", post(verify_setup_2fa))
        .route("/auth/2fa/verify-login", post(verify_login_2fa))
        .route("/auth/2fa/skip", post(skip_2fa))
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
pub struct AuthResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub pre_auth_token: Option<String>,
    pub requires_2fa: bool,
    pub requires_2fa_setup: bool,
    pub method: Option<String>,
    pub user: Option<UserOut>,
}

#[derive(Serialize)]
pub struct UserOut {
    pub id: Uuid,
    pub role: String,
}

async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<AuthResponse>> {
    let user = sqlx::query_as::<_, crate::models::user::User>(
        "SELECT * FROM users WHERE email = $1 AND is_active = true",
    )
    .bind(&body.email)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let parsed_hash = PasswordHash::new(&user.password_hash).map_err(|_| AppError::Unauthorized)?;
    Argon2::default()
        .verify_password(body.password.as_bytes(), &parsed_hash)
        .map_err(|_| AppError::Unauthorized)?;

    if !user.two_factor_setup_completed {
        let pre_auth = mint_token(
            &user.id,
            &user.email,
            &user.role,
            "pre_auth",
            3600,
            &state.cfg.jwt_secret,
        )?;
        return Ok(Json(AuthResponse {
            access_token: None,
            refresh_token: None,
            pre_auth_token: Some(pre_auth),
            requires_2fa: false,
            requires_2fa_setup: true,
            method: None,
            user: None,
        }));
    }

    if user.two_factor_enabled {
        let pre_auth = mint_token(
            &user.id,
            &user.email,
            &user.role,
            "pre_auth",
            300,
            &state.cfg.jwt_secret,
        )?;

        // If email method, dispatch code to server logs
        if user.two_factor_method.as_deref() == Some("email") {
            if let Some(secret) = &user.two_factor_secret {
                let totp = TOTP::new(
                    Algorithm::SHA1,
                    6,
                    1,
                    30,
                    Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
                )
                .unwrap();
                let code = totp.generate_current().unwrap();
                info!("📧 [MOCK EMAIL] 2FA Code for {}: {}", user.email, code);
            }
        }

        return Ok(Json(AuthResponse {
            access_token: None,
            refresh_token: None,
            pre_auth_token: Some(pre_auth),
            requires_2fa: true,
            requires_2fa_setup: false,
            method: user.two_factor_method,
            user: None,
        }));
    }

    // Normal Login
    issue_tokens(&state, user)
}

// ---------------------------------------------------------------------------
// 2FA Setup & Verification
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Setup2FARequest {
    pub pre_auth_token: String,
    pub method: String, // "authenticator" or "email"
}

#[derive(Serialize)]
pub struct Setup2FAResponse {
    pub secret: String,
    pub otpauth_url: Option<String>,
}

async fn setup_2fa(
    State(state): State<AppState>,
    Json(body): Json<Setup2FARequest>,
) -> Result<Json<Setup2FAResponse>> {
    let claims = decode_token(&body.pre_auth_token, "pre_auth", &state.cfg.jwt_secret)?;

    let secret = Secret::generate_secret().to_encoded().to_string();

    sqlx::query("UPDATE users SET two_factor_secret = $1, two_factor_method = $2 WHERE id = $3")
        .bind(&secret)
        .bind(&body.method)
        .bind(claims.sub)
        .execute(&state.db)
        .await?;

    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.clone()).to_bytes().unwrap(),
    )
    .unwrap();

    let otpauth_url = if body.method == "authenticator" {
        Some(format!(
            "otpauth://totp/AxiomDB:{}?secret={}&issuer=AxiomDB",
            claims.email, secret
        ))
    } else {
        // Email method -> log it
        let code = totp.generate_current().unwrap();
        info!("[MOCK EMAIL] 2FA Setup Code for {}: {}", claims.email, code);
        None
    };

    Ok(Json(Setup2FAResponse {
        secret,
        otpauth_url,
    }))
}

#[derive(Deserialize)]
pub struct Verify2FARequest {
    pub pre_auth_token: String,
    pub code: String,
}

async fn verify_setup_2fa(
    State(state): State<AppState>,
    Json(body): Json<Verify2FARequest>,
) -> Result<Json<AuthResponse>> {
    let claims = decode_token(&body.pre_auth_token, "pre_auth", &state.cfg.jwt_secret)?;
    let user = get_user(&state, claims.sub).await?;

    verify_totp(&user, &body.code)?;

    sqlx::query("UPDATE users SET two_factor_setup_completed = true, two_factor_enabled = true WHERE id = $1")
        .bind(claims.sub)
        .execute(&state.db)
        .await?;

    issue_tokens(&state, user)
}

async fn verify_login_2fa(
    State(state): State<AppState>,
    Json(body): Json<Verify2FARequest>,
) -> Result<Json<AuthResponse>> {
    let claims = decode_token(&body.pre_auth_token, "pre_auth", &state.cfg.jwt_secret)?;
    let user = get_user(&state, claims.sub).await?;

    verify_totp(&user, &body.code)?;

    issue_tokens(&state, user)
}

#[derive(Deserialize)]
pub struct Skip2FARequest {
    pub pre_auth_token: String,
}

async fn skip_2fa(
    State(state): State<AppState>,
    Json(body): Json<Skip2FARequest>,
) -> Result<Json<AuthResponse>> {
    let claims = decode_token(&body.pre_auth_token, "pre_auth", &state.cfg.jwt_secret)?;

    sqlx::query("UPDATE users SET two_factor_setup_completed = true, two_factor_enabled = false WHERE id = $1")
        .bind(claims.sub)
        .execute(&state.db)
        .await?;

    let user = get_user(&state, claims.sub).await?;
    issue_tokens(&state, user)
}

fn verify_totp(user: &crate::models::user::User, code: &str) -> Result<()> {
    let secret = user
        .two_factor_secret
        .as_ref()
        .ok_or(AppError::Unauthorized)?;
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
    )
    .unwrap();
    if !totp.check_current(code).unwrap_or(false) {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

async fn get_user(state: &AppState, id: Uuid) -> Result<crate::models::user::User> {
    sqlx::query_as::<_, crate::models::user::User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)
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
    let claims = decode_token(&body.refresh_token, "refresh", &state.cfg.jwt_secret)?;
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
// Helpers
// ---------------------------------------------------------------------------

fn issue_tokens(state: &AppState, user: crate::models::user::User) -> Result<Json<AuthResponse>> {
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
    Ok(Json(AuthResponse {
        access_token: Some(access),
        refresh_token: Some(refresh),
        pre_auth_token: None,
        requires_2fa: false,
        requires_2fa_setup: false,
        method: None,
        user: Some(UserOut {
            id: user.id,
            role: user.role,
        }),
    }))
}

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
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("jwt encode: {e}")))
}

fn decode_token(token: &str, expected_type: &str, secret: &str) -> Result<Claims> {
    let data = jsonwebtoken::decode::<Claims>(
        token,
        &jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        &jsonwebtoken::Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;

    if data.claims.token_type != expected_type {
        return Err(AppError::Unauthorized);
    }
    Ok(data.claims)
}
