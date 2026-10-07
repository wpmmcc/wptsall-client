//! Read-only, provider-specific evidence query for an unchanged submit intent.
use super::*;
use crate::db::async_jobs::ProviderReview;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

const MAX_PROOF_BYTES: usize = 1024 * 1024;
const ATTEMPT_TOKEN: &str = "{{operation.attempt_id}}";
const BINDING_TOKEN: &str = "{{operation.binding}}";

pub(crate) fn validate_contract(template: &ComponentTemplate) -> anyhow::Result<()> {
    let Some(poll) = &template.async_poll else {
        return Ok(());
    };
    let Some(spec) = &poll.reconcile else {
        return Ok(());
    };
    let request = &spec.request;
    anyhow::ensure!(
        request.method == "GET"
            && request.body.is_none()
            && request
                .body_type
                .as_deref()
                .is_none_or(|kind| kind == "none")
            && request
                .response_type
                .as_deref()
                .is_none_or(|kind| kind == "json")
            && request.url.contains(ATTEMPT_TOKEN),
        "provider reconciliation requires a read-only identity query"
    );
    let submit = serde_json::to_string(&serde_json::json!({
        "url": template.request.url,
        "headers": template.request.headers,
        "body": if template.request.body_type.as_deref() == Some("none") {
            None
        } else {
            template.request.body.as_ref()
        },
    }))?;
    anyhow::ensure!(
        submit.contains(ATTEMPT_TOKEN) && submit.contains(BINDING_TOKEN),
        "provider submit must send the committed attempt and binding"
    );
    anyhow::ensure!(
        [
            &spec.matches_path,
            &spec.attempt_id_path,
            &spec.binding_path,
            &spec.job_id_path,
            &spec.status_path,
        ]
        .iter()
        .all(|path| !path.trim().is_empty() && !path.contains("{{") && !path.contains("}}")),
        "provider evidence paths must be explicit"
    );
    let accepted = normalize_status_values(&spec.resumable_values);
    let failed = normalize_status_values(&poll.failed_values);
    anyhow::ensure!(
        !accepted.is_empty() && !accepted.iter().any(|value| failed.contains(value)),
        "provider evidence cannot authorize failed jobs"
    );
    Ok(())
}

fn secure_proof_origin(url: &reqwest::Url) -> anyhow::Result<()> {
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "provider evidence requires HTTPS outside loopback"
    );
    anyhow::ensure!(
        url.fragment().is_none(),
        "provider evidence URL has a fragment"
    );
    Ok(())
}

async fn query_proof(review: &ProviderReview) -> anyhow::Result<Value> {
    let template = review.template();
    validate_contract(template)?;
    let spec = &template
        .async_poll
        .as_ref()
        .unwrap()
        .reconcile
        .as_ref()
        .unwrap()
        .request;
    let budget = HttpBudget::new(
        spec.http_limits.as_ref(),
        HttpResponseKind::Json,
        &template.id,
    )?
    .response_ceiling(MAX_PROOF_BYTES as u64);
    budget
        .wait(async {
            let mut ctx = review.context();
            prime_sign_context(template.sign.as_ref(), &mut ctx)?;
            let raw = render_template_string(&spec.url, &ctx);
            let submit = render_template_string(&template.request.url, &ctx);
            anyhow::ensure!(
                !raw.contains("{{") && !submit.contains("{{"),
                "unresolved evidence URL"
            );
            let url = reqwest::Url::parse(&raw)?;
            let submit = reqwest::Url::parse(&submit)?;
            anyhow::ensure!(
                url.origin() == submit.origin(),
                "provider evidence origin changed"
            );
            secure_proof_origin(&url)?;
            assert_provider_url_allowed(url.as_str())?;
            inject_rendered_request_context(spec, url.as_str(), &mut ctx);
            let sign = process_sign_config(template.sign.as_ref(), &mut ctx, "GET", url.as_str())?;
            let mut headers = HeaderMap::new();
            if let Some(configured) = &spec.headers {
                for (name, value) in configured {
                    let value = render_template_string(value, &ctx);
                    anyhow::ensure!(!value.contains("{{"), "unresolved evidence header");
                    if !value.trim().is_empty() {
                        headers.insert(
                            HeaderName::from_bytes(name.as_bytes())?,
                            HeaderValue::from_str(&value)?,
                        );
                    }
                }
            }
            match sign {
                SignResult::ContextOnly(_) => {}
                SignResult::WithHeaders(_, values) => {
                    for (name, value) in values {
                        headers.insert(
                            HeaderName::from_bytes(name.as_bytes())?,
                            HeaderValue::from_str(&value)?,
                        );
                    }
                }
                SignResult::AuthorizationHeader(value) => {
                    headers.insert(
                        reqwest::header::AUTHORIZATION,
                        HeaderValue::from_str(&value)?,
                    );
                }
            }
            anyhow::ensure!(
                !headers.contains_key(reqwest::header::HOST)
                    && !headers.contains_key(reqwest::header::CONTENT_LENGTH)
                    && !headers.contains_key(reqwest::header::TRANSFER_ENCODING),
                "provider evidence cannot override request routing or framing"
            );
            // Do not inherit a caller's redirect policy or ambient proxy. Neither a
            // redirect nor an environment change may send frozen auth to a new host.
            let client = Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?;
            let response = client
                .get(url)
                .headers(headers)
                .send()
                .await
                .map_err(|_| anyhow!("provider evidence query failed; retained"))?;
            anyhow::ensure!(
                response.status().is_success(),
                "provider evidence query was not successful"
            );
            let bytes = budget.read(response).await?;
            let proof: Value =
                serde_json::from_slice(&bytes).context("provider evidence is not JSON")?;
            if let Some(path) = template.response.error_path.as_deref() {
                anyhow::ensure!(
                    extract_component_logical_error(&proof, path).is_none(),
                    "provider evidence contains an upstream error"
                );
            }
            Ok(proof)
        })
        .await?
}

pub(crate) fn validate_record(
    template: &ComponentTemplate,
    attempt_id: &str,
    binding: &str,
    record: &Value,
    job: &str,
) -> anyhow::Result<()> {
    let spec = template
        .async_poll
        .as_ref()
        .and_then(|p| p.reconcile.as_ref())
        .context("provider evidence contract unavailable")?;
    anyhow::ensure!(
        record.is_object()
            && extract_json_path(record, &spec.attempt_id_path).and_then(Value::as_str)
                == Some(attempt_id)
            && extract_json_path(record, &spec.binding_path).and_then(Value::as_str)
                == Some(binding)
            && extract_json_path_string(record, &spec.job_id_path).as_deref() == Some(job),
        "provider evidence belongs to a different operation or job"
    );
    let status = extract_json_path(record, &spec.status_path)
        .and_then(Value::as_str)
        .context("provider evidence has no status")?;
    anyhow::ensure!(
        normalize_status_values(&spec.resumable_values).contains(&normalize_status_value(status)),
        "provider job cannot be resumed"
    );
    anyhow::ensure!(
        !job.is_empty()
            && job.len() <= 512
            && !job.contains("..")
            && job
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:~".contains(&byte)),
        "unsafe provider job id"
    );
    Ok(())
}

fn verified_job_context(
    review: &ProviderReview,
    proof: &Value,
) -> anyhow::Result<(String, HashMap<String, String>, Value)> {
    let poll = review.template().async_poll.as_ref().unwrap();
    let spec = poll.reconcile.as_ref().unwrap();
    let matches = extract_json_path(proof, &spec.matches_path)
        .and_then(Value::as_array)
        .context("provider evidence has no exact match list")?;
    anyhow::ensure!(
        matches.len() == 1,
        "provider evidence is missing or ambiguous"
    );
    let record = &matches[0];
    let job = match extract_json_path(record, &spec.job_id_path) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value
            .as_u64()
            .filter(|value| *value > 0)
            .map(|value| value.to_string())
            .context("invalid provider job id")?,
        _ => anyhow::bail!("provider evidence has no job id"),
    };
    validate_record(
        review.template(),
        review.operation_id(),
        review.binding(),
        record,
        &job,
    )?;
    let mut ctx = review.context();
    ctx.insert("computed.job_id".into(), job.clone());
    ctx.insert("computed.async_job_id".into(), job.clone());
    // Use the original submit extraction contract, not user-supplied context.
    apply_async_poll_submit_extract(poll, record, &mut ctx, &review.template().id)
        .map_err(|_| anyhow!("provider evidence lacks required submit context"))?;
    anyhow::ensure!(
        ctx.get("computed.job_id") == Some(&job) && ctx.get("computed.async_job_id") == Some(&job),
        "provider evidence extraction changed the proven job"
    );
    Ok((job, ctx, record.clone()))
}

pub(crate) async fn reconcile(review: &ProviderReview) -> anyhow::Result<String> {
    let proof = query_proof(review).await?;
    let (job, ctx, record) = verified_job_context(review, &proof)?;
    crate::db::async_jobs::commit_provider_reconciliation(review, &job, &ctx, &record).await?;
    Ok(job)
}
