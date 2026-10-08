//! GAP-04 (tasks/client/06 §2.4, 批 I 2026-09-23): durable async provider
//! jobs.
//!
//! Components with `async_poll` (video / document translation) submit a job
//! to the provider and then poll it. Before this module the polling state
//! (job_id + the submit-extracted render context) lived only on the runner's
//! stack: a client restart mid-poll orphaned the paid remote job and the WP
//! write-back never happened.
//!
//! Lifecycle, keyed on the translation unit (domain × relation × object ×
//! field × chunk × lane):
//!
//! ```text
//! submit ok ──▶ upsert_polling_job ──▶ poll (touch per attempt)
//!                                    ├─ done   ──▶ close_job (DELETE)
//!                                    └─ failed ──▶ mark_job_failed
//! crash mid-poll ──▶ row survives ('polling')
//! re-offer (WP outbox available_at) ──▶ find_polling_job hits
//!    ──▶ skip the submit, restore the ctx snapshot, resume polling
//! ```
//!
//! Re-entry rides the EXISTING WP re-offer machinery (outbox `available_at`
//! backoff re-lists the item) — no separate boot drain is needed for
//! correctness; `async_jobs_inventory` at boot only counts retained rows.
//! Age is not authorization to delete failed or in-flight provider jobs.
//!
//! Resume reads fail closed on SQL/decryption/shape errors. New snapshots are
//! encrypted before insertion. Durable submission/unknown-outcome handling
//! and full retention controls are separate W3/W4/W5 work.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::logging::unix_ts;

mod execution;
mod ledger;
pub(crate) use execution::ProviderExecution;


pub(crate) use ledger::reconciliation::{
    begin_provider_runtime_intent, commit_provider_reconciliation, commit_provider_submit_context,
    list_provider_operations, review_provider_operation, ProviderReview,
};
pub(crate) use ledger::{
    provider_result_storage_credit, recover_provider_operation, save_provider_job,
    save_provider_result, ProviderRecovery,
};

/// Status for an in-flight, resumable job. Only rows in this status are
/// returned by [`find_polling_job`].
pub(crate) const STATUS_POLLING: &str = "polling";
/// Terminal status for a job whose poll failed/timed out. Kept for audit,
/// never resumed automatically. The operation ledger blocks blind resubmission.
pub(crate) const STATUS_FAILED: &str = "failed";

// ---------------------------------------------------------------------------
// Envelope — threaded from the discovery executor down to the runner
// ---------------------------------------------------------------------------

/// The item-level scope of an async job: everything the executor knows at
/// `execute.rs` when it starts translating one content item.
#[derive(Clone)]
pub(crate) struct AsyncJobScope {
    pub db: Arc<Mutex<Connection>>,
    pub domain: String,
    pub relation_id: i64,
    pub object_type: String,
    pub object_id: i64,
    pub source_snapshot: Option<Value>,
}

/// The unit-level identity of one resumable async job: scope + which field
/// (and, for chunked text, which chunk) of the item it belongs to, plus the
/// lane ('text' | 'non_text') so a media_ref field and a text field on the
/// same object never collide.
#[derive(Clone)]
pub(crate) struct AsyncJobEnv {
    pub db: Arc<Mutex<Connection>>,
    pub domain: String,
    pub relation_id: i64,
    pub object_type: String,
    pub object_id: i64,
    pub field_name: String,
    pub chunk_index: i64,
    pub lane: &'static str,
    pub source_snapshot: Option<Value>,
    pub resume_binding: Option<String>,
}

impl AsyncJobScope {
    /// Derive the unit env for one field of the item.
    pub(crate) fn field_env(&self, field_name: &str, lane: &'static str) -> AsyncJobEnv {
        AsyncJobEnv {
            db: Arc::clone(&self.db),
            domain: self.domain.clone(),
            relation_id: self.relation_id,
            object_type: self.object_type.clone(),
            object_id: self.object_id,
            field_name: self
                .source_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.get("manual_generation"))
                .and_then(Value::as_str)
                .map(|request| format!("{field_name}@manual-{request}"))
                .unwrap_or_else(|| field_name.to_string()),
            chunk_index: 0,
            lane,
            source_snapshot: self.source_snapshot.clone(),
            resume_binding: None,
        }
    }
}

impl AsyncJobEnv {
    /// Bind before lookup, key selection or egress. Credentials remain the
    /// saved job's credentials; changing the template/input cannot retarget it.
    pub(crate) fn for_runtime(
        &self,
        runtime: &crate::types::ComponentRuntime,
        input: &Value,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<Self> {
        let mut next = self.clone();
        next.resume_binding = Some(Self::runtime_binding(
            &runtime.template,
            &runtime.language_map,
            &self.source_snapshot,
            input,
            source_lang,
            target_lang,
        )?);
        Ok(next)
    }

    fn runtime_binding(
        template: &crate::types::ComponentTemplate,
        language_map: &HashMap<String, String>,
        source_snapshot: &Option<Value>,
        input: &Value,
        source_lang: &str,
        target_lang: &str,
    ) -> Result<String> {
        super::system::private_json_digest(&json!({
            "template": template,
            "language_map": language_map,
            "source_snapshot": source_snapshot,
            "input": input,
            "source_lang": source_lang,
            "target_lang": target_lang,
        }))
    }

    /// The same unit at a different chunk index (chunking lane).
    pub(crate) fn with_chunk(&self, chunk_index: i64) -> AsyncJobEnv {
        let mut next = self.clone();
        next.chunk_index = chunk_index;
        next
    }

    /// A sub-unit of a structured field (serialized_php / json_structured
    /// translate leaf strings); the leaf path is appended to the field name
    /// so each leaf gets its own resumable job row.
    pub(crate) fn with_sub_path(&self, leaf_path: &str) -> AsyncJobEnv {
        let mut next = self.clone();
        if !leaf_path.trim().is_empty() {
            next.field_name = format!("{}[{}]", next.field_name, leaf_path);
        }
        next
    }
}

// ---------------------------------------------------------------------------
// Row type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct AsyncJobRow {
    pub job_id: String,
    /// Full render-context snapshot taken right after the submit response
    /// was extracted (auth + defaults + input + computed.*).
    pub ctx: HashMap<String, String>,
    pub attempts: i64,
}

struct StoredUnit {
    status: String,
    job: AsyncJobRow,
    component_id: String,
    source_lang: String,
    target_lang: String,
    resume_binding: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AsyncUnit {
    site_key: String,
    relation_id: i64,
    object_type: String,
    object_id: i64,
    field_name: String,
    chunk_index: i64,
    lane: String,
    source_snapshot: Option<Value>,
}

impl AsyncUnit {
    fn from_env(env: &AsyncJobEnv) -> Self {
        Self {
            site_key: super::system::private_site_key(&env.domain),
            relation_id: env.relation_id,
            object_type: env.object_type.clone(),
            object_id: env.object_id,
            field_name: env.field_name.clone(),
            chunk_index: env.chunk_index,
            lane: env.lane.to_string(),
            source_snapshot: env.source_snapshot.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedAsyncContext {
    format: String,
    unit: AsyncUnit,
    component_id: String,
    source_lang: String,
    target_lang: String,
    resume_binding: Option<String>,
    job_id: String,
    ctx: HashMap<String, String>,
}

fn read_unit(conn: &Connection, env: &AsyncJobEnv) -> Result<Option<StoredUnit>> {
    let mut statement = conn.prepare(
        "SELECT domain, status, job_id, ctx_json, attempts, component_id, source_lang, target_lang FROM async_jobs
         WHERE (domain = ?1 OR domain = ?8) AND relation_id = ?2 AND object_type = ?3
           AND object_id = ?4 AND field_name = ?5 AND chunk_index = ?6 AND lane = ?7
         LIMIT 2",
    )?;
    let rows = statement
        .query_map(
            params![
                super::system::private_site_key(&env.domain),
                env.relation_id,
                env.object_type,
                env.object_id,
                env.field_name,
                env.chunk_index,
                env.lane,
                env.domain
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > 1 {
        anyhow::bail!("ambiguous async unit; original provider jobs retained");
    }
    let Some((
        domain,
        status,
        job_id,
        stored_ctx,
        attempts,
        component_id,
        source_lang,
        target_lang,
    )) = rows.into_iter().next()
    else {
        return Ok(None);
    };
    if domain.starts_with("wptsall-site-v1:") && !stored_ctx.starts_with("V1BUQw") {
        anyhow::bail!("damaged encrypted async context; original row retained");
    }
    if !matches!(status.as_str(), STATUS_POLLING | STATUS_FAILED) {
        anyhow::bail!("invalid async job status; original row retained");
    }
    anyhow::ensure!(
        !job_id.trim().is_empty() && !component_id.trim().is_empty() && attempts >= 0,
        "invalid async job identity or attempts; original row retained"
    );
    let plain = super::system::decrypt_config_value(&stored_ctx)?;
    let value: Value = serde_json::from_str(&plain).context("invalid async resume context")?;
    // Legacy render variables are a string map, including `format` and `unit`.
    // A reserved marker or non-string shape must validate as an envelope,
    // never fall back to legacy after an envelope validation failure.
    let legacy_variables = value
        .as_object()
        .is_some_and(|variables| variables.values().all(Value::is_string))
        && !value
            .get("format")
            .and_then(Value::as_str)
            .is_some_and(|format| format.starts_with("wptsall-async-context-"));
    let (ctx, resume_binding) = if !legacy_variables {
        let saved: SavedAsyncContext =
            serde_json::from_value(value).context("invalid bound async resume context")?;
        anyhow::ensure!(
            saved.format == "wptsall-async-context-v1"
                && domain == super::system::private_site_key(&env.domain)
                && saved.unit == AsyncUnit::from_env(env)
                && saved.component_id == component_id
                && saved.source_lang == source_lang
                && saved.target_lang == target_lang
                && saved.job_id == job_id
                && env
                    .resume_binding
                    .as_ref()
                    .map_or(true, |expected| saved.resume_binding.as_ref()
                        == Some(expected)),
            "async resume scope mismatch; original provider job retained"
        );
        (saved.ctx, saved.resume_binding)
    } else {
        anyhow::ensure!(
            env.resume_binding.is_none(),
            "legacy async job has no proven runtime binding; original provider job retained"
        );
        (
            serde_json::from_value(value).context("invalid legacy async resume context")?,
            None,
        )
    };
    Ok(Some(StoredUnit {
        status,
        job: AsyncJobRow {
            job_id,
            ctx,
            attempts,
        },
        component_id,
        source_lang,
        target_lang,
        resume_binding,
    }))
}

// ---------------------------------------------------------------------------
// Operations (all take the env's db lock only for the duration of the op)
// ---------------------------------------------------------------------------

/// Persist a freshly submitted polling job, preserving an identical live job.
/// Standalone legacy rows may be superseded, but an operation ledger never
/// grants permission to reset failed evidence. This is not submit intent.
pub(crate) async fn upsert_polling_job(
    execution: &ProviderExecution,
    component_id: &str,
    job_id: &str,
    ctx: &HashMap<String, String>,
    source_lang: &str,
    target_lang: &str,
) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    anyhow::ensure!(
        !component_id.trim().is_empty()
            && !job_id.trim().is_empty()
            && !source_lang.is_empty()
            && !target_lang.is_empty(),
        "invalid async provider identity"
    );
    let snapshot = SavedAsyncContext {
        format: "wptsall-async-context-v1".to_string(),
        unit: AsyncUnit::from_env(env),
        component_id: component_id.to_string(),
        source_lang: source_lang.to_string(),
        target_lang: target_lang.to_string(),
        resume_binding: env.resume_binding.clone(),
        job_id: job_id.to_string(),
        ctx: ctx.clone(),
    };
    let ctx_json = super::system::encrypt_config_value(&serde_json::to_string(&snapshot)?)?;
    let now = unix_ts() as i64;
    let mut connection = env.db.lock().await;
    let conn = connection.savepoint()?;
    execution.assert_owner(&conn)?;
    ledger::assert_job_projection(&conn, env, &snapshot)?;
    if let Some(existing) = read_unit(&conn, env)? {
        if existing.status == STATUS_POLLING {
            anyhow::ensure!(
                existing.job.job_id == job_id
                    && existing.component_id == component_id
                    && existing.source_lang == source_lang
                    && existing.target_lang == target_lang
                    && existing.resume_binding == env.resume_binding
                    && existing.job.ctx == *ctx,
                "live async provider job conflicts with saved identity; original row retained"
            );
            conn.commit()?;
            return Ok(());
        }
    }
    // Replace a single validated legacy unit and its hashed identity together.
    // Failed insertion rolls back the deletion; ambiguous/damaged rows never
    // reach either statement. This is not durable provider submit intent.
    let credit = ledger::continuation_credit(&conn, env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        conn.execute(
            "DELETE FROM async_jobs WHERE domain = ?1 AND relation_id = ?2 AND object_type = ?3
         AND object_id = ?4 AND field_name = ?5 AND chunk_index = ?6 AND lane = ?7",
            params![
                env.domain,
                env.relation_id,
                env.object_type,
                env.object_id,
                env.field_name,
                env.chunk_index,
                env.lane
            ],
        )?;
        let affected = conn.execute(
            "INSERT OR REPLACE INTO async_jobs
         (domain, relation_id, object_type, object_id, field_name, chunk_index, lane,
          component_id, job_id, ctx_json, source_lang, target_lang,
          status, attempts, error, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,0,'',?14,?14)",
            params![
                super::system::private_site_key(&env.domain),
                env.relation_id,
                env.object_type,
                env.object_id,
                env.field_name,
                env.chunk_index,
                env.lane,
                component_id,
                job_id,
                ctx_json,
                source_lang,
                target_lang,
                STATUS_POLLING,
                now,
            ],
        )?;
        anyhow::ensure!(
            affected == 1,
            "async provider job was not saved; original row retained"
        );
        conn.commit()?;
        Ok(())
    })
}

/// Find a resumable ('polling') projection for this exact unit.
/// Callers must first consult the operation ledger: `None` does not authorize
/// a fresh submit. Failed rows remain retained and require reconciliation.
pub(crate) async fn find_polling_job(env: &AsyncJobEnv) -> Result<Option<AsyncJobRow>> {
    let conn = env.db.lock().await;
    Ok(read_unit(&conn, env)?
        .filter(|unit| unit.status == STATUS_POLLING)
        .map(|unit| unit.job))
}

/// Progress heartbeat: bump attempts/updated_at after each poll attempt.
/// Cheap (one UPDATE per poll interval) and gives crash forensics — "how
/// far did the poll get before the process died".
pub(crate) async fn touch_polling_job(execution: &ProviderExecution, attempts: i64) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    let now = unix_ts() as i64;
    let mut connection = env.db.lock().await;
    let conn = connection.savepoint()?;
    execution.assert_owner(&conn)?;
    ledger::assert_polling_operation(&conn, env)?;
    let existing = read_unit(&conn, env)?
        .ok_or_else(|| anyhow::anyhow!("async job not found for progress"))?;
    anyhow::ensure!(
        existing.status == STATUS_POLLING && attempts >= existing.job.attempts,
        "invalid async progress transition; original row retained"
    );
    let credit = ledger::continuation_credit(&conn, env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        let affected = conn.execute(
            "UPDATE async_jobs SET attempts = ?1, updated_at = ?2
         WHERE (domain = ?3 OR domain = ?11) AND relation_id = ?4 AND object_type = ?5
           AND object_id = ?6 AND field_name = ?7 AND chunk_index = ?8
           AND lane = ?9 AND status = ?10",
            params![
                attempts,
                now,
                super::system::private_site_key(&env.domain),
                env.relation_id,
                env.object_type,
                env.object_id,
                env.field_name,
                env.chunk_index,
                env.lane,
                STATUS_POLLING,
                env.domain,
            ],
        )?;
        anyhow::ensure!(
            affected == 1,
            "async progress was not saved; original row retained"
        );
        conn.commit()?;
        Ok(())
    })
}

/// Terminal failure: retain the row for audit (never resumed automatically).
pub(crate) async fn mark_job_failed(
    execution: &ProviderExecution,
    error_snippet: &str,
) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    let now = unix_ts() as i64;
    let mut connection = env.db.lock().await;
    let conn = connection.savepoint()?;
    execution.assert_owner(&conn)?;
    ledger::assert_polling_operation(&conn, env)?;
    let existing =
        read_unit(&conn, env)?.ok_or_else(|| anyhow::anyhow!("async job not found for failure"))?;
    anyhow::ensure!(
        existing.status == STATUS_POLLING,
        "async job is not polling; original row retained"
    );
    let credit = ledger::continuation_credit(&conn, env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        let affected = conn.execute(
            "UPDATE async_jobs SET status = ?1, error = ?2, updated_at = ?3
         WHERE (domain = ?4 OR domain = ?11) AND relation_id = ?5 AND object_type = ?6
           AND object_id = ?7 AND field_name = ?8 AND chunk_index = ?9
           AND lane = ?10",
            params![
                STATUS_FAILED,
                crate::logging::redact_string_for_log(error_snippet),
                now,
                super::system::private_site_key(&env.domain),
                env.relation_id,
                env.object_type,
                env.object_id,
                env.field_name,
                env.chunk_index,
                env.lane,
                env.domain,
            ],
        )?;
        anyhow::ensure!(
            affected == 1,
            "async failure was not saved; original row retained"
        );
        conn.commit()?;
        Ok(())
    })
}

/// Success closure: the unit is done, the row has no further value.
pub(crate) async fn close_job(execution: &ProviderExecution) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    let mut connection = env.db.lock().await;
    let conn = connection.savepoint()?;
    execution.assert_owner(&conn)?;
    ledger::assert_ready_operation(&conn, env)?;
    let existing =
        read_unit(&conn, env)?.ok_or_else(|| anyhow::anyhow!("async job not found for closure"))?;
    anyhow::ensure!(
        existing.status == STATUS_POLLING,
        "async job is not polling; original row retained"
    );
    let credit = ledger::continuation_credit(&conn, env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        let affected = conn.execute(
            "DELETE FROM async_jobs
         WHERE (domain = ?1 OR domain = ?8) AND relation_id = ?2 AND object_type = ?3
           AND object_id = ?4 AND field_name = ?5 AND chunk_index = ?6
           AND lane = ?7",
            params![
                super::system::private_site_key(&env.domain),
                env.relation_id,
                env.object_type,
                env.object_id,
                env.field_name,
                env.chunk_index,
                env.lane,
                env.domain,
            ],
        )?;
        anyhow::ensure!(
            affected == 1,
            "async job was not closed; original row retained"
        );
        conn.commit()?;
        Ok(())
    })
}

/// Non-destructive boot inventory. Unconfirmed provider jobs survive any age.
/// Sync on purpose: boot paths hold a bare Connection, not the async mutex.
pub(crate) fn async_jobs_inventory(conn: &Connection) -> Result<(usize, usize)> {
    let failed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM async_jobs WHERE status = ?1",
        params![STATUS_FAILED],
        |row| row.get(0),
    )?;
    let polling: i64 = conn.query_row(
        "SELECT COUNT(*) FROM async_jobs WHERE status = ?1",
        params![STATUS_POLLING],
        |row| row.get(0),
    )?;
    Ok((usize::try_from(failed)?, usize::try_from(polling)?))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
