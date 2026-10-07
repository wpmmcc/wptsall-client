//! One encrypted, resumable upload authority for every local delivery backend.
use super::*;
use recovery_store::RecoveryStore;
use serde::{Deserialize, Serialize};

use std::path::Path;



#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkpoint {
    pub format: String,
    pub key: String,
    pub operation_id: String,
    pub wp_base: String,
    pub device_id: String,
    pub source_id: i64,
    pub task_id: i64,
    pub relation_id: i64,
    pub scope: Option<String>,
    pub filename: String,
    pub content_type: String,
    pub sha256: String,
    pub size: u64,
    pub chunk_size: usize,
    pub asset: String,
    pub state: String,
    pub upload_id: Option<String>,
    pub attachment_id: Option<i64>,
}

pub(crate) fn digest_file(path: &Path) -> anyhow::Result<(String, u64)> {
    ;
    let mut asset = crate::retained_assets::AssetReader::open(path, 512 * 1024 * 1024)?;
    anyhow::ensure!(
        asset.len() > 0,
        "media source must be a regular file between 1 byte and 512 MiB"
    );
    asset.digest()
}

fn validate(row: &Checkpoint) -> anyhow::Result<()> {
    anyhow::ensure!(
        row.format == "media-session-v1"
            && uuid::Uuid::parse_str(&row.operation_id)
                .is_ok_and(|id| id.to_string() == row.operation_id && !id.is_nil())
            && !row.device_id.is_empty()
            && row.source_id > 0
            && row.relation_id > 0
            && row.task_id >= 0
            && row.size > 0
            && row.size <= 512 * 1024 * 1024
            && row.chunk_size > 0
            && row.chunk_size <= 5 * 1024 * 1024
            && row.sha256.len() == 64
            && row
                .sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && matches!(
                row.state.as_str(),
                "prepared" | "init_unknown" | "uploading" | "complete_unknown" | "result_ready"
            )
            && row.attachment_id.is_none_or(|id| id > 0)
            && (row.state == "result_ready") == row.attachment_id.is_some(),
        "damaged media checkpoint; retained"
    );
    anyhow::ensure!(
        row.upload_id
            .as_ref()
            .is_none_or(|id| !id.is_empty() && reqwest::header::HeaderValue::from_str(id).is_ok())
            && (!matches!(row.state.as_str(), "prepared" | "init_unknown")
                || row.upload_id.is_none())
            && (row.state != "uploading" || row.upload_id.is_some()),
        "damaged media checkpoint session state; retained"
    );
    anyhow::ensure!(
        row.key
            == checkpoint_key(
                &row.wp_base,
                &row.device_id,
                row.source_id,
                row.task_id,
                row.relation_id,
                row.scope.as_deref(),
                &row.filename,
                &row.content_type,
                &row.sha256
            )?,
        "media checkpoint identity/key mismatch; retained"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn checkpoint_key(
    wp_base: &str,
    _device_id: &str,
    source_id: i64,
    task_id: i64,
    relation_id: i64,
    scope: Option<&str>,
    _filename: &str,
    _content_type: &str,
    digest: &str,
) -> anyhow::Result<String> {
    crate::db::system::private_json_digest(&serde_json::json!({
        "identity":{"site":wp_base,"source":source_id,"task":task_id,
            "relation":relation_id,"scope":scope},
        "content":if scope.is_none(){Some(digest)}else{None}}))
}
fn save(store: &RecoveryStore, row: &Checkpoint) -> anyhow::Result<()> {
    validate(row)?;
    store.update(&row.key, |previous: Option<Checkpoint>| {
        if let Some(old) = previous {
            validate(&old)?;
            anyhow::ensure!(
                old.operation_id == row.operation_id
                    && old.wp_base == row.wp_base
                    && old.device_id == row.device_id
                    && old.source_id == row.source_id
                    && old.task_id == row.task_id
                    && old.relation_id == row.relation_id
                    && old.scope == row.scope
                    && old.filename == row.filename
                    && old.content_type == row.content_type
                    && old.sha256 == row.sha256
                    && old.key == row.key
                    && old.size == row.size
                    && old.chunk_size == row.chunk_size
                    && old.asset == row.asset
                    && (old.attachment_id.is_none()
                        || (old.state == row.state && old.attachment_id == row.attachment_id))
                    && (old.upload_id.is_none() || old.upload_id == row.upload_id),
                "media checkpoint ownership changed; retained"
            );
        }
        Ok((row.clone(), ()))
    })
}

fn copy_asset(store: &RecoveryStore, path: &Path) -> anyhow::Result<String> {
    let mut source = crate::retained_assets::AssetReader::open(path, 512 * 1024 * 1024)?;
    anyhow::ensure!(
        source.len() > 0,
        "media source bounds changed before staging"
    );
    let destination = store
        .assets()?
        .join(format!("wpa1{}.bin", uuid::Uuid::new_v4().simple()));
    let result = crate::retained_assets::copy(&destination, &mut source);
    if let Err(error) = result {
        // This create-new scratch has not been published in any checkpoint.
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    Ok(destination
        .to_str()
        .context("media asset path is not UTF-8")?
        .into())
}

fn verify_retained_asset(store: &RecoveryStore, row: &Checkpoint) -> anyhow::Result<()> {
    store.verify_asset(Path::new(&row.asset))?;
    anyhow::ensure!(
        std::fs::canonicalize(&row.asset)?.starts_with(store.assets()?)
            && digest_file(Path::new(&row.asset))? == (row.sha256.clone(), row.size),
        "retained media asset failed integrity check"
    );
    Ok(())
}

#[derive(Deserialize)]
struct Remote {
    operation_id: Option<String>,
    content_sha256: Option<String>,
    source_id: Option<i64>,
    task_id: Option<i64>,
    relation_id: Option<i64>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    attachment_id: i64,
    upload_id: Option<String>,
    #[serde(default)]
    received_chunks: Vec<usize>,
    #[serde(default)]
    total_chunks: usize,
    #[serde(default)]
    missing_chunks: Vec<usize>,
}

async fn status(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    row: &Checkpoint,
) -> anyhow::Result<Remote> {
    let mut url = reqwest::Url::parse(&make_wp_url(&config.wp_base, "media-upload/status"))?;
    if let Some(id) = &row.upload_id {
        url.query_pairs_mut().append_pair("upload_id", id);
    } else {
        url.query_pairs_mut()
            .append_pair("operation_id", &row.operation_id);
    }
    let url = url.to_string();
    let mut headers = auth_headers(&config.token, &config.worker_id, &config.device_id);
    add_signature_headers(&mut headers, "GET", &url, &config.token, &[], &[]);
    let value = super::send_media_request(
        client,
        config,
        reqwest::Method::GET,
        &url,
        headers,
        vec![],
        "application/json",
    )
    .await?;
    anyhow::ensure!(value["success"] == true, "media status rejected; retained");
    serde_json::from_value(value.get("data").cloned().unwrap_or(value))
        .context("invalid media status")
}

fn recovered(
    store: &RecoveryStore,
    row: &mut Checkpoint,
    remote: &Remote,
) -> anyhow::Result<Option<MediaUploadResult>> {
    if remote.state == "result_ready" {
        anyhow::ensure!(remote.attachment_id > 0, "invalid completed media receipt");
        anyhow::ensure!(
            remote.operation_id.as_deref() == Some(&row.operation_id)
                && remote.content_sha256.as_deref() == Some(&row.sha256)
                && remote.source_id == Some(row.source_id)
                && remote.task_id == Some(row.task_id)
                && remote.relation_id == Some(row.relation_id),
            "remote receipt belongs to a different media operation"
        );
        row.state = "result_ready".into();
        row.attachment_id = Some(remote.attachment_id);
        save(store, row)?;
        return Ok(Some(MediaUploadResult::ok(
            remote.attachment_id,
            String::new(),
        )));
    }
    anyhow::ensure!(
        !matches!(remote.state.as_str(), "completion_unknown" | "needs_review"),
        "remote import unknown; explicit verified-attachment reconciliation required"
    );
    Ok(None)
}

fn verify_scope(row: &Checkpoint, remote: &Remote) -> anyhow::Result<()> {
    anyhow::ensure!(
        remote.operation_id.as_deref() == Some(&row.operation_id)
            && remote.content_sha256.as_deref() == Some(&row.sha256)
            && remote.source_id == Some(row.source_id)
            && remote.task_id == Some(row.task_id)
            && remote.relation_id == Some(row.relation_id),
        "remote media scope does not match the checkpoint"
    );
    Ok(())
}

pub(crate) async fn reconcile(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    operation_id: &str,
    attachment_id: Option<i64>,
) -> anyhow::Result<MediaUploadResult> {
    let store = RecoveryStore::configured()?;
    let rows: Vec<serde_json::Value> = store.list()?;
    let mut rows: Vec<_> = rows
        .into_iter()
        .filter(|v| v["format"] == "media-session-v1" && v["operation_id"] == operation_id)
        .collect();
    anyhow::ensure!(
        rows.len() == 1,
        "media operation is absent or ambiguous; retained"
    );
    let value = rows.pop().unwrap();
    let mut row: Checkpoint = serde_json::from_value(value)?;
    validate(&row)?;
    anyhow::ensure!(
        row.wp_base == config.wp_base && row.device_id == config.device_id,
        "media reconciliation scope mismatch"
    );
    let _guard = store.guard(&row.key)?;
    // A list snapshot is not the authority once another owner has finished.
    row = store
        .load(&row.key)?
        .context("media checkpoint disappeared; retained")?;
    validate(&row)?;
    anyhow::ensure!(
        row.operation_id == operation_id
            && row.wp_base == config.wp_base
            && row.device_id == config.device_id,
        "media reconciliation scope changed"
    );
    let remote = status(client, config, &row).await?;
    verify_scope(&row, &remote)?;
    if remote.state == "completion_unknown" {
        anyhow::ensure!(
            attachment_id.is_none_or(|id| id > 0),
            "attachment ID must be positive"
        );
        crate::auth::wp_post_with_transport_and_headers(
            client,
            &make_wp_url(&config.wp_base, "media-upload/reconcile"),
            &config.token,
            &config.worker_id,
            &config.device_id,
            &serde_json::json!({"operation_id":row.operation_id,"attachment_id":attachment_id}),
            &[],
        )
        .await?;
        let verified = status(client, config, &row).await?;
        return recovered(&store, &mut row, &verified)?
            .context("reconciliation did not produce a verified result");
    }
    recovered(&store, &mut row, &remote)?
        .context("media result is not confirmed; no import was retried")
}

pub(crate) fn operations() -> anyhow::Result<Vec<Checkpoint>> {
    let values: Vec<serde_json::Value> = RecoveryStore::configured()?.list()?;
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter(|v| v["format"] == "media-session-v1")
        .map(|v| {
            let row: Checkpoint = serde_json::from_value(v)?;
            validate(&row)?;
            anyhow::ensure!(
                seen.insert(row.operation_id.clone()),
                "ambiguous media operation retained"
            );
            Ok(row)
        })
        .collect()
}

pub(crate) async fn resume_scope(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    scope: &str,
) -> anyhow::Result<Option<MediaUploadResult>> {
    let store = RecoveryStore::configured()?;
    let values: Vec<serde_json::Value> = store.list()?;
    let mut values: Vec<_> = values
        .into_iter()
        .filter(|v| v["format"] == "media-session-v1" && v["scope"] == scope)
        .collect();
    anyhow::ensure!(
        values.len() <= 1,
        "ambiguous retained media scope; no network"
    );
    let Some(value) = values.pop() else {
        return Ok(None);
    };
    let row: Checkpoint = serde_json::from_value(value)?;
    validate(&row)?;
    anyhow::ensure!(
        row.wp_base == config.wp_base && row.device_id == config.device_id,
        "retained media scope changed; no network"
    );
    if let Some(id) = row.attachment_id {
        return Ok(Some(MediaUploadResult::ok(id, String::new())));
    }
    store.verify_asset(Path::new(&row.asset))?;
    upload(
        client,
        config,
        &row.asset,
        &row.filename,
        &row.content_type,
        row.source_id,
        row.task_id,
        row.relation_id,
        Some(scope),
    )
    .await
    .map(Some)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload(
    client: &reqwest::Client,
    config: &ChunkedUploadConfig,
    local_path: &str,
    filename: &str,
    content_type: &str,
    source_id: i64,
    task_id: i64,
    relation_id: i64,
    scope: Option<&str>,
) -> anyhow::Result<MediaUploadResult> {
    anyhow::ensure!(
        !config.device_id.is_empty()
            && source_id > 0
            && relation_id > 0
            && task_id >= 0
            && config.effective_chunk_size() <= 5 * 1024 * 1024,
        "durable media requires site/device/source/relation identity"
    );
    let store = RecoveryStore::configured()?;
    // A caller-supplied unit remains stable even if a remote URL expires or
    // the producer's original local file is no longer available.
    let (digest, size) = digest_file(Path::new(local_path))?;
    let key = checkpoint_key(
        &config.wp_base,
        &config.device_id,
        source_id,
        task_id,
        relation_id,
        scope,
        filename,
        content_type,
        &digest,
    )?;
    let _guard = store.guard(&key)?;
    let previous = store.load::<Checkpoint>(&key)?;
    let existing = previous.is_some();
    let mut row = if let Some(old) = previous {
        validate(&old)?;
        anyhow::ensure!(
            old.sha256 == digest
                && old.key == key
                && old.source_id == source_id
                && old.task_id == task_id
                && old.relation_id == relation_id
                && old.scope.as_deref() == scope
                && old.size == size
                && old.device_id == config.device_id
                && old.wp_base == config.wp_base
                && old.filename == filename
                && old.content_type == content_type,
            "media bytes changed under retained unit"
        );
        old
    } else {
        // Key/crypto refusal must happen before storing a plaintext asset.
        crate::db::system::encrypt_config_value("{}")?;
        let asset = copy_asset(&store, Path::new(local_path))?;
        let new = Checkpoint {
            format: "media-session-v1".into(),
            key: key.clone(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            wp_base: config.wp_base.clone(),
            device_id: config.device_id.clone(),
            source_id,
            task_id,
            relation_id,
            scope: scope.map(str::to_owned),
            filename: filename.into(),
            content_type: content_type.into(),
            sha256: digest,
            size,
            chunk_size: config.effective_chunk_size(),
            asset,
            state: "prepared".into(),
            upload_id: None,
            attachment_id: None,
        };
        // Authenticate the readback before publishing, not again after the save.
        verify_retained_asset(&store, &new).context("media changed while staging")?;
        save(&store, &new)?;
        new
    };
    if let Some(id) = row.attachment_id {
        return Ok(MediaUploadResult::ok(id, String::new()));
    }
    if existing {
        verify_retained_asset(&store, &row)?;
    }
    if row.state != "prepared" {
        let remote = status(client, config, &row).await?;
        if let Some(done) = recovered(&store, &mut row, &remote)? {
            return Ok(done);
        }
        if remote.state == "not_started" {
            anyhow::ensure!(
                remote.operation_id.as_deref() == Some(&row.operation_id)
                    && row.upload_id.is_none(),
                "invalid not-started proof; retained"
            );
            row.state = "prepared".into();
            save(&store, &row)?;
        } else {
            verify_scope(&row, &remote)?;
            anyhow::ensure!(
                remote.state == "uploading",
                "remote session is not uploadable; retained"
            );
            if row.upload_id.is_none() {
                row.upload_id = remote.upload_id;
                anyhow::ensure!(
                    row.upload_id
                        .as_ref()
                        .is_some_and(|id| !id.is_empty()
                            && reqwest::header::HeaderValue::from_str(id).is_ok()),
                    "remote session identity is unavailable"
                );
                row.state = "uploading".into();
                save(&store, &row)?;
            }
            if row.size < 50 * 1024 * 1024 {
                anyhow::bail!("single upload outcome unknown; no blind reupload");
            }
            anyhow::ensure!(
                remote.state == "uploading",
                "complete outcome unknown; remote cannot prove it is safe to retry"
            );
        }
    }
    if row.size < 50 * 1024 * 1024 {
        row.state = "complete_unknown".into();
        save(&store, &row)?;
        let result = super::single_upload_recoverable(client, config, &row).await?;
        row.state = "result_ready".into();
        row.attachment_id = Some(result.attachment_id);
        save(&store, &row)?;
        return Ok(result);
    }
    if row.upload_id.is_none() {
        row.state = "init_unknown".into();
        save(&store, &row)?;
        row.upload_id = Some(super::init_upload_recoverable(client, config, &row).await?);
        row.state = "uploading".into();
        save(&store, &row)?;
    }
    let remote = status(client, config, &row).await?;
    if let Some(done) = recovered(&store, &mut row, &remote)? {
        return Ok(done);
    }
    let count = usize::try_from(row.size)?.div_ceil(row.chunk_size);
    let mut remote = remote;
    for attempt in 0..=3 {
        anyhow::ensure!(
            remote.state == "uploading",
            "remote session is not uploadable; retained"
        );
        verify_scope(&row, &remote)?;
        let partition = StatusResponse {
            received_chunks: remote.received_chunks,
            total_chunks: remote.total_chunks,
            missing_chunks: remote.missing_chunks,
        };
        if validate_chunk_status(&partition, count)? {
            break;
        }
        anyhow::ensure!(
            attempt < 3,
            "media still missing chunks; resume retained session"
        );
        for index in &partition.missing_chunks {
            upload_single_chunk(
                client,
                config,
                &row.asset,
                row.upload_id.as_ref().unwrap(),
                *index,
                row.chunk_size,
            )
            .await?;
        }
        remote = status(client, config, &row).await?;
        if let Some(done) = recovered(&store, &mut row, &remote)? {
            return Ok(done);
        }
    }
    row.state = "complete_unknown".into();
    save(&store, &row)?;
    let result = complete_upload(client, config, &row).await?;
    row.state = "result_ready".into();
    row.attachment_id = Some(result.attachment_id);
    save(&store, &row)?;
    Ok(result)
}
