//! Secure update download + verification pipeline.

use anyhow::{Context, Result};
use client_runtime_core::updater;
use std::path::{Path, PathBuf};

use crate::gate::SecurityGate;
use crate::manifest::SignedReleaseManifest;
use crate::signing;

/// Result of a verified update download, ready for self-replace.
pub struct VerifiedUpdate {
    pub binary_path: PathBuf,
    pub target_version: String,
    pub sha256_hex: Option<String>,
}

/// Verified artifact still on disk (kit or UI bundle), before extract/apply.
pub struct VerifiedArtifact {
    pub path: PathBuf,
    pub target_version: String,
    pub sha256_hex: Option<String>,
}

fn security_skip_allowed() -> bool {
    client_runtime_core::env_helpers::env_bool("WPTSALL_SKIP_SECURITY", false)
}

/// Download + minisign (+ optional signed SHA256SUMS) without extracting.
pub async fn download_and_verify_signed_artifact(
    gate: &SecurityGate,
    http: &reqwest::Client,
    download_url: &str,
    target_version: &str,
    installed_version: &str,
    artifact_name: &str,
    sha256sums_url: Option<&str>,
) -> Result<VerifiedArtifact> {
    SignedReleaseManifest::assert_not_downgrade(installed_version, target_version)?;

    let tmp_artifact = updater::download_update(http, download_url)
        .await
        .context("download update artifact")?;

    let sig_url = format!("{download_url}.minisig");
    let tmp_sig = updater::download_update(http, &sig_url).await;
    match tmp_sig {
        Ok(sig_path) => {
            gate.verify_update_artifact(
                &tmp_artifact.display().to_string(),
                &sig_path.display().to_string(),
            )?;
            let _ = std::fs::remove_file(&sig_path);
        }
        Err(e) => {
            if let Some(sums_url) = sha256sums_url {
                verify_via_sha256sums(http, gate, &tmp_artifact, sums_url, artifact_name).await?;
            } else {
                return Err(e).context(
                    "no .minisig for artifact and no SHA256SUMS URL — refusing unsigned update",
                );
            }
        }
    }

    let sha256_hex = if let Some(sums_url) = sha256sums_url {
        Some(fetch_and_match_sha256(http, &tmp_artifact, sums_url, artifact_name).await?)
    } else {
        None
    };

    Ok(VerifiedArtifact {
        path: tmp_artifact,
        target_version: target_version.to_string(),
        sha256_hex,
    })
}

/// Full secure update: anti-rollback → download → minisign → sha256 → extract binary.
pub async fn download_and_verify_update(
    gate: &SecurityGate,
    http: &reqwest::Client,
    download_url: &str,
    target_version: &str,
    installed_version: &str,
    artifact_name: &str,
    sha256sums_url: Option<&str>,
) -> Result<VerifiedUpdate> {
    let verified = download_and_verify_signed_artifact(
        gate,
        http,
        download_url,
        target_version,
        installed_version,
        artifact_name,
        sha256sums_url,
    )
    .await?;

    let binary_path = updater::extract_update_binary(&verified.path)
        .context("extract wptsall-client from verified kit")?;
    if binary_path != verified.path {
        let _ = std::fs::remove_file(&verified.path);
    }

    Ok(VerifiedUpdate {
        binary_path,
        target_version: verified.target_version,
        sha256_hex: verified.sha256_hex,
    })
}

/// Secure UI-only OTA: verify signed UI bundle, then replace `ui/{subdir}/`.
pub async fn download_and_verify_apply_ui(
    gate: &SecurityGate,
    http: &reqwest::Client,
    download_url: &str,
    target_version: &str,
    installed_version: &str,
    artifact_name: &str,
    sha256sums_url: Option<&str>,
    install_root: &Path,
    ui_subdir: &str,
) -> Result<PathBuf> {
    let verified = download_and_verify_signed_artifact(
        gate,
        http,
        download_url,
        target_version,
        installed_version,
        artifact_name,
        sha256sums_url,
    )
    .await?;

    if let Err(e) = updater::apply_ui_bundle(&verified.path, install_root, ui_subdir) {
        let _ = std::fs::remove_file(&verified.path);
        return Err(e);
    }
    let _ = std::fs::remove_file(&verified.path);
    Ok(install_root.to_path_buf())
}

/// Fetch releases JSON and optionally verify detached minisign over the `data` object.
///
/// Production (no `WPTSALL_SKIP_SECURITY`): requires `{releases_url}.minisig` unless
/// `WPTSALL_ALLOW_UNSIGNED_MANIFEST=1` (migration escape hatch).
pub async fn fetch_verified_releases_data(
    http: &reqwest::Client,
    releases_url: &str,
) -> Result<serde_json::Value> {
    let resp = http
        .get(releases_url)
        .send()
        .await?
        .error_for_status()
        .context("releases HTTP")?;
    let body_text = resp.text().await.context("releases body")?;
    let body: serde_json::Value =
        serde_json::from_str(&body_text).context("parse releases JSON")?;
    let data = body
        .get("data")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing data field in releases response"))?;

    // Production (no SKIP_SECURITY): require signed manifest unless ALLOW_UNSIGNED_MANIFEST=1.
    let allow_unsigned =
        client_runtime_core::env_helpers::env_bool("WPTSALL_ALLOW_UNSIGNED_MANIFEST", false);
    let require_sig = !security_skip_allowed() && !allow_unsigned;

    let sig_url = format!("{releases_url}.minisig");
    match updater::download_update(http, &sig_url).await {
        Ok(sig_path) => {
            let tmp_json = std::env::temp_dir().join(format!(
                "wptsall-releases-{}.json",
                uuid::Uuid::new_v4()
            ));
            let canonical = crate::manifest::canonical_json_bytes(&data)
                .context("canonicalize manifest data")?;
            std::fs::write(&tmp_json, &canonical)?;
            signing::verify_release_artifact(
                &tmp_json.display().to_string(),
                &sig_path.display().to_string(),
            )
            .context("releases manifest minisign verify")?;
            let _ = std::fs::remove_file(&tmp_json);
            let _ = std::fs::remove_file(&sig_path);
            let _ = SignedReleaseManifest::from_json(&serde_json::to_string(&data)?);
            Ok(data)
        }
        Err(e) if require_sig => Err(e).context(
            "releases manifest .minisig required (set WPTSALL_ALLOW_UNSIGNED_MANIFEST=1 only for migration)",
        ),
        Err(_) => {
            if !security_skip_allowed() {
                eprintln!(
                    "[security] WARNING: unsigned releases manifest accepted via WPTSALL_ALLOW_UNSIGNED_MANIFEST"
                );
            }
            Ok(data)
        }
    }
}

async fn verify_via_sha256sums(
    http: &reqwest::Client,
    gate: &SecurityGate,
    binary: &Path,
    sums_url: &str,
    artifact_name: &str,
) -> Result<()> {
    let sums_content = http
        .get(sums_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let sig_url = format!("{sums_url}.minisig");
    let tmp_sums = std::env::temp_dir().join(format!("wptsall-sums-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp_sums, &sums_content)?;

    let tmp_sig = updater::download_update(http, &sig_url).await?;
    gate.verify_update_artifact(
        &tmp_sums.display().to_string(),
        &tmp_sig.display().to_string(),
    )?;

    let expected = updater::parse_sha256sums(&sums_content, artifact_name)
        .ok_or_else(|| anyhow::anyhow!("checksum entry not found for {artifact_name}"))?;
    updater::verify_checksum(binary, &expected).await?;

    let _ = std::fs::remove_file(&tmp_sums);
    let _ = std::fs::remove_file(&tmp_sig);
    Ok(())
}

async fn fetch_and_match_sha256(
    http: &reqwest::Client,
    binary: &Path,
    sums_url: &str,
    artifact_name: &str,
) -> Result<String> {
    let sums_content = http
        .get(sums_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let expected = updater::parse_sha256sums(&sums_content, artifact_name)
        .ok_or_else(|| anyhow::anyhow!("checksum entry not found for {artifact_name}"))?;
    updater::verify_checksum(binary, &expected).await?;
    Ok(expected)
}
