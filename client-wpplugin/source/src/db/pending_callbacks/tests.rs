use super::*;
#[path = "../../../../../tests/modules/client-wpplugin/unit/encrypted_json_pending_callbacks.rs"]
mod encrypted_json_pending_callbacks;
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
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let entry = make_entry("https://wp.example.com", 1, 100);
    add_pending_callback(&conn, &entry).unwrap();

    let found =
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
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
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let entry = make_entry("https://wp.example.com", 1, 100);
    add_pending_callback(&conn, &entry).unwrap();
    // Second add with same idempotency_key should replace, not duplicate
    add_pending_callback(&conn, &entry).unwrap();

    let all = list_all_pending_callbacks(&conn).unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn test_find_not_found() {
    let conn = make_db();
    let found =
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 999).unwrap();
    assert!(found.is_none());
}

#[test]
fn test_remove() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 100)).unwrap();
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 200)).unwrap();

    let removed =
        remove_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    assert!(removed);

    assert!(
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100)
            .unwrap()
            .is_none()
    );
    assert!(
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 200)
            .unwrap()
            .is_some()
    );

    // Removing again returns false
    let not_removed =
        remove_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    assert!(!not_removed);
}

#[test]
fn test_increment_retry() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 100)).unwrap();

    increment_retry_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();
    increment_retry_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100).unwrap();

    let found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100)
        .unwrap()
        .unwrap();
    assert_eq!(found.retry_count, 2);
    assert!(found.last_retry_at > 0);
}

#[test]
fn test_list_all() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://a.com", 1, 10)).unwrap();
    add_pending_callback(&conn, &make_entry("https://a.com", 1, 20)).unwrap();
    add_pending_callback(&conn, &make_entry("https://b.com", 2, 30)).unwrap();

    let all = list_all_pending_callbacks(&conn).unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn test_different_domains_isolated() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    add_pending_callback(&conn, &make_entry("https://a.com", 1, 100)).unwrap();
    add_pending_callback(&conn, &make_entry("https://b.com", 1, 100)).unwrap();

    // Same relation_id + object_id but different domain — both should exist
    let a = find_pending_callback(&conn, "https://a.com", 1, "post_type", 100).unwrap();
    let b = find_pending_callback(&conn, "https://b.com", 1, "post_type", 100).unwrap();
    assert!(a.is_some());
    assert!(b.is_some());

    // Removing one doesn't affect the other
    remove_pending_callback(&conn, "https://a.com", 1, "post_type", 100).unwrap();
    assert!(
        find_pending_callback(&conn, "https://a.com", 1, "post_type", 100)
            .unwrap()
            .is_none()
    );
    assert!(
        find_pending_callback(&conn, "https://b.com", 1, "post_type", 100)
            .unwrap()
            .is_some()
    );
}

/// route_secret = None should round-trip inside the encrypted endpoint.
#[test]
fn test_route_secret_none() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let mut entry = make_entry("https://wp.example.com", 1, 100);
    entry.route_secret = None;
    add_pending_callback(&conn, &entry).unwrap();

    let found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100)
        .unwrap()
        .unwrap();
    assert!(
        found.route_secret.is_none(),
        "NULL route_secret should round-trip"
    );
}

/// Corruption must stop recovery, never become absence or a partial list.
#[test]
fn test_corrupted_payload_json_stops_recovery() {
    let _key = crate::db::owned_mock_bindings_key();
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

    // A malformed saved callback must not trigger fresh paid translation.
    let found = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 100);
    assert!(found.is_err(), "corrupted JSON must be an error");

    // A partial recovery list would conceal damaged saved work.
    let all = list_all_pending_callbacks(&conn);
    assert!(all.is_err());
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM pending_callbacks", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 2, "both original rows must remain intact");
}

#[test]
fn test_same_object_id_different_object_type_are_isolated() {
    let _key = crate::db::owned_mock_bindings_key();
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

    let post_found =
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 42).unwrap();
    let term_found =
        find_pending_callback(&conn, "https://wp.example.com", 1, "taxonomy", 42).unwrap();
    assert!(post_found.is_some());
    assert!(term_found.is_some());
    assert_eq!(post_found.unwrap().object_type, "post_type");
    assert_eq!(term_found.unwrap().object_type, "taxonomy");
}

/// Age and retry budget do not remove unconfirmed results.
#[test]
fn test_cleanup_retains_unconfirmed_callbacks() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();

    // Entry 1: retry_count at the limit — retained.
    let mut exhausted = make_entry("https://wp.example.com", 1, 10);
    exhausted.retry_count = 10;
    add_pending_callback(&conn, &exhausted).unwrap();

    // Entry 2: created_at far in the past (73 h ago) — retained.
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
        removed, 0,
        "age and retry budget do not authorize deleting unconfirmed results"
    );

    let remaining = list_all_pending_callbacks(&conn).unwrap();
    assert_eq!(remaining.len(), 3);
    assert!(remaining.iter().any(|entry| entry.object_id == 30));
}

/// Backlog isolation semantics (plan §7 TEST-NETWORK-RESILIENCE-001 /
/// failure-modes FM-HTTP-413; gap doc NET-02 "部分成立"): find does NOT
/// cap-filter: limits pause retries, not visibility or retention.
#[test]
fn find_retains_over_cap_rows_after_cleanup_pin() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();

    add_pending_callback(&conn, &make_entry("https://wp.example.com", 1, 40)).unwrap();
    // Grow the row past the cap the same way the worker loop does.
    for _ in 0..10 {
        increment_retry_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 40)
            .unwrap();
    }
    let over_cap = find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 40)
        .expect("read")
        .expect("over-cap row remains available for explicit recovery");
    assert_eq!(over_cap.retry_count, 10);

    // The former cleanup hook cannot remove an unconfirmed result.
    let removed = cleanup_stale_pending_callbacks(
        &conn,
        /*max_retries=*/ 10,
        /*max_age_secs=*/ 72 * 3600,
    )
    .unwrap();
    assert_eq!(removed, 0);
    assert!(
        find_pending_callback(&conn, "https://wp.example.com", 1, "post_type", 40)
            .unwrap()
            .is_some(),
        "after cleanup the original backlog entry is retained"
    );
}

/// list_all_pending_callbacks should return rows ordered by created_at ASC.
#[test]
fn test_list_all_ordered_by_created_at() {
    let _key = crate::db::owned_mock_bindings_key();
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

    let all = list_all_pending_callbacks(&conn).unwrap();
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

#[test]
fn remaining_retention_database_limits_do_not_authorize_pending_deletion() {
    let _key = crate::db::owned_mock_bindings_key();
    let conn = make_db();
    let mut entry = make_entry("https://owned.invalid", 8, 80);
    entry.created_at = unix_ts().saturating_sub(90 * 3600);
    entry.retry_count = 40;
    add_pending_callback(&conn, &entry).unwrap();
    assert_eq!(
        cleanup_stale_pending_callbacks(&conn, 10, 72 * 3600).unwrap(),
        0
    );
    let retained = find_pending_callback(&conn, &entry.api_base_url, 8, "post_type", 80)
        .unwrap()
        .unwrap();
    assert_eq!(retained.idempotency_key, entry.idempotency_key);
    assert_eq!(retained.retry_count, 40);
}
