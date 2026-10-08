use anyhow::{ensure, Context};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::*;

pub(super) fn language_pack_batch_storage_identity(
    relation_id: i64,
    business_line: &str,
    subtype: &str,
    translated_entries: &[I18nCallbackEntry],
) -> (String, i64) {
    let mut entry_ids: Vec<i64> = translated_entries
        .iter()
        .map(|entry| entry.entry_id)
        .collect();
    entry_ids.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(relation_id.to_le_bytes());
    hasher.update(business_line.as_bytes());
    hasher.update(b":");
    hasher.update(subtype.as_bytes());
    for entry_id in entry_ids {
        hasher.update(b":");
        hasher.update(entry_id.to_le_bytes());
    }

    let digest = hasher.finalize();
    let storage_key: String = digest[..6].iter().map(|b| format!("{:02x}", b)).collect();
    let mut object_id_bytes = [0u8; 8];
    object_id_bytes.copy_from_slice(&digest[..8]);
    let synthetic_object_id = (u64::from_le_bytes(object_id_bytes) & i64::MAX as u64) as i64;
    (storage_key, synthetic_object_id.max(1))
}

pub(super) fn language_pack_batch_idempotency_key(
    wp_base: &str,
    relation_id: i64,
    business_line: &str,
    subtype: &str,
    source_lang: &str,
    target_lang: &str,
    worker_id: &str,
    translated_entries: &[I18nCallbackEntry],
) -> String {
    let normalized_domain = crate::bindings::normalize_domain_base(wp_base);
    let mut domain_hasher = Sha256::new();
    domain_hasher.update(normalized_domain.as_bytes());
    let domain_digest = domain_hasher.finalize();
    let domain_key: String = domain_digest[..6]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    let mut batch_hasher = Sha256::new();
    batch_hasher.update(relation_id.to_le_bytes());
    batch_hasher.update(b":");
    batch_hasher.update(business_line.as_bytes());
    batch_hasher.update(b":");
    batch_hasher.update(subtype.as_bytes());
    batch_hasher.update(b":");
    batch_hasher.update(source_lang.as_bytes());
    batch_hasher.update(b":");
    batch_hasher.update(target_lang.as_bytes());
    for entry in translated_entries {
        batch_hasher.update(b":");
        batch_hasher.update(entry.entry_id.to_le_bytes());
        batch_hasher.update(b":");
        batch_hasher.update(entry.msgstr.as_bytes());
    }
    let batch_digest = batch_hasher.finalize();
    let batch_key: String = batch_digest[..8]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect();

    format!(
        "lang-pack-{}-{}-{}-{}-{}-{}",
        domain_key, relation_id, business_line, subtype, batch_key, worker_id
    )
}

#[derive(Debug, Clone)]
pub(super) struct PersistedLanguagePackBatch {
    pub(super) item_id: i64,
    pub(super) translated_path: String,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn translate_language_pack_entry(
    client: &reqwest::Client,
    runtime: &ComponentRuntime,
    db: Option<&Arc<tokio::sync::Mutex<rusqlite::Connection>>>,
    wp_base: &str,
    relation: &DiscoveredRelation,
    business_line: &str,
    subtype: &str,
    item: &LanguagePackItem,
    task_params: &DiscoveryTaskParams,
    constraints: &EffectiveConstraints,
) -> anyhow::Result<String> {
    let db = db.context("LANGUAGE_PACK_AUTHORITY_REQUIRED: durable database is unavailable")?;
    ensure!(
        relation.id > 0 && item.complete_data.entry_id > 0 && item.object_id > 0,
        "invalid language-pack source identity"
    );
    let base = url::Url::parse(wp_base)?
        .as_str()
        .trim_end_matches('/')
        .to_string();
    let physical = crate::db::unit_lock::UnitLease::content_suffix(
        &base,
        relation.id,
        "language_pack",
        item.complete_data.entry_id,
    )?;
    let snapshot = json!({
        "format":"language-pack-entry-v1",
        "relation":relation,
        "business_line":business_line,
        "subtype":subtype,
        "source":language_pack_source_snapshot(item),
        "selected_component_id":task_params.selected_component_id,
        "proxy_profile_id":runtime.proxy_profile_id,
        "editable_overrides":task_params.editable_overrides,
        "constraints":{
            "max_input_chars":constraints.max_input_chars,
            "split_strategy":constraints.split_strategy,
            "split_separator":constraints.split_separator
        }
    });
    let env = crate::db::async_jobs::AsyncJobScope {
        db: db.clone(),
        domain: base.clone(),
        relation_id: relation.id,
        object_type: "language_pack".into(),
        object_id: item.complete_data.entry_id,
        source_snapshot: Some(snapshot),
    }
    .field_env(&format!("msgstr.{business_line}.{subtype}"), "text")
    .for_runtime(
        runtime,
        &json!({"text":language_pack_source_text(&item.complete_data)}),
        &relation.source_lang,
        &relation.target_lang,
    )?;
    // A physical entry head also fences a changed source/runtime after an
    // unknown synchronous submit, which has no async projection to find.
    let key = format!(
        "language-pack-entry-v1:{}",
        crate::db::system::private_json_digest(&json!({
            "physical":physical,"business_line":business_line,"subtype":subtype,
        }))?
    );
    let _head_lease = {
        let mut conn = db.lock().await;
        let lease = crate::db::unit_lock::UnitLease::acquire(
            &conn,
            db,
            &format!("language-pack-entry-{}", key.rsplit(':').next().unwrap()),
            "LANGUAGE_PACK_ENTRY_BUSY: original entry is active",
        )?;
        let tx = conn.savepoint()?;
        let scope =
            json!({"format":"language-pack-entry-v1","base":base,"binding":env.resume_binding});
        if let Some(saved) = crate::db::system::get_system_config_checked(&tx, &key)? {
            ensure!(
                saved.starts_with("V1BUQw"),
                "language-pack entry head is not encrypted; retained"
            );
            let saved: serde_json::Value =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&saved)?)?;
            ensure!(
                saved == scope,
                "LANGUAGE_PACK_SCOPE_CHANGED: original entry retained"
            );
        } else {
            crate::db::system::set_encrypted_config(&tx, &key, &scope.to_string())?;
        }
        tx.commit()?;
        lease
    };
    crate::component_rt::runner::translate_text_with_constraints_with_env(
        client,
        runtime,
        language_pack_source_text(&item.complete_data),
        &relation.source_lang,
        &relation.target_lang,
        constraints,
        Some(env),
    )
    .await
}

fn language_pack_source_snapshot(item: &LanguagePackItem) -> serde_json::Value {
    let source = &item.complete_data;
    json!({
        "object_id":item.object_id, "entry_id":source.entry_id,
        "text_domain":source.text_domain, "item_text_domain":item.text_domain,
        "msgctxt":source.msgctxt,"msgid":source.msgid,
        "msgid_plural":source.msgid_plural,"plural_index":source.plural_index
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedPackBatch {
    format: String,
    binding: String,
    raw_path: String,
    translated_path: String,
    raw: serde_json::Value,
    envelope: I18nTranslatedEnvelope,
    item_id: Option<i64>,
}

fn install_pack_artifact(path: &str, value: &serde_json::Value) -> anyhow::Result<()> {
    let path = std::path::Path::new(path);
    match path.symlink_metadata() {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_file(),
                "language-pack artifact is not a regular file; retained"
            );
            ensure!(
                std::fs::read(path)?.starts_with(b"WPTC"),
                "language-pack artifact encryption was changed; retained"
            );
            let saved: serde_json::Value =
                serde_json::from_str(&crate::bindings::load_encrypted_or_plain(path)?)?;
            ensure!(
                saved == *value,
                "language-pack artifact conflicts with saved authority; retained"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let encoded = crate::bindings::encrypt_for_save(&serde_json::to_string(value)?)?;
            crate::bindings::atomic_file::install_new(path, &encoded)?;
        }
        Err(error) => return Err(error).context("inspect saved language-pack artifact"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_language_pack_batch_for_sync(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    job_id: i64,
    data_dir: &str,
    _domain_key: &str,
    wp_base: &str,
    relation: &DiscoveredRelation,
    effective_relation: &DiscoveredRelation,
    business_line: &str,
    subtype: &str,
    source_items: &[LanguagePackItem],
    translated_entries: &[I18nCallbackEntry],
    idempotency_key: &str,
    route_secret: Option<&str>,
    payload: &I18nCallbackPayload,
    component_id: &str,
    task_params: &DiscoveryTaskParams,
) -> anyhow::Result<PersistedLanguagePackBatch> {
    persist_language_pack_batch(
        db,
        job_id,
        data_dir,
        wp_base,
        relation,
        effective_relation,
        business_line,
        subtype,
        source_items,
        translated_entries,
        idempotency_key,
        route_secret,
        payload,
        component_id,
        task_params,
        "translated",
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_language_pack_batch_for_review(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    job_id: i64,
    data_dir: &str,
    wp_base: &str,
    relation: &DiscoveredRelation,
    effective_relation: &DiscoveredRelation,
    business_line: &str,
    subtype: &str,
    source_items: &[LanguagePackItem],
    translated_entries: &[I18nCallbackEntry],
    idempotency_key: &str,
    route_secret: Option<&str>,
    payload: &I18nCallbackPayload,
    component_id: &str,
    task_params: &DiscoveryTaskParams,
) -> anyhow::Result<PersistedLanguagePackBatch> {
    persist_language_pack_batch(
        db,
        job_id,
        data_dir,
        wp_base,
        relation,
        effective_relation,
        business_line,
        subtype,
        source_items,
        translated_entries,
        idempotency_key,
        route_secret,
        payload,
        component_id,
        task_params,
        "pending_review",
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn persist_language_pack_batch(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    job_id: i64,
    data_dir: &str,
    wp_base: &str,
    relation: &DiscoveredRelation,
    effective_relation: &DiscoveredRelation,
    business_line: &str,
    subtype: &str,
    source_items: &[LanguagePackItem],
    translated_entries: &[I18nCallbackEntry],
    idempotency_key: &str,
    route_secret: Option<&str>,
    payload: &I18nCallbackPayload,
    component_id: &str,
    task_params: &DiscoveryTaskParams,
    status: &str,
) -> anyhow::Result<PersistedLanguagePackBatch> {
    ensure!(
        relation.id > 0
            && !component_id.is_empty()
            && !translated_entries.is_empty()
            && !idempotency_key.trim().is_empty()
            && payload.relation_id == u64::try_from(relation.id)?
            && payload.business_line == business_line
            && payload.client_task_id == idempotency_key
            && payload.source_lang == effective_relation.source_lang
            && payload.target_lang == effective_relation.target_lang
            && serde_json::to_value(&payload.entries)? == serde_json::to_value(translated_entries)?,
        "language-pack payload scope differs; retained"
    );
    let mut source_by_entry_id = HashMap::new();
    for item in source_items {
        ensure!(
            item.object_id > 0
                && item.complete_data.entry_id > 0
                && source_by_entry_id
                    .insert(item.complete_data.entry_id, item)
                    .is_none(),
            "invalid or duplicate language-pack source identity"
        );
    }
    let mut ids = std::collections::BTreeSet::new();
    for translated in translated_entries {
        ensure!(
            translated.entry_id > 0
                && ids.insert(translated.entry_id)
                && source_by_entry_id.contains_key(&translated.entry_id)
                && !translated.msgstr.trim().is_empty(),
            "missing, duplicate or empty language-pack entry; retained"
        );
    }
    let (storage_key, batch_object_id) = language_pack_batch_storage_identity(
        relation.id,
        business_line,
        subtype,
        translated_entries,
    );
    // Local item identity includes the entry table/lane even for one-entry
    // reviews; wire entry IDs remain unchanged in the encrypted envelope.
    let synthetic_object_id = batch_object_id;
    let raw_json = json!({
        "relation_id": relation.id,
        "business_line": business_line,
        "subtype": subtype,
        "source_lang": effective_relation.source_lang.clone(),
        "target_lang": effective_relation.target_lang.clone(),
        "entries": translated_entries.iter().map(|translated| {
            let source_item = source_by_entry_id[&translated.entry_id];
            json!({
                "entry_id": translated.entry_id,
                "msgstr": translated.msgstr,
                "source": {
                    "object_id": source_item.object_id,
                    "text_domain": source_item.complete_data.text_domain,
                    "msgctxt": source_item.complete_data.msgctxt,
                    "msgid": source_item.complete_data.msgid,
                    "msgid_plural": source_item.complete_data.msgid_plural,
                    "plural_index": source_item.complete_data.plural_index.unwrap_or(0),
                }
            })
        }).collect::<Vec<_>>(),
    });
    let binding = crate::db::system::private_json_digest(&json!({
        "site":wp_base.trim_end_matches('/'),"relation":relation,
        "effective_relation":effective_relation,"raw":raw_json,"payload":payload,
        "idempotency_key":idempotency_key,"route_secret":route_secret,
        "component":component_id,"selected_component":task_params.selected_component_id,
        "overrides":task_params.editable_overrides,"review_status":status
    }))?;
    let physical = crate::db::unit_lock::UnitLease::content_suffix(
        wp_base,
        relation.id,
        "language_pack",
        synthetic_object_id,
    )?;
    let checkpoint_key = format!("language-pack-batch-v1:{physical}");
    let authority = crate::db::unit_lock::ContentExecution::acquire(
        db,
        wp_base,
        relation.id,
        "language_pack",
        synthetic_object_id,
    )
    .await?;
    let saved = authority
        .mutate(db, |conn| {
            if let Some(raw) = crate::db::system::get_system_config_checked(conn, &checkpoint_key)?
            {
                ensure!(
                    raw.starts_with("V1BUQw"),
                    "language-pack batch is not encrypted; retained"
                );
                let saved: SavedPackBatch =
                    serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
                ensure!(
                    saved.format == "language-pack-batch-v1" && saved.binding == binding,
                    "LANGUAGE_PACK_SCOPE_CHANGED: original batch retained"
                );
                Ok(saved)
            } else {
                let basename = format!("language_pack_{}_{}.json", storage_key, binding);
                let site = crate::db::system::private_json_digest(&json!({
                    "site":wp_base.trim_end_matches('/')
                }))?;
                let saved = SavedPackBatch {
                    format: "language-pack-batch-v1".into(),
                    binding: binding.clone(),
                    raw_path: format!("{data_dir}/raw/{site}/rel_{}/{basename}", relation.id),
                    translated_path: format!(
                        "{data_dir}/translated/{site}/rel_{}/{basename}",
                        relation.id
                    ),
                    raw: raw_json.clone(),
                    envelope: I18nTranslatedEnvelope {
                        payload_type: "i18n_language_pack".into(),
                        idempotency_key: idempotency_key.into(),
                        route_secret: route_secret.map(str::to_string),
                        payload: payload.clone(),
                        persisted_at: unix_ts() as i64,
                    },
                    item_id: None,
                };
                crate::db::system::set_encrypted_config(
                    conn,
                    &checkpoint_key,
                    &serde_json::to_string(&saved)?,
                )?;
                Ok(saved)
            }
        })
        .await?;
    install_pack_artifact(&saved.raw_path, &saved.raw)?;
    install_pack_artifact(
        &saved.translated_path,
        &serde_json::to_value(&saved.envelope)?,
    )?;
    let item_id = authority
        .mutate(db, |conn| {
            let existing: Option<i64> = conn
                .query_row(
                    "SELECT id FROM translation_items WHERE domain=?1 AND relation_id=?2
             AND object_type='language_pack' AND wp_object_id=?3 AND task_type='text'",
                    params![wp_base, relation.id, synthetic_object_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                let item = crate::db::jobs::get_item_checked(conn, id)?
                    .context("saved language-pack item disappeared; retained")?;
                ensure!(
                    saved.item_id == Some(id)
                        && item.raw_path == saved.raw_path
                        && item.translated_path == saved.translated_path
                        && item.client_task_id == idempotency_key
                        && item.component_id == component_id
                        && matches!(
                            item.status.as_str(),
                            "translated" | "pending_review" | "done" | "failed" | "rejected"
                        ),
                    "language-pack projection conflicts with saved authority; retained"
                );
                return Ok(id);
            }
            ensure!(
                saved.item_id.is_none(),
                "saved language-pack item is missing; retained"
            );
            let id = crate::db::jobs::record_item_outcome(
                conn,
                &crate::db::jobs::CreateItemRequest {
                    job_id,
                    domain: wp_base.to_string(),
                    relation_id: relation.id,
                    business_line: business_line.to_string(),
                    object_type: "language_pack".to_string(),
                    wp_object_id: synthetic_object_id,
                    wp_object_subtype: subtype.to_string(),
                    task_type: "text".to_string(),
                    source_lang: effective_relation.source_lang.clone(),
                    target_lang: effective_relation.target_lang.clone(),
                    component_id: component_id.to_string(),
                    component_ids: vec![component_id.to_string()],
                    selected_component_id: task_params
                        .selected_component_id
                        .clone()
                        .or_else(|| Some(component_id.to_string())),
                    effective_source_lang: Some(effective_relation.source_lang.clone()),
                    effective_target_lang: Some(effective_relation.target_lang.clone()),
                    editable_overrides: task_params.editable_overrides.clone(),
                    raw_path: saved.raw_path.clone(),
                    client_task_id: idempotency_key.to_string(),
                    max_retries: 2,
                },
                &saved.translated_path,
                status,
                None,
            )?;
            let original = crate::db::system::get_system_config_checked(conn, &checkpoint_key)?
                .context("language-pack batch checkpoint disappeared; retained")?;
            let mut prior: SavedPackBatch =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&original)?)?;
            ensure!(
                prior.binding == binding && prior.item_id.is_none(),
                "language-pack batch projection changed; retained"
            );
            prior.item_id = Some(id);
            crate::db::system::set_encrypted_config(
                conn,
                &checkpoint_key,
                &serde_json::to_string(&prior)?,
            )?;
            Ok(id)
        })
        .await?;

    Ok(PersistedLanguagePackBatch {
        item_id,
        translated_path: saved.translated_path,
    })
}
