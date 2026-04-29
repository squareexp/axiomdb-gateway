use axum::{routing::get, Json, Router};
use serde_json::json;

use crate::{error::Result, middleware::AuthUser, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/network/rules", get(list_rules))
}

async fn list_rules(AuthUser(_claims): AuthUser) -> Result<Json<serde_json::Value>> {
    // Phase 1: return static rules. Phase 2: read from iptables/pg_hba.conf via dbctl.
    Ok(Json(json!({
        "rules": [
            { "cidr": "0.0.0.0/0", "action": "ALLOW", "note": "public internet (default)" }
        ],
        "note": "Dynamic rule management requires dbctl network sub-command (phase 2)"
    })))
}
