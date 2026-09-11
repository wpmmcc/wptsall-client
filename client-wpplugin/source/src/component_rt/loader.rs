use anyhow::{anyhow, Context};
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::auth::{request_json, request_json_encrypted, UpstreamApiError};
use crate::bindings::{
    load_proxy_profiles, load_vendor_keys, load_vendor_oauth, parse_business_line_key,
    save_component_bindings, save_components_local,
};
use crate::component_rt::key_pool::KeyPool;
use crate::component_rt::key_registry::GlobalKeyRegistry;
use crate::component_rt::oauth::OAuthTokenManager;
use crate::component_rt::proxy::ProxyClientPool;
use crate::component_rt::runner::resolve_auth_values;
use crate::crypto::{resolve_download_template_json, verify_component_signature};
use crate::logging::log_event;
use crate::types::*;

const MAX_SUPPORTED_COMPONENT_API_MAJOR: u64 = 2;
const REVOCATION_CATALOG_VERSION: &str = "component-revocation-v1";
const REVOCATION_SIGNATURE_SCOPE: &str = "component-revocation-json-v1";
type VendorPoolKeyTuple = (String, HashMap<String, String>, usize, u32);
type ComponentBindingKeyTuple = (String, HashMap<String, String>, usize, u32, usize, f64, f64);
type RuntimeLimits = (
    u32,
    u64,
    Option<Arc<tokio::sync::Semaphore>>,
    Option<Arc<tokio::sync::Mutex<Instant>>>,
);

fn web_ui_sqlite_storage_enabled() -> bool {
    std::env::var("WPTSALL_WEB_UI")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

fn load_local_components_runtime_doc(components_local_path: &str) -> ComponentsLocalDoc {
    if web_ui_sqlite_storage_enabled() {
        crate::db::components::load_runtime_local_components_doc()
    } else {
        crate::bindings::load_components_local(components_local_path).unwrap_or_default()
    }
}

fn save_local_components_runtime_doc(
    components_local_path: &str,
    doc: &ComponentsLocalDoc,
) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::components::save_runtime_local_components_doc(doc)
    } else {
        save_components_local(components_local_path, doc)
    }
}

fn save_component_bindings_runtime_doc(
    component_bindings_path: &str,
    doc: &ComponentBindingsDoc,
) -> anyhow::Result<()> {
    if web_ui_sqlite_storage_enabled() {
        crate::db::bindings::save_runtime_component_bindings_doc(doc)
    } else {
        save_component_bindings(component_bindings_path, doc)
    }
}

/// Verify the signed component tombstones carried alongside the encrypted
/// catalog.  Older servers do not advertise this catalog and remain readable;
/// once a server advertises v1, an unsigned or malformed list is fail-closed.
fn verified_revoked_component_ids(
    data: &ComponentsData,
    trusted_public_key: Option<&str>,
    skip_signature_check: bool,
) -> anyhow::Result<BTreeSet<String>> {
    let Some(version) = data.revocation_catalog_version.as_deref() else {
        return Ok(BTreeSet::new());
    };
    if version != REVOCATION_CATALOG_VERSION {
        anyhow::bail!("unsupported component revocation catalog version: {version}");
    }
    if data.revocation_signature_scope.as_deref() != Some(REVOCATION_SIGNATURE_SCOPE)
        || data.revocation_signature_contract_version.as_deref()
            != Some(client_runtime_core::signed_catalog::CATALOG_SIGNATURE_CONTRACT)
        || data.revocation_signature_algorithm.as_deref()
            != Some(client_runtime_core::signed_catalog::CATALOG_SIGNATURE_ALGORITHM)
        || data
            .revocation_signing_key_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        anyhow::bail!("component revocation catalog has invalid signature metadata");
    }

    let signature = data
        .revocation_signature
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (signature, trusted_public_key) {
        (Some(signature), Some(public_key)) => {
            let payload = serde_json::to_vec(&data.revocations)
                .context("serialize component revocation catalog for verification")?;
            verify_component_signature(&payload, signature, public_key)
                .context("component revocation catalog signature verification failed")?;
        }
        (_, _) if skip_signature_check => {
            eprintln!(
                "[WARN] WPTSALL_SKIP_SIGNATURE_CHECK=true — accepting component revocation catalog without signature verification (development only!)"
            );
        }
        (None, _) => anyhow::bail!("component revocation catalog signature is missing"),
        (Some(_), None) => anyhow::bail!(
            "component revocation catalog is signed but no trusted public key is configured"
        ),
    }

    let mut ids = BTreeSet::new();
    for revocation in &data.revocations {
        let component_id = revocation.component_id.trim();
        if component_id.is_empty()
            || revocation.version.trim().is_empty()
            || revocation.content_digest.len() != 64
            || !revocation
                .content_digest
                .chars()
                .all(|ch| ch.is_ascii_hexdigit())
            || revocation.key_id.trim().is_empty()
            || revocation.revoked_at.trim().is_empty()
        {
            anyhow::bail!("component revocation catalog contains an invalid tombstone");
        }
        ids.insert(component_id.to_string());
    }
    Ok(ids)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn load_component_runtimes(
    client: &Client,
    server_base: &str,
    session_token: &str,
    log_file: &str,
    component_bindings: &mut ComponentBindingsDoc,
    component_bindings_path: &str,
    target_component_ids: Option<&BTreeSet<String>>,
    // Pre-loaded signing key (e.g. from DB). When Some, takes priority over
    // the on-disk signing-public-key.pem file. Pass None from CLI mode so
    // the existing file-based path is used as before.
    signing_key_override: Option<&str>,
) -> anyhow::Result<ComponentRuntimeRegistry> {
    let components_local_path = crate::config::components_local_file();
    let mut local_doc = load_local_components_runtime_doc(&components_local_path);

    let server_target_ids: Option<BTreeSet<String>> = target_component_ids.map(|ids| {
        ids.iter()
            .filter_map(|component_id| {
                if let Some(local_component) = local_doc.components.get(component_id) {
                    let template_id = local_component.template_id.trim();
                    if template_id.is_empty() || local_component.template_json.is_some() {
                        None
                    } else {
                        Some(template_id.to_string())
                    }
                } else {
                    Some(component_id.clone())
                }
            })
            .collect()
    });
    // Default standalone mode is local-first: if every selected component has
    // an inline local template_json, do not contact the official catalog. Server
    // fetches remain available only when a selected item still needs a template
    // alias download / revocation check.
    let should_fetch_server_components = target_component_ids.is_none_or(|ids| {
        if ids.is_empty() {
            return false;
        }
        ids.iter().any(|component_id| {
            local_doc
                .components
                .get(component_id)
                .map(|local_component| {
                    local_component.template_json.is_none()
                        && !local_component
                            .kind
                            .eq_ignore_ascii_case("openai_compatible")
                })
                .unwrap_or(true)
        })
    });

    // Load signing public key only when we actually need the server catalog.
    // Pure local templates are user-owned local runtime objects and must not be
    // blocked by missing official signing keys.
    let skip_sig_check = std::env::var("WPTSALL_SKIP_SIGNATURE_CHECK")
        .ok()
        .as_deref()
        == Some("true");
    let trusted_public_key: Option<String> = if should_fetch_server_components {
        if let Some(pem) = signing_key_override {
            log_event(
                log_file,
                "info",
                "component.signing_key_loaded",
                json!({ "source": "db" }),
            )
            .ok();
            Some(pem.to_string())
        } else {
            let config_dir = Path::new(component_bindings_path)
                .parent()
                .unwrap_or_else(|| Path::new("."));
            let signing_pub_key_path = config_dir.join("signing-public-key.pem");
            match std::fs::read_to_string(&signing_pub_key_path) {
                Ok(pem) if pem.trim().starts_with("-----BEGIN PUBLIC KEY-----") => {
                    log_event(
                        log_file,
                        "info",
                        "component.signing_key_loaded",
                        json!({ "source": "file", "path": signing_pub_key_path.display().to_string() }),
                    )
                    .ok();
                    Some(pem)
                }
                _ => {
                    if skip_sig_check {
                        log_event(log_file, "warn", "component.signing_key_not_found",
                            json!({ "path": signing_pub_key_path.display().to_string(),
                                    "note": "WPTSALL_SKIP_SIGNATURE_CHECK=true — bypassed (dev only)" })).ok();
                        eprintln!("[WARN] signing-public-key.pem not found at {}. WPTSALL_SKIP_SIGNATURE_CHECK=true — bypassing (development only!)", signing_pub_key_path.display());
                        None
                    } else {
                        log_event(
                            log_file,
                            "error",
                            "component.signing_key_missing",
                            json!({ "path": signing_pub_key_path.display().to_string() }),
                        )
                        .ok();
                        return Err(anyhow!(
                            "signing-public-key.pem not found at {}. Component signature verification is required. \
                             Set WPTSALL_SKIP_SIGNATURE_CHECK=true to bypass (development only).",
                            signing_pub_key_path.display()
                        ));
                    }
                }
            }
        }
    } else {
        log_event(
            log_file,
            "info",
            "component.server_catalog_skipped",
            json!({ "reason": "local_template_catalog" }),
        )
        .ok();
        None
    };
    let trusted_pub_key_ref = trusted_public_key.as_deref();

    // Load vendor keys and oauth configs for per-component pool building
    let config_dir = std::path::Path::new(component_bindings_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_string_lossy()
        .to_string();
    let vk_path = format!("{}/vendor-keys.json", config_dir);
    let vo_path = format!("{}/vendor-oauth.json", config_dir);
    let vendor_keys_doc = crate::bindings::load_vendor_keys(&vk_path).unwrap_or_default();
    let vendor_oauth_doc = crate::bindings::load_vendor_oauth(&vo_path).unwrap_or_default();
    let oauth_http_client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap_or_default();
    let oauth_manager_arc: Option<std::sync::Arc<crate::component_rt::oauth::OAuthTokenManager>> =
        if !vendor_oauth_doc.configs.is_empty() {
            Some(std::sync::Arc::new(
                crate::component_rt::oauth::OAuthTokenManager::new(
                    vendor_oauth_doc.configs.clone(),
                    oauth_http_client,
                    vo_path,
                ),
            ))
        } else {
            None
        };

    let mut list_items: Vec<ComponentItem> = Vec::new();
    let mut revoked_component_ids = BTreeSet::new();
    let mut headers = HeaderMap::new();
    if should_fetch_server_components {
        let session_header = session_token.trim();
        if session_header.is_empty() {
            return Err(anyhow!(
                "SESSION_REQUIRED: server component catalog requested but session_token is empty"
            ));
        }
        headers.insert("X-Client-Session", session_header.parse()?);
        let mut page: usize = 1;
        let per_page: usize = 200;
        let mut total_pages: usize = 1;

        loop {
            let list_url = format!(
                "{}/api/v1/client/components?page={}&per_page={}&client_type=wpplugin",
                server_base, page, per_page
            );
            let list_raw: Value = request_json_encrypted(
                client.get(list_url).headers(headers.clone()),
                "client components",
                session_token,
            )
            .await?;

            let page_data = if let Ok(resp) =
                serde_json::from_value::<ApiResponse<ComponentsData>>(list_raw.clone())
            {
                if !resp.success {
                    return Err(anyhow!("client components: server returned success=false"));
                } else {
                    resp.data
                }
            } else if let Ok(data) = serde_json::from_value::<ComponentsData>(list_raw.clone()) {
                data
            } else {
                return Err(anyhow!(
                    "client components: unsupported response shape ({})",
                    list_raw
                ));
            };

            revoked_component_ids.extend(verified_revoked_component_ids(
                &page_data,
                trusted_pub_key_ref,
                skip_sig_check,
            )?);
            let page_total_pages = page_data.total_pages.unwrap_or(1).max(1);
            let mut page_items = page_data.items;

            list_items.append(&mut page_items);
            total_pages = total_pages.max(page_total_pages);
            if page >= total_pages {
                break;
            }
            page += 1;
        }

        if list_items.is_empty() && revoked_component_ids.is_empty() {
            return Err(anyhow!("component list is empty"));
        }
        list_items.sort_by(|a, b| a.id.cmp(&b.id));
        list_items.dedup_by(|a, b| a.id == b.id);
        list_items.retain(|item| !revoked_component_ids.contains(&item.id));
        if let Some(server_target_ids) = server_target_ids.as_ref() {
            list_items.retain(|item| server_target_ids.contains(&item.id));
        }
    }

    let mut locally_revoked_count = 0usize;
    if !revoked_component_ids.is_empty() {
        for (local_component_id, component) in local_doc.components.iter_mut() {
            let template_id = component.template_id.trim().to_string();
            if revoked_component_ids.contains(local_component_id)
                || (!template_id.is_empty() && revoked_component_ids.contains(&template_id))
            {
                component.enabled = false;
                component.template_json = None;
                component.updated_at = Some(format!("{}", crate::logging::unix_ts()));
                locally_revoked_count += 1;
                let _ = log_event(
                    log_file,
                    "warning",
                    "component.local_snapshot_revoked",
                    json!({
                        "component_id": local_component_id,
                        "template_id": template_id,
                    }),
                );
            }
        }
        if locally_revoked_count > 0 {
            save_local_components_runtime_doc(&components_local_path, &local_doc)?;
        }
    }

    let mut runtimes = HashMap::new();
    let mut ordered_ids = Vec::new();
    let mut bindings_dirty = false;
    let mut last_upstream_error: Option<UpstreamApiError> = None;

    for item in &list_items {
        let Some(normalized_kind) = normalize_component_runtime_kind(&item.kind) else {
            continue;
        };
        if !is_component_api_version_compatible(item.api_version.as_deref()) {
            let _ = log_event(
                log_file,
                "warning",
                "component.api_version_incompatible",
                json!({
                    "component_id": item.id,
                    "api_version": item.api_version,
                    "max_supported_major": MAX_SUPPORTED_COMPONENT_API_MAJOR
                }),
            );
            continue;
        }
        if !component_kind_matches_supported_types(&normalized_kind, &item.supported_types) {
            let _ = log_event(
                log_file,
                "warning",
                "component.supported_types_mismatch",
                json!({
                    "component_id": item.id,
                    "kind": item.kind,
                    "normalized_kind": normalized_kind,
                    "supported_types": item.supported_types
                }),
            );
            continue;
        }

        let download_url = format!(
            "{}/api/v1/client/components/{}/download",
            server_base, item.id
        );
        let download_resp: ApiResponse<ComponentDownloadData> = match request_json(
            client.get(download_url).headers(headers.clone()),
            "client component download",
        )
        .await
        {
            Ok(resp) => resp,
            Err(err) => {
                if let Some(upstream) = err.downcast_ref::<UpstreamApiError>() {
                    last_upstream_error = Some(upstream.clone());
                }
                let _ = log_event(
                    log_file,
                    "warning",
                    "component.download_failed",
                    json!({
                        "component_id": item.id,
                        "reason": format!("{:#}", err)
                    }),
                );
                continue;
            }
        };

        if !download_resp.success {
            continue;
        }
        if download_resp.data.component_id != item.id {
            continue;
        }

        let template_json = match resolve_download_template_json(
            &download_resp.data,
            session_token,
            trusted_pub_key_ref,
        ) {
            Ok(tj) => tj,
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "component.template_decode_failed",
                    json!({
                        "component_id": item.id,
                        "reason": format!("{:#}", err)
                    }),
                );
                eprintln!(
                    "[WARN] Skipping component {} — template decode failed: {}",
                    item.id, err
                );
                continue;
            }
        };
        let mut template: ComponentTemplate = match serde_json::from_value(template_json) {
            Ok(t) => t,
            Err(err) => {
                let _ = log_event(
                    log_file,
                    "warning",
                    "component.template_parse_failed",
                    json!({
                        "component_id": item.id,
                        "reason": format!("{:#}", err)
                    }),
                );
                eprintln!(
                    "[WARN] Skipping component {} — invalid template JSON: {}",
                    item.id, err
                );
                continue;
            }
        };

        let binding_entry = component_bindings.components.get(&template.id).cloned();
        let has_pooled_auth = binding_entry
            .as_ref()
            .map(|entry| !entry.key_ids.is_empty() || !entry.oauth_ids.is_empty())
            .unwrap_or(false);
        let (auth_values, updated) =
            match resolve_auth_values(&template.id, template.auth.as_ref(), component_bindings) {
                Ok(result) => result,
                Err(err) if has_pooled_auth => {
                    let _ = log_event(
                        log_file,
                        "info",
                        "component.auth_deferred_to_pool",
                        json!({
                            "component_id": template.id,
                            "reason": format!("{:#}", err)
                        }),
                    );
                    (HashMap::new(), false)
                }
                Err(err) => {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "component.auth_skipped",
                        json!({
                            "component_id": template.id,
                            "reason": format!("{:#}", err)
                        }),
                    );
                    eprintln!("[WARN] Skipping component {} — {}", template.id, err);
                    continue;
                }
            };
        bindings_dirty = bindings_dirty || updated;
        let _ = log_event(
            log_file,
            "info",
            "component.runtime_ready",
            json!({
                "component_id": template.id,
                "component_name": template.name,
                "component_version": template.version,
                "component_type": template.kind,
                "auth_fields_count": auth_values.len()
            }),
        );

        let language_map = component_bindings
            .components
            .get(&template.id)
            .map(|entry| entry.language_map.clone())
            .unwrap_or_default();

        apply_binding_overrides_to_template(&mut template, binding_entry.as_ref());

        // Build per-component key pool from binding.key_ids
        let (comp_key_pool, comp_oauth_pool) = build_component_pools(
            &template.id,
            item.vendor_id.as_deref(),
            binding_entry.as_ref(),
            &vendor_keys_doc,
            &vendor_oauth_doc,
        )?;

        // Resolve supported_content_formats: prefer list-level (from Server) then template constraints
        let supported_content_formats = if !item.supported_content_formats.is_empty() {
            item.supported_content_formats.clone()
        } else {
            template
                .constraints
                .as_ref()
                .and_then(|c| c.supported_content_formats.clone())
                .unwrap_or_default()
        };
        // Resolve supported_formats similarly
        let supported_formats = if !item.supported_formats.is_empty() {
            item.supported_formats.clone()
        } else {
            template
                .constraints
                .as_ref()
                .and_then(|c| c.supported_formats.clone())
                .unwrap_or_default()
        };

        ordered_ids.push(template.id.clone());
        let (
            runtime_max_concurrent_requests,
            runtime_min_interval_ms,
            runtime_concurrency_sem,
            runtime_last_request_at,
        ) = runtime_limits_from_constraints(template.constraints.as_ref());
        runtimes.insert(
            template.id.clone(),
            ComponentRuntime {
                template,
                auth_values,
                supported_business_lines: normalize_component_supported_business_lines(
                    &item.supported_business_lines,
                ),
                language_map,
                supported_content_formats,
                supported_formats,
                key_pool: comp_key_pool,
                oauth_pool: comp_oauth_pool,
                oauth_manager: oauth_manager_arc.clone(),
                proxy_profile_id: None,
                runtime_max_concurrent_requests,
                runtime_min_interval_ms,
                runtime_concurrency_sem,
                runtime_last_request_at,
            },
        );
    }

    if bindings_dirty {
        save_component_bindings_runtime_doc(component_bindings_path, component_bindings)?;
        let _ = log_event(
            log_file,
            "info",
            "component.bindings_saved",
            json!({
                "path": component_bindings_path,
                "components": component_bindings.components.len()
            }),
        );
    }

    // Load local components:
    // - Inline templates (openai_compatible)
    // - Aliases to Server templates via template_id
    if !local_doc.components.is_empty() {
        let mut local_loaded = 0usize;
        let mut local_snapshots_backfilled = 0usize;
        let mut local_doc_dirty = false;
        for (comp_id, comp) in local_doc.components.iter_mut() {
            if !comp.enabled {
                let _ = log_event(
                    log_file,
                    "info",
                    "component.local_runtime_skipped_disabled",
                    json!({ "component_id": comp_id }),
                );
                continue;
            }
            if runtimes.contains_key(comp_id) {
                continue;
            } // Server component takes priority
            let effective_api_version = effective_local_component_api_version(comp, &list_items);
            if comp.source_template_api_version != effective_api_version {
                comp.source_template_api_version = effective_api_version.clone();
                comp.updated_at = Some(format!("{}", crate::logging::unix_ts()));
                local_doc_dirty = true;
            }
            if !is_component_api_version_compatible(effective_api_version.as_deref()) {
                let _ = log_event(
                    log_file,
                    "warning",
                    "component.local_api_version_incompatible",
                    json!({
                        "component_id": comp_id,
                        "template_id": comp.template_id,
                        "api_version": effective_api_version,
                        "max_supported_major": MAX_SUPPORTED_COMPONENT_API_MAJOR
                    }),
                );
                continue;
            }
            let mut inherited_supported_business_lines: Vec<String> = Vec::new();
            let mut inherited_supported_content_formats: Vec<String> = Vec::new();
            let mut inherited_supported_formats: Vec<String> = Vec::new();

            let mut template: ComponentTemplate = if let Some(ref tpl_json) = comp.template_json {
                match serde_json::from_value(tpl_json.clone()) {
                    Ok(t) => t,
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "component.local_template_parse_failed",
                            json!({ "component_id": comp_id, "reason": format!("{:#}", err) }),
                        );
                        continue;
                    }
                }
            } else {
                let template_ref_id = comp.template_id.trim();
                if template_ref_id.is_empty() {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "component.local_template_missing",
                        json!({
                            "component_id": comp_id,
                            "reason": "template_json/template_id both empty"
                        }),
                    );
                    continue;
                }
                let Some(base_runtime) = runtimes.get(template_ref_id) else {
                    let _ = log_event(
                        log_file,
                        "warning",
                        "component.local_template_ref_missing",
                        json!({
                            "component_id": comp_id,
                            "template_id": template_ref_id
                        }),
                    );
                    continue;
                };
                inherited_supported_business_lines = base_runtime.supported_business_lines.clone();
                inherited_supported_content_formats =
                    base_runtime.supported_content_formats.clone();
                inherited_supported_formats = base_runtime.supported_formats.clone();
                let mut snapshot_template = base_runtime.template.clone();
                apply_local_component_kind_to_template(&mut snapshot_template, &comp.kind);
                if comp.template_json.is_none() {
                    match serde_json::to_value(&snapshot_template) {
                        Ok(snapshot_json) => {
                            comp.template_json = Some(snapshot_json);
                            comp.updated_at = Some(format!("{}", crate::logging::unix_ts()));
                            local_doc_dirty = true;
                            local_snapshots_backfilled += 1;
                            let _ = log_event(
                                log_file,
                                "info",
                                "component.local_snapshot_backfilled",
                                json!({
                                    "component_id": comp_id,
                                    "template_id": template_ref_id
                                }),
                            );
                        }
                        Err(err) => {
                            let _ = log_event(
                                log_file,
                                "warning",
                                "component.local_snapshot_backfill_failed",
                                json!({
                                    "component_id": comp_id,
                                    "template_id": template_ref_id,
                                    "reason": format!("{:#}", err)
                                }),
                            );
                        }
                    }
                }
                snapshot_template
            };
            apply_local_component_kind_to_template(&mut template, &comp.kind);
            // Local runtime must expose local component ID for selection/probe stability.
            template.id = comp_id.clone();
            apply_component_instance_overrides_to_template(
                &mut template,
                comp.component_overrides.as_ref(),
            );
            // Resolve auth from component_bindings (same as Server components)
            let binding_entry = component_bindings.components.get(comp_id).cloned();
            let has_pooled_auth = binding_entry
                .as_ref()
                .map(|entry| !entry.key_ids.is_empty() || !entry.oauth_ids.is_empty())
                .unwrap_or(false);
            let (auth_values, updated) =
                match resolve_auth_values(comp_id, template.auth.as_ref(), component_bindings) {
                    Ok(result) => result,
                    Err(err) if has_pooled_auth => {
                        let _ = log_event(
                            log_file,
                            "info",
                            "component.local_auth_deferred_to_pool",
                            json!({
                                "component_id": comp_id,
                                "reason": format!("{:#}", err)
                            }),
                        );
                        (HashMap::new(), false)
                    }
                    Err(err) => {
                        let _ = log_event(
                            log_file,
                            "warning",
                            "component.local_auth_skipped",
                            json!({
                                "component_id": comp_id,
                                "reason": format!("{:#}", err)
                            }),
                        );
                        eprintln!("[WARN] Skipping local component {} — {}", comp_id, err);
                        continue;
                    }
                };
            bindings_dirty = bindings_dirty || updated;
            let language_map = component_bindings
                .components
                .get(comp_id)
                .map(|e| e.language_map.clone())
                .unwrap_or_default();
            apply_binding_overrides_to_template(&mut template, binding_entry.as_ref());
            let (key_pool, oauth_pool) = build_component_pools(
                comp_id,
                Some(comp.vendor_id.as_str()),
                binding_entry.as_ref(),
                &vendor_keys_doc,
                &vendor_oauth_doc,
            )?;

            let normalized_kind = normalize_component_runtime_kind(&comp.kind)
                .or_else(|| normalize_component_runtime_kind(&template.kind))
                .unwrap_or_else(|| "text".to_string());
            let selected_version = select_local_component_version(comp);
            let selected_proxy_profile_id = selected_version
                .and_then(|(_, version)| version.proxy_profile_id.clone())
                .filter(|value| !value.trim().is_empty());
            let supported_content_formats = if !inherited_supported_content_formats.is_empty() {
                inherited_supported_content_formats
            } else {
                resolve_local_supported_content_formats(&normalized_kind, &template)
            };
            let supported_formats = if !inherited_supported_formats.is_empty() {
                inherited_supported_formats
            } else {
                template
                    .constraints
                    .as_ref()
                    .and_then(|c| c.supported_formats.clone())
                    .unwrap_or_default()
            };

            let _ = log_event(
                log_file,
                "info",
                "component.local_runtime_ready",
                json!({
                    "component_id": comp_id,
                    "component_name": comp.name,
                    "kind": comp.kind,
                    "template_id": comp.template_id,
                    "supported_content_formats": supported_content_formats,
                    "supported_formats": supported_formats
                }),
            );

            ordered_ids.push(comp_id.clone());
            let (
                runtime_max_concurrent_requests,
                runtime_min_interval_ms,
                runtime_concurrency_sem,
                runtime_last_request_at,
            ) = runtime_limits_from_constraints(template.constraints.as_ref());
            runtimes.insert(
                comp_id.clone(),
                ComponentRuntime {
                    template,
                    auth_values,
                    // Empty means "all business lines", which keeps local components
                    // selectable for post/taxonomy/plugin/config i18n.
                    supported_business_lines: inherited_supported_business_lines,
                    language_map,
                    supported_content_formats,
                    supported_formats,
                    key_pool,
                    oauth_pool,
                    oauth_manager: oauth_manager_arc.clone(),
                    proxy_profile_id: selected_proxy_profile_id,
                    runtime_max_concurrent_requests,
                    runtime_min_interval_ms,
                    runtime_concurrency_sem,
                    runtime_last_request_at,
                },
            );
            local_loaded += 1;
        }
        if local_loaded > 0 {
            let _ = log_event(
                log_file,
                "info",
                "component.local_components_loaded",
                json!({ "count": local_loaded }),
            );
        }
        if local_doc_dirty {
            save_local_components_runtime_doc(&components_local_path, &local_doc)?;
            let _ = log_event(
                log_file,
                "info",
                "component.local_snapshots_saved",
                json!({ "count": local_snapshots_backfilled, "path": components_local_path }),
            );
        }
    }

    if bindings_dirty {
        save_component_bindings_runtime_doc(component_bindings_path, component_bindings)?;
    }

    let _ = log_event(
        log_file,
        "info",
        "component.registry_ready",
        json!({
            "total_items": list_items.len(),
            "loaded_runtimes": ordered_ids.len(),
            "ordered_ids": ordered_ids
        }),
    );

    if runtimes.is_empty() {
        if let Some(upstream) = last_upstream_error {
            return Err(upstream.into());
        }
        return Err(anyhow!("no supported component runtime loaded"));
    }

    Ok(ComponentRuntimeRegistry {
        runtimes,
        ordered_ids,
    })
}

fn select_local_component_version<'a>(
    comp: &'a ComponentInstanceLocal,
) -> Option<(&'a String, &'a ComponentVersion)> {
    if let Some(active) = comp
        .active_version
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        if let Some(version) = comp.versions.get(active) {
            return comp
                .versions
                .keys()
                .find(|key| key.as_str() == active)
                .map(|key| (key, version));
        }
    }
    if comp.versions.len() == 1 {
        return comp.versions.iter().next();
    }
    comp.versions
        .iter()
        .max_by(|(left_id, left), (right_id, right)| {
            left.created_at
                .cmp(&right.created_at)
                .then(left_id.cmp(right_id))
        })
}

/// Local data resources loaded from config files (vendor-keys, proxy-profiles, vendor-oauth).
#[allow(dead_code)]
pub(crate) struct LocalDataResources {
    pub(crate) key_pools: HashMap<String, KeyPool>,
    pub(crate) proxy_pool: ProxyClientPool,
    pub(crate) oauth_manager: OAuthTokenManager,
}

/// Load local data resources from the config directory.
/// This loads vendor-keys.json, proxy-profiles.json, and vendor-oauth.json,
/// then builds KeyPool instances and a ProxyClientPool.
///
/// When `registry` is provided, all `KeyPool` entries use globally-shared
/// concurrency counters from the registry instead of per-pool counters.
#[allow(dead_code)]
pub(crate) fn load_local_data_resources(
    config_dir: &str,
    log_file: &str,
) -> anyhow::Result<LocalDataResources> {
    load_local_data_resources_with_registry(config_dir, log_file, None)
}

#[allow(dead_code)]
pub(crate) fn load_local_data_resources_with_registry(
    config_dir: &str,
    log_file: &str,
    registry: Option<&GlobalKeyRegistry>,
) -> anyhow::Result<LocalDataResources> {
    let vendor_keys_path = Path::new(config_dir)
        .join("vendor-keys.json")
        .to_string_lossy()
        .to_string();
    let proxy_profiles_path = Path::new(config_dir)
        .join("proxy-profiles.json")
        .to_string_lossy()
        .to_string();
    let vendor_oauth_path = Path::new(config_dir)
        .join("vendor-oauth.json")
        .to_string_lossy()
        .to_string();

    // Load vendor keys
    let vendor_keys_doc =
        load_vendor_keys(&vendor_keys_path).with_context(|| "load vendor keys failed")?;
    let _ = log_event(
        log_file,
        "info",
        "local_data.vendor_keys_loaded",
        json!({ "count": vendor_keys_doc.keys.len(), "path": vendor_keys_path }),
    );

    // Build key pools grouped by vendor_id
    let mut vendor_key_groups: HashMap<String, Vec<VendorPoolKeyTuple>> = HashMap::new();
    for (key_id, key) in &vendor_keys_doc.keys {
        if !key.enabled {
            continue;
        }
        vendor_key_groups
            .entry(key.vendor_id.clone())
            .or_default()
            .push((
                key_id.clone(),
                key.auth_values.clone(),
                key.max_concurrent,
                key.weight,
            ));
    }
    let mut key_pools = HashMap::new();
    for (vendor_id, keys) in vendor_key_groups {
        let pool = match registry {
            Some(r) => KeyPool::new_with_registry(keys, KeySelectionStrategy::RoundRobin, r),
            None => KeyPool::new(keys, KeySelectionStrategy::RoundRobin),
        };
        key_pools.insert(vendor_id, pool);
    }

    // Load proxy profiles
    let proxy_profiles_doc =
        load_proxy_profiles(&proxy_profiles_path).with_context(|| "load proxy profiles failed")?;
    let _ = log_event(
        log_file,
        "info",
        "local_data.proxy_profiles_loaded",
        json!({ "count": proxy_profiles_doc.profiles.len(), "path": proxy_profiles_path }),
    );
    let proxy_pool = ProxyClientPool::new(&proxy_profiles_doc.profiles)
        .with_context(|| "build proxy client pool failed")?;

    // Load vendor OAuth configs
    let vendor_oauth_doc =
        load_vendor_oauth(&vendor_oauth_path).with_context(|| "load vendor oauth failed")?;
    let _ = log_event(
        log_file,
        "info",
        "local_data.vendor_oauth_loaded",
        json!({ "count": vendor_oauth_doc.configs.len(), "path": vendor_oauth_path }),
    );
    let oauth_http_client = Client::builder()
        .no_proxy()
        .build()
        .with_context(|| "build oauth http client failed")?;
    let oauth_manager = OAuthTokenManager::new(
        vendor_oauth_doc.configs,
        oauth_http_client,
        vendor_oauth_path,
    );

    Ok(LocalDataResources {
        key_pools,
        proxy_pool,
        oauth_manager,
    })
}

pub(crate) fn normalize_component_runtime_kind(raw: &str) -> Option<String> {
    let lower = raw.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let normalized = match lower.as_str() {
        "text" | "text_translation" | "field" | "fields" => "text",
        "image" | "images" | "image_translation" => "image",
        "video" | "videos" | "video_translation" => "video",
        "audio" | "audios" | "audio_translation" => "audio",
        "document" | "documents" | "doc" | "file" | "files" | "document_translation" => "document",
        "openai_compatible" | "openai-compatible" | "openai" | "llm" => "openai_compatible",
        _ => return None,
    };
    Some(normalized.to_string())
}

pub(crate) fn canonical_template_type_for_local_kind(raw: &str) -> Option<&'static str> {
    let normalized = normalize_component_runtime_kind(raw)?;
    match normalized.as_str() {
        "text" | "openai_compatible" => Some("text_translation"),
        "image" => Some("image_translation"),
        "video" => Some("video_translation"),
        "audio" => Some("audio_translation"),
        "document" => Some("document_translation"),
        _ => None,
    }
}

pub(crate) fn apply_local_component_kind_to_template(
    template: &mut ComponentTemplate,
    local_kind: &str,
) {
    if let Some(template_type) = canonical_template_type_for_local_kind(local_kind) {
        template.kind = template_type.to_string();
    }
}

fn default_supported_content_formats_for_kind(normalized_kind: &str) -> Vec<String> {
    match normalized_kind {
        "image" | "video" | "audio" | "document" => vec!["media_ref".to_string()],
        _ => vec![
            "plain_text".to_string(),
            "rich_html".to_string(),
            "json_structured".to_string(),
            "serialized_php".to_string(),
        ],
    }
}

pub(crate) fn resolve_local_supported_content_formats(
    normalized_kind: &str,
    template: &ComponentTemplate,
) -> Vec<String> {
    let declared = template
        .constraints
        .as_ref()
        .and_then(|c| c.supported_content_formats.clone())
        .unwrap_or_default();
    if !declared.is_empty() {
        return declared;
    }
    default_supported_content_formats_for_kind(normalized_kind)
}

fn find_translation_mode_for_content_format<'a>(
    template: &'a ComponentTemplate,
    content_format: &str,
) -> Option<&'a ComponentTranslationMode> {
    let normalized_format = content_format.trim().to_ascii_lowercase();
    if normalized_format.is_empty() {
        return None;
    }
    template.translation_modes.iter().find(|mode| {
        mode.supported_content_formats
            .iter()
            .any(|fmt| fmt.trim().eq_ignore_ascii_case(&normalized_format))
    })
}

fn apply_request_overrides_to_template_unrestricted(
    template: &mut ComponentTemplate,
    request_overrides: Option<&ComponentRequestOverrides>,
) {
    let Some(request_overrides) = request_overrides else {
        return;
    };
    if let Some(url) = request_overrides.url.as_deref() {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            template.request.url = trimmed.to_string();
        }
    }
    if let Some(headers) = request_overrides.headers.as_ref() {
        let mut merged = template.request.headers.clone().unwrap_or_default();
        for (key, value) in headers {
            merged.insert(key.clone(), value.clone());
        }
        if !merged.is_empty() {
            template.request.headers = Some(merged);
        }
    }
    if let Some(body) = request_overrides.body.as_ref() {
        template.request.body = Some(body.clone());
    }
}

fn apply_default_values_override_to_template_unrestricted(
    template: &mut ComponentTemplate,
    default_values_override: Option<&Value>,
) {
    let Some(default_values_override) = default_values_override else {
        return;
    };
    let Some(override_obj) = default_values_override.as_object() else {
        template.default_values = Some(default_values_override.clone());
        return;
    };
    let mut merged = template
        .default_values
        .as_ref()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (key, value) in override_obj {
        merged.insert(key.clone(), value.clone());
    }
    template.default_values = Some(Value::Object(merged));
}

pub(crate) fn resolve_runtime_for_content_format(
    runtime: &ComponentRuntime,
    content_format: &str,
) -> ComponentRuntime {
    let Some(mode) = find_translation_mode_for_content_format(&runtime.template, content_format)
    else {
        return runtime.clone();
    };

    let mut resolved = runtime.clone();
    resolved.template.constraints = merge_component_constraints(
        resolved.template.constraints.clone(),
        mode.constraints_overrides.clone(),
    );
    apply_request_overrides_to_template_unrestricted(
        &mut resolved.template,
        mode.request_overrides.as_ref(),
    );
    apply_default_values_override_to_template_unrestricted(
        &mut resolved.template,
        mode.default_values_overrides.as_ref(),
    );
    if !mode.supported_content_formats.is_empty() {
        resolved.supported_content_formats = mode
            .supported_content_formats
            .iter()
            .map(|fmt| fmt.trim().to_ascii_lowercase())
            .filter(|fmt| !fmt.is_empty())
            .collect();
    }
    let (
        runtime_max_concurrent_requests,
        runtime_min_interval_ms,
        runtime_concurrency_sem,
        runtime_last_request_at,
    ) = runtime_limits_from_constraints(resolved.template.constraints.as_ref());
    resolved.runtime_max_concurrent_requests = runtime_max_concurrent_requests;
    resolved.runtime_min_interval_ms = runtime_min_interval_ms;
    resolved.runtime_concurrency_sem = runtime_concurrency_sem;
    resolved.runtime_last_request_at = runtime_last_request_at;
    resolved
}

pub(crate) fn merge_component_constraints(
    base: Option<ComponentConstraints>,
    override_constraints: Option<ComponentConstraints>,
) -> Option<ComponentConstraints> {
    match (base, override_constraints) {
        (None, None) => None,
        (Some(base_constraints), None) => Some(base_constraints),
        (None, Some(override_only)) => Some(override_only),
        (Some(base_constraints), Some(override_constraints)) => Some(ComponentConstraints {
            max_input_chars: override_constraints
                .max_input_chars
                .or(base_constraints.max_input_chars),
            max_input_bytes: override_constraints
                .max_input_bytes
                .or(base_constraints.max_input_bytes),
            split_strategy: override_constraints
                .split_strategy
                .or(base_constraints.split_strategy),
            split_separator: override_constraints
                .split_separator
                .or(base_constraints.split_separator),
            batch_supported: override_constraints
                .batch_supported
                .or(base_constraints.batch_supported),
            rate_limit_rpm: override_constraints
                .rate_limit_rpm
                .or(base_constraints.rate_limit_rpm),
            rate_limit_qps: override_constraints
                .rate_limit_qps
                .or(base_constraints.rate_limit_qps),
            supported_content_types: override_constraints
                .supported_content_types
                .or(base_constraints.supported_content_types),
            max_concurrent_requests: override_constraints
                .max_concurrent_requests
                .or(base_constraints.max_concurrent_requests),
            max_file_size_mb: override_constraints
                .max_file_size_mb
                .or(base_constraints.max_file_size_mb),
            supported_formats: override_constraints
                .supported_formats
                .or(base_constraints.supported_formats),
            supported_content_formats: override_constraints
                .supported_content_formats
                .or(base_constraints.supported_content_formats),
            input_artifact_kind: override_constraints
                .input_artifact_kind
                .or(base_constraints.input_artifact_kind),
            output_artifact_kinds: override_constraints
                .output_artifact_kinds
                .or(base_constraints.output_artifact_kinds),
        }),
    }
}

pub(crate) fn template_allows_editable_path(template: &ComponentTemplate, path: &str) -> bool {
    let normalized = path.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    template.editable_params.iter().any(|item| {
        let p = item.path.trim().to_ascii_lowercase();
        p == normalized
            || (p == "request.body" && normalized.starts_with("request.body."))
            || (p == "request.headers" && normalized.starts_with("request.headers."))
            || (p == "default_values" && normalized.starts_with("default_values."))
            || (p.ends_with(".*")
                && normalized.starts_with(p.trim_end_matches('*').trim_end_matches('.')))
    })
}

pub(crate) fn task_editable_overrides_to_component_overrides(
    value: Option<&serde_json::Value>,
) -> ComponentInstanceOverrides {
    let Some(obj) = value.and_then(|v| v.as_object()) else {
        return ComponentInstanceOverrides::default();
    };

    let mut constraints_override = ComponentConstraints::default();
    let mut has_constraints = false;
    let mut request_headers: HashMap<String, String> = HashMap::new();
    let mut request_body_map = serde_json::Map::new();
    let mut request_url: Option<String> = None;
    let mut default_values_map = serde_json::Map::new();

    for (path, raw_value) in obj {
        let normalized = path.trim();
        if normalized == "request.url" {
            if let Some(v) = raw_value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                request_url = Some(v.to_string());
            }
            continue;
        }
        if let Some(key) = normalized.strip_prefix("request.headers.") {
            if let Some(v) = raw_value.as_str() {
                request_headers.insert(key.to_string(), v.to_string());
            }
            continue;
        }
        if let Some(key) = normalized.strip_prefix("request.body.") {
            request_body_map.insert(key.to_string(), raw_value.clone());
            continue;
        }
        if let Some(key) = normalized.strip_prefix("default_values.") {
            default_values_map.insert(key.to_string(), raw_value.clone());
            continue;
        }
        match normalized {
            "constraints.max_input_chars" => {
                constraints_override.max_input_chars = raw_value.as_u64();
                has_constraints = true;
            }
            "constraints.max_input_bytes" => {
                constraints_override.max_input_bytes = raw_value.as_u64();
                has_constraints = true;
            }
            "constraints.rate_limit_rpm" => {
                constraints_override.rate_limit_rpm = raw_value.as_u64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.rate_limit_qps" => {
                constraints_override.rate_limit_qps = raw_value.as_u64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.max_concurrent_requests" => {
                constraints_override.max_concurrent_requests = raw_value.as_u64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.max_file_size_mb" => {
                constraints_override.max_file_size_mb = raw_value.as_f64().map(|v| v as u32);
                has_constraints = true;
            }
            "constraints.split_strategy" => {
                constraints_override.split_strategy = raw_value.as_str().map(|v| v.to_string());
                has_constraints = true;
            }
            "constraints.split_separator" => {
                constraints_override.split_separator = raw_value.as_str().map(|v| v.to_string());
                has_constraints = true;
            }
            "constraints.supported_formats" => {
                constraints_override.supported_formats = raw_value.as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                });
                has_constraints = true;
            }
            "constraints.supported_content_formats" => {
                constraints_override.supported_content_formats =
                    raw_value.as_array().map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    });
                has_constraints = true;
            }
            _ => {}
        }
    }

    ComponentInstanceOverrides {
        constraints_override: if has_constraints {
            Some(constraints_override)
        } else {
            None
        },
        request_overrides: if request_url.is_some()
            || !request_headers.is_empty()
            || !request_body_map.is_empty()
        {
            Some(ComponentRequestOverrides {
                method: None,
                url: request_url,
                headers: if request_headers.is_empty() {
                    None
                } else {
                    Some(request_headers)
                },
                body: if request_body_map.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(request_body_map))
                },
            })
        } else {
            None
        },
        default_values_override: if default_values_map.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(default_values_map))
        },
    }
}

fn filter_constraints_override_by_editable(
    template: &ComponentTemplate,
    override_constraints: Option<ComponentConstraints>,
) -> Option<ComponentConstraints> {
    let override_constraints = override_constraints?;
    let allow =
        |field: &str| template_allows_editable_path(template, &format!("constraints.{field}"));

    let filtered = ComponentConstraints {
        max_input_chars: if allow("max_input_chars") {
            override_constraints.max_input_chars
        } else {
            None
        },
        max_input_bytes: if allow("max_input_bytes") {
            override_constraints.max_input_bytes
        } else {
            None
        },
        split_strategy: if allow("split_strategy") {
            override_constraints.split_strategy
        } else {
            None
        },
        split_separator: if allow("split_separator") {
            override_constraints.split_separator
        } else {
            None
        },
        batch_supported: if allow("batch_supported") {
            override_constraints.batch_supported
        } else {
            None
        },
        rate_limit_rpm: if allow("rate_limit_rpm") {
            override_constraints.rate_limit_rpm
        } else {
            None
        },
        rate_limit_qps: if allow("rate_limit_qps") {
            override_constraints.rate_limit_qps
        } else {
            None
        },
        supported_content_types: if allow("supported_content_types") {
            override_constraints.supported_content_types
        } else {
            None
        },
        max_concurrent_requests: if allow("max_concurrent_requests") {
            override_constraints.max_concurrent_requests
        } else {
            None
        },
        max_file_size_mb: if allow("max_file_size_mb") {
            override_constraints.max_file_size_mb
        } else {
            None
        },
        supported_formats: if allow("supported_formats") {
            override_constraints.supported_formats
        } else {
            None
        },
        supported_content_formats: if allow("supported_content_formats") {
            override_constraints.supported_content_formats
        } else {
            None
        },
        input_artifact_kind: if allow("input_artifact_kind") {
            override_constraints.input_artifact_kind
        } else {
            None
        },
        output_artifact_kinds: if allow("output_artifact_kinds") {
            override_constraints.output_artifact_kinds
        } else {
            None
        },
    };
    if filtered.max_input_chars.is_none()
        && filtered.max_input_bytes.is_none()
        && filtered.split_strategy.is_none()
        && filtered.split_separator.is_none()
        && filtered.batch_supported.is_none()
        && filtered.rate_limit_rpm.is_none()
        && filtered.rate_limit_qps.is_none()
        && filtered.supported_content_types.is_none()
        && filtered.max_concurrent_requests.is_none()
        && filtered.max_file_size_mb.is_none()
        && filtered.supported_formats.is_none()
        && filtered.supported_content_formats.is_none()
        && filtered.input_artifact_kind.is_none()
        && filtered.output_artifact_kinds.is_none()
    {
        None
    } else {
        Some(filtered)
    }
}

pub(crate) fn apply_binding_overrides_to_template(
    template: &mut ComponentTemplate,
    binding_entry: Option<&ComponentBindingEntry>,
) -> Option<ComponentConstraints> {
    let constraints_override = binding_entry.and_then(|entry| entry.constraints_override.clone());
    let request_overrides = binding_entry.and_then(|entry| entry.request_overrides.clone());
    let default_values_override =
        binding_entry.and_then(|entry| entry.default_values_override.clone());
    apply_template_overrides(
        template,
        constraints_override,
        request_overrides,
        default_values_override,
    )
}

pub(crate) fn apply_component_instance_overrides_to_template(
    template: &mut ComponentTemplate,
    overrides: Option<&ComponentInstanceOverrides>,
) -> Option<ComponentConstraints> {
    let constraints_override = overrides.and_then(|entry| entry.constraints_override.clone());
    let request_overrides = overrides.and_then(|entry| entry.request_overrides.clone());
    let default_values_override = overrides.and_then(|entry| entry.default_values_override.clone());
    apply_template_overrides(
        template,
        constraints_override,
        request_overrides,
        default_values_override,
    )
}

fn apply_template_overrides(
    template: &mut ComponentTemplate,
    constraints_override: Option<ComponentConstraints>,
    request_overrides: Option<ComponentRequestOverrides>,
    default_values_override: Option<Value>,
) -> Option<ComponentConstraints> {
    let filtered_override = filter_constraints_override_by_editable(template, constraints_override);
    let merged_constraints =
        merge_component_constraints(template.constraints.clone(), filtered_override);
    template.constraints = merged_constraints.clone();
    apply_request_overrides_to_template(template, request_overrides.as_ref());
    apply_default_values_override_to_template(template, default_values_override.as_ref());
    merged_constraints
}

fn apply_request_overrides_to_template(
    template: &mut ComponentTemplate,
    request_overrides: Option<&ComponentRequestOverrides>,
) {
    let Some(request_overrides) = request_overrides else {
        return;
    };
    if let Some(method) = request_overrides.method.as_deref() {
        if template_allows_editable_path(template, "request.method") {
            let trimmed = method.trim();
            if !trimmed.is_empty() {
                template.request.method = trimmed.to_string();
            }
        }
    }
    if let Some(url) = request_overrides.url.as_deref() {
        if template_allows_editable_path(template, "request.url") {
            let trimmed = url.trim();
            if !trimmed.is_empty() {
                template.request.url = trimmed.to_string();
            }
        }
    }
    if let Some(headers) = request_overrides.headers.as_ref() {
        if template_allows_editable_path(template, "request.headers")
            || template_allows_editable_path(template, "request.headers.*")
        {
            let mut merged = template.request.headers.clone().unwrap_or_default();
            for (key, value) in headers {
                merged.insert(key.clone(), value.clone());
            }
            if !merged.is_empty() {
                template.request.headers = Some(merged);
            }
        } else {
            let mut merged = template.request.headers.clone().unwrap_or_default();
            for (key, value) in headers {
                let path = format!("request.headers.{key}");
                if template_allows_editable_path(template, &path) {
                    merged.insert(key.clone(), value.clone());
                }
            }
            if !merged.is_empty() {
                template.request.headers = Some(merged);
            }
        }
    }
    if let Some(body) = request_overrides.body.as_ref() {
        if template_allows_editable_path(template, "request.body")
            || template_allows_editable_path(template, "request.body.*")
        {
            template.request.body = Some(body.clone());
        } else if let Some(body_obj) = body.as_object() {
            let mut merged = template
                .request
                .body
                .as_ref()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            for (key, value) in body_obj {
                let path = format!("request.body.{key}");
                if template_allows_editable_path(template, &path) {
                    merged.insert(key.clone(), value.clone());
                }
            }
            if !merged.is_empty() {
                template.request.body = Some(serde_json::Value::Object(merged));
            }
        }
    }
}

fn apply_default_values_override_to_template(
    template: &mut ComponentTemplate,
    default_values_override: Option<&Value>,
) {
    let Some(default_values_override) = default_values_override else {
        return;
    };
    if template_allows_editable_path(template, "default_values")
        || template_allows_editable_path(template, "default_values.*")
    {
        template.default_values = Some(default_values_override.clone());
        return;
    }
    let Some(override_obj) = default_values_override.as_object() else {
        return;
    };
    let mut merged = template
        .default_values
        .as_ref()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (key, value) in override_obj {
        let path = format!("default_values.{key}");
        if template_allows_editable_path(template, &path) {
            merged.insert(key.clone(), value.clone());
        }
    }
    if !merged.is_empty() {
        template.default_values = Some(Value::Object(merged));
    }
}

pub(crate) fn runtime_limits_from_constraints(
    constraints: Option<&ComponentConstraints>,
) -> RuntimeLimits {
    let max_concurrent_requests = constraints
        .and_then(|c| c.max_concurrent_requests)
        .unwrap_or(0);
    let rate_limit_qps = constraints
        .and_then(|c| c.rate_limit_qps)
        .or_else(|| {
            constraints.and_then(|c| {
                c.rate_limit_rpm
                    .map(|rpm| if rpm == 0 { 0 } else { rpm.div_ceil(60) })
            })
        })
        .unwrap_or(0);
    let min_interval_ms = if rate_limit_qps > 0 {
        ((1000.0 / rate_limit_qps as f64).ceil() as u64).max(1)
    } else {
        0
    };

    let concurrency_sem = if max_concurrent_requests > 0 {
        Some(Arc::new(tokio::sync::Semaphore::new(
            max_concurrent_requests as usize,
        )))
    } else {
        None
    };

    let runtime_last_request_at = if min_interval_ms > 0 {
        Some(Arc::new(tokio::sync::Mutex::new(
            Instant::now() - Duration::from_secs(60),
        )))
    } else {
        None
    };

    (
        max_concurrent_requests,
        min_interval_ms,
        concurrency_sem,
        runtime_last_request_at,
    )
}

fn parse_component_api_version(raw: &str) -> Option<(u64, u64, u64)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let core = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub(crate) fn is_component_api_version_compatible(api_version: Option<&str>) -> bool {
    let Some(raw) = api_version else {
        return true;
    };
    let Some((major, _, _)) = parse_component_api_version(raw) else {
        return false;
    };
    major <= MAX_SUPPORTED_COMPONENT_API_MAJOR
}

pub(crate) fn collect_configured_runtime_component_ids(
    local_doc: &ComponentsLocalDoc,
    task_type_bindings: Option<&TaskTypeComponentBindingsDoc>,
    rule_component_bindings: Option<&RuleComponentBindingsDoc>,
    component_bindings: Option<&ComponentBindingsDoc>,
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();

    ids.extend(local_doc.components.keys().cloned());

    if let Some(bindings) = task_type_bindings {
        ids.extend(
            bindings
                .task_types
                .values()
                .map(|entry| entry.component_id.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
        ids.extend(
            bindings
                .business_line_task_types
                .values()
                .map(|entry| entry.component_id.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
    }

    if let Some(bindings) = rule_component_bindings {
        ids.extend(
            bindings
                .global_defaults
                .values()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
        ids.extend(
            bindings
                .plugin_bindings
                .values()
                .flat_map(|slot_map| slot_map.values())
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
        ids.extend(
            bindings
                .relation_bindings
                .values()
                .flat_map(|slot_map| slot_map.values())
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
        ids.extend(
            bindings
                .rule_bindings
                .values()
                .flat_map(|slot_map| slot_map.values())
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
    }

    if let Some(bindings) = component_bindings {
        ids.extend(
            bindings
                .components
                .keys()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        );
    }

    ids
}

fn normalize_component_supported_type(raw: &str) -> Option<&'static str> {
    match raw.trim().to_lowercase().as_str() {
        "text" => Some("text"),
        "image" => Some("image"),
        "video" => Some("video"),
        "audio" => Some("audio"),
        "document" | "doc" | "file" => Some("document"),
        _ => None,
    }
}

pub(crate) fn component_kind_matches_supported_types(
    normalized_kind: &str,
    supported_types: &[String],
) -> bool {
    if supported_types.is_empty() {
        return true;
    }
    if matches!(normalized_kind, "openai_compatible") {
        return true;
    }
    let required = match normalized_kind {
        "text" => "text",
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        "document" => "document",
        _ => return true,
    };
    let normalized_supported: BTreeSet<&'static str> = supported_types
        .iter()
        .filter_map(|t| normalize_component_supported_type(t))
        .collect();
    // Backward compatibility: unknown list entries should not hard-fail old templates.
    if normalized_supported.is_empty() {
        return true;
    }
    normalized_supported.contains(required)
}

fn effective_local_component_api_version(
    comp: &ComponentInstanceLocal,
    server_components: &[ComponentItem],
) -> Option<String> {
    server_components
        .iter()
        .find(|item| item.id == comp.template_id.trim())
        .and_then(|item| item.api_version.clone())
        .or_else(|| comp.source_template_api_version.clone())
}

pub(crate) fn normalize_component_supported_business_lines(raw_lines: &[String]) -> Vec<String> {
    if raw_lines.is_empty() {
        return Vec::new();
    }
    let mut out: BTreeSet<String> = BTreeSet::new();
    for raw in raw_lines {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if matches!(trimmed.to_lowercase().as_str(), "all" | "*") {
            return Vec::new();
        }
        if let Some(normalized) = parse_business_line_key(trimmed) {
            out.insert(normalized);
        }
    }
    out.into_iter().collect()
}

/// Build per-component KeyPool and OAuthPool from the component binding's key_ids / oauth_ids.
fn build_component_pools(
    _component_id: &str,
    component_vendor_id: Option<&str>,
    binding: Option<&crate::types::ComponentBindingEntry>,
    vendor_keys_doc: &crate::types::VendorKeysDoc,
    vendor_oauth_doc: &crate::types::VendorOAuthDoc,
) -> anyhow::Result<(
    Option<std::sync::Arc<crate::component_rt::key_pool::KeyPool>>,
    Option<std::sync::Arc<crate::component_rt::oauth::OAuthPool>>,
)> {
    let Some(binding) = binding else {
        return Ok((None, None));
    };

    // Build KeyPool from key_ids
    let key_pool = if !binding.key_ids.is_empty() {
        let mut entries: Vec<ComponentBindingKeyTuple> = Vec::new();
        for kid in &binding.key_ids {
            if let Some(vk) = vendor_keys_doc.keys.get(kid) {
                if vk.enabled
                    && component_vendor_id
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(|expected| vk.vendor_id.trim() == expected)
                        .unwrap_or(true)
                {
                    entries.push((
                        kid.clone(),
                        vk.auth_values
                            .iter()
                            .map(|(name, value)| {
                                crate::component_rt::runner::resolve_credential_reference(value)
                                    .map(|resolved| (name.clone(), resolved))
                            })
                            .collect::<anyhow::Result<HashMap<String, String>>>()?,
                        vk.max_concurrent,
                        vk.weight,
                        vk.max_input_chars,
                        vk.max_file_size_mb,
                        vk.requests_per_second,
                    ));
                }
            }
        }
        if !entries.is_empty() {
            Some(std::sync::Arc::new(
                crate::component_rt::key_pool::KeyPool::new_with_ext_with_rps(
                    entries,
                    binding.auth_strategy.clone(),
                ),
            ))
        } else {
            None
        }
    } else {
        None
    };

    // Build OAuthPool from oauth_ids
    let oauth_pool = if !binding.oauth_ids.is_empty() {
        let mut entries: Vec<crate::component_rt::oauth::OAuthPoolEntry> = Vec::new();
        for oid in &binding.oauth_ids {
            if let Some(oc) = vendor_oauth_doc.configs.get(oid) {
                if component_vendor_id
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(|expected| oc.vendor_id.trim() == expected)
                    .unwrap_or(true)
                {
                    entries.push(crate::component_rt::oauth::OAuthPoolEntry {
                        config_id: oid.clone(),
                        token_field: oc.token_field.clone(),
                        max_concurrent: oc.max_concurrent,
                        max_input_chars: oc.max_input_chars,
                        max_file_size_mb: oc.max_file_size_mb,
                        weight: oc.weight,
                        active_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    });
                }
            }
        }
        if !entries.is_empty() {
            Some(std::sync::Arc::new(
                crate::component_rt::oauth::OAuthPool::new(entries, binding.auth_strategy.clone()),
            ))
        } else {
            None
        }
    } else {
        None
    };

    Ok((key_pool, oauth_pool))
}

#[cfg(test)]
mod tests;
