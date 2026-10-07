//! SQLite-owned media delivery recovery, before callback envelope mutation.
use super::*;
use std::sync::Arc;
use tokio::sync::Mutex;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_pending_media_with_optional_db(
    db: Option<&Arc<Mutex<rusqlite::Connection>>>,
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    payload: &mut TranslationCallbackPayload,
    log_file: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(db) = db {
        upload_pending_media_durable(
            db,
            client,
            wp_base,
            token,
            worker_id,
            device_id,
            task_id,
            relation_id,
            payload,
            log_file,
            route_secret,
        )
        .await
    } else {
        upload_pending_media_common(
            None,
            client,
            wp_base,
            token,
            worker_id,
            device_id,
            task_id,
            relation_id,
            payload,
            log_file,
            route_secret,
        )
        .await
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    format: String,
    unit: Value,
    binding: String,
    attempt_id: String,
    attachment_id: Option<u64>,
}

fn load_receipt(
    conn: &rusqlite::Connection,
    key: &str,
    unit: &Value,
    binding: &str,
) -> anyhow::Result<Option<(String, Receipt)>> {
    let Some(raw) = crate::db::system::get_system_config_checked(conn, key)? else {
        return Ok(None);
    };
    anyhow::ensure!(
        raw.starts_with("V1BUQw"),
        "media receipt is not encrypted; retained"
    );
    let row: Receipt = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)
        .context("damaged media receipt; retained")?;
    anyhow::ensure!(
        row.format == "media-upload-receipt-v1"
            && row.unit == *unit
            && row.binding == binding
            && uuid::Uuid::parse_str(&row.attempt_id).is_ok()
            && row.attachment_id.is_none_or(|id| id > 0),
        "media receipt scope/result mismatch; retained"
    );
    Ok(Some((raw, row)))
}

fn receipt_value(row: &Receipt) -> anyhow::Result<String> {
    crate::db::system::encrypt_config_value(&serde_json::to_string(row)?)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_pending_media_durable(
    db: &Arc<Mutex<rusqlite::Connection>>,
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    payload: &mut TranslationCallbackPayload,
    log_file: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<()> {
    upload_pending_media_common(
        Some(db),
        client,
        wp_base,
        token,
        worker_id,
        device_id,
        task_id,
        relation_id,
        payload,
        log_file,
        route_secret,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn upload_pending_media_common(
    db: Option<&Arc<Mutex<rusqlite::Connection>>>,
    client: &Client,
    wp_base: &str,
    token: &str,
    worker_id: &str,
    device_id: &str,
    task_id: i64,
    relation_id: u64,
    payload: &mut TranslationCallbackPayload,
    log_file: &str,
    route_secret: Option<&str>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        relation_id == payload.relation_id
            && relation_id > 0
            && relation_id <= i64::MAX as u64
            && task_id >= 0,
        "media receipt relation mismatch"
    );
    let original = payload.clone();
    for (index, mapping) in original.media_mappings.iter().enumerate() {
        if mapping.translated_ref.trim().is_empty() || mapping.attachment_id.is_some() {
            continue;
        }
        anyhow::ensure!(
            !original.client_task_id.trim().is_empty()
                && !device_id.trim().is_empty()
                && mapping.source_id > 0
                && mapping.source_id <= i64::MAX as u64,
            "media receipt requires a complete operation identity"
        );
        let unit = json!({
            "site":crate::db::system::private_site_key(wp_base),
            "relation_id":relation_id,"object_type":original.object_type,"object_id":original.object_id,
            "client_task_id":original.client_task_id,"source_revision":original.source_revision,
            "source_id":mapping.source_id,
        });
        let key = format!(
            "media-upload-receipt-v1:{}",
            crate::db::system::private_json_digest(&unit)?
        );
        let binding = crate::db::system::private_json_digest(&json!({
            "unit":unit,"mapping":mapping,"device_id":device_id,"task_id":task_id,
            "business_line":original.business_line,"source_lang":original.source_lang,
            "target_lang":original.target_lang,"route_secret":route_secret,
        }))?;
        let files = crate::component_rt::non_text::recovery_store::RecoveryStore::configured()?;
        let _delivery_guard = files.guard(&format!("delivery:{key}"))?;
        let reserved = if let Some(db) = db {
            let mut conn = db.lock().await;
            let tx = conn.savepoint()?;
            if let Some((_, row)) = load_receipt(&tx, &key, &unit, &binding)? {
                if let Some(id) = row.attachment_id {
                    payload.media_mappings[index].attachment_id = Some(id);
                    payload.media_mappings[index].translated_ref.clear();
                    continue;
                }
                (String::new(), row, false)
            } else {
                let row = Receipt {
                    format: "media-upload-receipt-v1".into(),
                    unit: unit.clone(),
                    binding: binding.clone(),
                    attempt_id: uuid::Uuid::new_v4().to_string(),
                    attachment_id: None,
                };
                let raw = receipt_value(&row)?;
                let n = tx.execute(
                    "INSERT INTO system_config (key,value) VALUES (?1,?2)",
                    rusqlite::params![key, raw],
                )?;
                anyhow::ensure!(n == 1, "media receipt intent was not committed");
                tx.commit()?;
                (raw, row, true)
            }
        } else {
            files.update(&format!("delivery:{key}"), |old: Option<Receipt>| {
                let fresh = old.is_none();
                let row = old.unwrap_or_else(|| Receipt {
                    format: "media-upload-receipt-v1".into(),
                    unit: unit.clone(),
                    binding: binding.clone(),
                    attempt_id: uuid::Uuid::new_v4().to_string(),
                    attachment_id: None,
                });
                anyhow::ensure!(
                    row.unit == unit
                        && row.binding == binding
                        && row.format == "media-upload-receipt-v1"
                        && uuid::Uuid::parse_str(&row.attempt_id).is_ok()
                        && row.attachment_id.is_none_or(|id| id > 0),
                    "media file receipt scope mismatch"
                );
                Ok((row.clone(), (String::new(), row, fresh)))
            })?
        };
        if let Some(id) = reserved.1.attachment_id {
            anyhow::ensure!(id > 0, "invalid retained media attachment");
            payload.media_mappings[index].attachment_id = Some(id);
            payload.media_mappings[index].translated_ref.clear();
            continue;
        }
        // Work on a private mapping copy. The caller only observes a successful
        // mapping after its encrypted receipt has committed.
        let mut one = original.clone();
        one.media_mappings = vec![mapping.clone()];
        let config = crate::component_rt::non_text::ChunkedUploadConfig {
            wp_base: wp_base.into(),
            token: token.into(),
            worker_id: worker_id.into(),
            device_id: device_id.into(),
            route_secret: route_secret.map(str::to_owned),
            chunk_size: 0,
        };
        if let Some(done) =
            crate::component_rt::non_text::recovery::resume_scope(client, &config, &key).await?
        {
            one.media_mappings[0].attachment_id = Some(u64::try_from(done.attachment_id)?);
            one.media_mappings[0].translated_ref.clear();
        } else {
            anyhow::ensure!(
                reserved.2,
                "legacy unknown upload requires explicit attachment verification; no reupload"
            );
            super::upload_pending_media_with_scope(
                client,
                wp_base,
                token,
                worker_id,
                device_id,
                task_id,
                relation_id,
                &mut one,
                log_file,
                route_secret,
                Some(&key),
            )
            .await?;
        }
        let id = one.media_mappings[0]
            .attachment_id
            .filter(|id| *id > 0)
            .ok_or_else(|| {
                anyhow!("media upload has no confirmed attachment; original and intent retained")
            })?;
        if let Some(db) = db {
            let mut conn = db.lock().await;
            let tx = conn.savepoint()?;
            let (raw, mut row) = load_receipt(&tx, &key, &unit, &binding)?.ok_or_else(|| {
                anyhow!("media upload intent disappeared; result not acknowledged")
            })?;
            anyhow::ensure!(
                (reserved.0.is_empty() || raw == reserved.0)
                    && row.attempt_id == reserved.1.attempt_id
                    && row.attachment_id.is_none(),
                "media upload receipt ownership changed; retained"
            );
            row.attachment_id = Some(id);
            let n = tx.execute(
                "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                rusqlite::params![receipt_value(&row)?, key, raw],
            )?;
            anyhow::ensure!(
                n == 1,
                "media upload receipt result was not committed; intent retained"
            );
            tx.commit()?;
        } else {
            files.update(&format!("delivery:{key}"), |old: Option<Receipt>| {
                let mut row = old.context("media file intent disappeared")?;
                anyhow::ensure!(
                    row.attempt_id == reserved.1.attempt_id
                        && row.attachment_id.is_none()
                        && row.unit == unit
                        && row.binding == binding
                        && row.format == "media-upload-receipt-v1",
                    "media file receipt ownership changed"
                );
                row.attachment_id = Some(id);
                Ok((row, ()))
            })?;
        }
        payload.media_mappings[index] = one.media_mappings.remove(0);
    }
    Ok(())
}
