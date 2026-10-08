use super::*;

fn make_db() -> rusqlite::Connection {
    crate::db::open_db(":memory:").expect("in-memory db")
}

fn make_record(domain: &str, status: &str) -> InsertTranslationRecord {
    InsertTranslationRecord {
        domain: domain.to_string(),
        relation_id: Some(1),
        object_id: Some(42),
        object_type: Some("post".to_string()),
        business_line: Some("post_content".to_string()),
        source_lang: "en".to_string(),
        target_lang: "zh-CN".to_string(),
        status: status.to_string(),
        execution_ms: Some(123),
        worker_id: Some("worker-1".to_string()),
        idempotency_key: Some(format!("key-{}-{}", domain, status)),
        fields_count: 3,
        error_message: None,
        component_ids: Vec::new(),
        media_mappings_count: 0,
        failed_fields_count: 0,
        primary_failure_reason: None,
    }
}

#[test]
fn test_insert_and_query_basic() {
    let conn = make_db();
    let rec = make_record("https://example.com", "success");
    let id = insert_translation_record(&conn, &rec).unwrap();
    assert!(id > 0);

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.total, 1);
    assert_eq!(result.records.len(), 1);
    let r = &result.records[0];
    assert_eq!(r.domain, "https://example.com");
    assert_eq!(r.status, "success");
    assert_eq!(r.source_lang, "en");
    assert_eq!(r.target_lang, "zh-CN");
    assert_eq!(r.fields_count, 3);
    assert_eq!(r.relation_id, Some(1));
    assert_eq!(r.object_id, Some(42));
    assert!(r.component_ids.is_empty());
    assert_eq!(r.media_mappings_count, 0);
    assert_eq!(r.failed_fields_count, 0);
    assert_eq!(r.primary_failure_reason, None);
}

#[test]
fn test_insert_and_query_preserves_execution_summary() {
    let conn = make_db();
    let mut rec = make_record("https://example.com", "failed");
    rec.error_message = Some("callback failed".to_string());
    rec.component_ids = vec!["comp-text".to_string(), "comp-image".to_string()];
    rec.media_mappings_count = 2;
    rec.failed_fields_count = 1;
    rec.primary_failure_reason = Some("unsupported_content_format".to_string());

    insert_translation_record(&conn, &rec).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.total, 1);
    let record = &result.records[0];
    assert_eq!(
        record.component_ids,
        vec!["comp-text".to_string(), "comp-image".to_string()]
    );
    assert_eq!(record.media_mappings_count, 2);
    assert_eq!(record.failed_fields_count, 1);
    assert_eq!(
        record.primary_failure_reason.as_deref(),
        Some("unsupported_content_format")
    );
    assert_eq!(record.error_message.as_deref(), Some("callback failed"));
}

#[test]
fn test_materialized_success_record_ignores_noop_success() {
    let conn = make_db();
    let mut rec = make_record("https://example.com", "success");
    rec.fields_count = 0;
    rec.component_ids = Vec::new();
    rec.media_mappings_count = 0;
    insert_translation_record(&conn, &rec).unwrap();

    assert!(!has_materialized_success_record(
        &conn,
        "https://example.com",
        1,
        42,
        "post"
    ));
}

#[test]
fn test_materialized_success_record_accepts_real_translation_trace() {
    let conn = make_db();
    let mut rec = make_record("https://example.com", "success");
    rec.fields_count = 0;
    rec.component_ids = vec!["official-mymemory-v1".to_string()];
    insert_translation_record(&conn, &rec).unwrap();

    assert!(has_materialized_success_record(
        &conn,
        "https://example.com",
        1,
        42,
        "post"
    ));
}

#[test]
fn test_query_empty_returns_empty() {
    let conn = make_db();
    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.total, 0);
    assert!(result.records.is_empty());
}

#[test]
fn test_query_pagination() {
    let conn = make_db();
    // Insert 5 records
    for i in 0..5 {
        let rec = make_record(&format!("https://domain{}.com", i), "success");
        insert_translation_record(&conn, &rec).unwrap();
    }

    // Page 1 with limit 2
    let page1 = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 2,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();
    assert_eq!(page1.total, 5);
    assert_eq!(page1.records.len(), 2);

    // Page 2 with limit 2
    let page2 = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 2,
            limit: 2,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();
    assert_eq!(page2.total, 5);
    assert_eq!(page2.records.len(), 2);

    // Page 3 with limit 2 → 1 remaining
    let page3 = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 3,
            limit: 2,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();
    assert_eq!(page3.total, 5);
    assert_eq!(page3.records.len(), 1);
}

#[test]
fn test_query_filter_by_domain() {
    let conn = make_db();
    insert_translation_record(&conn, &make_record("https://domain1.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://domain1.com", "failed")).unwrap();
    insert_translation_record(&conn, &make_record("https://domain2.com", "success")).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: Some("https://domain1.com".to_string()),
            status: None,
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.total, 2);
    for r in &result.records {
        assert_eq!(r.domain, "https://domain1.com");
    }
}

#[test]
fn test_query_filter_by_status() {
    let conn = make_db();
    insert_translation_record(&conn, &make_record("https://example.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://example.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://example.com", "failed")).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: Some("success".to_string()),
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.total, 2);
    for r in &result.records {
        assert_eq!(r.status, "success");
    }
}

#[test]
fn test_query_filter_by_search() {
    let conn = make_db();
    insert_translation_record(&conn, &make_record("https://alpha-site.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://beta-site.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://gamma-site.com", "success")).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: Some("alpha".to_string()),
        },
    )
    .unwrap();

    assert_eq!(result.total, 1);
    assert_eq!(result.records[0].domain, "https://alpha-site.com");
}

#[test]
fn test_update_translation_status() {
    let conn = make_db();
    let rec = make_record("https://example.com", "pending_callback");
    let id = insert_translation_record(&conn, &rec).unwrap();

    update_translation_status(&conn, id, "success", None).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.records[0].status, "success");
}

#[test]
fn test_mark_callback_sent() {
    let conn = make_db();
    let rec = make_record("https://example.com", "pending_callback");
    let id = insert_translation_record(&conn, &rec).unwrap();

    mark_callback_sent(&conn, id).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    let r = &result.records[0];
    assert_eq!(r.status, "success");
    assert!(r.callback_sent_at.is_some());
    assert!(r.callback_sent_at.unwrap() > 0);
}

#[test]
fn test_increment_callback_retries() {
    let conn = make_db();
    let rec = make_record("https://example.com", "pending_callback");
    let id = insert_translation_record(&conn, &rec).unwrap();

    increment_callback_retries(&conn, id).unwrap();
    increment_callback_retries(&conn, id).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.records[0].callback_retries, 2);
}

#[test]
fn test_query_ordered_by_created_desc() {
    let conn = make_db();
    // Insert 3 records — they'll get sequential IDs and timestamps
    // We sleep briefly between inserts to ensure different created_at values,
    // but since created_at uses seconds precision, we rely on id DESC ordering
    // as the secondary sort key.
    insert_translation_record(&conn, &make_record("https://first.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://second.com", "success")).unwrap();
    insert_translation_record(&conn, &make_record("https://third.com", "success")).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    assert_eq!(result.records.len(), 3);
    // The records should be ordered newest first (by id DESC as secondary)
    // IDs: first=1, second=2, third=3 → order: third, second, first
    assert_eq!(result.records[0].domain, "https://third.com");
    assert_eq!(result.records[1].domain, "https://second.com");
    assert_eq!(result.records[2].domain, "https://first.com");
}

#[test]
fn test_batch_retry_only_failed() {
    let conn = make_db();
    let id1 = insert_translation_record(&conn, &make_record("https://a.com", "failed")).unwrap();
    let id2 = insert_translation_record(&conn, &make_record("https://b.com", "success")).unwrap();
    let id3 = insert_translation_record(&conn, &make_record("https://c.com", "failed")).unwrap();

    let queued = batch_retry_translations(&conn, &[id1, id2, id3]).unwrap();
    assert_eq!(queued, 2); // only failed records are queued
}

#[test]
fn test_batch_retry_empty_ids() {
    let conn = make_db();
    let queued = batch_retry_translations(&conn, &[]).unwrap();
    assert_eq!(queued, 0);
}

#[test]
fn test_batch_delete() {
    let conn = make_db();
    let id1 = insert_translation_record(&conn, &make_record("https://a.com", "success")).unwrap();
    let id2 = insert_translation_record(&conn, &make_record("https://b.com", "success")).unwrap();
    let id3 = insert_translation_record(&conn, &make_record("https://c.com", "success")).unwrap();

    let deleted = batch_delete_translations(&conn, &[id1, id3]).unwrap();
    assert_eq!(deleted, 2);

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.records[0].id, id2);
}

#[test]
fn test_batch_delete_nonexistent_ids() {
    let conn = make_db();
    insert_translation_record(&conn, &make_record("https://a.com", "success")).unwrap();
    let deleted = batch_delete_translations(&conn, &[999, 1000]).unwrap();
    assert_eq!(deleted, 0);
}

#[test]
fn test_query_default_page_limit() {
    let conn = make_db();
    // Insert a few records
    for i in 0..3 {
        insert_translation_record(
            &conn,
            &make_record(&format!("https://site{}.com", i), "success"),
        )
        .unwrap();
    }

    // page=0 and limit=0 should use defaults (page=1, limit=20)
    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 0,
            limit: 0,
            domain: None,
            status: None,
            search: None,
        },
    )
    .unwrap();

    // page should be clamped to 1
    assert_eq!(result.page, 1);
    // limit should default to 20
    assert_eq!(result.limit, 20);
    assert_eq!(result.total, 3);
    assert_eq!(result.records.len(), 3);
}

#[test]
fn test_schema_upgrade_preserves_old_translation_records() {
    let conn = rusqlite::Connection::open(":memory:").expect("in-memory db");
    conn.execute_batch(
        "CREATE TABLE translation_records (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at       INTEGER NOT NULL DEFAULT 0,
            domain           TEXT NOT NULL DEFAULT '',
            relation_id      INTEGER,
            object_id        INTEGER,
            object_type      TEXT,
            business_line    TEXT,
            source_lang      TEXT NOT NULL DEFAULT '',
            target_lang      TEXT NOT NULL DEFAULT '',
            status           TEXT NOT NULL DEFAULT 'pending',
            execution_ms     INTEGER,
            worker_id        TEXT,
            idempotency_key  TEXT,
            callback_sent_at INTEGER,
            callback_retries INTEGER NOT NULL DEFAULT 0,
            fields_count     INTEGER NOT NULL DEFAULT 0,
            error_message    TEXT
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO translation_records
         (created_at, domain, relation_id, object_id, object_type, business_line,
          source_lang, target_lang, status, execution_ms, worker_id, idempotency_key,
          callback_sent_at, callback_retries, fields_count, error_message)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        rusqlite::params![
            1,
            "https://legacy.example.com",
            7,
            99,
            "post_type",
            "post_content",
            "en",
            "zh-CN",
            "success",
            120,
            "worker-1",
            "legacy-key",
            0,
            0,
            2,
            Option::<String>::None,
        ],
    )
    .unwrap();

    crate::db::schema::create_tables(&conn).unwrap();

    let result = query_translation_records(
        &conn,
        &TranslationQueryParams {
            page: 1,
            limit: 20,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.total, 1);
    let record = &result.records[0];
    assert_eq!(record.domain, "https://legacy.example.com");
    assert!(record.component_ids.is_empty());
    assert_eq!(record.media_mappings_count, 0);
    assert_eq!(record.failed_fields_count, 0);
    assert_eq!(record.primary_failure_reason, None);
}
