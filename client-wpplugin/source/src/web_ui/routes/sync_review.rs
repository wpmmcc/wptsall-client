//! Sync content human-review HTTP handlers.

use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::config::{sync_pairs_file, sync_peer_credentials_file, sync_state_file};
use crate::logging::unix_ts;
use crate::sync_engine::{
    find_pair_in_doc, find_peer_credential, find_review_item, find_review_item_mut, list_pending,
    load_peer_credentials, load_sync_pairs, load_sync_review, sync_review_file, update_pair_state,
    update_sync_review, KnownEntity, Shipper, SyncReviewDoc, SyncReviewItem, SyncReviewStatus,
};
use crate::types::WebUiState;

use super::errors::{
    err_public, write_error_response, write_error_response_with_status, write_not_found_response,
};
use super::http::{parse_query_string, write_http_response};

async fn execution_lease_for_request(
    socket: &mut TcpStream,
    path: &str,
    id: &str,
) -> anyhow::Result<Option<crate::bindings::native_lock::NativeLease>> {
    match crate::sync_engine::review::execution_lease(path, id) {
        Ok(lease) => Ok(Some(lease)),
        Err(error) => {
            write_error_response_with_status(socket, "409 Conflict", "SYNC_REVIEW_BUSY", &err_public(&error)).await?;
            Ok(None)
        }
    }
}

enum ReviewEdit {
    NotFound,
    NotPending,
    Updated(SyncReviewItem),
}

#[derive(Debug)]
struct ReviewStateSaveFailure;

impl std::fmt::Display for ReviewStateSaveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sync review state save failed")
    }
}

impl std::error::Error for ReviewStateSaveFailure {}

async fn load_review_for_request(
    socket: &mut TcpStream,
    path: &str,
) -> anyhow::Result<Option<SyncReviewDoc>> {
    match load_sync_review(path) {
        Ok(doc) => Ok(Some(doc)),
        Err(error) => {
            write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "SYNC_REVIEW_READ_FAILED",
                &err_public(&error),
            )
            .await?;
            Ok(None)
        }
    }
}

/// GET /api/sync-review?pair_id=&count_only=
pub(super) async fn handle_sync_review_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    let pair_id = params
        .get("pair_id")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let count_only = params
        .get("count_only")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let Some(doc) = load_review_for_request(socket, &sync_review_file()).await? else {
        return Ok(());
    };
    let items = list_pending(&doc, pair_id.as_deref());
    let total = items.len();
    let payload = if count_only {
        json!({ "success": true, "data": { "total": total, "items": [] } })
    } else {
        let light: Vec<_> = items
            .into_iter()
            .map(|i| {
                json!({
                    "id": i.id,
                    "pair_id": i.pair_id,
                    "canonical_uuid": i.canonical_uuid,
                    "status": "pending_review",
                    "source_title": i.source_title,
                    "proposed_title": i.proposed_title,
                    "post_type": i.post_type,
                    "error_message": i.error_message,
                    "created_at": i.created_at,
                    "updated_at": i.updated_at,
                })
            })
            .collect();
        json!({ "success": true, "data": { "total": total, "items": light } })
    };
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// GET /api/sync-review/:id
pub(super) async fn handle_sync_review_detail(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let Some(doc) = load_review_for_request(socket, &sync_review_file()).await? else {
        return Ok(());
    };
    let Some(item) = find_review_item(&doc, id) else {
        return write_not_found_response(socket, "NOT_FOUND", "sync review item not found").await;
    };
    let payload = json!({ "success": true, "data": { "item": item } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// PUT /api/sync-review/:id — edit proposed fields before approve.
pub(super) async fn handle_sync_review_update(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    id: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: serde_json::Value = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) if value.is_object() => value,
        _ => {
            return write_error_response(
                socket,
                "INVALID_PAYLOAD",
                "review edits must be a JSON object",
            )
            .await
        }
    };
    let path = sync_review_file();
    let Some(lease) = execution_lease_for_request(socket, &path, id).await? else {
        return Ok(());
    };
    let Some(_) = load_review_for_request(socket, &path).await? else {
        return Ok(());
    };
    let edited = update_sync_review(&path, |doc| {
        let Some(item) = find_review_item_mut(doc, id) else {
            return Ok(ReviewEdit::NotFound);
        };
        if item.status != SyncReviewStatus::PendingReview || item.delivery.is_some() {
            return Ok(ReviewEdit::NotPending);
        }
        if let Some(t) = req.get("proposed_title").and_then(|v| v.as_str()) {
            item.proposed_title = t.to_string();
            item.relayed_packet
                .entity
                .core_fields
                .insert("post_title".into(), json!(t));
        }
        if let Some(t) = req.get("proposed_content").and_then(|v| v.as_str()) {
            item.proposed_content = t.to_string();
            item.relayed_packet
                .entity
                .core_fields
                .insert("post_content".into(), json!(t));
        }
        if let Some(t) = req.get("proposed_excerpt").and_then(|v| v.as_str()) {
            item.proposed_excerpt = t.to_string();
            item.relayed_packet
                .entity
                .core_fields
                .insert("post_excerpt".into(), json!(t));
        }
        item.updated_at = unix_ts();
        lease.assert_owner()?;
        let out = item.clone();
        doc.updated_at = unix_ts();
        Ok(ReviewEdit::Updated(out))
    });
    let out = match edited {
        Ok(ReviewEdit::Updated(item)) => item,
        Ok(ReviewEdit::NotFound) => {
            return write_not_found_response(socket, "NOT_FOUND", "sync review item not found")
                .await
        }
        Ok(ReviewEdit::NotPending) => {
            return write_error_response(socket, "INVALID_STATUS", "item is not pending review")
                .await
        }
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "SYNC_REVIEW_SAVE_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    crate::logging::log_event_global(
        "info",
        "sync_review.item_updated",
        json!({ "id": id, "pair_id": out.pair_id, "canonical_uuid": out.canonical_uuid }),
    );
    let payload = json!({ "success": true, "data": { "item": out } });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// POST /api/sync-review/:id/approve — push parked packet to target WP.
pub(super) async fn handle_sync_review_approve(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    id: &str,
) -> anyhow::Result<()> {
    let path = sync_review_file();
    let Some(lease) = execution_lease_for_request(socket, &path, id).await? else {
        return Ok(());
    };
    let Some(doc) = load_review_for_request(socket, &path).await? else {
        return Ok(());
    };
    let Some(mut item) = find_review_item(&doc, id).cloned() else {
        return write_not_found_response(socket, "NOT_FOUND", "sync review item not found").await;
    };
    if item.status != SyncReviewStatus::PendingReview {
        return write_error_response(socket, "INVALID_STATUS", "item is not pending review").await;
    }

    let pairs = match load_sync_pairs(&sync_pairs_file()) {
        Ok(pairs) => pairs,
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "SYNC_PAIRS_READ_FAILED",
                &err_public(&error),
            )
            .await
        }
    };
    let Some(pair) = find_pair_in_doc(&pairs, &item.pair_id).cloned() else {
        return write_error_response(socket, "PAIR_NOT_FOUND", "sync pair not found").await;
    };
    let credentials = load_peer_credentials(&sync_peer_credentials_file())?;
    let Some(target_cred) = find_peer_credential(&credentials, &pair.target_domain) else {
        return write_error_response(socket, "TARGET_NOT_PAIRED", "target site not paired").await;
    };
    let Some(source_cred) = find_peer_credential(&credentials, &pair.source_domain) else {
        return write_error_response(socket, "SOURCE_NOT_PAIRED", "source confirmation authority is missing").await;
    };
    let client_uuid = credentials.client_origin_uuid.clone();
    let target_secret = target_cred.shared_secret_bytes()?;
    let target_base = target_cred.rest_base_url();

    let http = {
        let guard = state.lock().await;
        guard.http_client.clone()
    };
    let shipper = Shipper::new(http)?.with_source_confirmation(
        source_cred.rest_base_url(), source_cred.shared_secret_bytes()?, target_cred.peer_uuid.clone(),
    );
    let packet_sha256 = crate::db::system::private_json_digest(&serde_json::to_value(&item.relayed_packet)?)?;
    let authority_sha256 = crate::db::system::private_json_digest(&json!({
        "pair_id":pair.id, "source":pair.source_domain, "target":pair.target_domain,
        "source_lang":pair.source_lang, "target_lang":pair.target_lang,
        "direction":pair.direction, "mode":pair.sync_mode,
        "client":client_uuid,
        "source_peer":source_cred.peer_uuid, "target_peer":target_cred.peer_uuid,
        "source_key":crate::sync_engine::hmac::sha256_hex(&source_cred.shared_secret_bytes()?),
        "target_key":crate::sync_engine::hmac::sha256_hex(&target_secret),
    }))?;
    let original_delivery = item.delivery.as_ref().is_some_and(|delivery| delivery.submitted);
    if let Some(delivery) = &item.delivery {
        if delivery.packet_sha256 != packet_sha256 || delivery.authority_sha256 != authority_sha256 {
            return write_error_response_with_status(socket, "409 Conflict", "SYNC_REVIEW_SCOPE_CHANGED",
                "Original packet or paired authority changed; target effect retained.").await;
        }
    } else {
        let expected = item.clone();
        item.delivery = Some(crate::sync_engine::review::SyncReviewDelivery {
            packet_sha256, authority_sha256, submitted: false, target_receipt: None,
        });
        update_sync_review(&path, |doc| {
            lease.assert_owner()?;
            let row = find_review_item_mut(doc, id).ok_or_else(|| anyhow::anyhow!("review item removed"))?;
            anyhow::ensure!(serde_json::to_value(&*row)? == serde_json::to_value(&expected)?,
                "sync review authority changed; no target request started");
            *row = item.clone();
            Ok(())
        })?;
    }
    lease.assert_owner()?;
    // A previous submission is queried, never blindly pushed again. The
    // authenticated target can distinguish applied from unknown/missing.
    let result = if let Some(receipt) = item.delivery.as_ref().and_then(|delivery| delivery.target_receipt.clone()) {
        shipper.validate_saved_result(&target_base, &item.relayed_packet, &receipt)?;
        Ok(receipt)
    } else if original_delivery {
        shipper.resume_submitted_packet(&item.pair_id, &target_base, &client_uuid, &target_secret, &item.relayed_packet).await
    } else {
        shipper.prepare_configured_source(&client_uuid, &target_base, &item.relayed_packet).await?;
        let expected = item.clone();
        item.delivery.as_mut().expect("saved delivery intent").submitted = true;
        update_sync_review(&path, |doc| {
            lease.assert_owner()?;
            let row = find_review_item_mut(doc, id).ok_or_else(|| anyhow::anyhow!("review item removed"))?;
            anyhow::ensure!(serde_json::to_value(&*row)? == serde_json::to_value(&expected)?,
                "sync review authority changed; no target request started");
            *row = item.clone();
            Ok(())
        })?;
        lease.assert_owner()?;
        shipper.push_packet(
            &item.pair_id,
            &target_base,
            &client_uuid,
            &target_secret,
            &item.relayed_packet,
        )
        .await
    };
    match result {
        Ok(ack) if ack.success => {
            if item.delivery.as_ref().and_then(|delivery| delivery.target_receipt.as_ref()).is_none() {
                let expected = item.clone();
                item.delivery.as_mut().expect("saved delivery intent").target_receipt = Some(ack.clone());
                update_sync_review(&path, |doc| {
                    lease.assert_owner()?;
                    let row = find_review_item_mut(doc, id).ok_or_else(|| anyhow::anyhow!("review item removed"))?;
                    anyhow::ensure!(serde_json::to_value(&*row)? == serde_json::to_value(&expected)?,
                        "sync review authority changed; original target receipt retained");
                    *row = item.clone();
                    Ok(())
                })?;
            }
            lease.assert_owner()?;
            if let Err(error) = shipper.confirm_configured_source(&client_uuid, &item.relayed_packet, &ack).await {
                return write_error_response_with_status(
                    socket, "502 Bad Gateway", "SYNC_SOURCE_CONFIRMATION_FAILED", &err_public(&error),
                ).await;
            }
            if let Err(error) = update_pair_state(&sync_state_file(), &pair.id, |s| {
                s.known.insert(
                    item.canonical_uuid.clone(),
                    KnownEntity {
                        canonical_uuid: item.canonical_uuid.clone(),
                        source_fingerprint: item.source_fingerprint.clone(),
                        vector_clock: item.vector_clock,
                        post_type: item.post_type.clone(),
                        post_status: item.post_status.clone(),
                        target_post_id: ack.target_id,
                        last_shipped_at: unix_ts(),
                        review_rejected: false,
                    },
                );
            }) {
                return write_error_response_with_status(
                    socket,
                    "500 Internal Server Error",
                    "SYNC_REVIEW_STATE_SAVE_FAILED",
                    &err_public(&error),
                )
                .await;
            }
            lease.assert_owner()?;
            if let Err(error) = finish_review_push(&path, id, &item, None) {
                return write_error_response_with_status(
                    socket,
                    "500 Internal Server Error",
                    "SYNC_REVIEW_SAVE_FAILED",
                    &err_public(&error),
                )
                .await;
            }
            crate::logging::log_event_global(
                "info",
                "sync_review.item_approved",
                json!({
                    "id": id,
                    "pair_id": item.pair_id,
                    "canonical_uuid": item.canonical_uuid,
                    "target_post_id": ack.target_id,
                }),
            );
            let payload = json!({
                "success": true,
                "data": { "id": id, "status": "pushed" }
            });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Ok(ack) => {
            let msg = format!(
                "target rejected ({}): {}",
                ack.status,
                ack.error.unwrap_or_default()
            );
            if let Err(error) = finish_review_push(&path, id, &item, Some(&msg)) {
                return write_error_response_with_status(
                    socket,
                    "500 Internal Server Error",
                    "SYNC_REVIEW_SAVE_FAILED",
                    &err_public(&error),
                )
                .await;
            }
            crate::logging::log_event_global(
                "warn",
                "sync_review.item_approve_failed",
                json!({ "id": id, "pair_id": item.pair_id, "error": msg }),
            );
            write_error_response(socket, "PUSH_REJECTED", &msg).await
        }
        Err(err) => {
            let msg = format!("{err:#}");
            if let Err(error) = finish_review_push(&path, id, &item, Some(&msg)) {
                return write_error_response_with_status(
                    socket,
                    "500 Internal Server Error",
                    "SYNC_REVIEW_SAVE_FAILED",
                    &err_public(&error),
                )
                .await;
            }
            crate::logging::log_event_global(
                "warn",
                "sync_review.item_approve_failed",
                json!({ "id": id, "pair_id": item.pair_id, "error": msg }),
            );
            write_error_response(socket, "PUSH_FAILED", &msg).await
        }
    }
}

fn finish_review_push(
    path: &str,
    id: &str,
    expected: &SyncReviewItem,
    error: Option<&str>,
) -> anyhow::Result<()> {
    update_sync_review(path, |doc| {
        let row = find_review_item_mut(doc, id)
            .ok_or_else(|| anyhow::anyhow!("review item removed during push"))?;
        if serde_json::to_value(&*row)? != serde_json::to_value(expected)? {
            anyhow::bail!("review item changed during push; local changes retained");
        }
        if error.is_none() {
            row.status = SyncReviewStatus::Pushed;
        }
        row.error_message = error.map(str::to_string);
        row.updated_at = unix_ts();
        doc.updated_at = unix_ts();
        Ok(())
    })
}

/// POST /api/sync-review/:id/reject
pub(super) async fn handle_sync_review_reject(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    id: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: serde_json::Value = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(value) if value.is_object() => value,
        _ => {
            return write_error_response(
                socket,
                "INVALID_PAYLOAD",
                "review rejection must be a JSON object",
            )
            .await
        }
    };
    let reason = req
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Rejected by reviewer");
    let path = sync_review_file();
    let Some(lease) = execution_lease_for_request(socket, &path, id).await? else {
        return Ok(());
    };
    let Some(_) = load_review_for_request(socket, &path).await? else {
        return Ok(());
    };
    let rejected = update_sync_review(&path, |doc| {
        let Some(item) = find_review_item_mut(doc, id) else {
            return Ok(ReviewEdit::NotFound);
        };
        if item.status != SyncReviewStatus::PendingReview || item.delivery.is_some() {
            return Ok(ReviewEdit::NotPending);
        }
        let pair_id = item.pair_id.clone();
        let canonical_uuid = item.canonical_uuid.clone();
        update_pair_state(&sync_state_file(), &pair_id, |state| {
            let previous = state.known.get(&canonical_uuid);
            let target_post_id = previous.and_then(|known| known.target_post_id);
            let last_shipped_at = previous.map(|known| known.last_shipped_at).unwrap_or(0);
            state.known.insert(
                canonical_uuid.clone(),
                KnownEntity {
                    canonical_uuid: canonical_uuid.clone(),
                    source_fingerprint: item.source_fingerprint.clone(),
                    vector_clock: item.vector_clock,
                    post_type: item.post_type.clone(),
                    post_status: item.post_status.clone(),
                    target_post_id,
                    last_shipped_at,
                    review_rejected: true,
                },
            );
        })
        .map_err(|error| error.context(ReviewStateSaveFailure))?;
        item.status = SyncReviewStatus::Rejected;
        item.error_message = Some(reason.to_string());
        item.updated_at = unix_ts();
        lease.assert_owner()?;
        let out = item.clone();
        doc.updated_at = unix_ts();
        Ok(ReviewEdit::Updated(out))
    });
    let out = match rejected {
        Ok(ReviewEdit::Updated(item)) => item,
        Ok(ReviewEdit::NotFound) => {
            return write_not_found_response(socket, "NOT_FOUND", "sync review item not found")
                .await
        }
        Ok(ReviewEdit::NotPending) => {
            return write_error_response(socket, "INVALID_STATUS", "item is not pending review")
                .await
        }
        Err(error) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                if error.is::<ReviewStateSaveFailure>() {
                    "SYNC_REVIEW_STATE_SAVE_FAILED"
                } else {
                    "SYNC_REVIEW_SAVE_FAILED"
                },
                &err_public(&error),
            )
            .await
        }
    };
    let pair_id = out.pair_id;
    let canonical_uuid = out.canonical_uuid;
    crate::logging::log_event_global(
        "info",
        "sync_review.item_rejected",
        json!({
            "id": id,
            "pair_id": pair_id,
            "canonical_uuid": canonical_uuid,
            "reason": reason,
        }),
    );
    let payload = json!({
        "success": true,
        "data": { "id": id, "status": "rejected", "reason": reason }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
