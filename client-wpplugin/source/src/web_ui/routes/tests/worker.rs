use super::*;

mod physical_worker_config_scope {
    use super::*;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/modules/client-wpplugin/unit/physical_worker_config_scope.rs"
    ));
}

#[tokio::test]
async fn runtime_configuration_authority_worker_preflight_never_degrades_damage_to_permission() {
    let _scope = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("local.json");
    let keys = root.path().join("keys.json");
    let oauth = root.path().join("oauth.json");
    let db = root.path().join("owned.sqlite");
    let _guards = [
        EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string()),
        EnvVarGuard::set("WPTSALL_DB_PATH", db.display().to_string()),
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", local.display().to_string()),
        EnvVarGuard::set("WPTSALL_VENDOR_KEYS_FILE", keys.display().to_string()),
        EnvVarGuard::set("WPTSALL_VENDOR_OAUTH_FILE", oauth.display().to_string()),
        EnvVarGuard::set("WPTSALL_WEB_UI", "1"),
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0"),
    ];
    let doc: ComponentsLocalDoc = serde_json::from_value(json!({"version":1,"components":{
        "owned-inline":{"name":"Owned","template_id":"","kind":"text","enabled":true,"created_at":"1","versions":{},
            "template_json":{"id":"owned-inline","name":"Owned","version":"1.0.0","type":"text","auth":null,
                "request":{"method":"POST","url":"http://127.0.0.1:1/translate","body":{}},
                "response":{"translated_text_path":"data.text"}}}
    }})).unwrap();
    crate::bindings::save_components_local(local.to_str().unwrap(), &doc).unwrap();
    crate::bindings::save_vendor_keys(keys.to_str().unwrap(), &VendorKeysDoc::default()).unwrap();
    crate::bindings::save_vendor_oauth(oauth.to_str().unwrap(), &VendorOAuthDoc::default())
        .unwrap();
    let conn = crate::db::open_db(db.to_str().unwrap()).unwrap();
    mark_json_migration_done(&conn);
    crate::db::components::save_local_components_doc(&conn, &doc).unwrap();
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    state.lock().await.db = Arc::new(Mutex::new(conn));
    let good = crate::web_ui::routes::worker::collect_worker_start_preflight(&state, "/dev/null")
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(good).unwrap()["can_start"], true);
    for path in [&keys, &oauth] {
        let original = std::fs::read(path).unwrap();
        std::fs::write(path, b"{").unwrap();
        let error =
            crate::web_ui::routes::worker::collect_worker_start_preflight(&state, "/dev/null")
                .await
                .err()
                .expect("damaged credentials cannot give allow-start with confirmation");
        assert!(error.is::<crate::component_rt::loader::RuntimeConfigurationFault>());
        assert!(!state.lock().await.worker_loop_running);
        std::fs::write(path, original).unwrap();
    }
    state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute(
            "UPDATE system_config SET value='{' WHERE key='local_components_doc'",
            [],
        )
        .unwrap();
    let error = crate::web_ui::routes::worker::collect_worker_start_preflight(&state, "/dev/null")
        .await
        .err()
        .unwrap();
    assert!(error.is::<crate::component_rt::loader::RuntimeConfigurationFault>());
    assert!(!state.lock().await.worker_loop_running);
}

// catalog: WEBUI-API-GET-api-worker-config
// catalog: WEBUI-API-POST-api-worker-config
// catalog: WEBUI-API-POST-api-worker-run-once
// catalog: WEBUI-API-POST-api-worker-start-check
// catalog: WEBUI-API-POST-api-worker-stop
// catalog: WEBUI-MOD-web-ui-routes-worker-rs
// catalog: WEBUI-MOD-types-runtime-rs
// catalog: WEBUI-MOD-task-engine-workflow-policy-rs
// catalog: WEBUI-MOD-task-engine-workflow-dsl-rs
// oracle: L2
// (handler-level deep tests on real sockets: config partial-update/policy writes, run-once structured error preservation, start-check bounds; F-T1 annotation batch 2026-09-22)

#[tokio::test]
async fn worker_run_once_preserves_structured_server_auth_errors() {
    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).await;
        let body =
            r#"{"success":false,"error":{"code":"SESSION_EXPIRED","message":"Session expired"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let state = build_test_web_ui_state(&format!("http://{}", upstream_addr), Some("sess_test"));
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            format!("http://{}", upstream_addr),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: "route_test".to_string(),
                ..Default::default()
            },
        );
    }

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
    handle_worker_run_once(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-run-once-auth-error-test.log",
        b"{}",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "worker run-once should preserve upstream auth status instead of rewriting it as WORKER_RUN_FAILED: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_EXPIRED\""),
        "worker run-once should preserve upstream auth error code: {}",
        response
    );
}

#[tokio::test]
async fn worker_start_check_allows_default_local_mode_without_server_session() {
    // Other worker tests explicitly exercise the legacy control-plane lane;
    // keep this local-first assertion isolated from process-global env state.
    let _scope = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let _db = EnvVarGuard::set(
        "WPTSALL_DB_PATH",
        root.path().join("owned.sqlite").display().to_string(),
    );
    let _components = EnvVarGuard::set(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        root.path().join("local.json").display().to_string(),
    );
    let _keys = EnvVarGuard::set(
        "WPTSALL_VENDOR_KEYS_FILE",
        root.path().join("keys.json").display().to_string(),
    );
    let _oauth = EnvVarGuard::set(
        "WPTSALL_VENDOR_OAUTH_FILE",
        root.path().join("oauth.json").display().to_string(),
    );
    let _local_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "false".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);
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
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-local-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "default local worker preflight should not require server session: {}",
        response
    );
    assert!(
        response.contains("\"can_start\":true"),
        "default local worker preflight should allow start: {}",
        response
    );
}

#[tokio::test]
async fn worker_start_check_requires_confirmation_when_all_domains_skipped() {
    // §63 (tasks/cursor feedback): a start-check whose every configured
    // domain is uncheckable (here: a binding with a token but no route
    // secret, so no WP base can be built) must not hand back a bare green
    // light — can_start stays true (nothing blocking), but the response
    // must require confirmation and surface domains_skipped so the vacuous
    // green over 0-checked/0-missing is no longer silent.
    let _env_scope = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _data_guard = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let db_path = root.path().join("preflight.db").display().to_string();
    let components_path = root.path().join("components.json").display().to_string();
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());
    let _local_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "false".to_string());
    let components: ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "owned-preflight-inline": {
                "name": "Owned preflight", "template_id": "", "vendor_id": "owned",
                "kind": "text", "enabled": true, "created_at": "1", "versions": {},
                "template_json": {
                    "id": "owned-preflight-inline", "name": "Owned preflight",
                    "version": "1.0.0", "type": "text", "auth": null,
                    "request": {"method": "POST", "url": "http://127.0.0.1:1/translate",
                        "body": {"text": "{{input.text}}"}},
                    "response": {"translated_text_path": "data.text"}
                }
            }
        }
    }))
    .unwrap();
    let conn = crate::db::open_db(&db_path).unwrap();
    mark_json_migration_done(&conn);
    crate::db::components::save_local_components_doc(&conn, &components).unwrap();
    crate::bindings::save_components_local(&components_path, &components).unwrap();
    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);
    {
        let mut guard = state.lock().await;
        guard.component_bindings_path = root
            .path()
            .join("component-bindings.json")
            .display()
            .to_string();
        guard.domain_token_bindings.domains.insert(
            "https://unresolvable.example.com".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: String::new(),
                ..Default::default()
            },
        );
    }
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
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-skipped-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "all-skipped start-check should still respond 200: {}",
        response
    );
    assert!(
        response.contains("\"can_start\":true"),
        "all-skipped start-check keeps can_start (nothing blocking): {}",
        response
    );
    assert!(
        response.contains("\"requires_confirmation\":true"),
        "all-skipped start-check must require confirmation instead of a silent green light: {}",
        response
    );
    assert!(
        response.contains("\"domains_skipped\":1"),
        "all-skipped start-check must surface domains_skipped: {}",
        response
    );
    assert!(
        response.contains("\"domains_checked\":0"),
        "all-skipped start-check must keep domains_checked honest at 0: {}",
        response
    );
}

#[tokio::test]
async fn worker_start_check_requires_confirmation_when_component_registry_unavailable() {
    // §64 recheck (tasks/cursor): when the component registry cannot load at
    // all (here: a bound component that needs a template download from an
    // unreachable catalog server), the old early return handed back a bare
    // green light — can_start:true with every counter at 0. The degraded
    // state must surface component_registry_unavailable and require
    // confirmation instead of the vacuous green.
    let _scope = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let _db = EnvVarGuard::set(
        "WPTSALL_DB_PATH",
        root.path().join("owned.sqlite").display().to_string(),
    );
    let _local_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "false".to_string());
    let _components_local_guard = EnvVarGuard::set(
        "WPTSALL_COMPONENTS_LOCAL_FILE",
        root.path().join("local.json").display().to_string(),
    );
    let _timeout_guard =
        EnvVarGuard::set("WPTSALL_PREFLIGHT_COMPONENT_TIMEOUT_SECS", "2".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    {
        let mut guard = state.lock().await;
        guard.task_type_component_bindings.task_types.insert(
            "translation".to_string(),
            TaskTypeComponentBindingEntry {
                component_id: "comp-registry-unavailable-test".to_string(),
            },
        );
    }
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
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-registry-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "registry-unavailable start-check should still respond 200: {}",
        response
    );
    assert!(
        response.contains("\"can_start\":true"),
        "registry-unavailable start-check keeps can_start (nothing blocking): {}",
        response
    );
    assert!(
        response.contains("\"requires_confirmation\":true"),
        "registry-unavailable start-check must require confirmation instead of a silent green light: {}",
        response
    );
    assert!(
        response.contains("\"component_registry_unavailable\":true"),
        "registry-unavailable start-check must surface the degraded flag: {}",
        response
    );
    assert!(
        response.contains("\"domains_checked\":0"),
        "registry-unavailable start-check keeps domains_checked honest at 0: {}",
        response
    );
}

#[tokio::test]
async fn worker_start_requires_server_session() {
    let _legacy_control_plane_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:8787", None);
    {
        let mut guard = state.lock().await;
        guard.domain_token_bindings.domains.insert(
            "https://example.com".to_string(),
            DomainTokenBindingEntry {
                wp_client_token: "wp_token_test".to_string(),
                route_secret: "route_test".to_string(),
                ..Default::default()
            },
        );
    }
    let runtime_control = WebUiRuntimeControl::new();

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
    handle_worker_start(
        &mut socket,
        &state,
        &runtime_control,
        "/tmp/wptsall-worker-start-test.log",
        b"{}",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 401 Unauthorized"),
        "worker start should reject missing server session before spawning loop: {}",
        response
    );
    assert!(
        response.contains("\"SESSION_REQUIRED\""),
        "worker start should report explicit session requirement: {}",
        response
    );
    assert!(
        !runtime_control.worker_running.load(Ordering::SeqCst),
        "worker loop must remain stopped when start is rejected"
    );
}

#[tokio::test]
async fn worker_config_partial_update_preserves_existing_poll_seconds() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let db_arc = {
        let mut guard = state.lock().await;
        guard.worker_loop_poll_seconds = 45;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::system::set_system_config(&conn, "worker_poll_seconds", "45").unwrap();
    }

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
    handle_worker_config(&mut socket, &state, br#"{"review_mode":true}"#)
        .await
        .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    let body = parse_http_json_body(&response);
    assert_eq!(body["success"], serde_json::json!(true));
    assert_eq!(body["data"]["poll_seconds"], serde_json::json!(45));

    {
        let guard = state.lock().await;
        assert_eq!(
            guard.worker_loop_poll_seconds, 45,
            "partial worker config updates must not reset poll_seconds"
        );
    }

    let conn = db_arc.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "worker_poll_seconds"),
        Some("45".to_string())
    );
    assert_eq!(
        crate::db::system::get_system_config(&conn, "review_mode"),
        Some("true".to_string())
    );
}

// ---- FL-8: workflow policy single source of truth (2026-09-18) ----
//
// The engine resolves items with the stored `workflow_policy` JSON when
// present. The legacy `review_mode` flag and the stored policy used to be
// written independently (Settings UI save always persisted a policy JSON;
// the API toggle only touched the flag), so "review off" via API could
// return success while items still parked for review.

async fn spawn_config_reader() -> (TcpStream, tokio::task::JoinHandle<String>) {
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
    let (socket, _) = downstream.accept().await.unwrap();
    (socket, reader)
}

#[tokio::test]
async fn worker_config_get_reports_effective_review_mode_over_stale_legacy_flag() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
        // The exact FL-8 skew: a persisted review policy with the legacy
        // flag toggled off afterwards.
        crate::db::system::set_system_config(&conn, "review_mode", "false").unwrap();
        crate::db::system::set_system_config(
            &conn,
            "workflow_policy",
            r#"{"schema_version":"workflow-policy-v1","default_mode":"review"}"#,
        )
        .unwrap();
    }

    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config_get(&mut socket, &state).await.unwrap();
    drop(socket);
    let response = reader.await.unwrap();

    let body = parse_http_json_body(&response);
    assert_eq!(
        body["data"]["review_mode"],
        serde_json::json!(true),
        "GET must report the effective mode (stored policy default wins), not the stale legacy flag: {}",
        response
    );
}

#[tokio::test]
async fn worker_config_flag_write_resyncs_stored_workflow_policy() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::system::set_system_config(
            &conn,
            "workflow_policy",
            r#"{"schema_version":"workflow-policy-v1","default_mode":"review","by_domain":{"shop.example.com":"auto"}}"#,
        )
        .unwrap();
    }

    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(&mut socket, &state, br#"{"review_mode":false}"#)
        .await
        .unwrap();
    drop(socket);
    let response = reader.await.unwrap();

    let body = parse_http_json_body(&response);
    assert_eq!(body["success"], serde_json::json!(true));

    let conn = db_arc.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "review_mode"),
        Some("false".to_string())
    );
    let raw = crate::db::system::get_system_config(&conn, "workflow_policy")
        .expect("policy row should survive a flag write");
    let policy = crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw)
        .expect("synced policy must stay parseable");
    assert_eq!(
        policy.default_mode, "auto",
        "flag-only write must resync the stored policy default_mode (FL-8)"
    );
    assert_eq!(
        policy.by_domain.get("shop.example.com").map(String::as_str),
        Some("auto"),
        "finer-grained overrides must survive the flag write"
    );
    // The loader the engine uses now resolves auto.
    assert_eq!(
        crate::task_engine::workflow_policy::load_workflow_policy(&conn, false)
            .resolve(None, None, None),
        crate::task_engine::workflow_policy::WorkflowMode::Auto
    );
}

#[tokio::test]
async fn worker_config_policy_write_mirrors_legacy_flag() {
    let state = build_test_web_ui_state("http://127.0.0.1:8787", Some("sess_test"));
    let db_arc = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db_arc.lock().await;
        crate::db::schema::create_tables(&conn).unwrap();
    }

    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(
        &mut socket,
        &state,
        br#"{"workflow_policy":{"schema_version":"workflow-policy-v1","default_mode":"review"}}"#,
    )
    .await
    .unwrap();
    drop(socket);
    let response = reader.await.unwrap();

    let body = parse_http_json_body(&response);
    assert_eq!(body["success"], serde_json::json!(true));

    let conn = db_arc.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "review_mode"),
        Some("true".to_string()),
        "policy write must mirror its default_mode into the legacy flag so both read paths agree"
    );
    assert_eq!(
        crate::db::system::get_system_config(&conn, "workflow_policy")
            .and_then(|raw| {
                crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw)
                    .map(|p| p.default_mode)
            })
            .as_deref(),
        Some("review")
    );
}

#[tokio::test]
async fn worker_config_rejects_invalid_workflows_without_writing_other_fields() {
    let bad_workflows = [
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v0", "default_mode": "auto" } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "not-a-mode" } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "auto", "by_domain": { "site.test": "not-a-mode" } } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "auto", "by_content_format": { "html": "not-a-mode" } } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "auto", "by_rule": { "7": "not-a-mode" } } }),
        json!({ "workflow_policy": "not-an-object" }),
        json!({ "workflow_policy": ["workflow-policy-v1", "auto", {}, {}, {}] }),
        json!({ "workflow_policy": { "schema_version": "", "default_mode": "auto" } }),
        json!({ "workflow_policy": { "default_mode": "auto" } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1" } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "AUTO" } }),
        json!({ "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "auto", "unknown": true } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v0", "steps": [{ "id": "sync", "type": "builtin" }] } }),
        json!({ "workflow_dsl": ["workflow-dsl-v1", [{ "id": "sync", "type": "builtin" }]] }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [["sync", "builtin", null]] } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [] } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [{ "id": "sync", "type": "not-a-step" }] } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [{ "id": "", "type": "builtin" }] } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [{ "id": " ", "type": "builtin" }] } }),
        json!({ "workflow_dsl": { "steps": [{ "id": "sync", "type": "builtin" }] } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1" } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [{ "id": "sync", "type": "builtin" }], "unknown": true } }),
        json!({ "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [{ "id": "sync", "type": "builtin", "unknown": true }] } }),
    ];
    for mut body in bad_workflows {
        let state = build_test_web_ui_state("http://127.0.0.1:1", None);
        body["poll_seconds"] = json!(99);
        body["auto_start_worker"] = json!(true);
        let (mut socket, reader) = spawn_config_reader().await;
        handle_worker_config(&mut socket, &state, &serde_json::to_vec(&body).unwrap())
            .await
            .unwrap();
        drop(socket);
        let response = reader.await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 400 "),
            "invalid workflow must be rejected, not silently ignored: {body} => {response}"
        );
        assert_eq!(parse_http_json_body(&response)["success"], json!(false));
        let guard = state.lock().await;
        assert_eq!(guard.worker_loop_poll_seconds, 20);
        assert_eq!(guard.last_event, "test.ready");
        let conn = guard.db.lock().await;
        assert_eq!(
            crate::db::system::get_system_config(&conn, "auto_start_worker"),
            None,
            "validation must happen before the first write"
        );
    }
}

#[tokio::test]
async fn worker_config_rejects_malformed_json_and_invalid_scalar_types() {
    for body in [
        "{",
        "[]",
        "[99,true,null,null,null,null,null,null,null,null,null,null,null,null,null,null,null]",
        r#"{"auto_start_worker":"true","poll_seconds":99}"#,
        r#"{"callback_retry_max":-1,"poll_seconds":99}"#,
        r#"{"callback_retry_max":1.5,"poll_seconds":99}"#,
        r#"{"poll_seconds":"99"}"#,
    ] {
        let state = build_test_web_ui_state("http://127.0.0.1:1", None);
        let (mut socket, reader) = spawn_config_reader().await;
        handle_worker_config(&mut socket, &state, body.as_bytes())
            .await
            .unwrap();
        drop(socket);
        let response = reader.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 400 "), "{body}: {response}");
        let error = parse_http_json_body(&response);
        assert_eq!(error["error"]["code"], json!("INVALID_WORKER_CONFIG"));
        let guard = state.lock().await;
        assert_eq!(guard.worker_loop_poll_seconds, 20);
        assert_eq!(guard.last_event, "test.ready");
        let conn = guard.db.lock().await;
        assert_eq!(
            crate::db::system::get_system_config(&conn, "worker_poll_seconds"),
            None
        );
    }
}

#[tokio::test]
async fn worker_config_get_reports_storage_failure_instead_of_defaults() {
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    {
        let guard = state.lock().await;
        let conn = guard.db.lock().await;
        conn.execute_batch("DROP TABLE system_config").unwrap();
    }
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config_get(&mut socket, &state).await.unwrap();
    drop(socket);
    let response = reader.await.unwrap();
    assert!(
        response.starts_with("HTTP/1.1 500 "),
        "readback must not report default values as a successful read: {response}"
    );
    let body = parse_http_json_body(&response);
    assert_eq!(body["success"], json!(false));
    assert_eq!(body["error"]["code"], json!("WORKER_CONFIG_READ_FAILED"));
}

#[tokio::test]
async fn worker_config_flag_sync_failure_rolls_back_the_whole_save() {
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let db_arc = std::sync::Arc::clone(&state.lock().await.db);
    let stored =
        r#"{"schema_version":"workflow-policy-v1","default_mode":"auto","by_rule":{"7":"review"}}"#;
    {
        let conn = db_arc.lock().await;
        crate::db::system::set_system_config(&conn, "workflow_policy", stored).unwrap();
        crate::db::system::set_system_config(&conn, "review_mode", "false").unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_policy_sync BEFORE INSERT ON system_config
             WHEN NEW.key = 'workflow_policy'
             BEGIN SELECT RAISE(ABORT, 'test_policy_sync_write_failure'); END;",
        )
        .unwrap();
    }
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(
        &mut socket,
        &state,
        br#"{"domain_concurrency":9,"review_mode":true,"poll_seconds":99}"#,
    )
    .await
    .unwrap();
    drop(socket);
    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 500 "), "{response}");
    let conn = db_arc.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "workflow_policy").as_deref(),
        Some(stored)
    );
    assert_eq!(
        crate::db::system::get_system_config(&conn, "review_mode").as_deref(),
        Some("false")
    );
    assert_eq!(
        crate::db::system::get_system_config(&conn, "domain_concurrency"),
        None
    );
    drop(conn);
    let guard = state.lock().await;
    assert_eq!(guard.worker_loop_poll_seconds, 20);
    assert_eq!(guard.last_event, "test.ready");
}

#[tokio::test]
async fn worker_config_explicit_policy_owns_default_over_conflicting_flag() {
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let policy = json!({
        "schema_version": "workflow-policy-v1",
        "default_mode": "auto",
        "by_domain": { "site.test": "review" },
        "by_content_format": { "html": "review" },
        "by_rule": { "7": "review" }
    });
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(
        &mut socket,
        &state,
        &serde_json::to_vec(&json!({
            "review_mode": true, "workflow_policy": policy,
            "workflow_dsl": {
                "schema_version": "workflow-dsl-v1",
                "steps": [{ "id": "human_review", "type": "human_review", "when": null }]
            }
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    drop(socket);
    let body = parse_http_json_body(&reader.await.unwrap());
    assert_eq!(body["data"]["review_mode"], json!(false));
    assert_eq!(body["data"]["workflow_policy"], policy);
    let guard = state.lock().await;
    let conn = guard.db.lock().await;
    assert_eq!(
        crate::db::system::get_system_config(&conn, "review_mode").as_deref(),
        Some("false")
    );
    assert_eq!(
        crate::task_engine::workflow_policy::load_workflow_policy(&conn, true).resolve(
            Some("site.test"),
            None,
            None
        ),
        crate::task_engine::workflow_policy::WorkflowMode::Review
    );
}

#[tokio::test]
async fn worker_config_concurrent_saves_return_each_own_snapshot() {
    use std::future::Future;
    use std::task::Poll;

    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let (mut first_socket, first_reader) = spawn_config_reader().await;
    let (mut second_socket, second_reader) = spawn_config_reader().await;
    let held_state = state.lock().await;
    let mut first = Box::pin(handle_worker_config(
        &mut first_socket,
        &state,
        br#"{"poll_seconds":33,"domain_concurrency":3}"#,
    ));
    let mut second = Box::pin(handle_worker_config(
        &mut second_socket,
        &state,
        br#"{"poll_seconds":66,"domain_concurrency":6}"#,
    ));
    // Poll both requests up to the state lock, putting both saves in the
    // fair mutex's queue before releasing it. No timing-based race injection.
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(held_state);
    let (a, b) = tokio::join!(first, second);
    a.unwrap();
    b.unwrap();
    drop(first_socket);
    drop(second_socket);
    let first = parse_http_json_body(&first_reader.await.unwrap());
    let second = parse_http_json_body(&second_reader.await.unwrap());
    assert_eq!(first["data"]["poll_seconds"], json!(33));
    assert_eq!(first["data"]["domain_concurrency"], json!(3));
    assert_eq!(second["data"]["poll_seconds"], json!(66));
    assert_eq!(second["data"]["domain_concurrency"], json!(6));
}

#[tokio::test]
async fn worker_config_snapshot_failure_rolls_back_before_commit() {
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let db = {
        let guard = state.lock().await;
        std::sync::Arc::clone(&guard.db)
    };
    {
        let conn = db.lock().await;
        conn.execute(
            "INSERT INTO system_config (key, value) VALUES ('callback_concurrency', X'FF')",
            [],
        )
        .unwrap();
    }
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(
        &mut socket,
        &state,
        br#"{"poll_seconds":99,"auto_start_worker":true}"#,
    )
    .await
    .unwrap();
    drop(socket);
    let response = reader.await.unwrap();
    assert!(response.starts_with("HTTP/1.1 500 "), "{response}");
    assert_eq!(
        parse_http_json_body(&response)["error"]["code"],
        json!("WORKER_CONFIG_SAVE_FAILED")
    );
    let guard = state.lock().await;
    assert_eq!(guard.worker_loop_poll_seconds, 20);
    assert_eq!(guard.last_event, "test.ready");
    let conn = db.lock().await;
    for key in ["auto_start_worker", "worker_poll_seconds"] {
        assert_eq!(crate::db::system::get_system_config(&conn, key), None);
    }
    let original: Vec<u8> = conn
        .query_row(
            "SELECT value FROM system_config WHERE key = 'callback_concurrency'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(original, vec![0xff]);
}

#[tokio::test]
async fn worker_config_write_failure_rolls_back_and_preserves_runtime_state() {
    let input = json!({
        "auto_start_worker": true, "adaptive_rate_control": false,
        "domain_concurrency": 9, "relation_concurrency": 9,
        "global_translation_concurrency": 9, "global_callback_concurrency": 9,
        "relation_max_pending_callbacks": 9, "adaptive_max_delay_ms": 900,
        "callback_concurrency": 9, "callback_timeout_secs": 9, "callback_retry_max": 9,
        "fetch_timeout_secs": 9, "fetch_retry_max": 9, "poll_seconds": 99,
        "workflow_policy": { "schema_version": "workflow-policy-v1", "default_mode": "review" },
        "workflow_dsl": { "schema_version": "workflow-dsl-v1", "steps": [{ "id": "sync", "type": "sync" }] }
    });
    for rejected_key in [
        "domain_concurrency",
        "relation_concurrency",
        "global_translation_concurrency",
        "global_callback_concurrency",
        "relation_max_pending_callbacks",
        "adaptive_max_delay_ms",
        "callback_concurrency",
        "callback_timeout_secs",
        "callback_retry_max",
        "fetch_timeout_secs",
        "fetch_retry_max",
        "auto_start_worker",
        "adaptive_rate_control",
        "workflow_policy",
        "review_mode",
        "workflow_dsl",
        "worker_poll_seconds",
    ] {
        let state = build_test_web_ui_state("http://127.0.0.1:1", None);
        let db_arc = {
            let mut guard = state.lock().await;
            guard.worker_loop_poll_seconds = 7;
            guard.last_error = "existing runtime error".to_string();
            guard.updated_at = 123;
            std::sync::Arc::clone(&guard.db)
        };
        {
            let conn = db_arc.lock().await;
            crate::db::system::set_system_config(&conn, "worker_poll_seconds", "7").unwrap();
            crate::db::system::set_system_config(&conn, "auto_start_worker", "false").unwrap();
            // The key comes only from the fixed whitelist above.
            conn.execute_batch(&format!(
                "CREATE TRIGGER reject_config_write BEFORE INSERT ON system_config
                 WHEN NEW.key = '{rejected_key}'
                 BEGIN SELECT RAISE(ABORT, 'test_worker_config_write_failure'); END;"
            ))
            .unwrap();
        }
        let (mut socket, reader) = spawn_config_reader().await;
        handle_worker_config(&mut socket, &state, &serde_json::to_vec(&input).unwrap())
            .await
            .unwrap();
        drop(socket);
        let response = reader.await.unwrap();
        assert!(
            response.starts_with("HTTP/1.1 500 "),
            "a {rejected_key} storage failure must not report success: {response}"
        );
        let body = parse_http_json_body(&response);
        assert_eq!(body["success"], json!(false));
        assert_eq!(body["error"]["code"], json!("WORKER_CONFIG_SAVE_FAILED"));
        assert!(!response.contains("test_worker_config_write_failure"));
        let guard = state.lock().await;
        assert_eq!(guard.worker_loop_poll_seconds, 7);
        assert_eq!(guard.last_event, "test.ready");
        assert_eq!(guard.last_error, "existing runtime error");
        assert_eq!(guard.updated_at, 123);
        let conn = db_arc.lock().await;
        assert_eq!(
            crate::db::system::get_system_config(&conn, "auto_start_worker").as_deref(),
            Some("false")
        );
        assert_eq!(
            crate::db::system::get_system_config(&conn, "worker_poll_seconds").as_deref(),
            Some("7")
        );
        for key in input
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .chain(["review_mode"])
        {
            if key != "auto_start_worker" && key != "poll_seconds" {
                assert_eq!(
                    crate::db::system::get_system_config(&conn, key),
                    None,
                    "{key} must roll back when {rejected_key} fails"
                );
            }
        }
    }
}

#[tokio::test]
async fn worker_config_post_returns_the_same_effective_values_as_get() {
    let _scope = components_env_lock().lock().unwrap();
    for (input, expected) in [
        (
            json!({
                "poll_seconds": 0, "domain_concurrency": 0, "relation_concurrency": 0,
                "global_translation_concurrency": 0, "global_callback_concurrency": 0,
                "relation_max_pending_callbacks": 0, "adaptive_max_delay_ms": 0,
                "callback_concurrency": 0, "callback_timeout_secs": 0,
                "callback_retry_max": 0, "fetch_timeout_secs": 0, "fetch_retry_max": 0
            }),
            json!({
                "poll_seconds": 1, "domain_concurrency": 1, "relation_concurrency": 1,
                "global_translation_concurrency": 1, "global_callback_concurrency": 1,
                "relation_max_pending_callbacks": 1, "adaptive_max_delay_ms": 200,
                "callback_concurrency": 1, "callback_timeout_secs": 1,
                "callback_retry_max": 0, "fetch_timeout_secs": 1, "fetch_retry_max": 0
            }),
        ),
        (
            json!({
                "poll_seconds": 3601, "callback_concurrency": 51,
                "callback_timeout_secs": 301, "callback_retry_max": 11,
                "fetch_timeout_secs": 301, "fetch_retry_max": 11
            }),
            json!({
                "poll_seconds": 3600, "callback_concurrency": 50,
                "callback_timeout_secs": 300, "callback_retry_max": 10,
                "fetch_timeout_secs": 300, "fetch_retry_max": 10
            }),
        ),
    ] {
        let state = build_test_web_ui_state("http://127.0.0.1:1", None);
        let (mut socket, reader) = spawn_config_reader().await;
        handle_worker_config(&mut socket, &state, &serde_json::to_vec(&input).unwrap())
            .await
            .unwrap();
        drop(socket);
        let response = reader.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200 "));
        let post = parse_http_json_body(&response);
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(post["data"][key], *value, "{key} must echo its saved value");
        }
        let (mut socket, reader) = spawn_config_reader().await;
        handle_worker_config_get(&mut socket, &state).await.unwrap();
        drop(socket);
        let get = parse_http_json_body(&reader.await.unwrap());
        assert_eq!(post["data"], get["data"], "POST and readback must agree");
        let guard = state.lock().await;
        let conn = guard.db.lock().await;
        for (key, value) in expected.as_object().unwrap() {
            let stored_key = if key == "poll_seconds" {
                "worker_poll_seconds"
            } else {
                key
            };
            assert_eq!(
                crate::db::system::get_system_config(&conn, stored_key),
                Some(value.to_string()),
                "the database, not just the response, must contain {key}"
            );
        }
    }
}

#[tokio::test]
async fn capacity_worker_limits_survive_post_and_independent_get() {
    let _scope = components_env_lock().lock().unwrap();
    let state = build_test_web_ui_state("", None);
    let input = json!({
        "storage_max_retained_units": 17,
        "storage_max_reserved_bytes": 8_589_934_592u64,
    });
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config(&mut socket, &state, &serde_json::to_vec(&input).unwrap())
        .await
        .unwrap();
    drop(socket);
    let post = parse_http_json_body(&reader.await.unwrap());
    for (key, value) in input.as_object().unwrap() {
        assert_eq!(post["data"][key], *value, "{key} must actually be saved");
    }
    let (mut socket, reader) = spawn_config_reader().await;
    handle_worker_config_get(&mut socket, &state).await.unwrap();
    drop(socket);
    let get = parse_http_json_body(&reader.await.unwrap());
    assert_eq!(post["data"], get["data"]);
}

#[tokio::test]
async fn capacity_invalid_worker_limit_refuses_the_whole_save() {
    for invalid in [0u64, 9_007_199_254_740_992u64] {
        let state = build_test_web_ui_state("", None);
        let (mut socket, reader) = spawn_config_reader().await;
        let body = serde_json::to_vec(&json!({
            "poll_seconds": 61, "storage_max_retained_units": invalid,
        }))
        .unwrap();
        handle_worker_config(&mut socket, &state, &body)
            .await
            .unwrap();
        drop(socket);
        let response = reader.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        let guard = state.lock().await;
        let conn = guard.db.lock().await;
        assert!(
            crate::db::system::get_system_config_checked(&conn, "worker_poll_seconds")
                .unwrap()
                .is_none(),
            "invalid storage limits must not partially save unrelated settings"
        );
    }
}

// ---- preflight hang regressions (2026-09-08 hazard fix) ----
//
// The worker preflight must never hang forever: the component registry load
// is bounded and degrades, and the whole preflight collection is bounded and
// answers with a structured 504. Black-hole servers (accept, never respond)
// reproduce the stalled-catalog / stalled-endpoint conditions.

async fn spawn_blackhole_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hold = tokio::spawn(async move {
        // Accept every connection and keep it alive: never read, never
        // respond. Dropping the connection would let the client error out
        // immediately instead of stalling.
        let mut held: Vec<tokio::net::TcpStream> = Vec::new();
        while let Ok((conn, _)) = listener.accept().await {
            held.push(conn);
        }
    });
    (format!("http://{}", addr), hold)
}

fn seed_preflight_local_docs(component: serde_json::Value) -> (String, String) {
    let db_path = format!(
        "/tmp/wptsall-preflight-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let components_path = format!(
        "/tmp/wptsall-preflight-{}-{}.json",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let doc: crate::types::ComponentsLocalDoc = serde_json::from_value(json!({
        "version": 1,
        "components": {
            "comp-preflight-stall": component
        }
    }))
    .unwrap();
    {
        let conn = crate::db::open_db(&db_path).unwrap();
        crate::db::schema::create_tables(&conn).unwrap();
        crate::db::components::save_local_components_doc(&conn, &doc).unwrap();
    }
    std::fs::write(&components_path, serde_json::to_vec(&doc).unwrap()).unwrap();
    (db_path, components_path)
}

#[tokio::test]
async fn worker_start_check_degrades_when_component_catalog_stalls() {
    let _env_scope = components_env_lock().lock().unwrap();
    let _server_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let _component_timeout_guard =
        EnvVarGuard::set("WPTSALL_PREFLIGHT_COMPONENT_TIMEOUT_SECS", "1".to_string());
    let _skip_sig_guard = EnvVarGuard::set("WPTSALL_SKIP_SIGNATURE_CHECK", "true".to_string());

    // A local component that still needs a server-side template alias
    // download forces the loader to contact the (stalled) catalog.
    let (db_path, components_path) = seed_preflight_local_docs(json!({
        "name": "Stalled Template",
        "template_id": "stalled-template-v1",
        "vendor_id": "local-vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {}
    }));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let (server_base, hold) = spawn_blackhole_server().await;
    let state = build_test_web_ui_state(&server_base, Some("sess_stall_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&mut response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-stall-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    hold.abort();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "stalled component catalog must degrade preflight instead of hanging: {}",
        response
    );
    assert!(
        response.contains("\"can_start\":true"),
        "degraded preflight should default to allow-start: {}",
        response
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn worker_start_check_returns_504_when_preflight_exceeds_total_bound() {
    let _env_scope = components_env_lock().lock().unwrap();
    let _server_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let _total_timeout_guard = EnvVarGuard::set("WPTSALL_PREFLIGHT_TIMEOUT_SECS", "1".to_string());

    // Inline local template: the catalog fetch is skipped entirely, so the
    // registry builds locally; the server-mode domain list fetch then stalls
    // on the black-hole server and the total bound must answer with a 504.
    let (db_path, components_path) = seed_preflight_local_docs(json!({
        "name": "Inline Local",
        "template_id": "",
        "vendor_id": "local-vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {},
        "template_json": {
            "id": "inline-template",
            "name": "Inline Template",
            "version": "1.0.0",
            "type": "text",
            "auth": null,
            "request": {
                "method": "POST",
                "url": "http://127.0.0.1:1/translate",
                "headers": null,
                "body": {"text": "{{input.text}}"}
            },
            "response": {
                "translated_text_path": "data.text",
                "error_path": null,
                "translated_ref_path": null,
                "translated_media_ref_path": null,
                "translated_image_ref_path": null,
                "translated_video_ref_path": null,
                "translated_audio_ref_path": null,
                "translated_document_ref_path": null
            }
        }
    }));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let (server_base, hold) = spawn_blackhole_server().await;
    let state = build_test_web_ui_state(&server_base, Some("sess_total_test"));

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&mut response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start_check(
        &mut socket,
        &state,
        "/tmp/wptsall-worker-start-check-total-timeout-test.log",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    hold.abort();

    assert!(
        response.starts_with("HTTP/1.1 504 Gateway Timeout"),
        "preflight exceeding the total bound must answer with a structured 504: {}",
        response
    );
    assert!(
        response.contains("\"WORKER_START_PREFLIGHT_TIMEOUT\""),
        "preflight timeout must carry a stable error code: {}",
        response
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}

#[tokio::test]
async fn worker_start_returns_504_when_preflight_exceeds_total_bound() {
    let _env_scope = components_env_lock().lock().unwrap();
    let _server_mode_guard =
        EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "true".to_string());
    let _total_timeout_guard = EnvVarGuard::set("WPTSALL_PREFLIGHT_TIMEOUT_SECS", "1".to_string());
    let _wp_token_guard = EnvVarGuard::set("WPTSALL_WP_CLIENT_TOKEN", "wp_token_test".to_string());

    let (db_path, components_path) = seed_preflight_local_docs(json!({
        "name": "Inline Local",
        "template_id": "",
        "vendor_id": "local-vendor",
        "kind": "text",
        "enabled": true,
        "created_at": "1",
        "versions": {},
        "template_json": {
            "id": "inline-template",
            "name": "Inline Template",
            "version": "1.0.0",
            "type": "text",
            "auth": null,
            "request": {
                "method": "POST",
                "url": "http://127.0.0.1:1/translate",
                "headers": null,
                "body": {"text": "{{input.text}}"}
            },
            "response": {
                "translated_text_path": "data.text",
                "error_path": null,
                "translated_ref_path": null,
                "translated_media_ref_path": null,
                "translated_image_ref_path": null,
                "translated_video_ref_path": null,
                "translated_audio_ref_path": null,
                "translated_document_ref_path": null
            }
        }
    }));
    let _db_guard = EnvVarGuard::set("WPTSALL_DB_PATH", db_path.clone());
    let _components_guard =
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", components_path.clone());

    let (server_base, hold) = spawn_blackhole_server().await;
    let state = build_test_web_ui_state(&server_base, Some("sess_total_test"));
    let runtime_control = WebUiRuntimeControl::new();

    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let downstream_addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut client = tokio::net::TcpStream::connect(downstream_addr)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&mut response).to_string()
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_start(
        &mut socket,
        &state,
        &runtime_control,
        "/tmp/wptsall-worker-start-total-timeout-test.log",
        b"{}",
    )
    .await
    .unwrap();
    drop(socket);

    let response = reader.await.unwrap();
    hold.abort();

    assert!(
        response.starts_with("HTTP/1.1 504 Gateway Timeout"),
        "worker start preflight timeout must answer with a structured 504: {}",
        response
    );
    assert!(
        response.contains("\"WORKER_START_PREFLIGHT_TIMEOUT\""),
        "worker start timeout must carry a stable error code: {}",
        response
    );
    assert!(
        !runtime_control
            .worker_running
            .load(std::sync::atomic::Ordering::SeqCst),
        "worker loop must remain stopped when preflight times out"
    );

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&components_path);
}

// ---------------------------------------------------------------------------
// 批 N3 / U-7 stop log contract (doc21 G1 line): the UI Stop Loop path
// aborts the loop task, which can never reach its natural tail log. The
// handler must emit `worker.loop.stopped` exactly-once — a loop that
// already exited naturally is not re-logged, and stopping a plain
// run-once must not claim the loop stopped.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worker_stop_emits_loop_stopped_exactly_once_for_running_loop() {
    // Serializes with every other logging-state test and snapshots/restores
    // the process-global log flags (see logging::acquire_log_state).
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(true, "info");
    let log_path = std::env::temp_dir()
        .join(format!("wptsall-worker-stop-once-{}.log", unix_ts()))
        .to_string_lossy()
        .to_string();
    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_stop_once"));
    {
        let mut guard = state.lock().await;
        guard.worker_loop_running = true;
        guard.last_event = "worker.loop.started".to_string();
    }
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
    let runtime_control = WebUiRuntimeControl::new();
    handle_worker_stop(&mut socket, &state, &log_path, &runtime_control)
        .await
        .unwrap();
    drop(socket);
    crate::logging::flush_log();
    let response = reader.await.unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "worker stop should answer 200: {}",
        response
    );
    assert!(
        response.contains("\"success\":true"),
        "worker stop payload: {}",
        response
    );
    {
        let guard = state.lock().await;
        assert!(!guard.worker_loop_running, "loop running flag must clear");
        assert_eq!(guard.last_event, "worker.loop.stopped");
    }
    crate::logging::flush_log();
    assert_eq!(
        count_loop_stopped(&log_path),
        1,
        "stop of a running loop must log worker.loop.stopped exactly once"
    );
    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn worker_stop_does_not_relog_loop_stopped_for_naturally_exited_loop() {
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(true, "info");
    let log_path = std::env::temp_dir()
        .join(format!("wptsall-worker-stop-natural-{}.log", unix_ts()))
        .to_string_lossy()
        .to_string();
    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_stop_natural"));
    {
        let mut guard = state.lock().await;
        guard.worker_loop_running = true;
        // The loop already broke on its own and logged its terminal event.
        guard.last_event = "worker.loop.stopped".to_string();
    }
    let response = drive_worker_stop_with_log(&state, &log_path).await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "worker stop should answer 200: {}",
        response
    );
    assert_eq!(
        count_loop_stopped(&log_path),
        0,
        "a naturally-exited loop must not be re-logged as stopped"
    );
    let _ = std::fs::remove_file(&log_path);
}

#[tokio::test]
async fn worker_stop_does_not_claim_loop_stopped_for_plain_run_once() {
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(true, "info");
    let log_path = std::env::temp_dir()
        .join(format!("wptsall-worker-stop-runonce-{}.log", unix_ts()))
        .to_string_lossy()
        .to_string();
    let state = build_test_web_ui_state("http://127.0.0.1:1", Some("sess_stop_runonce"));
    {
        let mut guard = state.lock().await;
        guard.worker_loop_running = false;
        guard.last_event = "worker.run_once.completed".to_string();
    }
    let response = drive_worker_stop_with_log(&state, &log_path).await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "worker stop should answer 200: {}",
        response
    );
    {
        let guard = state.lock().await;
        assert_eq!(
            guard.last_event, "worker.run_once.completed",
            "run-once stop must not rewrite last_event to the loop event"
        );
    }
    assert_eq!(
        count_loop_stopped(&log_path),
        0,
        "stopping a plain run-once must not log worker.loop.stopped"
    );
    let _ = std::fs::remove_file(&log_path);
}

/// Drive handle_worker_stop against a socket pair, flush the process log
/// writer, and return the raw HTTP response (批 N3 stop-contract tests).
async fn drive_worker_stop_with_log(
    state: &Arc<tokio::sync::Mutex<WebUiState>>,
    log_path: &str,
) -> String {
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
    let runtime_control = WebUiRuntimeControl::new();
    handle_worker_stop(&mut socket, state, log_path, &runtime_control)
        .await
        .unwrap();
    drop(socket);
    crate::logging::flush_log();
    reader.await.unwrap()
}

fn count_loop_stopped(log_path: &str) -> usize {
    std::fs::read_to_string(log_path)
        .map(|content| {
            content
                .lines()
                .filter(|line| line.contains("\"worker.loop.stopped\""))
                .count()
        })
        .unwrap_or(0)
}

// catalog: WEBUI-MOD-web-ui-routes-worker-rs
// oracle: L2
#[tokio::test]
async fn cli05_worker_stop_allows_an_inflight_tick_to_commit_before_aborting() {
    let (_log_lock, _log_state) = crate::logging::acquire_log_state(false, "info");
    let root = tempfile::tempdir().unwrap();
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let conn = crate::db::open_db(root.path().join("stop.db").to_str().unwrap()).unwrap();
    let job_id = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: "https://cli05.invalid".to_string(),
            relation_id: 7,
            business_line: "discovery".to_string(),
            triggered_by: "auto".to_string(),
        },
    )
    .unwrap();
    crate::db::jobs::update_job_status(&conn, job_id, "running").unwrap();
    let item_id = crate::db::jobs::create_item(
        &conn,
        &crate::db::jobs::CreateItemRequest {
            job_id,
            domain: "https://cli05.invalid".to_string(),
            relation_id: 7,
            business_line: "post_content".to_string(),
            object_type: "post_type".to_string(),
            wp_object_id: 42,
            wp_object_subtype: "post".to_string(),
            task_type: "text".to_string(),
            source_lang: "en".to_string(),
            target_lang: "zh".to_string(),
            component_id: String::new(),
            component_ids: Vec::new(),
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: String::new(),
            client_task_id: "fixture-stop".to_string(),
            max_retries: 3,
        },
    )
    .unwrap();
    crate::db::jobs::update_item_status(&conn, item_id, "translating", None).unwrap();
    let db = Arc::new(tokio::sync::Mutex::new(conn));
    {
        let mut guard = state.lock().await;
        guard.db = Arc::clone(&db);
        guard.worker_loop_running = true;
        guard.last_event = "worker.loop.started".to_string();
    }
    let runtime_control = WebUiRuntimeControl::new();
    runtime_control.worker_running.store(true, Ordering::SeqCst);
    let (started, ready) = tokio::sync::oneshot::channel();
    let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = Arc::clone(&committed);
    let tick_db = Arc::clone(&db);
    let handle = tokio::spawn(async move {
        started.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let conn = tick_db.lock().await;
        crate::db::jobs::update_item_status(&conn, item_id, "done", None).unwrap();
        crate::db::jobs::project_job_from_items(&conn, job_id, true, "completed").unwrap();
        done.store(true, Ordering::SeqCst);
    });
    *runtime_control.worker_handle.lock().await = Some(handle);
    ready.await.unwrap();
    let downstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = downstream.local_addr().unwrap();
    let reader = tokio::spawn(async move {
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let (mut socket, _) = downstream.accept().await.unwrap();
    handle_worker_stop(
        &mut socket,
        &state,
        &root.path().join("stop.log").display().to_string(),
        &runtime_control,
    )
    .await
    .unwrap();
    drop(socket);
    assert!(reader.await.unwrap().starts_with(b"HTTP/1.1 200 OK"));
    assert!(
        committed.load(Ordering::SeqCst),
        "Stop must allow the owned in-flight tick a bounded commit grace"
    );
    let conn = db.lock().await;
    assert_eq!(
        crate::db::jobs::get_item(&conn, item_id).unwrap().status,
        "done"
    );
    assert_eq!(
        crate::db::jobs::get_job(&conn, job_id).unwrap().status,
        "completed"
    );
    assert!(!runtime_control.worker_running.load(Ordering::SeqCst));
    assert!(runtime_control.worker_handle.lock().await.is_none());
}

// catalog: WEBUI-MOD-web-ui-routes-worker-rs
// catalog: WEBUI-MOD-db-runtime-rs
// oracle: L2
#[tokio::test]
async fn cli05_stop_timeout_cancels_owned_descendants_before_the_lease_can_be_reused() {
    struct Dropped(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cancel.db");
    let lease = crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    let descendant = crate::db::runtime::RuntimeLease::for_connection(&conn).unwrap();
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&dropped);
    let (started, ready) = tokio::sync::oneshot::channel();
    let runtime = WebUiRuntimeControl::new();
    runtime.worker_running.store(true, Ordering::SeqCst);
    let handle = tokio::spawn(async move {
        let _lease = lease;
        let mut children = tokio::task::JoinSet::new();
        children.spawn(async move {
            let _lease = descendant;
            let _dropped = Dropped(observed);
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        std::future::pending::<()>().await;
    });
    *runtime.worker_handle.lock().await = Some(handle);
    ready.await.unwrap();
    assert!(crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).is_err());
    let stop_at = std::time::Instant::now();
    stop_worker_loop(&runtime, std::time::Duration::from_millis(20)).await;
    assert!(
        stop_at.elapsed() < std::time::Duration::from_secs(1),
        "hung tick is bounded"
    );
    assert!(!runtime.worker_running.load(Ordering::SeqCst));
    assert!(runtime.worker_handle.lock().await.is_none());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned descendants must receive cancellation");
    // Cancellation can finish just after the parent await; each descendant
    // holds the lease until that actual finish, not just until Stop is signaled.
    crate::db::runtime::RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
}

// catalog: WEBUI-MOD-web-ui-routes-worker-rs
// oracle: L2
#[tokio::test]
async fn cli05_concurrent_start_waits_for_stop_and_the_old_handles_natural_tail() {
    let _local = EnvVarGuard::set("WPTSALL_USE_SERVER_CONTROL_PLANE", "0".to_string());
    let _components = EnvVarGuard::set("WPTSALL_COMPONENT_RUNTIME", "0".to_string());
    let root = tempfile::tempdir().unwrap();
    let _data = EnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let state = build_test_web_ui_state("http://127.0.0.1:1", None);
    let runtime = WebUiRuntimeControl::new();
    runtime.worker_running.store(true, Ordering::SeqCst);
    state.lock().await.worker_loop_running = true;
    let release = Arc::new(tokio::sync::Notify::new());
    let old_release = Arc::clone(&release);
    let old_runtime = runtime.clone();
    let old = tokio::spawn(async move {
        old_release.notified().await;
        old_runtime.worker_running.store(false, Ordering::SeqCst);
        *old_runtime.worker_handle.lock().await = None;
    });
    *runtime.worker_handle.lock().await = Some(old);
    let stopping_runtime = runtime.clone();
    let stopping = tokio::spawn(async move {
        stop_worker_loop(&stopping_runtime, std::time::Duration::from_secs(2)).await;
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while runtime.worker_handle.lock().await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let starting_runtime = runtime.clone();
    let starting_state = Arc::clone(&state);
    let log = root
        .path()
        .join("serialized-start.log")
        .display()
        .to_string();
    let (started, ready) = tokio::sync::oneshot::channel();
    let starting = tokio::spawn(async move {
        started.send(()).unwrap();
        spawn_worker_loop(starting_state, starting_runtime, log, "fixture").await;
    });
    ready.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(
        !starting.is_finished(),
        "Start must stay behind the still-pending Stop lifecycle"
    );
    assert!(
        !runtime.worker_running.load(Ordering::SeqCst),
        "an old natural tail must not race a replacement loop"
    );
    release.notify_one();
    stopping.await.unwrap();
    starting.await.unwrap();
    assert!(runtime.worker_running.load(Ordering::SeqCst));
    assert!(
        runtime.worker_handle.lock().await.is_some(),
        "old tail cannot erase the replacement handle"
    );
    stop_worker_loop(&runtime, std::time::Duration::from_secs(2)).await;
    assert!(!runtime.worker_running.load(Ordering::SeqCst));
    assert!(runtime.worker_handle.lock().await.is_none());
}
