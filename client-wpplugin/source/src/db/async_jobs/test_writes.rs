//! Fixture writes acquire the same authority as the product. No test-only
//! bypass inside production persistence; live runner tests use the real APIs.
use super::*;
use crate::types::ComponentRuntime;

pub(crate) async fn upsert_polling_job(
    env: &AsyncJobEnv,
    component: &str,
    job: &str,
    ctx: &HashMap<String, String>,
    source: &str,
    target: &str,
) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::upsert_polling_job(&execution, component, job, ctx, source, target).await
}

pub(crate) async fn touch_polling_job(env: &AsyncJobEnv, attempts: i64) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::touch_polling_job(&execution, attempts).await
}

pub(crate) async fn mark_job_failed(env: &AsyncJobEnv, error: &str) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::mark_job_failed(&execution, error).await
}

pub(crate) async fn close_job(env: &AsyncJobEnv) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::close_job(&execution).await
}

pub(crate) async fn begin_provider_intent(
    env: &AsyncJobEnv,
    component: &str,
    ctx: &HashMap<String, String>,
    source: &str,
    target: &str,
) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::begin_provider_intent(&execution, component, ctx, source, target).await
}

pub(crate) async fn begin_provider_runtime_intent(
    env: &AsyncJobEnv,
    runtime: &ComponentRuntime,
    input: &Value,
    ctx: &mut HashMap<String, String>,
    source: &str,
    target: &str,
) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::begin_provider_runtime_intent(&execution, runtime, input, ctx, source, target).await
}

pub(crate) async fn commit_provider_submit_context(
    env: &AsyncJobEnv,
    ctx: &HashMap<String, String>,
) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::commit_provider_submit_context(&execution, ctx).await
}

pub(crate) async fn save_provider_job(
    env: &AsyncJobEnv,
    component: &str,
    job: &str,
    ctx: &HashMap<String, String>,
    source: &str,
    target: &str,
) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::save_provider_job(&execution, component, job, ctx, source, target).await
}

pub(crate) async fn save_provider_result(env: &AsyncJobEnv, result: Value) -> Result<()> {
    let execution = ProviderExecution::acquire(env.clone()).await?;
    super::save_provider_result(&execution, result).await
}
