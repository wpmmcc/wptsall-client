use super::*;
use serde_json::json;

fn owned_item(conn: &Connection) -> i64 {
    let job = crate::db::jobs::create_job(
        conn,
        &crate::db::jobs::CreateJobRequest {
            domain: "http://127.0.0.1:9".into(),
            relation_id: 1,
            business_line: "post_content".into(),
            triggered_by: "auto".into(),
        },
    )
    .unwrap();
    crate::db::jobs::create_item(
        conn,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: "http://127.0.0.1:9".into(),
            relation_id: 1,
            business_line: "post_content".into(),
            object_type: "post_type".into(),
            wp_object_id: 88,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            component_id: "owned".into(),
            component_ids: vec!["owned".into()],
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: "".into(),
            client_task_id: "owned-reclaim".into(),
            max_retries: 1,
        },
    )
    .unwrap()
}

fn claim_rows(conn: &Connection) -> Vec<(String, String)> {
    conn.prepare("SELECT key,value FROM system_config ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test]
async fn physical_sqlite_unit_original_claim_reacquisition_is_read_only_and_exclusive() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let id = owned_item(&*db.lock().await);
    let lease = UnitLease::item(&db, id).await.unwrap();
    drop(lease);
    let before = {
        let conn = db.lock().await;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
        claim_rows(&conn)
    };
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let lease = UnitLease::item(&db, id).await.unwrap();
    lease.assert_item(&*db.lock().await, &db, id).unwrap();
    assert!(UnitLease::item(&db, id).await.is_err());
    assert!(
        ContentExecution::acquire(&db, "http://127.0.0.1:9", 1, "post_type", 88)
            .await
            .is_err()
    );
    assert_eq!(claim_rows(&*db.lock().await), before);
    drop(lease);
    let content = ContentExecution::acquire(&db, "http://127.0.0.1:9", 1, "post_type", 88)
        .await
        .unwrap();
    content.assert_owner(&*db.lock().await, &db).unwrap();
    assert!(UnitLease::item(&db, id).await.is_err());
    drop(content);
    assert_eq!(claim_rows(&*db.lock().await), before);
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

async fn invalid_reclaim_boundary(changed: &str) {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let id = owned_item(&*db.lock().await);
    if changed != "fresh" {
        drop(UnitLease::item(&db, id).await.unwrap());
    }
    let suffix = UnitLease::content_suffix("http://127.0.0.1:9", 1, "post_type", 88).unwrap();
    let item_key = format!("review-execution-claim-v1:{id}");
    let content_key = format!("content-execution-claim-v1:{suffix}");
    {
        let conn = db.lock().await;
        let owner = uuid::Uuid::new_v4().to_string();
        match changed {
            "item-plain" => crate::db::system::set_system_config(&conn,&item_key,
                &json!({"format":"review-execution-claim-v1","item_id":id,"owner":owner}).to_string(),
            ).unwrap(),
            "item-foreign" | "item-nil" => crate::db::system::set_encrypted_config(&conn,&item_key,
                &json!({"format":"review-execution-claim-v1",
                    "item_id":if changed=="item-foreign" {id+1} else {id},
                    "owner":if changed=="item-nil" {uuid::Uuid::nil().to_string()} else {owner}
                }).to_string(),
            ).unwrap(),
            "content-plain" => crate::db::system::set_system_config(&conn,&content_key,
                &json!({"format":"content-execution-claim-v1","suffix":suffix,"owner":owner}).to_string(),
            ).unwrap(),
            "content-foreign" => crate::db::system::set_encrypted_config(&conn,&content_key,
                &json!({"format":"content-execution-claim-v1","suffix":"content-foreign","owner":owner}).to_string(),
            ).unwrap(),
            "missing-item" | "missing-content" => {
                conn.execute("DELETE FROM system_config WHERE key=?1",
                    [if changed=="missing-item" {&item_key} else {&content_key}],
                ).unwrap();
            }
            "fresh" => {}
            _ => unreachable!(),
        }
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }
    let before = claim_rows(&*db.lock().await);
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    let credit =
        crate::storage_capacity::database_recovery_credit(&root.path().join("owned.sqlite"), false)
            .unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let result =
        crate::storage_capacity::with_result_credit(credit, UnitLease::item(&db, id)).await;
    assert!(
        result.is_err(),
        "invalid or new unit claim must not borrow ambient credit: {changed}"
    );
    assert_eq!(claim_rows(&*db.lock().await), before);
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked
    );
}

#[tokio::test]
async fn physical_sqlite_unit_fresh_missing_or_changed_claim_cannot_borrow_ambient_credit() {
    for changed in [
        "fresh",
        "item-plain",
        "item-foreign",
        "item-nil",
        "content-plain",
        "content-foreign",
        "missing-item",
        "missing-content",
    ] {
        invalid_reclaim_boundary(changed).await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn physical_sqlite_unit_replaced_lock_inode_revokes_original_authority_without_writes() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = Arc::new(tokio::sync::Mutex::new(
        crate::db::open_db(root.path().join("owned.sqlite").to_str().unwrap()).unwrap(),
    ));
    let id = owned_item(&*db.lock().await);
    let lease = UnitLease::item(&db, id).await.unwrap();
    let before = claim_rows(&*db.lock().await);
    {
        let conn = db.lock().await;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }
    let original = root.path().join(format!("owned.sqlite.review-{id}.lock"));
    std::fs::rename(&original, root.path().join("owned-old.lock")).unwrap();
    File::create(&original).unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    assert!(lease.assert_item(&*db.lock().await, &db, id).is_err());
    assert!(lease
        .mutate_item(&db, id, |conn| {
            crate::db::jobs::update_item_status(conn, id, "done", None)
        })
        .await
        .is_err());
    assert_eq!(claim_rows(&*db.lock().await), before);
    assert_eq!(
        root.path()
            .join("owned.sqlite-wal")
            .metadata()
            .unwrap()
            .len(),
        0
    );
}
