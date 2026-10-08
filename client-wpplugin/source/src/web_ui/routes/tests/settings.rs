use super::*;

// catalog: WEBUI-API-POST-api-log-settings
// catalog: WEBUI-API-POST-api-access-control
// oracle: L2
// (handler-level write validation: invalid level rejected, invalid ip rejected, restart-required reporting; F-T1 annotation batch 2026-09-22)

#[tokio::test]
async fn log_settings_update_rejects_invalid_level() {
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
    handle_log_settings_update(
        &mut socket,
        &state,
        br#"{"enabled":true,"level":"verbose"}"#,
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "invalid log level should be rejected as bad request: {}",
        response
    );
    assert_eq!(payload["error"]["code"], json!("INVALID_LEVEL"));
}

#[tokio::test]
async fn access_control_update_rejects_invalid_ip() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let access_control = AccessControl::new(false, &[]);

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
    handle_access_control_update(
        &mut socket,
        &state,
        &access_control,
        br#"{"external_access":true,"allowed_ips":["not-an-ip"]}"#,
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "invalid IP should be rejected as bad request: {}",
        response
    );
    assert_eq!(payload["error"]["code"], json!("INVALID_IP"));
}

#[test]
fn access_control_audit_detail_lists_the_actual_allowed_ips() {
    // 12号批 H 小项①: the audit event must carry the IP list itself —
    // the count alone cannot answer "which IP was granted access?".
    // Shape as AccessControl::get_settings() returns it (Vec<String>).
    let ips = vec!["10.0.0.8".to_string()];
    let detail = super::super::settings::access_control_audit_detail(true, &ips);
    assert_eq!(detail["external_access"], json!(true));
    assert_eq!(detail["allowed_ips_count"], json!(1));
    assert_eq!(detail["allowed_ips"], json!(["10.0.0.8"]));

    // Loopback-only close: the list still records what was persisted.
    let loopback = vec!["127.0.0.1".to_string()];
    let detail = super::super::settings::access_control_audit_detail(false, &loopback);
    assert_eq!(detail["external_access"], json!(false));
    assert_eq!(detail["allowed_ips"], json!(["127.0.0.1"]));
    assert_eq!(detail["allowed_ips_count"], json!(1));
}

#[tokio::test]
async fn access_control_update_reports_restart_required_when_external_mode_changes() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let access_control = AccessControl::new(false, &[]);

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
    handle_access_control_update(
        &mut socket,
        &state,
        &access_control,
        br#"{"external_access":true,"allowed_ips":["10.0.0.8"]}"#,
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let payload = parse_http_json_body(&response);
    let (external_access, allowed_ips) = access_control.get_settings().await;

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "valid access-control update should succeed: {}",
        response
    );
    assert_eq!(payload["data"]["restart_required"], json!(true));
    assert_eq!(payload["data"]["external_access"], json!(true));
    assert_eq!(payload["data"]["allowed_ips"], json!(["10.0.0.8"]));
    assert!(
        external_access,
        "access control should switch to external mode"
    );
    assert_eq!(allowed_ips, vec!["10.0.0.8".to_string()]);
}
