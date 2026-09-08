use super::*;

/// Local mode must report `runtime_mode: "local"` and must not serialize a
/// server URL, session metadata, or server components — even when legacy
/// state is present in the process.
#[tokio::test]
async fn status_local_mode_hides_server_fields_and_reports_runtime_mode() {
    let _env = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "false".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess-secret-value"));

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
    handle_get_status(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "status should succeed: {}",
        response
    );
    let (_, body) = response
        .split_once("\r\n\r\n")
        .expect("status response must have a body");
    let payload: serde_json::Value = serde_json::from_str(body).expect("valid json body");
    let data = &payload["data"];

    assert_eq!(
        data["runtime_mode"], "local",
        "default mode must report local"
    );
    for key in [
        "server_base",
        "logged_in",
        "session_token_prefix",
        "components",
        "legacy_control_plane",
    ] {
        assert!(
            data.get(key).is_none(),
            "local status must not expose {}: {}",
            key,
            data
        );
    }
}

/// Legacy mode keeps the server fields for the legacy lane and adds a
/// nested, explicitly named projection. Session material stays redacted.
#[tokio::test]
async fn status_legacy_mode_uses_nested_named_projection() {
    let _env = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess-secret-value"));

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
    handle_get_status(&mut socket, &state).await.unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "status should succeed: {}",
        response
    );
    let (_, body) = response
        .split_once("\r\n\r\n")
        .expect("status response must have a body");
    let payload: serde_json::Value = serde_json::from_str(body).expect("valid json body");
    let data = &payload["data"];

    assert_eq!(
        data["runtime_mode"], "legacy_server_control_plane",
        "opted-in mode must be labelled"
    );
    assert_eq!(data["server_base"], "http://127.0.0.1:8787");
    assert_eq!(data["logged_in"], true);

    let legacy = &data["legacy_control_plane"];
    assert_eq!(legacy["configured"], true);
    assert_eq!(legacy["logged_in"], true);
    let prefix = legacy["session_token_prefix"].as_str().unwrap_or_default();
    assert!(
        !prefix.is_empty(),
        "legacy projection should include a session prefix"
    );
    assert!(
        !prefix.contains("sess-secret-value"),
        "session token must be redacted to a prefix, got: {}",
        prefix
    );
}
