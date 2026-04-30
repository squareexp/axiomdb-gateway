use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, error, info};

/// Parsed result of a successful `square-dbctl provision` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionOutput {
    pub database: String,
    pub owner_role: String,
    pub runtime_role: String,
    pub readonly_role: String,
    pub runtime_key: String,
    pub direct_key: String,
    /// Raw stdout captured for the job log — masked before storage.
    pub raw_stdout: String,
}

/// Low-level executor error.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("command not found or permission denied: {0}")]
    NotFound(String),
    #[error("command timed out after {0}s")]
    Timeout(u64),
    #[error("provision failed (exit {code}): {stderr}")]
    ProvisionFailed { code: i32, stderr: String },
    #[error("parse error: {0}")]
    Parse(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

const PROVISION_TIMEOUT_SECS: u64 = 120;

/// Execute `square-dbctl provision --app <app> --env <env> --json`.
///
/// Runs as the current OS process user (expected: `opsdc`).
/// Returns structured output parsed from JSON stdout.
pub async fn run_provision(
    dbctl_bin: &str,
    app: &str,
    env: &str,
) -> Result<ProvisionOutput, ExecError> {
    info!("executor: provision --app {app} --env {env}");

    let output = tokio::time::timeout(
        Duration::from_secs(PROVISION_TIMEOUT_SECS),
        Command::new(dbctl_bin)
            .args(["provision", "--app", app, "--env", env, "--json"])
            .output(),
    )
    .await
    .map_err(|_| ExecError::Timeout(PROVISION_TIMEOUT_SECS))?
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ExecError::NotFound(dbctl_bin.to_string())
        } else {
            ExecError::Io(e)
        }
    })?;

    // Backward compatibility: older square-dbctl versions don't support --json.
    let output = if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("unknown argument: --json")
    {
        info!("executor: dbctl has no --json support; retrying without --json");
        tokio::time::timeout(
            Duration::from_secs(PROVISION_TIMEOUT_SECS),
            Command::new(dbctl_bin)
                .args(["provision", "--app", app, "--env", env])
                .output(),
        )
        .await
        .map_err(|_| ExecError::Timeout(PROVISION_TIMEOUT_SECS))?
        .map_err(ExecError::Io)?
    } else {
        output
    };

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    debug!("executor stdout: {stdout}");
    if !stderr.is_empty() {
        debug!("executor stderr: {stderr}");
    }

    if !output.status.success() {
        error!("provision failed (exit {exit_code}): {stderr}");
        return Err(ExecError::ProvisionFailed {
            code: exit_code,
            stderr: redact(&stderr),
        });
    }

    parse_provision_output(&stdout, app, env)
}

/// Execute `square-dbctl smoke --app <app> --env <env>`.
pub async fn run_smoke(dbctl_bin: &str, app: &str, env: &str) -> Result<String, ExecError> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(dbctl_bin)
            .args(["smoke", "--app", app, "--env", env])
            .output(),
    )
    .await
    .map_err(|_| ExecError::Timeout(30))?
    .map_err(ExecError::Io)?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(ExecError::ProvisionFailed {
            code: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        })
    }
}

/// Execute `square-dbctl deprovision --app <app> --env <env> --yes`.
#[allow(dead_code)]
pub async fn run_deprovision(dbctl_bin: &str, app: &str, env: &str) -> Result<(), ExecError> {
    let output = tokio::time::timeout(
        Duration::from_secs(60),
        Command::new(dbctl_bin)
            .args(["deprovision", "--app", app, "--env", env, "--yes"])
            .output(),
    )
    .await
    .map_err(|_| ExecError::Timeout(60))?
    .map_err(ExecError::Io)?;

    if output.status.success() {
        Ok(())
    } else {
        Err(ExecError::ProvisionFailed {
            code: output.status.code().unwrap_or(-1),
            stderr: redact(&String::from_utf8_lossy(&output.stderr)),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse JSON output from square-dbctl provision.
///
/// Expected shape (square-dbctl --json mode):
/// ```json
/// {
///   "database": "sq_servers_prod",
///   "owner_role": "sq_servers_prod_owner",
///   "runtime_role": "sq_servers_prod_runtime",
///   "readonly_role": "sq_servers_prod_readonly",
///   "runtime_key": "DATABASE_URL_SERVERS_PROD",
///   "direct_key": "DIRECT_URL_SERVERS_PROD"
/// }
/// ```
///
/// Falls back to heuristic parsing if the binary doesn't yet support --json.
fn parse_provision_output(
    stdout: &str,
    app: &str,
    env: &str,
) -> Result<ProvisionOutput, ExecError> {
    // Try JSON first
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) {
        let get = |key: &str| {
            v.get(key)
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .ok_or_else(|| ExecError::Parse(format!("missing field: {key}")))
        };

        return Ok(ProvisionOutput {
            database: get("database")?,
            owner_role: get("owner_role")?,
            runtime_role: get("runtime_role")?,
            readonly_role: get("readonly_role")?,
            runtime_key: get("runtime_key")?,
            direct_key: get("direct_key")?,
            raw_stdout: redact(stdout),
        });
    }

    // Heuristic fallback — derive names from convention sq_<app>_<env>
    let env_upper = env.to_uppercase();
    let app_upper = app.to_uppercase();
    let db = format!("sq_{app}_{env}");
    Ok(ProvisionOutput {
        owner_role: format!("{db}_owner"),
        runtime_role: format!("{db}_runtime"),
        readonly_role: format!("{db}_readonly"),
        runtime_key: format!("DATABASE_URL_{app_upper}_{env_upper}"),
        direct_key: format!("DIRECT_URL_{app_upper}_{env_upper}"),
        raw_stdout: redact(stdout),
        database: db,
    })
}

/// Remove connection strings that might appear in raw output.
fn redact(s: &str) -> String {
    // Replace postgres://user:pass@host/db patterns
    let re = regex_lite::Regex::new(r#"postgres://[^\s'"]++"#)
        .unwrap_or_else(|_| regex_lite::Regex::new("postgres://[^[:space:]]+").unwrap());
    re.replace_all(s, "postgres://[REDACTED]").to_string()
}
