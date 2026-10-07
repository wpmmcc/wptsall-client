//! Frozen encrypted media sessions. Restart queries the original target first.
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::hmac::sha256_hex;
use super::shipper::{Shipper, TransferMediaResult, MAX_DOWNLOAD_BYTES, MEDIA_CHUNK_BYTES};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    format: String,
    operation: String,
    authority_sha256: String,
    file_uuid: String,
    filename: String,
    mime_type: String,
    sha256: String,
    size: u64,
    total: u32,
    submitted: bool,
    result: Option<TransferMediaResult>,
}

pub(super) async fn transfer(
    shipper: &Shipper,
    operation: &str,
    pair_id: &str,
    source_url: &str,
    target: &str,
    client_uuid: &str,
    secret: &[u8],
) -> anyhow::Result<TransferMediaResult> {
    ensure!(operation.len() == 64 && operation.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid frozen sync media operation");
    let directory = crate::retained_assets::directory(
        &crate::config::data_dir().unwrap_or_else(|| std::path::PathBuf::from(crate::config::DEFAULT_DATA_DIR))
            .join("sync-media").join(operation),
    )?;
    let lease = crate::bindings::native_lock::NativeLease::acquire(
        &directory.join("owner.lock"), "original sync media session is active",
    )?;
    let checkpoint = directory.join("session.json");
    let asset = directory.join(format!("wpa1-sync-{operation}.blob"));
    let authority = crate::db::system::private_json_digest(&json!({
        "pair":pair_id, "source":source_url, "target":target, "client":client_uuid,
        "credential":sha256_hex(secret),
    }))?;
    let mut session: Session = if checkpoint.try_exists()? {
        let raw = std::fs::read(&checkpoint)?;
        serde_json::from_str(&crate::bindings::decrypt_from_bytes(&raw)?)?
    } else {
        if !asset.try_exists()? {
            let bytes = shipper.download_media(source_url).await?;
            ensure!(!bytes.is_empty(), "empty sync media asset");
            lease.assert_owner()?;
            crate::storage_capacity::with_database_credit(None, || crate::retained_assets::write_bytes(&asset, &bytes))?;
        }
        let mut reader = crate::retained_assets::AssetReader::open(&asset, MAX_DOWNLOAD_BYTES as u64)?;
        let (sha256, size) = reader.digest()?;
        let url = url::Url::parse(source_url).map_err(|_| anyhow::anyhow!("invalid sync media source"))?;
        let filename = url.path_segments().and_then(|parts| parts.filter(|part| !part.is_empty()).next_back())
            .unwrap_or("wpmmcc-asset.bin").to_string();
        let session = Session {
            format:"sync-media-session-v1".into(), operation:operation.into(), authority_sha256:authority.clone(),
            file_uuid:format!("media_{operation}"), mime_type:super::shipper::mime_for_filename(&filename).into(),
            filename, sha256, size, total:u32::try_from(size.div_ceil(MEDIA_CHUNK_BYTES as u64))?,
            submitted:false, result:None,
        };
        save(&checkpoint, &session, &lease)?;
        session
    };
    ensure!(session.format == "sync-media-session-v1" && session.operation == operation
        && session.authority_sha256 == authority && session.size > 0
        && session.size <= MAX_DOWNLOAD_BYTES as u64
        && session.total == u32::try_from(session.size.div_ceil(MEDIA_CHUNK_BYTES as u64))?,
        "sync media scope differs; original asset retained");
    let mut reader = crate::retained_assets::AssetReader::open(&asset, MAX_DOWNLOAD_BYTES as u64)?;
    ensure!(reader.digest()? == (session.sha256.clone(), session.size),
        "frozen sync media bytes differ; retained");
    if let Some(result) = session.result {
        ensure!(result.sha256 == session.sha256 && result.size_bytes as u64 == session.size
            && result.attachment_id > 0,
            "saved sync media receipt differs; retained");
        super::shipper::scoped_http_url(&result.target_url, target)?;
        return Ok(result);
    }
    let missing = if session.submitted {
        let body = json!({
            "file_uuid":session.file_uuid,"total":session.total,"filename":session.filename,
            "mime_type":session.mime_type,"expected_sha256":session.sha256,"expected_size":session.size,
        });
        let receipt = shipper.media_receipt(target, client_uuid, secret, &body).await?;
        ensure!(receipt["file_uuid"] == session.file_uuid
            && receipt["total"].as_u64() == Some(session.total as u64)
            && receipt["expected_sha256"] == session.sha256
            && receipt["expected_size"].as_u64() == Some(session.size)
            && receipt["filename"] == session.filename
            && receipt["mime_type"] == session.mime_type,
            "sync media proof does not belong to the frozen session; retained");
        if receipt["completed"] == true {
            let result = result(&receipt, &session, target)?;
            session.result = Some(result.clone());
            save(&checkpoint, &session, &lease)?;
            return Ok(result);
        }
        let phase = receipt["phase"].as_str().unwrap_or("");
        ensure!((phase == "receiving" || phase == "absent")
            && receipt["file_uuid"] == session.file_uuid
            && receipt["total"].as_u64() == Some(session.total as u64),
            "original sync media effect is unknown; no new session or reapply");
        if phase == "absent" {
            // Zero-effect proof from the target: it commits the intent row
            // under its SQL lease before admitting any bytes, so absence
            // means this session never produced a target effect. Resend
            // every chunk of the SAME frozen session (same file_uuid).
            (0..session.total).collect()
        } else {
            let missing: Vec<u32> = serde_json::from_value(receipt["missing_chunks"].clone())
                .context("sync media receipt missing-chunk scope is invalid")?;
            let unique = missing.iter().copied().collect::<std::collections::HashSet<_>>();
            ensure!(missing.len() == unique.len() && missing.iter().all(|index| *index < session.total),
                "sync media receipt contains foreign chunks; retained");
            missing
        }
    } else {
        session.submitted = true;
        save(&checkpoint, &session, &lease)?;
        (0..session.total).collect()
    };
    for index in missing {
        lease.assert_owner()?;
        let offset = index as u64 * MEDIA_CHUNK_BYTES as u64;
        let bytes = reader.read_range(offset, usize::try_from((session.size - offset).min(MEDIA_CHUNK_BYTES as u64))?)?;
        let ack = shipper.upload_media_chunk_with_evidence(
            pair_id,target,client_uuid,secret,&session.file_uuid,index,session.total,&bytes,
            &session.filename,&session.mime_type, &session.sha256, session.size,
        ).await?;
        if ack.completed {
            let result = result(&json!({
                "assembled_sha256":ack.assembled_sha256,"attachment_id":ack.attachment_id,
                "attachment_url":ack.attachment_url,"reused":ack.reused,
            }), &session, target)?;
            session.result = Some(result.clone());
            save(&checkpoint, &session, &lease)?;
            return Ok(result);
        }
    }
    anyhow::bail!("original sync media effect is not confirmed; encrypted asset retained")
}

fn result(value: &serde_json::Value, session: &Session, target: &str) -> anyhow::Result<TransferMediaResult> {
    ensure!(value["assembled_sha256"] == session.sha256
        && value["attachment_id"].as_u64().is_some_and(|id| id > 0),
        "sync media receipt differs from frozen bytes");
    let url = value["attachment_url"].as_str().context("sync media receipt URL missing")?;
    super::shipper::scoped_http_url(url, target)?;
    Ok(TransferMediaResult {
        target_url:url.into(), attachment_id:value["attachment_id"].as_u64().context("media ID missing")?,
        sha256:session.sha256.clone(), reused:value["reused"].as_bool().unwrap_or(false),
        size_bytes:usize::try_from(session.size)?,
    })
}

fn save(path: &std::path::Path, session: &Session, lease: &crate::bindings::native_lock::NativeLease) -> anyhow::Result<()> {
    lease.assert_owner()?;
    let bytes = crate::bindings::encrypt_for_save(&serde_json::to_string(session)?)?;
    crate::bindings::atomic_file::install_uncredited(path, &bytes)?;
    ensure!(std::fs::read(path)? == bytes, "sync media checkpoint readback differs");
    lease.assert_owner()
}
