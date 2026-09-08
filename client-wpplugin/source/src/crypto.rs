use anyhow::{anyhow, Context};
use hkdf::Hkdf;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::{
    COMPONENT_CRYPTO_ALGO_AES, COMPONENT_CRYPTO_ALGO_XOR_LEGACY, COMPONENT_KDF_VERSION_HKDF,
};
use crate::types::ComponentDownloadData;

const SIGNED_REQUEST_HEADERS: &[&str] = &[
    "idempotency-key",
    "x-route-secret",
    "x-wptsall-task-id",
    "x-wptsall-relation-id",
    "x-wptsall-source-id",
    "x-wptsall-filename",
    "x-wptsall-upload-id",
    "x-wptsall-chunk-index",
];

pub(crate) fn resolve_download_template_json(
    download_data: &ComponentDownloadData,
    session_token: &str,
    trusted_public_key: Option<&str>,
) -> anyhow::Result<Value> {
    // -- Signature verification (before decryption, fail-fast) --
    let skip_sig_check = std::env::var("WPTSALL_SKIP_SIGNATURE_CHECK")
        .ok()
        .as_deref()
        == Some("true");

    match (download_data.signature.as_deref(), trusted_public_key) {
        (Some(sig_b64), Some(pub_pem)) => {
            // Both signature and trusted key present — verify
            let signed_data: Vec<u8> = if let Some(ep) = download_data.encrypted_payload.as_deref()
            {
                ep.as_bytes().to_vec()
            } else if let Some(tj) = &download_data.template_json {
                serde_json::to_vec(tj)
                    .with_context(|| "serialize template_json for signature verification")?
            } else {
                return Err(anyhow!(
                    "signature present but no payload to verify against"
                ));
            };
            verify_component_signature(&signed_data, sig_b64, pub_pem).with_context(|| {
                format!(
                    "component signature verification failed for {}",
                    download_data.component_id
                )
            })?;
        }
        (Some(_sig_b64), None) => {
            // Signature present but no trusted key — cannot verify
            if skip_sig_check {
                eprintln!("[WARN] WPTSALL_SKIP_SIGNATURE_CHECK=true — skipping signature verification for component {} (development only!)", download_data.component_id);
            } else {
                anyhow::bail!(
                    "No trusted public key configured to verify signature for component {}. \
                     Set WPTSALL_SKIP_SIGNATURE_CHECK=true to bypass (development only).",
                    download_data.component_id
                );
            }
        }
        (None, Some(_pub_pem)) => {
            // Trusted key is configured but component download has no signature.
            // In normal mode this is a hard failure. In mock/dev mode, allow the
            // explicit opt-out flag to bypass the requirement.
            if skip_sig_check {
                eprintln!(
                    "[WARN] WPTSALL_SKIP_SIGNATURE_CHECK=true — signature missing for component {} even though a trusted public key is configured (development only!)",
                    download_data.component_id
                );
            } else {
                eprintln!(
                    "Component download has no signature — rejected for security (component={})",
                    download_data.component_id
                );
                anyhow::bail!(
                    "Component signature is required but missing (component={})",
                    download_data.component_id
                );
            }
        }
        (None, None) => {
            // No signature and no trusted key — reject unless explicitly opted out
            if skip_sig_check {
                eprintln!("[WARN] WPTSALL_SKIP_SIGNATURE_CHECK=true — no signature or trusted key for component {} (development only!)", download_data.component_id);
            } else {
                anyhow::bail!(
                    "Component signature verification is required but neither signature nor trusted public key \
                     is available for component {}. Set WPTSALL_SKIP_SIGNATURE_CHECK=true to bypass (development only).",
                    download_data.component_id
                );
            }
        }
    }

    if let Some(template_json) = download_data.template_json.clone() {
        return Ok(template_json);
    }

    let encrypted_payload = download_data
        .encrypted_payload
        .as_deref()
        .ok_or_else(|| anyhow!("missing template_json/encrypted_payload in component download"))?;
    let nonce = download_data
        .nonce
        .as_deref()
        .ok_or_else(|| anyhow!("missing nonce in encrypted component download"))?;
    let algorithm = download_data
        .algorithm
        .as_deref()
        .unwrap_or(COMPONENT_CRYPTO_ALGO_AES);
    let use_hkdf = download_data.kdf_version.as_deref() == Some(COMPONENT_KDF_VERSION_HKDF);

    let owner_type = Some(download_data.owner_type.as_str());

    match algorithm {
        COMPONENT_CRYPTO_ALGO_AES => {
            client_runtime_core::component_crypto::decrypt_component_template_aes(
                encrypted_payload,
                session_token,
                nonce,
                &download_data.component_id,
                use_hkdf,
                owner_type,
            )
        }
        COMPONENT_CRYPTO_ALGO_XOR_LEGACY => {
            anyhow::bail!("xor-sha256-v1 cipher is deprecated and no longer supported. Server should use aes-256-gcm.");
        }
        _ => Err(anyhow!(
            "unsupported component payload algorithm: {}",
            algorithm
        )),
    }
}

/// Verify a component signature using RSA-PKCS#1 v1.5 + SHA-256.
/// Delegates to `client_runtime_core::signed_catalog` (shared with L1/L2 catalogs).
pub(crate) fn verify_component_signature(
    data: &[u8],
    signature_b64: &str,
    public_key_pem: &str,
) -> anyhow::Result<()> {
    client_runtime_core::signed_catalog::verify_rsa_pkcs1_sha256(
        data,
        signature_b64,
        public_key_pem,
    )
    .map_err(|e| match e {
        client_runtime_core::signed_catalog::CatalogSignatureError::MissingSignature => {
            anyhow!("signature missing")
        }
        client_runtime_core::signed_catalog::CatalogSignatureError::SignatureDecodeFailed => {
            anyhow!("decode signature base64 / format failed")
        }
        client_runtime_core::signed_catalog::CatalogSignatureError::SignatureInvalid => {
            anyhow!("signature verification failed: data integrity compromised")
        }
        other => anyhow!("signature verification failed: {}", other.code()),
    })
}

#[allow(dead_code)]
pub(crate) fn derive_component_crypto_key(
    session_token: &str,
    nonce: &str,
    component_id: &str,
) -> [u8; 32] {
    client_runtime_core::component_crypto::derive_component_crypto_key(
        session_token,
        nonce,
        component_id,
    )
}

#[allow(dead_code)]
pub(crate) fn derive_component_crypto_nonce(nonce: &str, component_id: &str) -> [u8; 12] {
    client_runtime_core::component_crypto::derive_component_crypto_nonce(nonce, component_id)
}

#[allow(dead_code)]
pub(crate) fn derive_component_crypto_key_hkdf(
    session_token: &str,
    nonce: &str,
    component_id: &str,
) -> [u8; 32] {
    client_runtime_core::component_crypto::derive_component_crypto_key_hkdf(
        session_token,
        nonce,
        component_id,
    )
}

#[allow(dead_code)]
pub(crate) fn derive_component_crypto_key_hkdf_with_owner(
    session_token: &str,
    nonce: &str,
    component_id: &str,
    owner_type: Option<&str>,
) -> [u8; 32] {
    client_runtime_core::component_crypto::derive_component_crypto_key_hkdf_with_owner(
        session_token,
        nonce,
        component_id,
        owner_type,
    )
}

#[allow(dead_code)]
pub(crate) fn derive_component_crypto_nonce_hkdf(nonce: &str, component_id: &str) -> [u8; 12] {
    client_runtime_core::component_crypto::derive_component_crypto_nonce_hkdf(nonce, component_id)
}

/// Decrypt a Server encrypted response envelope (RFC #183).
///
/// Key derivation:
///   HKDF-SHA256(IKM=ikm, Salt=nonce_bytes, Info=kdf_info) → 32-byte AES key
///   AES-256-GCM decrypt(key, nonce_bytes, ciphertext+tag)
///
/// Unlike `transport_decrypt()` which uses a packed format with fixed HKDF Info,
/// this function takes an externally-provided nonce and per-endpoint `kdf_info`.
#[allow(dead_code)]
pub(crate) fn decrypt_server_response(
    encrypted_payload_b64: &str,
    nonce_b64url: &str,
    kdf_info: &str,
    ikm: &str,
) -> anyhow::Result<Vec<u8>> {
    client_runtime_core::signed_catalog::decrypt_server_response(
        encrypted_payload_b64,
        nonce_b64url,
        kdf_info,
        ikm,
    )
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

// -- Request signing (Client → WP Plugin, Protocol v2) --

const SIGNING_HKDF_SALT: &[u8] = b"request-signing";
const SIGNING_HKDF_INFO: &[u8] = b"wptsall-signing-v1";

/// Derive HMAC-SHA256 signing key from the WP client token.
///
/// ```text
/// signing_key = HKDF-SHA256(
///     ikm  = client_api_token,
///     salt = "request-signing",
///     info = "wptsall-signing-v1",
///     length = 32,
/// )
/// ```
pub(crate) fn derive_signing_key(client_token: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(SIGNING_HKDF_SALT), client_token.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(SIGNING_HKDF_INFO, &mut key)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    key
}

/// Build the canonical string for request signing.
///
/// ```text
/// {METHOD}\n{PATH_WITH_QUERY}\n{TIMESTAMP}\n{NONCE}\n{SHA256(body)}\n{SHA256(signed_headers)}
/// ```
///
/// For GET requests (no body), `body` should be an empty slice.
pub(crate) fn build_canonical_string(
    method: &str,
    path_with_query: &str,
    timestamp: &str,
    nonce: &str,
    body: &[u8],
    signed_headers: &[(&str, &str)],
) -> String {
    let body_hash = {
        let mut hasher = Sha256::new();
        hasher.update(body);
        to_hex(&hasher.finalize())
    };
    let signed_headers_hash = {
        let mut hasher = Sha256::new();
        hasher.update(canonicalize_signed_headers(signed_headers).as_bytes());
        to_hex(&hasher.finalize())
    };
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path_with_query,
        timestamp,
        nonce,
        body_hash,
        signed_headers_hash
    )
}

fn canonicalize_signed_headers(headers: &[(&str, &str)]) -> String {
    let mut normalized: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(name, value)| {
            let header_name = name.trim().to_ascii_lowercase();
            if header_name.is_empty() || !SIGNED_REQUEST_HEADERS.contains(&header_name.as_str()) {
                return None;
            }
            let header_value = value.trim();
            if header_value.is_empty() {
                return None;
            }
            Some((header_name, header_value.to_string()))
        })
        .collect();
    normalized.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut out = String::new();
    for (name, value) in normalized {
        out.push_str(&name);
        out.push(':');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

/// Compute HMAC-SHA256 signature and return base64url-no-pad encoded result.
pub(crate) fn compute_request_signature(signing_key: &[u8; 32], canonical_string: &str) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;

    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(signing_key).expect("HMAC can take key of any size");
    mac.update(canonical_string.as_bytes());
    let result = mac.finalize();
    URL_SAFE_NO_PAD.encode(result.into_bytes())
}

/// Verify a response signature from `X-WPTSALL-Response-Signature` header.
///
/// The signature covers the **plaintext** response JSON (after decryption,
/// before any client-side parsing).
pub(crate) fn verify_response_signature(
    signing_key: &[u8; 32],
    response_body: &[u8],
    expected_sig_b64url: &str,
) -> bool {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;

    let expected_bytes = match URL_SAFE_NO_PAD.decode(expected_sig_b64url) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(signing_key).expect("HMAC can take key of any size");
    mac.update(response_body);
    mac.verify_slice(&expected_bytes).is_ok()
}

// -- Transport encryption (Client ↔ WP Plugin) --
// Wire crypto lives in `client_runtime_core::wp_transport` (aligned with PHP
// `Transport_Crypto`). Policy (when to encrypt) stays in `auth.rs`.

#[allow(dead_code)]
pub(crate) fn derive_transport_key(token: &str, nonce: &[u8]) -> [u8; 32] {
    client_runtime_core::wp_transport::derive_transport_key(token, nonce)
}

pub(crate) fn transport_encrypt(plaintext: &[u8], token: &str) -> anyhow::Result<(String, String)> {
    client_runtime_core::wp_transport::transport_encrypt(plaintext, token)
}

pub(crate) fn transport_decrypt(
    encrypted_payload: &str,
    nonce_b64url: &str,
    token: &str,
) -> anyhow::Result<Vec<u8>> {
    client_runtime_core::wp_transport::transport_decrypt(encrypted_payload, nonce_b64url, token)
}

#[cfg(test)]
mod tests;
