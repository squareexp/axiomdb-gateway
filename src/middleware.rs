use axum::{
    body::Body,
    extract::State,
    http::{header, Request},
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, DecodingKey, Validation};

use crate::{error::AppError, routes::auth::Claims, state::AppState};

/// Injects the authenticated Claims into request extensions.
pub async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> std::result::Result<Response, AppError> {
    let token = extract_bearer(req.headers()).ok_or(AppError::Unauthorized)?;

    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.cfg.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|_| AppError::Unauthorized)?;

    req.extensions_mut().insert(data.claims);
    Ok(next.run(req).await)
}

fn extract_bearer(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// Convenience extractor — pulls Claims from extensions.
pub struct AuthUser(pub Claims);

#[axum::async_trait]
impl<S> axum::extract::FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Claims>()
            .cloned()
            .map(AuthUser)
            .ok_or(AppError::Unauthorized)
    }
}

/// Role gate — returns 403 if user role is not in allowed list.
pub fn require_role(user_role: &str, allowed: &[&str]) -> crate::error::Result<()> {
    if allowed.contains(&user_role) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
