//! Unified security facade — single entry for Desktop and WebUI clients.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::antitamper;
use crate::device::{self, DeviceKeypair};
use crate::manifest::SignedReleaseManifest;
use crate::signing;
use crate::token::{self, SessionToken};

#[derive(Debug, Clone)]
pub struct SecurityConfig {
    pub app_id: String,
    pub product_id: String,
    pub client_version: String,
    pub data_dir: PathBuf,
    pub api_base_url: String,
}

pub struct SecurityGate {
    config: SecurityConfig,
    device: Arc<DeviceKeypair>,
}

impl SecurityGate {
    /// Phase 5: initialize all security subsystems at startup.
    pub fn bootstrap(config: SecurityConfig, current_binary: Option<&Path>) -> Result<Self> {
        // Phase 7: anti-tamper
        antitamper::run_startup_checks(current_binary)?;
        if antitamper::injection_indicators_present() {
            #[cfg(feature = "strict-antitamper")]
            anyhow::bail!("injection indicators detected");
        }

        // Phase 3: device identity
        let device_id = stable_device_id(&config)?;
        let device = device::load_or_create_device_key(
            &config.data_dir.display().to_string(),
            &config.app_id,
            &device_id,
        )
        .context("load device key")?;

        Ok(Self {
            config,
            device: Arc::new(device),
        })
    }

    pub fn device(&self) -> &DeviceKeypair {
        &self.device
    }

    pub fn config(&self) -> &SecurityConfig {
        &self.config
    }

    /// Phase 2: verify release artifact before install.
    pub fn verify_update_artifact(&self, artifact: &str, sig: &str) -> Result<()> {
        signing::verify_release_artifact(artifact, sig)
    }

    /// Phase 2: parse manifest + anti-rollback.
    pub fn validate_update_manifest(
        &self,
        raw: &str,
        installed_version: &str,
    ) -> Result<SignedReleaseManifest> {
        let manifest = SignedReleaseManifest::from_json(raw)?;
        let latest = manifest
            .products
            .get(&self.config.product_id)
            .map(|p| p.latest_version.clone())
            .unwrap_or_else(|| installed_version.to_string());
        SignedReleaseManifest::assert_not_downgrade(installed_version, &latest)?;
        manifest.assert_min_supported(&self.config.product_id, installed_version)?;
        Ok(manifest)
    }

    /// Phase 4: build signed token request for server secondary auth.
    pub fn build_token_request(&self) -> token::TokenRequest {
        let device_id = self.device.identity.device_id.clone();
        let product_id = self.config.product_id.clone();
        let client_version = self.config.client_version.clone();
        let device = self.device.clone();
        token::build_token_request(&device_id, &product_id, &client_version, move |msg| {
            device.sign(msg)
        })
    }

    /// Phase 4: validate session token locally before use.
    pub fn validate_session_token(&self, token: &SessionToken) -> Result<()> {
        token.assert_valid()
    }

    /// Phase 3: register device key with server (once or on rotation).
    pub async fn register_device(&self, http: &reqwest::Client) -> Result<()> {
        let url = format!(
            "{}/api/v1/client/device-key",
            self.config.api_base_url.trim_end_matches('/')
        );
        let body = device::device_key_register_body(&self.device);
        http.post(&url)
            .json(&body)
            .send()
            .await
            .context("device-key register request")?
            .error_for_status()
            .context("device-key register HTTP error")?;
        Ok(())
    }

    /// Pinned minisign pubkey fingerprint for diagnostics.
    pub fn release_key_fingerprint(&self) -> String {
        self.device.fingerprint()
    }
}

fn stable_device_id(config: &SecurityConfig) -> Result<String> {
    let marker = config
        .data_dir
        .join(format!(".{}-device-id", config.app_id));
    if marker.exists() {
        let id = std::fs::read_to_string(&marker).context("read device id")?;
        if !id.trim().is_empty() {
            return Ok(id.trim().to_string());
        }
    }
    let id = format!("wptsall-{}", uuid::Uuid::new_v4());
    std::fs::create_dir_all(&config.data_dir).context("create data dir")?;
    std::fs::write(&marker, &id).context("write device id")?;
    Ok(id)
}
