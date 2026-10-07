//! Encrypted, compare-and-swap provider operations in the existing SQLite store.
//! No schema migration and no automatic retry of an unresolved submit.

use super::{
    read_unit, AsyncJobEnv, AsyncJobRow, AsyncUnit, ProviderExecution, SavedAsyncContext,
    StoredUnit,
};
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(super) mod reconciliation;



#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    format: String,
    unit: AsyncUnit,
    binding: String,
    attempt_id: String,
    component_id: String,
    source_lang: String,
    target_lang: String,
    state: String,
    job_id: Option<String>,
    ctx: HashMap<String, String>,
    result: Option<Value>,
    asset_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery: Option<reconciliation::RecoverySnapshot>,
}

pub(crate) enum ProviderRecovery {
    Vacant,
    Polling(AsyncJobRow),
    Ready(Value),
}

fn key(env: &AsyncJobEnv) -> Result<String> {
    ensure!(
        env.resume_binding.is_some(),
        "provider operation requires a runtime binding"
    );
    Ok(format!(
        "provider-operation-v1:{}",
        crate::db::system::private_json_digest(&json!({"unit": AsyncUnit::from_env(env)}))?
    ))
}

fn load(conn: &Connection, env: &AsyncJobEnv) -> Result<Option<(String, Operation)>> {
    let Some(raw) = crate::db::system::get_system_config_checked(conn, &key(env)?)? else {
        return Ok(None);
    };
    let value = decode(&raw)?;
    ensure!(
        value.unit == AsyncUnit::from_env(env)
            && Some(&value.binding) == env.resume_binding.as_ref(),
        "provider operation scope mismatch; retained"
    );
    Ok(Some((raw, value)))
}

fn decode(raw: &str) -> Result<Operation> {
    ensure!(
        raw.starts_with("V1BUQw"),
        "provider operation is not encrypted; retained"
    );
    let value: Operation = serde_json::from_str(&crate::db::system::decrypt_config_value(raw)?)
        .context("damaged provider operation; retained")?;
    ensure!(
        value.format == "provider-operation-v1"
            && uuid::Uuid::parse_str(&value.attempt_id)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == value.attempt_id)
            && !value.component_id.is_empty()
            && !value.source_lang.is_empty()
            && !value.target_lang.is_empty(),
        "provider operation scope mismatch; retained"
    );
    match value.state.as_str() {
        "submit_unknown" => ensure!(
            value.job_id.is_none() && value.result.is_none() && value.asset_sha256.is_none(),
            "damaged submit intent; retained"
        ),
        "polling" => ensure!(
            value.job_id.as_ref().is_some_and(|v| !v.is_empty())
                && value.result.is_none()
                && value.asset_sha256.is_none(),
            "damaged provider job; retained"
        ),
        "result_ready" => ensure!(
            value.result.is_some(),
            "missing saved provider result; retained"
        ),
        _ => anyhow::bail!("invalid provider operation state; retained"),
    }
    if let Some(snapshot) = &value.recovery {
        snapshot.validate(&value)?;
    }
    Ok(value)
}

fn install(
    conn: &Connection,
    env: &AsyncJobEnv,
    prior: Option<&str>,
    value: &Operation,
) -> Result<()> {
    let encoded = crate::db::system::encrypt_config_value(&serde_json::to_string(value)?)?;
    if prior.is_none() {
        let minimum = if value.unit.lane == "text" {
            crate::db::capacity::TEXT_RESERVATION_BYTES
        } else {
            crate::db::capacity::MEDIA_RESERVATION_BYTES
        };
        let frozen_bytes = u64::try_from(encoded.len())?
            .checked_mul(4)
            .context("provider retained-capacity size overflow")?;
        if value.state == "polling" {
            let row = read_unit(conn, env)?.context("existing capacity has no paid projection")?;
            assert_projection_matches(value, &row)?;
            crate::db::capacity::record_existing(conn, &key(env)?, minimum.max(frozen_bytes))?;
        } else {
            crate::db::capacity::reserve_new(conn, &key(env)?, minimum.max(frozen_bytes))?;
        }
    }
    let n = match prior {
        Some(raw) => conn.execute(
            "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
            params![encoded, key(env)?, raw],
        )?,
        None => conn.execute(
            "INSERT INTO system_config (key,value) VALUES (?1,?2)",
            params![key(env)?, encoded],
        )?,
    };
    ensure!(
        n == 1
            && crate::db::system::get_system_config_checked(conn, &key(env)?)?.as_deref()
                == Some(encoded.as_str()),
        "provider operation was not committed; original retained"
    );
    Ok(())
}

fn asset_digest(result: &Value) -> Result<Option<String>> {
    let Some(path) = result
        .get("translated_ref")
        .and_then(Value::as_str)
        .and_then(|s| s.strip_prefix("file://"))
    else {
        return Ok(None);
    };
    let root = std::path::PathBuf::from(crate::config::env_or(
        "WPTSALL_DATA_DIR",
        crate::config::DEFAULT_DATA_DIR,
    ))
    .join("provider-assets");
    let root = std::fs::canonicalize(root).context("saved provider asset root unavailable")?;
    let resolved =
        std::fs::canonicalize(path).context("saved provider asset unavailable; retained")?;
    ensure!(
        resolved.parent() == Some(root.as_path()),
        "provider asset is outside owned result storage"
    );
    let (digest, _) =
        crate::retained_assets::AssetReader::open(std::path::Path::new(path), u64::MAX)?
            .digest()?;
    Ok(Some(digest))
}

fn validate_result(env: &AsyncJobEnv, value: &Value) -> Result<()> {
    if env.lane == "text" {
        ensure!(
            value.as_str().is_some_and(|s| !s.trim().is_empty()),
            "invalid saved text result"
        );
    } else {
        ensure!(
            value.as_object().is_some_and(|m| m.len() == 2)
                && value.get("translated_ref").is_some_and(Value::is_string)
                && value.get("translated_text").is_some_and(Value::is_string)
                && ["translated_ref", "translated_text"].iter().any(|k| value
                    .get(*k)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())),
            "invalid saved media result"
        );
    }
    Ok(())
}

/// An existing, exact operation/projection may continue on previously
/// admitted database capacity. No vacant unit can borrow recovery credit.
pub(super) fn continuation_credit(
    conn: &Connection,
    env: &AsyncJobEnv,
) -> Result<Option<crate::storage_capacity::StorageCredit>> {
    let operation = if env.resume_binding.is_some() {
        load(conn, env)?
    } else {
        None
    };
    let projection = read_unit(conn, env)?;
    if let Some((_, operation)) = &operation {
        if let Some(row) = &projection {
            assert_projection_matches(operation, row)?;
        }
        if operation.state != "result_ready" {
            let credit = crate::storage_capacity::database_result_credit(conn, &key(env)?)?;
            if credit.is_some() {
                return Ok(credit);
            }
        }
    } else if projection.is_none() {
        return Ok(None);
    }
    match conn.path().filter(|path| !path.is_empty()) {
        Some(path) => {
            crate::storage_capacity::database_recovery_credit(std::path::Path::new(path), false)
        }
        None => Ok(None),
    }
}

pub(crate) async fn recover_provider_operation(env: &AsyncJobEnv) -> Result<ProviderRecovery> {
    let conn = env.db.lock().await;
    let Some((_, value)) = load(&conn, env)? else {
        return Ok(ProviderRecovery::Vacant);
    };
    let row = read_unit(&conn, env)?;
    if let Some(row) = &row {
        assert_projection_matches(&value, row)?;
    }
    match value.state.as_str() {
        "submit_unknown" => anyhow::bail!(
            "provider submit outcome unknown; reconciliation required, no automatic resubmit"
        ),
        "result_ready" => {
            let result = value.result.unwrap();
            validate_result(env, &result)?;
            ensure!(
                asset_digest(&result)? == value.asset_sha256,
                "saved provider asset changed; retained"
            );
            if crate::storage_capacity::release_confirmed_result(&conn, &key(env)?).is_err() {
                crate::logging::log_event_global(
                    "warn",
                    "storage.booking.retained",
                    serde_json::json!({"saved_result_replayed":true,"unused_booking_retained":true}),
                );
            }
            Ok(ProviderRecovery::Ready(result))
        }
        "polling" => {
            ensure!(
                row.as_ref()
                    .is_none_or(|r| r.status == super::STATUS_POLLING),
                "provider job failed; reconciliation required, no automatic repoll or resubmit"
            );
            Ok(ProviderRecovery::Polling(AsyncJobRow {
                job_id: value.job_id.unwrap(),
                ctx: value.ctx,
                attempts: row.map_or(0, |r| r.job.attempts),
            }))
        }
        _ => unreachable!(),
    }
}

pub(super) fn assert_polling_operation(conn: &Connection, env: &AsyncJobEnv) -> Result<()> {
    if env.resume_binding.is_some() {
        if let Some((_, op)) = load(conn, env)? {
            ensure!(
                op.state == "polling",
                "provider operation is not polling; retained"
            );
            if let Some(row) = read_unit(conn, env)? {
                assert_projection_matches(&op, &row)?;
            }
        }
    }
    Ok(())
}

fn assert_projection_matches(op: &Operation, row: &StoredUnit) -> Result<()> {
    ensure!(
        op.job_id.as_deref() == Some(row.job.job_id.as_str())
            && op.component_id == row.component_id
            && op.ctx == row.job.ctx
            && op.source_lang == row.source_lang
            && op.target_lang == row.target_lang,
        "provider ledger conflicts with async job; retained"
    );
    Ok(())
}

pub(super) fn assert_job_projection(
    conn: &Connection,
    env: &AsyncJobEnv,
    snapshot: &SavedAsyncContext,
) -> Result<()> {
    if env.resume_binding.is_some() {
        if let Some((_, op)) = load(conn, env)? {
            assert_polling_operation(conn, env)?;
            ensure!(
                op.job_id.as_deref() == Some(snapshot.job_id.as_str())
                    && op.component_id == snapshot.component_id
                    && op.ctx == snapshot.ctx
                    && op.source_lang == snapshot.source_lang
                    && op.target_lang == snapshot.target_lang,
                "provider projection conflicts with operation; retained"
            );
            ensure!(
                read_unit(conn, env)?.is_none_or(|row| row.status == super::STATUS_POLLING),
                "provider job failed; retained"
            );
        }
    }
    Ok(())
}

pub(super) fn assert_ready_operation(conn: &Connection, env: &AsyncJobEnv) -> Result<()> {
    if env.resume_binding.is_some() {
        let (_, op) = load(conn, env)?.context("provider completion has no saved operation")?;
        ensure!(
            op.state == "result_ready",
            "provider completion has no saved result; retained"
        );
        let row = read_unit(conn, env)?.context("provider completion has no projection")?;
        assert_projection_matches(&op, &row)?;
    }
    Ok(())
}



pub(crate) async fn save_provider_job(
    execution: &ProviderExecution,
    component: &str,
    job: &str,
    ctx: &HashMap<String, String>,
    source: &str,
    target: &str,
) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    ensure!(!job.is_empty(), "provider job id is empty");
    let mut db = env.db.lock().await;
    let tx = db.savepoint()?;
    execution.assert_owner(&tx)?;
    ensure!(
        read_unit(&tx, env)?.is_none_or(|row| row.status == super::STATUS_POLLING),
        "provider job failed; retained"
    );
    let prior = load(&tx, env)?;
    let mut value = if let Some((_, op)) = &prior {
        if let Some(row) = read_unit(&tx, env)? {
            assert_projection_matches(op, &row)?;
        }
        ensure!(
            op.component_id == component && op.source_lang == source && op.target_lang == target,
            "provider job identity mismatch"
        );
        if op.state == "polling" {
            ensure!(
                op.job_id.as_deref() == Some(job) && op.ctx == *ctx,
                "provider job changed; retained"
            );
            return Ok(());
        }
        ensure!(
            op.state == "submit_unknown",
            "provider result already saved"
        );
        op.clone()
    } else {
        let row = read_unit(&tx, env)?.context("provider job has no committed submit intent")?;
        ensure!(
            row.job.job_id == job
                && row.job.ctx == *ctx
                && row.component_id == component
                && row.source_lang == source
                && row.target_lang == target,
            "provider job recovery identity mismatch"
        );
        Operation {
            format: "provider-operation-v1".into(),
            unit: AsyncUnit::from_env(env),
            binding: env.resume_binding.clone().unwrap(),
            attempt_id: uuid::Uuid::new_v4().to_string(),
            component_id: component.into(),
            source_lang: source.into(),
            target_lang: target.into(),
            state: "polling".into(),
            job_id: Some(job.into()),
            ctx: ctx.clone(),
            result: None,
            asset_sha256: None,
            recovery: None,
        }
    };
    value.state = "polling".into();
    value.job_id = Some(job.into());
    value.ctx = ctx.clone();
    let credit = continuation_credit(&tx, env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        install(&tx, env, prior.as_ref().map(|p| p.0.as_str()), &value)?;
        tx.commit()?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

pub(crate) async fn save_provider_result(
    execution: &ProviderExecution,
    result: Value,
) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    validate_result(env, &result)?;
    let digest = asset_digest(&result)?;
    let mut db = env.db.lock().await;
    let tx = db.savepoint()?;
    execution.assert_owner(&tx)?;
    if let Some(row) = read_unit(&tx, env)? {
        ensure!(
            row.status == super::STATUS_POLLING,
            "provider job failed; late result retained"
        );
    }
    let (raw, mut op) = load(&tx, env)?.context("provider result has no operation")?;
    if let Some(row) = read_unit(&tx, env)? {
        assert_projection_matches(&op, &row)?;
    }
    if op.state == "result_ready" {
        ensure!(
            op.result.as_ref() == Some(&result) && op.asset_sha256 == digest,
            "conflicting provider result; original retained"
        );
        return Ok(());
    }
    ensure!(
        matches!(op.state.as_str(), "submit_unknown" | "polling"),
        "invalid result transition"
    );
    op.state = "result_ready".into();
    op.result = Some(result);
    op.asset_sha256 = digest;
    let credit = crate::storage_capacity::database_result_credit(&tx, &key(env)?)?;
    crate::storage_capacity::with_database_credit(credit, || {
        install(&tx, env, Some(&raw), &op)?;
        tx.commit()?;
        Ok::<(), anyhow::Error>(())
    })?;
    crate::storage_capacity::release_confirmed_result(&db, &key(env)?)?;
    Ok(())
}

pub(crate) async fn provider_result_storage_credit(
    execution: &ProviderExecution,
) -> Result<Option<crate::storage_capacity::StorageCredit>> {
    let env: &AsyncJobEnv = execution;
    let db = env.db.lock().await;
    execution.assert_owner(&db)?;
    let Some((_, operation)) = load(&db, env)? else {
        return Ok(None); // Legacy paid jobs have no new root booking.
    };
    ensure!(
        matches!(operation.state.as_str(), "submit_unknown" | "polling"),
        "result capacity does not belong to an unfinished operation"
    );
    crate::storage_capacity::result_credit(&db, &key(env)?)
}
