//! 批 R (P2P pair 状态机 — 冻结表销账): relay 泳道在途单元行级恢复。
//!
//! 批 I 的 async_jobs 恢复骑 WP outbox re-offer；relay（wpmmcc 跨站同步）
//! 的回调面是目标站 `/sync/push`，没有 re-offer 机制——客户端崩溃时在途
//! 单元既无人重新呈递也无法续跑。本表就是该泳道的 pair 状态机：每行
//! `(pair_id, canonical_uuid)` 一个 relay 单元，把最贵的已付费副作用
//! （翻译三元组、逐资产媒体转存映射）与可幂等重放的载荷（原 relayed
//! packet——同 packet_id 重推，目标端按指纹 ack 幂等跳过）落库。
//!
//! ```text
//! begin ─▶ store_translation（已付费快照）
//!       ─▶ store_media_entry ×N（逐资产已付副作用）
//!       ─▶ store_relayed_packet（定 packet_id）
//! push ack / park ─▶ close（原子归档原证据，再移除活跃行）
//! 错误            ─▶ 行保持 shipping（错误注记），下轮 pair run 走恢复
//!                    路径跳过已付费阶段；不按年龄清理
//! ```
//!
//! 坏读与未提交的写入返回错误，不能以空快照授权再次收费。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use tokio::sync::Mutex;

use crate::logging::unix_ts;
use crate::sync_engine::shipper::RelayTranslation;

/// Only rows in this phase are resumable (see the schema comment).
pub(crate) const PHASE_SHIPPING: &str = "shipping";

/// Durable snapshot of the paid side effects for one relay unit.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct InflightCtx {
    /// Completed paid fields, including a partial cascade. Missing fields
    /// are requested later; Some fields are never requested again in-scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<RelayTranslation>,
    /// Digest of source revision and effective translation configuration.
    /// Legacy rows omit this and cannot prove scoped paid-field reuse.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub translation_scope: String,
    /// Per-asset media transfer map: source remote_url → target url.
    /// Grown one entry per successful transfer so a crash mid-media
    /// resumes WITHOUT re-transferring finished assets.
    #[serde(default)]
    pub media_url_map: HashMap<String, String>,
    /// Packet action of the stored relayed packet ('upsert'|'delete'|...).
    /// A resumed packet whose CURRENT action differs (e.g. the source moved
    /// on to a delete while we were down) must not replay the stale packet.
    #[serde(default)]
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_receipt: Option<crate::sync_engine::shipper::ShipResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_phase: Option<String>,
}

/// One shipping row as seen by the recovery sweep / ship path.
#[derive(Debug, Clone)]
pub(crate) struct InflightRow {
    pub ctx: InflightCtx,
    /// Serialized relayed packet ('' = the relay transform never ran).
    pub relayed_json: String,
    pub attempts: i64,
}

fn encode_evidence<T: serde::Serialize>(
    pair: &str,
    uuid: &str,
    kind: &str,
    value: &T,
) -> Result<String> {
    super::system::encrypt_config_value(&serde_json::to_string(&serde_json::json!({
        "format":"sync-evidence-v1", "pair":pair, "uuid":uuid, "kind":kind, "value":value,
    }))?)
}

fn decode_evidence<T: serde::de::DeserializeOwned>(
    pair: &str,
    uuid: &str,
    kind: &str,
    raw: &str,
) -> Result<T> {
    // Existing plaintext paid snapshots remain readable without being rewritten
    // merely for a read. New snapshots always authenticate their row identity.
    if raw.trim_start().starts_with('{') {
        if kind == "packet" {
            let _: serde_json::Value = serde_json::from_str(raw).context("damaged legacy sync packet; retained")?;
            return serde_json::from_value(serde_json::Value::String(raw.into())).context("decode legacy packet");
        }
        return serde_json::from_str(raw).context("damaged legacy sync evidence; retained");
    }
    let value: serde_json::Value = serde_json::from_str(&super::system::decrypt_config_value(raw)?)?;
    ensure!(value["format"] == "sync-evidence-v1" && value["pair"] == pair
        && value["uuid"] == uuid && value["kind"] == kind, "sync evidence belongs to another row; retained");
    serde_json::from_value(value["value"].clone()).context("damaged sync evidence; retained")
}

/// Open (or re-open) the shipping row for a unit. Open-or-reopen on purpose:
/// a re-begin after an error INCREMENTS attempts and clears the error note
/// but PRESERVES the paid snapshots (ctx/relayed) — the recovery path must
/// not pay twice for what the previous attempt already paid. Callers that
/// have a changed scope must retain this row for reconciliation, not wipe it.
pub(crate) async fn begin_shipping(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
) -> Result<()> {
    begin_shipping_scoped(db, pair_id, canonical_uuid, "").await
}

pub(crate) async fn begin_shipping_scoped(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    scope: &str,
) -> Result<()> {
    let now = unix_ts() as i64;
    let mut conn = db.lock().await;
    let tx = conn.savepoint()?;
    let prior: Option<(String, String)> = tx
        .query_row(
            "SELECT phase,ctx_json FROM sync_inflight WHERE pair_id=?1 AND canonical_uuid=?2",
            params![pair_id, canonical_uuid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    ensure!(
        prior
            .as_ref()
            .is_none_or(|(phase, _)| phase == PHASE_SHIPPING),
        "sync inflight phase is unknown; original retained"
    );
    if let Some((_, raw)) = &prior {
        let ctx: InflightCtx = decode_evidence(pair_id, canonical_uuid, "context", raw)?;
        ensure!(
            scope.is_empty() || ctx.translation_scope == scope,
            "sync inflight admission scope differs; original retained"
        );
    }
    let updated = tx.execute(
        "UPDATE sync_inflight
         SET attempts = attempts + 1, phase = ?1, error = '', updated_at = ?2
         WHERE pair_id = ?3 AND canonical_uuid = ?4",
        params![PHASE_SHIPPING, now, pair_id, canonical_uuid],
    )?;
    if updated == 0 {
        let ctx = encode_evidence(pair_id, canonical_uuid, "context", &InflightCtx {
            translation_scope: scope.into(),
            ..Default::default()
        })?;
        let inserted = tx.execute(
            "INSERT INTO sync_inflight
             (pair_id, canonical_uuid, phase, ctx_json, relayed_json, attempts, error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, '', 1, '', ?5, ?5)",
            params![pair_id, canonical_uuid, PHASE_SHIPPING,ctx,now],
        )?;
        ensure!(inserted == 1, "sync inflight admission was not committed");
    }
    tx.commit()?;
    Ok(())
}

/// Find the resumable shipping row for one unit. `None` = no live row
/// (caller runs the fresh path).
pub(crate) async fn find_shipping(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
) -> Result<Option<InflightRow>> {
    let conn = db.lock().await;
    let found = conn.query_row(
        "SELECT ctx_json, relayed_json, attempts, phase FROM sync_inflight
         WHERE pair_id = ?1 AND canonical_uuid = ?2",
        params![pair_id, canonical_uuid],
        |row| {
            let ctx_json: String = row.get(0)?;
            let relayed_json: String = row.get(1)?;
            let attempts: i64 = row.get(2)?;
            let phase: String = row.get(3)?;
            Ok((ctx_json, relayed_json, attempts, phase))
        },
    );
    match found {
        Ok((ctx_json, relayed_json, attempts, phase)) => {
            ensure!(
                phase == PHASE_SHIPPING,
                "sync inflight phase is unknown; original retained"
            );
            Ok(Some(InflightRow {
                ctx: decode_evidence(pair_id, canonical_uuid, "context", &ctx_json)?,
                relayed_json: if relayed_json.is_empty() { relayed_json } else {
                    decode_evidence(pair_id, canonical_uuid, "packet", &relayed_json)?
                },
                attempts,
            }))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// All resumable shipping rows of one pair (the recovery sweep surface).
pub(crate) async fn list_pair_shipping(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
) -> Result<Vec<(String, InflightRow)>> {
    let conn = db.lock().await;
    let mut stmt = conn.prepare(
        "SELECT canonical_uuid, ctx_json, relayed_json, attempts FROM sync_inflight
         WHERE pair_id = ?1 AND phase = ?2 ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map(params![pair_id, PHASE_SHIPPING], |row| {
            let uuid: String = row.get(0)?;
            let ctx_json: String = row.get(1)?;
            let relayed_json: String = row.get(2)?;
            let attempts: i64 = row.get(3)?;
            Ok((uuid, ctx_json, relayed_json, attempts))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(uuid, ctx_json, relayed_json, attempts)| {
            let ctx = decode_evidence(pair_id, &uuid, "context", &ctx_json)?;
            let relayed_json = if relayed_json.is_empty() { relayed_json } else {
                decode_evidence(pair_id, &uuid, "packet", &relayed_json)?
            };
            Ok((
                uuid,
                InflightRow {
                    ctx,
                    relayed_json,
                    attempts,
                },
            ))
        })
        .collect()
}

/// Persist the paid translation snapshot (read-modify-write under the db
/// lock so concurrent phase writes cannot lose each other).
pub(crate) async fn store_translation(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    translation: &RelayTranslation,
) -> Result<()> {
    merge_ctx(db, pair_id, canonical_uuid, |ctx, _| {
        if let Some(prior) = &ctx.translation {
            ensure!(prior.title.as_ref().is_none_or(|value| translation.title.as_ref() == Some(value))
                && prior.content.as_ref().is_none_or(|value| translation.content.as_ref() == Some(value))
                && prior.excerpt.as_ref().is_none_or(|value| translation.excerpt.as_ref() == Some(value)),
                "paid sync fields cannot be replaced; original retained");
        }
        ctx.translation = Some(translation.clone());
        Ok(())
    })
    .await
}

pub(crate) async fn store_translation_scope(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    scope: &str,
) -> Result<()> {
    merge_ctx(db, pair_id, canonical_uuid, |ctx, _| {
        ensure!(ctx.translation_scope.is_empty() || ctx.translation_scope == scope,
            "paid sync scope cannot be replaced; original retained");
        ctx.translation_scope = scope.to_owned();
        Ok(())
    })
    .await
}

/// Persist one finished media transfer (per-asset paid side effect).
pub(crate) async fn store_media_entry(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    remote_url: &str,
    target_url: &str,
) -> Result<()> {
    merge_ctx(db, pair_id, canonical_uuid, |ctx, _| {
        ensure!(ctx.media_url_map.get(remote_url).is_none_or(|prior| prior == target_url),
            "paid sync media cannot be replaced; original retained");
        ctx.media_url_map
            .insert(remote_url.to_string(), target_url.to_string());
        Ok(())
    })
    .await
}

pub(crate) async fn store_target_receipt(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    receipt: &crate::sync_engine::shipper::ShipResult,
) -> Result<()> {
    let payload = receipt.receipt.as_ref().context("sync receipt has no committed payload")?;
    ensure!(receipt.success && payload["committed"] == true
        && payload["packet_id"] == receipt.packet_id
        && payload["canonical_uuid"] == canonical_uuid
        && matches!(payload["status"].as_str(), Some("success" | "skipped")),
        "sync receipt is not a committed target completion");
    merge_ctx(db, pair_id, canonical_uuid, |ctx, relayed| {
        ensure!(ctx.delivery_phase.as_deref() == Some("submitted") && !relayed.is_empty(),
            "sync receipt has no frozen submitted packet");
        let packet: serde_json::Value = serde_json::from_str(relayed)?;
        ensure!(payload["packet_id"] == packet["packet_id"]
            && payload["fingerprint_ack"] == packet["source_fingerprint"]
            && payload["origin_site_uuid"] == packet["origin_context"]["origin_site_uuid"]
            && payload["target_lang"] == packet["target_lang"],
            "sync receipt belongs to another original packet");
        if let Some(prior) = &ctx.target_receipt {
            ensure!(serde_json::to_value(prior)? == serde_json::to_value(receipt)?,
                "original sync receipt cannot be replaced; retained");
            return Ok(());
        }
        ctx.target_receipt = Some(receipt.clone());
        Ok(())
    }).await
}

pub(crate) async fn mark_submitted(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
) -> Result<()> {
    merge_ctx(db, pair_id, canonical_uuid, |ctx, relayed| {
        ensure!(!relayed.is_empty()
            && matches!(ctx.delivery_phase.as_deref(), Some("prepared" | "submitted")),
            "sync submission has no prepared original packet");
        ctx.delivery_phase = Some("submitted".into());
        Ok(())
    }).await
}

/// Record the packet action + the serialized relayed packet. Replays use
/// the SAME packet_id (target-side fingerprint ack makes the re-push
/// idempotent), so this must be written BEFORE the push.
pub(crate) async fn store_relayed_packet(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    action: &str,
    relayed_json: &str,
) -> Result<()> {
    let now = unix_ts() as i64;
    let mut connection = db.lock().await;
    let conn = connection.savepoint()?;
    let (ctx_json, prior_packet): (String, String) = conn
        .query_row(
            "SELECT ctx_json,relayed_json FROM sync_inflight WHERE pair_id = ?1 AND canonical_uuid = ?2",
            params![pair_id, canonical_uuid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("read sync inflight context for checkpoint failed")?;
    if !prior_packet.is_empty() {
        let prior: String = decode_evidence(pair_id, canonical_uuid, "packet", &prior_packet)?;
        ensure!(prior == relayed_json, "original relay packet cannot be replaced; retained");
        return Ok(());
    }
    let mut ctx: InflightCtx = decode_evidence(pair_id, canonical_uuid, "context", &ctx_json)?;
    ctx.action = action.to_string();
    ctx.delivery_phase = Some("prepared".into());
    let prior_ctx = ctx_json;
    let ctx_json = encode_evidence(pair_id, canonical_uuid, "context", &ctx)?;
    let packet_json = encode_evidence(pair_id, canonical_uuid, "packet", &relayed_json)?;
    let changed = conn.execute(
        "UPDATE sync_inflight SET ctx_json = ?1, relayed_json = ?2, updated_at = ?3
         WHERE pair_id = ?4 AND canonical_uuid = ?5 AND ctx_json = ?6 AND relayed_json = '' AND phase = ?7",
        params![ctx_json, packet_json, now, pair_id, canonical_uuid, prior_ctx, PHASE_SHIPPING],
    )?;
    ensure!(
        changed == 1,
        "sync inflight packet checkpoint was not committed; retained"
    );
    let saved: (String, String) = conn.query_row(
        "SELECT ctx_json,relayed_json FROM sync_inflight WHERE pair_id=?1 AND canonical_uuid=?2",
        params![pair_id, canonical_uuid], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(saved == (ctx_json, packet_json), "sync packet readback differs; retained");
    conn.commit()?;
    Ok(())
}

/// Keep a failed unit resumable with an error note (phase stays shipping —
/// the next pair run's recovery path retries the unit with the stored
/// paid snapshot instead of re-paying the vendor).
pub(crate) async fn note_error(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    error_snippet: &str,
) -> Result<()> {
    let now = unix_ts() as i64;
    let conn = db.lock().await;
    let changed = conn.execute(
        "UPDATE sync_inflight SET error = ?1, updated_at = ?2
         WHERE pair_id = ?3 AND canonical_uuid = ?4",
        params![error_snippet, now, pair_id, canonical_uuid],
    )?;
    ensure!(
        changed == 1,
        "sync inflight error note was not committed; retained"
    );
    Ok(())
}

/// Terminal closure: the unit reached a durable home (pair state after a
/// push ack, or the sync-review inbox after a park). Archive the exact
/// original encrypted evidence before releasing its active row identity.
pub(crate) async fn close_shipping(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
) -> Result<()> {
    let mut connection = db.lock().await;
    let conn = connection.savepoint()?;
    let original: (String, String, i64, String, i64, i64) = conn.query_row(
        "SELECT ctx_json,relayed_json,attempts,error,created_at,updated_at FROM sync_inflight
         WHERE pair_id=?1 AND canonical_uuid=?2 AND phase=?3",
        params![pair_id, canonical_uuid, PHASE_SHIPPING],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    let _: InflightCtx = decode_evidence(pair_id, canonical_uuid, "context", &original.0)?;
    if !original.1.is_empty() {
        let _: String = decode_evidence(pair_id, canonical_uuid, "packet", &original.1)?;
    }
    let key = crate::sync_engine::hmac::sha256_hex(
        &serde_json::to_vec(&(pair_id, canonical_uuid, &original))?,
    );
    conn.execute(
        "INSERT OR IGNORE INTO sync_delivery_archive
         (evidence_key,pair_id,canonical_uuid,ctx_json,relayed_json,attempts,error,created_at,updated_at,closed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![key, pair_id, canonical_uuid, original.0, original.1, original.2,
            original.3, original.4, original.5, unix_ts() as i64],
    )?;
    let saved: (String, String, i64, String, i64, i64) = conn.query_row(
        "SELECT ctx_json,relayed_json,attempts,error,created_at,updated_at FROM sync_delivery_archive
         WHERE evidence_key=?1 AND pair_id=?2 AND canonical_uuid=?3",
        params![key, pair_id, canonical_uuid],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )?;
    ensure!(saved == original, "sync archival readback differs; original retained");
    let changed = conn.execute(
        "DELETE FROM sync_inflight WHERE pair_id = ?1 AND canonical_uuid = ?2
         AND phase=?3 AND ctx_json=?4 AND relayed_json=?5",
        params![pair_id, canonical_uuid, PHASE_SHIPPING, original.0, original.1],
    )?;
    ensure!(
        changed == 1,
        "sync inflight closure was not committed; retained"
    );
    conn.commit()?;
    Ok(())
}

/// Read-only inventory. Age cannot prove that a paid or unknown unit is safe
/// to delete; resumption uses the original shipping identity.
pub(crate) fn sync_inflight_inventory(conn: &Connection) -> Result<usize> {
    let shipping: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_inflight WHERE phase = ?1",
        params![PHASE_SHIPPING],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(shipping)?)
}

async fn merge_ctx<F>(
    db: &Arc<Mutex<Connection>>,
    pair_id: &str,
    canonical_uuid: &str,
    merge: F,
) -> Result<()>
where
    F: FnOnce(&mut InflightCtx, &str) -> Result<()>,
{
    let now = unix_ts() as i64;
    let mut connection = db.lock().await;
    let conn = connection.savepoint()?;
    let (ctx_json, packet_json): (String, String) = conn
        .query_row(
            "SELECT ctx_json,relayed_json FROM sync_inflight WHERE pair_id = ?1 AND canonical_uuid = ?2",
            params![pair_id, canonical_uuid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("read sync inflight context for checkpoint failed")?;
    let mut ctx: InflightCtx = decode_evidence(pair_id, canonical_uuid, "context", &ctx_json)?;
    let packet = if packet_json.is_empty() { String::new() } else {
        decode_evidence(pair_id, canonical_uuid, "packet", &packet_json)?
    };
    merge(&mut ctx, &packet)?;
    let encoded = encode_evidence(pair_id, canonical_uuid, "context", &ctx)?;
    let affected = conn.execute(
        "UPDATE sync_inflight SET ctx_json = ?1, updated_at = ?2
         WHERE pair_id = ?3 AND canonical_uuid = ?4 AND ctx_json = ?5 AND phase = ?6 AND relayed_json=?7",
        params![encoded, now, pair_id, canonical_uuid, ctx_json, PHASE_SHIPPING, packet_json],
    )?;
    anyhow::ensure!(affected == 1, "sync inflight checkpoint row is missing");
    let readback: String = conn.query_row(
        "SELECT ctx_json FROM sync_inflight WHERE pair_id=?1 AND canonical_uuid=?2",
        params![pair_id, canonical_uuid], |row| row.get(0),
    )?;
    ensure!(readback == encoded, "sync evidence readback differs; retained");
    conn.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
