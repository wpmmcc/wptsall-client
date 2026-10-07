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

/// 12号批 H 小项① (2026-09-23): audit detail for
/// settings.access_control_updated. The count alone cannot answer the audit
/// question "WHICH IPs were granted access?"; the allowlist itself is the
/// audit fact (operator-entered addresses, no credential material). Kept as
/// a pure function so the event shape is unit-pinnable — log_event_global
/// is a no-op when the process-wide log path was never registered (unit
/// tests), so the handler cannot be asserted on directly.
pub(super) fn access_control_audit_detail(
    external_access: bool,
    ips: &[String],
) -> serde_json::Value {
    json!({
        "external_access": external_access,
        "allowed_ips_count": ips.len(),
        "allowed_ips": ips,
    })
}

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
    crate::logging::log_event_global(
        "info",
        "settings.log_settings_updated",
        json!({ "enabled": get_log_enabled(), "level": get_log_min_level_str() }),
    );

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

/// S2 (07 号 audit, 12 批 A2): get-or-create the WebUI access token — 32
/// hex chars of OS entropy (uuid v4) persisted in DB system config. Created
/// on first use; returned once by the access-control update that enables
/// external mode, and always readable via GET /api/access-control (local
/// mode, or loopback peers in external mode — the recovery lane).
pub(crate) async fn webui_access_token(state: &Arc<Mutex<WebUiState>>) -> anyhow::Result<String> {
    let guard = state.lock().await;
    let db = guard.db.lock().await;
    if let Some(existing) = crate::db::system::get_system_config(&db, "web_ui_access_token") {
        if !existing.trim().is_empty() {
            return Ok(existing);
        }
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    crate::db::system::set_system_config(&db, "web_ui_access_token", &token)?;
    Ok(token)
}

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
    // Token display: every caller of this endpoint is either local mode
    // (loopback-only bind), a loopback peer (recovery lane), or an
    // external peer already presenting the token — so echoing it here
    // leaks nothing the caller does not already hold or already trust.
    let access_token = webui_access_token(state).await.ok();
    let payload = json!({
        "success": true,
        "data": {
            "external_access": external_access,
            "allowed_ips": allowed_ips,
            "current_bind": current_bind,
            // Actual listen port (env override → bind-addr parse → default).
            // The UI renders this verbatim; before this field existed it
            // hard-coded `:8977` and misreported any custom-port deployment.
            "current_port": crate::web_ui::web_ui_effective_port(),
            "access_token": access_token
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

    // If enabling external access, require at least one non-loopback IP.
    // A6 / N-4 (08 号回归集, 12 批 A6): a comment claimed this before but
    // nothing enforced it — `external_access=true` with a loopback-only or
    // empty whitelist was persisted as-is, flipping the listener open with
    // no usable remote allowlist.
    let (current_external, _) = access_control.get_settings().await;
    let external_access = new_external.unwrap_or(current_external);
    let has_non_loopback = parsed_ips.iter().any(|ip| !ip.is_loopback());
    if external_access && !has_non_loopback {
        return write_error_response(
            socket,
            "EXTERNAL_REQUIRES_IP",
            "enabling external access requires at least one non-loopback allowed IP",
        )
        .await;
    }

    // Provision the access token BEFORE persisting external=true so
    // external mode can never activate without a token to enforce (S2).
    let access_token = if external_access {
        Some(webui_access_token(state).await?)
    } else {
        None
    };

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
    crate::logging::log_event_global(
        "warn",
        "settings.access_control_updated",
        access_control_audit_detail(external_access, &updated_ips),
    );
    let payload = json!({
        "success": true,
        "data": {
            "external_access": external_access,
            "allowed_ips": updated_ips,
            "restart_required": bind_changed,
            // One-time token display for the operator enabling external
            // mode (this request itself arrived before the gate armed).
            "access_token": access_token
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
