use super::*;

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
