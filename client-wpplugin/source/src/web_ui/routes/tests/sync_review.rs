//! G-04: sync-review route coverage — the five human-review endpoints
//! (list / detail / update / approve / reject) through the real dispatcher,
//! including the full approve → HMAC-signed `/sync/push` journey against a
//! mock target site and the push-rejected error path.

use serde_json::json;
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::{sync_pairs_file, sync_peer_credentials_file, sync_state_file};
use crate::sync_engine::packet::{EntityPayload, OriginContext, SyncPacket, PACKET_SCHEMA_VERSION};
use crate::sync_engine::{
    // catalog: WEBUI-API-PREFIX-api-sync-review
    // oracle: L1
    // (route-level review roundtrip: list/filters/count_only/detail via real harness; F-T1 annotation batch 2026-09-22)
    load_peer_credentials,
    load_sync_pairs,
    load_sync_review,
    load_sync_state,
    save_peer_credentials,
    save_sync_pairs,
    save_sync_review,
    sync_review_file,
    upsert_pair_in_doc,
    upsert_pending_item,
    ConflictStrategy,
    PeerCredential,
    SyncDirection,
    SyncFrequency,
    SyncMode,
    SyncPair,
    SyncPairStatus,
    SyncReviewItem,
    SyncReviewStatus,
};
use crate::web_ui::test_support::WebUiTestHarness;
use crate::sync_engine::discoverer::tests::{MockSite, credentials_doc};

/// Minimal relay packet, shaped exactly like the packets the discoverer
/// parks for review (see `src/sync_engine/discoverer.rs` pull/translate lane).
fn test_packet(uuid: &str, title: &str) -> SyncPacket {
    let mut core_fields = HashMap::new();
    core_fields.insert("post_title".to_string(), json!(title));
    core_fields.insert("post_content".to_string(), json!("<p>body</p>"));
    SyncPacket {
        schema_version: PACKET_SCHEMA_VERSION.to_string(),
        packet_id: format!("pkt-{uuid}"),
        origin_context: OriginContext {
            origin_site_uuid: "22222222-2222-2222-2222-222222222222".to_string(),
            origin_site_url: "https://src.example.com".to_string(),
            origin_permalink: Some("https://src.example.com/original/".into()),
            origin_blog_id: 1,
            origin_lang: "en_US".to_string(),
            vector_clock: HashMap::from([("22222222-2222-2222-2222-222222222222".into(), 1)]),
            hop_count: 1,
            dispatch_timestamp: 1,
        },
        action: "upsert".to_string(),
        sync_mode: "sync_only".to_string(),
        target_lang: "zh_CN".to_string(),
        source_fingerprint: crate::sync_engine::hmac::sha256_hex(uuid.as_bytes()),
        entity: EntityPayload {
            guid: uuid.to_string(),
            object_type: "post".to_string(),
            subtype: "post".to_string(),
            source_id: 1,
            slug: format!("slug-{uuid}"),
            status: "publish".to_string(),
            author_hint: None,
            core_fields,
            taxonomies: HashMap::new(),
            meta_fields: HashMap::new(),
            plugin_specific: HashMap::new(),
            changed_fields: None,
            preserved_fields: None,
        },
        multimodal_manifest: vec![],
    }
}

fn pending_item(id: &str, pair_id: &str, uuid: &str, source_title: &str) -> SyncReviewItem {
    SyncReviewItem {
        id: id.to_string(),
        pair_id: pair_id.to_string(),
        canonical_uuid: uuid.to_string(),
        status: SyncReviewStatus::PendingReview,
        source_title: source_title.to_string(),
        source_content: "<p>source</p>".to_string(),
        source_excerpt: String::new(),
        proposed_title: format!("{source_title} (translated)"),
        proposed_content: "<p>proposed</p>".to_string(),
        proposed_excerpt: String::new(),
        relayed_packet: test_packet(uuid, source_title),
        source_fingerprint: crate::sync_engine::hmac::sha256_hex(uuid.as_bytes()),
        vector_clock: 1,
        post_type: "post".to_string(),
        post_status: "publish".to_string(),
        error_message: None,
        delivery: None,
        created_at: 1,
        updated_at: 1,
    }
}

fn seed_review_items(items: &[SyncReviewItem]) {
    let path = sync_review_file();
    let mut doc = load_sync_review(&path).unwrap_or_default();
    for item in items {
        upsert_pending_item(&mut doc, item.clone());
    }
    save_sync_review(&path, &doc).expect("seed sync review doc");
}

/// Seed a sync pair plus a peer credential for its TARGET domain, pointing
/// the target at `target_base` (the mock site), exactly like a completed
/// pairing handshake would have left on disk.
fn seed_pair_with_peers(pair_id: &str, source: &MockSite, target: &MockSite) {
    let target_base = &target.base_url;
    let mut pairs = load_sync_pairs(&sync_pairs_file()).unwrap_or_default();
    upsert_pair_in_doc(
        &mut pairs,
        SyncPair {
            id: pair_id.to_string(),
            name: format!("Pair {pair_id}"),
            source_domain: source.base_url.clone(),
            target_domain: target_base.trim_end_matches('/').to_string(),
            direction: SyncDirection::Unidirectional,
            sync_mode: SyncMode::SyncOnly,
            source_lang: "en_US".to_string(),
            target_lang: "zh_CN".to_string(),
            conflict_strategy: ConflictStrategy::Lww,
            sync_frequency: SyncFrequency::Manual,
            post_types: vec!["post".to_string()],
            status: SyncPairStatus::Active,
            last_sync_at: None,
            last_seen_source_id: None,
            last_sync_count: None,
            last_error: None,
            translate_component_id: None,
            field_actions: Vec::new(),
            review_before_push: true,
            created_at: 0,
            updated_at: 0,
        },
    );
    save_sync_pairs(&sync_pairs_file(), &pairs).expect("seed sync pairs doc");

    let creds = credentials_doc(source, target);
    save_peer_credentials(&sync_peer_credentials_file(), &creds).expect("seed peer credentials");
}

/// Read one full HTTP request (headers + Content-Length body) from a socket
/// and return its raw text — used by the mock target site.
async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(header_end) = text.find("\r\n\r\n") {
            let content_length = text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.trim().eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            // Header block itself ends with the CRLFCRLF; body bytes follow.
            let header_bytes = text[..header_end].len() + 4;
            let body_bytes = buf.len() - header_bytes;
            if body_bytes >= content_length {
                return text;
            }
        }
        let read = socket.read(&mut chunk).await.expect("mock target read");
        if read == 0 {
            return String::from_utf8_lossy(&buf).to_string();
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

#[tokio::test]
async fn sync_review_list_detail_update_reject_roundtrip() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    seed_review_items(&[
        pending_item("sr_a1", "sp_a", "uuid-a1", "Source Alpha"),
        pending_item("sr_a2", "sp_a", "uuid-a2", "Source Beta"),
        pending_item("sr_b1", "sp_b", "uuid-b1", "Source Gamma"),
    ]);

    // Full pending list across pairs.
    let res = harness.get_json("/api/sync-review").await.unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "list: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["total"], 3);
    assert_eq!(res.body["data"]["items"].as_array().map(Vec::len), Some(3));

    // Pair-filtered list.
    let res = harness
        .get_json("/api/sync-review?pair_id=sp_a")
        .await
        .unwrap();
    assert_eq!(res.body["data"]["total"], 2);
    assert!(res.raw_body.contains("sr_a1"));
    assert!(res.raw_body.contains("sr_a2"));
    assert!(!res.raw_body.contains("sr_b1"));

    // Count-only list.
    let res = harness
        .get_json("/api/sync-review?count_only=1")
        .await
        .unwrap();
    assert_eq!(res.body["data"]["total"], 3);
    assert_eq!(
        res.body["data"]["items"].as_array().map(Vec::len),
        Some(0),
        "count_only must not return items: {}",
        res.raw_body
    );

    // Detail view.
    let res = harness.get_json("/api/sync-review/sr_a1").await.unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "detail: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["item"]["source_title"], "Source Alpha");
    assert_eq!(res.body["data"]["item"]["pair_id"], "sp_a");

    // Update proposed fields and verify persistence in the review doc.
    let res = harness
        .put_json(
            "/api/sync-review/sr_a1",
            json!({"proposed_title": "Polished"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "update: {}",
        res.raw_body
    );
    {
        let doc = load_sync_review(&sync_review_file()).unwrap();
        let item = doc.items.iter().find(|i| i.id == "sr_a1").unwrap();
        assert_eq!(item.proposed_title, "Polished");
        assert_eq!(
            item.relayed_packet.entity.core_fields.get("post_title"),
            Some(&json!("Polished")),
            "update must patch the relay packet's core fields too"
        );
    }
    let res = harness.get_json("/api/sync-review/sr_a1").await.unwrap();
    assert_eq!(res.body["data"]["item"]["proposed_title"], "Polished");

    // Reject with a reason and verify persistence.
    let res = harness
        .post_json(
            "/api/sync-review/sr_a2/reject",
            json!({"reason": "bad tone"}),
        )
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "reject: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["status"], "rejected");
    assert_eq!(res.body["data"]["reason"], "bad tone");
    {
        let doc = load_sync_review(&sync_review_file()).unwrap();
        let item = doc.items.iter().find(|i| i.id == "sr_a2").unwrap();
        assert_eq!(item.status, SyncReviewStatus::Rejected);
        assert_eq!(item.error_message.as_deref(), Some("bad tone"));
    }

    // Rejected item drops out of the pending list.
    let res = harness.get_json("/api/sync-review").await.unwrap();
    assert_eq!(res.body["data"]["total"], 2);
    assert!(
        !res.raw_body.contains("sr_a2"),
        "rejected item must leave pending list"
    );

    // Rejecting an already-rejected item is an INVALID_STATUS error.
    let res = harness
        .post_json("/api/sync-review/sr_a2/reject", json!({"reason": "again"}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 400"),
        "re-reject: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "INVALID_STATUS");
}

#[tokio::test]
async fn sync_review_detail_not_found() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();

    let res = harness
        .get_json("/api/sync-review/sr_unknown")
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 404"),
        "got: {}",
        res.raw_body
    );
    assert_eq!(res.body["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn cli07_corrupt_review_storage_returns_http_failure_and_preserves_bytes() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let path = sync_review_file();
    let bytes = b"{invalid review data";
    std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    for endpoint in ["/api/sync-review", "/api/sync-review/unknown"] {
        let res = harness.get_json(endpoint).await.unwrap();
        assert!(
            res.status_line.starts_with("HTTP/1.1 500"),
            "corrupt review must not become an empty success: {res:?}"
        );
        assert_eq!(res.body["success"], false);
        assert_eq!(res.body["error"]["code"], "SYNC_REVIEW_READ_FAILED");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn cli07_reject_state_write_failure_returns_http_error_and_keeps_pending_review() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    seed_review_items(&[pending_item(
        "sr_storage",
        "sp_a",
        "uuid-storage",
        "Keep review",
    )]);
    let original = std::fs::read(sync_review_file()).unwrap();
    let path = harness.data_dir.join("state-directory");
    std::fs::create_dir(&path).unwrap();
    let _state_path = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        path.to_string_lossy().into_owned(),
    );
    let response = harness
        .post_json("/api/sync-review/sr_storage/reject", json!({}))
        .await;
    assert!(
        response.is_ok(),
        "local persistence failure needs a readable HTTP response: {response:?}"
    );
    let response = response.unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 500"),
        "{response:?}"
    );
    assert_eq!(
        response.body["error"]["code"],
        "SYNC_REVIEW_STATE_SAVE_FAILED"
    );
    assert_eq!(std::fs::read(sync_review_file()).unwrap(), original);

    let path = harness.data_dir.join("state-write.json");
    let _write_path = crate::db::TestEnvVarGuard::set(
        "WPTSALL_SYNC_STATE_FILE",
        path.to_string_lossy().into_owned(),
    );
    std::fs::write(
        &path,
        serde_json::to_vec(&crate::sync_engine::SyncStateDoc::default()).unwrap(),
    )
    .unwrap();
    let state_before = std::fs::read(&path).unwrap();
    // The writer no longer uses a fixed .tmp.json. Block its real lock-open
    // boundary instead, leaving the owned document valid and readable.
    std::fs::create_dir(path.with_file_name(".state-write.json.lock")).unwrap();
    let response = harness
        .post_json("/api/sync-review/sr_storage/reject", json!({}))
        .await
        .unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 500"),
        "{response:?}"
    );
    assert_eq!(
        response.body["error"]["code"],
        "SYNC_REVIEW_STATE_SAVE_FAILED"
    );
    assert_eq!(std::fs::read(sync_review_file()).unwrap(), original);
    assert_eq!(std::fs::read(&path).unwrap(), state_before);
}

#[tokio::test]
async fn sync_review_approve_pushes_to_target() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |_| vec![]);
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);
    seed_pair_with_peers("sp_a", &source, &target);
    seed_review_items(&[pending_item("sr_push", "sp_a", "uuid-push", "Source Push")]);

    let edited = harness
        .put_json(
            "/api/sync-review/sr_push",
            json!({
                "proposed_title": "Edited title", "proposed_content": "<p>Edited content</p>",
                "proposed_excerpt": "Edited excerpt",
            }),
        )
        .await
        .unwrap();
    assert!(edited.status_line.starts_with("HTTP/1.1 200"));

    let res = harness
        .post_json("/api/sync-review/sr_push/approve", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 200"),
        "approve: {}",
        res.raw_body
    );
    assert_eq!(res.body["data"]["status"], "pushed");

    // The shared model authenticates every request and binds every response
    // to it, including prepare, exact receipt and original confirmation.
    let packets = target.received_packets();
    assert_eq!(packets.len(), 1);
    let packet = &packets[0];
    assert_eq!(packet["entity"]["guid"], "uuid-push");
    assert_eq!(
        packet["entity"]["core_fields"]["post_title"],
        "Edited title"
    );
    assert_eq!(
        packet["entity"]["core_fields"]["post_content"],
        "<p>Edited content</p>"
    );
    assert_eq!(
        packet["entity"]["core_fields"]["post_excerpt"],
        "Edited excerpt"
    );

    // The sync state records the known entity with the target post id.
    let state_doc = load_sync_state(&sync_state_file()).unwrap();
    let pair_state = state_doc
        .pairs
        .get("sp_a")
        .expect("sync state must have an entry for the pair");
    let known = pair_state
        .known
        .get("uuid-push")
        .expect("pushed entity must be in the known map");
    assert_eq!(known.target_post_id, Some(101));
    assert_eq!(known.post_type, "post");
    assert!(!known.review_rejected);

    // The review item is now Pushed.
    let doc = load_sync_review(&sync_review_file()).unwrap();
    let item = doc.items.iter().find(|i| i.id == "sr_push").unwrap();
    assert_eq!(item.status, SyncReviewStatus::Pushed);
    assert!(item.error_message.is_none());
}

#[tokio::test]
async fn sync_review_approve_target_rejected_records_error() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn("22222222-2222-2222-2222-222222222222", |_| vec![]);
    let target = MockSite::spawn("33333333-3333-3333-3333-333333333333", |_| vec![]);
    target.inject_push_faults(1, 403);
    seed_pair_with_peers("sp_a", &source, &target);
    seed_review_items(&[pending_item("sr_rej", "sp_a", "uuid-rej", "Source Reject")]);

    let res = harness
        .post_json("/api/sync-review/sr_rej/approve", json!({}))
        .await
        .unwrap();
    assert!(
        res.status_line.starts_with("HTTP/1.1 4"),
        "target rejection must surface a 4xx, got: {}",
        res.raw_body
    );
    assert_eq!(
        res.body["error"]["code"], "PUSH_REJECTED",
        "got: {}",
        res.raw_body
    );
    assert!(target.received_packets().is_empty(), "a refusal is never a committed write");

    // The review item stays pending and records the error for the reviewer.
    let doc = load_sync_review(&sync_review_file()).unwrap();
    let item = doc.items.iter().find(|i| i.id == "sr_rej").unwrap();
    assert_eq!(item.status, SyncReviewStatus::PendingReview);
    let error = item.error_message.as_deref().unwrap_or_default();
    assert!(
        error.contains("target rejected"),
        "unexpected error_message: {}",
        error
    );
}
