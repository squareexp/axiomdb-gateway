use crate::config::Config;
use sqlx::PgPool;
use std::sync::Arc;
use sysinfo::System;
use tokio::sync::Mutex;

/// Shared application state — cloned cheaply via Arc.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cfg: Config,
    /// sysinfo System — wrapped in Mutex because refresh() takes &mut self.
    pub sys: Arc<Mutex<System>>,
}

impl AppState {
    pub fn new(db: PgPool, cfg: Config) -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self {
            db,
            cfg,
            sys: Arc::new(Mutex::new(sys)),
        }
    }
}
