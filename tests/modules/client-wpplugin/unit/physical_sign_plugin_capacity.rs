use base64::Engine;
use rsa::pkcs8::{EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, SignerMut};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn owned_download(root: &std::path::Path, bytes: &[u8]) -> anyhow::Result<bool> {
    let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let public = rsa::RsaPublicKey::from(&private)
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let mut signing = rsa::pkcs1v15::SigningKey::<Sha256>::new_unprefixed(private);
    let signature =
        base64::engine::general_purpose::STANDARD.encode(signing.sign(bytes).to_bytes());
    let hash = format!("{:x}", Sha256::digest(bytes));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let bytes = bytes.to_vec();
    let server = tokio::spawn(async move {
        for step in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let length = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            let (headers, body) = if step == 0 {
                assert!(request.starts_with("GET /api/v1/client/sign-plugin/version "));
                (
                    "Content-Type: application/json\r\n".to_string(),
                    serde_json::to_vec(&serde_json::json!({
                        "success":true,
                        "data":{"version":"owned-1","sha256":hash,"size":bytes.len(),"available":true}
                    }))
                    .unwrap(),
                )
            } else {
                assert!(request.starts_with("GET /api/v1/client/sign-plugin/download "));
                (
                    format!(
                        "Content-Type: application/wasm\r\nX-Plugin-SHA256: {hash}\r\nX-Plugin-Signature: {signature}\r\nX-Plugin-Signing-Key-Id: owned-key\r\n"
                    ),
                    bytes.clone(),
                )
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let result = super::auto_download_sign_plugin(
        &client,
        &format!("http://{address}"),
        "owned-session",
        root.join("sign.wasm").to_str().unwrap(),
        Some(&public),
        Some("owned-key"),
        root.join("owned-sign.log").to_str().unwrap(),
    )
    .await;
    server.await.unwrap();
    result
}

#[tokio::test]
async fn physical_sign_plugin_full_root_keeps_original_after_actual_signed_download() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let _signature = crate::db::TestEnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "false");
    let path = root.path().join("sign.wasm");
    std::fs::write(&path, b"original-owned-signer").unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    assert!(owned_download(root.path(), b"\0asm\x01\0\0\0")
        .await
        .is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"original-owned-signer");
}

#[tokio::test]
async fn physical_sign_plugin_room_installs_same_verified_bytes_as_private_snapshot() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let _signature = crate::db::TestEnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "false");
    let bytes = b"\0asm\x01\0\0\0";
    assert!(owned_download(root.path(), bytes).await.unwrap());
    let path = root.path().join("sign.wasm");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn physical_snapshot_new_parents_are_private_without_rechmod_of_existing_directory() {
    use std::os::unix::fs::PermissionsExt;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let parent = root.path().join("new-parent").join("new-child");
    crate::bindings::atomic_file::install(&parent.join("owned.bin"), b"owned-snapshot").unwrap();
    for directory in [parent.clone(), parent.parent().unwrap().to_path_buf()] {
        assert_eq!(
            std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let existing = root.path().join("existing-wide-directory");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::bindings::atomic_file::install(&existing.join("owned.bin"), b"owned-snapshot").unwrap();
    assert_eq!(
        std::fs::metadata(existing).unwrap().permissions().mode() & 0o777,
        0o755
    );
}
