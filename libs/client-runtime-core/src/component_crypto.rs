//! L3 component template encryption (AES-256-GCM + legacy SHA-256 or HKDF-SHA256 KDF).
//!
//! Shared with the official server `services/encryption.rs` derivation rules so
//! downloaded component templates decrypt identically on every client runtime.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const COMPONENT_CRYPTO_ALGO_AES: &str = "aes-256-gcm-sha256-v1";
pub const COMPONENT_CRYPTO_ALGO_XOR_LEGACY: &str = "xor-sha256-v1";
pub const COMPONENT_KDF_VERSION_HKDF: &str = "hkdf-sha256-v1";

const COMPONENT_NONCE_INFO: &[u8] = b"wptsall-nonce-v2";

/// Legacy SHA-256 KDF key (pre-HKDF components).
pub fn derive_component_crypto_key(
    session_token: &str,
    nonce: &str,
    component_id: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(session_token.as_bytes());
    hasher.update(b":");
    hasher.update(nonce.as_bytes());
    hasher.update(b":");
    hasher.update(component_id.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest[..32]);
    key
}

/// Legacy SHA-256 KDF GCM nonce (pre-HKDF components).
pub fn derive_component_crypto_nonce(nonce: &str, component_id: &str) -> [u8; 12] {
    let mut hasher = Sha256::new();
    hasher.update(b"nonce:");
    hasher.update(nonce.as_bytes());
    hasher.update(b":");
    hasher.update(component_id.as_bytes());
    let digest = hasher.finalize();
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&digest[..12]);
    nonce_bytes
}

/// HKDF-SHA256 component encryption key (current server default).
pub fn derive_component_crypto_key_hkdf(
    session_token: &str,
    nonce: &str,
    component_id: &str,
) -> [u8; 32] {
    derive_component_crypto_key_hkdf_with_owner(session_token, nonce, component_id, None)
}

/// HKDF key with optional `owner_type` (`official` uses a distinct info string).
pub fn derive_component_crypto_key_hkdf_with_owner(
    session_token: &str,
    nonce: &str,
    component_id: &str,
    owner_type: Option<&str>,
) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(nonce.as_bytes()), session_token.as_bytes());
    let info = match owner_type {
        Some("official") => format!("wptsall-official-v2:{component_id}"),
        _ => format!("wptsall-component-v2:{component_id}"),
    };
    let mut key = [0u8; 32];
    hk.expand(info.as_bytes(), &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// HKDF-SHA256 GCM nonce for component templates.
pub fn derive_component_crypto_nonce_hkdf(nonce: &str, component_id: &str) -> [u8; 12] {
    let hk = Hkdf::<Sha256>::new(Some(component_id.as_bytes()), nonce.as_bytes());
    let mut nonce_bytes = [0u8; 12];
    hk.expand(COMPONENT_NONCE_INFO, &mut nonce_bytes)
        .expect("12 bytes is a valid HKDF-SHA256 output length");
    nonce_bytes
}

/// Decrypt a component template ciphertext to JSON.
pub fn decrypt_component_template_aes(
    encrypted_payload: &str,
    session_token: &str,
    nonce: &str,
    component_id: &str,
    use_hkdf: bool,
    owner_type: Option<&str>,
) -> anyhow::Result<Value> {
    let plain_bytes = decrypt_component_template_aes_bytes(
        encrypted_payload,
        session_token,
        nonce,
        component_id,
        use_hkdf,
        owner_type,
    )?;
    serde_json::from_slice(&plain_bytes).context("decode decrypted component template json failed")
}

/// Decrypt a component template ciphertext to raw bytes.
pub fn decrypt_component_template_aes_bytes(
    encrypted_payload: &str,
    session_token: &str,
    nonce: &str,
    component_id: &str,
    use_hkdf: bool,
    owner_type: Option<&str>,
) -> anyhow::Result<Vec<u8>> {
    let cipher_bytes = BASE64_STANDARD
        .decode(encrypted_payload)
        .context("decode component encrypted_payload (base64) failed")?;
    let (key, nonce_bytes) = if use_hkdf {
        (
            derive_component_crypto_key_hkdf_with_owner(
                session_token,
                nonce,
                component_id,
                owner_type,
            ),
            derive_component_crypto_nonce_hkdf(nonce, component_id),
        )
    } else {
        (
            derive_component_crypto_key(session_token, nonce, component_id),
            derive_component_crypto_nonce(nonce, component_id),
        )
    };
    let cipher = Aes256Gcm::new_from_slice(&key).context("init component cipher failed")?;
    cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), cipher_bytes.as_ref())
        .map_err(|_| anyhow!("decrypt component template failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::Aead;
    use serde_json::json;

    #[test]
    fn legacy_kdf_is_deterministic() {
        let key1 = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-001");
        let key2 = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-001");
        assert_eq!(key1, key2);
    }

    #[test]
    fn hkdf_key_differs_from_legacy() {
        let legacy = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-001");
        let hkdf = derive_component_crypto_key_hkdf("session-abc", "nonce-xyz", "comp-001");
        assert_ne!(legacy, hkdf);
    }

    #[test]
    fn official_owner_type_uses_distinct_hkdf_info() {
        let user = derive_component_crypto_key_hkdf_with_owner(
            "session-abc",
            "nonce-xyz",
            "comp-001",
            Some("user"),
        );
        let official = derive_component_crypto_key_hkdf_with_owner(
            "session-abc",
            "nonce-xyz",
            "comp-001",
            Some("official"),
        );
        assert_ne!(user, official);
    }

    #[test]
    fn hkdf_roundtrip_decrypts_template_json() {
        let session = "test-session-hkdf-token";
        let nonce_str = "hkdf-nonce-value";
        let component_id = "component-hkdf-roundtrip";
        let original = json!({"message": "hkdf hello", "count": 99});

        let key = derive_component_crypto_key_hkdf(session, nonce_str, component_id);
        let nonce_bytes = derive_component_crypto_nonce_hkdf(nonce_str, component_id);
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let plain_bytes = serde_json::to_vec(&original).unwrap();
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plain_bytes.as_ref())
            .unwrap();
        let encrypted_b64 = BASE64_STANDARD.encode(encrypted);

        let decrypted = decrypt_component_template_aes(
            &encrypted_b64,
            session,
            nonce_str,
            component_id,
            true,
            None,
        )
        .unwrap();
        assert_eq!(decrypted, original);
    }
}
