// catalog: WEBUI-MOD-sync-engine-discoverer-rs
// catalog: WEBUI-MOD-sync-engine-state-rs
// catalog: WEBUI-MOD-web-ui-routes-sync-review-rs
// oracle: L2
use super::*;
use crate::sync_engine::save_peer_credentials;
use crate::web_ui::test_support::WebUiTestHarness;

#[tokio::test]
async fn cli07_rejected_revision_does_not_requeue_when_an_earlier_failure_rewinds_the_scan() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| {
        vec![
            sample_source_packet(base, "uuid-low", 1, "First"),
            sample_source_packet(base, "uuid-rejected", 2, "Review me"),
        ]
    });
    source
        .records
        .lock()
        .unwrap()
        .media_faults
        .push("uuid-low".into());
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let creds = credentials_doc(&source, &target);
    save_peer_credentials(&crate::config::sync_peer_credentials_file(), &creds).unwrap();
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.review_before_push = true;
    let pairs_path = crate::config::sync_pairs_file();
    let state_path = crate::config::sync_state_file();
    let review_path = crate::sync_engine::sync_review_file();
    persist_pair(&pairs_path, &pair);
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log = harness.log_file.to_string_lossy().into_owned();
    let first = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!((first.error_count, first.pending_review_count), (1, 1));
    assert_eq!(
        load_sync_state(&state_path).unwrap().pairs[&pair.id].last_seen_source_id,
        0
    );
    let doc = load_sync_review(&review_path).unwrap();
    let row = doc
        .items
        .iter()
        .find(|item| item.canonical_uuid == "uuid-rejected")
        .unwrap();
    let id = row.id.clone();
    let rejected_fp = row.source_fingerprint.clone();
    let response = harness
        .post_json(
            &format!("/api/sync-review/{id}/reject"),
            json!({"reason":"Not this version"}),
        )
        .await
        .unwrap();
    assert!(
        response.status_line.starts_with("HTTP/1.1 200"),
        "{response:?}"
    );
    assert_eq!(response.body["data"]["status"], "rejected");
    let rejected = load_sync_review(&review_path)
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.id == id)
        .unwrap();
    let rejected_snapshot = serde_json::to_value(&rejected).unwrap();
    source.records.lock().unwrap().media_faults.clear();
    let chunks_before = target.received_chunks();
    let next = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(
        next.pending_review_count, 1,
        "only the previously failed lower item may enter review"
    );
    assert_eq!(next.error_count, 0);
    assert_eq!(
        target.received_chunks(),
        chunks_before + 1,
        "a rejected revision must not upload media again"
    );
    let doc = load_sync_review(&review_path).unwrap();
    assert_eq!(
        doc.items
            .iter()
            .filter(|item| item.canonical_uuid == "uuid-rejected")
            .count(),
        1
    );
    assert_eq!(
        doc.items.iter().find(|item| item.id == id).unwrap().status,
        SyncReviewStatus::Rejected
    );
    assert_eq!(
        serde_json::to_value(doc.items.iter().find(|item| item.id == id).unwrap()).unwrap(),
        rejected_snapshot
    );
    let state = load_sync_state(&state_path).unwrap();
    let remembered = state.pairs[&pair.id]
        .known
        .get("uuid-rejected")
        .expect("rejection must remember its source revision");
    assert_eq!(remembered.source_fingerprint, rejected_fp);
    assert_eq!(remembered.target_post_id, None);
    assert_eq!(
        remembered.last_shipped_at, 0,
        "rejection is not a successful push"
    );
    assert!(remembered.review_rejected);
    assert_eq!(target.received_packets().len(), 0);

    source.mutate_post("uuid-rejected", "A new revision");
    let changed = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(
        changed.pending_review_count, 1,
        "a changed source revision must still be reviewable"
    );
    let doc = load_sync_review(&review_path).unwrap();
    let pending = doc
        .items
        .iter()
        .find(|item| {
            item.canonical_uuid == "uuid-rejected" && item.status == SyncReviewStatus::PendingReview
        })
        .unwrap();
    assert_eq!(pending.source_title, "A new revision");
    assert_ne!(pending.source_fingerprint, rejected_fp);
    assert_eq!(target.received_packets().len(), 0);
    let changed_id = pending.id.clone();
    let chunks = target.received_chunks();
    let again = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(
        (again.pending_review_count, target.received_chunks()),
        (0, chunks),
        "pending new revision must not repeat media transfer through reconciliation"
    );
    let response = harness
        .post_json(&format!("/api/sync-review/{changed_id}/reject"), json!({}))
        .await
        .unwrap();
    assert!(response.status_line.starts_with("HTTP/1.1 200"));
    let review = load_sync_review(&review_path).unwrap();
    let low_id = review
        .items
        .iter()
        .find(|item| {
            item.canonical_uuid == "uuid-low" && item.status == SyncReviewStatus::PendingReview
        })
        .unwrap()
        .id
        .clone();
    let response = harness
        .post_json(&format!("/api/sync-review/{low_id}/reject"), json!({}))
        .await
        .unwrap();
    assert!(response.status_line.starts_with("HTTP/1.1 200"));
    source.trash_post("uuid-low");
    crate::sync_engine::update_pair_state(&state_path, &pair.id, |state| {
        state.last_seen_source_id = 0
    })
    .unwrap();
    let trashed = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(
        trashed.deleted_count, 0,
        "never-shipped rejected trash must not delete target content"
    );
    assert!(target.received_packets().is_empty());
    assert!(!load_sync_state(&state_path).unwrap().pairs[&pair.id]
        .known
        .contains_key("uuid-low"));
    source.delete_post("uuid-rejected");
    let deleted = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(
        deleted.deleted_count, 0,
        "never-shipped rejection must not send a target delete"
    );
    assert!(target.received_packets().is_empty());
    assert!(!load_sync_state(&state_path).unwrap().pairs[&pair.id]
        .known
        .contains_key("uuid-rejected"));
}

#[tokio::test]
async fn cli07_reject_preserves_earlier_target_metadata_and_later_deletion_still_ships() {
    let harness = WebUiTestHarness::new("", None).await.unwrap();
    let source = MockSite::spawn(SOURCE_UUID, |base| {
        vec![sample_source_packet(
            base,
            "uuid-old-target",
            1,
            "New source revision",
        )]
    });
    let target = MockSite::spawn(TARGET_UUID, |_| vec![]);
    let creds = credentials_doc(&source, &target);
    let mut pair = make_pair(&source.base_url, &target.base_url);
    pair.review_before_push = true;
    persist_pair(&crate::config::sync_pairs_file(), &pair);
    crate::sync_engine::update_pair_state(&crate::config::sync_state_file(), &pair.id, |state| {
        state.known.insert(
            "uuid-old-target".into(),
            crate::sync_engine::KnownEntity {
                canonical_uuid: "uuid-old-target".into(),
                source_fingerprint: "old-fingerprint".into(),
                vector_clock: 1,
                post_type: "post".into(),
                post_status: "publish".into(),
                target_post_id: Some(777),
                last_shipped_at: 123,
                review_rejected: false,
            },
        );
    })
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let log = harness.log_file.to_string_lossy().into_owned();
    let first = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(
        first.pending_review_count, 1,
        "digest and reconcile must not both park the same revision"
    );
    let review = load_sync_review(&crate::sync_engine::sync_review_file()).unwrap();
    let row = review
        .items
        .iter()
        .find(|item| item.canonical_uuid == "uuid-old-target")
        .unwrap();
    let response = harness
        .post_json(&format!("/api/sync-review/{}/reject", row.id), json!({}))
        .await
        .unwrap();
    assert!(response.status_line.starts_with("HTTP/1.1 200"));
    let state = load_sync_state(&crate::config::sync_state_file()).unwrap();
    let known = &state.pairs[&pair.id].known["uuid-old-target"];
    assert_eq!(
        (known.target_post_id, known.last_shipped_at),
        (Some(777), 123)
    );
    assert!(known.review_rejected);
    assert!(target.received_packets().is_empty());
    source.delete_post("uuid-old-target");
    let next = super::super::sync_pair_run(&client, &pair.id, &creds, None, &log)
        .await
        .unwrap();
    assert_eq!(next.deleted_count, 1);
    let packets = target.received_packets();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0]["action"], "delete");
    assert_eq!(packets[0]["entity"]["guid"], "uuid-old-target");
}
