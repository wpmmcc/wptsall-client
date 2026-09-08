use super::*;

pub(in crate::web_ui::routes) async fn handle_component_version_create(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    comp_id: &str,
) -> anyhow::Result<()> {
    let req: Value =
        serde_json::from_slice(body).with_context(|| "invalid POST versions json payload")?;
    let version = req
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if version.is_empty() {
        return write_error_response(socket, "INVALID_VERSION", "version is required").await;
    }
    let mut doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get_mut(comp_id) else {
        return write_not_found_response(socket, "NOT_FOUND", "component not found").await;
    };
    if comp.versions.len() >= 10 {
        return write_error_response(socket, "MAX_VERSIONS", "maximum 10 versions per component")
            .await;
    }
    if comp.versions.contains_key(&version) {
        return write_conflict_response(socket, "DUPLICATE_VERSION", "version already exists")
            .await;
    }
    let key_ids: Vec<String> = req
        .get("key_ids")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let vendor_keys_doc = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
    if let Err(err) = validate_component_binding_vendor_alignment(
        comp_id,
        normalized_vendor_id(&comp.vendor_id).as_deref(),
        &key_ids,
        &[] as &[String],
        &vendor_keys_doc,
        &VendorOAuthDoc::default(),
    ) {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INVALID_COMPONENT_VERSION_KEYS",
            &format!("{:#}", err),
        )
        .await;
    }
    let key_selection_strategy = req
        .get("key_selection_strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("round_robin")
        .to_string();
    let proxy_profile_id = req
        .get("proxy_profile_id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let auth_type = req
        .get("auth_type")
        .and_then(|v| v.as_str())
        .unwrap_or("key")
        .to_string();
    let remarks = req
        .get("remarks")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let config_overrides: HashMap<String, serde_json::Value> = req
        .get("config_overrides")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let now = format!("{}", unix_ts());
    comp.versions.insert(
        version.clone(),
        crate::types::ComponentVersion {
            version: version.clone(),
            remarks,
            key_ids,
            key_selection_strategy,
            proxy_profile_id,
            auth_type,
            config_overrides,
            created_at: now,
        },
    );
    if comp
        .active_version
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        comp.active_version = Some(version.clone());
    }
    save_local_components_runtime_doc(&doc)?;
    let payload =
        json!({ "success": true, "data": { "component_id": comp_id, "version": version } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_component_version_update(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    comp_id: &str,
    ver: &str,
) -> anyhow::Result<()> {
    let req: Value =
        serde_json::from_slice(body).with_context(|| "invalid PUT version json payload")?;
    let mut doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get_mut(comp_id) else {
        return write_not_found_response(socket, "NOT_FOUND", "component not found").await;
    };
    let Some(version_entry) = comp.versions.get_mut(ver) else {
        return write_not_found_response(socket, "NOT_FOUND", "version not found").await;
    };
    if let Some(remarks) = req.get("remarks").and_then(|v| v.as_str()) {
        version_entry.remarks = remarks.to_string();
    }
    if let Some(key_ids) = req.get("key_ids") {
        if let Ok(ids) = serde_json::from_value::<Vec<String>>(key_ids.clone()) {
            let vendor_keys_doc = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
            if let Err(err) = validate_component_binding_vendor_alignment(
                comp_id,
                normalized_vendor_id(&comp.vendor_id).as_deref(),
                &ids,
                &[] as &[String],
                &vendor_keys_doc,
                &VendorOAuthDoc::default(),
            ) {
                return write_error_response_with_status(
                    socket,
                    "422 Unprocessable Entity",
                    "INVALID_COMPONENT_VERSION_KEYS",
                    &format!("{:#}", err),
                )
                .await;
            }
            version_entry.key_ids = ids;
        }
    }
    if let Some(strategy) = req.get("key_selection_strategy").and_then(|v| v.as_str()) {
        version_entry.key_selection_strategy = strategy.to_string();
    }
    if let Some(proxy_id) = req.get("proxy_profile_id") {
        version_entry.proxy_profile_id = proxy_id
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    if let Some(auth_type) = req.get("auth_type").and_then(|v| v.as_str()) {
        version_entry.auth_type = auth_type.to_string();
    }
    if let Some(overrides) = req.get("config_overrides") {
        if let Ok(m) =
            serde_json::from_value::<HashMap<String, serde_json::Value>>(overrides.clone())
        {
            version_entry.config_overrides = m;
        }
    }
    if comp
        .active_version
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        comp.active_version = Some(ver.to_string());
    }
    save_local_components_runtime_doc(&doc)?;
    let payload = json!({ "success": true, "data": { "component_id": comp_id, "version": ver } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_component_version_delete(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    comp_id: &str,
    ver: &str,
) -> anyhow::Result<()> {
    let mut doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get_mut(comp_id) else {
        return write_not_found_response(socket, "NOT_FOUND", "component not found").await;
    };
    let removed = comp.versions.remove(ver).is_some();
    if removed && comp.active_version.as_deref() == Some(ver) {
        comp.active_version = comp
            .versions
            .iter()
            .max_by(|(left_id, left), (right_id, right)| {
                left.created_at
                    .cmp(&right.created_at)
                    .then(left_id.cmp(right_id))
            })
            .map(|(version_id, _)| version_id.clone());
    }
    save_local_components_runtime_doc(&doc)?;
    let payload = json!({ "success": true, "data": { "component_id": comp_id, "version": ver, "deleted": removed } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(in crate::web_ui::routes) async fn handle_component_version_test(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    comp_id: &str,
    ver: &str,
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let text = req
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Hello World")
        .to_string();
    let source_lang = req
        .get("source_lang")
        .and_then(|v| v.as_str())
        .unwrap_or("en_US")
        .to_string();
    let target_lang = req
        .get("target_lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh_CN")
        .to_string();

    let doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get(comp_id) else {
        return write_not_found_response(socket, "NOT_FOUND", "component not found").await;
    };
    if !comp.enabled {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "COMPONENT_DISABLED",
            "component is disabled",
        )
        .await;
    }
    {
        // P0-LF-03 5.5: version validation uses LOCAL data only in local
        // mode; the server component cache is never consulted.
        let cached_server_components = if crate::config::server_control_plane_enabled() {
            let guard = state.lock().await;
            guard.components.clone()
        } else {
            Vec::new()
        };
        if let Err(err) = validate_local_component_api_version_with_cached_components(
            comp,
            &cached_server_components,
        ) {
            if let Some(result) = maybe_write_upstream_api_error(socket, &err).await {
                return result;
            }
            return write_error_response(
                socket,
                "COMPONENT_API_VERSION_UNSUPPORTED",
                &format!("{:#}", err),
            )
            .await;
        }
    }
    let Some(version_entry) = comp.versions.get(ver) else {
        return write_not_found_response(socket, "NOT_FOUND", "version not found").await;
    };

    let keys_path = vendor_keys_path();
    let keys_doc = load_vendor_keys(&keys_path).unwrap_or_default();
    let mut auth_values = HashMap::new();
    let expected_vendor_id = normalized_vendor_id(&comp.vendor_id);
    for key_id in &version_entry.key_ids {
        if let Some(vendor_key) = keys_doc.keys.get(key_id) {
            if vendor_key.enabled
                && expected_vendor_id
                    .as_deref()
                    .map(|expected| vendor_key.vendor_id.trim() == expected)
                    .unwrap_or(true)
            {
                for (k, v) in &vendor_key.auth_values {
                    auth_values.insert(format!("auth.{}", k), v.clone());
                }
                break;
            }
        }
    }

    let start = std::time::Instant::now();
    let result = async {
        let (comp, snapshot) = ensure_local_component_snapshot(state, comp_id).await?;
        let mut template: crate::types::ComponentTemplate = serde_json::from_value(snapshot)
            .with_context(|| "invalid inline component template json")?;
        crate::component_rt::loader::apply_local_component_kind_to_template(
            &mut template,
            &comp.kind,
        );
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            comp.component_overrides.as_ref(),
        );

        let runtime = crate::types::ComponentRuntime {
            template,
            auth_values,
            supported_business_lines: vec![],
            language_map: HashMap::new(),
            supported_content_formats: vec![],
            supported_formats: vec![],
            key_pool: None,
            oauth_pool: None,
            oauth_manager: None,
            runtime_max_concurrent_requests: 0,
            runtime_min_interval_ms: 0,
            runtime_concurrency_sem: None,
            runtime_last_request_at: None,
            proxy_profile_id: None,
        };

        let api_client = if let Some(ref proxy_id) = version_entry.proxy_profile_id {
            let proxy_path = proxy_profiles_path();
            let proxy_doc = load_proxy_profiles(&proxy_path).unwrap_or_default();
            if let Some(proxy) = proxy_doc.profiles.get(proxy_id) {
                crate::component_rt::proxy::validate_proxy_profile(proxy)
                    .with_context(|| format!("invalid proxy profile '{}'", proxy_id))?;
                let proxy_url = format!("{}://{}:{}", proxy.protocol, proxy.host, proxy.port);
                let mut builder = Client::builder().timeout(Duration::from_secs(30));
                let rp = reqwest::Proxy::all(&proxy_url)?;
                let username =
                    crate::component_rt::runner::resolve_credential_reference(&proxy.username)
                        .with_context(|| format!("resolve proxy username '{}' failed", proxy_id))?;
                let password =
                    crate::component_rt::runner::resolve_credential_reference(&proxy.password)
                        .with_context(|| format!("resolve proxy password '{}' failed", proxy_id))?;
                let rp = if !username.is_empty() {
                    rp.basic_auth(&username, &password)
                } else {
                    rp
                };
                builder = builder.proxy(rp);
                builder.build()?
            } else {
                Client::builder().timeout(Duration::from_secs(30)).build()?
            }
        } else {
            Client::builder().timeout(Duration::from_secs(30)).build()?
        };

        let translated = crate::component_rt::runner::translate_text_via_component(
            &api_client,
            &runtime,
            &text,
            &source_lang,
            &target_lang,
        )
        .await?;
        Ok::<String, anyhow::Error>(translated)
    }
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(translated_text) => {
            let payload = json!({
                "success": true,
                "data": {
                    "translated_text": translated_text,
                    "elapsed_ms": elapsed_ms,
                    "component_id": comp_id,
                    "version": ver,
                }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            if let Some(result) = maybe_write_upstream_api_error(socket, &err).await {
                return result;
            }
            let payload = json!({
                "success": false,
                "error": { "code": "VERSION_TEST_FAILED", "message": format!("{:#}", err) },
                "data": { "elapsed_ms": elapsed_ms, "component_id": comp_id, "version": ver }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
    }
}
