use anyhow::Context;
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
pub(super) async fn persist_language_pack_batch_for_sync(
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    job_id: i64,
    data_dir: &str,
    domain_key: &str,
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
    let (storage_key, synthetic_object_id) = language_pack_batch_storage_identity(
        relation.id,
        business_line,
        subtype,
        translated_entries,
    );
    let raw_path = format!(
        "{}/raw/{}/rel_{}/language_pack_{}_{}_{}.json",
        data_dir, domain_key, relation.id, business_line, subtype, storage_key
    );
    let translated_path = format!(
        "{}/translated/{}/rel_{}/language_pack_{}_{}_{}.json",
        data_dir, domain_key, relation.id, business_line, subtype, storage_key
    );

    let source_by_entry_id: HashMap<i64, &LanguagePackItem> = source_items
        .iter()
        .map(|item| (item.complete_data.entry_id, item))
        .collect();
    let raw_json = json!({
        "relation_id": relation.id,
        "business_line": business_line,
        "subtype": subtype,
        "source_lang": effective_relation.source_lang.clone(),
        "target_lang": effective_relation.target_lang.clone(),
        "entries": translated_entries.iter().map(|translated| {
            let source_item = source_by_entry_id.get(&translated.entry_id).copied();
            json!({
                "entry_id": translated.entry_id,
                "msgstr": translated.msgstr,
                "source": {
                    "object_id": source_item.map(|it| it.object_id).unwrap_or(0),
                    "text_domain": source_item.map(|it| it.complete_data.text_domain.clone()).unwrap_or_default(),
                    "msgctxt": source_item.map(|it| it.complete_data.msgctxt.clone()).unwrap_or_default(),
                    "msgid": source_item.map(|it| it.complete_data.msgid.clone()).unwrap_or_default(),
                    "msgid_plural": source_item.map(|it| it.complete_data.msgid_plural.clone()).unwrap_or_default(),
                    "plural_index": source_item.and_then(|it| it.complete_data.plural_index).unwrap_or(0),
                }
            })
        }).collect::<Vec<_>>(),
    });
    let envelope = I18nTranslatedEnvelope {
        payload_type: "i18n_language_pack".to_string(),
        idempotency_key: idempotency_key.to_string(),
        route_secret: route_secret.map(str::to_string),
        payload: payload.clone(),
        persisted_at: unix_ts() as i64,
    };

    if let Some(parent) = std::path::Path::new(&raw_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create raw dir failed: {}", parent.display()))?;
    }
    if let Some(parent) = std::path::Path::new(&translated_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create translated dir failed: {}", parent.display()))?;
    }
    let raw_str = serde_json::to_string_pretty(&raw_json)
        .context("serialize language_pack raw json failed")?;
    std::fs::write(&raw_path, raw_str)
        .with_context(|| format!("write language_pack raw file failed: {}", raw_path))?;
    let envelope_str = serde_json::to_string_pretty(&envelope)
        .context("serialize language_pack translated envelope failed")?;
    std::fs::write(&translated_path, envelope_str).with_context(|| {
        format!(
            "write language_pack translated file failed: {}",
            translated_path
        )
    })?;

    let item_id = {
        let conn = db.lock().await;
        let item_id = crate::db::jobs::create_item(
            &conn,
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
                raw_path: raw_path.clone(),
                client_task_id: idempotency_key.to_string(),
                max_retries: 2,
            },
        )?;
        crate::db::jobs::update_item_translated_path(&conn, item_id, &translated_path)?;
        crate::db::jobs::update_item_status(&conn, item_id, "translated", None)?;
        item_id
    };

    Ok(PersistedLanguagePackBatch {
        item_id,
        translated_path,
    })
}
