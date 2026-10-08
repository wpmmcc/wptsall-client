//! Owned scratch and actual ephemeral signed HTTP upload; no real WP site.
use super::*;

#[test]
fn encrypted_spool_upload_scratch_never_persists_plaintext() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let bytes = b"owned-upload-spool-private-content";
    let path =
        stage_chunk_upload_bytes(root.path().to_str().unwrap(), 9, "owned.bin", bytes).unwrap();
    let stored = std::fs::read(&path).unwrap();
    assert!(
        !stored.windows(bytes.len()).any(|window| window == bytes),
        "tmp-media-upload must not retain plaintext scratch"
    );
    assert_eq!(crate::retained_assets::read(&path, 128).unwrap(), bytes);
}

#[tokio::test]
async fn encrypted_spool_actual_submitter_preserves_pdf_plaintext_signature_and_one_upload() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "always");
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let path = root.path().join("wpa1owned-result.pdf");
    let plaintext = b"%PDF-1.7\nowned private paid document".to_vec();
    crate::retained_assets::write_bytes(&path, &plaintext).unwrap();
    let original = std::fs::read(&path).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut headers = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            headers.push_str(&line);
        }
        let mut wire = vec![0; length];
        reader.read_exact(&mut wire).await.unwrap();
        let field = |name: &str| -> String {
            headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_string())
                })
                .unwrap()
        };
        assert_eq!(field("X-WPTSALL-Filename"), "owned-result.pdf");
        assert_eq!(field("Content-Type"), "application/pdf");
        assert_eq!(field("X-WPTSALL-Transport"), "encrypted");
        let envelope: Value = serde_json::from_slice(&wire).unwrap();
        let recovered = crate::crypto::transport_decrypt(
            envelope["encrypted_payload"].as_str().unwrap(),
            envelope["nonce"].as_str().unwrap(),
            "owned-token",
        )
        .unwrap();
        assert_eq!(recovered, plaintext);
        assert_eq!(
            field("X-WPTSALL-Content-SHA256"),
            crate::sync_engine::hmac::sha256_hex(&plaintext)
        );
        let signed = [
            ("X-WPTSALL-Filename", field("X-WPTSALL-Filename")),
            ("X-WPTSALL-Task-ID", field("X-WPTSALL-Task-ID")),
            ("X-WPTSALL-Source-ID", field("X-WPTSALL-Source-ID")),
            ("X-WPTSALL-Relation-ID", field("X-WPTSALL-Relation-ID")),
            ("X-WPTSALL-Operation-ID", field("X-WPTSALL-Operation-ID")),
            (
                "X-WPTSALL-Content-SHA256",
                field("X-WPTSALL-Content-SHA256"),
            ),
        ];
        let signed: Vec<_> = signed
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let target = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap();
        let canonical = crate::crypto::build_canonical_string(
            "POST",
            target.strip_prefix("/wp-json").unwrap_or(target),
            &field("X-WPTSALL-Timestamp"),
            &field("X-WPTSALL-Signature-Nonce"),
            &plaintext,
            &signed,
        );
        assert_eq!(
            field("X-WPTSALL-Signature"),
            crate::crypto::compute_request_signature(
                &crate::crypto::derive_signing_key("owned-token"),
                &canonical
            )
        );
        let response =
            crate::web_ui::test_support::media_operation_response_from_headers(&headers, 321)
                .to_string();
        let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
            "owned-token",
            response.as_bytes(),
        );
        let message = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nX-WPTSALL-Response-Signature: {signature}\r\nConnection: close\r\n\r\n{response}", response.len());
        reader
            .get_mut()
            .write_all(message.as_bytes())
            .await
            .unwrap();
    });
    let make_payload = || {
        serde_json::from_value::<TranslationCallbackPayload>(json!({
            "relation_id":7,"business_line":"content_translation","object_type":"post_type",
            "post_type":"post","object_id":42,"translated_fields":{},"translated_meta":{},
            "media_mappings":[{"source_id":9,"translated_ref":format!("file://{}",path.display())}],
            "client_task_id":"owned-paid","worker_id":"owned-worker","source_lang":"en",
            "target_lang":"zh","execution_time_ms":0
        }))
        .unwrap()
    };
    let client = Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    for _ in 0..2 {
        let mut payload = make_payload();
        upload_pending_media(
            &client,
            &base,
            "owned-token",
            "owned-worker",
            "owned-device",
            1,
            7,
            &mut payload,
            "/dev/null",
            Some("owned"),
        )
        .await
        .unwrap();
        assert_eq!(payload.media_mappings[0].attachment_id, Some(321));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}
