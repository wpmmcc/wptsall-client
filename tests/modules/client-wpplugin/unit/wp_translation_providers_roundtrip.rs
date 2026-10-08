//! End-to-end: spin up a minimal local HTTP server that returns the same
//! `wptsall-wp-providers-v1` encrypted envelope the real server sends,
//! then call `fetch_wp_translation_providers_for_session` and verify
//! the client decrypts it correctly and returns the seeded items.
//!
//! The test does not require the real server, network, or any
//! WordPress plugin. It is fully self-contained.
//!
//! Run:    cargo test --test wp_translation_providers_roundtrip -- --nocapture

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use wptsall_client::web_ui::{
    fetch_wp_translation_providers_for_session,
    fetch_wp_translation_providers_for_session_with_signing_key,
};

const KDF_INFO: &str = "wptsall-wp-providers-v1";
const IKM: &str = "test-session-token-abc";
const NONCE_INFO: &[u8] = b"wptsall-response-nonce-v1";

/// Build the same envelope the real server emits: AES-256-GCM over
/// `plain_data` keyed by HKDF(IKM, salt=nonce_b64url, info=kdf_info).
fn build_envelope(plain_data: &serde_json::Value) -> serde_json::Value {
    use aes_gcm::{
        aead::{Aead, KeyInit},
        Aes256Gcm, Nonce,
    };
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use hkdf::Hkdf;
    use sha2::Sha256;

    // Server uses a 16-char alphanumeric nonce.
    let nonce_chars = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let nonce_b64url: String = (0..16)
        .map(|i| nonce_chars[i % nonce_chars.len()] as char)
        .collect();

    // key = HKDF(salt=nonce_b64url, ikm=IKM, info=KDF_INFO)
    let hk = Hkdf::<Sha256>::new(Some(nonce_b64url.as_bytes()), IKM.as_bytes());
    let mut key = [0u8; 32];
    hk.expand(KDF_INFO.as_bytes(), &mut key).unwrap();

    // gcm_nonce = HKDF(salt=KDF_INFO, ikm=nonce_b64url, info="wptsall-response-nonce-v1")
    let hk_n = Hkdf::<Sha256>::new(Some(KDF_INFO.as_bytes()), nonce_b64url.as_bytes());
    let mut gcm_nonce = [0u8; 12];
    hk_n.expand(NONCE_INFO, &mut gcm_nonce).unwrap();

    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let plaintext = serde_json::to_vec(plain_data).unwrap();
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&gcm_nonce), plaintext.as_ref())
        .unwrap();

    serde_json::json!({
        "success": true,
        "data": {
            "encrypted": true,
            "encrypted_payload": STANDARD.encode(ciphertext),
            "nonce": nonce_b64url,
            "algorithm": "AES-256-GCM",
            "kdf_version": "hkdf-sha256-v1",
            "kdf_info": KDF_INFO,
        }
    })
}

/// Build the same envelope `wptsall-cloud-api-types-v1` style the real server
/// emits (same wire format, different kdf_info). Used to verify the client
/// rejects envelope drift with the right kdf_info.
fn build_envelope_with_info(plain_data: &serde_json::Value, kdf_info: &str) -> serde_json::Value {
    let mut env = build_envelope(plain_data);
    env["data"]["kdf_info"] = serde_json::Value::String(kdf_info.to_string());
    env
}

fn test_signing_keypair() -> (rsa::RsaPrivateKey, String) {
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::RsaPrivateKey;
    let mut rng = rand::thread_rng();
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("rsa key");
    let public_pem = rsa::RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .expect("pem");
    (private, public_pem)
}

fn build_signed_envelope(
    plain_data: &serde_json::Value,
    private_key: &rsa::RsaPrivateKey,
) -> serde_json::Value {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{SignatureEncoding, SignerMut};
    use sha2::Sha256;

    let mut envelope = build_envelope(plain_data);
    let payload_b64 = envelope["data"]["encrypted_payload"]
        .as_str()
        .expect("encrypted_payload");
    let mut signing_key = SigningKey::<Sha256>::new_unprefixed(private_key.clone());
    let signature = STANDARD.encode(signing_key.sign(payload_b64.as_bytes()).to_bytes());
    envelope["data"]["signature"] = serde_json::Value::String(signature);
    envelope["data"]["signing_key_id"] = serde_json::Value::String("test-signing-key".into());
    envelope["data"]["signature_algorithm"] =
        serde_json::Value::String("RSA-PKCS1v15-RAW-SHA256".into());
    envelope["data"]["signature_scope"] =
        serde_json::Value::String("encrypted_payload_base64_bytes".into());
    envelope["data"]["signature_contract_version"] =
        serde_json::Value::String("component-signature-v1".into());
    envelope
}

/// Minimal HTTP/1.1 server: only handles `GET /api/v1/client/wp-translation-providers`
/// and returns whatever envelope was set by the test via `set_payload`.
async fn run_mock_server(listener: TcpListener, payload: Arc<Mutex<Option<serde_json::Value>>>) {
    loop {
        let (mut stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => break,
        };
        let payload = payload.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            // We only care that the request is for wp-translation-providers.
            let path_ok = req.starts_with("GET /api/v1/client/wp-translation-providers ");
            // ^ ignore query string & extra whitespace
            let payload_guard = payload.lock().await;
            let response_body = if path_ok {
                match payload_guard.as_ref() {
                    Some(p) => serde_json::to_string(p).unwrap(),
                    None => {
                        String::from_utf8_lossy(br#"{"success":false,"error":{"code":"NOT_SET"}}"#)
                            .to_string()
                    }
                }
            } else {
                String::from_utf8_lossy(br#"{"success":false,"error":{"code":"NOT_FOUND"}}"#)
                    .to_string()
            };
            drop(payload_guard);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_wp_translation_providers_decrypts_envelope() {
    // Bind to an ephemeral port (port 0) and read the assigned port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_base = format!("http://127.0.0.1:{}", port);
    let payload = Arc::new(Mutex::new(None));
    let payload_clone = payload.clone();
    let server_handle = tokio::spawn(async move {
        run_mock_server(listener, payload_clone).await;
    });

    // Seed 10 providers with the same ids the real server seeds.
    let plain = serde_json::json!({
        "items": [
            {"id": "baidu-translate",   "name": "Baidu Translate",  "provider_kind": "http"},
            {"id": "youdao-translate",  "name": "Youdao Translate", "provider_kind": "http"},
            {"id": "google-translate",  "name": "Google Translate", "provider_kind": "http"},
            {"id": "deepl-translate",   "name": "DeepL",            "provider_kind": "http"},
            {"id": "openai-translate",  "name": "OpenAI GPT-4o",    "provider_kind": "http"},
            {"id": "claude-translate",  "name": "Claude Haiku",     "provider_kind": "http"},
            {"id": "microsoft-translate","name": "Microsoft",       "provider_kind": "http"},
            {"id": "tencent-translate", "name": "Tencent Transmart","provider_kind": "http"},
            {"id": "caiyun-translate",  "name": "Caiyun",           "provider_kind": "http"},
            {"id": "volcengine-translate","name": "Volcengine",     "provider_kind": "http"},
        ],
        "page": 1,
        "per_page": 20,
        "total": 10,
        "total_pages": 1,
    });
    let envelope = build_envelope(&plain);
    *payload.lock().await = Some(envelope);

    let client = reqwest::Client::new();
    let items = fetch_wp_translation_providers_for_session(&client, &server_base, IKM)
        .await
        .expect("fetch_wp_translation_providers_for_session");

    assert_eq!(
        items.len(),
        10,
        "expected 10 providers, got {}",
        items.len()
    );
    let ids: Vec<String> = items
        .iter()
        .map(|i| {
            serde_json::to_value(i).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    for expected in [
        "baidu-translate",
        "youdao-translate",
        "google-translate",
        "deepl-translate",
        "openai-translate",
        "claude-translate",
        "microsoft-translate",
        "tencent-translate",
        "caiyun-translate",
        "volcengine-translate",
    ] {
        assert!(
            ids.contains(&expected.to_string()),
            "missing seeded provider: {} (got {:?})",
            expected,
            ids
        );
    }

    server_handle.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_wp_translation_providers_verifies_signed_envelope() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_base = format!("http://127.0.0.1:{}", port);
    let payload = Arc::new(Mutex::new(None));
    let payload_clone = payload.clone();
    let server_handle = tokio::spawn(async move {
        run_mock_server(listener, payload_clone).await;
    });

    let (private_key, public_pem) = test_signing_keypair();
    let plain = serde_json::json!({
        "items": [{"id": "baidu-translate", "name": "Baidu Translate", "provider_kind": "http"}],
        "page": 1,
        "per_page": 20,
        "total": 1,
        "total_pages": 1,
    });
    let envelope = build_signed_envelope(&plain, &private_key);
    *payload.lock().await = Some(envelope);

    let client = reqwest::Client::new();
    let items = fetch_wp_translation_providers_for_session_with_signing_key(
        &client,
        &server_base,
        IKM,
        Some(&public_pem),
    )
    .await
    .expect("signed catalog fetch");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "baidu-translate");

    server_handle.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_wp_translation_providers_rejects_signed_without_trusted_key() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_base = format!("http://127.0.0.1:{}", port);
    let payload = Arc::new(Mutex::new(None));
    let payload_clone = payload.clone();
    let server_handle = tokio::spawn(async move {
        run_mock_server(listener, payload_clone).await;
    });

    let (private_key, _) = test_signing_keypair();
    let plain =
        serde_json::json!({"items": [], "page": 1, "per_page": 1, "total": 0, "total_pages": 0});
    let envelope = build_signed_envelope(&plain, &private_key);
    *payload.lock().await = Some(envelope);

    let client = reqwest::Client::new();
    let result = fetch_wp_translation_providers_for_session(&client, &server_base, IKM).await;
    assert!(
        result.is_err(),
        "signed envelope without trusted key should fail, got {:?}",
        result
    );

    server_handle.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_wp_translation_providers_rejects_kdf_info_drift() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_base = format!("http://127.0.0.1:{}", port);
    let payload = Arc::new(Mutex::new(None));
    let payload_clone = payload.clone();
    let server_handle = tokio::spawn(async move {
        run_mock_server(listener, payload_clone).await;
    });

    // Build envelope with the WRONG kdf_info string — decryption must fail.
    let plain =
        serde_json::json!({"items": [], "page": 1, "per_page": 1, "total": 0, "total_pages": 0});
    let envelope = build_envelope_with_info(&plain, "wptsall-WRONG-INFO-v9");
    *payload.lock().await = Some(envelope);

    let client = reqwest::Client::new();
    let result = fetch_wp_translation_providers_for_session(&client, &server_base, IKM).await;
    assert!(
        result.is_err(),
        "expected decrypt failure on kdf_info drift, got {:?}",
        result
    );

    server_handle.abort();
}
