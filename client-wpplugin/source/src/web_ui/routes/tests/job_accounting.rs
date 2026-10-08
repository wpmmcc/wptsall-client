// catalog: WEBUI-MOD-db-jobs-rs
// catalog: WEBUI-API-GET-api-jobs
// catalog: WEBUI-API-PREFIX-api-jobs
// oracle: L2
use super::*;
use crate::web_ui::test_support::WebUiTestHarness;

#[tokio::test]
async fn cli06_jobs_http_readback_matches_stored_items_and_callback_retry_state() {
    let harness = WebUiTestHarness::new("http://127.0.0.1:1", None)
        .await
        .unwrap();
    let conn = crate::db::open_db(harness.db_path.to_str().unwrap()).unwrap();
    let job_id = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: "https://fixture.invalid".to_string(),
            relation_id: 7,
            business_line: "post_content".to_string(),
            triggered_by: "auto".to_string(),
        },
    )
    .unwrap();
    let mut ids = Vec::new();
    for (index, status) in ["done", "failed", "translated", "pending_review", "skipped"]
        .into_iter()
        .enumerate()
    {
        let id = crate::db::jobs::create_item(
            &conn,
            &crate::db::jobs::CreateItemRequest {
                job_id,
                domain: "https://fixture.invalid".to_string(),
                relation_id: 7,
                business_line: "post_content".to_string(),
                object_type: "post".to_string(),
                wp_object_id: index as i64 + 1,
                wp_object_subtype: "post".to_string(),
                task_type: "text".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                component_id: String::new(),
                component_ids: Vec::new(),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
                raw_path: String::new(),
                client_task_id: format!("fixture-{index}"),
                max_retries: 2,
            },
        )
        .unwrap();
        crate::db::jobs::update_item_status(&conn, id, status, Some("fixture retry error"))
            .unwrap();
        ids.push(id);
    }
    crate::db::jobs::project_job_from_items(&conn, job_id, true, "completed").unwrap();
    let listed = harness.list_jobs().await.unwrap();
    assert!(listed.status_line.contains("200 OK"));
    let jobs = listed.body["data"]["items"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["status"], "partial");
    assert_eq!(jobs[0]["total_items"], 5);
    assert_eq!(jobs[0]["done_items"], 2);
    assert_eq!(jobs[0]["failed_items"], 1);
    assert_eq!(
        jobs[0]["progress"],
        json!({
            "total": 5, "done": 2, "failed": 1, "pending_review": 1, "translating": 1
        })
    );
    let detail = harness
        .get_json(&format!("/api/jobs/{job_id}"))
        .await
        .unwrap();
    assert_eq!(detail.body["data"]["total_items"], 5);
    assert_eq!(detail.body["data"]["done_items"], 2);
    assert_eq!(detail.body["data"]["failed_items"], 1);
    let items = harness.list_job_items(job_id).await.unwrap();
    assert_eq!(items.body["data"]["items"].as_array().unwrap().len(), 5);
    assert_eq!(
        crate::db::jobs::get_item(&conn, ids[2]).unwrap().status,
        "translated"
    );
    for id in ids {
        crate::db::jobs::update_item_status(&conn, id, "done", None).unwrap();
    }
    let listed = harness.list_jobs().await.unwrap();
    let job = &listed.body["data"]["items"][0];
    assert_eq!(job["status"], "completed");
    assert_eq!(
        (
            job["total_items"].as_i64(),
            job["done_items"].as_i64(),
            job["failed_items"].as_i64()
        ),
        (Some(5), Some(5), Some(0))
    );
    assert_eq!(
        job["progress"],
        json!({
            "total": 5, "done": 5, "failed": 0, "pending_review": 0, "translating": 0
        })
    );
}
