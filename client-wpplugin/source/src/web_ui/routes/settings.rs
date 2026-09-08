use anyhow::Context;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::logging::{get_log_enabled, get_log_min_level_str, set_log_enabled, set_log_min_level};
use crate::types::{WebUiLogSettingsRequest, WebUiState};
use crate::web_ui::AccessControl;

use super::errors::write_error_response;
use super::http::write_http_response;

pub(super) async fn handle_log_settings_get(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let payload = json!({
        "success": true,
        "data": {
            "enabled": get_log_enabled(),
            "level": get_log_min_level_str()
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_log_settings_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiLogSettingsRequest =
        serde_json::from_slice(body).with_context(|| "invalid /api/log-settings json payload")?;

    if let Some(enabled) = req.enabled {
        set_log_enabled(enabled);
    }
    if let Some(ref level) = req.level {
        let level = level.trim().to_lowercase();
        let valid = matches!(
            level.as_str(),
            "debug" | "info" | "warn" | "warning" | "error"
        );
        if !valid {
            return write_error_response(
                socket,
                "INVALID_LEVEL",
                "level must be debug/info/warn/error",
            )
            .await;
        }
        set_log_min_level(&level);
    }

    // Persist to DB
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::system::set_system_config(
            &db,
            "log_enabled",
            if get_log_enabled() { "true" } else { "false" },
        );
        let _ = crate::db::system::set_system_config(&db, "log_min_level", get_log_min_level_str());
    }

    // Update in-memory state
    {
        let mut g = state.lock().await;
        g.log_enabled = get_log_enabled();
        g.log_min_level = get_log_min_level_str().to_string();
    }

    let payload = json!({
        "success": true,
        "data": {
            "enabled": get_log_enabled(),
            "level": get_log_min_level_str()
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

// ---------------------------------------------------------------------------
// Access Control API
// ---------------------------------------------------------------------------

pub(super) async fn handle_access_control_get(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    access_control: &AccessControl,
) -> anyhow::Result<()> {
    let (external_access, allowed_ips) = access_control.get_settings().await;
    // Read the actual bind address from DB to show current state
    let current_bind = {
        let guard = state.lock().await;
        let conn = guard.db.lock().await;
        let ext = crate::db::system::get_system_config(&conn, "web_ui_external_access")
            .map(|v| v == "true")
            .unwrap_or(false);
        if ext { "0.0.0.0" } else { "127.0.0.1" }.to_string()
    };
    let payload = json!({
        "success": true,
        "data": {
            "external_access": external_access,
            "allowed_ips": allowed_ips,
            "current_bind": current_bind
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_access_control_update(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    access_control: &AccessControl,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: serde_json::Value =
        serde_json::from_slice(body).with_context(|| "invalid /api/access-control json payload")?;

    let new_external = req.get("external_access").and_then(|v| v.as_bool());
    let new_ips_raw = req.get("allowed_ips").and_then(|v| v.as_array());

    // Parse and validate IP addresses
    let mut parsed_ips: Vec<std::net::IpAddr> = Vec::new();
    if let Some(ip_arr) = new_ips_raw {
        for ip_val in ip_arr {
            let ip_str = ip_val.as_str().unwrap_or("").trim();
            if ip_str.is_empty() {
                continue;
            }
            match ip_str.parse::<std::net::IpAddr>() {
                Ok(ip) => parsed_ips.push(ip),
                Err(_) => {
                    return write_error_response(
                        socket,
                        "INVALID_IP",
                        &format!("invalid IP address: {}", ip_str),
                    )
                    .await;
                }
            }
        }
    }

    // If enabling external access, require at least one non-loopback IP
    let (current_external, _) = access_control.get_settings().await;
    let external_access = new_external.unwrap_or(current_external);

    // Persist to DB
    let db_arc = {
        let g = state.lock().await;
        std::sync::Arc::clone(&g.db)
    };
    {
        let db = db_arc.lock().await;
        let _ = crate::db::system::set_system_config(
            &db,
            "web_ui_external_access",
            if external_access { "true" } else { "false" },
        );
        let ips_str = parsed_ips
            .iter()
            .map(|ip| ip.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = crate::db::system::set_system_config(&db, "web_ui_allowed_ips", &ips_str);
    }

    // Update in-memory whitelist (takes effect immediately for new connections)
    access_control.update(external_access, &parsed_ips).await;

    // Check if bind address change requires restart
    let bind_changed = external_access != current_external;

    let (_, updated_ips) = access_control.get_settings().await;
    let payload = json!({
        "success": true,
        "data": {
            "external_access": external_access,
            "allowed_ips": updated_ips,
            "restart_required": bind_changed
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
