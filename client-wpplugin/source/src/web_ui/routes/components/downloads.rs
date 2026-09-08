use super::*;

#[derive(Clone, Debug)]
pub(crate) struct SigningKeyMaterial {
    pub(crate) pem: String,
    pub(crate) key_id: Option<String>,
}

async fn read_signing_key_from_db(
    state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<Option<SigningKeyMaterial>> {
    let db_arc = {
        let guard = state.lock().await;
        Arc::clone(&guard.db)
    };
    let conn = db_arc.lock().await;
    let Some(pem) = crate::db::system::get_signing_key(&conn) else {
        return Ok(None);
    };
    if !pem.trim().starts_with("-----BEGIN PUBLIC KEY-----") {
        return Ok(None);
    }
    let key_id = crate::db::system::get_signing_key_id(&conn)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Ok(Some(SigningKeyMaterial { pem, key_id }))
}

async fn persist_signing_key_to_db(
    state: &Arc<Mutex<WebUiState>>,
    key: &SigningKeyMaterial,
) -> anyhow::Result<()> {
    let db_arc = {
        let guard = state.lock().await;
        Arc::clone(&guard.db)
    };
    let conn = db_arc.lock().await;
    crate::db::system::set_signing_key_material(&conn, &key.pem, key.key_id.as_deref())?;
    Ok(())
}

pub(crate) async fn persist_signing_key_material(
    state: &Arc<Mutex<WebUiState>>,
    key: &SigningKeyMaterial,
) -> anyhow::Result<()> {
    persist_signing_key_to_db(state, key).await
}

pub(crate) async fn fetch_signing_key_from_server(
    client: &Client,
    server_base: &str,
    session_token: &str,
) -> anyhow::Result<SigningKeyMaterial> {
    let key_url = format!("{}/api/v1/client/signing-public-key", server_base);
    let resp: ApiResponse<Value> = request_json_encrypted(
        client
            .get(&key_url)
            .header("X-Client-Session", session_token),
        "signing-public-key",
        session_token,
    )
    .await?;
    if !resp.success {
        anyhow::bail!("signing-public-key response is success=false");
    }
    let pem = resp
        .data
        .get("public_key_pem")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| v.starts_with("-----BEGIN PUBLIC KEY-----"))
        .ok_or_else(|| anyhow!("invalid signing-public-key response: missing public_key_pem"))?
        .to_string();
    let key_id = resp
        .data
        .get("key_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string);
    Ok(SigningKeyMaterial { pem, key_id })
}

pub(crate) fn is_signature_related_error(err: &anyhow::Error) -> bool {
    let msg = format!("{:#}", err).to_lowercase();
    msg.contains("signature")
        || msg.contains("signing public key")
        || msg.contains("trusted public key")
}

fn needs_signing_key_refresh(
    current: Option<&SigningKeyMaterial>,
    download_data: &ComponentDownloadData,
) -> bool {
    if current.is_none() {
        return true;
    }
    let Some(download_key_id) = download_data
        .signing_key_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return false;
    };
    let local_key_id = current.and_then(|k| k.key_id.as_deref());
    local_key_id != Some(download_key_id)
}

pub(crate) async fn resolve_download_template_with_signing_key(
    state: &Arc<Mutex<WebUiState>>,
    client: &Client,
    server_base: &str,
    session_token: &str,
    download_data: &ComponentDownloadData,
    op_label: &str,
) -> anyhow::Result<Value> {
    let mut current_key = read_signing_key_from_db(state).await?;

    if needs_signing_key_refresh(current_key.as_ref(), download_data) {
        let fetched = fetch_signing_key_from_server(client, server_base, session_token)
            .await
            .with_context(|| format!("{op_label}: fetch signing key before verify"))?;
        persist_signing_key_to_db(state, &fetched).await?;
        current_key = Some(fetched);
    }

    match resolve_download_template_json(
        download_data,
        session_token,
        current_key.as_ref().map(|k| k.pem.as_str()),
    ) {
        Ok(v) => Ok(v),
        Err(first_err) if is_signature_related_error(&first_err) => {
            let fetched = fetch_signing_key_from_server(client, server_base, session_token)
                .await
                .with_context(|| format!("{op_label}: refresh signing key after verify failed"))?;
            persist_signing_key_to_db(state, &fetched).await?;
            resolve_download_template_json(download_data, session_token, Some(&fetched.pem))
                .with_context(|| format!("{op_label}: verify/decrypt failed after key refresh"))
        }
        Err(err) => Err(err),
    }
}

pub(crate) async fn handle_components_template(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: WebUiComponentTemplateRequest = serde_json::from_slice(body)
        .with_context(|| "invalid /api/components/template json payload")?;
    let component_id = req.component_id.unwrap_or_default().trim().to_string();
    if component_id.is_empty() {
        return write_error_response(socket, "INVALID_COMPONENT_ID", "component_id is required")
            .await;
    }
    let (server_base, session_token_opt, client) = {
        let guard = state.lock().await;
        (
            guard.server_base.clone(),
            guard.session_token.clone(),
            guard.http_client.clone(),
        )
    };
    let Some(session_token) = session_token_opt else {
        return write_session_required(socket).await;
    };

    let result = async {
        let mut headers = HeaderMap::new();
        headers.insert("X-Client-Session", session_token.parse()?);
        let download_url = format!(
            "{}/api/v1/client/components/{}/download",
            server_base, component_id
        );
        let download_resp = request_component_download_with_passthrough(
            client.get(download_url).headers(headers),
            "client component download (web_ui)",
        )
        .await?;
        let template_json = resolve_download_template_with_signing_key(
            state,
            &client,
            &server_base,
            &session_token,
            &download_resp.data,
            "client component download (web_ui)",
        )
        .await?;
        let template: ComponentTemplate = serde_json::from_value(template_json.clone())
            .with_context(|| "invalid component template json".to_string())?;
        let auth_modes = template
            .auth
            .as_ref()
            .map(extract_template_auth_modes)
            .unwrap_or_else(|| vec!["none".to_string()]);
        let auth_fields = template
            .auth
            .as_ref()
            .map(|auth| {
                auth.fields
                    .iter()
                    .map(|field| {
                        json!({
                            "name": field.name,
                            "required": field.required.unwrap_or(false)
                        })
                    })
                    .collect::<Vec<Value>>()
            })
            .unwrap_or_default();
        Ok::<Value, anyhow::Error>(json!({
            "component_id": template.id,
            "template_name": template.name,
            "template_version": template.version,
            "template_type": template.kind,
            "auth_modes": auth_modes,
            "auth_fields": auth_fields,
            "template_json": template_json
        }))
    }
    .await;

    match result {
        Ok(data) => {
            {
                let mut guard = state.lock().await;
                guard.last_error.clear();
                guard.last_event = "components.template_loaded".to_string();
                guard.updated_at = unix_ts();
            }
            let payload = json!({ "success": true, "data": data });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            update_state_error(state, &err, "components.template_load_failed").await;
            if let Some(upstream) = err.downcast_ref::<UpstreamApiError>() {
                let status = upstream.status.clone();
                let code = upstream.code.clone();
                let message = upstream.message.clone();
                return write_error_response_with_status(socket, &status, &code, &message).await;
            }
            write_error_response(
                socket,
                "COMPONENT_TEMPLATE_LOAD_FAILED",
                &format!("{:#}", err),
            )
            .await
        }
    }
}

fn extract_template_auth_modes(auth: &crate::types::ComponentAuth) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(modes) = auth.modes.as_ref() {
        for mode in modes {
            let normalized = mode.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "key" | "oauth" | "none") && !out.contains(&normalized)
            {
                out.push(normalized);
            }
        }
    }
    if out.is_empty() {
        if let Some(mode) = auth.mode.as_ref() {
            let normalized = mode.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "key" | "oauth" | "none") {
                out.push(normalized);
            }
        }
    }
    if out.is_empty() {
        if auth.fields.is_empty() {
            out.push("none".to_string());
        } else {
            out.push("key".to_string());
        }
    }
    out
}

pub(crate) async fn request_component_download_with_passthrough(
    builder: reqwest::RequestBuilder,
    label: &str,
) -> anyhow::Result<ApiResponse<ComponentDownloadData>> {
    let request_id = build_request_id(label);
    let response = builder
        .header(REQUEST_ID_HEADER, request_id)
        .send()
        .await
        .with_context(|| format!("{}: request failed", label))?;
    let status = response.status();
    let url = response.url().to_string();
    let body = response
        .text()
        .await
        .with_context(|| format!("{}: read body failed ({})", label, url))?;

    if body.trim().is_empty() {
        return Err(anyhow!(
            "{}: empty response body (status={}, url={})",
            label,
            status,
            url
        ));
    }

    if let Some(api_error) = parse_api_error_response(&body) {
        return Err(UpstreamApiError {
            status: http_status_line(status),
            code: api_error.error.code,
            message: api_error.error.message,
        }
        .into());
    }

    serde_json::from_str::<ApiResponse<ComponentDownloadData>>(&body).with_context(|| {
        let snippet = if body.len() > 220 {
            format!("{}...", &body[..body.floor_char_boundary(220)])
        } else {
            body.clone()
        };
        format!(
            "{}: invalid json response (status={}, url={}, body={})",
            label, status, url, snippet
        )
    })
}
