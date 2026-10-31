use serde::{Deserialize, Serialize};
use std::{path::Path, sync::OnceLock};

pub const READY_PATH: &str = "MPC_STARTUP_READY";
pub const READY_TOKEN: &str = "MPC_STARTUP_TOKEN";

#[derive(Serialize, Deserialize)]
pub struct Ready {
    pub token: String,
    pub version: String,
    pub pid: u32,
}

pub fn managed() -> bool {
    std::env::var_os(READY_PATH).is_some()
}

/// Called by the initialized window's message loop, after settings and controls load.
pub fn notify_ready() -> Result<(), String> {
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            let Some(path) = std::env::var_os(READY_PATH) else {
                return Ok(());
            };
            let token = std::env::var(READY_TOKEN).map_err(|e| e.to_string())?;
            let ready = Ready {
                token,
                version: env!("CARGO_PKG_VERSION").into(),
                pid: std::process::id(),
            };
            crate::updater::atomic_json(Path::new(&path), &ready).map_err(|e| e.to_string())
        })
        .clone()
}
