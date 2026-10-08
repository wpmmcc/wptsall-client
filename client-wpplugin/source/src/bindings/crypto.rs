use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::Path;

use crate::config::BINDINGS_CRYPTO_ALGO;
use crate::types::EncryptedBindingsDoc;

const WPTC_MAGIC: &[u8; 4] = b"WPTC";
const WPTC_VERSION: u8 = 0x01;
const WPTC_HEADER_LEN: usize = 4 + 1 + 12;

/// S3 / SEC-02 (07 audit, 12 批 A5): the DB-loaded device identity,
/// registered once at process boot (WebUI / worker entrypoints) so
/// `bindings_secret()` can derive a default encryption key without the
/// caller threading a DB connection into every file write. First
/// registration wins (idempotent for multi-entrypoint processes); explicit
/// env sources still take precedence. RwLock<Option<_>> (not OnceLock) so
/// the test seam can drop the registration again.
static DEFAULT_DEVICE_ID: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

/// Register the process-wide default device identity for key derivation
/// (S3/SEC-02). Called from boot paths after the device_id is resolved
/// from the DB (or an explicit override). No-op for empty values and after
/// a first registration.
pub(crate) fn set_default_device_id(device_id: &str) {
    let trimmed = device_id.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut guard = DEFAULT_DEVICE_ID
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.is_none() {
        *guard = Some(trimmed.to_string());
    }
}



pub(crate) fn bindings_secret() -> Option<String> {
    if let Ok(s) = env::var("WPTSALL_COMPONENT_BINDINGS_SECRET") {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }

    if let Ok(id) = env::var("WPTSALL_DEVICE_ID") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Some(derive_bindings_secret_from_device_id(&id));
        }
    }

    // S3/SEC-02: the boot-registered DB identity is the default source —
    // before this existed, WebUI/worker never wired the DB-loaded
    // device_id into the crypto layer and every at-rest credential stayed
    // in plain text (the SEC-02 known-red pin).
    if let Some(id) = DEFAULT_DEVICE_ID
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        return Some(derive_bindings_secret_from_device_id(&id));
    }

    None
}

fn derive_bindings_secret_from_device_id(device_id: &str) -> String {
    let ikm = device_id.as_bytes();
    let hk = Hkdf::<Sha256>::new(Some(b"wptsall-bindings-device"), ikm);
    let mut key = [0u8; 32];
    hk.expand(b"bindings-secret-from-device-id-v1", &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key.iter().map(|b| format!("{:02x}", b)).collect::<String>()
}

pub(crate) fn derive_retained_asset_key(id: &[u8; 16]) -> anyhow::Result<[u8; 32]> {
    let secret =
        bindings_secret().ok_or_else(|| anyhow!("retained assets require a bindings key"))?;
    let master = derive_bindings_crypto_key(&secret);
    let hkdf = Hkdf::<Sha256>::new(Some(id), &master);
    let mut key = [0; 32];
    hkdf.expand(b"wptsall-retained-assets-aes256gcm-v1", &mut key)
        .map_err(|_| anyhow!("derive retained asset key failed"))?;
    Ok(key)
}

pub(crate) fn load_encrypted_or_plain(file_path: &Path) -> anyhow::Result<String> {
    let raw_bytes = fs::read(file_path)
        .with_context(|| format!("read bindings file failed: {}", file_path.display()))?;

    if raw_bytes.is_empty() {
        return Ok(String::new());
    }

    if raw_bytes.len() >= WPTC_HEADER_LEN && &raw_bytes[..4] == WPTC_MAGIC {
        return decrypt_wptc_binary(&raw_bytes);
    }

    let raw_str = String::from_utf8(raw_bytes)
        .with_context(|| format!("bindings file is not valid UTF-8: {}", file_path.display()))?;

    if is_legacy_encrypted_json(&raw_str) {
        return decrypt_legacy_json_payload(&raw_str);
    }

    Ok(raw_str)
}

pub(crate) fn encrypt_for_save(plain_json: &str) -> anyhow::Result<Vec<u8>> {
    let secret = bindings_secret()
        .ok_or_else(|| anyhow!("cannot save encrypted configuration without a bindings key"))?;

    let key = derive_bindings_crypto_key(&secret);
    let mut nonce_bytes = [0u8; 12];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce_bytes);
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow!("init bindings cipher failed"))?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain_json.as_bytes())
        .map_err(|_| anyhow!("encrypt bindings payload failed"))?;

    let mut out = Vec::with_capacity(WPTC_HEADER_LEN + encrypted.len());
    out.extend_from_slice(WPTC_MAGIC);
    out.push(WPTC_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&encrypted);
    Ok(out)
}

fn decrypt_wptc_binary(data: &[u8]) -> anyhow::Result<String> {
    if data.len() < WPTC_HEADER_LEN + 16 {
        return Err(anyhow!("WPTC file too short"));
    }
    if data[4] != WPTC_VERSION {
        return Err(anyhow!("unsupported WPTC version: 0x{:02x}", data[4]));
    }
    let nonce_bytes = &data[5..17];
    let ciphertext = &data[17..];

    let secret = bindings_secret().ok_or_else(|| {
        anyhow!(
            "encrypted config file requires WPTSALL_COMPONENT_BINDINGS_SECRET environment variable"
        )
    })?;

    let key_hkdf = derive_bindings_crypto_key(&secret);
    let cipher_hkdf =
        Aes256Gcm::new_from_slice(&key_hkdf).map_err(|_| anyhow!("init cipher failed"))?;
    if let Ok(plain) = cipher_hkdf.decrypt(Nonce::from_slice(nonce_bytes), ciphertext) {
        return String::from_utf8(plain)
            .with_context(|| "decrypted WPTC content is not valid UTF-8");
    }

    let key_legacy = derive_bindings_crypto_key_legacy(&secret);
    let cipher_legacy =
        Aes256Gcm::new_from_slice(&key_legacy).map_err(|_| anyhow!("init cipher failed"))?;
    let plain = cipher_legacy
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| anyhow!("WPTC decrypt failed (tried HKDF and legacy KDF)"))?;
    String::from_utf8(plain).with_context(|| "decrypted WPTC content is not valid UTF-8")
}

pub(crate) fn decrypt_from_bytes(data: &[u8]) -> anyhow::Result<String> {
    if data.is_empty() {
        return Ok(String::new());
    }

    if data.len() >= WPTC_HEADER_LEN && &data[..4] == WPTC_MAGIC {
        return decrypt_wptc_binary(data);
    }

    let raw_str = String::from_utf8(data.to_vec())
        .with_context(|| "encrypted config value is not valid UTF-8")?;

    if is_legacy_encrypted_json(&raw_str) {
        return decrypt_legacy_json_payload(&raw_str);
    }

    Ok(raw_str)
}

fn is_legacy_encrypted_json(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let Value::Object(map) = value else {
        return false;
    };
    map.get("format").and_then(|v| v.as_str()) == Some("encrypted")
}

fn decrypt_legacy_json_payload(raw: &str) -> anyhow::Result<String> {
    let doc: EncryptedBindingsDoc =
        serde_json::from_str(raw).with_context(|| "parse legacy encrypted bindings json failed")?;
    if doc.format != "encrypted" || doc.algorithm != BINDINGS_CRYPTO_ALGO {
        return Err(anyhow!(
            "unsupported bindings encryption format/algorithm: {}/{}",
            doc.format,
            doc.algorithm
        ));
    }

    let secret = bindings_secret().ok_or_else(|| {
        anyhow!("encrypted bindings require WPTSALL_COMPONENT_BINDINGS_SECRET environment variable")
    })?;
    let nonce_bytes = BASE64_STANDARD
        .decode(doc.nonce.as_bytes())
        .with_context(|| "decode bindings nonce failed")?;
    if nonce_bytes.len() != 12 {
        return Err(anyhow!("invalid bindings nonce length"));
    }
    let payload_bytes = BASE64_STANDARD
        .decode(doc.payload.as_bytes())
        .with_context(|| "decode bindings payload failed")?;

    let key_hkdf = derive_bindings_crypto_key(&secret);
    let cipher_hkdf =
        Aes256Gcm::new_from_slice(&key_hkdf).map_err(|_| anyhow!("init bindings cipher failed"))?;
    if let Ok(plain) = cipher_hkdf.decrypt(Nonce::from_slice(&nonce_bytes), payload_bytes.as_ref())
    {
        return String::from_utf8(plain).with_context(|| "bindings plaintext is not valid utf-8");
    }

    let key_legacy = derive_bindings_crypto_key_legacy(&secret);
    let cipher_legacy = Aes256Gcm::new_from_slice(&key_legacy)
        .map_err(|_| anyhow!("init bindings cipher failed"))?;
    let plain = cipher_legacy
        .decrypt(Nonce::from_slice(&nonce_bytes), payload_bytes.as_ref())
        .map_err(|_| anyhow!("decrypt bindings payload failed (tried HKDF and legacy KDF)"))?;
    String::from_utf8(plain).with_context(|| "bindings plaintext is not valid utf-8")
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

pub(crate) fn derive_bindings_crypto_key(secret: &str) -> [u8; 32] {
    let ikm = decode_hex(secret).unwrap_or_else(|| secret.as_bytes().to_vec());
    let hk = Hkdf::<Sha256>::new(Some(b"wptsall-bindings"), &ikm);
    let mut key = [0u8; 32];
    hk.expand(b"bindings-encryption-v1", &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

fn derive_bindings_crypto_key_legacy(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"wptsall-bindings:");
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest[..32]);
    key
}
