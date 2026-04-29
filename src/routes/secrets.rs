use axum::{routing::post, Json, Router};
use rand::{rngs::OsRng, RngCore};
use serde::Deserialize;
use serde_json::json;

use crate::{error::Result, middleware::AuthUser, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/secrets/generate", post(generate_secret))
}

#[derive(Deserialize)]
pub struct GenerateSecretRequest {
    pub label: String,
    pub format: Option<String>,
    pub bytes: Option<usize>,
}

async fn generate_secret(
    AuthUser(_claims): AuthUser,
    Json(body): Json<GenerateSecretRequest>,
) -> Result<Json<serde_json::Value>> {
    let byte_count = body.bytes.unwrap_or(32).min(128);
    let mut buf = vec![0u8; byte_count];
    OsRng.fill_bytes(&mut buf);

    let value = match body.format.as_deref().unwrap_or("base64url") {
        "hex" => hex::encode(&buf),
        _ => base64url_encode(&buf),
    };

    Ok(Json(json!({
        "label": body.label,
        "value": value,
        "bytes": byte_count,
    })))
}

fn base64url_encode(data: &[u8]) -> String {
    // Simple base64url without padding — uses alphabet A-Za-z0-9-_
    let b64 = base64_encode(data);
    b64.replace('+', "-").replace('/', "_").replace('=', "")
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let chunks = data.chunks(3);
    for chunk in chunks {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        out.push(TABLE[b0 >> 2] as char);
        out.push(TABLE[((b0 & 3) << 4) | (b1 >> 4)] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[b2 & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}
