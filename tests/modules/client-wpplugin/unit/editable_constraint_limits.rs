//! Numeric limits are exercised through the actual fresh/version/task builder.
//! All configuration is synthetic and scoped to the owned fixture root.
use super::*;

fn local_doc(version_overrides: Value) -> ComponentsLocalDoc {
    serde_json::from_value(json!({
        "version":1, "components":{"owned-limits":{
            "name":"Owned limits","vendor_id":"owned-vendor","template_id":"owned-template",
            "kind":"text","enabled":true,"created_at":"1","active_version":"v1",
            "source_template_api_version":"1.0.0",
            "versions":{"v1":{"version":"v1","created_at":"1",
                "config_overrides":version_overrides}},
            "template_json":{
                "id":"owned-template","name":"Owned limits","version":"1.0.0",
                "type":"text_translation","auth":{"fields":[]},
                "request":{"method":"POST","url":"http://127.0.0.1:1/owned","body":{}},
                "response":{"translated_text_path":"data.text"},
                "constraints":{"max_input_chars":9,"max_input_bytes":17,
                    "rate_limit_rpm":60,"rate_limit_qps":5,
                    "max_concurrent_requests":2,"max_file_size_mb":1},
                "editable_params":[{"path":"constraints.*"},{"path":"request.body.*"},
                    {"path":"default_values.*"}]
            }
        }}
    }))
    .unwrap()
}

async fn fresh(version: Value, task: Option<&Value>) -> anyhow::Result<ComponentRuntime> {
    build_owned(version, task, false).await
}

async fn build_owned(
    version: Value,
    task: Option<&Value>,
    registry_loader: bool,
) -> anyhow::Result<ComponentRuntime> {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _paths = configure_owned(root.path(), version);
    if registry_loader {
        let selected = std::collections::BTreeSet::from(["owned-limits".to_string()]);
        let mut bindings = ComponentBindingsDoc::default();
        let registry = crate::component_rt::loader::load_component_runtimes(
            &Client::builder().no_proxy().build().unwrap(),
            "http://127.0.0.1:1",
            "",
            root.path().join("owned.log").to_str().unwrap(),
            &mut bindings,
            root.path().join("bindings.json").to_str().unwrap(),
            Some(&selected),
            None,
        )
        .await?;
        return Ok(registry.runtimes["owned-limits"].clone());
    }
    let state = build_test_web_ui_state("", None);
    build_local_component_runtime_for_task(&state, "owned-limits", task).await
}

fn configure_owned(root: &std::path::Path, version: Value) -> [EnvVarGuard; 4] {
    let path = root.join("local.json");
    let paths = [
        EnvVarGuard::set("WPTSALL_COMPONENTS_LOCAL_FILE", path.display().to_string()),
        EnvVarGuard::set(
            "WPTSALL_VENDOR_KEYS_FILE",
            root.join("keys.json").display().to_string(),
        ),
        EnvVarGuard::set(
            "WPTSALL_VENDOR_OAUTH_FILE",
            root.join("oauth.json").display().to_string(),
        ),
        EnvVarGuard::set(
            "WPTSALL_PROXY_PROFILES_FILE",
            root.join("proxy.json").display().to_string(),
        ),
    ];
    save_components_local(path.to_str().unwrap(), &local_doc(version)).unwrap();
    paths
}

macro_rules! bad_task {
    ($name:ident, $path:literal, $value:expr) => {
        #[tokio::test]
        async fn $name() {
            let overrides = json!({$path:$value});
            let result = fresh(json!({}), Some(&overrides)).await;
            assert!(result.is_err(), "damaged explicit limit must refuse, not inherit/wrap/unlimit");
            let error = result.unwrap_err();
            assert!(error.downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>().is_some());
            assert!(!format!("{error:#}").contains("owned-private-invalid"));
        }
    };
}

bad_task!(
    fresh_task_qps_u32_overflow_refuses,
    "constraints.rate_limit_qps",
    4_294_967_296u64
);
bad_task!(
    fresh_task_rpm_u32_overflow_refuses,
    "constraints.rate_limit_rpm",
    4_294_967_296u64
);
bad_task!(
    fresh_task_concurrency_u32_overflow_refuses,
    "constraints.max_concurrent_requests",
    4_294_967_296u64
);
bad_task!(
    fresh_task_file_limit_u32_overflow_refuses,
    "constraints.max_file_size_mb",
    4_294_967_296u64
);
bad_task!(
    fresh_task_negative_file_limit_refuses,
    "constraints.max_file_size_mb",
    -1
);
bad_task!(
    fresh_task_fractional_file_limit_refuses,
    "constraints.max_file_size_mb",
    0.5
);
bad_task!(
    fresh_task_negative_input_chars_refuses,
    "constraints.max_input_chars",
    -1
);
bad_task!(
    fresh_task_wrong_type_input_bytes_refuses,
    "constraints.max_input_bytes",
    "owned-private-invalid"
);
bad_task!(
    fresh_task_null_limit_refuses,
    "constraints.rate_limit_qps",
    Value::Null
);

#[tokio::test]
async fn fresh_selected_version_negative_file_limit_refuses() {
    let result = fresh(json!({"constraints.max_file_size_mb":-1}), None).await;
    assert!(
        result.is_err(),
        "version override must not turn negative into zero/unlimited"
    );
}

#[tokio::test]
async fn fresh_task_non_object_limits_refuse() {
    let result = fresh(json!({}), Some(&json!(["owned-private-invalid"]))).await;
    assert!(
        result.is_err(),
        "non-object overrides must not silently disappear"
    );
}

#[tokio::test]
async fn fresh_task_duplicate_normalized_limit_path_refuses() {
    let values = json!({
        " constraints.rate_limit_qps ":0,
        "constraints.rate_limit_qps":5
    });
    let result = fresh(json!({}), Some(&values)).await;
    assert!(
        result.is_err(),
        "normalized case must not silently pick one authority"
    );
}

#[tokio::test]
async fn fresh_numeric_zero_exact_and_version_task_priority_still_work() {
    let task = json!({
        "constraints.max_input_chars":0,"constraints.max_input_bytes":0,
        "constraints.rate_limit_rpm":0,"constraints.rate_limit_qps":0,
        "constraints.max_concurrent_requests":0,"constraints.max_file_size_mb":0
    });
    let runtime = fresh(json!({"constraints.max_file_size_mb":2}), Some(&task))
        .await
        .unwrap();
    let limits = runtime.template.constraints.unwrap();
    assert_eq!(limits.max_input_chars, Some(0));
    assert_eq!(limits.max_input_bytes, Some(0));
    assert_eq!(limits.max_file_size_mb, Some(0));
    assert_eq!(runtime.runtime_min_interval_ms, 0);
    assert_eq!(runtime.runtime_max_concurrent_requests, 0);

    let task = json!({"constraints.max_file_size_mb":3,"request.body.temperature":0.125});
    let runtime = fresh(json!({"constraints.max_file_size_mb":2}), Some(&task))
        .await
        .unwrap();
    assert_eq!(
        runtime.template.constraints.unwrap().max_file_size_mb,
        Some(3)
    );
    assert_eq!(runtime.template.request.body.unwrap()["temperature"], 0.125);
}

#[tokio::test]
async fn automatic_registry_bad_version_is_typed_authority_failure_not_fallback() {
    let error = build_owned(
        json!({"constraints.rate_limit_qps":4_294_967_296u64}),
        None,
        true,
    )
    .await
    .unwrap_err();
    assert!(error
        .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
        .is_some());
}

#[tokio::test]
async fn automatic_registry_valid_selected_version_limits_still_work() {
    let runtime = build_owned(json!({"constraints.max_file_size_mb":3}), None, true)
        .await
        .unwrap();
    assert_eq!(
        runtime.template.constraints.unwrap().max_file_size_mb,
        Some(3)
    );
}

#[tokio::test]
async fn review_api_invalid_limits_refuse_without_persisting_or_echoing_value() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _paths = configure_owned(root.path(), json!({}));
    let state = build_test_web_ui_state("", None);
    let item_id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:1/wp-json/wptsall/v2/owned-limit",
        "",
        "pending_review",
        "owned-limit-api",
    )
    .await;
    let db = Arc::clone(&state.lock().await.db);
    let original = crate::db::jobs::get_item_checked(&*db.lock().await, item_id)
        .unwrap()
        .unwrap();
    for value in [
        json!({"constraints.max_input_bytes":"owned-private-invalid"}),
        json!({"constraints.rate_limit_qps":4_294_967_296u64}),
        json!(["owned-private-invalid"]),
        json!({"constraints.rate_limit_qps":0}),
    ] {
        let body = serde_json::to_vec(&json!({
            "component_id":"owned-limits","editable_overrides":value
        }))
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap());
        let (client, accepted) = tokio::join!(client, listener.accept());
        let mut client = client.unwrap();
        let mut server = accepted.unwrap().0;
        handle_item_override_save(&mut server, &state, &body, &item_id.to_string())
            .await
            .unwrap();
        server.shutdown().await.unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).await.unwrap();
        assert!(!response.contains("owned-private-invalid"));
        let item = crate::db::jobs::get_item_checked(&*db.lock().await, item_id)
            .unwrap()
            .unwrap();
        if value.get("constraints.rate_limit_qps") == Some(&json!(0)) {
            assert!(response.starts_with("HTTP/1.1 200"), "response={response}");
            assert_eq!(item.selected_component_id.as_deref(), Some("owned-limits"));
            assert_eq!(item.editable_overrides, Some(value));
        } else {
            assert!(response.starts_with("HTTP/1.1 422"), "response={response}");
            assert_eq!(item.selected_component_id, original.selected_component_id);
            assert_eq!(item.editable_overrides, original.editable_overrides);
        }
        assert_eq!(item.status, "pending_review");
    }
}

const HTTP_PHASES: [&str; 7] = [
    "request",
    "prepare.request",
    "async_poll.request",
    "async_poll.result_request",
    "async_poll.result_download",
    "async_poll.reconcile.request",
    "source_upload",
];

fn http_local_doc(version: Value) -> ComponentsLocalDoc {
    let mut doc = local_doc(version);
    let template = doc
        .components
        .get_mut("owned-limits")
        .unwrap()
        .template_json
        .as_mut()
        .unwrap();
    let request = json!({"method":"POST","url":"http://127.0.0.1:1/owned",
        "http_limits":{"max_response_bytes":100,"timeout_ms":1000}});
    template["request"] = request.clone();
    template["prepare"] = json!({"request":request.clone()});
    template["async_poll"] = json!({"job_id_path":"job_id","request":request.clone(),
        "result_request":request.clone(),"result_download":request.clone(),
        "reconcile":{"request":request.clone(),"matches_path":"matches",
            "attempt_id_path":"attempt","binding_path":"binding","job_id_path":"job_id",
            "status_path":"status","resumable_values":["pending"]}});
    template["source_upload"] = request;
    for phase in HTTP_PHASES {
        template["editable_params"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path":format!("{phase}.http_limits.*")}));
    }
    doc
}

async fn http_fresh(
    version: Value,
    task: Option<&Value>,
    registry_loader: bool,
) -> anyhow::Result<ComponentRuntime> {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _paths = configure_owned(root.path(), json!({}));
    save_components_local(
        root.path().join("local.json").to_str().unwrap(),
        &http_local_doc(version),
    )
    .unwrap();
    if registry_loader {
        let selected = std::collections::BTreeSet::from(["owned-limits".to_string()]);
        let registry = crate::component_rt::loader::load_component_runtimes(
            &Client::builder().no_proxy().build().unwrap(),
            "http://127.0.0.1:1",
            "",
            root.path().join("owned.log").to_str().unwrap(),
            &mut ComponentBindingsDoc::default(),
            root.path().join("bindings.json").to_str().unwrap(),
            Some(&selected),
            None,
        )
        .await?;
        return Ok(registry.runtimes["owned-limits"].clone());
    }
    let state = build_test_web_ui_state("", None);
    build_local_component_runtime_for_task(&state, "owned-limits", task).await
}

#[tokio::test]
async fn http_editable_all_phase_leaves_apply_in_fresh_task() {
    let mut overrides = serde_json::Map::new();
    for (index, phase) in HTTP_PHASES.iter().enumerate() {
        overrides.insert(
            format!("{phase}.http_limits.max_response_bytes"),
            json!(index + 1),
        );
        overrides.insert(
            format!("{phase}.http_limits.timeout_ms"),
            json!(index + 300),
        );
    }
    let runtime = http_fresh(json!({}), Some(&Value::Object(overrides)), false)
        .await
        .unwrap();
    let template = serde_json::to_value(runtime.template).unwrap();
    for (index, phase) in HTTP_PHASES.iter().enumerate() {
        let pointer = format!("/{}/http_limits", phase.replace('.', "/"));
        assert_eq!(
            template.pointer(&pointer).unwrap(),
            &json!({
            "max_response_bytes":index+1,"timeout_ms":index+300})
        );
    }
}

#[tokio::test]
async fn http_editable_selected_version_registry_and_task_priority() {
    let version = json!({"request.http_limits.max_response_bytes":11,
        "request.http_limits.timeout_ms":333});
    let runtime = http_fresh(version.clone(), None, true).await.unwrap();
    assert_eq!(
        runtime
            .template
            .request
            .http_limits
            .unwrap()
            .max_response_bytes,
        Some(11)
    );
    let runtime = http_fresh(
        version,
        Some(&json!({"request.http_limits.timeout_ms":444})),
        false,
    )
    .await
    .unwrap();
    let limits = runtime.template.request.http_limits.unwrap();
    assert_eq!(limits.max_response_bytes, Some(11));
    assert_eq!(limits.timeout_ms, Some(444));
}

#[tokio::test]
async fn http_editable_bad_numbers_all_phases_refuse_without_value_echo() {
    for phase in HTTP_PHASES {
        for member in ["max_response_bytes", "timeout_ms"] {
            for value in [
                json!(0),
                json!(-1),
                json!(0.5),
                Value::Null,
                json!(true),
                json!("owned-private-invalid"),
                json!(u64::MAX),
            ] {
                let task = json!({format!("{phase}.http_limits.{member}"):value});
                let error = http_fresh(json!({}), Some(&task), false).await.unwrap_err();
                assert!(error
                    .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
                    .is_some());
                assert!(!format!("{error:#}").contains("owned-private-invalid"));
            }
        }
    }
}

#[tokio::test]
async fn http_editable_invalid_selected_version_refuses() {
    let error = http_fresh(
        json!({"source_upload.http_limits.timeout_ms":0}),
        None,
        true,
    )
    .await
    .unwrap_err();
    assert!(error
        .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
        .is_some());
}

#[tokio::test]
async fn http_editable_unknown_limit_member_refuses() {
    let task = json!({"request.http_limits.owned-private-invalid":9});
    let error = http_fresh(json!({}), Some(&task), false).await.unwrap_err();
    assert!(error
        .downcast_ref::<crate::component_rt::loader::RuntimeConfigurationFault>()
        .is_some());
    assert!(!format!("{error:#}").contains("owned-private-invalid"));
}

#[tokio::test]
async fn http_editable_review_save_refuses_invalid_before_mutation() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let _paths = configure_owned(root.path(), json!({}));
    save_components_local(
        root.path().join("local.json").to_str().unwrap(),
        &http_local_doc(json!({})),
    )
    .unwrap();
    let state = build_test_web_ui_state("", None);
    let id = seed_translation_item_for_web_ui_test(
        &state,
        "http://127.0.0.1:1/wp-json/wptsall/v2/owned-limit",
        "",
        "pending_review",
        "owned-http-limit-api",
    )
    .await;
    let db = state.lock().await.db.clone();
    let original = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    let values = json!({"request.http_limits.timeout_ms":"owned-private-invalid"});
    let body =
        serde_json::to_vec(&json!({"component_id":"owned-limits","editable_overrides":values}))
            .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connect = TcpStream::connect(listener.local_addr().unwrap());
    let (client, server) = tokio::join!(connect, listener.accept());
    let mut client = client.unwrap();
    let mut server = server.unwrap().0;
    handle_item_override_save(&mut server, &state, &body, &id.to_string())
        .await
        .unwrap();
    server.shutdown().await.unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 422"));
    assert!(!response.contains("owned-private-invalid"));
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert_eq!(item.selected_component_id, original.selected_component_id);
    assert_eq!(item.editable_overrides, original.editable_overrides);
    assert_eq!(item.status, original.status);
    let values = json!({"request.http_limits.timeout_ms":333});
    let body = serde_json::to_vec(&json!({"component_id":"owned-limits",
        "editable_overrides":values}))
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connect = TcpStream::connect(listener.local_addr().unwrap());
    let (client, server) = tokio::join!(connect, listener.accept());
    let mut client = client.unwrap();
    let mut server = server.unwrap().0;
    handle_item_override_save(&mut server, &state, &body, &id.to_string())
        .await
        .unwrap();
    server.shutdown().await.unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    let item = crate::db::jobs::get_item_checked(&*db.lock().await, id)
        .unwrap()
        .unwrap();
    assert_eq!(item.editable_overrides, Some(values));
    let runtime = build_local_component_runtime_for_task(
        &state,
        "owned-limits",
        item.editable_overrides.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(
        runtime.template.request.http_limits.unwrap().timeout_ms,
        Some(333)
    );
}

#[test]
fn http_editable_absent_instance_field_keeps_legacy_serialization() {
    let old = json!({"constraints_override":null,"request_overrides":null,
        "default_values_override":null});
    let overrides: ComponentInstanceOverrides = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(overrides).unwrap(), old);
    let error = serde_json::from_value::<ComponentInstanceOverrides>(json!({
        "http_limits_overrides":{"owned-private-invalid":{"timeout_ms":9}}
    }))
    .unwrap_err();
    assert!(!error.to_string().contains("owned-private-invalid"));
}

#[test]
fn http_editable_uneditable_and_missing_phase_refuse_atomically() {
    let mut template: ComponentTemplate = serde_json::from_value(
        local_doc(json!({})).components["owned-limits"]
            .template_json
            .clone()
            .unwrap(),
    )
    .unwrap();
    let original = serde_json::to_value(&template).unwrap();
    let overrides = crate::component_rt::loader::task_editable_overrides_to_component_overrides(
        Some(&json!({"request.http_limits.timeout_ms":333})),
    )
    .unwrap();
    assert!(
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            Some(&overrides)
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&template).unwrap(), original);
    template.editable_params.extend(
        serde_json::from_value::<Vec<ComponentEditableParam>>(json!([
        {"path":"request.http_limits.*"},{"path":"source_upload.http_limits.*"}]))
        .unwrap(),
    );
    let original = serde_json::to_value(&template).unwrap();
    let overrides = crate::component_rt::loader::task_editable_overrides_to_component_overrides(
        Some(&json!({"request.http_limits.timeout_ms":333,
            "source_upload.http_limits.max_response_bytes":22})),
    )
    .unwrap();
    assert!(
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            Some(&overrides)
        )
        .is_err()
    );
    assert_eq!(serde_json::to_value(&template).unwrap(), original);
}

#[tokio::test]
async fn http_editable_exact_ceilings_and_duplicate_normalized_paths() {
    let task = json!({"request.http_limits.max_response_bytes":2u64*1024*1024*1024,
        "request.http_limits.timeout_ms":24u64*60*60*1000});
    let runtime = http_fresh(json!({}), Some(&task), false).await.unwrap();
    let limits = runtime.template.request.http_limits.unwrap();
    assert_eq!(limits.max_response_bytes, Some(2 * 1024 * 1024 * 1024));
    assert_eq!(limits.timeout_ms, Some(24 * 60 * 60 * 1000));
    for task in [
        json!({"request.http_limits.max_response_bytes":2u64*1024*1024*1024+1}),
        json!({"request.http_limits.timeout_ms":24u64*60*60*1000+1}),
        json!({" request.http_limits.timeout_ms ":3,"request.http_limits.timeout_ms":9}),
    ] {
        assert!(http_fresh(json!({}), Some(&task), false).await.is_err());
    }
}
