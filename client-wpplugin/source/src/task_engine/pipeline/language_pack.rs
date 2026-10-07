//! Language-pack manual work uses the same frozen request and provider ledger.
use super::*;
use crate::db::jobs::TranslationItem;
use crate::db::unit_lock::UnitLease;
use anyhow::{ensure, Result};

fn delivery_key(item_id: i64) -> String {
    format!("language-pack-delivery-v1:{item_id}")
}

pub(crate) fn has_delivery(conn: &rusqlite::Connection, item_id: i64) -> Result<bool> {
    Ok(crate::db::system::get_system_config_checked(conn, &delivery_key(item_id))?.is_some())
}

pub(crate) fn assert_no_delivery(conn: &rusqlite::Connection, item_id: i64) -> Result<()> {
    ensure!(
        !has_delivery(conn, item_id)?,
        "original language-pack delivery must be reconciled before changing intent"
    );
    Ok(())
}

fn entry_delivery_key(
    base: &str,
    relation: i64,
    entry: i64,
    business_line: &str,
) -> Result<String> {
    Ok(format!(
        "language-pack-delivery-entry-v1:{}",
        crate::db::system::private_json_digest(&json!({
            "physical":UnitLease::content_suffix(base, relation, "language_pack", entry)?,
            "business_line":business_line,
        }))?
    ))
}

pub(crate) fn prepare_delivery(
    conn: &rusqlite::Connection,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    lease: &UnitLease,
    item: &TranslationItem,
    wp_base: &str,
    payload: &I18nCallbackPayload,
    idempotency_key: &str,
) -> Result<String> {
    lease.assert_item(conn, db, item.id)?;
    ensure!(
        !conn.is_autocommit(),
        "language-pack delivery requires an authority transaction"
    );
    ensure!(
        !crate::db::review_attempts::unfinished(conn, item.id)?
            && matches!(
                item.status.as_str(),
                "pending_review" | "translated" | "failed"
            )
            && (item.max_retries <= 0 || item.retry_count < item.max_retries),
        "language-pack delivery is unfinished, terminal or its retry budget is closed; retained"
    );
    let mut ids = BTreeSet::new();
    ensure!(
        !payload.entries.is_empty()
            && payload.entries.iter().all(|entry| entry.entry_id > 0
                && ids.insert(entry.entry_id)
                && !entry.msgstr.trim().is_empty()),
        "language-pack callback has invalid entries; retained"
    );
    let key = delivery_key(item.id);
    let document = json!({
        "format":"language-pack-delivery-v1","item_id":item.id,
        "site":wp_base,"path":item.translated_path,"idempotency_key":idempotency_key,
        "payload":payload,"max_retries":item.max_retries,
    });
    let encoded = if let Some(raw) = crate::db::system::get_system_config_checked(conn, &key)? {
        ensure!(
            raw.starts_with("V1BUQw"),
            "language-pack delivery intent is not encrypted; retained"
        );
        let saved: Value = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
        ensure!(
            saved == document,
            "language-pack delivery intent differs; original retained"
        );
        raw
    } else {
        crate::db::system::set_encrypted_config(conn, &key, &document.to_string())?;
        crate::db::system::get_system_config_checked(conn, &key)?
            .context("language-pack delivery intent was not committed")?
    };
    let association = json!({
        "format":"language-pack-delivery-entry-v1","item_id":item.id,
        "delivery_key":key,"idempotency_key":idempotency_key,
    });
    for entry in &payload.entries {
        let key = entry_delivery_key(
            wp_base,
            item.relation_id,
            entry.entry_id,
            &payload.business_line,
        )?;
        if let Some(raw) = crate::db::system::get_system_config_checked(conn, &key)? {
            ensure!(
                raw.starts_with("V1BUQw"),
                "language-pack entry delivery is not encrypted; retained"
            );
            let saved: Value =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
            ensure!(
                saved == association,
                "original language-pack entry delivery must be reconciled before regrouping"
            );
        } else {
            crate::db::system::set_encrypted_config(conn, &key, &association.to_string())?;
        }
    }
    lease.assert_item(conn, db, item.id)?;
    Ok(encoded)
}

pub(crate) fn finish_delivery(
    conn: &rusqlite::Connection,
    item_id: i64,
    original: &str,
) -> Result<()> {
    ensure!(
        conn.execute(
            "DELETE FROM system_config WHERE key=?1 AND value=?2",
            rusqlite::params![delivery_key(item_id), original],
        )? == 1,
        "language-pack delivery intent changed before acknowledgement; retained"
    );
    ensure!(
        crate::db::system::get_system_config_checked(conn, &delivery_key(item_id))?.is_none(),
        "language-pack delivery intent cleanup was not committed; retained"
    );
    Ok(())
}

pub(crate) fn delivery_authority(
    conn: &rusqlite::Connection,
    item: &TranslationItem,
    wp_base: &str,
    payload: &I18nCallbackPayload,
    idempotency_key: &str,
    original: &str,
) -> Result<String> {
    let key = delivery_key(item.id);
    ensure!(
        original.starts_with("V1BUQw")
            && crate::db::system::get_system_config_checked(conn, &key)?.as_deref()
                == Some(original),
        "original language-pack delivery changed; retained"
    );
    let saved: Value = serde_json::from_str(&crate::db::system::decrypt_config_value(original)?)?;
    ensure!(
        saved
            == json!({
                "format":"language-pack-delivery-v1","item_id":item.id,
                "site":wp_base,"path":item.translated_path,"idempotency_key":idempotency_key,
                "payload":payload,"max_retries":item.max_retries,
            }),
        "original language-pack delivery scope differs; retained"
    );
    let association = json!({
        "format":"language-pack-delivery-entry-v1","item_id":item.id,
        "delivery_key":key,"idempotency_key":idempotency_key,
    });
    let mut physical = BTreeMap::new();
    for entry in &payload.entries {
        let key = entry_delivery_key(
            wp_base,
            item.relation_id,
            entry.entry_id,
            &payload.business_line,
        )?;
        let raw = crate::db::system::get_system_config_checked(conn, &key)?
            .context("original language-pack entry delivery missing; retained")?;
        ensure!(
            raw.starts_with("V1BUQw")
                && serde_json::from_str::<Value>(&crate::db::system::decrypt_config_value(&raw)?)?
                    == association,
            "original language-pack entry delivery changed; retained"
        );
        physical.insert(key, raw);
    }
    crate::db::system::private_json_digest(&json!({
        "delivery":original,"entries":physical,
    }))
}

pub(crate) fn existing_delivery_authority(
    conn: &rusqlite::Connection,
    item: &TranslationItem,
    wp_base: &str,
    payload: &I18nCallbackPayload,
    idempotency_key: &str,
) -> Result<Option<String>> {
    crate::db::system::get_system_config_checked(conn, &delivery_key(item.id))?
        .map(|original| {
            delivery_authority(conn, item, wp_base, payload, idempotency_key, &original)
        })
        .transpose()
}

pub(crate) fn read_envelope(
    value: &Value,
    item: &TranslationItem,
) -> Result<I18nTranslatedEnvelope> {
    let envelope: I18nTranslatedEnvelope = serde_json::from_value(value.clone())?;
    let payload = &envelope.payload;
    let mut ids = BTreeSet::new();
    ensure!(
        item.object_type == "language_pack"
            && envelope.payload_type == "i18n_language_pack"
            && envelope.idempotency_key == payload.client_task_id
            && !payload.client_task_id.is_empty()
            && payload.client_task_id.len() <= 128
            && payload.relation_id == u64::try_from(item.relation_id)?
            && payload.business_line == item.business_line
            && !payload.worker_id.is_empty()
            && !payload.source_lang.is_empty()
            && !payload.target_lang.is_empty()
            && !payload.entries.is_empty()
            && payload.entries.iter().all(|entry| entry.entry_id > 0
                && ids.insert(entry.entry_id)
                && !entry.msgstr.trim().is_empty()),
        "manual language-pack payload scope differs; retained"
    );
    Ok(envelope)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn retranslate(
    proxy_pool: &ProxyClientPool,
    registry: &ComponentRuntimeRegistry,
    selected_component_id: &str,
    item: &TranslationItem,
    content: &Value,
    relation: &DiscoveredRelation,
    scope: &crate::db::async_jobs::AsyncJobScope,
) -> Result<I18nTranslatedEnvelope> {
    let mut envelope = read_envelope(
        content
            .get("__manual_i18n_envelope")
            .context("frozen language-pack callback missing")?,
        item,
    )?;
    ensure!(
        content["relation_id"].as_i64() == Some(item.relation_id)
            && content["business_line"].as_str() == Some(item.business_line.as_str())
            && relation.id == item.relation_id
            && scope.relation_id == item.relation_id
            && scope.object_type == item.object_type
            && scope.object_id == item.wp_object_id,
        "frozen language-pack source scope differs; retained"
    );
    let entries = content["entries"]
        .as_array()
        .context("frozen language-pack entries missing")?;
    let mut sources = BTreeMap::new();
    for entry in entries {
        let id = entry["entry_id"]
            .as_i64()
            .context("invalid frozen language-pack entry")?;
        let mut source = entry["source"].clone();
        ensure!(
            id > 0
                && source.is_object()
                && source["object_id"].as_i64().is_some_and(|id| id > 0)
                && entry["msgstr"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()),
            "invalid frozen language-pack source; no new fee"
        );
        source["entry_id"] = json!(id);
        let source: LanguagePackCompleteData = serde_json::from_value(source)?;
        let text = if source.plural_index.unwrap_or(0) > 0 && !source.msgid_plural.trim().is_empty()
            || source.msgid.trim().is_empty()
        {
            source.msgid_plural
        } else {
            source.msgid
        };
        ensure!(
            !text.trim().is_empty() && sources.insert(id, text).is_none(),
            "empty or duplicate frozen language-pack source; no new fee"
        );
    }
    ensure!(
        sources.len() == envelope.payload.entries.len()
            && envelope
                .payload
                .entries
                .iter()
                .all(|entry| sources.contains_key(&entry.entry_id)),
        "frozen language-pack entry identities differ; no new fee"
    );
    let selected = registry
        .runtimes
        .get(selected_component_id)
        .context("explicit language-pack component is unavailable; no replacement selected")?;
    let supported = crate::component_rt::selector::select_component_runtime_for_task_type(
        Some(registry),
        &json!({"__input_artifact_kind":"i18n_bundle",
            "__expected_output_artifact_kind":"translated_i18n_bundle"}),
        &item.business_line,
        "text",
        selected_component_id,
        &[],
        None,
        None,
    );
    ensure!(
        supported.is_some_and(|runtime| std::ptr::eq(runtime, selected)),
        "explicit language-pack component cannot handle this route; no replacement selected"
    );
    let limits = content
        .get("__manual_i18n_limits")
        .context("frozen language-pack limits missing")?;
    let max_input_chars = limits["max_input_chars"]
        .as_u64()
        .context("invalid frozen language-pack input limit")?;
    let split_strategy = limits["split_strategy"]
        .as_str()
        .context("invalid frozen split strategy")?;
    // The worker contract defines zero as no input limit. Keep that frozen
    // policy; template and credential limits still apply during execution.
    let constraints = EffectiveConstraints::resolve(
        selected.template.constraints.as_ref(),
        None,
        None,
        max_input_chars,
        split_strategy,
    );
    let selected_client = proxy_pool.get_client(selected.proxy_profile_id.as_deref())?;
    let request = scope
        .source_snapshot
        .as_ref()
        .and_then(|source| source["manual_generation"].as_str())
        .context("manual language-pack generation missing")?;
    crate::db::review_attempts::validate_request(request)?;
    envelope.idempotency_key = format!("manual-pack-{}-{request}", item.id);
    envelope.payload.client_task_id = envelope.idempotency_key.clone();
    envelope.payload.source_lang = relation.source_lang.clone();
    envelope.payload.target_lang = relation.target_lang.clone();
    for entry in &mut envelope.payload.entries {
        let source = &sources[&entry.entry_id];
        let env = scope.field_env(&format!("entry-{}.msgstr", entry.entry_id), "text");
        entry.msgstr = translate_text_with_constraints_with_env(
            selected_client,
            selected,
            source,
            &relation.source_lang,
            &relation.target_lang,
            &constraints,
            Some(env),
        )
        .await?;
        ensure!(
            !entry.msgstr.trim().is_empty(),
            "empty paid language-pack output; retained"
        );
        crate::component_rt::content_safety::validate_interpolation_tokens(source, &entry.msgstr)?;
    }
    // This timestamp is frozen in the original envelope, so orphan replay is byte stable.
    Ok(envelope)
}

pub(crate) async fn persist_manual_result(
    lease: &UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_id: i64,
    envelope: &I18nTranslatedEnvelope,
    path: &str,
    scope: &crate::db::async_jobs::AsyncJobScope,
) -> Result<()> {
    let value = serde_json::to_value(envelope)?;
    let prepared =
        crate::db::review_attempts::prepare_result(db, lease, scope, item_id, &value, path).await?;
    let mut conn = db.lock().await;
    let tx = conn.savepoint()?;
    lease.assert_item(&tx, db, item_id)?;
    let item = crate::db::jobs::get_item_checked(&tx, item_id)
        .context("manual item read")?
        .context("manual item missing")?;
    read_envelope(&value, &item)?;
    ensure!(
        !matches!(item.status.as_str(), "done" | "skipped"),
        "manual language-pack result cannot reset a delivered item"
    );
    install_json_snapshot(std::path::Path::new(path), &value, &prepared)?;
    project_saved_manual_result(&tx, lease, db, item_id, envelope, path, scope)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn project_saved_manual_result(
    conn: &rusqlite::Connection,
    lease: &UnitLease,
    db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>,
    item_id: i64,
    envelope: &I18nTranslatedEnvelope,
    path: &str,
    scope: &crate::db::async_jobs::AsyncJobScope,
) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "manual language-pack projection requires an authority transaction"
    );
    lease.assert_item(conn, db, item_id)?;
    let item = crate::db::jobs::get_item_checked(conn, item_id)
        .context("manual item read")?
        .context("manual item missing")?;
    read_envelope(&serde_json::to_value(envelope)?, &item)?;
    ensure!(
        !matches!(item.status.as_str(), "done" | "skipped"),
        "manual language-pack result cannot reset a delivered item"
    );
    ensure!(
        conn.execute(
            "UPDATE translation_items SET client_task_id=?1 WHERE id=?2",
            rusqlite::params![envelope.payload.client_task_id, item_id],
        )? == 1,
        "manual language-pack identity was not committed"
    );
    update_item_translated_path(conn, item_id, path)?;
    update_item_status(conn, item_id, "pending_review", None)?;
    crate::db::review_attempts::finish(conn, db, lease, scope, item_id)?;
    lease.assert_item(conn, db, item_id)?;
    Ok(())
}
