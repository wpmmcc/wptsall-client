use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ContentItem;
use crate::types::TranslationCallbackPayload;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContentReceipt {
    format: String,
    scope: String,
    source_digest: String,
    pub(super) identity: String,
    pub(super) payload: TranslationCallbackPayload,
    ack: Value,
}

fn scope(base: &str, relation: i64, object_type: &str, object: i64) -> Result<String> {
    crate::db::unit_lock::UnitLease::content_suffix(base, relation, object_type, object)
}

fn read(conn: &Connection, key: &str, expected: &str) -> Result<Option<(String, ContentReceipt)>> {
    let Some(raw) = crate::db::system::get_system_config_checked(conn, key)? else {
        return Ok(None);
    };
    ensure!(
        raw.starts_with("V1BUQw"),
        "discovery receipt is not encrypted; retained"
    );
    let saved: ContentReceipt =
        serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)
            .context("damaged discovery receipt; retained")?;
    ensure!(
        saved.format == "discovery-content-receipt-v1"
            && saved.scope == expected
            && !saved.identity.is_empty()
            && !saved.payload.client_task_id.is_empty()
            && saved.source_digest.len() == 64
            && saved.source_digest.bytes().all(|c| c.is_ascii_hexdigit()),
        "discovery receipt identity differs; retained"
    );
    crate::task_engine::submitter::validate_translation_callback_ack(saved.ack.clone())?;
    Ok(Some((raw, saved)))
}

pub(super) fn completed(
    conn: &Connection,
    base: &str,
    relation: i64,
    item: &ContentItem,
) -> Result<Option<ContentReceipt>> {
    let scope = scope(base, relation, &item.object_type, item.object_id)?;
    let key = format!("discovery-content-receipt-v1:{scope}");
    let Some((_, saved)) = read(conn, &key, &scope)? else {
        return Ok(None);
    };
    ensure!(
        i64::try_from(saved.payload.relation_id)? == relation
            && i64::try_from(saved.payload.object_id)? == item.object_id
            && crate::db::pending_callbacks::normalize_pending_object_type(
                &saved.payload.object_type
            )? == crate::db::pending_callbacks::normalize_pending_object_type(
                &item.object_type
            )?,
        "discovery receipt payload scope differs; retained"
    );
    let source_digest = crate::db::system::private_json_digest(&item.complete_data)?;
    if item.needs_resync && saved.source_digest != source_digest {
        return Ok(None);
    }
    ensure!(
        saved.source_digest == source_digest,
        "completed discovery source differs; explicit resync required, original result retained"
    );
    Ok(Some(saved))
}

pub(super) fn save(
    conn: &Connection,
    base: &str,
    identity: &str,
    payload: &TranslationCallbackPayload,
    item: &ContentItem,
    ack: &Value,
) -> Result<()> {
    let relation = i64::try_from(payload.relation_id)?;
    let object = i64::try_from(payload.object_id)?;
    ensure!(
        object == item.object_id
            && crate::db::pending_callbacks::normalize_pending_object_type(&payload.object_type)?
                == crate::db::pending_callbacks::normalize_pending_object_type(&item.object_type)?
            && !identity.is_empty(),
        "discovery receipt source identity differs; retained"
    );
    let scope = scope(base, relation, &payload.object_type, object)?;
    let key = format!("discovery-content-receipt-v1:{scope}");
    let source_digest = crate::db::system::private_json_digest(&item.complete_data)?;
    let prior = read(conn, &key, &scope)?;
    if let Some((_, saved)) = &prior {
        if saved.identity == identity {
            ensure!(
                saved.source_digest == source_digest
                    && crate::db::system::private_json_digest(&serde_json::to_value(
                        &saved.payload
                    )?)? == crate::db::system::private_json_digest(&serde_json::to_value(
                        payload
                    )?)?,
                "discovery receipt was rebound; original result retained"
            );
            return Ok(());
        }
    }
    let saved =
        crate::db::system::encrypt_config_value(&serde_json::to_string(&ContentReceipt {
            format: "discovery-content-receipt-v1".into(),
            scope,
            source_digest,
            identity: identity.to_string(),
            payload: payload.clone(),
            ack: crate::task_engine::submitter::validate_translation_callback_ack(ack.clone())?,
        })?)?;
    let evidence_key = format!(
        "discovery-content-receipt-evidence-v1:{}",
        crate::db::system::private_json_digest(
            &serde_json::json!({"scope":key,"identity":identity,"payload":payload})
        )?
    );
    ensure!(
        conn.execute(
            "INSERT INTO system_config(key,value) VALUES (?1,?2)",
            params![evidence_key, saved]
        )? == 1
            && crate::db::system::get_system_config_checked(conn, &evidence_key)?.as_deref()
                == Some(saved.as_str()),
        "discovery receipt evidence was not committed; original callback retained"
    );
    let changed = match prior {
        Some((prior, _)) => conn.execute(
            "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
            params![saved, key, prior],
        )?,
        None => conn.execute(
            "INSERT INTO system_config(key,value) VALUES (?1,?2)",
            params![key, saved],
        )?,
    };
    ensure!(
        changed == 1
            && crate::db::system::get_system_config_checked(conn, &key)?.as_deref()
                == Some(saved.as_str()),
        "discovery receipt was not committed; original callback retained"
    );
    Ok(())
}
