use super::*;

fn default_non_text_source_ref(task_type: &str) -> &'static str {
    match task_type.trim().to_lowercase().as_str() {
        "image" | "image_translation" => {
            "https://upload.wikimedia.org/wikipedia/commons/thumb/3/3f/Fronalpstock_big.jpg/640px-Fronalpstock_big.jpg"
        }
        "audio" | "audio_translation" => {
            "https://raw.githubusercontent.com/anars/blank-audio/master/1-second-of-silence.mp3"
        }
        "video" | "video_translation" => "https://samplelib.com/lib/preview/mp4/sample-5s.mp4",
        "document" | "document_translation" => {
            "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf"
        }
        _ => "https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf",
    }
}

pub(in crate::web_ui::routes) async fn handle_local_component_test(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid /api/components/test json payload")?;
    let component_key = req
        .get("component_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if component_key.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_KEY", "component_key is required")
            .await;
    }
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
    let source_ref_input = req
        .get("source_ref")
        .or_else(|| req.get("file_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let source_payload_input = req.get("source_payload").cloned();
    let field_key = req
        .get("field_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let task_type_override = req
        .get("task_type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let (server_base, session_token_opt, bindings_doc, server_client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.component_bindings.clone(),
            guard.http_client.clone(),
        )
    };

    let binding = match bindings_doc.components.get(&component_key) {
        Some(b) => b.clone(),
        None => {
            return write_not_found_response(
                socket,
                "COMPONENT_NOT_FOUND",
                &format!("component '{}' not found in local bindings", component_key),
            )
            .await;
        }
    };

    let local_components_doc = load_local_components_runtime_doc();
    let local_component = local_components_doc.components.get(&component_key).cloned();
    let response_template_id = local_component
        .as_ref()
        .map(|comp| comp.template_id.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            binding
                .template_id
                .as_deref()
                .map(str::trim)
                .map(str::to_string)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| component_key.clone());

    // P0-LF-03 5.5: testing a LOCAL component uses its local template and
    // local key/OAuth binding and never requires website session state; the
    // session is only required by the legacy server-download branch below.
    let session_token = if local_component.is_some() {
        session_token_opt
    } else {
        let Some(session_token) = session_token_opt else {
            return write_session_required(socket).await;
        };
        Some(session_token)
    };

    let start = std::time::Instant::now();
    let result = async {
        let runtime = if local_component.is_some() {
            build_local_component_runtime_for_task(state, &component_key, None).await?
        } else {
            if !crate::config::server_control_plane_enabled() {
                // P0-LF-03 5.5: a binding without a local component is stale
                // in local mode; never download the template from the website.
                return Err(anyhow::anyhow!(
                    "COMPONENT_NOT_FOUND: component '{component_key}' has no local component record; bind a local component instead"
                ));
            }
            let session_token = session_token.expect("session required above");
            let template_id = if let Some(tid) = binding
                .template_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                tid.to_string()
            } else {
                component_key.clone()
            };
            let mut headers = HeaderMap::new();
            headers.insert("X-Client-Session", session_token.parse()?);
            let download_url = format!(
                "{}/api/v1/client/components/{}/download",
                server_base, template_id
            );
            let download_resp = request_component_download_with_passthrough(
                server_client.get(download_url).headers(headers),
                "client component download (test)",
            )
            .await?;
            let template_json = resolve_download_template_with_signing_key(
                state,
                &server_client,
                &server_base,
                &session_token,
                &download_resp.data,
                "client component download (test)",
            )
            .await?;
            let mut template: crate::types::ComponentTemplate =
                serde_json::from_value(template_json)
                    .with_context(|| "invalid component template json")?;
            crate::component_rt::loader::apply_binding_overrides_to_template(
                &mut template,
                Some(&binding),
            );
            let (
                runtime_max_concurrent_requests,
                runtime_min_interval_ms,
                runtime_concurrency_sem,
                runtime_last_request_at,
            ) = crate::component_rt::loader::runtime_limits_from_constraints(
                template.constraints.as_ref(),
            );

            let mut auth_values = HashMap::new();
            for (k, v) in &binding.auth {
                auth_values.insert(format!("auth.{}", k), v.clone());
            }
            crate::types::ComponentRuntime {
                template,
                auth_values,
                supported_business_lines: vec![],
                language_map: binding.language_map.clone(),
                supported_content_formats: vec![],
                supported_formats: vec![],
                key_pool: None,
                oauth_pool: None,
                oauth_manager: None,
                proxy_profile_id: None,
                runtime_max_concurrent_requests,
                runtime_min_interval_ms,
                runtime_concurrency_sem,
                runtime_last_request_at,
            }
        };
        let normalized_kind =
            crate::component_rt::loader::normalize_component_runtime_kind(&runtime.template.kind)
                .unwrap_or_else(|| "text".to_string());

        let api_client = Client::builder().timeout(Duration::from_secs(30)).build()?;

        if normalized_kind == "text" {
            let translated = crate::component_rt::runner::translate_text_via_component(
                &api_client,
                &runtime,
                &text,
                &source_lang,
                &target_lang,
            )
            .await?;
            Ok::<Value, anyhow::Error>(json!({
                "translated_text": translated
            }))
        } else {
            let effective_task_type = if !task_type_override.is_empty() {
                task_type_override.clone()
            } else {
                normalized_kind.clone()
            };
            let effective_source_ref = if !source_ref_input.is_empty() {
                source_ref_input.clone()
            } else {
                default_non_text_source_ref(&effective_task_type).to_string()
            };
            let source_payload = source_payload_input.clone().or_else(|| {
                if effective_source_ref.is_empty() {
                    None
                } else {
                    Some(json!({
                        "source_url": effective_source_ref,
                        "file_url": effective_source_ref,
                        "url": effective_source_ref
                    }))
                }
            });
            let outcome = crate::component_rt::runner::translate_non_text_via_component(
                &api_client,
                &runtime,
                &text,
                source_payload.as_ref(),
                &effective_source_ref,
                &effective_task_type,
                &field_key,
                &source_lang,
                &target_lang,
            )
            .await?;

            Ok::<Value, anyhow::Error>(json!({
                "translated_text": outcome.translated_text,
                "translated_ref": outcome.translated_ref,
                "task_type": effective_task_type,
                "source_ref": effective_source_ref,
            }))
        }
    }
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(outcome) => {
            {
                let mut guard = state.lock().await;
                guard.last_error.clear();
                guard.last_event = "component.test_ok".to_string();
                guard.updated_at = unix_ts();
            }
            let payload = json!({
                "success": true,
                "data": {
                    "translated_text": outcome.get("translated_text").and_then(|v| v.as_str()).unwrap_or(""),
                    "translated_ref": outcome.get("translated_ref").and_then(|v| v.as_str()),
                    "task_type": outcome.get("task_type").and_then(|v| v.as_str()),
                    "source_ref": outcome.get("source_ref").and_then(|v| v.as_str()),
                    "elapsed_ms": elapsed_ms,
                    "component_key": component_key,
                    "template_id": response_template_id
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
            {
                let mut guard = state.lock().await;
                guard.last_error = format!("{:#}", err);
                guard.last_event = "component.test_failed".to_string();
                guard.updated_at = unix_ts();
            }
            if let Some(upstream) = err.downcast_ref::<UpstreamApiError>() {
                let status = upstream.status.clone();
                let code = upstream.code.clone();
                let message = upstream.message.clone();
                return write_error_response_with_status(socket, &status, &code, &message).await;
            }
            let payload = json!({
                "success": false,
                "error": {
                    "code": "COMPONENT_TEST_FAILED",
                    "message": format!("{:#}", err)
                },
                "data": {
                    "elapsed_ms": elapsed_ms,
                    "component_key": component_key
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
    }
}

pub(in crate::web_ui::routes) async fn handle_local_component_test_file(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid /api/components/local/test-file json payload")?;

    let component_id = req
        .get("component_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if component_id.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_ID", "component_id is required")
            .await;
    }

    let file_url = req
        .get("file_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if file_url.is_empty() {
        return write_error_response(socket, "INVALID_FILE_URL", "file_url is required").await;
    }

    let source_lang = req
        .get("source_lang")
        .and_then(|v| v.as_str())
        .unwrap_or("en")
        .to_string();
    let target_lang = req
        .get("target_lang")
        .and_then(|v| v.as_str())
        .unwrap_or("zh-CN")
        .to_string();

    let doc = load_local_components_runtime_doc();
    let Some(comp) = doc.components.get(&component_id) else {
        return write_not_found_response(
            socket,
            "COMPONENT_NOT_FOUND",
            &format!("component '{}' not found", component_id),
        )
        .await;
    };
    // Quick-test is allowed while disabled: the provider wizard runs
    // install → key → quick-test → enable. Rejecting disabled components
    // would force enable-before-smoke and leave bad configs live.

    let keys_path = vendor_keys_path();
    let keys_doc = load_vendor_keys(&keys_path).unwrap_or_default();
    let mut auth_values = HashMap::new();
    let expected_vendor_id = normalized_vendor_id(&comp.vendor_id);
    let first_version = comp.versions.values().next();
    if let Some(ver) = first_version {
        for key_id in &ver.key_ids {
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
    }

    let task_type = comp.kind.clone();

    let component_bindings_doc = {
        let guard = state.lock().await;
        guard.component_bindings.clone()
    };

    let comp_id_clone = component_id.clone();
    let binding_entry = component_bindings_doc
        .components
        .get(&component_id)
        .cloned();
    let task_type_clone = task_type.clone();
    let file_url_clone = file_url.clone();
    let state_for_snapshot = Arc::clone(state);

    let start = std::time::Instant::now();
    let result = async move {
        let (comp, snapshot) =
            ensure_local_component_snapshot(&state_for_snapshot, &component_id).await?;
        let mut template: crate::types::ComponentTemplate = serde_json::from_value(snapshot)
            .with_context(|| "invalid inline component template json")?;
        crate::component_rt::loader::apply_local_component_kind_to_template(
            &mut template,
            &task_type_clone,
        );
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            comp.component_overrides.as_ref(),
        );
        crate::component_rt::loader::apply_binding_overrides_to_template(
            &mut template,
            binding_entry.as_ref(),
        );
        let (
            runtime_max_concurrent_requests,
            runtime_min_interval_ms,
            runtime_concurrency_sem,
            runtime_last_request_at,
        ) = crate::component_rt::loader::runtime_limits_from_constraints(
            template.constraints.as_ref(),
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
            proxy_profile_id: None,
            runtime_max_concurrent_requests,
            runtime_min_interval_ms,
            runtime_concurrency_sem,
            runtime_last_request_at,
        };

        let api_client = Client::builder().timeout(Duration::from_secs(60)).build()?;

        let outcome = crate::component_rt::runner::translate_non_text_via_component(
            &api_client,
            &runtime,
            "",
            None,
            &file_url_clone,
            &task_type_clone,
            "",
            &source_lang,
            &target_lang,
        )
        .await?;

        Ok::<crate::types::NonTextComponentOutcome, anyhow::Error>(outcome)
    }
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(outcome) => {
            {
                let mut guard = state.lock().await;
                guard.last_error.clear();
                guard.last_event = "component.file_test_ok".to_string();
                guard.updated_at = unix_ts();
            }
            let payload = json!({
                "success": true,
                "data": {
                    "translated_ref": outcome.translated_ref,
                    "translated_text": outcome.translated_text,
                    "elapsed_ms": elapsed_ms,
                    "component_id": comp_id_clone,
                    "task_type": task_type,
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
            {
                let mut guard = state.lock().await;
                guard.last_error = format!("{:#}", err);
                guard.last_event = "component.file_test_failed".to_string();
                guard.updated_at = unix_ts();
            }
            if let Some(result) = maybe_write_upstream_api_error(socket, &err).await {
                return result;
            }
            let payload = json!({
                "success": false,
                "error": {
                    "code": "COMPONENT_FILE_TEST_FAILED",
                    "message": format!("{:#}", err)
                },
                "data": {
                    "elapsed_ms": elapsed_ms,
                    "component_id": comp_id_clone,
                    "task_type": task_type,
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
    }
}

pub(in crate::web_ui::routes) async fn handle_local_component_quick_test(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
    comp_id: &str,
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
    // Disabled components are allowed: provider wizard runs quick-test before enable.
    let Some(ref tpl_json) = comp.template_json else {
        return write_error_response(
            socket,
            "NO_INLINE_TEMPLATE",
            "quick-test requires a local component with inline template_json (install from catalog or import a pack). Any family works: openai_compatible, http_mt, custom-http-mt.",
        )
        .await;
    };

    // Auth: prefer auth_values map; fall back to flat api_key for OpenAI-family wizards.
    let mut auth_values = HashMap::new();
    if let Some(obj) = req.get("auth_values").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            let name = k.trim().trim_start_matches("auth.");
            if name.is_empty() {
                continue;
            }
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() {
                    auth_values.insert(format!("auth.{name}"), s.trim().to_string());
                }
            }
        }
    }
    if auth_values.is_empty() {
        let api_key = req
            .get("api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !api_key.is_empty() {
            auth_values.insert("auth.api_key".to_string(), api_key);
        }
    }
    if auth_values.is_empty() {
        return write_error_response(
            socket,
            "INVALID_AUTH",
            "provide api_key or auth_values (e.g. {\"api_key\":\"…\"} or {\"app_id\":\"…\",\"secret_key\":\"…\"})",
        )
        .await;
    }

    let config_overrides = req.get("config_overrides").cloned().unwrap_or(json!({}));
    let request_overrides = build_quick_test_request_overrides(&config_overrides);
    let response_path_override = config_overrides
        .get("response.translated_text_path")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let error_path_override = config_overrides
        .get("response.error_path")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let start = std::time::Instant::now();
    let result = async {
        let mut template: crate::types::ComponentTemplate =
            serde_json::from_value(tpl_json.clone())
                .with_context(|| "invalid inline component template json")?;
        crate::component_rt::loader::apply_local_component_kind_to_template(
            &mut template,
            &comp.kind,
        );
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            comp.component_overrides.as_ref(),
        );
        let ephemeral = crate::types::ComponentInstanceOverrides {
            request_overrides,
            ..Default::default()
        };
        crate::component_rt::loader::apply_component_instance_overrides_to_template(
            &mut template,
            Some(&ephemeral),
        );
        if let Some(path) = response_path_override {
            if crate::component_rt::loader::template_allows_editable_path(
                &template,
                "response.translated_text_path",
            ) {
                template.response.translated_text_path = Some(path);
            }
        }
        if let Some(path) = error_path_override {
            if crate::component_rt::loader::template_allows_editable_path(
                &template,
                "response.error_path",
            ) {
                template.response.error_path = Some(path);
            }
        }

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

        let api_client = Client::builder().timeout(Duration::from_secs(60)).build()?;

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
                    "hint": "ok — bind this component to a content_format slot, then run worker (review_mode controls auto vs manual WP callback)"
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
            let message = format!("{:#}", err);
            let (code, hint) = classify_quick_test_error(&message);
            let payload = json!({
                "success": false,
                "error": { "code": code, "message": message, "hint": hint },
                "data": { "elapsed_ms": elapsed_ms, "component_id": comp_id }
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

fn build_quick_test_request_overrides(
    config: &Value,
) -> Option<crate::types::ComponentRequestOverrides> {
    let mut out = crate::types::ComponentRequestOverrides::default();
    let mut any = false;
    if let Some(method) = config.get("request.method").and_then(|v| v.as_str()) {
        if !method.trim().is_empty() {
            out.method = Some(method.trim().to_string());
            any = true;
        }
    }
    if let Some(url) = config
        .get("request.url")
        .or_else(|| config.get("url"))
        .and_then(|v| v.as_str())
    {
        if !url.trim().is_empty() {
            out.url = Some(url.trim().to_string());
            any = true;
        }
    }
    let mut headers = HashMap::new();
    if let Some(authz) = config
        .get("request.headers.Authorization")
        .and_then(|v| v.as_str())
    {
        if !authz.trim().is_empty() {
            headers.insert("Authorization".to_string(), authz.trim().to_string());
        }
    }
    if let Some(ct) = config
        .get("request.headers.Content-Type")
        .and_then(|v| v.as_str())
    {
        if !ct.trim().is_empty() {
            headers.insert("Content-Type".to_string(), ct.trim().to_string());
        }
    }
    if !headers.is_empty() {
        out.headers = Some(headers);
        any = true;
    }
    let mut body = serde_json::Map::new();
    if let Some(model) = config
        .get("request.body.model")
        .or_else(|| config.get("model"))
        .and_then(|v| v.as_str())
    {
        if !model.trim().is_empty() {
            body.insert("model".to_string(), json!(model.trim()));
        }
    }
    for key in ["q", "source", "target", "text", "source_lang", "target_lang"] {
        let path = format!("request.body.{key}");
        if let Some(v) = config.get(&path).and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                body.insert(key.to_string(), json!(v.trim()));
            }
        }
    }
    if !body.is_empty() {
        out.body = Some(Value::Object(body));
        any = true;
    }
    any.then_some(out)
}

fn classify_quick_test_error(message: &str) -> (&'static str, &'static str) {
    let lower = message.to_ascii_lowercase();
    if lower.contains("translated_text_path") && lower.contains("not found") {
        return (
            "RESPONSE_PATH_MISS",
            "Check response.translated_text_path against the vendor JSON (Postman-style). Mock-verified templates use paths proven against mock-api.",
        );
    }
    if lower.contains("ssrf")
        || lower.contains("provider url")
        || lower.contains("loopback")
        || lower.contains("private")
    {
        return (
            "PROVIDER_URL_BLOCKED",
            "URL rejected by egress policy (non-http(s), embedded credentials, or cloud metadata host).",
        );
    }
    if lower.contains("unauthorized") || lower.contains("401") || lower.contains("403") {
        return (
            "AUTH_REJECTED",
            "Vendor rejected credentials. Verify auth field names match the template (api_key vs app_id/secret_key) and signing family.",
        );
    }
    if lower.contains("sign") || lower.contains("signature") {
        return (
            "SIGNING_FAILED",
            "Signing algorithm/context mismatch. Prefer a mock-verified catalog template; custom sign families need Client support.",
        );
    }
    if lower.contains("timed out") || lower.contains("timeout") || lower.contains("connect") {
        return (
            "NETWORK_FAILED",
            "Cannot reach endpoint. Confirm URL, proxy profile, and that mock-api is running for mock-verified templates.",
        );
    }
    (
        "QUICK_TEST_FAILED",
        "Fix request method/url/body/auth/response path (catalog editable_params). See tasks/test/08-PROVIDER-POSTMAN-MODEL.md.",
    )
}
