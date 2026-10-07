use std::path::Path;

use anyhow::{ensure, Context, Result};
use rusqlite::Connection;
use serde_json::Value;

use crate::bindings::{
    build_wp_base_url, domain_token_binding_local_sites, gate, normalize_domain_base, now_unix,
    resolve_entry_for_domain, resolve_route_secret_for_domain, GateVerdict,
};
use crate::types::{
    ComponentBindingsDoc, ComponentsLocalDoc, DomainTokenBindingsDoc, PluginIdentity,
    TranslationCallbackPayload,
};

fn missing_document(path: &Path) -> Result<bool> {
    match path.symlink_metadata() {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "component configuration is not a regular file; retained"
            );
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).context("inspect worker component configuration"),
    }
}

fn install_missing_document(path: &Path, document: &impl serde::Serialize) -> Result<()> {
    let bytes = crate::bindings::encrypt_for_save(&serde_json::to_string_pretty(document)?)?;
    crate::bindings::atomic_file::install_new_uncredited(path, &bytes)
}

pub(super) fn load_component_documents(
    conn: &Connection,
    bindings: &DomainTokenBindingsDoc,
    component_path: &str,
    local_path: &str,
) -> Result<(ComponentBindingsDoc, ComponentsLocalDoc)> {
    let component_missing = missing_document(Path::new(component_path))?;
    let local_missing = missing_document(Path::new(local_path))?;
    let components = crate::bindings::read_component_bindings(component_path)?;
    let local = crate::bindings::read_components_local(local_path).map_err(|_| {
        crate::component_rt::loader::RuntimeConfigurationFault::error("local components file")
    })?;
    ensure!(
        missing_document(Path::new(component_path))? == component_missing
            && missing_document(Path::new(local_path))? == local_missing,
        "worker component configuration changed during startup; retained"
    );
    // Skipping initialization is only for an already saved delivery. This
    // inventory grants no DB credit; execution still rechecks its native lease,
    // encrypted intent and exact result before sending or closing the receipt.
    if (component_missing || local_missing) && !has_original_delivery(conn, bindings)? {
        if component_missing {
            install_missing_document(Path::new(component_path), &components)?;
        }
        if local_missing {
            install_missing_document(Path::new(local_path), &local)?;
        }
    }
    Ok((components, local))
}

fn has_original_delivery(conn: &Connection, bindings: &DomainTokenBindingsDoc) -> Result<bool> {
    for site in domain_token_binding_local_sites(bindings) {
        let Some(entry) = resolve_entry_for_domain(&site.api_base_url, bindings) else {
            continue;
        };
        if !matches!(
            gate(entry, now_unix()),
            GateVerdict::Fresh(PluginIdentity::WpmmccAts)
        ) {
            continue;
        }
        let Some(secret) = resolve_route_secret_for_domain(&site.api_base_url, bindings) else {
            continue;
        };
        let Some(base) = build_wp_base_url(&normalize_domain_base(&site.api_base_url), &secret)
        else {
            continue;
        };
        let legacy = crate::task_engine::pipeline::sanitize_domain_key(&base);
        for item in crate::db::jobs::list_resumable_items_for_client_base(
            conn,
            &base,
            &legacy,
            &["translated"],
        )? {
            let job = crate::db::jobs::get_job_checked(conn, item.job_id)?
                .context("saved delivery has no owning job; retained")?;
            if job.domain != base
                || item.translated_path.is_empty()
                || (item.max_retries > 0 && item.retry_count >= item.max_retries)
                || crate::db::review_attempts::unfinished(conn, item.id)?
            {
                continue;
            }
            let path = Path::new(&item.translated_path);
            ensure!(
                path.symlink_metadata()?.file_type().is_file(),
                "saved delivery result is not a regular file; retained"
            );
            let bytes = std::fs::read(path)?;
            if !bytes.starts_with(b"WPTC") {
                continue;
            }
            let envelope: Value =
                serde_json::from_str(&crate::bindings::decrypt_from_bytes(&bytes)?)?;
            let payload = envelope.get("payload").unwrap_or(&envelope);
            let identity = envelope
                .get("idempotency_key")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&item.client_task_id);
            if payload.get("entries").is_some() && item.object_type == "language_pack" {
                let payload: crate::types::I18nCallbackPayload =
                    serde_json::from_value(payload.clone())?;
                ensure!(
                    payload.relation_id == u64::try_from(item.relation_id)?
                        && payload.business_line == item.business_line
                        && payload.client_task_id == item.client_task_id
                        && identity == payload.client_task_id
                        && payload.source_lang
                            == item
                                .effective_source_lang
                                .as_deref()
                                .unwrap_or(&item.source_lang)
                        && payload.target_lang
                            == item
                                .effective_target_lang
                                .as_deref()
                                .unwrap_or(&item.target_lang)
                        && !payload.entries.is_empty(),
                    "saved language-pack delivery scope differs; retained"
                );
                if crate::task_engine::pipeline::language_pack::existing_delivery_authority(
                    conn, &item, &base, &payload, identity,
                )?
                .is_some()
                {
                    return Ok(true);
                }
            } else {
                let payload: TranslationCallbackPayload = serde_json::from_value(payload.clone())?;
                ensure!(
                    payload.relation_id == u64::try_from(item.relation_id)?
                        && payload.object_id == u64::try_from(item.wp_object_id)?
                        && crate::db::pending_callbacks::normalize_pending_object_type(
                            &payload.object_type
                        )? == crate::db::pending_callbacks::normalize_pending_object_type(
                            &item.object_type
                        )?
                        && payload.business_line == item.business_line
                        && payload.client_task_id == item.client_task_id
                        && payload.source_lang
                            == item
                                .effective_source_lang
                                .as_deref()
                                .unwrap_or(&item.source_lang)
                        && payload.target_lang
                            == item
                                .effective_target_lang
                                .as_deref()
                                .unwrap_or(&item.target_lang)
                        && (item.wp_object_subtype.is_empty()
                            || payload.subtype == item.wp_object_subtype),
                    "saved content delivery scope differs; retained"
                );
                if crate::db::pending_callbacks::find_pending_callback(
                    conn,
                    &base,
                    item.relation_id,
                    &item.object_type,
                    item.wp_object_id,
                )?
                .is_some()
                    && crate::db::pending_callbacks::continuation_authority(
                        conn,
                        &base,
                        identity,
                        &payload,
                        Some(&secret),
                    )?
                    .1
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
