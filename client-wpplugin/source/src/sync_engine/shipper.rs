//! WPMMCC sync transport: HMAC-authenticated calls against the plugin REST
//! surface (`/wp-json/wpmmcc/v1/sync/*`) plus the packet relay transform.
//!
//! Endpoint contract (server: `wpmmcc/source/includes/rest/`):
//!   - POST /sync/digest          {limit, last_seen_id, post_types} → items[]
//!   - POST /sync/pull            {canonical_uuids ≤ 50}            → packets[]
//!   - POST /sync/push            one packet body                   → ack
//!   - POST /sync/reconcile-digest {fingerprints[]}                 → verdicts
//!   - POST /sync/media-chunk     {file_uuid, chunk_index, total, bytes} → ack
//!
//! All of them (except handshake) authenticate via the HMAC middleware:
//! headers X-WPMMCC-Site-UUID/Timestamp/Nonce/Signature over
//! `METHOD\nURI\ntimestamp\nnonce\npeer_uuid\nsha256(body)`.

use anyhow::{ensure, Context};
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::hmac::{build_hmac_headers, sha256_hex};
use super::packet::{rewrite_content_urls, SyncPacket, PACKET_SCHEMA_VERSION};
use super::types::SyncMode;
use super::types::SyncPair;
use crate::logging::{log_event_global, unix_ts};

/// One entry of `/sync/digest` output (Reconciler::generate_digest item).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigestItem {
    pub canonical_uuid: String,
    pub source_fingerprint: String,
    pub vector_clock: u64,
    pub local_id: u64,
    pub post_status: String,
    pub post_type: String,
    #[serde(default)]
    pub last_modified: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShipResult {
    pub success: bool,
    pub packet_id: String,
    pub target_id: Option<u64>,
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub receipt: Option<Value>,
    pub status: String,
    pub error: Option<String>,
}

/// Outcome of one HMAC call: HTTP-level result with parsed plugin envelope.
struct SyncCall {
    status_code: u16,
    ok: bool,
    body: Value,
}

pub(super) fn scoped_http_url(value: &str, rest_base: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(value).map_err(|_| anyhow::anyhow!("sync receipt URL is invalid"))?;
    let base = url::Url::parse(rest_base).map_err(|_| anyhow::anyhow!("sync target URL is invalid"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.origin() == base.origin(),
        "sync receipt URL does not belong to the configured target"
    );
    Ok(())
}

fn safe_network_error(error: reqwest::Error, stage: &'static str) -> anyhow::Error {
    anyhow::anyhow!(
        "{} failed ({})",
        stage,
        if error.is_timeout() { "timeout" } else { "transport" }
    )
}

/// Media chunk size for `/sync/media-chunk` uploads (plugin quota: 4MB
/// decoded per chunk, base64 on the wire; 1MB keeps headroom).
pub const MEDIA_CHUNK_BYTES: usize = 1024 * 1024;

/// Y-1 (07/10 audit, 12 批 A3): hard cap for a single downloaded media
/// asset, aligned with the plugin-side doc 14 §5.1 quota (512MB). Declared
/// Content-Length beyond this is refused up front; a lying or chunked body
/// is cut off at the same bound while streaming.
pub const MAX_DOWNLOAD_BYTES: usize = 512 * 1024 * 1024;

pub struct Shipper {
    client: Client,
    /// Dedicated download client (N-2): redirects are never followed —
    /// the source manifest's remote_url is attacker-influenceable, so a
    /// 30x surfaces as an error status instead of silently fetching a
    /// cross-host target. Built once, reused across downloads.
    download_client: Client,
    /// Configurable copy of MAX_DOWNLOAD_BYTES (tests inject small caps).
    max_download_bytes: usize,
    source_confirmation: Option<(String, Vec<u8>, String)>,
}

impl Shipper {
    pub fn new(_client: Client) -> anyhow::Result<Self> {
        // An opaque reqwest client cannot prove redirect or credential routing.
        let client = crate::auth::wp_http_client_builder().build()?;
        let download_client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("build media download client failed")?;
        Ok(Self {
            client,
            download_client,
            max_download_bytes: MAX_DOWNLOAD_BYTES,
            source_confirmation: None,
        })
    }

    pub fn with_source_confirmation(
        mut self,
        source_rest_base: String,
        source_secret: Vec<u8>,
        target_uuid: String,
    ) -> Self {
        self.source_confirmation = Some((source_rest_base, source_secret, target_uuid));
        self
    }

    pub async fn confirm_configured_source(
        &self,
        client_uuid: &str,
        packet: &SyncPacket,
        ack: &ShipResult,
    ) -> anyhow::Result<()> {
        if packet.action != "upsert" {
            return Ok(());
        }
        let (source, secret, target) = self.source_confirmation.as_ref()
            .context("sync source confirmation authority is missing; original target receipt retained")?;
        self.confirm_source(source, client_uuid, secret, target, packet, ack).await
    }

    pub async fn prepare_configured_source(
        &self,
        client_uuid: &str,
        target_rest_base: &str,
        packet: &SyncPacket,
    ) -> anyhow::Result<()> {
        if packet.action != "upsert" {
            return Ok(());
        }
        let (source, secret, target) = self.source_confirmation.as_ref()
            .context("sync source preparation authority is missing")?;
        let suffix = "/wp-json/wpmmcc/v1";
        let target_url = target_rest_base.trim_end_matches('/').strip_suffix(suffix)
            .context("sync target REST root is invalid")?;
        let call = self.signed_post(source, "/sync/prepare", client_uuid, secret,
            &json!({"packet":packet,"target_site_uuid":target,"target_site_url":target_url})).await?;
        ensure!(call.ok && call.body["data"]["prepared"] == true
            && call.body["data"]["packet_id"] == packet.packet_id
            && call.body["data"]["canonical_uuid"] == packet.entity.guid
            && call.body["data"]["target_site_uuid"] == target.as_str(),
            "sync source authority was not saved; no target write started");
        Ok(())
    }



    /// Signed POST to one wpmmcc/v1 sync endpoint. `rest_base` is the
    /// site's `scheme://host` + `/wp-json/wpmmcc/v1`.
    async fn signed_post(
        &self,
        rest_base: &str,
        route: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        body: &Value,
    ) -> anyhow::Result<SyncCall> {
        let body_bytes = serde_json::to_vec(body).context("serialize sync request body")?;
        self.signed_post_bytes(rest_base, route, client_uuid, shared_secret, &body_bytes)
            .await
    }

    fn call_error(call: &SyncCall, context: &str) -> anyhow::Error {
        anyhow::anyhow!("{} rejected (HTTP {})", context, call.status_code)
    }

    /// `/sync/digest` — incremental entity discovery with a keyset cursor.
    pub async fn fetch_digest(
        &self,
        pair_id: &str,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        last_seen_id: u64,
        post_types: &[String],
        limit: u32,
    ) -> anyhow::Result<Vec<DigestItem>> {
        let body = json!({
            "limit": limit,
            "last_seen_id": last_seen_id,
            "post_types": post_types,
        });
        let call = self
            .signed_post(rest_base, "/sync/digest", client_uuid, shared_secret, &body)
            .await?;
        if !call.ok {
            log_event_global(
                "warning",
                "sync_engine.digest_rejected",
                json!({
                "pair_id": pair_id,
                                        "http_status": call.status_code,
                    "code": call.body["code"].as_str().unwrap_or("wpmmcc_error"),
                    "last_seen_id": last_seen_id,
                }),
            );
            return Err(Self::call_error(&call, "sync digest"));
        }
        let items = call
            .body
            .pointer("/data/items")
            .cloned()
            .context("sync digest has no items array; cursor retained")?;
        let items: Vec<DigestItem> =
            serde_json::from_value(items).context("parse sync digest items failed")?;
        ensure!(items.len() <= limit as usize, "sync digest exceeds requested window; cursor retained");
        let mut previous_id = last_seen_id;
        let mut identities = std::collections::HashSet::new();
        for item in &items {
            ensure!(
                !item.canonical_uuid.trim().is_empty()
                    && !item.source_fingerprint.trim().is_empty()
                    && item.vector_clock > 0
                    && item.local_id > previous_id
                    && identities.insert(&item.canonical_uuid),
                "sync digest identity or cursor is invalid; cursor retained"
            );
            previous_id = item.local_id;
        }
        log_event_global(
            "info",
            "sync_engine.digest_fetched",
            json!({
            "pair_id": pair_id,
                                "items": items.len(),
                "last_seen_id": last_seen_id,
                "limit": limit,
            }),
        );
        Ok(items)
    }

    /// `/sync/pull` — full packets for known canonical UUIDs (batches ≤ 50).
    pub async fn pull_packets(
        &self,
        pair_id: &str,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        canonical_uuids: &[String],
    ) -> anyhow::Result<Vec<SyncPacket>> {
        let uuids: Vec<&str> = canonical_uuids.iter().map(String::as_str).collect();
        let body = json!({ "canonical_uuids": uuids });
        let call = self
            .signed_post(rest_base, "/sync/pull", client_uuid, shared_secret, &body)
            .await?;
        if !call.ok {
            log_event_global(
                "warning",
                "sync_engine.pull_batch_rejected",
                json!({
                "pair_id": pair_id,
                                        "http_status": call.status_code,
                    "code": call.body["code"].as_str().unwrap_or("wpmmcc_error"),
                    "requested": canonical_uuids.len(),
                }),
            );
            return Err(Self::call_error(&call, "sync pull"));
        }
        let packets = call
            .body
            .pointer("/data/packets")
            .cloned()
            .context("sync pull has no packets array; original scope retained")?;
        let packets: Vec<SyncPacket> =
            serde_json::from_value(packets).context("parse sync pull packets failed")?;
        let mut identities = std::collections::HashSet::new();
        ensure!(packets.len() <= canonical_uuids.len(), "sync pull returned an unexpected packet count");
        for packet in &packets {
            ensure!(
                canonical_uuids.contains(&packet.entity.guid)
                    && identities.insert(&packet.entity.guid)
                    && packet.schema_version == PACKET_SCHEMA_VERSION
                    && !packet.packet_id.trim().is_empty()
                    && !packet.source_fingerprint.trim().is_empty()
                    && !packet.origin_context.origin_site_uuid.trim().is_empty()
                    && matches!(packet.action.as_str(), "upsert" | "trash" | "delete"),
                "sync pull returned an invalid or foreign packet; original scope retained"
            );
        }
        ensure!(packets.len() == canonical_uuids.len(), "sync pull omitted requested entities; cursor retained");
        log_event_global(
            "info",
            "sync_engine.pull_batch_completed",
            json!({
            "pair_id": pair_id,
                                "requested": canonical_uuids.len(),
                "received": packets.len(),
            }),
        );
        Ok(packets)
    }

    /// `/sync/reconcile-digest` — drift verdicts for the client's known set.
    pub async fn reconcile_fingerprints(
        &self,
        pair_id: &str,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        fingerprints: &[(String, String, u64)],
    ) -> anyhow::Result<HashMap<String, String>> {
        let entries: Vec<Value> = fingerprints
            .iter()
            .map(|(uuid, fp, clock)| {
                json!({
                    "canonical_uuid": uuid,
                    "source_fingerprint": fp,
                    "vector_clock": clock,
                })
            })
            .collect();
        let body = json!({ "fingerprints": entries });
        let call = self
            .signed_post(
                rest_base,
                "/sync/reconcile-digest",
                client_uuid,
                shared_secret,
                &body,
            )
            .await?;
        if !call.ok {
            log_event_global(
                "warning",
                "sync_engine.reconcile_rejected",
                json!({
                "pair_id": pair_id,
                                        "http_status": call.status_code,
                    "code": call.body["code"].as_str().unwrap_or("wpmmcc_error"),
                    "fingerprints": fingerprints.len(),
                }),
            );
            return Err(Self::call_error(&call, "sync reconcile-digest"));
        }
        let verdicts = call
            .body
            .pointer("/data/verdicts")
            .cloned()
            .context("sync reconcile has no verdicts; original scope retained")?;
        let verdicts: HashMap<String, String> =
            serde_json::from_value(verdicts).context("parse reconcile verdicts failed")?;
        ensure!(
            verdicts.len() == fingerprints.len()
                && fingerprints.iter().all(|(id, _, _)| verdicts.get(id).is_some_and(|value|
                    matches!(value.as_str(), "in_sync" | "source_newer" | "local_edited" | "missing")
                )),
            "sync reconcile verdicts are incomplete or foreign; original scope retained"
        );
        let mut verdict_counts: HashMap<&str, usize> = HashMap::new();
        for v in verdicts.values() {
            *verdict_counts.entry(v.as_str()).or_insert(0) += 1;
        }
        log_event_global(
            "info",
            "sync_engine.reconcile_completed",
            json!({
            "pair_id": pair_id,
                                "fingerprints": fingerprints.len(),
                "verdicts": verdicts.len(),
                "verdict_counts": verdict_counts,
            }),
        );
        Ok(verdicts)
    }

    /// `/sync/push` — relay one packet to the target site.
    pub async fn push_packet(
        &self,
        pair_id: &str,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        packet: &SyncPacket,
    ) -> anyhow::Result<ShipResult> {
        let packet_json = serde_json::to_vec(packet).context("serialize sync packet failed")?;
        let call = self
            .signed_post_bytes(
                rest_base,
                "/sync/push",
                client_uuid,
                shared_secret,
                &packet_json,
            )
            .await?;

        let packet_id = packet.packet_id.clone();
        if !call.ok {
            let err_msg = format!("sync push rejected (HTTP {})", call.status_code);
            log_event_global(
                "warning",
                "sync_engine.push_rejected",
                json!({
                "pair_id": pair_id,
                                        "packet_id": packet_id,
                    "http_status": call.status_code,
                    "error": err_msg,
                }),
            );
            return Ok(ShipResult {
                success: false,
                packet_id,
                target_id: None,
                target_url: None,
                receipt: None,
                status: format!("http_{}", call.status_code),
                error: Some(err_msg),
            });
        }

        self.validate_packet_receipt(rest_base, packet, &call.body["data"])
    }

    pub async fn find_packet_receipt(
        &self,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        packet: &SyncPacket,
    ) -> anyhow::Result<ShipResult> {
        let call = self.signed_post(rest_base, "/sync/receipt", client_uuid, shared_secret, &serde_json::to_value(packet)?).await?;
        ensure!(call.ok, "original sync effect has no confirmed target receipt; no reapply");
        let data = &call.body["data"];
        if data["committed"] == false && data["phase"] == "absent" {
            let (_, _, target) = self.source_confirmation.as_ref()
                .context("sync zero-effect proof has no configured target authority")?;
            ensure!(data["packet_id"] == packet.packet_id
                && data["canonical_uuid"] == packet.entity.guid
                && data["origin_site_uuid"] == packet.origin_context.origin_site_uuid
                && data["fingerprint_ack"] == packet.source_fingerprint
                && data["target_lang"] == packet.target_lang
                && data["target_site_uuid"] == target.as_str(),
                "sync zero-effect proof scope differs; original packet retained");
            return Ok(ShipResult {
                success: false, packet_id: packet.packet_id.clone(), target_id: None,
                target_url: None, receipt: None, status: "absent".into(), error: None,
            });
        }
        self.validate_packet_receipt(rest_base, packet, &call.body["data"])
    }

    /// Receipt-first recovery. Only an authenticated, lease-checked absence
    /// proof allows sending the exact already-frozen packet again.
    pub async fn resume_submitted_packet(
        &self,
        pair_id: &str,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        packet: &SyncPacket,
    ) -> anyhow::Result<ShipResult> {
        let result = self.find_packet_receipt(rest_base, client_uuid, shared_secret, packet).await?;
        if result.status == "absent" {
            self.push_packet(pair_id, rest_base, client_uuid, shared_secret, packet).await
        } else {
            Ok(result)
        }
    }

    pub(crate) fn validate_saved_result(
        &self,
        rest_base: &str,
        packet: &SyncPacket,
        result: &ShipResult,
    ) -> anyhow::Result<()> {
        let receipt = result.receipt.as_ref().context("saved sync result has no committed receipt")?;
        let checked = self.validate_packet_receipt(rest_base, packet, receipt)?;
        ensure!(serde_json::to_value(checked)? == serde_json::to_value(result)?,
            "saved sync receipt projection differs; original packet retained");
        Ok(())
    }

    fn validate_packet_receipt(
        &self,
        rest_base: &str,
        packet: &SyncPacket,
        data: &Value,
    ) -> anyhow::Result<ShipResult> {
        ensure!(
            data["committed"] == true
                && data["packet_id"] == packet.packet_id
                && data["fingerprint_ack"] == packet.source_fingerprint
                && data["canonical_uuid"] == packet.entity.guid
                && data["origin_site_uuid"] == packet.origin_context.origin_site_uuid,
            "sync push has no matching committed receipt; original packet retained"
        );
        let (_, _, expected_target) = self.source_confirmation.as_ref()
            .context("sync receipt has no configured target authority; original packet retained")?;
        ensure!(data["target_site_uuid"] == expected_target.as_str()
            && data["target_lang"] == packet.target_lang,
            "sync push receipt target identity or language differs; original packet retained");
        let target_id = data["target_id"].as_u64().context("sync push receipt target ID is invalid")?;
        let outcome_status = data["status"].as_str().context("sync push receipt status is missing")?;
        ensure!(matches!(outcome_status, "success" | "skipped" | "conflict"),
            "sync push is not terminal; original packet retained");
        let target_url = data["target_url"].as_str().context("sync push receipt target URL is missing")?;
        if packet.action == "upsert" {
            ensure!(target_id > 0, "sync push has no materialized target; original packet retained");
            scoped_http_url(target_url, rest_base)?;
        }
        let success = matches!(outcome_status, "success" | "skipped");
        log_event_global(
            "info",
            "sync_engine.push_ok",
            json!({
            "packet_id": packet.packet_id,
                "target_id": target_id,
                "status": outcome_status,
            }),
        );
        Ok(ShipResult {
            success,
            packet_id: packet.packet_id.clone(),
            target_id: (target_id > 0).then_some(target_id),
            target_url: (!target_url.is_empty()).then(|| target_url.to_string()),
            receipt: Some(data.clone()),
            status: outcome_status.into(),
            error: (!success).then(|| "sync target requires conflict review; original packet retained".into()),
        })
    }

    pub async fn confirm_source(
        &self,
        source_rest_base: &str,
        client_uuid: &str,
        source_secret: &[u8],
        expected_target_uuid: &str,
        packet: &SyncPacket,
        ack: &ShipResult,
    ) -> anyhow::Result<()> {
        if packet.action != "upsert" {
            return Ok(());
        }
        let receipt = ack.receipt.as_ref().context("sync source confirmation has no target receipt")?;
        ensure!(ack.success && receipt["target_site_uuid"] == expected_target_uuid,
            "sync receipt does not belong to the paired target; original packet retained");
        let call = self.signed_post(source_rest_base, "/sync/confirm", client_uuid, source_secret,
            &json!({"packet":packet,"receipt":receipt})).await?;
        ensure!(call.ok && call.body["data"]["confirmed"] == true
            && call.body["data"]["packet_id"] == packet.packet_id
            && call.body["data"]["canonical_uuid"] == packet.entity.guid
            && call.body["data"]["target_site_uuid"] == expected_target_uuid,
            "sync source mapping was not confirmed; original target receipt retained");
        Ok(())
    }

    /// Signed POST with a raw body (the push packet must be byte-exact —
    /// the fingerprint in it was computed by the source site).
    async fn signed_post_bytes(
        &self,
        rest_base: &str,
        route: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        body: &[u8],
    ) -> anyhow::Result<SyncCall> {
        let canonical_uri = format!("/wpmmcc/v1{}", route);
        let headers = build_hmac_headers(
            client_uuid,
            shared_secret,
            "POST",
            &canonical_uri,
            body,
            unix_ts(),
        )?;
        let nonce = headers.iter().find(|(key, _)| key == "X-WPMMCC-Nonce")
            .map(|(_, value)| value.clone()).context("sync request nonce is missing")?;
        let timestamp = headers.iter().find(|(key, _)| key == "X-WPMMCC-Timestamp")
            .map(|(_, value)| value.clone()).context("sync request timestamp is missing")?;
        let url = format!("{}{}", rest_base.trim_end_matches('/'), route);
        let parsed = url::Url::parse(&url).map_err(|_| anyhow::anyhow!("sync endpoint is invalid"))?;
        ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "sync endpoint scope is invalid"
        );
        crate::component_rt::runner::assert_provider_url_allowed(&url)
            .map_err(|_| anyhow::anyhow!("sync endpoint refused by egress policy"))?;
        let mut req = self.client.post(&url);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let mut resp = req
            .body(body.to_vec())
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await
            .map_err(|error| safe_network_error(error, "sync request"))?;
        let status_code = resp.status().as_u16();
        if !resp.status().is_success() {
            return Ok(SyncCall { status_code, ok: false, body: Value::Null });
        }
        let response_signature = resp.headers().get("x-wpmmcc-response-signature")
            .and_then(|value| value.to_str().ok()).map(String::from)
            .context("sync response has no authenticated receipt; original intent retained")?;
        const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
        ensure!(resp.content_length().is_none_or(|bytes| bytes <= MAX_RESPONSE_BYTES as u64),
            "sync response exceeds 16 MiB; original state retained");
        let mut body_bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|error| safe_network_error(error, "sync response"))? {
            ensure!(body_bytes.len().checked_add(chunk.len()).is_some_and(|bytes| bytes <= MAX_RESPONSE_BYTES),
                "sync response exceeds 16 MiB; original state retained");
            body_bytes.extend_from_slice(&chunk);
        }
        let signed = super::hmac::response_string_to_sign(
            &nonce, &timestamp, client_uuid, "POST", &canonical_uri, status_code, &body_bytes,
        );
        super::hmac::verify_response(&response_signature, &signed, shared_secret)?;
        let parsed: Value = serde_json::from_slice(&body_bytes)
            .map_err(|_| anyhow::anyhow!("sync response is invalid JSON; original state retained"))?;
        ensure!(parsed.is_object() && parsed["success"] == true && parsed["data"].is_object(),
            "sync response has no successful data envelope; original state retained");
        Ok(SyncCall {
            status_code,
            ok: true,
            body: parsed,
        })
    }

    /// `/sync/media-chunk` requires the frozen byte evidence (expected
    /// SHA-256 + size). The evidence-free wrapper was removed: a chunk
    /// without the original asset digest can never be admitted.
    pub(super) async fn media_receipt(
        &self,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        scope: &Value,
    ) -> anyhow::Result<Value> {
        let call = self.signed_post(rest_base, "/sync/media-receipt", client_uuid, shared_secret, scope).await?;
        ensure!(call.ok, "original sync media receipt unavailable; no new session");
        Ok(call.body["data"].clone())
    }

    pub(super) async fn upload_media_chunk_with_evidence(
        &self,
        pair_id: &str,
        rest_base: &str,
        client_uuid: &str,
        shared_secret: &[u8],
        file_uuid: &str,
        chunk_index: u32,
        total: u32,
        bytes: &[u8],
        filename: &str,
        mime_type: &str,
        expected_sha256: &str,
        expected_size: u64,
    ) -> anyhow::Result<MediaChunkAck> {
        ensure!(!file_uuid.is_empty() && total > 0 && total <= 512 && chunk_index < total
            && !bytes.is_empty() && bytes.len() <= 4 * 1024 * 1024,
            "sync media chunk scope is invalid");
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let body = json!({
            "file_uuid": file_uuid,
            "chunk_index": chunk_index,
            "total": total,
            "bytes": encoded,
            "filename": filename,
            "mime_type": mime_type,
            "expected_sha256": expected_sha256,
            "expected_size": expected_size,
        });
        let call = self
            .signed_post(
                rest_base,
                "/sync/media-chunk",
                client_uuid,
                shared_secret,
                &body,
            )
            .await?;
        if !call.ok {
            log_event_global(
                "warning",
                "sync_engine.media_chunk_rejected",
                json!({
                "pair_id": pair_id,
                                        "file_uuid": file_uuid,
                    "chunk_index": chunk_index,
                    "total": total,
                    "http_status": call.status_code,
                    "code": call.body["code"].as_str().unwrap_or("wpmmcc_error"),
                }),
            );
            return Err(Self::call_error(&call, "sync media-chunk"));
        }
        let data = &call.body["data"];
        ensure!(data["chunk_acked"] == true
            && data["file_uuid"] == file_uuid
            && data["chunk_index"].as_u64() == Some(chunk_index as u64)
            && data["total"].as_u64() == Some(total as u64)
            && data["completed"].is_boolean(),
            "sync media chunk receipt differs; original asset retained");
        let ack = MediaChunkAck {
            completed: data["completed"].as_bool().unwrap_or(false),
            assembled_sha256: data["assembled_sha256"].as_str().unwrap_or("").to_string(),
            attachment_id: data["attachment_id"].as_u64().unwrap_or(0),
            attachment_url: data["attachment_url"].as_str().unwrap_or("").to_string(),
            reused: data["reused"].as_bool().unwrap_or(false),
        };
        if ack.completed {
            ensure!(ack.attachment_id > 0 && ack.assembled_sha256.len() == 64
                && ack.assembled_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "sync media completion receipt is incomplete; original asset retained");
            scoped_http_url(&ack.attachment_url, rest_base)?;
        }
        // Item-level media visibility: log the assembled outcome (final
        // chunk) at info; intermediate chunks only at debug to keep the
        // JSONL tail readable during large multi-chunk transfers.
        if ack.completed {
            log_event_global(
                "info",
                "sync_engine.media_assembled",
                json!({
                "pair_id": pair_id,
                                        "file_uuid": file_uuid,
                    "chunks": total,
                    "attachment_id": ack.attachment_id,
                    "reused": ack.reused,
                }),
            );
        } else {
            log_event_global(
                "debug",
                "sync_engine.media_chunk_uploaded",
                json!({
                "pair_id": pair_id,
                                        "file_uuid": file_uuid,
                    "chunk_index": chunk_index,
                    "total": total,
                }),
            );
        }
        Ok(ack)
    }

    /// Download a media asset from the source site by its public URL
    /// (attachment URLs in pulled packets are plain GETs).
    ///
    /// Y-1 (07/10 audit, 12 批 A3): the manifest's remote_url is
    /// attacker-influenceable (a compromised source site can point the
    /// client at internal endpoints and relay the bytes to the target via
    /// transfer_media). Three guards: the provider egress floor (literal +
    /// post-resolve metadata IPs, incl. IPv4-mapped IPv6 forms), a
    /// no-redirect client (cross-host 30x never followed), and the
    /// MAX_DOWNLOAD_BYTES cap (declared up front, enforced while
    /// streaming).
    pub async fn download_media(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        crate::component_rt::runner::assert_provider_url_allowed(url)
            .map_err(|_| anyhow::anyhow!("download media rejected by egress policy"))?;
        let mut response = self
            .download_client
            .get(url)
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await
            .map_err(|error| safe_network_error(error, "download media"))?;
        // error_for_status only rejects 4xx/5xx; a 30x here means the
        // no-redirect client refused a hop (cross-host by policy) — the
        // redirect body must never be mistaken for media bytes.
        if response.status().is_redirection() {
            anyhow::bail!("download media redirect not followed (egress policy)");
        }
        ensure!(response.status().is_success(), "download media rejected (HTTP {})", response.status().as_u16());
        if let Some(declared) = response.content_length() {
            if declared as usize > self.max_download_bytes {
                anyhow::bail!(
                    "download media declares {} bytes, over the {} byte cap",
                    declared,
                    self.max_download_bytes
                );
            }
        }
        let mut data: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| safe_network_error(error, "media body"))? {
            if data.len().checked_add(chunk.len()).is_none_or(|bytes| bytes > self.max_download_bytes) {
                anyhow::bail!(
                    "download media exceeded the {} byte cap while streaming",
                    self.max_download_bytes
                );
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    /// Transfer one media asset source → target in chunks, returning the
    /// target-side attachment URL.
    pub async fn transfer_media(
        &self,
        pair_id: &str,
        source_url: &str,
        target_rest_base: &str,
        client_uuid: &str,
        target_secret: &[u8],
    ) -> anyhow::Result<TransferMediaResult> {
        let operation = crate::db::system::private_json_digest(&json!({
            "pair":pair_id, "source":source_url, "target":target_rest_base, "client":client_uuid,
        }))?;
        self.transfer_media_for_operation(&operation,pair_id,source_url,target_rest_base,client_uuid,target_secret).await
    }

    pub(crate) async fn transfer_media_for_operation(
        &self, operation: &str, pair_id: &str, source_url: &str,
        target_rest_base: &str, client_uuid: &str, target_secret: &[u8],
    ) -> anyhow::Result<TransferMediaResult> {
        super::media_delivery::transfer(self,operation,pair_id,source_url,target_rest_base,client_uuid,target_secret).await
    }
}

#[derive(Debug, Clone)]
pub struct MediaChunkAck {
    pub completed: bool,
    pub assembled_sha256: String,
    pub attachment_id: u64,
    pub attachment_url: String,
    pub reused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferMediaResult {
    pub target_url: String,
    pub attachment_id: u64,
    pub sha256: String,
    pub reused: bool,
    pub size_bytes: usize,
}

pub(super) fn mime_for_filename(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".webm") {
        "video/webm"
    } else if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".wav") {
        "audio/wav"
    } else if lower.ends_with(".ogg") {
        "audio/ogg"
    } else {
        "application/octet-stream"
    }
}

/// Translated fields for a relay (None fields keep the source values).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelayTranslation {
    pub title: Option<String>,
    pub content: Option<String>,
    pub excerpt: Option<String>,
}

/// Relay transform: adapt a packet pulled from the source site for push to
/// the target site. Content URLs are rewritten source→target; media
/// manifest entries are rewritten by the caller (after transfer); hop
/// count advances; the origin context stays the SOURCE site's (canonical
/// UUID, fingerprint and vector-clock semantics belong to it).
pub fn relay_packet_for_target(
    packet: &SyncPacket,
    pair: &SyncPair,
    translated: Option<RelayTranslation>,
    media_url_map: &HashMap<String, String>,
) -> SyncPacket {
    let mut out = packet.clone();
    out.packet_id = format!("pkt_{}", uuid::Uuid::new_v4().simple());
    out.origin_context.hop_count = packet.origin_context.hop_count.saturating_add(1);
    out.target_lang = pair.target_lang.clone();
    out.sync_mode = match pair.sync_mode {
        SyncMode::SyncOnly => "sync_only".to_string(),
        SyncMode::SyncAndTranslate => "sync_and_translate".to_string(),
    };

    if let Some(translation) = &translated {
        if let Some(v) = &translation.title {
            out.entity
                .core_fields
                .insert("post_title".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &translation.excerpt {
            out.entity
                .core_fields
                .insert("post_excerpt".to_string(), Value::String(v.clone()));
        }
    }

    // Exact per-asset media rewrites FIRST (map keys are source-site URLs;
    // the domain rewrite below would erase them), then the domain-wide
    // rewrite for every remaining source URL shape.
    let source_content = out
        .entity
        .core_fields
        .get("post_content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let base_content = if let Some(translation) = &translated {
        if let Some(v) = &translation.content {
            v.clone()
        } else {
            source_content
        }
    } else {
        source_content
    };
    let mut rewritten = base_content;
    for (source_url, target_url) in media_url_map {
        rewritten = rewritten.replace(source_url.as_str(), target_url.as_str());
    }
    rewritten = rewrite_content_urls(&rewritten, &pair.source_domain, &pair.target_domain);
    out.entity
        .core_fields
        .insert("post_content".to_string(), Value::String(rewritten));

    for entry in out.multimodal_manifest.iter_mut() {
        if let Some(remote) = entry.get("remote_url").and_then(Value::as_str) {
            if let Some(target_url) = media_url_map.get(remote) {
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert("remote_url".to_string(), Value::String(target_url.clone()));
                }
            }
        }
    }

    out
}

/// Build a minimal `trash`/`delete` action packet for one canonical UUID
/// (the target's materializer only needs the guid + object type for these
/// actions; it maps via cross-mappings). The origin identity is the real
/// configured source site/language of the pair — never a placeholder.
pub fn build_tombstone_packet(
    guid: &str,
    subtype: &str,
    source_id: u64,
    source_uuid: &str,
    source_clock: u64,
    action: &str,
    origin_site_url: &str,
    origin_lang: &str,
    target_lang: &str,
) -> SyncPacket {
    let mut vector_clock = HashMap::new();
    vector_clock.insert(source_uuid.to_string(), source_clock);
    SyncPacket {
        schema_version: PACKET_SCHEMA_VERSION.to_string(),
        packet_id: format!("pkt_{}", uuid::Uuid::new_v4().simple()),
        origin_context: super::packet::OriginContext {
            origin_site_uuid: source_uuid.to_string(),
            origin_site_url: origin_site_url.to_string(),
            origin_permalink: None,
            origin_blog_id: 1,
            origin_lang: origin_lang.to_string(),
            vector_clock,
            hop_count: 1,
            dispatch_timestamp: unix_ts(),
        },
        action: action.to_string(),
        sync_mode: "sync_only".to_string(),
        target_lang: target_lang.to_string(),
        // Tombstones carry a marker fingerprint; the target does not
        // verify it for delete/trash actions (mapping-keyed).
        source_fingerprint: sha256_hex(format!("{guid}|{action}").as_bytes()),
        entity: super::packet::EntityPayload {
            guid: guid.to_string(),
            object_type: "post".to_string(),
            subtype: subtype.to_string(),
            source_id,
            slug: String::new(),
            status: "trash".to_string(),
            author_hint: None,
            core_fields: HashMap::new(),
            taxonomies: HashMap::new(),
            meta_fields: HashMap::new(),
            plugin_specific: HashMap::new(),
            changed_fields: None,
        },
        multimodal_manifest: Vec::new(),
    }
}
