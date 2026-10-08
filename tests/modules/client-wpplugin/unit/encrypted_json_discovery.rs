//! Real discovery/provider writes, restricted to owned loopback and SQLite.
use super::*;

#[tokio::test]
async fn encrypted_json_discovery_actual_provider_saves_private_raw_and_result_for_review() {
    let _key = crate::db::owned_mock_bindings_key();
    let _transport = crate::db::TestEnvVarGuard::set("WPTSALL_WP_TRANSPORT_ENCRYPT", "off");
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let wp = format!("{base}/wp-json/wptsall/v2/owned/client");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 8192];
        let end = loop {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..end]).unwrap();
        assert!(headers
            .lines()
            .next()
            .unwrap()
            .starts_with("POST /provider "));
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        while request.len() < end + length {
            let n = socket.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
        }
        let value: serde_json::Value = serde_json::from_slice(&request[end..end + length]).unwrap();
        assert_eq!(value["text"], "owned-discovery-source-private-sentinel");
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let body = r#"{"text":"owned-discovery-result-private-sentinel"}"#;
        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let db = Arc::new(Mutex::new(crate::db::open_db(":memory:").unwrap()));
    let job = crate::db::jobs::create_job(
        &*db.lock().await,
        &crate::db::jobs::CreateJobRequest {
            domain: wp.clone(),
            relation_id: sample_relation().id as i64,
            business_line: "post_content".into(),
            triggered_by: "owned-json-discovery".into(),
        },
    )
    .unwrap();
    let mut registry = sample_registry();
    let component = format!("owned-json-discovery-{}", uuid::Uuid::new_v4());
    let mut runtime = registry.runtimes.remove("comp-selected").unwrap();
    runtime.template.id = component.clone();
    runtime.template.request.url = format!("{base}/provider");
    runtime.supported_business_lines = Vec::new();
    runtime.supported_content_formats = vec!["plain_text".into()];
    registry.runtimes.insert(component.clone(), runtime);
    let mut rule = sample_rule("post", "post");
    rule.translate_fields = vec!["post_title".into()];
    rule.field_content_formats
        .insert("post_title".into(), "plain_text".into());
    rule.field_storage_map
        .insert("post_title".into(), "post_column".into());
    let source = json!({"post_title":"owned-discovery-source-private-sentinel","__wptsall_job_snapshot":{
        "source_revision":"owned-discovery-revision","policy_version":"owned-policy",
    }});
    let item = ContentItem {
        object_type: "post_type".into(),
        subtype: "post".into(),
        object_id: 808,
        needs_resync: false,
        mapping_id: None,
        complete_data: source.clone(),
    };
    let mut worker = discovery_test_worker_config();
    worker.review_mode = true;
    let outcome = translate_content_item_owned(
        Client::builder().no_proxy().build().unwrap(),
        wp.clone(),
        "owned".into(),
        "/dev/null".into(),
        item,
        sample_relation(),
        Arc::new(vec![rule]),
        Some(Arc::new(registry)),
        None,
        component,
        vec![],
        None,
        None,
        worker,
        Some("owned".into()),
        Arc::new(Mutex::new(PendingCallbackStore::open(":memory:").unwrap())),
        Some(db.clone()),
        Arc::new(Semaphore::new(1)),
        None,
        None,
        Arc::new(AdaptiveRateControl::new(false, 0)),
        Some(job),
        DiscoveryTaskParams::default(),
    )
    .await
    .unwrap();
    assert!(!outcome, "a retained review is not callback completion");
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let conn = db.lock().await;
    let saved = crate::db::jobs::list_items_by_domain_object_checked(
        &conn,
        &wp,
        sample_relation().id as i64,
        808,
    )
    .unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].status, "pending_review");
    for (path, marker) in [
        (
            &saved[0].raw_path,
            "owned-discovery-source-private-sentinel",
        ),
        (
            &saved[0].translated_path,
            "owned-discovery-result-private-sentinel",
        ),
    ] {
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.starts_with(b"WPTC"));
        assert!(!bytes
            .windows(marker.len())
            .any(|window| window == marker.as_bytes()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
    let raw: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&saved[0].raw_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(raw, source);
    let result: serde_json::Value = serde_json::from_str(
        &crate::bindings::load_encrypted_or_plain(std::path::Path::new(&saved[0].translated_path))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        result["payload"]["translated_fields"]["post_title"],
        "owned-discovery-result-private-sentinel"
    );
    assert_eq!(result["payload"]["client_task_id"], saved[0].client_task_id);
}
