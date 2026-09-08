use super::*;
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use serde_json::json;
use sha2::Sha256;

/// Allow tests that don't care about signature verification to bypass the check.
fn allow_skip_signature_check() -> crate::db::TestEnvVarGuard {
    crate::db::TestEnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true")
}

// -----------------------------------------------------------------------
// Legacy SHA-256 key derivation
// -----------------------------------------------------------------------

#[test]
fn derive_key_deterministic() {
    let key1 = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-001");
    let key2 = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-001");
    assert_eq!(key1, key2, "same inputs must produce same key");
}

#[test]
fn derive_key_changes_with_different_session() {
    let key1 = derive_component_crypto_key("session-a", "nonce-xyz", "comp-001");
    let key2 = derive_component_crypto_key("session-b", "nonce-xyz", "comp-001");
    assert_ne!(key1, key2);
}

#[test]
fn derive_key_changes_with_different_nonce() {
    let key1 = derive_component_crypto_key("session-abc", "nonce-1", "comp-001");
    let key2 = derive_component_crypto_key("session-abc", "nonce-2", "comp-001");
    assert_ne!(key1, key2);
}

#[test]
fn derive_key_changes_with_different_component() {
    let key1 = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-a");
    let key2 = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-b");
    assert_ne!(key1, key2);
}

#[test]
fn derive_key_length_is_32_bytes() {
    let key = derive_component_crypto_key("s", "n", "c");
    assert_eq!(key.len(), 32);
}

// -----------------------------------------------------------------------
// HKDF key derivation
// -----------------------------------------------------------------------

#[test]
fn hkdf_key_deterministic() {
    let key1 = derive_component_crypto_key_hkdf("session-abc", "nonce-xyz", "comp-001");
    let key2 = derive_component_crypto_key_hkdf("session-abc", "nonce-xyz", "comp-001");
    assert_eq!(key1, key2, "same inputs must produce same HKDF key");
}

#[test]
fn hkdf_key_changes_with_different_component() {
    let key1 = derive_component_crypto_key_hkdf("session-abc", "nonce-xyz", "comp-a");
    let key2 = derive_component_crypto_key_hkdf("session-abc", "nonce-xyz", "comp-b");
    assert_ne!(key1, key2);
}

#[test]
fn hkdf_key_differs_from_legacy() {
    let legacy = derive_component_crypto_key("session-abc", "nonce-xyz", "comp-001");
    let hkdf = derive_component_crypto_key_hkdf("session-abc", "nonce-xyz", "comp-001");
    assert_ne!(legacy, hkdf, "HKDF and legacy keys must differ");
}

#[test]
fn hkdf_nonce_deterministic() {
    let n1 = derive_component_crypto_nonce_hkdf("nonce-xyz", "comp-001");
    let n2 = derive_component_crypto_nonce_hkdf("nonce-xyz", "comp-001");
    assert_eq!(n1, n2);
}

// -----------------------------------------------------------------------
// Nonce derivation (legacy)
// -----------------------------------------------------------------------

#[test]
fn derive_nonce_deterministic() {
    let n1 = derive_component_crypto_nonce("nonce-xyz", "comp-001");
    let n2 = derive_component_crypto_nonce("nonce-xyz", "comp-001");
    assert_eq!(n1, n2);
}

#[test]
fn derive_nonce_length_is_12_bytes() {
    let n = derive_component_crypto_nonce("n", "c");
    assert_eq!(n.len(), 12);
}

#[test]
fn derive_nonce_changes_with_different_inputs() {
    let n1 = derive_component_crypto_nonce("nonce-a", "comp-001");
    let n2 = derive_component_crypto_nonce("nonce-b", "comp-001");
    assert_ne!(n1, n2);
}

// -----------------------------------------------------------------------
// AES-256-GCM encrypt / decrypt roundtrip (legacy SHA-256 KDF)
// -----------------------------------------------------------------------

#[test]
fn aes_gcm_roundtrip() {
    let _skip_sig_guard = allow_skip_signature_check();
    let session = "test-session-token-abc123";
    let nonce_str = "unique-nonce-value";
    let component_id = "component-roundtrip";
    let original = json!({"message": "hello world", "count": 42});

    // Encrypt with legacy KDF
    let key = derive_component_crypto_key(session, nonce_str, component_id);
    let nonce_bytes = derive_component_crypto_nonce(nonce_str, component_id);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher init");
    let plain_bytes = serde_json::to_vec(&original).expect("serialize");
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain_bytes.as_ref())
        .expect("encrypt");
    let encrypted_b64 = BASE64_STANDARD.encode(&encrypted);

    // Decrypt via resolve (no kdf_version => legacy path)
    let download_data = ComponentDownloadData {
        component_id: component_id.to_string(),
        _version: "1.0".to_string(),
        template_json: None,
        encrypted_payload: Some(encrypted_b64),
        nonce: Some(nonce_str.to_string()),
        algorithm: Some(COMPONENT_CRYPTO_ALGO_AES.to_string()),
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };

    let result = resolve_download_template_json(&download_data, session, None)
        .expect("decrypt should succeed");
    assert_eq!(result, original);
}

// -----------------------------------------------------------------------
// AES-256-GCM encrypt / decrypt roundtrip (HKDF-SHA256 KDF)
// -----------------------------------------------------------------------

#[test]
fn aes_gcm_hkdf_roundtrip() {
    let _skip_sig_guard = allow_skip_signature_check();
    let session = "test-session-hkdf-token";
    let nonce_str = "hkdf-nonce-value";
    let component_id = "component-hkdf-roundtrip";
    let original = json!({"message": "hkdf hello", "count": 99});

    // Encrypt with HKDF
    let key = derive_component_crypto_key_hkdf(session, nonce_str, component_id);
    let nonce_bytes = derive_component_crypto_nonce_hkdf(nonce_str, component_id);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher init");
    let plain_bytes = serde_json::to_vec(&original).expect("serialize");
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain_bytes.as_ref())
        .expect("encrypt");
    let encrypted_b64 = BASE64_STANDARD.encode(&encrypted);

    // Decrypt via resolve (kdf_version present => HKDF path)
    let download_data = ComponentDownloadData {
        component_id: component_id.to_string(),
        _version: "1.0".to_string(),
        template_json: None,
        encrypted_payload: Some(encrypted_b64),
        nonce: Some(nonce_str.to_string()),
        algorithm: Some(COMPONENT_CRYPTO_ALGO_AES.to_string()),
        kdf_version: Some(COMPONENT_KDF_VERSION_HKDF.to_string()),
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };

    let result = resolve_download_template_json(&download_data, session, None)
        .expect("HKDF decrypt should succeed");
    assert_eq!(result, original);
}

#[test]
fn hkdf_encrypted_fails_without_kdf_version() {
    let _skip_sig_guard = allow_skip_signature_check();
    let session = "test-session-hkdf-token";
    let nonce_str = "hkdf-nonce-value";
    let component_id = "component-hkdf-fail";
    let original = json!({"data": "secret"});

    // Encrypt with HKDF
    let key = derive_component_crypto_key_hkdf(session, nonce_str, component_id);
    let nonce_bytes = derive_component_crypto_nonce_hkdf(nonce_str, component_id);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher init");
    let plain_bytes = serde_json::to_vec(&original).expect("serialize");
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain_bytes.as_ref())
        .expect("encrypt");
    let encrypted_b64 = BASE64_STANDARD.encode(&encrypted);

    // Try decrypt without kdf_version (falls back to legacy SHA-256 => should fail)
    let download_data = ComponentDownloadData {
        component_id: component_id.to_string(),
        _version: String::new(),
        template_json: None,
        encrypted_payload: Some(encrypted_b64),
        nonce: Some(nonce_str.to_string()),
        algorithm: Some(COMPONENT_CRYPTO_ALGO_AES.to_string()),
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };

    let result = resolve_download_template_json(&download_data, session, None);
    assert!(
        result.is_err(),
        "HKDF-encrypted data must fail with legacy KDF"
    );
}

#[test]
fn plaintext_template_json_passthrough() {
    let _skip_sig_guard = allow_skip_signature_check();
    let original = json!({"id": "comp-1", "name": "Test"});
    let download_data = ComponentDownloadData {
        component_id: "comp-1".to_string(),
        _version: String::new(),
        template_json: Some(original.clone()),
        encrypted_payload: None,
        nonce: None,
        algorithm: None,
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };

    let result = resolve_download_template_json(&download_data, "any-token", None)
        .expect("passthrough should succeed");
    assert_eq!(result, original);
}

#[test]
fn missing_both_template_and_payload_fails() {
    let _skip_sig_guard = allow_skip_signature_check();
    let download_data = ComponentDownloadData {
        component_id: "comp-x".to_string(),
        _version: String::new(),
        template_json: None,
        encrypted_payload: None,
        nonce: None,
        algorithm: None,
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };
    let result = resolve_download_template_json(&download_data, "token", None);
    assert!(result.is_err());
}

#[test]
fn unsupported_algorithm_fails() {
    let _skip_sig_guard = allow_skip_signature_check();
    let download_data = ComponentDownloadData {
        component_id: "comp-x".to_string(),
        _version: String::new(),
        template_json: None,
        encrypted_payload: Some("AAAA".to_string()),
        nonce: Some("nonce".to_string()),
        algorithm: Some("rsa-4096".to_string()),
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };
    let result = resolve_download_template_json(&download_data, "token", None);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unsupported"),
        "error should mention unsupported algo: {}",
        err_msg
    );
}

#[test]
fn wrong_session_token_fails_decrypt() {
    let _skip_sig_guard = allow_skip_signature_check();
    let session = "correct-session-token";
    let nonce_str = "nonce-val";
    let component_id = "comp-001";
    let original = json!({"data": "secret"});

    let key = derive_component_crypto_key(session, nonce_str, component_id);
    let nonce_bytes = derive_component_crypto_nonce(nonce_str, component_id);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher init");
    let plain_bytes = serde_json::to_vec(&original).expect("serialize");
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain_bytes.as_ref())
        .expect("encrypt");
    let encrypted_b64 = BASE64_STANDARD.encode(&encrypted);

    let download_data = ComponentDownloadData {
        component_id: component_id.to_string(),
        _version: String::new(),
        template_json: None,
        encrypted_payload: Some(encrypted_b64),
        nonce: Some(nonce_str.to_string()),
        algorithm: Some(COMPONENT_CRYPTO_ALGO_AES.to_string()),
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };

    let result = resolve_download_template_json(&download_data, "wrong-session-token", None);
    assert!(result.is_err(), "decrypt with wrong token should fail");
}

// -----------------------------------------------------------------------
// XOR legacy roundtrip
// -----------------------------------------------------------------------

#[test]
fn xor_legacy_rejected_as_deprecated() {
    let _skip_sig_guard = allow_skip_signature_check();
    // XOR dead code removed; just verify the dispatch rejects the algorithm
    let download_data = ComponentDownloadData {
        component_id: "comp-xor".to_string(),
        _version: String::new(),
        template_json: None,
        encrypted_payload: Some("AAAA".to_string()),
        nonce: Some("nonce-xor".to_string()),
        algorithm: Some(COMPONENT_CRYPTO_ALGO_XOR_LEGACY.to_string()),
        kdf_version: None,
        owner_type: String::new(),
        signature: None,
        signing_key_id: None,
    };

    let result = resolve_download_template_json(&download_data, "session-xor-test", None);
    assert!(
        result.is_err(),
        "xor-sha256-v1 cipher should be rejected as deprecated"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("deprecated"),
        "error should mention deprecated: {}",
        err_msg
    );
}

// -----------------------------------------------------------------------
// to_hex
// -----------------------------------------------------------------------

#[test]
fn to_hex_empty() {
    assert_eq!(to_hex(&[]), "");
}

#[test]
fn to_hex_known_values() {
    assert_eq!(to_hex(&[0x00]), "00");
    assert_eq!(to_hex(&[0xff]), "ff");
    assert_eq!(to_hex(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    assert_eq!(
        to_hex(&[0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]),
        "0123456789abcdef"
    );
}

// -----------------------------------------------------------------------
// Signature verification
// -----------------------------------------------------------------------

fn generate_test_keypair() -> (rsa::RsaPrivateKey, String) {
    let mut rng = rand::thread_rng();
    let private_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
    let public_key = rsa::RsaPublicKey::from(&private_key);
    let pem =
        rsa::pkcs8::EncodePublicKey::to_public_key_pem(&public_key, rsa::pkcs8::LineEnding::LF)
            .unwrap();
    (private_key, pem)
}

fn sign_test_data(private_key: &rsa::RsaPrivateKey, data: &[u8]) -> String {
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, SignerMut};
    let mut signing_key = SigningKey::<Sha256>::new_unprefixed(private_key.clone());
    let signature = signing_key.sign(data);
    BASE64_STANDARD.encode(signature.to_bytes())
}

#[test]
fn test_verify_valid_signature() {
    let (private_key, pub_pem) = generate_test_keypair();
    let data = b"test component payload data";
    let sig_b64 = sign_test_data(&private_key, data);
    assert!(verify_component_signature(data, &sig_b64, &pub_pem).is_ok());
}

#[test]
fn test_verify_invalid_signature() {
    let (private_key, pub_pem) = generate_test_keypair();
    let data = b"original data";
    let sig_b64 = sign_test_data(&private_key, data);
    let tampered = b"tampered data";
    assert!(verify_component_signature(tampered, &sig_b64, &pub_pem).is_err());
}

#[test]
fn test_resolve_with_signature_verification() {
    let (private_key, pub_pem) = generate_test_keypair();
    let session = "test-session-sig";
    let nonce_str = "nonce-sig-test";
    let component_id = "comp-sig-roundtrip";
    let original = json!({"name": "signed-component", "version": "2.0"});

    // Encrypt with HKDF
    let key = derive_component_crypto_key_hkdf(session, nonce_str, component_id);
    let nonce_bytes = derive_component_crypto_nonce_hkdf(nonce_str, component_id);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher init");
    let plain_bytes = serde_json::to_vec(&original).expect("serialize");
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plain_bytes.as_ref())
        .expect("encrypt");
    let encrypted_b64 = BASE64_STANDARD.encode(&encrypted);

    // Sign the encrypted payload
    let sig_b64 = sign_test_data(&private_key, encrypted_b64.as_bytes());

    let download_data = ComponentDownloadData {
        component_id: component_id.to_string(),
        _version: "2.0".to_string(),
        template_json: None,
        encrypted_payload: Some(encrypted_b64),
        nonce: Some(nonce_str.to_string()),
        algorithm: Some(COMPONENT_CRYPTO_ALGO_AES.to_string()),
        kdf_version: Some(COMPONENT_KDF_VERSION_HKDF.to_string()),
        owner_type: String::new(),
        signature: Some(sig_b64),
        signing_key_id: Some("test-key-id".to_string()),
    };

    // With correct public key: should verify and decrypt
    let result = resolve_download_template_json(&download_data, session, Some(&pub_pem))
        .expect("signed+encrypted roundtrip should succeed");
    assert_eq!(result, original);

    // With WPTSALL_SKIP_SIGNATURE_CHECK=true and no public key: should skip verification and decrypt
    let _skip_sig_guard = allow_skip_signature_check();
    let result2 = resolve_download_template_json(&download_data, session, None)
        .expect("no-key skip with WPTSALL_SKIP_SIGNATURE_CHECK=true should succeed");
    assert_eq!(result2, original);

    // With wrong public key: should fail verification
    let (_other_key, other_pem) = generate_test_keypair();
    let result3 = resolve_download_template_json(&download_data, session, Some(&other_pem));
    assert!(
        result3.is_err(),
        "wrong public key should fail verification"
    );
}

// -----------------------------------------------------------------------
// decrypt_server_response (RFC #183)
// -----------------------------------------------------------------------

/// Helper: encrypt a plaintext with the same HKDF-SHA256 → AES-256-GCM
/// scheme that the Server uses, so we can test the decrypt round-trip.
fn encrypt_server_response_for_test(
    plaintext: &[u8],
    kdf_info: &str,
    ikm: &str,
) -> (String, String) {
    use aes_gcm::aead::generic_array::GenericArray;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use rand::RngCore;

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    // Same KDF as decrypt_server_response: HKDF(IKM, Salt=nonce, Info=kdf_info)
    let hk = Hkdf::<Sha256>::new(Some(&nonce_bytes), ikm.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(kdf_info.as_bytes(), &mut key).unwrap();

    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let encrypted = cipher
        .encrypt(GenericArray::from_slice(&nonce_bytes), plaintext)
        .unwrap();

    let payload_b64 = BASE64_STANDARD.encode(&encrypted);
    let nonce_b64url = URL_SAFE_NO_PAD.encode(nonce_bytes);
    (payload_b64, nonce_b64url)
}

#[test]
fn server_response_decrypt_roundtrip() {
    let plaintext = br#"{"success":true,"data":{"items":[]}}"#;
    let kdf_info = "wptsall-domains-v1";
    let ikm = "sess_abc123def456";

    let (payload_b64, nonce_b64url) = encrypt_server_response_for_test(plaintext, kdf_info, ikm);

    let decrypted = decrypt_server_response(&payload_b64, &nonce_b64url, kdf_info, ikm)
        .expect("decrypt should succeed");
    assert_eq!(decrypted, plaintext);
}

#[test]
fn server_response_decrypt_wrong_ikm_fails() {
    let plaintext = b"secret data";
    let kdf_info = "wptsall-domains-v1";
    let ikm = "correct-session-token";

    let (payload_b64, nonce_b64url) = encrypt_server_response_for_test(plaintext, kdf_info, ikm);

    let result = decrypt_server_response(&payload_b64, &nonce_b64url, kdf_info, "wrong-token");
    assert!(result.is_err(), "wrong IKM should fail decryption");
}

#[test]
fn server_response_decrypt_wrong_kdf_info_fails() {
    let plaintext = b"secret data";
    let kdf_info = "wptsall-domains-v1";
    let ikm = "sess_token_xyz";

    let (payload_b64, nonce_b64url) = encrypt_server_response_for_test(plaintext, kdf_info, ikm);

    let result = decrypt_server_response(&payload_b64, &nonce_b64url, "wptsall-components-v1", ikm);
    assert!(
        result.is_err(),
        "wrong kdf_info should fail (per-endpoint isolation)"
    );
}

#[test]
fn server_response_decrypt_invalid_nonce_length() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    // 8 bytes instead of 12
    let short_nonce = URL_SAFE_NO_PAD.encode([0u8; 8]);
    let result = decrypt_server_response("AAAA", &short_nonce, "info", "ikm");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("12 bytes"),
        "should mention expected nonce length"
    );
}

#[test]
fn server_response_decrypt_deterministic() {
    let plaintext = b"hello world";
    let kdf_info = "wptsall-token-v1";
    let ikm = "code_verifier_abc";

    let (payload_b64, nonce_b64url) = encrypt_server_response_for_test(plaintext, kdf_info, ikm);

    let d1 = decrypt_server_response(&payload_b64, &nonce_b64url, kdf_info, ikm).unwrap();
    let d2 = decrypt_server_response(&payload_b64, &nonce_b64url, kdf_info, ikm).unwrap();
    assert_eq!(d1, d2, "same inputs must produce same plaintext");
}
