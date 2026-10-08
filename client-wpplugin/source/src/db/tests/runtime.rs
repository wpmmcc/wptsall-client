// catalog: WEBUI-MOD-db-runtime-rs
// catalog: WEBUI-MOD-db-mod-rs
// oracle: L2
use super::*;

fn make_job(conn: &Connection, phase: &str) -> (i64, i64) {
    let job = crate::db::jobs::create_job(
        conn,
        &crate::db::jobs::CreateJobRequest {
            domain: "https://cli05.invalid".to_string(),
            relation_id: 7,
            business_line: "discovery".to_string(),
            triggered_by: "auto".to_string(),
        },
    )
    .unwrap();
    crate::db::jobs::update_job_status(conn, job, "running").unwrap();
    let item = crate::db::jobs::create_item(
        conn,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: "https://cli05.invalid".to_string(),
            relation_id: 7,
            business_line: "post_content".to_string(),
            object_type: "post_type".to_string(),
            wp_object_id: 42,
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
            raw_path: "fixture-raw".to_string(),
            client_task_id: "fixture-recovery".to_string(),
            max_retries: 3,
        },
    )
    .unwrap();
    crate::db::jobs::update_item_status(conn, item, phase, Some("fixture-error")).unwrap();
    assert!(crate::db::discovery_tasks::try_claim_in_progress(
        conn,
        "https://cli05.invalid",
        7,
        "post_type",
        42
    )
    .unwrap());
    (job, item)
}

fn state(conn: &Connection, job: i64, item: i64) -> serde_json::Value {
    let claims: i64 = conn
        .query_row("SELECT COUNT(*) FROM translation_in_progress", [], |row| {
            row.get(0)
        })
        .unwrap();
    serde_json::json!({
        "job": crate::db::jobs::get_job(conn, job).unwrap(),
        "item": crate::db::jobs::get_item(conn, item).unwrap(), "claims": claims
    })
}

#[test]
fn cli05_recovery_transaction_rolls_back_items_projection_and_claim_deletion() {
    for trigger in [
        "CREATE TRIGGER fail_recovery BEFORE UPDATE ON translation_jobs BEGIN SELECT RAISE(ABORT, 'fixture recovery projection'); END;",
        "CREATE TRIGGER fail_recovery BEFORE DELETE ON translation_in_progress BEGIN SELECT RAISE(ABORT, 'fixture recovery claims'); END;",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("atomic.db");
        let lease = RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
        let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
        let (job, item) = make_job(&conn, "fetched");
        conn.execute("UPDATE translation_jobs SET total_items=2 WHERE id=?1", params![job]).unwrap();
        let before = state(&conn, job, item);
        conn.execute_batch(trigger).unwrap();
        assert!(recover_interrupted_work(&conn, &lease).is_err());
        assert_eq!(state(&conn, job, item), before, "no part of recovery may escape a failed transaction");
        assert!(conn.is_autocommit());
        conn.execute_batch("DROP TRIGGER fail_recovery").unwrap();
        let report = recover_interrupted_work(&conn, &lease).unwrap();
        assert_eq!((report.reset_items, report.finalized_jobs, report.released_claims), (1,1,1));
        assert_eq!(crate::db::jobs::get_item(&conn, item).unwrap().status, "pending");
        assert_eq!(crate::db::jobs::get_job(&conn, job).unwrap().status, "partial");
        let stable = state(&conn, job, item);
        let report = recover_interrupted_work(&conn, &lease).unwrap();
        assert_eq!((report.reset_items, report.finalized_jobs, report.released_claims), (0,0,0));
        assert_eq!(state(&conn, job, item), stable, "a second sweep is a no-op");
    }
}

#[test]
fn cli05_plain_database_readers_do_not_recover_active_work() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("readers.db");
    let lease = RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    let (job, item) = make_job(&conn, "fetching");
    let before = state(&conn, job, item);
    for _ in 0..3 {
        let reader = crate::db::open_db(path.to_str().unwrap()).unwrap();
        assert_eq!(state(&reader, job, item), before);
        let owner = RuntimeLease::for_connection(&reader).unwrap();
        assert!(Arc::ptr_eq(&owner, &lease));
    }
}

#[test]
fn cli05_descendant_lease_outlives_parent_and_wrong_database_cannot_be_recovered() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.db");
    let lease = RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    let descendant = RuntimeLease::for_connection(&conn).unwrap();
    drop(lease);
    assert!(
        RuntimeLease::acquire(path.to_str().unwrap()).is_err(),
        "a still-running descendant protects its database"
    );
    let other_path = root.path().join("other.db");
    let other = crate::db::open_db(other_path.to_str().unwrap()).unwrap();
    let (job, item) = make_job(&other, "fetched");
    let before = state(&other, job, item);
    let error = recover_interrupted_work(&other, &descendant).unwrap_err();
    assert!(error
        .to_string()
        .contains("runtime lease does not own this database"));
    assert_eq!(state(&other, job, item), before);
    drop(descendant);
    RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    assert!(
        path.with_file_name("owned.db.runtime.lock").exists(),
        "never unlink a lock file to release it"
    );
}

#[cfg(unix)]
#[test]
fn cli05_symlink_and_parent_path_aliases_share_one_runtime_lease() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned.db");
    let _lease = RuntimeLease::acquire(path.to_str().unwrap()).unwrap();
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    let alias = root.path().join("alias.db");
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let relative = root.path().join("sub/../owned.db");
    for alias in [&alias, &relative] {
        let error = RuntimeLease::acquire(alias.to_str().unwrap())
            .err()
            .unwrap();
        assert!(error.to_string().contains("CLIENT_RUNTIME_ALREADY_RUNNING"));
    }
    let (job, item) = make_job(&conn, "fetching");
    let before = state(&conn, job, item);
    assert_eq!(state(&conn, job, item), before);
    let mut lock = path.file_name().unwrap().to_os_string();
    lock.push(".runtime.lock");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(path.with_file_name(lock))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
