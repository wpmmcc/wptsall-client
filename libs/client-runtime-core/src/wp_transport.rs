//! WP Plugin ↔ Client transport encryption (Protocol body crypto).
//!
//! Must stay aligned with PHP `WPTSALL\Core\Transport_Crypto`:
//! - HKDF-SHA256(ikm=client_api_token, salt=nonce_bytes, info=`wptsall-transport-v1`)
//! - AES-256-GCM packed payload: `nonce(12) || ciphertext || tag`
//!
//! Policy (when to encrypt) stays in the client (`https_optional`); this module
//! only implements the wire crypto primitives so WP PHP and future Rust clients
//! share one Rust implementation for the encrypt/decrypt path.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;

/// HKDF info string — must match PHP `Transport_Crypto::HKDF_INFO`.
pub const TRANSPORT_HKDF_INFO: &str = "wptsall-transport-v1";

/// Algorithm id used in transport headers / docs.
pub const TRANSPORT_ALGORITHM_ID: &str = "aes-256-gcm-v1";

/// Derive the 32-byte AES key for a request/response nonce.
pub fn derive_transport_key(token: &str, nonce: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(nonce), token.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(TRANSPORT_HKDF_INFO.as_bytes(), &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// Encrypt plaintext for WP transport.
///
/// Returns `(encrypted_payload_b64, nonce_b64url)` where payload packing is
/// `nonce(12) || ciphertext+tag` (AES-GCM appends the tag).
pub fn transport_encrypt(plaintext: &[u8], token: &str) -> anyhow::Result<(String, String)> {
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let key = derive_transport_key(token, &nonce_bytes);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("init transport cipher failed"))?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| anyhow!("transport encryption failed"))?;

    let mut packed = Vec::with_capacity(12 + encrypted.len());
    packed.extend_from_slice(&nonce_bytes);
    packed.extend_from_slice(&encrypted);

    let payload_b64 = BASE64_STANDARD.encode(&packed);
    let nonce_b64url = URL_SAFE_NO_PAD.encode(nonce_bytes);
    Ok((payload_b64, nonce_b64url))
}

/// Decrypt a WP transport packed payload.
pub fn transport_decrypt(
    encrypted_payload: &str,
    nonce_b64url: &str,
    token: &str,
) -> anyhow::Result<Vec<u8>> {
    let nonce_bytes = URL_SAFE_NO_PAD
        .decode(nonce_b64url)
        .context("decode transport nonce (base64url) failed")?;

    let key = derive_transport_key(token, &nonce_bytes);

    let raw = BASE64_STANDARD
        .decode(encrypted_payload)
        .context("decode transport payload (base64) failed")?;

    if raw.len() < 12 {
        return Err(anyhow!("transport encrypted payload too short"));
    }

    // Packed format: nonce (12) + ciphertext+tag
    let iv = &raw[..12];
    let ciphertext_and_tag = &raw[12..];

    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("init transport cipher failed"))?;
    cipher
        .decrypt(Nonce::from_slice(iv), ciphertext_and_tag)
        .map_err(|_| anyhow!("transport decryption failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_transport_key_deterministic() {
        let nonce = [1u8; 12];
        let k1 = derive_transport_key("token-a", &nonce);
        let k2 = derive_transport_key("token-a", &nonce);
        assert_eq!(k1, k2);
    }

    #[test]
    fn derive_transport_key_changes_with_token_or_nonce() {
        let nonce_a = [1u8; 12];
        let nonce_b = [2u8; 12];
        assert_ne!(
            derive_transport_key("token-a", &nonce_a),
            derive_transport_key("token-b", &nonce_a)
        );
        assert_ne!(
            derive_transport_key("token-a", &nonce_a),
            derive_transport_key("token-a", &nonce_b)
        );
    }

    #[test]
    fn transport_encrypt_decrypt_roundtrip() {
        let token = "wp-client-token-xyz";
        let plaintext = br#"{"success":true,"data":{"ok":1}}"#;
        let (payload, nonce) = transport_encrypt(plaintext, token).expect("encrypt");
        let decrypted = transport_decrypt(&payload, &nonce, token).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn transport_decrypt_wrong_token_fails() {
        let (payload, nonce) = transport_encrypt(b"secret", "correct-token").unwrap();
        let err = transport_decrypt(&payload, &nonce, "wrong-token");
        assert!(err.is_err());
    }

    #[test]
    fn transport_decrypt_rejects_short_payload() {
        let nonce = URL_SAFE_NO_PAD.encode([0u8; 12]);
        let short = BASE64_STANDARD.encode([0u8; 8]);
        let err = transport_decrypt(&short, &nonce, "token");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("too short"));
    }
}
