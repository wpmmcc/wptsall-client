//! Desktop security bootstrap.

use anyhow::Result;
use std::path::PathBuf;
use wptsall_client_security::{SecurityConfig, SecurityGate};

static GATE: std::sync::OnceLock<SecurityGate> = std::sync::OnceLock::new();

pub fn init_desktop() -> Result<()> {
    if GATE.get().is_some() {
        return Ok(());
    }

    let data_dir = dirs_data("wptsall-desktop");
    // The website/control-plane is a legacy/test opt-in.  Keep this empty
    // unless the caller explicitly configures WPTSALL_SERVER_URL.
    let api_base = std::env::var("WPTSALL_SERVER_URL").unwrap_or_default();

    let config = SecurityConfig {
        app_id: "wptsall-desktop".to_string(),
        product_id: "client-desktop".to_string(),
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir,
        api_base_url: api_base,
    };

    let binary = std::env::current_exe().ok();
    let gate = SecurityGate::bootstrap(config, binary.as_deref())?;
    GATE.set(gate).map_err(|_| anyhow::anyhow!("gate init"))?;
    Ok(())
}

pub fn gate() -> Option<&'static SecurityGate> {
    GATE.get()
}

fn dirs_data(app: &str) -> PathBuf {
    if let Some(base) = directories::ProjectDirs::from("cc", "wpmm", app) {
        base.data_dir().to_path_buf()
    } else {
        PathBuf::from(format!("/tmp/{app}"))
    }
}
