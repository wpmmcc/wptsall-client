use super::*;

#[tokio::test]
async fn physical_capacity_policy_dispatcher_blocks_cross_origin_and_shares_the_real_route() {
    let _scope = components_env_lock().lock().unwrap();
    let harness = crate::web_ui::test_support::WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let first = harness.get_json("/api/storage/capacity").await.unwrap();
    assert!(first.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(first.body["success"], true);
    let body = json!({"max_physical_bytes":4000,"expected_revision":null,"confirm_change":true});
    let (status, _) = harness
        .send_raw_request(
            "POST",
            "/api/storage/capacity",
            Some(&serde_json::to_vec(&body).unwrap()),
            &[
                ("Origin", "https://owned-cross-origin.invalid"),
                ("Content-Type", "application/json"),
            ],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 403"));
    assert!(!harness.data_dir.join(".wptsall-storage-v1.json").exists());
    let saved = harness
        .post_json("/api/storage/capacity", body)
        .await
        .unwrap();
    assert!(saved.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(saved.body["data"]["max_physical_bytes"], 4000);
    assert!(harness.data_dir.join(".wptsall-storage-v1.json").exists());
}

async fn request_capacity(body: Option<&[u8]>) -> (String, Value) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = TcpStream::connect(address).await.unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    match body {
        None => super::super::storage::get(&mut socket).await.unwrap(),
        Some(body) => super::super::storage::update(&mut socket, body)
            .await
            .unwrap(),
    }
    drop(socket);
    let response = reader.await.unwrap();
    let payload = response.split("\r\n\r\n").nth(1).unwrap();
    let value = serde_json::from_str(payload).unwrap();
    (response, value)
}

#[tokio::test]
async fn physical_capacity_policy_route_requires_exact_revision_and_confirmation() {
    let _scope = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _root = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    std::fs::write(root.path().join("owned-existing.bin"), b"retained").unwrap();
    let (_, first) = request_capacity(None).await;
    assert_eq!(first["success"], true);
    assert_eq!(first["data"]["physical_retained_bytes"], 8);
    assert_eq!(first["data"]["revision"], Value::Null);
    let body = json!({"max_physical_bytes":4000,"expected_revision":null,"confirm_change":true});
    let (http, changed) = request_capacity(Some(&serde_json::to_vec(&body).unwrap())).await;
    assert!(http.starts_with("HTTP/1.1 200"));
    assert_eq!(changed["success"], true);
    let revision = changed["data"]["revision"].as_str().unwrap();
    assert_eq!(revision.len(), 64);
    let original = std::fs::read(root.path().join(".wptsall-storage-v1.json")).unwrap();
    for invalid in [
        body,
        json!({"max_physical_bytes":5000,"expected_revision":revision,"confirm_change":false}),
        json!({"max_physical_bytes":0,"expected_revision":revision,"confirm_change":true}),
        json!({"max_physical_bytes":1.5,"expected_revision":revision,"confirm_change":true}),
        json!({"max_physical_bytes":9007199254740992u64,"expected_revision":revision,"confirm_change":true}),
        json!({"max_physical_bytes":5000,"expected_revision":revision,"confirm_change":true,"unknown":1}),
    ] {
        let (http, result) = request_capacity(Some(&serde_json::to_vec(&invalid).unwrap())).await;
        assert!(!http.starts_with("HTTP/1.1 200"));
        assert_eq!(result["success"], false);
        assert_eq!(
            std::fs::read(root.path().join(".wptsall-storage-v1.json")).unwrap(),
            original
        );
    }
    let (http, lowered) = request_capacity(Some(
        &serde_json::to_vec(&json!({
            "max_physical_bytes":1,"expected_revision":revision,"confirm_change":true
        }))
        .unwrap(),
    ))
    .await;
    assert!(http.starts_with("HTTP/1.1 200"));
    assert_eq!(lowered["data"]["max_physical_bytes"], 1);
    assert_eq!(
        std::fs::read(root.path().join("owned-existing.bin")).unwrap(),
        b"retained"
    );
}

#[tokio::test]
async fn physical_capacity_policy_route_refuses_damaged_inventory_without_path_disclosure() {
    let _scope = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _root = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let policy = root.path().join(".wptsall-storage-v1.json");
    std::fs::write(&policy, b"{").unwrap();
    let (http, result) = request_capacity(None).await;
    assert!(http.starts_with("HTTP/1.1 409"));
    assert_eq!(result["error"]["code"], "STORAGE_CAPACITY_INVALID");
    assert!(result.get("data").is_none());
    assert!(!http.contains(root.path().to_str().unwrap()));
    let (http, _) = request_capacity(Some(
        br#"{"max_physical_bytes":4000,"confirm_change":true,"expected_revision":null}"#,
    ))
    .await;
    assert!(http.starts_with("HTTP/1.1 409"));
    assert_eq!(std::fs::read(policy).unwrap(), b"{");
}
