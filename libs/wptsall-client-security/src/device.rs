//! Device identity: Ed25519 keypair aligned with `POST /api/v1/client/device-key`.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use client_runtime_core::secure_store;
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

pub const DEVICE_KEY_PURPOSE: &str = "device-signing-key-v1";
pub const ED25519_ALGO: &str = "Ed25519";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub public_key_b64: String,
    pub algorithm: String,
}

#[derive(Debug, Clone)]
pub struct DeviceKeypair {
    pub identity: DeviceIdentity,
    signing_key: SigningKey,
}

impl DeviceKeypair {
    pub fn generate(device_id: impl Into<String>) -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        let public_key_b64 = B64.encode(verifying_key.to_bytes());
        Self {
            identity: DeviceIdentity {
                device_id: device_id.into(),
                public_key_b64,
                algorithm: ED25519_ALGO.to_string(),
            },
            signing_key,
        }
    }

    pub fn from_seed(device_id: impl Into<String>, seed: [u8; 32]) -> Result<Self> {
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        Ok(Self {
            identity: DeviceIdentity {
                device_id: device_id.into(),
                public_key_b64: B64.encode(verifying_key.to_bytes()),
                algorithm: ED25519_ALGO.to_string(),
            },
            signing_key,
        })
    }

    pub fn sign(&self, message: &[u8]) -> String {
        B64.encode(self.signing_key.sign(message).to_bytes())
    }

    pub fn fingerprint(&self) -> String {
        let hash = Sha256::digest(self.identity.public_key_b64.as_bytes());
        hex::encode(&hash[..8])
    }
}

/// Persist device signing seed in encrypted secure store.
pub fn save_device_key(data_dir: &str, app_id: &str, kp: &DeviceKeypair) -> Result<()> {
    let seed = kp.signing_key.to_bytes();
    let seed_b64 = B64.encode(seed);
    secure_store::encrypt_secret(data_dir, app_id, DEVICE_KEY_PURPOSE, &seed_b64)
        .context("encrypt device signing key")?;
    Ok(())
}

/// Load or create device keypair. `device_id` is stable per installation.
pub fn load_or_create_device_key(
    data_dir: &str,
    app_id: &str,
    device_id: &str,
) -> Result<DeviceKeypair> {
    let key_path = Path::new(data_dir).join(format!(".{app_id}-device-key.enc"));
    std::fs::create_dir_all(data_dir).context("create data dir")?;

    if key_path.exists() {
        let raw = std::fs::read_to_string(&key_path).context("read device key file")?;
        if let Some(plain) =
            secure_store::decrypt_secret(data_dir, app_id, DEVICE_KEY_PURPOSE, &raw)?
        {
            let seed_bytes = B64.decode(plain.trim()).context("decode device key")?;
            if seed_bytes.len() != 32 {
                return Err(anyhow!("device key seed length {}", seed_bytes.len()));
            }
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&seed_bytes);
            return DeviceKeypair::from_seed(device_id, seed);
        }
    }

    let kp = DeviceKeypair::generate(device_id);
    let encrypted = secure_store::encrypt_secret(
        data_dir,
        app_id,
        DEVICE_KEY_PURPOSE,
        &B64.encode(kp.signing_key.to_bytes()),
    )?;
    std::fs::write(&key_path, encrypted).context("write encrypted device key")?;
    Ok(kp)
}

/// Build registration body for `POST /api/v1/client/device-key`.
pub fn device_key_register_body(kp: &DeviceKeypair) -> serde_json::Value {
    serde_json::json!({
        "device_id": kp.identity.device_id,
        "public_key": kp.identity.public_key_b64,
        "algorithm": kp.identity.algorithm,
    })
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_key_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_string_lossy().to_string();
        let kp1 = load_or_create_device_key(&data, "test-app", "dev-test-1").unwrap();
        let kp2 = load_or_create_device_key(&data, "test-app", "dev-test-1").unwrap();
        assert_eq!(kp1.identity.public_key_b64, kp2.identity.public_key_b64);
        let sig = kp1.sign(b"hello");
        assert!(!sig.is_empty());
    }
}
