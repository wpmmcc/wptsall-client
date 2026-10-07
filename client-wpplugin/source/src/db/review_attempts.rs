//! An explicit manual request resumes one fee generation, including lost replies.
use super::jobs::TranslationItem;
use super::unit_lock::UnitLease;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) mod plan;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    format: String,
    item_id: i64,
    generation: String,
    binding: String,
    plan: Value,
    candidate: Option<Receipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encoded_result: Option<String>,
    result: Option<Receipt>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub(crate) translated_path: String,
    pub(crate) component_id: String,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
    sha256: String,
}

fn key(item: i64, request: &str) -> String {
    format!("review-request-v2:{item}:{request}")
}

fn load(conn: &Connection, item: i64, request: &str) -> Result<Option<(String, Attempt)>> {
    validate_request(request)?;
    let Some(raw) = super::system::get_system_config_checked(conn, &key(item, request))? else {
        return Ok(None);
    };
    ensure!(
        raw.starts_with("V1BUQw"),
        "manual attempt is not encrypted; retained"
    );
    let attempt: Attempt = serde_json::from_str(&super::system::decrypt_config_value(&raw)?)
        .context("damaged manual attempt; retained")?;
    ensure!(
        attempt.format == "review-attempt-v2"
            && attempt.item_id == item
            && attempt.generation == request
            && attempt.binding.len() == 64
            && attempt.binding.bytes().all(|b| b.is_ascii_hexdigit()),
        "manual request scope differs; retained"
    );
    Ok(Some((raw, attempt)))
}

pub(crate) fn validate_request(request: &str) -> Result<()> {
    ensure!(
        uuid::Uuid::parse_str(request).is_ok_and(|id| !id.is_nil() && id.to_string() == request),
        "request_id must be a non-nil canonical UUID"
    );
    Ok(())
}

fn current(conn: &Connection, item: i64) -> Result<Option<String>> {
    ensure!(
        super::system::get_system_config_checked(conn, &format!("review-attempt-v1:{item}"))?
            .is_none(),
        "legacy manual attempt requires explicit reconciliation; retained"
    );
    let Some(raw) =
        super::system::get_system_config_checked(conn, &format!("review-head-v2:{item}"))?
    else {
        return Ok(None);
    };
    ensure!(
        raw.starts_with("V1BUQw"),
        "manual request head is not encrypted; retained"
    );
    let request = super::system::decrypt_config_value(&raw)?;
    validate_request(&request)?;
    ensure!(
        load(conn, item, &request)?.is_some(),
        "manual request head has no attempt; retained"
    );
    Ok(Some(request))
}

pub(crate) fn replay(conn: &Connection, item: i64, request: &str) -> Result<Option<Receipt>> {
    let Some((_, attempt)) = load(conn, item, request)? else {
        return Ok(None);
    };
    let Some(receipt) = attempt.result else {
        return Ok(None);
    };
    let bytes = std::fs::read(&receipt.translated_path)
        .context("saved manual result missing; no new fee")?;
    use sha2::Digest;
    ensure!(
        receipt.sha256 == format!("{:x}", sha2::Sha256::digest(&bytes)),
        "saved manual result differs; retained"
    );
    Ok(Some(receipt))
}

pub(crate) fn saved_plan(conn: &Connection, item: i64, request: &str) -> Result<Option<Value>> {
    Ok(load(conn, item, request)?.map(|(_, attempt)| attempt.plan))
}

pub(crate) fn retained_plan(conn: &Connection, item: i64) -> Result<Option<Value>> {
    match current(conn, item)? {
        Some(request) => saved_plan(conn, item, &request),
        None => Ok(None),
    }
}

pub(crate) async fn recover_complete_file(
    db: &Arc<tokio::sync::Mutex<Connection>>,
    lease: &UnitLease,
    item: i64,
    request: &str,
) -> Result<Option<Receipt>> {
    let candidate = {
        let conn = db.lock().await;
        lease.assert_item(&conn, db, item)?;
        let Some((previous, attempt)) = load(&conn, item, request)? else {
            return Ok(None);
        };
        if attempt.result.is_some() {
            return replay(&conn, item, request);
        }
        ensure!(
            current(&conn, item)?.as_deref() == Some(request),
            "manual request is not current; retained"
        );
        (attempt.candidate, attempt.encoded_result, previous)
    };
    let (Some(candidate), encoded_result, original) = candidate else {
        return Ok(None);
    };
    let bytes = match std::fs::read(&candidate.translated_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let Some(encoded) = encoded_result else {
                return Ok(None);
            };
            let bytes = BASE64_STANDARD.decode(encoded)?;
            use sha2::Digest;
            ensure!(
                bytes.starts_with(b"WPTC")
                    && candidate.sha256 == format!("{:x}", sha2::Sha256::digest(&bytes)),
                "frozen manual ciphertext differs; retained"
            );
            let value: Value = serde_json::from_str(&crate::bindings::decrypt_from_bytes(&bytes)?)?;
            crate::task_engine::pipeline::install_json_snapshot(
                std::path::Path::new(&candidate.translated_path),
                &value,
                &bytes,
            )?;
            bytes
        }
        Err(error) => return Err(error).context("inspect retained manual candidate"),
    };
    use sha2::Digest;
    ensure!(
        candidate.sha256 == format!("{:x}", sha2::Sha256::digest(&bytes)),
        "manual candidate differs; retained"
    );
    let envelope: Value = serde_json::from_str(&crate::bindings::load_encrypted_or_plain(
        std::path::Path::new(&candidate.translated_path),
    )?)?;
    let _identity = envelope
        .get("idempotency_key")
        .and_then(Value::as_str)
        .context("manual candidate identity missing")?;
    let (scope, saved_item) = {
        let conn = db.lock().await;
        let (previous, attempt) = load(&conn, item, request)?.context("manual request missing")?;
        ensure!(
            previous == original && current(&conn, item)?.as_deref() == Some(request),
            "manual candidate authority changed; original retained"
        );
        let saved_item =
            super::jobs::get_item_checked(&conn, item)?.context("manual item missing")?;
        result_languages(&envelope, &saved_item)?;
        let scope = super::async_jobs::AsyncJobScope {
            db: db.clone(),
            domain: saved_item.domain.clone(),
            relation_id: saved_item.relation_id,
            object_type: saved_item.object_type.clone(),
            object_id: saved_item.wp_object_id,
            source_snapshot: Some(
                json!({"manual_generation":attempt.generation,"binding":attempt.binding}),
            ),
        };
        (scope, saved_item)
    };
    let i18n = if envelope["payload_type"] == "i18n_language_pack" {
        Some(crate::task_engine::pipeline::language_pack::read_envelope(
            &envelope,
            &saved_item,
        )?)
    } else {
        None
    };
    let mut conn = db.lock().await;
    lease.assert_item(&conn, db, item)?;
    ensure!(
        load(&conn, item, request)?.map(|(raw, _)| raw).as_deref() == Some(original.as_str()),
        "manual candidate authority changed; original retained"
    );
    let credit = match conn
        .path()
        .filter(|path| !path.is_empty() && original.starts_with("V1BUQw"))
    {
        Some(path) => {
            crate::storage_capacity::database_recovery_credit(std::path::Path::new(path), false)?
        }
        None => None,
    };
    crate::storage_capacity::with_database_credit(credit, || -> Result<()> {
        let tx = conn.savepoint()?;
        lease.assert_item(&tx, db, item)?;
        ensure!(
            current(&tx, item)?.as_deref() == Some(request)
                && load(&tx, item, request)?.map(|(raw, _)| raw).as_deref()
                    == Some(original.as_str())
                && std::fs::read(&candidate.translated_path)? == bytes,
            "manual candidate authority or file changed; original retained"
        );
        if let Some(envelope) = &i18n {
            crate::task_engine::pipeline::language_pack::project_saved_manual_result(
                &tx,
                lease,
                db,
                item,
                envelope,
                &candidate.translated_path,
                &scope,
            )?;
        } else {
            let payload: crate::types::TranslationCallbackPayload =
                serde_json::from_value(envelope["payload"].clone())?;
            crate::task_engine::pipeline::project_saved_translation_claimed(
                &tx,
                lease,
                db,
                item,
                &payload,
                &candidate.translated_path,
                Some(&scope),
            )?;
        }
        tx.commit()?;
        Ok(())
    })?;
    drop(conn);
    replay(&*db.lock().await, item, request)
}

pub(crate) fn unfinished(conn: &Connection, item: i64) -> Result<bool> {
    let Some(request) = current(conn, item)? else {
        return Ok(false);
    };
    Ok(load(conn, item, &request)?
        .context("manual request missing")?
        .1
        .result
        .is_none())
}

pub(crate) fn pending_request(conn: &Connection, item: i64) -> Result<Option<String>> {
    let Some(request) = current(conn, item)? else {
        return Ok(None);
    };
    Ok(load(conn, item, &request)?
        .context("manual request missing")?
        .1
        .result
        .is_none()
        .then_some(request))
}

pub(crate) fn scope(
    conn: &Connection,
    db: &Arc<tokio::sync::Mutex<Connection>>,
    lease: &UnitLease,
    item: &TranslationItem,
    raw: &Value,
    request: &str,
) -> Result<super::async_jobs::AsyncJobScope> {
    lease.assert_item(conn, db, item.id)?;
    validate_request(request)?;
    let binding = super::system::private_json_digest(&json!({
        "domain": item.domain, "relation_id": item.relation_id, "business_line":item.business_line,
        "object_type": item.object_type, "object_id": item.wp_object_id,
        "subtype":item.wp_object_subtype, "task_type":item.task_type,
        "component": item.selected_component_id.as_ref().unwrap_or(&item.component_id),
        "source_lang": item.source_lang, "target_lang": item.target_lang,
        "effective_source_lang": item.effective_source_lang,
        "effective_target_lang": item.effective_target_lang,
        "overrides": item.editable_overrides, "raw": raw,
    }))?;
    let attempt = if let Some((_, saved)) = load(conn, item.id, request)? {
        ensure!(
            saved.binding == binding,
            "manual request differs; original paid evidence retained"
        );
        ensure!(
            current(conn, item.id)?.as_deref() == Some(request),
            "manual request is not current; retained"
        );
        saved
    } else {
        if let Some(head) = current(conn, item.id)? {
            ensure!(
                load(conn, item.id, &head)?
                    .context("manual request missing")?
                    .1
                    .result
                    .is_some(),
                "unfinished manual request must resume its original request_id; no new fee"
            );
        }
        let saved = Attempt {
            format: "review-attempt-v2".into(),
            item_id: item.id,
            generation: request.into(),
            binding,
            plan: raw.clone(),
            candidate: None,
            encoded_result: None,
            result: None,
        };
        crate::storage_capacity::with_database_credit(None, || -> Result<()> {
            let tx = conn.unchecked_transaction()?;
            lease.assert_item(&tx, db, item.id)?;
            let encoded = super::system::encrypt_config_value(&serde_json::to_string(&saved)?)?;
            ensure!(
                tx.execute(
                    "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                    params![key(item.id, request), encoded]
                )? == 1,
                "manual request was not committed; no new fee"
            );
            let head_key = format!("review-head-v2:{}", item.id);
            let prior = super::system::get_system_config_checked(&tx, &head_key)?;
            let next = super::system::encrypt_config_value(request)?;
            let changed = match prior {
                Some(prior) => tx.execute(
                    "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                    params![next, head_key, prior],
                )?,
                None => tx.execute(
                    "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                    params![head_key, next],
                )?,
            };
            ensure!(
                changed == 1,
                "manual request head was not committed; no new fee"
            );
            ensure!(
                current(&tx, item.id)?.as_deref() == Some(request),
                "manual request head differs; no new fee"
            );
            lease.assert_item(&tx, db, item.id)?;
            tx.commit()?;
            Ok(())
        })?;
        saved
    };
    Ok(super::async_jobs::AsyncJobScope {
        db: db.clone(),
        domain: item.domain.clone(),
        relation_id: item.relation_id,
        object_type: item.object_type.clone(),
        object_id: item.wp_object_id,
        source_snapshot: Some(
            json!({"manual_generation": attempt.generation, "binding": attempt.binding}),
        ),
    })
}

pub(crate) async fn prepare_result(
    db: &Arc<tokio::sync::Mutex<Connection>>,
    lease: &UnitLease,
    scope: &super::async_jobs::AsyncJobScope,
    item_id: i64,
    envelope: &Value,
    path: &str,
) -> Result<Vec<u8>> {
    let request = scope
        .source_snapshot
        .as_ref()
        .and_then(|v| v.get("manual_generation"))
        .and_then(Value::as_str)
        .context("manual result request missing")?;
    let mut conn = db.lock().await;
    let tx = conn.savepoint()?;
    lease.assert_item(&tx, db, item_id)?;
    let item = super::jobs::get_item_checked(&tx, item_id)?.context("manual item missing")?;
    ensure!(
        !matches!(item.status.as_str(), "done" | "skipped"),
        "manual result cannot reset delivered item"
    );
    let (source_lang, target_lang) = result_languages(envelope, &item)?;
    let (previous, mut attempt) = load(&tx, item_id, request)?.context("manual request missing")?;
    ensure!(
        attempt.result.is_none(),
        "completed manual request cannot replace its saved receipt"
    );
    ensure!(
        current(&tx, item_id)?.as_deref() == Some(request)
            && scope.source_snapshot.as_ref()
                == Some(&json!({"manual_generation":request,"binding":attempt.binding}))
            && Arc::ptr_eq(db, &scope.db),
        "manual result scope differs; retained"
    );
    use sha2::Digest;
    let result_bytes = if let Some(encoded) = &attempt.encoded_result {
        let bytes = BASE64_STANDARD.decode(encoded)?;
        let saved: Value = serde_json::from_str(&crate::bindings::decrypt_from_bytes(&bytes)?)?;
        ensure!(saved == *envelope, "frozen manual result differs; retained");
        bytes
    } else if envelope["payload_type"] == "i18n_language_pack" {
        crate::task_engine::pipeline::save_immutable_encrypted_json(
            std::path::Path::new(path),
            envelope,
        )?;
        std::fs::read(path)?
    } else if attempt.candidate.is_some() && !std::path::Path::new(path).exists() {
        // A legacy candidate already froze plaintext physical bytes.
        serde_json::to_vec_pretty(envelope)?
    } else {
        crate::task_engine::pipeline::prepare_json_snapshot(std::path::Path::new(path), envelope)?
    };
    let candidate = Receipt {
        translated_path: path.into(),
        component_id: item.selected_component_id.unwrap_or(item.component_id),
        source_lang,
        target_lang,
        sha256: format!("{:x}", sha2::Sha256::digest(&result_bytes)),
    };
    if let Some(saved) = &attempt.candidate {
        ensure!(
            saved == &candidate,
            "manual candidate differs; original retained"
        );
    } else {
        attempt.candidate = Some(candidate);
        if result_bytes.starts_with(b"WPTC") && envelope["payload_type"] != "i18n_language_pack" {
            attempt.encoded_result = Some(BASE64_STANDARD.encode(&result_bytes));
        }
        let next = super::system::encrypt_config_value(&serde_json::to_string(&attempt)?)?;
        ensure!(
            tx.execute(
                "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                params![next, key(item_id, request), previous]
            )? == 1,
            "manual candidate was not committed; no result overwrite"
        );
        ensure!(
            super::system::get_system_config_checked(&tx, &key(item_id, request))?.as_deref()
                == Some(next.as_str()),
            "manual candidate readback differs; original retained"
        );
    }
    lease.assert_item(&tx, db, item_id)?;
    tx.commit()?;
    Ok(result_bytes)
}

fn result_languages(envelope: &Value, item: &TranslationItem) -> Result<(String, String)> {
    if envelope["payload_type"] == "i18n_language_pack" {
        let envelope = crate::task_engine::pipeline::language_pack::read_envelope(envelope, item)?;
        ensure!(
            envelope.payload.source_lang
                == item
                    .effective_source_lang
                    .as_deref()
                    .unwrap_or(&item.source_lang)
                && envelope.payload.target_lang
                    == item
                        .effective_target_lang
                        .as_deref()
                        .unwrap_or(&item.target_lang),
            "manual language-pack result languages differ; retained"
        );
        return Ok((envelope.payload.source_lang, envelope.payload.target_lang));
    }
    let payload: crate::types::TranslationCallbackPayload = serde_json::from_value(
        envelope
            .get("payload")
            .cloned()
            .context("manual saved result missing payload")?,
    )?;
    ensure!(
        payload.relation_id == u64::try_from(item.relation_id)?
            && payload.object_id == u64::try_from(item.wp_object_id)?
            && super::pending_callbacks::normalize_pending_object_type(&payload.object_type)?
                == super::pending_callbacks::normalize_pending_object_type(&item.object_type)?,
        "manual saved result differs from item; original attempt retained"
    );
    Ok((payload.source_lang, payload.target_lang))
}

pub(crate) fn finish(
    conn: &Connection,
    db: &Arc<tokio::sync::Mutex<Connection>>,
    lease: &UnitLease,
    scope: &super::async_jobs::AsyncJobScope,
    item_id: i64,
) -> Result<()> {
    lease.assert_item(conn, db, item_id)?;
    let item = super::jobs::get_item_checked(conn, item_id)?
        .context("manual item missing before result save")?;
    ensure!(
        matches!(item.status.as_str(), "translated" | "pending_review")
            && !item.translated_path.is_empty(),
        "manual result is not durably saved; original attempt retained"
    );
    let envelope: Value = serde_json::from_str(&crate::bindings::load_encrypted_or_plain(
        std::path::Path::new(&item.translated_path),
    )?)
    .context("manual saved result is damaged; retained")?;
    let (source_lang, target_lang) = result_languages(&envelope, &item)?;
    let request = scope
        .source_snapshot
        .as_ref()
        .and_then(|value| value.get("manual_generation"))
        .and_then(Value::as_str)
        .context("manual result request missing")?;
    let (previous, mut attempt) =
        load(conn, item_id, request)?.context("manual attempt missing before result save")?;
    ensure!(
        current(conn, item_id)?.as_deref() == Some(request),
        "manual request head changed; retained"
    );
    ensure!(
        scope.source_snapshot.as_ref()
            == Some(&json!({
                "manual_generation":attempt.generation, "binding":attempt.binding,
            }))
            && Arc::ptr_eq(db, &scope.db),
        "manual result generation differs; original retained"
    );
    use sha2::Digest;
    let sha256 = format!(
        "{:x}",
        sha2::Sha256::digest(std::fs::read(&item.translated_path)?)
    );
    let receipt = Receipt {
        translated_path: item.translated_path,
        component_id: item.selected_component_id.unwrap_or(item.component_id),
        source_lang,
        target_lang,
        sha256,
    };
    ensure!(
        attempt.result.is_none() && attempt.candidate.as_ref() == Some(&receipt),
        "manual candidate differs or request already finished; original retained"
    );
    attempt.result = Some(receipt);
    let next = super::system::encrypt_config_value(&serde_json::to_string(&attempt)?)?;
    ensure!(
        conn.execute(
            "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
            params![next, key(item_id, request), previous],
        )? == 1,
        "manual result receipt was not committed; original attempt retained"
    );
    ensure!(
        super::system::get_system_config_checked(conn, &key(item_id, request))?.as_deref()
            == Some(next.as_str()),
        "manual result receipt readback differs; original attempt retained"
    );
    Ok(())
}
