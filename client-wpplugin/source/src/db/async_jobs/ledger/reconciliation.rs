//! Evidence-only recovery. Unknown operations are never removed or resubmitted.
use super::*;
use crate::types::{ComponentRuntime, ComponentTemplate};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoverySnapshot {
    domain: String,
    template: ComponentTemplate,
    language_map: HashMap<String, String>,
    input: Value,
    submit_context_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proxy_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<ReconciliationEvidence>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconciliationEvidence {
    record: Value,
    sha256: String,
    verified_at: u64,
}

impl RecoverySnapshot {
    pub(super) fn validate(&self, op: &Operation) -> Result<()> {
        ensure!(
            crate::db::system::private_site_key(&self.domain) == op.unit.site_key
                && self.template.id == op.component_id
                && AsyncJobEnv::runtime_binding(
                    &self.template,
                    &self.language_map,
                    &op.unit.source_snapshot,
                    &self.input,
                    &op.source_lang,
                    &op.target_lang,
                )? == op.binding
                && op.ctx.get("operation.attempt_id") == Some(&op.attempt_id)
                && op.ctx.get("operation.binding") == Some(&op.binding),
            "provider recovery snapshot identity mismatch; retained"
        );
        if let Some(evidence) = &self.evidence {
            ensure!(
                op.state != "submit_unknown"
                    && evidence.verified_at > 0
                    && evidence.sha256 == crate::db::system::private_json_digest(&evidence.record)?,
                "provider reconciliation evidence changed; retained"
            );
            crate::component_rt::runner::provider_recovery::validate_record(
                &self.template,
                &op.attempt_id,
                &op.binding,
                &evidence.record,
                op.job_id
                    .as_deref()
                    .context("reconciled provider job missing")?,
            )?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(crate) struct ProviderOperationSummary {
    pub(crate) operation_id: String,
    pub(crate) site: Option<String>,
    pub(crate) component_id: String,
    pub(crate) relation_id: i64,
    pub(crate) object_type: String,
    pub(crate) object_id: i64,
    pub(crate) field_name: String,
    pub(crate) chunk_index: i64,
    pub(crate) lane: String,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
    pub(crate) state: String,
    pub(crate) can_reconcile: bool,
}

/// Ciphertext and snapshot stay private, so callers cannot construct a review.
pub(crate) struct ProviderReview {
    env: AsyncJobEnv,
    raw: String,
    op: Operation,
}

impl ProviderReview {
    pub(crate) fn domain(&self) -> &str {
        &self.op.recovery.as_ref().unwrap().domain
    }

    pub(crate) fn operation_id(&self) -> &str {
        &self.op.attempt_id
    }

    pub(crate) fn binding(&self) -> &str {
        &self.op.binding
    }

    pub(crate) fn template(&self) -> &ComponentTemplate {
        &self.op.recovery.as_ref().unwrap().template
    }

    pub(crate) fn context(&self) -> HashMap<String, String> {
        self.op.ctx.clone()
    }
}

pub(crate) async fn begin_provider_runtime_intent(
    execution: &ProviderExecution,
    runtime: &ComponentRuntime,
    input: &Value,
    ctx: &mut HashMap<String, String>,
    source: &str,
    target: &str,
) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    crate::component_rt::runner::provider_recovery::validate_contract(&runtime.template)?;
    ensure!(
        !ctx.keys().any(|key| key.starts_with("operation.")),
        "reserved provider operation context"
    );
    let mut db = env.db.lock().await;
    let tx = db.savepoint()?;
    execution.assert_owner(&tx)?;
    ensure!(
        load(&tx, env)?.is_none(),
        "provider operation already reserved; no resubmit"
    );
    ensure!(
        read_unit(&tx, env)?.is_none(),
        "retained provider job requires reconciliation"
    );
    let mut op = Operation {
        format: "provider-operation-v1".into(),
        unit: AsyncUnit::from_env(env),
        binding: env
            .resume_binding
            .clone()
            .context("missing provider binding")?,
        attempt_id: uuid::Uuid::new_v4().to_string(),
        component_id: runtime.template.id.clone(),
        source_lang: source.into(),
        target_lang: target.into(),
        state: "submit_unknown".into(),
        job_id: None,
        ctx: ctx.clone(),
        result: None,
        asset_sha256: None,
        recovery: Some(RecoverySnapshot {
            domain: env.domain.clone(),
            template: runtime.template.clone(),
            language_map: runtime.language_map.clone(),
            input: input.clone(),
            submit_context_ready: false,
            proxy_profile_id: runtime.proxy_profile_id.clone(),
            evidence: None,
        }),
    };
    op.ctx
        .insert("operation.attempt_id".into(), op.attempt_id.clone());
    op.ctx
        .insert("operation.binding".into(), op.binding.clone());
    op.recovery.as_ref().unwrap().validate(&op)?;
    install(&tx, env, None, &op)?;
    tx.commit()?;
    *ctx = op.ctx;
    Ok(())
}

/// Capture prepare/upload-derived context before the actual submit boundary.
pub(crate) async fn commit_provider_submit_context(
    execution: &ProviderExecution,
    ctx: &HashMap<String, String>,
) -> Result<()> {
    let env: &AsyncJobEnv = execution;
    let mut db = env.db.lock().await;
    let tx = db.savepoint()?;
    execution.assert_owner(&tx)?;
    let (raw, mut op) = load(&tx, env)?.context("missing provider intent")?;
    ensure!(
        op.state == "submit_unknown",
        "provider intent already advanced"
    );
    let snapshot = op
        .recovery
        .as_mut()
        .context("provider intent has no recovery snapshot")?;
    ensure!(
        !snapshot.submit_context_ready,
        "provider submit boundary already committed"
    );
    snapshot.submit_context_ready = true;
    op.ctx = ctx.clone();
    op.recovery.as_ref().unwrap().validate(&op)?;
    let credit = continuation_credit(&tx, env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        install(&tx, env, Some(&raw), &op)?;
        tx.commit()?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

fn records(conn: &Connection) -> Result<Vec<(String, Operation)>> {
    let mut statement = conn.prepare(
        "SELECT key,value FROM system_config WHERE key LIKE 'provider-operation-v1:%' ORDER BY key",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ids = std::collections::HashSet::new();
    rows.into_iter()
        .map(|(physical, raw)| {
            let op = decode(&raw)?;
            let expected = format!(
                "provider-operation-v1:{}",
                crate::db::system::private_json_digest(&json!({"unit":op.unit}))?
            );
            ensure!(
                physical == expected && ids.insert(op.attempt_id.clone()),
                "ambiguous provider operation identity; retained"
            );
            Ok((raw, op))
        })
        .collect()
}

pub(crate) async fn list_provider_operations(
    db: &Arc<Mutex<Connection>>,
) -> Result<Vec<ProviderOperationSummary>> {
    let conn = db.lock().await;
    Ok(records(&conn)?
        .into_iter()
        .map(|(_, op)| {
            let snapshot = op.recovery.as_ref();
            let can_reconcile = op.state == "submit_unknown"
                && snapshot.is_some_and(|s| {
                    s.submit_context_ready
                        && s.proxy_profile_id.is_none()
                        && s.template
                            .async_poll
                            .as_ref()
                            .is_some_and(|poll| poll.reconcile.is_some())
                });
            ProviderOperationSummary {
                operation_id: op.attempt_id,
                site: snapshot
                    .and_then(|s| reqwest::Url::parse(&s.domain).ok())
                    .map(|url| url.origin().ascii_serialization()),
                component_id: op.component_id,
                relation_id: op.unit.relation_id,
                object_type: op.unit.object_type,
                object_id: op.unit.object_id,
                field_name: op.unit.field_name,
                chunk_index: op.unit.chunk_index,
                lane: op.unit.lane,
                source_lang: op.source_lang,
                target_lang: op.target_lang,
                state: op.state,
                can_reconcile,
            }
        })
        .collect())
}

pub(crate) async fn review_provider_operation(
    db: &Arc<Mutex<Connection>>,
    operation_id: &str,
) -> Result<ProviderReview> {
    ensure!(
        uuid::Uuid::parse_str(operation_id)
            .is_ok_and(|id| !id.is_nil() && id.to_string() == operation_id),
        "invalid provider operation id"
    );
    let conn = db.lock().await;
    let (raw, op) = records(&conn)?
        .into_iter()
        .find(|(_, op)| op.attempt_id == operation_id)
        .context("provider operation not found")?;
    ensure!(
        op.state == "submit_unknown",
        "only unknown submissions need this review"
    );
    let snapshot = op
        .recovery
        .as_ref()
        .context("old intent has no submitted identity evidence")?;
    ensure!(
        snapshot.submit_context_ready,
        "prepare/upload outcome still unknown; retained"
    );
    ensure!(
        snapshot.proxy_profile_id.is_none(),
        "provider evidence proxy is not frozen; retained without direct-query fallback"
    );
    ensure!(
        snapshot
            .template
            .async_poll
            .as_ref()
            .is_some_and(|p| p.reconcile.is_some()),
        "provider has no evidence query contract"
    );
    let lane = match op.unit.lane.as_str() {
        "text" => "text",
        "non_text" => "non_text",
        _ => anyhow::bail!("invalid provider lane"),
    };
    let env = AsyncJobEnv {
        db: Arc::clone(db),
        domain: snapshot.domain.clone(),
        relation_id: op.unit.relation_id,
        object_type: op.unit.object_type.clone(),
        object_id: op.unit.object_id,
        field_name: op.unit.field_name.clone(),
        chunk_index: op.unit.chunk_index,
        lane,
        source_snapshot: op.unit.source_snapshot.clone(),
        resume_binding: Some(op.binding.clone()),
    };
    ensure!(
        read_unit(&conn, &env)?.is_none(),
        "conflicting retained provider projection"
    );
    Ok(ProviderReview { env, raw, op })
}

/// Atomically adopt only the unchanged reviewed intent. A newer writer wins.
/// Polling projections are recreated by the normal runner after this commit.
pub(crate) async fn commit_provider_reconciliation(
    review: &ProviderReview,
    job_id: &str,
    ctx: &HashMap<String, String>,
    record: &Value,
) -> Result<()> {
    ensure!(
        !job_id.trim().is_empty()
            && job_id == job_id.trim()
            && job_id.len() <= 512
            && !job_id.chars().any(char::is_control),
        "invalid provider job id"
    );
    let execution = ProviderExecution::acquire(review.env.clone()).await?;
    let mut db = review.env.db.lock().await;
    let tx = db.savepoint()?;
    execution.assert_owner(&tx)?;
    let (raw, mut op) = load(&tx, &review.env)?.context("provider intent unavailable")?;
    ensure!(
        raw == review.raw && op.state == "submit_unknown" && op.attempt_id == review.op.attempt_id,
        "provider review is stale; original retained"
    );
    ensure!(
        read_unit(&tx, &review.env)?.is_none(),
        "provider projection changed during review"
    );
    ensure!(
        ctx.get("computed.job_id").is_some_and(|id| id == job_id)
            && ctx
                .get("computed.async_job_id")
                .is_some_and(|id| id == job_id),
        "provider recovery context conflicts with evidence"
    );
    op.state = "polling".into();
    op.job_id = Some(job_id.into());
    op.ctx = ctx.clone();
    crate::component_rt::runner::provider_recovery::validate_record(
        review.template(),
        review.operation_id(),
        review.binding(),
        record,
        job_id,
    )?;
    op.recovery.as_mut().unwrap().evidence = Some(ReconciliationEvidence {
        record: record.clone(),
        sha256: crate::db::system::private_json_digest(record)?,
        verified_at: crate::logging::unix_ts(),
    });
    op.recovery.as_ref().unwrap().validate(&op)?;
    let credit = continuation_credit(&tx, &review.env)?;
    crate::storage_capacity::with_database_credit(credit, || {
        install(&tx, &review.env, Some(&raw), &op)?;
        tx.commit()?;
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}
