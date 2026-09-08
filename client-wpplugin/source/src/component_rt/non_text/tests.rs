use super::*;

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
        let headers = auth_headers("tok123", "worker-1");
        assert!(headers.contains_key("X-WPTSALL-Client-Token"));
        assert!(headers.contains_key("X-WPTSALL-Worker-ID"));
        assert!(headers.contains_key("X-WPTSALL-Protocol-Version"));
        assert_eq!(headers["X-WPTSALL-Protocol-Version"], "2");
    }

    #[test]
    fn add_signature_headers_sets_v2_signature_fields() {
        let mut headers = auth_headers("tok123", "worker-1");
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
