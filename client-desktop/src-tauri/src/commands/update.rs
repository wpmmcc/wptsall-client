//! Desktop self-update (binary kit + UI-only bundle).
//!
//! Product axis: `client-desktop` / `client-desktop-webui` (see wptsall_client::updater).
//! Production OTA requires SecurityGate + minisign (no checksum-only path).

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use wptsall_client::updater::{
    check_for_desktop_update, default_http_client, default_server_base, download_verify_apply_ui,
    should_apply_ui_only, UpdateCheckResult,
};

use crate::security;

static UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
pub struct PerformUpdateResult {
    pub message: String,
    pub update_kind: String,
    pub current_version: String,
    pub target_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_path: Option<String>,
}

fn server_base() -> String {
    default_server_base()
}

fn skip_security_allowed() -> bool {
    client_runtime_core::env_helpers::env_bool("WPTSALL_SKIP_SECURITY", false)
}

#[tauri::command]
pub async fn check_for_update() -> Result<UpdateCheckResult, String> {
    let http = default_http_client();
    check_for_desktop_update(&http, &server_base())
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn perform_update(app: tauri::AppHandle) -> Result<PerformUpdateResult, String> {
    if UPDATE_IN_PROGRESS.swap(true, Ordering::Relaxed) {
        return Err("an update is already in progress".into());
    }
    let _guard = UpdateGuard;

    let http = default_http_client();
    let check = check_for_desktop_update(&http, &server_base())
        .await
        .map_err(|e| format!("update check failed: {e:#}"))?;

    if !check.update_available {
        return Err("no update available".into());
    }

    if should_apply_ui_only(&check) {
        let root = download_verify_apply_ui(&http, &check, "desktop", security::gate())
            .await
            .map_err(|e| format!("UI update failed: {e:#}"))?;
        return Ok(PerformUpdateResult {
            message: "UI update applied (reload window to see changes)".into(),
            update_kind: "ui".into(),
            current_version: check.ui_current_version,
            target_version: check.ui_latest_version,
            ui_path: Some(root.join("ui/desktop").display().to_string()),
        });
    }

    let download_url = client_runtime_core::updater::resolve_url(
        &check.download_url_template,
        &check.latest_version,
    );
    let artifact_name = download_url
        .rsplit('/')
        .next()
        .unwrap_or("kit-desktop.tar.gz")
        .to_string();
    let sha256sums_url = {
        let template = check.signature_url_template.replace(".minisig", "");
        Some(client_runtime_core::updater::resolve_url(
            &template,
            &check.latest_version,
        ))
    };

    let new_bin = if let Some(gate) = security::gate() {
        let verified = wptsall_client_security::download_and_verify_update(
            gate,
            &http,
            &download_url,
            &check.latest_version,
            &check.current_version,
            &artifact_name,
            sha256sums_url.as_deref(),
        )
        .await
        .map_err(|e| format!("secure update verify: {e:#}"))?;
        verified.binary_path
    } else if skip_security_allowed() {
        // Dev-only checksum path
        let sums_url = sha256sums_url.clone().unwrap_or_default();
        let sha256_content = http
            .get(&sums_url)
            .send()
            .await
            .map_err(|e| format!("SHA256SUMS fetch: {e:#}"))?
            .error_for_status()
            .map_err(|e| format!("SHA256SUMS HTTP: {e:#}"))?
            .text()
            .await
            .unwrap_or_default();
        let expected =
            client_runtime_core::updater::parse_sha256sums(&sha256_content, &artifact_name)
                .ok_or_else(|| format!("no checksum for {artifact_name}"))?;
        let tmp = client_runtime_core::updater::download_update(&http, &download_url)
            .await
            .map_err(|e| format!("download failed: {e:#}"))?;
        if let Err(e) = client_runtime_core::updater::verify_checksum(&tmp, &expected).await {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("checksum: {e:#}"));
        }
        let new_bin = client_runtime_core::updater::extract_update_binary(&tmp)
            .map_err(|e| format!("kit extract: {e:#}"))?;
        if new_bin != tmp {
            let _ = std::fs::remove_file(&tmp);
        }
        new_bin
    } else {
        return Err(
            "security gate inactive — refusing binary update (set WPTSALL_SKIP_SECURITY=1 for local/dev only)"
                .into(),
        );
    };

    let current_exe = std::env::current_exe().map_err(|e| format!("current_exe: {e:#}"))?;
    // Desktop mode (is_desktop = true): no service manager exists, so the
    // helper relaunches the GUI visibly (no -WindowStyle Hidden) after the
    // swap — see runtime-core perform_self_replace (BUG-UPD-02/03).
    if let Err(e) = client_runtime_core::updater::perform_self_replace(
        "wptsall-desktop",
        &current_exe,
        &new_bin,
        true,
    ) {
        let _ = std::fs::remove_file(&new_bin);
        return Err(format!("self-replace: {e:#}"));
    }

    // The swap only completes once this process exits (on Windows the helper
    // renames the running exe aside; on Unix the old inode stays mapped and
    // the relaunch would race the still-running app). Exit shortly after the
    // response reaches the frontend; the helper restarts the new version.
    {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            app.exit(0);
        });
    }

    std::mem::forget(_guard);
    UPDATE_IN_PROGRESS.store(true, Ordering::Relaxed);

    Ok(PerformUpdateResult {
        message: "Binary update initiated; app will restart".into(),
        update_kind: "binary".into(),
        current_version: check.current_version,
        target_version: check.latest_version,
        ui_path: None,
    })
}

struct UpdateGuard;
impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_IN_PROGRESS.store(false, Ordering::Relaxed);
    }
}
