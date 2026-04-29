use std::env;

/// Runtime configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    /// PostgreSQL connection URL for the astradb control plane DB.
    pub database_url: String,
    /// Address to bind the HTTP server on, e.g. 127.0.0.1:4060
    pub bind_addr: String,
    /// Secret key used to sign/verify JWTs.
    pub jwt_secret: String,
    /// JWT access token TTL in seconds (default 900 = 15 min).
    pub jwt_access_ttl_secs: u64,
    /// JWT refresh token TTL in seconds (default 604800 = 7 days).
    pub jwt_refresh_ttl_secs: u64,
    /// Absolute path to the square-dbctl wrapper binary on the VPS.
    pub dbctl_bin: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: required("DATABASE_URL")?,
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:4060".into()),
            jwt_secret: required("JWT_SECRET")?,
            jwt_access_ttl_secs: env::var("JWT_ACCESS_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900),
            jwt_refresh_ttl_secs: env::var("JWT_REFRESH_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(604_800),
            dbctl_bin: env::var("DBCTL_BIN")
                .unwrap_or_else(|_| "/usr/local/bin/square-dbctl".into()),
        })
    }
}

fn required(key: &str) -> anyhow::Result<String> {
    env::var(key).map_err(|_| anyhow::anyhow!("Missing required env var: {}", key))
}
