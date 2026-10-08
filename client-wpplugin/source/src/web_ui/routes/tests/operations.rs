use super::*;

// catalog: WEBUI-API-GET-api-logs
// catalog: WEBUI-API-GET-api-logs-recent
// catalog: WEBUI-API-POST-api-logs-recent
// catalog: WEBUI-API-POST-api-translations-batch-retry
// catalog: WEBUI-API-POST-api-translations-batch-delete
// catalog: WEBUI-API-PREFIX-api-discovery-tasks
// oracle: L2
// (handler-level deep tests: logs_recent clamp/filter/alias/paging/flush, batch retry+delete validation contracts, discovery-task update rejects; F-T1 annotation batch 2026-09-22)

#[tokio::test]
async fn logs_recent_clamps_limit_and_returns_tail_lines() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let log_path = format!("/tmp/test-web-ui-logs-{}.log", uuid::Uuid::new_v4());
    std::fs::write(&log_path, "line-1\nline-2\nline-3\n").unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_logs_recent(&mut socket, &state, br#"{"limit":5000}"#, &log_path)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(payload["data"]["limit"], json!(1000));
    assert_eq!(
        payload["data"]["lines"],
        json!(["line-1", "line-2", "line-3"])
    );

    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn logs_recent_filters_by_min_level() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let log_path = format!("/tmp/test-web-ui-logs-level-{}.log", uuid::Uuid::new_v4());
    let body = [
        r#"{"ts":1,"level":"info","event":"a"}"#,
        r#"{"ts":2,"level":"error","event":"b"}"#,
        r#"{"ts":3,"level":"debug","event":"c"}"#,
    ]
    .join("\n");
    std::fs::write(&log_path, body + "\n").unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_logs_recent(
        &mut socket,
        &state,
        br#"{"limit":100,"min_level":"error"}"#,
        &log_path,
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    let lines = payload["data"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].as_str().unwrap().contains("\"error\""));

    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn logs_recent_get_alias_returns_tail() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let log_path = format!("/tmp/test-web-ui-logs-get-{}.log", uuid::Uuid::new_v4());
    std::fs::write(&log_path, "g1\ng2\n").unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_logs_recent_get(&mut socket, &state, "limit=10", &log_path)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(payload["data"]["lines"], json!(["g1", "g2"]));

    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn logs_recent_pages_backward_with_before_ts_ms() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let log_path = format!("/tmp/test-web-ui-logs-cursor-{}.log", uuid::Uuid::new_v4());
    let body = [
        r#"{"ts_ms":1000,"level":"info","event":"a"}"#,
        r#"{"ts_ms":2000,"level":"info","event":"b"}"#,
        r#"{"ts_ms":3000,"level":"info","event":"c"}"#,
        r#"{"ts_ms":4000,"level":"info","event":"d"}"#,
    ]
    .join("\n");
    std::fs::write(&log_path, body + "\n").unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_logs_recent(
        &mut socket,
        &state,
        br#"{"limit":2,"before_ts_ms":4000}"#,
        &log_path,
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    let lines = payload["data"]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].as_str().unwrap().contains("\"ts_ms\":2000"));
    assert!(lines[1].as_str().unwrap().contains("\"ts_ms\":3000"));
    assert_eq!(payload["data"]["next_before_ts_ms"], json!(2000));
    assert_eq!(payload["data"]["has_more"], json!(true));

    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn translations_batch_retry_rejects_empty_ids() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_translations_batch_retry(&mut socket, &state, br#"{"ids":[]}"#)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(payload["error"]["code"], json!("EMPTY_IDS"));
}

#[tokio::test]
async fn translations_batch_delete_rejects_too_many_ids() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let ids: Vec<i64> = (0..101).collect();
    let body = serde_json::to_vec(&json!({ "ids": ids })).unwrap();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_translations_batch_delete(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(payload["error"]["code"], json!("TOO_MANY_IDS"));
}

#[tokio::test]
async fn discovery_task_update_rejects_invalid_id() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_discovery_task_update(&mut socket, &state, br#"{}"#, "abc")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(payload["error"]["code"], json!("INVALID_ID"));
}

#[tokio::test]
async fn discovery_task_update_rejects_invalid_json_body() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_discovery_task_update(&mut socket, &state, br#"{"enabled":"oops"}"#, "1")
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(payload["error"]["code"], json!("INVALID_BODY"));
}

// FL-1 (Wave-2): info-level audit events batch in the log writer until an
// 8 KB threshold (warn+ flush immediately). The /api/logs/recent read path
// must flush the writer BEFORE reading, or the Logs page's auto-refresh
// and every log-oracle window see stale data — the tail of a burst (e.g.
// the final job.finalized) sat invisible until the next event or shutdown.
#[tokio::test]
async fn logs_recent_flushes_buffered_info_events_before_reading() {
    // Serialize global log-state mutation with logging::tests (the same
    // shared lock) — this test flips LOG_ENABLED to route a real info
    // event through the writer.
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(true, "info");
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let log_path = format!("/tmp/test-web-ui-logs-fl1-{}.log", uuid::Uuid::new_v4());
    let _ = std::fs::remove_file(&log_path);

    // One small info event: far below the 8 KB threshold, so it sits in
    // the writer's buffer with no on-disk presence until a flush.
    crate::logging::log_event(
        &log_path,
        "info",
        "fl1.probe_event",
        serde_json::json!({ "k": "v" }),
    )
    .unwrap();
    let on_disk = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        !on_disk.contains("fl1.probe_event"),
        "precondition: the info event is still buffered, not on disk (got: {on_disk:?})"
    );

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_logs_recent(&mut socket, &state, br#"{"limit":10}"#, &log_path)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    let lines = payload["data"]["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.as_str().unwrap_or("").contains("fl1.probe_event")),
        "the read must surface the buffered info event (lines: {lines:?})"
    );

    let _ = std::fs::remove_file(&log_path);
}
