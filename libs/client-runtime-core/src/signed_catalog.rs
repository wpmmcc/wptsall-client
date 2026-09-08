//! RSA catalog signature verification + encrypted response envelope helpers.
//!
//! L1/L2 catalog lists and L3 component downloads share `component-signature-v1`
//! over `encrypted_payload` base64 ASCII bytes.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use rsa::pkcs1v15::VerifyingKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use rsa::RsaPublicKey;
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

pub const CATALOG_SIGNATURE_CONTRACT: &str = "component-signature-v1";
pub const CATALOG_SIGNATURE_ALGORITHM: &str = "RSA-PKCS1v15-RAW-SHA256";
pub const CATALOG_SIGNATURE_SCOPE: &str = "encrypted_payload_base64_bytes";
pub const RESPONSE_CRYPTO_ALGO: &str = "AES-256-GCM";
pub const RESPONSE_KDF_VERSION: &str = "hkdf-sha256-v1";
pub const RESPONSE_NONCE_INFO: &[u8] = b"wptsall-response-nonce-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSignatureError {
    MissingSignature,
    SignatureDecodeFailed,
    SignatureInvalid,
    UnsupportedContract,
    UnsupportedScope,
}

impl CatalogSignatureError {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingSignature => "SIGNATURE_MISSING",
            Self::SignatureDecodeFailed => "SIGNATURE_DECODE_FAILED",
            Self::SignatureInvalid => "SIGNATURE_INVALID",
            Self::UnsupportedContract => "SIGNATURE_CONTRACT_UNSUPPORTED",
            Self::UnsupportedScope => "SIGNATURE_SCOPE_UNSUPPORTED",
        }
    }
}

/// Verify RSA-PKCS#1 v1.5 unprefixed SHA-256 signature (shared by L3 components
/// and catalog envelopes).
pub fn verify_rsa_pkcs1_sha256(
    data: &[u8],
    signature_b64: &str,
    public_key_pem: &str,
) -> Result<(), CatalogSignatureError> {
    let signature_b64 = signature_b64.trim();
    if signature_b64.is_empty() {
        return Err(CatalogSignatureError::MissingSignature);
    }
    let signature_bytes = BASE64_STANDARD
        .decode(signature_b64)
        .map_err(|_| CatalogSignatureError::SignatureDecodeFailed)?;
    let signature = rsa::pkcs1v15::Signature::try_from(signature_bytes.as_slice())
        .map_err(|_| CatalogSignatureError::SignatureDecodeFailed)?;

    let public_key = RsaPublicKey::from_public_key_pem(public_key_pem.trim())
        .map_err(|_| CatalogSignatureError::SignatureInvalid)?;
    let verifying_key = VerifyingKey::<Sha256>::new_unprefixed(public_key);
    verifying_key
        .verify(data, &signature)
        .map_err(|_| CatalogSignatureError::SignatureInvalid)?;
    Ok(())
}

/// Verify RSA catalog signature over encrypted payload base64 bytes.
pub fn verify_catalog_signature(
    encrypted_payload_b64: &str,
    signature_b64: &str,
    public_key_pem: &str,
    signature_contract_version: Option<&str>,
    signature_scope: Option<&str>,
) -> Result<(), CatalogSignatureError> {
    if let Some(contract) = signature_contract_version {
        if contract != CATALOG_SIGNATURE_CONTRACT {
            return Err(CatalogSignatureError::UnsupportedContract);
        }
    }
    if let Some(scope) = signature_scope {
        if scope != CATALOG_SIGNATURE_SCOPE {
            return Err(CatalogSignatureError::UnsupportedScope);
        }
    }
    verify_rsa_pkcs1_sha256(
        encrypted_payload_b64.as_bytes(),
        signature_b64,
        public_key_pem,
    )
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SignedEncryptedEnvelope {
    pub encrypted_payload: String,
    pub nonce: String,
    pub kdf_info: String,
    pub algorithm: Option<String>,
    pub kdf_version: Option<String>,
    pub signature: Option<String>,
    pub signing_key_id: Option<String>,
    pub signature_algorithm: Option<String>,
    pub signature_scope: Option<String>,
    pub signature_contract_version: Option<String>,
}

/// Decrypt a signed catalog envelope after optional signature verification.
pub fn decrypt_signed_envelope(
    envelope: &SignedEncryptedEnvelope,
    ikm: &str,
    trusted_public_key_pem: Option<&str>,
    skip_signature_check: bool,
) -> anyhow::Result<Vec<u8>> {
    if let Some(sig) = envelope
        .signature
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        if let Some(pem) = trusted_public_key_pem {
            verify_catalog_signature(
                &envelope.encrypted_payload,
                sig,
                pem,
                envelope.signature_contract_version.as_deref(),
                envelope.signature_scope.as_deref(),
            )
            .map_err(|e| anyhow!("catalog signature verification failed: {}", e.code()))?;
        } else if skip_signature_check {
            eprintln!("[WARN] skipping catalog signature verification (no trusted public key)");
        } else {
            anyhow::bail!("catalog signature present but no trusted public key configured");
        }
    } else if trusted_public_key_pem.is_some() && !skip_signature_check {
        anyhow::bail!("catalog signature required but missing from server response");
    }

    decrypt_encrypted_payload(envelope, ikm)
}

/// Decrypt a Server encrypted response envelope (RFC #183).
///
/// Key derivation (current format):
///   HKDF-SHA256(IKM=ikm, Salt=nonce_b64url_ascii, Info=kdf_info) → AES key
///   GCM nonce = HKDF(ikm=nonce_b64url_ascii, salt=kdf_info, info=RESPONSE_NONCE_INFO)
///
/// Falls back to legacy format when the current algorithm fails.
pub fn decrypt_server_response(
    encrypted_payload_b64: &str,
    nonce_b64url: &str,
    kdf_info: &str,
    ikm: &str,
) -> anyhow::Result<Vec<u8>> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let ciphertext_and_tag = BASE64_STANDARD
        .decode(encrypted_payload_b64)
        .context("decode server response encrypted_payload (base64) failed")?;

    let current_attempt = (|| -> anyhow::Result<Vec<u8>> {
        let hk_key = Hkdf::<Sha256>::new(Some(nonce_b64url.as_bytes()), ikm.as_bytes());
        let mut key = [0u8; 32];
        hk_key
            .expand(kdf_info.as_bytes(), &mut key)
            .map_err(|e| anyhow!("hkdf expand key: {e}"))?;

        let hk_nonce = Hkdf::<Sha256>::new(Some(kdf_info.as_bytes()), nonce_b64url.as_bytes());
        let mut gcm_nonce = [0u8; 12];
        hk_nonce
            .expand(RESPONSE_NONCE_INFO, &mut gcm_nonce)
            .map_err(|e| anyhow!("hkdf expand nonce: {e}"))?;

        let cipher =
            Aes256Gcm::new_from_slice(&key).context("init server response cipher failed")?;
        cipher
            .decrypt(Nonce::from_slice(&gcm_nonce), ciphertext_and_tag.as_ref())
            .map_err(|e| anyhow!("server response decryption failed: {e}"))
    })();
    if let Ok(plaintext) = current_attempt {
        return Ok(plaintext);
    }

    // Legacy fallback: key = HKDF(ikm, salt=nonce_bytes, info=kdf_info), nonce = nonce_bytes
    let nonce_bytes = URL_SAFE_NO_PAD
        .decode(nonce_b64url)
        .context("decode server response nonce (base64url) failed")?;
    if nonce_bytes.len() != 12 {
        return Err(anyhow!(
            "server response nonce must be 12 bytes, got {}",
            nonce_bytes.len()
        ));
    }

    let hk = Hkdf::<Sha256>::new(Some(&nonce_bytes), ikm.as_bytes());
    let mut legacy_key = [0u8; 32];
    hk.expand(kdf_info.as_bytes(), &mut legacy_key)
        .map_err(|e| anyhow!("hkdf expand legacy key: {e}"))?;

    let cipher =
        Aes256Gcm::new_from_slice(&legacy_key).context("init server response cipher failed")?;
    cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext_and_tag.as_ref())
        .map_err(|e| anyhow!("server response decryption failed: {e}"))
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct EncryptedResponseFields {
    encrypted_payload: String,
    nonce: String,
    kdf_info: String,
    algorithm: Option<String>,
    kdf_version: Option<String>,
    signature: Option<String>,
    signing_key_id: Option<String>,
    signature_algorithm: Option<String>,
    signature_scope: Option<String>,
    signature_contract_version: Option<String>,
}

/// Detect and decrypt an encrypted Server HTTP response body.
/// Returns `Ok(None)` when the body is plaintext JSON (not an envelope).
pub fn try_decrypt_server_response_body(
    body: &str,
    ikm: &str,
    trusted_public_key_pem: Option<&str>,
    skip_signature_check: bool,
) -> anyhow::Result<Option<String>> {
    let root: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let mut candidates: Vec<&Value> = vec![&root];
    if let Some(data) = root.get("data") {
        candidates.push(data);
    }

    for candidate in candidates {
        if candidate.get("encrypted").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }

        let payload_source = candidate
            .get("data")
            .filter(|v| v.is_object())
            .unwrap_or(candidate);

        if payload_source.get("signature").is_some()
            || payload_source.get("encrypted_payload").is_some()
        {
            let envelope: SignedEncryptedEnvelope = serde_json::from_value(payload_source.clone())
                .map_err(|_| anyhow!("invalid signed encrypted response envelope fields"))?;
            let plaintext_bytes = decrypt_signed_envelope(
                &envelope,
                ikm,
                trusted_public_key_pem,
                skip_signature_check,
            )?;
            let plaintext = String::from_utf8(plaintext_bytes)
                .context("decrypted server response is not valid UTF-8")?;
            return Ok(Some(plaintext));
        }

        let data: EncryptedResponseFields = serde_json::from_value(payload_source.clone())
            .map_err(|_| anyhow!("invalid encrypted response envelope fields"))?;
        let algorithm = data.algorithm.as_deref().unwrap_or(RESPONSE_CRYPTO_ALGO);
        if algorithm != RESPONSE_CRYPTO_ALGO {
            return Err(anyhow!(
                "unsupported server response algorithm: {}",
                algorithm
            ));
        }
        let kdf_version = data.kdf_version.as_deref().unwrap_or(RESPONSE_KDF_VERSION);
        if kdf_version != RESPONSE_KDF_VERSION {
            return Err(anyhow!(
                "unsupported server response kdf_version: {}",
                kdf_version
            ));
        }

        if let Some(sig) = data.signature.as_deref().filter(|s| !s.trim().is_empty()) {
            if let Some(pem) = trusted_public_key_pem {
                verify_catalog_signature(
                    &data.encrypted_payload,
                    sig,
                    pem,
                    data.signature_contract_version.as_deref(),
                    data.signature_scope.as_deref(),
                )
                .map_err(|e| anyhow!("catalog signature verification failed: {}", e.code()))?;
            } else if !skip_signature_check {
                anyhow::bail!("catalog signature present but no trusted public key configured");
            }
        }

        let plaintext_bytes =
            decrypt_server_response(&data.encrypted_payload, &data.nonce, &data.kdf_info, ikm)
                .context("decrypt server response envelope failed")?;
        let plaintext = String::from_utf8(plaintext_bytes)
            .context("decrypted server response is not valid UTF-8")?;
        return Ok(Some(plaintext));
    }

    Ok(None)
}

fn decrypt_encrypted_payload(
    envelope: &SignedEncryptedEnvelope,
    ikm: &str,
) -> anyhow::Result<Vec<u8>> {
    let algorithm = envelope
        .algorithm
        .as_deref()
        .unwrap_or(RESPONSE_CRYPTO_ALGO);
    if algorithm != RESPONSE_CRYPTO_ALGO {
        return Err(anyhow!("unsupported response algorithm: {}", algorithm));
    }
    let kdf_version = envelope
        .kdf_version
        .as_deref()
        .unwrap_or(RESPONSE_KDF_VERSION);
    if kdf_version != RESPONSE_KDF_VERSION {
        return Err(anyhow!("unsupported response kdf_version: {}", kdf_version));
    }

    decrypt_server_response(
        &envelope.encrypted_payload,
        &envelope.nonce,
        &envelope.kdf_info,
        ikm,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::signature::{SignatureEncoding, SignerMut};
    use rsa::RsaPrivateKey;

    fn test_keypair() -> (RsaPrivateKey, String) {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key");
        let public_pem = RsaPublicKey::from(&private)
            .to_public_key_pem(LineEnding::LF)
            .expect("pem");
        (private, public_pem)
    }

    #[test]
    fn verify_catalog_signature_roundtrip() {
        let (private, public_pem) = test_keypair();
        let payload = "ZmFrZUVuY3J5cHRlZFBheWxvYWQ=";
        let mut signing_key = SigningKey::<Sha256>::new_unprefixed(private);
        let sig = BASE64_STANDARD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
        verify_catalog_signature(
            payload,
            &sig,
            &public_pem,
            Some(CATALOG_SIGNATURE_CONTRACT),
            Some(CATALOG_SIGNATURE_SCOPE),
        )
        .expect("valid signature");
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let (private, public_pem) = test_keypair();
        let payload = "ZmFrZQ==";
        let mut signing_key = SigningKey::<Sha256>::new_unprefixed(private);
        let sig = BASE64_STANDARD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
        let err = verify_catalog_signature("dGFtcGVk", &sig, &public_pem, None, None)
            .expect_err("tampered");
        assert_eq!(err, CatalogSignatureError::SignatureInvalid);
    }

    #[test]
    fn decrypt_server_response_current_format_roundtrip() {
        use aes_gcm::aead::{Aead, KeyInit};
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use rand::RngCore;

        let plaintext = br#"{"success":true,"data":[]}"#;
        let kdf_info = "wptsall-domains-v1";
        let ikm = "sess_test_token";

        let mut nonce_seed = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_seed);
        let nonce_b64url = URL_SAFE_NO_PAD.encode(nonce_seed);

        let hk = Hkdf::<Sha256>::new(Some(nonce_b64url.as_bytes()), ikm.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(kdf_info.as_bytes(), &mut key).unwrap();

        let hk_nonce = Hkdf::<Sha256>::new(Some(kdf_info.as_bytes()), nonce_b64url.as_bytes());
        let mut gcm_nonce = [0u8; 12];
        hk_nonce
            .expand(RESPONSE_NONCE_INFO, &mut gcm_nonce)
            .unwrap();

        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let encrypted = cipher
            .encrypt(Nonce::from_slice(&gcm_nonce), plaintext.as_ref())
            .unwrap();
        let payload_b64 = BASE64_STANDARD.encode(encrypted);

        let decrypted =
            decrypt_server_response(&payload_b64, &nonce_b64url, kdf_info, ikm).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn try_decrypt_server_response_body_plaintext_returns_none() {
        let body = r#"{"success":true,"data":{"items":[]}}"#;
        let result = try_decrypt_server_response_body(body, "ikm", None, false).unwrap();
        assert!(result.is_none());
    }
}
