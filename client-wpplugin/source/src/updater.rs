//! Self-update manifest fetcher + SemVer comparison (binary + UI axes).
//!
//! Used by WebUI HTTP handlers and Desktop Tauri commands.
//! - binary: download kit → verify → self-replace → restart
//! - UI-only: download `*-ui-*.tar.gz` → verify → replace `ui/{webui|desktop}/`

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// WebUI binary product id in the releases manifest.
pub const PRODUCT_ID: &str = "client-wpplugin";
/// WebUI UI-only product id.
pub const PRODUCT_ID_UI: &str = "client-wpplugin-webui";
/// Desktop binary product id.
#[allow(dead_code)]
pub const PRODUCT_ID_DESKTOP: &str = "client-desktop";
/// Desktop UI-only product id.
#[allow(dead_code)]
pub const PRODUCT_ID_DESKTOP_UI: &str = "client-desktop-webui";

#[derive(Debug, Clone, Copy)]
pub struct ProductAxis {
    pub binary_id: &'static str,
    pub ui_id: &'static str,
    /// Kit UI directory name under `ui/` (`webui` or `desktop`).
    pub ui_subdir: &'static str,
}

pub const AXIS_WEBUI: ProductAxis = ProductAxis {
    binary_id: PRODUCT_ID,
    ui_id: PRODUCT_ID_UI,
    ui_subdir: "webui",
};

#[allow(dead_code)]
pub const AXIS_DESKTOP: ProductAxis = ProductAxis {
    binary_id: PRODUCT_ID_DESKTOP,
    ui_id: PRODUCT_ID_DESKTOP_UI,
    ui_subdir: "desktop",
};

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseProduct {
    pub latest_version: String,
    pub min_supported_version: String,
    pub release_notes_url: String,
    pub download_url_template: String,
    pub signature_url_template: String,
    #[serde(default)]
    pub mandatory: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ReleasesManifest {
    #[serde(rename = "schema_version")]
    pub schema_version: u32,
    pub products: std::collections::HashMap<String, ReleaseProduct>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheckResult {
    pub current_version: String,
    pub latest_version: String,
    pub min_supported_version: String,
    pub update_available: bool,
    pub mandatory: bool,
    /// "patch" | "minor" | "major" | "none"
    pub bump_kind: String,
    pub release_notes_url: String,
    pub download_url_template: String,
    pub signature_url_template: String,
    /// "none" | "binary" | "ui" | "both"
    pub update_kind: String,
    pub ui_current_version: String,
    pub ui_latest_version: String,
    pub ui_update_available: bool,
    pub ui_download_url_template: String,
    pub ui_signature_url_template: String,
    pub ui_release_notes_url: String,
    pub ui_subdir: String,
    pub product_id: String,
}

/// Minimal SemVer compare: `MAJOR.MINOR.PATCH` only.
pub fn semver_cmp(a: &str, b: &str) -> Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|p| p.split('-').next().unwrap_or("").parse::<u64>().ok())
            .collect()
    };
    let av = parse(a);
    let bv = parse(b);
    let n = av.len().max(bv.len()).max(3);
    for i in 0..n {
        let x = *av.get(i).unwrap_or(&0);
        let y = *bv.get(i).unwrap_or(&0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    Ordering::Equal
}

pub fn classify_bump(current: &str, latest: &str) -> &'static str {
    let c: Vec<u64> = current.split('.').filter_map(|p| p.parse().ok()).collect();
    let l: Vec<u64> = latest.split('.').filter_map(|p| p.parse().ok()).collect();
    if c.is_empty() || l.is_empty() {
        return "none";
    }
    if l[0] > c[0] {
        "major"
    } else if l.get(1).copied().unwrap_or(0) > c.get(1).copied().unwrap_or(0) {
        "minor"
    } else if l.get(2).copied().unwrap_or(0) > c.get(2).copied().unwrap_or(0) {
        "patch"
    } else {
        "none"
    }
}

/// Read on-disk UI version: `VERSION-WEBUI` under install root, else binary version.
pub fn current_ui_version() -> String {
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Ok(root) = std::env::var("WPTSALL_INSTALL_ROOT") {
            v.push(PathBuf::from(root).join("VERSION-WEBUI"));
        }
        if let Ok(exe) = std::env::current_exe() {
            let root = client_runtime_core::updater::install_root_from_exe(&exe);
            v.push(root.join("VERSION-WEBUI"));
        }
        v.push(PathBuf::from("VERSION-WEBUI"));
        v
    };
    for p in candidates {
        if let Ok(s) = std::fs::read_to_string(&p) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

fn empty_product() -> ReleaseProduct {
    ReleaseProduct {
        latest_version: String::new(),
        min_supported_version: String::new(),
        release_notes_url: String::new(),
        download_url_template: String::new(),
        signature_url_template: String::new(),
        mandatory: false,
    }
}

/// Explicit control-plane base for release checks.
///
/// Empty means that no release source was configured.  In particular, this
/// must not fall back to the official website: normal client startup is
/// local-first and release checks are an explicit opt-in operation.
#[allow(dead_code)]
pub fn default_server_base() -> String {
    for key in ["WPTSALL_SERVER_BASE", "WPTSALL_SERVER_URL"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    String::new()
}

#[allow(dead_code)]
pub fn default_http_client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Fetch releases manifest and decide binary + UI update availability for `axis`.
pub async fn check_for_update_axis(
    http: &reqwest::Client,
    server_base: &str,
    axis: ProductAxis,
) -> anyhow::Result<UpdateCheckResult> {
    let server_base = server_base.trim().trim_end_matches('/');
    if server_base.is_empty() {
        anyhow::bail!(
            "release update source is not configured; set WPTSALL_SERVER_BASE explicitly"
        );
    }
    let url = format!("{server_base}/api/v1/client/releases");
    // Prefer signed manifest path (minisign over `data`); see wptsall-client-security.
    let data = wptsall_client_security::fetch_verified_releases_data(http, &url).await?;
    let manifest: ReleasesManifest = serde_json::from_value(data)?;

    let product = manifest
        .products
        .get(axis.binary_id)
        .ok_or_else(|| anyhow::anyhow!("product {} not in releases manifest", axis.binary_id))?;

    let current = env!("CARGO_PKG_VERSION").to_string();
    let latest = product.latest_version.clone();
    let bump = classify_bump(&current, &latest);
    let binary_available = semver_cmp(&current, &latest) == Ordering::Less;

    let ui_product = manifest
        .products
        .get(axis.ui_id)
        .cloned()
        .unwrap_or_else(empty_product);
    let ui_current = current_ui_version();
    let ui_latest = if ui_product.latest_version.is_empty() {
        latest.clone()
    } else {
        ui_product.latest_version.clone()
    };
    let ui_available = !ui_product.download_url_template.is_empty()
        && semver_cmp(&ui_current, &ui_latest) == Ordering::Less;

    let update_kind = match (binary_available, ui_available) {
        (true, true) => "both",
        (true, false) => "binary",
        (false, true) => "ui",
        (false, false) => "none",
    };

    Ok(UpdateCheckResult {
        current_version: current,
        latest_version: latest,
        min_supported_version: product.min_supported_version.clone(),
        update_available: binary_available || ui_available,
        mandatory: product.mandatory || ui_product.mandatory,
        bump_kind: bump.to_string(),
        release_notes_url: product.release_notes_url.clone(),
        download_url_template: product.download_url_template.clone(),
        signature_url_template: product.signature_url_template.clone(),
        update_kind: update_kind.to_string(),
        ui_current_version: ui_current,
        ui_latest_version: ui_latest,
        ui_update_available: ui_available,
        ui_download_url_template: ui_product.download_url_template,
        ui_signature_url_template: ui_product.signature_url_template,
        ui_release_notes_url: ui_product.release_notes_url,
        ui_subdir: axis.ui_subdir.to_string(),
        product_id: axis.binary_id.to_string(),
    })
}

/// WebUI axis (default for HTTP shell).
pub async fn check_for_update(
    http: &reqwest::Client,
    server_base: &str,
) -> anyhow::Result<UpdateCheckResult> {
    check_for_update_axis(http, server_base, AXIS_WEBUI).await
}

/// Desktop axis.
#[allow(dead_code)]
pub async fn check_for_desktop_update(
    http: &reqwest::Client,
    server_base: &str,
) -> anyhow::Result<UpdateCheckResult> {
    check_for_update_axis(http, server_base, AXIS_DESKTOP).await
}

fn skip_security_allowed() -> bool {
    client_runtime_core::env_helpers::env_bool("WPTSALL_SKIP_SECURITY", false)
}

/// Download UI-only bundle, verify minisign (or signed SUMS), replace `ui/{subdir}/`.
/// Pass the process [`SecurityGate`] from WebUI or Desktop bootstrap.
pub async fn download_verify_apply_ui(
    http: &reqwest::Client,
    check: &UpdateCheckResult,
    ui_subdir: &str,
    gate: Option<&wptsall_client_security::SecurityGate>,
) -> anyhow::Result<PathBuf> {
    let target = check.ui_latest_version.clone();
    let installed = check.ui_current_version.clone();
    let download_url =
        client_runtime_core::updater::resolve_url(&check.ui_download_url_template, &target);
    let artifact_name = download_url
        .rsplit('/')
        .next()
        .unwrap_or("ui-bundle.tar.gz")
        .to_string();
    let sums_url = {
        let template = check.ui_signature_url_template.replace(".minisig", "");
        client_runtime_core::updater::resolve_url(&template, &target)
    };

    let current_exe = std::env::current_exe()?;
    let install_root = client_runtime_core::updater::install_root_from_exe(&current_exe);

    if let Some(gate) = gate {
        return wptsall_client_security::download_and_verify_apply_ui(
            gate,
            http,
            &download_url,
            &target,
            &installed,
            &artifact_name,
            Some(sums_url.as_str()),
            &install_root,
            ui_subdir,
        )
        .await;
    }

    if !skip_security_allowed() {
        anyhow::bail!(
            "security gate inactive — refusing UI update (set WPTSALL_SKIP_SECURITY=1 for local/dev only)"
        );
    }

    // Dev-only checksum path (explicit SKIP_SECURITY).
    let sha256_content = http
        .get(&sums_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
        .unwrap_or_default();

    let expected_hash =
        client_runtime_core::updater::parse_sha256sums(&sha256_content, &artifact_name)
            .or_else(|| {
                sha256_content.lines().find_map(|line| {
                    let mut parts = line.split_whitespace();
                    let hash = parts.next()?;
                    let name = parts.next().unwrap_or("");
                    if name.is_empty() || name == artifact_name || name.ends_with(&artifact_name) {
                        Some(hash.to_string())
                    } else {
                        None
                    }
                })
            })
            .ok_or_else(|| anyhow::anyhow!("no checksum entry for {artifact_name}"))?;

    let tmp = client_runtime_core::updater::download_update(http, &download_url).await?;
    if let Err(e) = client_runtime_core::updater::verify_checksum(&tmp, &expected_hash).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = client_runtime_core::updater::apply_ui_bundle(&tmp, &install_root, ui_subdir) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(install_root)
}

/// Prefer binary when newer; otherwise UI-only when available.
pub fn should_apply_ui_only(check: &UpdateCheckResult) -> bool {
    let binary_newer = semver_cmp(&check.current_version, &check.latest_version) == Ordering::Less;
    check.ui_update_available && !binary_newer
}

/// Build the dry-run upgrade plan for the running product.
#[allow(dead_code)]
pub fn build_upgrade_plan(
    product: &ReleaseProduct,
    current_version: &str,
    target_version: Option<&str>,
) -> serde_json::Value {
    let target = target_version.unwrap_or(&product.latest_version);
    serde_json::json!({
        "current_version": current_version,
        "target_version": target,
        "min_supported_version": product.min_supported_version,
        "bump_kind": classify_bump(current_version, target),
        "mandatory": product.mandatory,
        "download_url": client_runtime_core::updater::resolve_url(
            &product.download_url_template,
            target,
        ),
        "signature_url": client_runtime_core::updater::resolve_url(
            &product.signature_url_template,
            target,
        ),
        "release_notes_url": product.release_notes_url,
        "steps": [
            "1. fetch download_url with retry/backoff",
            "2. fetch SHA256SUMS and verify minisign signature",
            "3. binary: self-replace; OR ui: apply_ui_bundle under install root",
            "4. /api/health re-check (binary restart) or soft reload (UI)",
        ],
    })
}

/// Helper for callers that already have an install root override.
#[allow(dead_code)]
pub fn apply_ui_at(archive: &Path, install_root: &Path, ui_subdir: &str) -> anyhow::Result<()> {
    client_runtime_core::updater::apply_ui_bundle(archive, install_root, ui_subdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_cmp_basic() {
        assert_eq!(semver_cmp("0.1.0", "0.1.0"), Ordering::Equal);
        assert_eq!(semver_cmp("0.1.0", "0.2.0"), Ordering::Less);
        assert_eq!(semver_cmp("0.2.0", "0.1.0"), Ordering::Greater);
        assert_eq!(semver_cmp("2.1.0", "2.1.1"), Ordering::Less);
        assert_eq!(semver_cmp("3.0.0", "2.9.9"), Ordering::Greater);
    }

    #[test]
    fn classify_bump_matrix() {
        assert_eq!(classify_bump("0.1.0", "0.1.0"), "none");
        assert_eq!(classify_bump("0.1.0", "0.1.1"), "patch");
        assert_eq!(classify_bump("0.1.0", "0.2.0"), "minor");
        assert_eq!(classify_bump("0.1.0", "1.0.0"), "major");
        assert_eq!(classify_bump("2.1.0", "3.0.0"), "major");
    }

    #[test]
    fn should_apply_ui_only_when_binary_current() {
        let mut check = UpdateCheckResult {
            current_version: "2.1.0".into(),
            latest_version: "2.1.0".into(),
            min_supported_version: "2.0.0".into(),
            update_available: true,
            mandatory: false,
            bump_kind: "none".into(),
            release_notes_url: String::new(),
            download_url_template: String::new(),
            signature_url_template: String::new(),
            update_kind: "ui".into(),
            ui_current_version: "2.1.0".into(),
            ui_latest_version: "2.1.1".into(),
            ui_update_available: true,
            ui_download_url_template: "https://x/{version}".into(),
            ui_signature_url_template: String::new(),
            ui_release_notes_url: String::new(),
            ui_subdir: "desktop".into(),
            product_id: PRODUCT_ID_DESKTOP.into(),
        };
        assert!(should_apply_ui_only(&check));
        check.latest_version = "2.2.0".into();
        assert!(!should_apply_ui_only(&check));
    }

    #[test]
    fn build_plan_uses_template() {
        let p = ReleaseProduct {
            latest_version: "2.2.0".into(),
            min_supported_version: "2.0.0".into(),
            release_notes_url: "https://example.com/notes".into(),
            download_url_template: "https://x.example/v{version}-{target}.tar.zst".into(),
            signature_url_template: "https://x.example/v{version}/SHA256SUMS.minisig".into(),
            mandatory: false,
        };
        let plan = build_upgrade_plan(&p, "2.1.0", Some("2.2.0"));
        assert_eq!(plan["current_version"], "2.1.0");
        assert_eq!(plan["target_version"], "2.2.0");
        assert_eq!(plan["bump_kind"], "minor");
        assert!(plan["download_url"].as_str().unwrap().contains("v2.2.0"));
    }

    #[test]
    fn resolve_url_replaces_placeholders() {
        let template = "https://example.com/bin-{version}-{target}";
        let url = client_runtime_core::updater::resolve_url(template, "2.1.1");
        let triple = client_runtime_core::updater::current_target_triple();
        assert_eq!(url, format!("https://example.com/bin-2.1.1-{}", triple));
    }

    #[test]
    fn default_server_base_has_no_website_fallback() {
        let _base = crate::db::TestEnvVarGuard::set("WPTSALL_SERVER_BASE", "");
        let _url = crate::db::TestEnvVarGuard::set("WPTSALL_SERVER_URL", "");
        assert!(default_server_base().is_empty());
    }

    #[tokio::test]
    async fn missing_update_source_fails_before_http_request() {
        let _base = crate::db::TestEnvVarGuard::set("WPTSALL_SERVER_BASE", "");
        let _url = crate::db::TestEnvVarGuard::set("WPTSALL_SERVER_URL", "");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = check_for_update(&http, &default_server_base())
            .await
            .expect_err("an unset release source must fail closed");
        assert!(error
            .to_string()
            .contains("release update source is not configured"));
    }
}
