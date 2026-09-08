use super::*;
use crate::db::open_db;
use crate::types::TranslationCallbackPayload;
use std::collections::HashMap;

fn make_db() -> Connection {
    open_db(":memory:").expect("in-memory db")
}

fn make_payload(relation_id: u64, object_id: u64) -> TranslationCallbackPayload {
    TranslationCallbackPayload {
        schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
        attempt_id: format!("att-db-{}-{}", relation_id, object_id),
        object_snapshot_hash: format!("hash-db-{}-{}", relation_id, object_id),
        source_revision: String::new(),
        policy_version: String::new(),
        field_results: Vec::new(),
        relation_id,
        business_line: "post_content".to_string(),
        object_type: "post_type".to_string(),
        subtype: "post".to_string(),
        object_id,
        translated_fields: HashMap::from([(
            "post_title".to_string(),
            "translated title".to_string(),
        )]),
        translated_meta: HashMap::new(),
        media_mappings: Vec::new(),
        media_field_sources: HashMap::new(),
        client_task_id: format!("discovery_{}_{}", relation_id, object_id),
        outbox_id: None,
        worker_id: "worker1".to_string(),
        source_lang: "en".to_string(),
        target_lang: "zh".to_string(),
        execution_time_ms: 100,
    }
}

fn make_entry(api_base_url: &str, relation_id: i64, object_id: i64) -> PendingCallbackEntry {
    // Include api_base_url in the key to avoid PRIMARY KEY conflicts across domains.
    // In production, the worker_id suffix achieves this since workers are domain-specific.
    let sanitized_domain = api_base_url
        .replace("://", "_")
        .replace('/', "_")
        .replace('.', "_");
    PendingCallbackEntry {
        api_base_url: api_base_url.to_string(),
        idempotency_key: format!(
            "discovery-{}-{}-worker1-{}",
            relation_id, object_id, sanitized_domain
        ),
        payload: make_payload(relation_id as u64, object_id as u64),
        route_secret: Some("secret123".to_string()),
        created_at: 1700000000,
        retry_count: 0,
        last_retry_at: 0,
        relation_id,
        object_id,
        object_type: "post_type".to_string(),
    }
}

#[test]
fn test_add_and_find() {
    let conn = make_db();
    let entry = make_entry("https://wp.example.com", 1, 100);
    add_pending_callback(&conn, &entry).unwrap();

    let found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100);
    assert!(found.is_some());
    let found = found.unwrap();
    assert_eq!(found.api_base_url, "https://wp.example.com");
    assert_eq!(found.relation_id, 1);
    assert_eq!(found.object_id, 100);
    assert_eq!(found.payload.object_id, 100);
    assert_eq!(
        found.payload.translated_fields.get("post_title").unwrap(),
        "translated title"
    );
}

#[test]
fn test_idempotent_add() {
    let conn = make_db();
    let entry = make_entry("https://wp.example.com", 1, 100);
    add_pending_callback(&conn, &entry).unwrap();
    // Second add with same idempotency_key should replace, not duplicate
    add_pending_callback(&conn, &entry).unwrap();

    let all = list_all_pending_callbacks(&conn);
    assert_eq!(all.len(), 1);
}

#[test]
fn test_find_not_found() {
    let conn = make_db();
    let found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 999);
    assert!(found.is_none());
}

#[test]
fn test_remove() {
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 100)).unwrap();
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 200)).unwrap();

    let removed =
        remove_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    assert!(removed);

    assert!(find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).is_none());
    assert!(find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 200).is_some());

    // Removing again returns false
    let not_removed =
        remove_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    assert!(!not_removed);
}

#[test]
fn test_increment_retry() {
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 100)).unwrap();

    increment_retry_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    increment_retry_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();

    let found =
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    assert_eq!(found.retry_count, 2);
    assert!(found.last_retry_at > 0);
}

#[test]
fn test_list_all() {
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://a.com", 1, 10)).unwrap();
    add_pending_callback(&conn, &make_entry("https://a.com", 1, 20)).unwrap();
    add_pending_callback(&conn, &make_entry("https://b.com", 2, 30)).unwrap();

    let all = list_all_pending_callbacks(&conn);
    assert_eq!(all.len(), 3);
}

#[test]
fn test_different_domains_isolated() {
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://a.com", 1, 100)).unwrap();
    add_pending_callback(&conn, &make_entry("https://b.com", 1, 100)).unwrap();

    // Same relation_id + object_id but different domain — both should exist
    let a = find_pending_callback(&conn, "https://a.com", 1, "post_type", 100);
    let b = find_pending_callback(&conn, "https://b.com", 1, "post_type", 100);
    assert!(a.is_some());
    assert!(b.is_some());

    // Removing one doesn't affect the other
    remove_pending_callback(&conn, "https://a.com", 1, "post_type", 100).unwrap();
    assert!(find_pending_callback(&conn, "https://a.com", 1, "post_type", 100).is_none());
    assert!(find_pending_callback(&conn, "https://b.com", 1, "post_type", 100).is_some());
}

/// route_secret = None should round-trip cleanly (stored as SQL NULL).
#[test]
fn test_route_secret_none() {
    let conn = make_db();
    let mut entry = make_entry("https://wp.example.com", 1, 100);
    entry.route_secret = None;
    add_pending_callback(&conn, &entry).unwrap();

    let found =
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    assert!(
        found.route_secret.is_none(),
        "NULL route_secret should round-trip"
    );
}

/// A row with corrupted payload_json should be silently skipped by both
/// find_pending_callback and list_all_pending_callbacks.
#[test]
fn test_corrupted_payload_json_skipped() {
    let conn = make_db();

    // Insert a valid row first.
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 200)).unwrap();

    // Manually insert a row with malformed JSON.
    conn.execute(
            "INSERT INTO pending_callbacks
             (api_base_url, idempotency_key, payload_json, created_at, relation_id, object_id, object_type)
             VALUES ('https://wp.example.com', 'bad-key', 'NOT JSON', 0, 1, 100, 'post_type')",
            [],
        )
        .unwrap();

    // find should return None for the corrupted row.
    let found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100);
    assert!(
        found.is_none(),
        "corrupted JSON should not be returned by find"
    );

    // list_all should only return the valid row.
    let all = list_all_pending_callbacks(&conn);
    assert_eq!(
        all.len(),
        1,
        "corrupted row should be filtered out of list_all"
    );
    assert_eq!(all[0].object_id, 200);
}

#[test]
fn test_same_object_id_different_object_type_are_isolated() {
    let conn = make_db();

    let mut post = make_entry("https://wp.example.com", 1, 42);
    post.object_type = "post_type".to_string();
    post.payload.object_type = "post_type".to_string();
    add_pending_callback(&conn, &post).unwrap();

    let mut term = make_entry("https://wp.example.com", 1, 42);
    term.object_type = "taxonomy".to_string();
    term.payload.object_type = "taxonomy".to_string();
    term.idempotency_key = "discovery-1-42-worker1-taxonomy".to_string();
    add_pending_callback(&conn, &term).unwrap();

    let post_found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 42);
    let term_found = find_pending_callback(&conn, "https://wp.example.com", 1, "taxonomy", 42);
    assert!(post_found.is_some());
    assert!(term_found.is_some());
    assert_eq!(post_found.unwrap().object_type, "post_type");
    assert_eq!(term_found.unwrap().object_type, "taxonomy");
}

/// cleanup_stale_pending_callbacks removes rows that exceed max_retries OR
/// are older than max_age_secs; fresh rows with low retry_count survive.
#[test]
fn test_cleanup_stale_callbacks() {
    let conn = make_db();

    // Entry 1: retry_count at the limit — must be removed.
    let mut exhausted = make_entry("https://wp.example.com", 1, 10);
    exhausted.retry_count = 10;
    add_pending_callback(&conn, &exhausted).unwrap();

    // Entry 2: created_at far in the past (73 h ago) — must be removed.
    let mut stale = make_entry("https://wp.example.com", 1, 20);
    stale.created_at = unix_ts().saturating_sub(73 * 3600);
    add_pending_callback(&conn, &stale).unwrap();

    // Entry 3: fresh entry with low retry_count — must survive.
    let mut fresh = make_entry("https://wp.example.com", 1, 30);
    fresh.created_at = unix_ts();
    fresh.retry_count = 3;
    add_pending_callback(&conn, &fresh).unwrap();

    let removed = cleanup_stale_pending_callbacks(
        &conn,
        /*max_retries=*/ 10,
        /*max_age_secs=*/ 72 * 3600,
    )
    .unwrap();
    assert_eq!(
        removed, 2,
        "exhausted + stale entries should both be removed"
    );

    let remaining = list_all_pending_callbacks(&conn);
    assert_eq!(remaining.len(), 1, "only the fresh entry should remain");
    assert_eq!(remaining[0].object_id, 30);
}

/// Backlog isolation semantics (plan §7 TEST-NETWORK-RESILIENCE-001 /
/// failure-modes FM-HTTP-413; gap doc NET-02 "部分成立"): find does NOT
/// cap-filter — a row that grew past max_retries via increment_retry keeps
/// being handed out until cleanup_stale_pending_callbacks runs (open() runs
/// it), and cleanup then removes it. Pinned as the designed bounded-isolation
/// behavior, so any drift (unbounded handout OR premature invisible drop)
/// surfaces here.
#[test]
fn find_hands_out_over_cap_rows_until_cleanup_pin() {
    let conn = make_db();

    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 40)).unwrap();
    // Grow the row past the cap the same way the worker loop does.
    for _ in 0..10 {
        increment_retry_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 40)
            .unwrap();
    }
    let over_cap = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 40)
        .expect("over-cap row is still handed out until cleanup runs");
    assert_eq!(over_cap.retry_count, 10);

    // Cleanup (called on open()) removes the exhausted row.
    let removed = cleanup_stale_pending_callbacks(
        &conn,
        /*max_retries=*/ 10,
        /*max_age_secs=*/ 72 * 3600,
    )
    .unwrap();
    assert_eq!(removed, 1, "the exhausted row must be cleaned up");
    assert!(
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 40).is_none(),
        "after cleanup the backlog entry is gone"
    );
}

/// list_all_pending_callbacks should return rows ordered by created_at ASC.
#[test]
fn test_list_all_ordered_by_created_at() {
    let conn = make_db();

    // Insert entries with explicit, out-of-order created_at values.
    let mut e1 = make_entry("https://wp.example.com", 1, 10);
    e1.created_at = 3000;
    let mut e2 = make_entry("https://wp.example.com", 1, 20);
    e2.created_at = 1000;
    let mut e3 = make_entry("https://wp.example.com", 1, 30);
    e3.created_at = 2000;

    add_pending_callback(&conn, &e1).unwrap();
    add_pending_callback(&conn, &e2).unwrap();
    add_pending_callback(&conn, &e3).unwrap();

    let all = list_all_pending_callbacks(&conn);
    assert_eq!(all.len(), 3);
    // Should be ordered: 1000, 2000, 3000
    assert_eq!(
        all[0].created_at, 1000,
        "first entry should have smallest created_at"
    );
    assert_eq!(all[1].created_at, 2000);
    assert_eq!(
        all[2].created_at, 3000,
        "last entry should have largest created_at"
    );
}
