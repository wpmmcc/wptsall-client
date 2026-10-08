use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn physical_capacity_actual_paid_binary_uses_original_booking_and_replays_above_lower_limit()
{
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let data = root.path().to_path_buf();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                assert!(request.len() < 16 * 1024);
            }
            observed.fetch_add(1, Ordering::SeqCst);
            // A user lowers the policy after this exact owned paid submit.
            std::fs::write(
                data.join(".wptsall-storage-v1.json"),
                br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
            )
            .unwrap();
            let bytes = b"%PDF-owned-original-paid-result";
            let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=\"translated.pdf\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(bytes).await.unwrap();
        }
    });
    let template = serde_json::from_value::<ComponentTemplate>(json!({
        "id":"owned-physical-provider","name":"Owned","version":"1.0.0","type":"document_translation",
        "request":{"method":"POST","url":format!("http://{address}/owned-paid"),"body":{},
            "body_type":"json","response_type":"binary"},
        "response":{}
    }))
    .unwrap();
    let runtime = ComponentRuntime {
        template,
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: vec!["media_ref".into()],
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };
    let env = crate::db::async_jobs::AsyncJobEnv {
        db: std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        )),
        domain: "https://owned.invalid".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 42,
        field_name: "owned-document".into(),
        chunk_index: 0,
        lane: "non_text",
        source_snapshot: None,
        resume_binding: None,
    };
    let client = Client::builder().no_proxy().build().unwrap();
    // Database recovery is a separate, previously admitted booking. Only the
    // current paid result's booking may disappear on Ready completion.
    let recovery_booked_before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    let booking_dir = root.path().join(".wptsall-storage-bookings-v1");
    let recovery_files_before: Vec<_> = std::fs::read_dir(&booking_dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_os_string(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect();
    assert_eq!(recovery_files_before.len(), 1);
    let translate = |env| {
        translate_non_text_via_component_with_env(
            &client,
            &runtime,
            "",
            None,
            "",
            "document",
            "owned-document",
            "en",
            "zh",
            Some(env),
        )
    };
    let result = translate(env.clone()).await.unwrap();
    let path = std::path::Path::new(result.translated_ref.strip_prefix("file://").unwrap());
    let original = std::fs::read(path).unwrap();
    assert_eq!(
        crate::retained_assets::read(path, 1024).unwrap(),
        b"%PDF-owned-original-paid-result"
    );
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        recovery_booked_before
    );
    let recovery_files_after: Vec<_> = std::fs::read_dir(&booking_dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_os_string(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect();
    assert_eq!(recovery_files_after, recovery_files_before);
    let replay = translate(env.clone()).await.unwrap();
    assert_eq!(replay.translated_ref, result.translated_ref);
    assert_eq!(std::fs::read(path).unwrap(), original);
    let mut another = env;
    another.object_id += 1;
    assert!(translate(another).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn physical_capacity_actual_paid_async_receipt_survives_limit_drop_after_submit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let submits = std::sync::Arc::new(AtomicUsize::new(0));
    let polls = std::sync::Arc::new(AtomicUsize::new(0));
    let (submitted, polled) = (submits.clone(), polls.clone());
    let data = root.path().to_path_buf();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&chunk[..read]);
                assert!(request.len() < 65536);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let submit = request.starts_with(b"POST /submit ");
            let body = if submit {
                assert_eq!(submitted.fetch_add(1, Ordering::SeqCst), 0);
                std::fs::write(
                    data.join(".wptsall-storage-v1.json"),
                    br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
                )
                .unwrap();
                br#"{"job_id":"owned-paid-job"}"#.as_slice()
            } else {
                assert!(request.starts_with(b"GET /poll/owned-paid-job "));
                polled.fetch_add(1, Ordering::SeqCst);
                br#"{"status":"done","text":"owned paid result"}"#.as_slice()
            };
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
        }
    });
    let runtime = ComponentRuntime {
        template: serde_json::from_value(json!({
            "id":"owned-physical-async","name":"Owned","version":"1.0.0","type":"text_translation",
            "request":{"method":"POST","url":format!("http://{address}/submit"),"body":{}},
            "response":{"translated_text_path":"text"},
            "async_poll":{"job_id_path":"job_id",
                "request":{"method":"GET","url":format!("http://{address}/poll/{{{{computed.job_id}}}}"),"body_type":"none"},
                "status_path":"status","done_values":["done"],"failed_values":["failed"],
                "interval_seconds":1,"timeout_seconds":5,"result_text_path":"text"}
        })).unwrap(),
        auth_values: HashMap::new(),
        supported_business_lines: Vec::new(),
        language_map: HashMap::new(),
        supported_content_formats: Vec::new(),
        supported_formats: Vec::new(),
        key_pool: None,
        oauth_pool: None,
        oauth_manager: None,
        runtime_max_concurrent_requests: 0,
        runtime_min_interval_ms: 0,
        runtime_concurrency_sem: None,
        runtime_last_request_at: None,
        proxy_profile_id: None,
    };
    let env = crate::db::async_jobs::AsyncJobEnv {
        db: std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
        )),
        domain: "https://owned.invalid".into(),
        relation_id: 7,
        object_type: "post_type".into(),
        object_id: 42,
        field_name: "owned-text".into(),
        chunk_index: 0,
        lane: "text",
        source_snapshot: None,
        resume_binding: None,
    };
    let client = Client::builder().no_proxy().build().unwrap();
    let translate = |env| {
        translate_text_via_component_with_env(
            &client,
            &runtime,
            "owned original",
            "en",
            "zh",
            Some(env),
        )
    };
    let result = translate(env.clone()).await.unwrap();
    assert_eq!(result, "owned paid result");
    assert_eq!(translate(env.clone()).await.unwrap(), result);
    let mut another = env;
    another.object_id += 1;
    assert!(translate(another).await.is_err());
    assert_eq!(submits.load(Ordering::SeqCst), 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    server.abort();
    let _ = server.await;
}
