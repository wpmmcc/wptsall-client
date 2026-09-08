use super::*;
use crate::db::schema::create_tables;

fn make_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    create_tables(&conn).unwrap();
    conn
}

fn sample_job_req(domain: &str) -> CreateJobRequest {
    CreateJobRequest {
        domain: domain.to_string(),
        relation_id: 1,
        business_line: "post_content".to_string(),
        triggered_by: "manual".to_string(),
    }
}

fn sample_item_req(job_id: i64, wp_object_id: i64) -> CreateItemRequest {
    CreateItemRequest {
        job_id,
        domain: "https://example.com".to_string(),
        relation_id: 1,
        business_line: "post_content".to_string(),
        object_type: "post".to_string(),
        wp_object_id,
        wp_object_subtype: "".to_string(),
        task_type: "text".to_string(),
        source_lang: "en".to_string(),
        target_lang: "zh-CN".to_string(),
        component_id: "comp-1".to_string(),
        component_ids: vec!["comp-1".to_string()],
        selected_component_id: None,
        effective_source_lang: None,
        effective_target_lang: None,
        editable_overrides: None,
        raw_path: "/tmp/raw.txt".to_string(),
        client_task_id: format!("ctask-{}", wp_object_id),
        max_retries: 3,
    }
}

#[test]
fn test_create_and_get_job_round_trip() {
    let conn = make_db();
    let req = sample_job_req("https://example.com");
    let job_id = create_job(&conn, &req).unwrap();
    assert!(job_id > 0);

    let job = get_job(&conn, job_id).expect("job must exist");
    assert_eq!(job.id, job_id);
    assert_eq!(job.domain, "https://example.com");
    assert_eq!(job.relation_id, 1);
    assert_eq!(job.business_line, "post_content");
    assert_eq!(job.status, "pending");
    assert_eq!(job.total_items, 0);
    assert_eq!(job.done_items, 0);
    assert_eq!(job.failed_items, 0);
    assert_eq!(job.triggered_by, "manual");
    assert!(job.started_at.is_none());
    assert!(job.completed_at.is_none());
    assert!(job.created_at > 0);
}

#[test]
fn test_get_job_returns_none_for_missing_id() {
    let conn = make_db();
    assert!(get_job(&conn, 9999).is_none());
}

#[test]
fn test_update_job_status_to_running_sets_started_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    update_job_status(&conn, job_id, "running").unwrap();
    let job = get_job(&conn, job_id).unwrap();
    assert_eq!(job.status, "running");
    assert!(job.started_at.is_some());
    assert!(job.started_at.unwrap() > 0);
    assert!(job.completed_at.is_none());
}

#[test]
fn test_update_job_status_running_does_not_overwrite_started_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    update_job_status(&conn, job_id, "running").unwrap();
    let first_started = get_job(&conn, job_id).unwrap().started_at.unwrap();

    update_job_status(&conn, job_id, "running").unwrap();
    let second_started = get_job(&conn, job_id).unwrap().started_at.unwrap();
    assert_eq!(first_started, second_started);
}

#[test]
fn test_update_job_status_completed_sets_completed_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    update_job_status(&conn, job_id, "running").unwrap();
    update_job_status(&conn, job_id, "completed").unwrap();
    let job = get_job(&conn, job_id).unwrap();
    assert_eq!(job.status, "completed");
    assert!(job.completed_at.is_some());
    assert!(job.completed_at.unwrap() > 0);
}

#[test]
fn test_update_job_status_failed_sets_completed_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    update_job_status(&conn, job_id, "failed").unwrap();
    let job = get_job(&conn, job_id).unwrap();
    assert_eq!(job.status, "failed");
    assert!(job.completed_at.is_some());
}

#[test]
fn test_update_job_status_partial_sets_completed_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    update_job_status(&conn, job_id, "partial").unwrap();
    let job = get_job(&conn, job_id).unwrap();
    assert_eq!(job.status, "partial");
    assert!(job.completed_at.is_some());
}

#[test]
fn test_increment_job_counters() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    increment_job_counters(&conn, job_id, 2, 1, 5).unwrap();
    let job = get_job(&conn, job_id).unwrap();
    assert_eq!(job.done_items, 2);
    assert_eq!(job.failed_items, 1);
    assert_eq!(job.total_items, 5);

    increment_job_counters(&conn, job_id, 1, 0, 1).unwrap();
    let job2 = get_job(&conn, job_id).unwrap();
    assert_eq!(job2.done_items, 3);
    assert_eq!(job2.failed_items, 1);
    assert_eq!(job2.total_items, 6);
}

#[test]
fn test_list_jobs_no_filter() {
    let conn = make_db();
    create_job(&conn, &sample_job_req("https://a.com")).unwrap();
    create_job(&conn, &sample_job_req("https://b.com")).unwrap();

    let jobs = list_jobs(&conn, None, 10, 0);
    assert_eq!(jobs.len(), 2);
}

#[test]
fn test_list_jobs_with_domain_filter() {
    let conn = make_db();
    create_job(&conn, &sample_job_req("https://a.com")).unwrap();
    create_job(&conn, &sample_job_req("https://a.com")).unwrap();
    create_job(&conn, &sample_job_req("https://b.com")).unwrap();

    let jobs = list_jobs(&conn, Some("https://a.com"), 10, 0);
    assert_eq!(jobs.len(), 2);
    for j in &jobs {
        assert_eq!(j.domain, "https://a.com");
    }
}

#[test]
fn test_list_jobs_pagination() {
    let conn = make_db();
    for _ in 0..5 {
        create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    }

    let page1 = list_jobs(&conn, None, 2, 0);
    assert_eq!(page1.len(), 2);

    let page2 = list_jobs(&conn, None, 2, 2);
    assert_eq!(page2.len(), 2);

    let page3 = list_jobs(&conn, None, 2, 4);
    assert_eq!(page3.len(), 1);
}

#[test]
fn test_create_item_returns_id() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 42)).unwrap();
    assert!(item_id > 0);
}

#[test]
fn test_create_item_dedup_returns_same_id() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let req = sample_item_req(job_id, 42);

    let id1 = create_item(&conn, &req).unwrap();
    let id2 = create_item(&conn, &req).unwrap();
    assert_eq!(id1, id2);

    let items = list_items_by_job(&conn, job_id, None);
    assert_eq!(items.len(), 1);
}

#[test]
fn test_create_item_different_task_types_are_distinct_rows() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let mut req = sample_item_req(job_id, 42);
    req.task_type = "text".to_string();
    let id_text = create_item(&conn, &req).unwrap();

    req.task_type = "image".to_string();
    let id_image = create_item(&conn, &req).unwrap();

    assert_ne!(id_text, id_image);
    assert_eq!(list_items_by_job(&conn, job_id, None).len(), 2);
}

#[test]
fn test_update_item_status_fetched_sets_fetched_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 1)).unwrap();

    update_item_status(&conn, item_id, "fetched", None).unwrap();

    let items = list_items_by_job(&conn, job_id, None);
    let item = &items[0];
    assert_eq!(item.status, "fetched");
    assert!(item.fetched_at.is_some());
    assert!(item.fetched_at.unwrap() > 0);
    assert!(item.translated_at.is_none());
    assert!(item.synced_at.is_none());
}

#[test]
fn test_update_item_status_translated_sets_translated_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 2)).unwrap();

    update_item_status(&conn, item_id, "translated", None).unwrap();

    let items = list_items_by_job(&conn, job_id, None);
    let item = &items[0];
    assert_eq!(item.status, "translated");
    assert!(item.translated_at.is_some());
    assert!(item.fetched_at.is_none());
    assert!(item.synced_at.is_none());
}

#[test]
fn test_update_item_status_done_sets_synced_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 3)).unwrap();

    update_item_status(&conn, item_id, "done", None).unwrap();

    let items = list_items_by_job(&conn, job_id, None);
    let item = &items[0];
    assert_eq!(item.status, "done");
    assert!(item.synced_at.is_some());
    assert!(item.synced_at.unwrap() > 0);
}

#[test]
fn test_update_item_status_failed_sets_error_message() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 4)).unwrap();

    update_item_status(&conn, item_id, "failed", Some("timeout")).unwrap();

    let items = list_items_by_job(&conn, job_id, None);
    let item = &items[0];
    assert_eq!(item.status, "failed");
    assert_eq!(item.error_message.as_deref(), Some("timeout"));
}

#[test]
fn test_update_item_upload_id() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 10)).unwrap();

    update_item_upload_id(&conn, item_id, Some("upload-abc")).unwrap();
    let items = list_items_by_job(&conn, job_id, None);
    assert_eq!(items[0].upload_id.as_deref(), Some("upload-abc"));

    update_item_upload_id(&conn, item_id, None).unwrap();
    let items2 = list_items_by_job(&conn, job_id, None);
    assert!(items2[0].upload_id.is_none());
}

#[test]
fn test_update_item_wp_attachment_id() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 11)).unwrap();

    update_item_wp_attachment_id(&conn, item_id, 999).unwrap();
    let items = list_items_by_job(&conn, job_id, None);
    assert_eq!(items[0].wp_attachment_id, Some(999));
}

#[test]
fn test_update_item_component_trace_persists_component_ids() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 12)).unwrap();

    update_item_component_trace(
        &conn,
        item_id,
        "comp-text",
        &["comp-text".to_string(), "comp-image".to_string()],
    )
    .unwrap();

    let item = get_item(&conn, item_id).expect("item should exist");
    assert_eq!(item.component_id, "comp-text");
    assert_eq!(
        item.component_ids,
        vec!["comp-text".to_string(), "comp-image".to_string()]
    );
}

#[test]
fn test_increment_item_retry() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 20)).unwrap();

    increment_item_retry(&conn, item_id).unwrap();
    increment_item_retry(&conn, item_id).unwrap();

    let items = list_items_by_job(&conn, job_id, None);
    assert_eq!(items[0].retry_count, 2);
}

#[test]
fn test_list_items_by_job_status_filter() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let id1 = create_item(&conn, &sample_item_req(job_id, 100)).unwrap();
    let id2 = create_item(&conn, &sample_item_req(job_id, 101)).unwrap();
    let id3 = create_item(&conn, &sample_item_req(job_id, 102)).unwrap();

    update_item_status(&conn, id1, "done", None).unwrap();
    update_item_status(&conn, id2, "failed", None).unwrap();

    let all = list_items_by_job(&conn, job_id, None);
    assert_eq!(all.len(), 3);

    let done_items = list_items_by_job(&conn, job_id, Some("done"));
    assert_eq!(done_items.len(), 1);
    assert_eq!(done_items[0].id, id1);

    let failed_items = list_items_by_job(&conn, job_id, Some("failed"));
    assert_eq!(failed_items.len(), 1);
    assert_eq!(failed_items[0].id, id2);

    let pending_items = list_items_by_job(&conn, job_id, Some("pending"));
    assert_eq!(pending_items.len(), 1);
    assert_eq!(pending_items[0].id, id3);
}

#[test]
fn test_get_items_by_object() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let mut req = sample_item_req(job_id, 55);
    req.task_type = "text".to_string();
    create_item(&conn, &req).unwrap();

    req.task_type = "image".to_string();
    create_item(&conn, &req).unwrap();

    create_item(&conn, &sample_item_req(job_id, 99)).unwrap();

    let items = get_items_by_object(&conn, "https://example.com", 1, 55);
    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(item.wp_object_id, 55);
    }
}

#[test]
fn test_count_pending_items() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let id1 = create_item(&conn, &sample_item_req(job_id, 200)).unwrap();
    let id2 = create_item(&conn, &sample_item_req(job_id, 201)).unwrap();
    let id3 = create_item(&conn, &sample_item_req(job_id, 202)).unwrap();

    assert_eq!(count_pending_items(&conn, job_id), 3);

    update_item_status(&conn, id1, "done", None).unwrap();
    assert_eq!(count_pending_items(&conn, job_id), 2);

    update_item_status(&conn, id2, "skipped", None).unwrap();
    assert_eq!(count_pending_items(&conn, job_id), 1);

    update_item_status(&conn, id3, "failed", None).unwrap();
    assert_eq!(count_pending_items(&conn, job_id), 0);
}

#[test]
fn test_count_pending_items_intermediate_statuses_count() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let id1 = create_item(&conn, &sample_item_req(job_id, 300)).unwrap();
    let id2 = create_item(&conn, &sample_item_req(job_id, 301)).unwrap();

    update_item_status(&conn, id1, "translating", None).unwrap();
    update_item_status(&conn, id2, "syncing", None).unwrap();

    assert_eq!(count_pending_items(&conn, job_id), 2);
}

#[test]
fn test_get_item_round_trip() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 77)).unwrap();

    let item = get_item(&conn, item_id).expect("item must exist");
    assert_eq!(item.id, item_id);
    assert_eq!(item.job_id, job_id);
    assert_eq!(item.domain, "https://example.com");
    assert_eq!(item.wp_object_id, 77);
    assert_eq!(item.object_type, "post");
    assert_eq!(item.task_type, "text");
    assert_eq!(item.status, "pending");
    assert_eq!(item.raw_path, "/tmp/raw.txt");
    assert_eq!(item.translated_path, "");
}

#[test]
fn test_get_item_returns_none_for_missing_id() {
    let conn = make_db();
    assert!(get_item(&conn, 99999).is_none());
}

#[test]
fn test_update_item_translated_path_persists() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 88)).unwrap();

    let item = get_item(&conn, item_id).unwrap();
    assert_eq!(item.translated_path, "");

    update_item_translated_path(&conn, item_id, "/tmp/translated/post_88.json").unwrap();

    let item2 = get_item(&conn, item_id).unwrap();
    assert_eq!(item2.translated_path, "/tmp/translated/post_88.json");
}

#[test]
fn test_update_item_translated_path_updates_updated_at() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 89)).unwrap();

    let before = get_item(&conn, item_id).unwrap().updated_at;

    update_item_translated_path(&conn, item_id, "/tmp/translated/post_89.json").unwrap();

    let after = get_item(&conn, item_id).unwrap();
    assert_eq!(after.translated_path, "/tmp/translated/post_89.json");
    assert!(after.updated_at >= before, "updated_at must not decrease");
}

#[test]
fn test_update_item_translated_path_overwrites() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 90)).unwrap();

    update_item_translated_path(&conn, item_id, "/tmp/v1.json").unwrap();
    update_item_translated_path(&conn, item_id, "/tmp/v2.json").unwrap();

    let item = get_item(&conn, item_id).unwrap();
    assert_eq!(
        item.translated_path, "/tmp/v2.json",
        "should store latest path"
    );
}

#[test]
fn test_list_resumable_items_returns_matching_statuses() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let id1 = create_item(&conn, &sample_item_req(job_id, 400)).unwrap();
    let id2 = create_item(&conn, &sample_item_req(job_id, 401)).unwrap();
    let id3 = create_item(&conn, &sample_item_req(job_id, 402)).unwrap();

    update_item_status(&conn, id1, "fetched", None).unwrap();
    update_item_status(&conn, id2, "translated", None).unwrap();
    update_item_status(&conn, id3, "done", None).unwrap();

    let both = list_resumable_items(&conn, "https://example.com", &["fetched", "translated"]);
    assert_eq!(both.len(), 2);

    let just_translated = list_resumable_items(&conn, "https://example.com", &["translated"]);
    assert_eq!(just_translated.len(), 1);
    assert_eq!(just_translated[0].id, id2);
}

#[test]
fn test_list_resumable_items_filters_by_domain() {
    let conn = make_db();
    let job_a = create_job(&conn, &sample_job_req("https://a.com")).unwrap();
    let job_b = create_job(&conn, &sample_job_req("https://b.com")).unwrap();

    let mut req_a1 = sample_item_req(job_a, 500);
    req_a1.domain = "https://a.com".to_string();
    let id_a1 = create_item(&conn, &req_a1).unwrap();

    let mut req_a2 = sample_item_req(job_a, 501);
    req_a2.domain = "https://a.com".to_string();
    let id_a2 = create_item(&conn, &req_a2).unwrap();

    let mut req_b1 = sample_item_req(job_b, 502);
    req_b1.domain = "https://b.com".to_string();
    let id_b1 = create_item(&conn, &req_b1).unwrap();

    update_item_status(&conn, id_a1, "translated", None).unwrap();
    update_item_status(&conn, id_a2, "translated", None).unwrap();
    update_item_status(&conn, id_b1, "translated", None).unwrap();

    let items_a = list_resumable_items(&conn, "https://a.com", &["translated"]);
    assert_eq!(items_a.len(), 2);
    for item in &items_a {
        assert_eq!(item.domain, "https://a.com");
    }

    let items_b = list_resumable_items(&conn, "https://b.com", &["translated"]);
    assert_eq!(items_b.len(), 1);
    assert_eq!(items_b[0].domain, "https://b.com");
}

#[test]
fn test_list_resumable_items_empty_statuses_returns_empty() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let id = create_item(&conn, &sample_item_req(job_id, 600)).unwrap();
    update_item_status(&conn, id, "translated", None).unwrap();

    let items = list_resumable_items(&conn, "https://example.com", &[]);
    assert!(items.is_empty());
}

#[test]
fn test_list_resumable_items_no_matching_items_returns_empty() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();

    let id1 = create_item(&conn, &sample_item_req(job_id, 700)).unwrap();
    let id2 = create_item(&conn, &sample_item_req(job_id, 701)).unwrap();

    update_item_status(&conn, id1, "done", None).unwrap();
    update_item_status(&conn, id2, "done", None).unwrap();

    let items = list_resumable_items(&conn, "https://example.com", &["translated"]);
    assert!(items.is_empty());
}

#[test]
fn test_list_resumable_items_preserves_translated_path() {
    let conn = make_db();
    let job_id = create_job(&conn, &sample_job_req("https://example.com")).unwrap();
    let item_id = create_item(&conn, &sample_item_req(job_id, 800)).unwrap();

    update_item_translated_path(&conn, item_id, "/data/translated/post_800.json").unwrap();
    update_item_status(&conn, item_id, "translated", None).unwrap();

    let items = list_resumable_items(&conn, "https://example.com", &["translated"]);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].translated_path, "/data/translated/post_800.json");
    assert_eq!(items[0].status, "translated");
    assert_eq!(items[0].wp_object_id, 800);
}
