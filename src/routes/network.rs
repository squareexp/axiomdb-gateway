use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::net::IpAddr;
use uuid::Uuid;

use crate::{
    error::{AppError, Result},
    middleware::AuthUser,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/network/current-ip", get(current_ip))
        .route("/network/my-ip", get(current_ip))
        .route("/network/policy", get(global_policy))
        .route("/network/rules", get(list_legacy_rules))
        .route(
            "/projects/:project_id/network/rules",
            get(list_project_rules).post(create_project_rule),
        )
        .route(
            "/projects/:project_id/network/public-mode",
            post(set_public_mode),
        )
}

async fn current_ip(
    AuthUser(_claims): AuthUser,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let ip = caller_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    let suggested_cidr = if ip.parse::<IpAddr>().is_ok() {
        format!("{ip}/32")
    } else {
        String::new()
    };

    Ok(Json(json!({
        "ip": ip,
        "suggested_cidr": suggested_cidr
    })))
}

async fn global_policy(AuthUser(_claims): AuthUser) -> Result<Json<serde_json::Value>> {
    Ok(Json(json!({
        "modes": ["restricted", "public_runtime", "public_all"],
        "default_mode": "restricted",
        "ports": {
            "runtime": 6432,
            "direct": 5432
        },
        "max_rule_ttl": "1y",
        "notes": [
            "CORS applies only to HTTP/Data API traffic.",
            "Native Postgres TCP access is controlled with network rules."
        ]
    })))
}

async fn list_legacy_rules(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
) -> Result<Json<serde_json::Value>> {
    let output = tokio::process::Command::new(&state.cfg.dbctl_bin)
        .arg("network-rules")
        .output()
        .await
        .map_err(|e| AppError::Executor(e.to_string()))?;

    if !output.status.success() {
        return Err(AppError::Executor(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(Json(
        serde_json::from_str::<serde_json::Value>(&stdout)
            .unwrap_or(json!({ "raw": stdout.trim() })),
    ))
}

async fn list_project_rules(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let policy = sqlx::query_as::<_, NetworkPolicy>(
        "SELECT project_id, mode, revision, last_applied_at, created_at, updated_at
         FROM network_policies WHERE project_id=$1",
    )
    .bind(project_id)
    .fetch_optional(&state.db)
    .await?;

    let rules = sqlx::query_as::<_, NetworkRule>(
        "SELECT id, project_id, branch_id, cidr, label, ports, scope, expires_at,
                created_by, source_ip, source_user_agent, deleted_at, created_at
         FROM network_rules
         WHERE project_id=$1
           AND deleted_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())
         ORDER BY created_at DESC",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({
        "project_id": project_id,
        "policy": policy.unwrap_or_else(|| NetworkPolicy::default_for(project_id)),
        "rules": rules
    })))
}

#[derive(Deserialize)]
struct CreateRuleRequest {
    cidr: String,
    label: Option<String>,
    ports: Option<String>,
    scope: Option<String>,
    branch_id: Option<Uuid>,
    expires_in: Option<String>,
}

async fn create_project_rule(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(project_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateRuleRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let cidr = normalize_cidr(&body.cidr)?;
    let ports = normalize_ports(body.ports.as_deref())?;
    let scope = if body.branch_id.is_some() {
        "branch"
    } else {
        body.scope.as_deref().unwrap_or("project")
    };
    if scope != "project" && scope != "branch" {
        return Err(AppError::Validation(
            "scope must be project or branch".into(),
        ));
    }
    if scope == "branch" && body.branch_id.is_none() {
        return Err(AppError::Validation(
            "branch_id is required when scope is branch".into(),
        ));
    }

    let expires_at = ttl_to_expiry(body.expires_in.as_deref())?;
    let source_ip = caller_ip(&headers);
    let user_agent = header_value(&headers, "user-agent");
    let label = body
        .label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("manual allowlist rule");

    let rule = sqlx::query_as::<_, NetworkRule>(
        "INSERT INTO network_rules
             (project_id, branch_id, cidr, label, ports, scope, expires_at,
              created_by, source_ip, source_user_agent)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING id, project_id, branch_id, cidr, label, ports, scope, expires_at,
                   created_by, source_ip, source_user_agent, deleted_at, created_at",
    )
    .bind(project_id)
    .bind(body.branch_id)
    .bind(&cidr)
    .bind(label)
    .bind(ports)
    .bind(scope)
    .bind(expires_at)
    .bind(claims.sub)
    .bind(source_ip)
    .bind(user_agent)
    .fetch_one(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'network.rule.created', 'network_rule', $2, $3)",
    )
    .bind(claims.sub)
    .bind(rule.id.to_string())
    .bind(json!({ "project_id": project_id, "cidr": rule.cidr, "ports": rule.ports }))
    .execute(&state.db)
    .await?;

    let applied = match reconcile_network_rule(&state, &rule.cidr, &rule.ports).await {
        Ok(applied) => applied,
        Err(error) => {
            tracing::error!(rule_id = %rule.id, cidr = %rule.cidr, ports = %rule.ports, error = %error, "network rule reconcile failed");
            sqlx::query("UPDATE network_rules SET deleted_at=now() WHERE id=$1")
                .bind(rule.id)
                .execute(&state.db)
                .await?;
            return Err(AppError::Executor(
                "network access update failed".to_string(),
            ));
        }
    };

    Ok((
        StatusCode::CREATED,
        Json(json!({ "rule": rule, "applied": applied })),
    ))
}

#[derive(Deserialize)]
struct PublicModeRequest {
    mode: String,
    confirm: Option<String>,
}

async fn set_public_mode(
    State(state): State<AppState>,
    AuthUser(claims): AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<PublicModeRequest>,
) -> Result<Json<serde_json::Value>> {
    crate::middleware::require_role(
        &claims.role,
        &[
            "owner",
            "operator",
            "admin",
            "super_admin",
            "SUPER_ADMIN",
            "ADMIN",
            "OPERATOR",
        ],
    )?;
    let mode = body.mode.trim();
    match mode {
        "restricted" => {}
        "public_runtime" if body.confirm.as_deref() == Some("make runtime public") => {}
        "public_all" if body.confirm.as_deref() == Some("make direct public") => {}
        "public_runtime" => {
            return Err(AppError::Validation(
                "confirm must be exactly: make runtime public".into(),
            ))
        }
        "public_all" => {
            return Err(AppError::Validation(
                "confirm must be exactly: make direct public".into(),
            ))
        }
        _ => {
            return Err(AppError::Validation(
                "mode must be restricted, public_runtime, or public_all".into(),
            ))
        }
    }

    let policy = sqlx::query_as::<_, NetworkPolicy>(
        "INSERT INTO network_policies (project_id, mode, revision)
         VALUES ($1, $2, 1)
         ON CONFLICT (project_id) DO UPDATE
           SET mode=EXCLUDED.mode,
               revision=network_policies.revision + 1,
               updated_at=now()
         RETURNING project_id, mode, revision, last_applied_at, created_at, updated_at",
    )
    .bind(project_id)
    .bind(mode)
    .fetch_one(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, metadata)
         VALUES ($1, 'network.public_mode.changed', 'project', $2, $3)",
    )
    .bind(claims.sub)
    .bind(project_id.to_string())
    .bind(json!({ "mode": mode, "revision": policy.revision }))
    .execute(&state.db)
    .await?;

    let applied = crate::executor::run_dbctl_json(
        &state.cfg.dbctl_bin,
        &["public-mode", "--mode", mode, "--yes"],
        60,
    )
    .await
    .map_err(|error| AppError::Executor(error.to_string()))?;

    Ok(Json(json!({ "policy": policy, "applied": applied })))
}

async fn reconcile_network_rule(
    state: &AppState,
    cidr: &str,
    ports: &str,
) -> Result<serde_json::Value> {
    let mut applied = Vec::new();

    if ports == "runtime" || ports == "both" {
        let output = crate::executor::run_dbctl(
            &state.cfg.dbctl_bin,
            &["allow-pgbouncer-cidr", "--cidr", cidr],
            60,
        )
        .await
        .map_err(|error| AppError::Executor(error.to_string()))?;
        applied.push(json!({ "target": "runtime", "port": 6432, "output": output }));
    }

    if ports == "direct" || ports == "both" {
        let output =
            crate::executor::run_dbctl(&state.cfg.dbctl_bin, &["allow-cidr", "--cidr", cidr], 60)
                .await
                .map_err(|error| AppError::Executor(error.to_string()))?;
        applied.push(json!({ "target": "direct", "port": 5432, "output": output }));
    }

    Ok(json!({ "cidr": cidr, "ports": ports, "targets": applied }))
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
struct NetworkPolicy {
    project_id: Uuid,
    mode: String,
    revision: i64,
    last_applied_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl NetworkPolicy {
    fn default_for(project_id: Uuid) -> Self {
        let now = chrono::Utc::now();
        Self {
            project_id,
            mode: "restricted".to_string(),
            revision: 0,
            last_applied_at: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
struct NetworkRule {
    id: Uuid,
    project_id: Uuid,
    branch_id: Option<Uuid>,
    cidr: String,
    label: String,
    ports: String,
    scope: String,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    created_by: Option<Uuid>,
    source_ip: Option<String>,
    source_user_agent: Option<String>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
}

fn caller_ip(headers: &HeaderMap) -> Option<String> {
    ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"]
        .iter()
        .find_map(|name| header_value(headers, name))
        .and_then(|value| value.split(',').next().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn normalize_ports(value: Option<&str>) -> Result<&'static str> {
    match value.unwrap_or("both").trim() {
        "runtime" => Ok("runtime"),
        "direct" => Ok("direct"),
        "both" => Ok("both"),
        _ => Err(AppError::Validation(
            "ports must be runtime, direct, or both".into(),
        )),
    }
}

fn normalize_cidr(value: &str) -> Result<String> {
    let value = value.trim();
    let (ip, prefix) = value.split_once('/').ok_or_else(|| {
        AppError::Validation("cidr must include a prefix, like 203.0.113.10/32".into())
    })?;
    let ip: IpAddr = ip
        .parse()
        .map_err(|_| AppError::Validation("cidr IP is invalid".into()))?;
    let prefix: u8 = prefix
        .parse()
        .map_err(|_| AppError::Validation("cidr prefix is invalid".into()))?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return Err(AppError::Validation(format!(
            "cidr prefix must be <= {max} for this address"
        )));
    }
    Ok(format!("{ip}/{prefix}"))
}

fn ttl_to_expiry(value: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let seconds = match value {
        "forever" | "permanent" => return Ok(None),
        "24h" => 24 * 60 * 60,
        "7d" => 7 * 24 * 60 * 60,
        "30d" | "1m" => 30 * 24 * 60 * 60,
        "1y" => 365 * 24 * 60 * 60,
        _ => {
            return Err(AppError::Validation(
                "expires_in must be 24h, 7d, 30d, 1m, or 1y".into(),
            ))
        }
    };
    Ok(Some(
        chrono::Utc::now() + chrono::Duration::seconds(seconds),
    ))
}

#[cfg(test)]
mod tests {
    use super::{normalize_cidr, normalize_ports, ttl_to_expiry};

    #[test]
    fn validates_cidr_prefixes() {
        assert_eq!(
            normalize_cidr("203.0.113.10/32").unwrap(),
            "203.0.113.10/32"
        );
        assert!(normalize_cidr("203.0.113.10").is_err());
        assert!(normalize_cidr("203.0.113.10/33").is_err());
        assert!(normalize_cidr("2001:db8::1/129").is_err());
    }

    #[test]
    fn validates_ports() {
        assert_eq!(normalize_ports(None).unwrap(), "both");
        assert_eq!(normalize_ports(Some("runtime")).unwrap(), "runtime");
        assert!(normalize_ports(Some("admin")).is_err());
    }

    #[test]
    fn parses_network_ttl() {
        assert!(ttl_to_expiry(None).unwrap().is_none());
        assert!(ttl_to_expiry(Some("forever")).unwrap().is_none());
        assert!(ttl_to_expiry(Some("24h")).unwrap().is_some());
        assert!(ttl_to_expiry(Some("3h")).is_err());
    }
}
