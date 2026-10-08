use super::*;

#[test]
fn chunk_recovery_wp_completed_status_fields_keep_partition_contract() {
    let status: StatusResponse = serde_json::from_value(serde_json::json!({
        "received_chunks":[0], "total_chunks":1, "missing_chunks":[],
        "state":"result_ready", "attachment_id":321
    }))
    .unwrap();
    assert!(validate_chunk_status(&status, 1).unwrap());
    // Parsing the extra WP receipt fields is not automatic reconciliation.
}

#[tokio::test]
async fn chunk_recovery_read_offset_overflow_is_error_not_panic() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.bin");
    std::fs::write(&path, b"owned").unwrap();
    assert!(read_chunk(path.to_str().unwrap(), usize::MAX, 2)
        .await
        .is_err());
}

#[test]
fn chunk_recovery_status_requires_exact_partition() {
    for (received, total, missing) in [
        (vec![], 0, vec![]),
        (vec![0], 2, vec![]),
        (vec![0], 2, vec![0]),
        (vec![0], 2, vec![2]),
        (vec![], 2, vec![0, 0]),
        (vec![0, 1], 2, vec![1]),
    ] {
        assert!(validate_chunk_status(
            &StatusResponse {
                received_chunks: received,
                total_chunks: total,
                missing_chunks: missing
            },
            2
        )
        .is_err());
    }
    assert!(validate_chunk_status(
        &StatusResponse {
            received_chunks: vec![1, 0],
            total_chunks: 2,
            missing_chunks: vec![]
        },
        2
    )
    .unwrap());
    assert!(!validate_chunk_status(
        &StatusResponse {
            received_chunks: vec![0],
            total_chunks: 2,
            missing_chunks: vec![1]
        },
        2
    )
    .unwrap());
}

#[tokio::test]
async fn chunk_recovery_final_short_chunk_reads_all_remaining_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.bin");
    let size = 5 * 1024 * 1024;
    let bytes = vec![7u8; size + 123];
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        read_chunk(path.to_str().unwrap(), 0, size).await.unwrap(),
        bytes[..size]
    );
    assert_eq!(
        read_chunk(path.to_str().unwrap(), 1, size).await.unwrap(),
        vec![7; 123]
    );
    assert!(read_chunk(path.to_str().unwrap(), 0, 0).await.is_err());
}

async fn chunk_recovery_http_case(mode: &'static str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/wp-json/wptsall/v2/owned/client",
        listener.local_addr().unwrap()
    );
    let counts = Arc::new(Mutex::new((0usize, 0usize, 0usize, 0usize)));
    let observed = counts.clone();
    let server = tokio::spawn(async move {
        let mut operation = serde_json::Value::Null;
        let mut sha = serde_json::Value::Null;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (header_end, length) = loop {
                let mut buf = [0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                if let Some(p) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&request[..p]);
                    let len = header
                        .lines()
                        .find_map(|line| {
                            let (k, v) = line.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break (p + 4, len);
                }
            };
            while request.len() < header_end + length {
                let mut buf = [0u8; 65536];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
            }
            let mut plaintext = request[header_end..header_end + length].to_vec();
            if length > 0 {
                let envelope: Value = serde_json::from_slice(&plaintext).unwrap();
                if let Some(payload) = envelope["encrypted_payload"].as_str() {
                    plaintext = crate::crypto::transport_decrypt(
                        payload,
                        envelope["nonce"].as_str().unwrap(),
                        "owned-chunk-token",
                    )
                    .unwrap();
                }
            }
            let length = plaintext.len();
            let header = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            assert!(header.contains("x-wptsall-device-id: owned-device"));
            assert!(header.contains("x-wptsall-protocol-version: 2"));
            assert!(header.contains("x-wptsall-signature:"));
            let target = header
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let body = {
                let mut guard = observed.lock().unwrap();
                if target.ends_with("/init") {
                    let init: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
                    assert_eq!(init["total_size"], 50 * 1024 * 1024);
                    assert_eq!(init["chunk_count"], 10);
                    assert_eq!(init["source_id"], 777);
                    operation = init["operation_id"].clone();
                    sha = init["content_sha256"].clone();
                    let id = match mode {
                        "empty_session" => "",
                        "header_session" => "owned\r\nInjected: bad",
                        "query_session" => "owned-session&unexpected=x",
                        _ => "owned-session",
                    };
                    serde_json::json!({"success":true,"upload_id":id})
                } else if target.ends_with("/chunk") {
                    guard.0 += 1;
                    guard.1 += length;
                    assert_eq!(length, 5 * 1024 * 1024);
                    let index = header
                        .lines()
                        .find_map(|line| line.strip_prefix("x-wptsall-chunk-index: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    assert!(index < 10);
                    assert!(
                        plaintext.iter().all(|byte| *byte == (index + 1) as u8),
                        "chunk bytes must match the requested file offset"
                    );
                    serde_json::json!({"success":true})
                } else if target.contains("/status?") {
                    if mode == "query_session" {
                        assert!(
                            target.ends_with("?upload_id=owned-session%26unexpected%3dx"),
                            "opaque upload ID must not inject another query argument"
                        );
                    }
                    guard.2 += 1;
                    let (received, total, missing) = match mode {
                        "wrong_count" => ((0..10).collect::<Vec<_>>(), 9, vec![]),
                        "duplicate" => (vec![0; 10], 10, vec![]),
                        _ if guard.0 == 0 => (vec![], 10, (0..10).collect()),
                        "missing_once" if guard.2 == 2 => ((0..9).collect(), 10, vec![9]),
                        _ => ((0..10).collect(), 10, vec![]),
                    };
                    serde_json::json!({"success":true,"data":{
                        "operation_id":operation,"content_sha256":sha,
                        "source_id":777,"task_id":22,"relation_id":33,"state":"uploading",
                        "received_chunks":received,"total_chunks":total,"missing_chunks":missing}})
                } else {
                    assert!(target.ends_with("/complete"));
                    guard.3 += 1;
                    if mode == "unknown_complete" {
                        drop(guard);
                        continue;
                    }
                    serde_json::json!({"success":true,"attachment_id":if mode=="zero_id" {0} else {321},
                        "operation_id":operation,"content_sha256":sha,"source_id":777,"task_id":22,"relation_id":33})
                }
            };
            let bytes = serde_json::to_vec(&body).unwrap();
            let signature = crate::web_ui::test_support::sign_wp_plaintext_response(
                "owned-chunk-token",
                &bytes,
            );
            let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-WPTSALL-Response-Signature: {}\r\nConnection: close\r\n\r\n",bytes.len(),signature);
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&bytes).await.unwrap();
        }
    });
    let path = root.path().join("owned.bin");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&path).unwrap();
        for index in 0..10u8 {
            file.write_all(&vec![index + 1; 5 * 1024 * 1024]).unwrap();
        }
    }
    let config = ChunkedUploadConfig {
        wp_base: base,
        token: "owned-chunk-token".into(),
        route_secret: Some("owned".into()),
        worker_id: "owned-worker".into(),
        device_id: "owned-device".into(),
        chunk_size: 0,
    };
    let result = upload_file_to_wp(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &config,
        path.to_str().unwrap(),
        "owned.bin",
        "application/octet-stream",
        777,
        22,
        33,
    )
    .await;
    server.abort();
    let joined = server.await;
    assert!(
        joined.unwrap_err().is_cancelled(),
        "owned server must not panic"
    );
    let (chunks, bytes, statuses, completes) = *counts.lock().unwrap();
    assert!(path.exists(), "original source must remain retained");
    match mode {
        "empty_session" | "header_session" => {
            assert!(
                !result.success,
                "unusable session ID must stop before chunks"
            );
            assert_eq!((chunks, statuses, completes), (0, 0, 0));
        }
        "wrong_count" | "duplicate" => {
            assert!(
                !result.success,
                "inconsistent signed status must fail closed"
            );
            assert_eq!(
                completes, 0,
                "do not complete a different/incomplete session"
            );
        }
        "zero_id" | "unknown_complete" => {
            assert!(
                !result.success,
                "unknown/non-positive attachment is not success"
            );
            assert_eq!(completes, 1, "never blindly retry a completion");
        }
        _ => {
            assert!(result.success, "{}", result.error);
            assert_eq!(result.attachment_id, 321);
            assert_eq!(chunks, if mode == "missing_once" { 11 } else { 10 });
            assert_eq!(statuses, if mode == "missing_once" { 3 } else { 2 });
            assert_eq!(completes, 1);
        }
    }
    assert_eq!(bytes, chunks * 5 * 1024 * 1024);
}

#[tokio::test]
async fn chunk_recovery_wrong_count_stops_completion() {
    chunk_recovery_http_case("wrong_count").await;
}
#[tokio::test]
async fn chunk_recovery_duplicate_indices_stop_completion() {
    chunk_recovery_http_case("duplicate").await;
}
#[tokio::test]
async fn chunk_recovery_zero_attachment_is_not_success() {
    chunk_recovery_http_case("zero_id").await;
}
#[tokio::test]
async fn chunk_recovery_missing_chunk_reuploads_only_missing() {
    chunk_recovery_http_case("missing_once").await;
}
#[tokio::test]
async fn chunk_recovery_valid_50mib_upload_completes_once() {
    chunk_recovery_http_case("valid").await;
}
#[tokio::test]
async fn chunk_recovery_lost_complete_response_is_not_retried() {
    chunk_recovery_http_case("unknown_complete").await;
}
#[tokio::test]
async fn chunk_recovery_empty_session_stops_before_chunks() {
    chunk_recovery_http_case("empty_session").await;
}
#[tokio::test]
async fn chunk_recovery_bad_header_session_stops_before_chunks() {
    chunk_recovery_http_case("header_session").await;
}
#[tokio::test]
async fn chunk_recovery_opaque_session_id_is_query_encoded() {
    chunk_recovery_http_case("query_session").await;
}

#[tokio::test]
async fn placeholder_returns_not_implemented_for_all_types() {
    let executor = PlaceholderNonTextExecutor::new();

    let image_result = executor
        .execute_image(&ImageInput {
            source_ref: "https://example.com/img.jpg".to_string(),
            alt_text: "test alt".to_string(),
            caption: String::new(),
            title: String::new(),
            source_lang: "zh_CN".to_string(),
            target_lang: "en_US".to_string(),
            source_payload: None,
        })
        .await;
    assert!(!image_result.success);
    assert_eq!(image_result.error_code, "NOT_IMPLEMENTED");
    assert!(image_result.translated_ref.is_empty());

    let video_result = executor
        .execute_video(&VideoInput {
            source_ref: "https://example.com/video.mp4".to_string(),
            transcript: "hello world".to_string(),
            caption: String::new(),
            source_lang: "zh_CN".to_string(),
            target_lang: "en_US".to_string(),
            source_payload: None,
        })
        .await;
    assert!(!video_result.success);
    assert_eq!(video_result.error_code, "NOT_IMPLEMENTED");

    let audio_result = executor
        .execute_audio(&AudioInput {
            source_ref: "https://example.com/audio.mp3".to_string(),
            transcript: String::new(),
            caption: String::new(),
            source_lang: "zh_CN".to_string(),
            target_lang: "en_US".to_string(),
            source_payload: None,
        })
        .await;
    assert!(!audio_result.success);
    assert_eq!(audio_result.error_code, "NOT_IMPLEMENTED");

    let doc_result = executor
        .execute_document(&DocumentInput {
            source_ref: "https://example.com/doc.pdf".to_string(),
            text_content: "document content".to_string(),
            title: "Test Doc".to_string(),
            source_lang: "zh_CN".to_string(),
            target_lang: "en_US".to_string(),
            source_payload: None,
        })
        .await;
    assert!(!doc_result.success);
    assert_eq!(doc_result.error_code, "NOT_IMPLEMENTED");
}

#[tokio::test]
async fn dispatch_routes_to_correct_method() {
    let executor = PlaceholderNonTextExecutor::new();

    let result = dispatch_non_text(&executor, "image", "", "", "zh", "en", None).await;
    assert!(result.is_some());
    let r = result.unwrap();
    assert_eq!(r.error_code, "NOT_IMPLEMENTED");
    assert!(r.error_message.contains("image"));

    let result = dispatch_non_text(&executor, "video", "", "", "zh", "en", None).await;
    assert!(result.is_some());
    assert!(result.unwrap().error_message.contains("video"));

    let result = dispatch_non_text(&executor, "audio", "", "", "zh", "en", None).await;
    assert!(result.is_some());
    assert!(result.unwrap().error_message.contains("audio"));

    let result = dispatch_non_text(&executor, "document", "", "", "zh", "en", None).await;
    assert!(result.is_some());
    assert!(result.unwrap().error_message.contains("document"));

    // Unknown types return None
    let result = dispatch_non_text(&executor, "text", "", "", "zh", "en", None).await;
    assert!(result.is_none());

    let result = dispatch_non_text(&executor, "unknown", "", "", "zh", "en", None).await;
    assert!(result.is_none());
}

#[tokio::test]
async fn dispatch_extracts_payload_fields() {
    use serde_json::json;

    struct SpyExecutor;

    #[async_trait]
    impl NonTextExecutor for SpyExecutor {
        fn executor_id(&self) -> &str {
            "spy"
        }
        async fn execute_image(&self, input: &ImageInput) -> NonTextExecutionResult {
            // Verify payload fields were extracted
            assert_eq!(input.alt_text, "photo of a cat");
            assert_eq!(input.title, "Cat Photo");
            NonTextExecutionResult::not_implemented("image", "spy")
        }
    }

    let payload = json!({
        "alt_text": "photo of a cat",
        "title": "Cat Photo",
        "url": "https://example.com/cat.jpg"
    });

    let result = dispatch_non_text(
        &SpyExecutor,
        "image",
        "https://example.com/cat.jpg",
        "",
        "zh_CN",
        "en_US",
        Some(&payload),
    )
    .await;
    assert!(result.is_some());
}

mod upload_tests {
    use super::*;

    // ---- make_wp_url --------------------------------------------------------

    #[test]
    fn make_wp_url_appends_endpoint() {
        let base = "https://blog.example.com/wp-json/wptsall/v2/abc123/client";
        assert_eq!(
            make_wp_url(base, "media-upload/init"),
            "https://blog.example.com/wp-json/wptsall/v2/abc123/client/media-upload/init"
        );
    }

    #[test]
    fn make_wp_url_trims_trailing_slash() {
        let base = "https://blog.example.com/wp-json/wptsall/v2/abc123/client/";
        assert_eq!(
            make_wp_url(base, "media-upload/chunk"),
            "https://blog.example.com/wp-json/wptsall/v2/abc123/client/media-upload/chunk"
        );
    }

    // ---- ChunkedUploadConfig ------------------------------------------------

    #[test]
    fn effective_chunk_size_defaults_to_5mib() {
        let cfg = ChunkedUploadConfig {
            wp_base: String::new(),
            token: String::new(),
            route_secret: None,
            worker_id: String::new(),
            device_id: String::new(),
            chunk_size: 0,
        };
        assert_eq!(cfg.effective_chunk_size(), 5 * 1024 * 1024);
    }

    #[test]
    fn effective_chunk_size_uses_caller_value() {
        let cfg = ChunkedUploadConfig {
            wp_base: String::new(),
            token: String::new(),
            route_secret: None,
            worker_id: String::new(),
            device_id: String::new(),
            chunk_size: 1024,
        };
        assert_eq!(cfg.effective_chunk_size(), 1024);
    }

    // ---- read_chunk ---------------------------------------------------------

    #[tokio::test]
    async fn read_chunk_reads_correct_slice() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // Write 10 bytes: 0x00..0x09
        let data: Vec<u8> = (0u8..10).collect();
        tmp.write_all(&data).unwrap();
        tmp.flush().unwrap();

        // chunk_size=4 → chunk 0 = [0,1,2,3], chunk 1 = [4,5,6,7], chunk 2 = [8,9]
        let c0 = read_chunk(tmp.path().to_str().unwrap(), 0, 4)
            .await
            .unwrap();
        assert_eq!(c0, vec![0, 1, 2, 3]);

        let c1 = read_chunk(tmp.path().to_str().unwrap(), 1, 4)
            .await
            .unwrap();
        assert_eq!(c1, vec![4, 5, 6, 7]);

        let c2 = read_chunk(tmp.path().to_str().unwrap(), 2, 4)
            .await
            .unwrap();
        assert_eq!(c2, vec![8, 9]); // partial last chunk
    }

    // ---- upload_file_to_wp routing decision ---------------------------------

    #[tokio::test]
    async fn upload_file_to_wp_returns_error_for_missing_file() {
        let client = reqwest::Client::new();
        let cfg = ChunkedUploadConfig {
            wp_base: "http://127.0.0.1:1".to_string(), // unreachable
            token: "tok".to_string(),
            route_secret: None,
            worker_id: "w1".to_string(),
            device_id: "w1".to_string(),
            chunk_size: 0,
        };
        let result = upload_file_to_wp(
            &client,
            &cfg,
            "/tmp/this_file_does_not_exist_wptsall_test",
            "test.jpg",
            "image/jpeg",
            1,
            2,
            3,
        )
        .await;
        assert!(!result.success);
        assert!(result.error.contains("stat"));
    }

    // ---- auth_headers -------------------------------------------------------

    #[test]
    fn auth_headers_sets_required_fields() {
        let headers = auth_headers("tok123", "worker-1", "worker-1");
        assert!(headers.contains_key("X-WPTSALL-Client-Token"));
        assert!(headers.contains_key("X-WPTSALL-Device-Id"));
        assert!(headers.contains_key("X-WPTSALL-Worker-Id"));
        assert!(headers.contains_key("X-WPTSALL-Protocol-Version"));
        assert_eq!(headers["X-WPTSALL-Protocol-Version"], "2");
        assert_eq!(headers["X-WPTSALL-Device-Id"], "worker-1");
    }

    #[test]
    fn add_signature_headers_sets_v2_signature_fields() {
        let mut headers = auth_headers("tok123", "worker-1", "worker-1");
        add_signature_headers(
            &mut headers,
            "POST",
            "https://example.com/wp-json/wptsall/v2/abc/client/media-upload",
            "tok123",
            b"{\"ok\":true}",
            &[],
        );
        assert!(headers.contains_key("X-WPTSALL-Timestamp"));
        assert!(headers.contains_key("X-WPTSALL-Signature-Nonce"));
        assert!(headers.contains_key("X-WPTSALL-Signature"));
    }
}
