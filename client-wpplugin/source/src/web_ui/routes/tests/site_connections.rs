use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn site_connection_import_claims_pairing_code_and_saves_local_binding() {
    let state = build_test_web_ui_state("https://www.wpmm.cc", None);
    let temp_path = format!(
        "/tmp/test-site-connection-domain-token-bindings-{}.json",
        uuid::Uuid::new_v4()
    );
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings_path = temp_path.clone();
    }
    {
        let db_arc = {
            let guard = state.lock().await;
            std::sync::Arc::clone(&guard.db)
        };
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
        mark_json_migration_done(&conn);
    }

    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    let upstream_handle = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .or_else(|| line.strip_prefix("Content-Length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + content_len {
                    break;
                }
            }
        }
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text
            .contains("POST /wp-json/wptsall/v2/secret-client/client/pairing/claim HTTP/1.1"));
        assert!(request_text.contains("PAIR-123"));
        assert!(request_text.contains("device-test"));
        let body = serde_json::to_vec(&json!({
            "success": true,
            "data": {
                "device_id": "device-test",
                "client_token": "claimed-token-123",
                "expires_at": 1999999999,
                "scopes": ["translate.read", "translate.write_callback"]
            }
        }))
        .unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
    });

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
    let wp_client_base = format!(
        "http://{}/wp-json/wptsall/v2/secret-client/client",
        upstream_addr
    );
    let body = serde_json::to_vec(&json!({
        "site_connection_pack": {
            "schema": "wptsall-site-connection.v1",
            "site_url": format!("http://{}", upstream_addr),
            "wp_client_base": wp_client_base,
            "route_secret": "secret-client",
            "device_id": "device-test",
            "pairing_code": "PAIR-123"
        }
    }))
    .unwrap();
    handle_site_connections_import(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "expected success response, got: {}",
        response
    );
    assert_eq!(payload["data"]["token_len"], json!(17));
    assert_eq!(payload["data"]["pairing_claimed"], json!(true));

    let guard = state.lock().await;
    let domain_key = format!("http://{}", upstream_addr);
    let binding = guard
        .domain_token_bindings
        .domains
        .get(&domain_key)
        .expect("binding should be saved");
    assert_eq!(binding.wp_client_token, "claimed-token-123");
    assert_eq!(binding.route_secret, "secret-client");
    assert_eq!(guard.domains.len(), 1);
    assert_eq!(guard.domains[0].api_base_url, domain_key);
    assert_eq!(guard.last_event, "site_connections.imported");

    upstream_handle.await.unwrap();
    let _ = std::fs::remove_file(&temp_path);
}

#[tokio::test]
async fn site_connection_import_rejects_mismatched_device_id_before_claim() {
    let state = build_test_web_ui_state("https://www.wpmm.cc", None);
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
    let body = serde_json::to_vec(&json!({
        "site_connection_pack": {
            "schema": "wptsall-site-connection.v1",
            "site_url": "https://example.test",
            "wp_client_base": "https://example.test/wp-json/wptsall/v2/secret/client",
            "device_id": "another-device",
            "pairing_code": "PAIR-123"
        }
    }))
    .unwrap();
    handle_site_connections_import(&mut socket, &state, &body)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(response.starts_with("HTTP/1.1 409 Conflict"));
    assert_eq!(payload["error"]["code"], json!("PAIRING_DEVICE_MISMATCH"));
}
