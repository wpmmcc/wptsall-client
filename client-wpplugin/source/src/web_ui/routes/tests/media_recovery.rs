use super::*;

fn seed_checkpoint() -> String {
    seed_checkpoint_for_site("https://owned.example/wp-json/wptsall/v2/private-route/client")
}

fn seed_checkpoint_for_site(site: &str) -> String {
    use crate::component_rt::non_text::{recovery::Checkpoint, recovery_store::RecoveryStore};
    let sha = "0".repeat(64);
    let key = crate::db::system::private_json_digest(&json!({
        "identity":{"site":site,"source":77,"task":22,"relation":33,"scope":"owned-api-unit"},
        "content":null,
    }))
    .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let row = Checkpoint {
        format: "media-session-v1".into(),
        key: key.clone(),
        operation_id: operation.clone(),
        wp_base: site.into(),
        device_id: "device-test".into(),
        source_id: 77,
        task_id: 22,
        relation_id: 33,
        scope: Some("owned-api-unit".into()),
        filename: "private-name".into(),
        content_type: "image/png".into(),
        sha256: sha,
        size: 1,
        chunk_size: 1,
        asset: "private-asset-path".into(),
        state: "complete_unknown".into(),
        upload_id: None,
        attachment_id: None,
    };
    RecoveryStore::configured()
        .unwrap()
        .update(&key, |_: Option<Checkpoint>| Ok((row, ())))
        .unwrap();
    operation
}

async fn invoke(body: Option<Value>, state: Arc<Mutex<WebUiState>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let (mut socket, _) = listener.accept().await.unwrap();
    if let Some(body) = body {
        super::super::media_recovery::reconcile(
            &mut socket,
            &state,
            &serde_json::to_vec(&body).unwrap(),
        )
        .await
        .unwrap();
    } else {
        super::super::media_recovery::list(&mut socket)
            .await
            .unwrap();
    }
    drop(socket);
    reader.await.unwrap()
}

#[tokio::test]
async fn media_recovery_api_list_redacts_route_and_private_asset_fields() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let operation = seed_checkpoint();
    let response = invoke(None, build_test_web_ui_state("", None)).await;
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(
        !response.contains("private-route")
            && !response.contains("private-asset-path")
            && !response.contains("private-name")
    );
    let value = parse_http_json_body(&response);
    assert_eq!(value["data"]["items"][0]["operation_id"], operation);
    assert_eq!(value["data"]["items"][0]["site"], "https://owned.example");
}

#[tokio::test]
async fn media_recovery_api_corrupt_checkpoint_is_not_empty_success() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let directory = root.path().join("media-recovery");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("owned.receipt"), b"{").unwrap();
    let response = invoke(None, build_test_web_ui_state("", None)).await;
    assert!(response.starts_with("HTTP/1.1 409 Conflict"));
    assert_eq!(
        parse_http_json_body(&response)["error"]["code"],
        "MEDIA_RECOVERY_UNREADABLE"
    );
}

#[tokio::test]
async fn media_recovery_api_changed_device_and_unbound_site_are_refused() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let operation = seed_checkpoint();
    for device in ["different-device", "device-test"] {
        let state = build_test_web_ui_state("", None);
        state.lock().await.device_id = device.into();
        let response = invoke(Some(json!({"operation_id":operation})), state).await;
        assert!(response.starts_with("HTTP/1.1 409 Conflict"));
        assert_eq!(
            parse_http_json_body(&response)["error"]["code"],
            "MEDIA_REVIEW_REQUIRED"
        );
    }
    assert_eq!(
        crate::component_rt::non_text::recovery::operations().unwrap()[0].state,
        "complete_unknown"
    );
}

#[tokio::test]
async fn media_recovery_router_applies_origin_host_and_external_token_guards() {
    use crate::web_ui::test_support::WebUiTestHarness;
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let operation = seed_checkpoint();
    let before = crate::component_rt::non_text::recovery::operations().unwrap()[0]
        .state
        .clone();
    let list = harness.get_json("/api/media-recovery").await.unwrap();
    assert!(list.status_line.starts_with("HTTP/1.1 200"));
    assert_eq!(list.body["data"]["items"][0]["operation_id"], operation);
    assert!(!list.raw_body.contains("private-route"));
    let body = serde_json::to_vec(&json!({"operation_id":operation})).unwrap();
    let (status, response) = harness
        .send_raw_request(
            "POST",
            "/api/media-recovery/reconcile",
            Some(&body),
            &[("Origin", "https://owned-attacker.invalid")],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 403") && response.contains("CSRF_REJECTED"));
    let (status, response) = harness
        .send_raw_request_without_host(
            "GET",
            "/api/media-recovery",
            None,
            &[("Host", "owned-attacker.invalid")],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 403") && response.contains("WEBUI_HOST_REJECTED"));
    let enabled = harness
        .post_json(
            "/api/access-control",
            json!({"external_access":true,"allowed_ips":["192.0.2.41"]}),
        )
        .await
        .unwrap();
    assert!(enabled.status_line.starts_with("HTTP/1.1 200"));
    let token = enabled.body["data"]["access_token"].as_str().unwrap();
    let denied = harness.get_json("/api/media-recovery").await.unwrap();
    assert!(denied.status_line.starts_with("HTTP/1.1 401"));
    let (status, response) = harness
        .send_raw_request(
            "POST",
            "/api/media-recovery/reconcile",
            Some(&body),
            &[("Origin", "http://127.0.0.1:8977")],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 401") && response.contains("WEBUI_TOKEN_REQUIRED"));
    let (status, _) = harness
        .send_raw_request(
            "GET",
            "/api/media-recovery",
            None,
            &[("X-WPTSALL-WebUI-Token", token)],
        )
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 200"));
    assert_eq!(
        crate::component_rt::non_text::recovery::operations().unwrap()[0].state,
        before
    );
}

#[tokio::test]
async fn media_recovery_router_commits_verified_result_with_current_binding() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let _signature = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "false");
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", upstream.local_addr().unwrap());
    let base = format!("{origin}/wp-json/wptsall/v2/owned/client");
    let operation = seed_checkpoint_for_site(&base);
    let expected = operation.clone();
    let server = tokio::spawn(async move {
        for stage in 0..3 {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (end, size) = loop {
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let size = String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("Content-Length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break (end + 4, size);
                }
            };
            while bytes.len() < end + size {
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            let header = String::from_utf8_lossy(&bytes[..end]);
            assert!(header
                .to_ascii_lowercase()
                .contains("x-wptsall-device-id: device-test"));
            let body = if stage == 1 {
                assert!(header.starts_with("POST ") && header.contains("/media-upload/reconcile "));
                let envelope: Value = serde_json::from_slice(&bytes[end..end + size]).unwrap();
                let plain = crate::crypto::transport_decrypt(
                    envelope["encrypted_payload"].as_str().unwrap(),
                    envelope["nonce"].as_str().unwrap(),
                    "owned-api-token",
                )
                .unwrap();
                let request: Value = serde_json::from_slice(&plain).unwrap();
                assert_eq!(
                    request,
                    json!({"operation_id":expected,"attachment_id":321})
                );
                json!({"success":true,"attachment_id":321})
            } else {
                assert!(header.starts_with("GET ") && header.contains("/media-upload/status?"));
                json!({"success":true,"data":{"state":if stage==0 {"completion_unknown"}else{"result_ready"},
                    "attachment_id":if stage==0 {0}else{321},"operation_id":expected,
                    "content_sha256":"0".repeat(64),"source_id":77,"task_id":22,"relation_id":33}})
            };
            let plain = serde_json::to_vec(&body).unwrap();
            let sig =
                crate::web_ui::test_support::sign_wp_plaintext_response("owned-api-token", &plain);
            let (payload, nonce) =
                crate::crypto::transport_encrypt(&plain, "owned-api-token").unwrap();
            let body = serde_json::to_vec(
                &json!({"encrypted_payload":payload,"nonce":nonce,"algorithm":"aes-256-gcm-v1"}),
            )
            .unwrap();
            let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-WPTSALL-Transport: encrypted\r\nX-WPTSALL-Response-Signature: {sig}\r\nConnection: close\r\n\r\n",body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let state = build_test_web_ui_state("", None);
    state.lock().await.domain_token_bindings.domains.insert(
        origin,
        DomainTokenBindingEntry {
            wp_client_token: "owned-api-token".into(),
            route_secret: "owned".into(),
            ..Default::default()
        },
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handler = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        handle_web_ui_connection(
            socket,
            state,
            WebUiRuntimeControl::new(),
            "/dev/null",
            0,
            std::time::Instant::now(),
            AccessControl::new(false, &[]),
        )
        .await
        .unwrap();
    });
    let body = serde_json::to_vec(&json!({"operation_id":operation,"attachment_id":321})).unwrap();
    let mut client = TcpStream::connect(address).await.unwrap();
    let request=format!("POST /api/media-recovery/reconcile HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://127.0.0.1:8977\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
    client.write_all(request.as_bytes()).await.unwrap();
    client.write_all(&body).await.unwrap();
    client.shutdown().await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    handler.await.unwrap();
    server.await.unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert_eq!(
        parse_http_json_body(&response)["data"]["attachment_id"],
        321
    );
    let row = crate::component_rt::non_text::recovery::operations()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(row.state, "result_ready");
    assert_eq!(row.attachment_id, Some(321));
}
