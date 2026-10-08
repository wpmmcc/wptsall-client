//! Actual ordinary JSON writers, using only owned local files and SQLite.
use super::*;

async fn owned_json_item(db: &Arc<tokio::sync::Mutex<rusqlite::Connection>>, path: &str) -> i64 {
    let conn = db.lock().await;
    let job = crate::db::jobs::create_job(
        &conn,
        &crate::db::jobs::CreateJobRequest {
            domain: "https://owned.invalid".into(),
            relation_id: 1,
            business_line: "post_content".into(),
            triggered_by: "owned-json-contract".into(),
        },
    )
    .unwrap();
    crate::db::jobs::create_item(
        &conn,
        &crate::db::jobs::CreateItemRequest {
            job_id: job,
            domain: "https://owned.invalid".into(),
            relation_id: 1,
            business_line: "post_content".into(),
            object_type: "post".into(),
            wp_object_id: 88,
            wp_object_subtype: "post".into(),
            task_type: "text".into(),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            component_id: String::new(),
            component_ids: Vec::new(),
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
            raw_path: path.into(),
            client_task_id: "owned-json-operation".into(),
            max_retries: 3,
        },
    )
    .unwrap()
}

fn assert_owned_json_encrypted(path: &std::path::Path, marker: &str) {
    let bytes = std::fs::read(path).unwrap();
    assert!(
        bytes.starts_with(b"WPTC"),
        "new ordinary JSON must be encrypted"
    );
    assert!(
        !bytes
            .windows(marker.len())
            .any(|window| window == marker.as_bytes()),
        "owned content must not remain plaintext on disk"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}

#[tokio::test]
async fn encrypted_json_actual_raw_writer_keeps_logical_payload_and_status() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("raw/owned.json");
    let db = ensure_db(None);
    let id = owned_json_item(&db, path.to_str().unwrap()).await;
    let marker = "owned-raw-json-private-sentinel";
    let value = json!({"post_title":marker,"post_content":"<p>owned source</p>"});
    persist_raw_content(&db, id, &value, path.to_str().unwrap(), "/dev/null")
        .await
        .unwrap();
    assert_owned_json_encrypted(&path, marker);
    let logical: Value =
        serde_json::from_str(&crate::bindings::load_encrypted_or_plain(&path).unwrap()).unwrap();
    assert_eq!(logical, value);
    let conn = db.lock().await;
    assert_eq!(
        crate::db::jobs::get_item_checked(&conn, id)
            .unwrap()
            .unwrap()
            .status,
        "fetched"
    );
}

#[tokio::test]
async fn encrypted_json_actual_result_writer_keeps_original_identity_and_status() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("translated/owned.json");
    let db = ensure_db(None);
    let id = owned_json_item(&db, "owned-unused-raw.json").await;
    {
        let conn = db.lock().await;
        crate::db::jobs::update_item_status(&conn, id, "fetched", None).unwrap();
    }
    let marker = "owned-result-json-private-sentinel";
    let payload = TranslationCallbackPayload {
        schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
        attempt_id: uuid::Uuid::new_v4().to_string(),
        object_snapshot_hash: "owned-object-snapshot".into(),
        source_revision: "owned-source-revision".into(),
        policy_version: "owned-policy".into(),
        field_results: Vec::new(),
        relation_id: 1,
        business_line: "post_content".into(),
        object_type: "post_type".into(),
        subtype: "post".into(),
        object_id: 88,
        translated_fields: [("post_title".into(), marker.into())].into(),
        translated_meta: HashMap::new(),
        media_mappings: Vec::new(),
        media_field_sources: HashMap::new(),
        client_task_id: "owned-json-operation".into(),
        outbox_id: None,
        worker_id: "owned-worker".into(),
        source_lang: "en".into(),
        target_lang: "zh".into(),
        execution_time_ms: 1,
    };
    persist_translated(
        &db,
        id,
        &payload,
        "owned-json-operation",
        None,
        path.to_str().unwrap(),
        "/dev/null",
    )
    .await
    .unwrap();
    assert_owned_json_encrypted(&path, marker);
    let logical: Value =
        serde_json::from_str(&crate::bindings::load_encrypted_or_plain(&path).unwrap()).unwrap();
    assert_eq!(logical["payload"], serde_json::to_value(&payload).unwrap());
    assert_eq!(logical["idempotency_key"], "owned-json-operation");
    let conn = db.lock().await;
    let item = crate::db::jobs::get_item_checked(&conn, id)
        .unwrap()
        .unwrap();
    assert_eq!(item.status, "translated");
    assert_eq!(item.translated_path, path.to_string_lossy());
    assert_eq!(item.client_task_id, "owned-json-operation");
}

#[tokio::test]
async fn encrypted_json_media_delivery_does_not_downgrade_or_rewrite_paid_ciphertext() {
    let (before, after) = pipeline_media_receipt_with_format(false, true).await;
    assert!(before.starts_with(b"WPTC"));
    assert_eq!(
        after, before,
        "paid physical receipt bytes must remain exact"
    );
}

#[tokio::test]
async fn encrypted_json_media_delivery_does_not_rewrite_legacy_paid_original() {
    let (before, after) = pipeline_media_receipt_with_format(false, false).await;
    assert_eq!(after, before, "legacy paid evidence remains read-only");
}

#[tokio::test]
async fn encrypted_json_media_pending_projection_refusal_replays_receipt_without_repeat_upload() {
    let (before, after) = pipeline_media_receipt_with_projection(false, true, true).await;
    assert!(before.starts_with(b"WPTC"));
    assert_eq!(
        after, before,
        "a refused projection cannot rewrite paid bytes"
    );
}

#[test]
fn encrypted_json_replay_preserves_exact_ciphertext_and_refuses_changed_payload() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result.json");
    let value = json!({"owned":"private-ciphertext-generation"});
    save_immutable_json(&path, &value).unwrap();
    let before = std::fs::read(&path).unwrap();
    save_immutable_json(&path, &value).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(save_immutable_json(&path, &json!({"owned":"different"})).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn encrypted_json_existing_legacy_is_read_only_and_not_migrated() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("legacy.json");
    let original = b"{ \"owned\" : \"legacy-paid\" }\n";
    std::fs::write(&path, original).unwrap();
    save_immutable_json(&path, &json!({"owned":"legacy-paid"})).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(save_immutable_json(&path, &json!({"owned":"changed"})).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn encrypted_json_wrong_key_or_corruption_cannot_overwrite_original() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("result.json");
    let value = json!({"owned":"retained-private-json"});
    save_immutable_json(&path, &value).unwrap();
    let before = std::fs::read(&path).unwrap();
    {
        let _wrong = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-wrong-json-key",
        );
        let error = save_immutable_json(&path, &value).unwrap_err();
        assert!(!format!("{error:#}").contains("retained-private-json"));
        assert!(!format!("{error:#}").contains("owned-wrong-json-key"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let mut damaged = before;
    *damaged.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &damaged).unwrap();
    assert!(save_immutable_json(&path, &value).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
}

#[test]
fn encrypted_json_no_key_never_creates_plaintext_or_downgrades_existing_ciphertext() {
    const PROBE: &str = "WPTSALL_OWNED_JSON_NO_KEY_PROBE";
    if let Ok(root) = std::env::var(PROBE) {
        let root = std::path::PathBuf::from(root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(crate::bindings::bindings_secret().is_none());
        let value = json!({"owned":"no-key-json"});
        let missing = root.join("missing.json");
        assert!(save_immutable_json(&missing, &value).is_err());
        assert!(!missing.exists());
        let cipher = root.join("encrypted.json");
        let before = std::fs::read(&cipher).unwrap();
        assert!(save_immutable_json(&cipher, &value).is_err());
        assert_eq!(std::fs::read(&cipher).unwrap(), before);
        let legacy = root.join("legacy.json");
        let before = std::fs::read(&legacy).unwrap();
        save_immutable_json(&legacy, &value).unwrap();
        assert_eq!(std::fs::read(&legacy).unwrap(), before);
        return;
    }
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let value = json!({"owned":"no-key-json"});
    save_immutable_json(&root.path().join("encrypted.json"), &value).unwrap();
    std::fs::write(
        root.path().join("legacy.json"),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
    let executable = if cfg!(target_os = "linux") {
        std::path::PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let output = std::process::Command::new(executable)
        .args([
            "--exact",
            "task_engine::pipeline::tests::encrypted_content_snapshots::encrypted_json_no_key_never_creates_plaintext_or_downgrades_existing_ciphertext",
            "--nocapture",
        ])
        .env(PROBE, root.path())
        .env_remove("WPTSALL_COMPONENT_BINDINGS_SECRET")
        .env_remove("WPTSALL_DEVICE_ID")
        .env("WPTSALL_DATA_DIR", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "owned no-key child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn encrypted_json_symlink_refuses_without_touching_target() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target.json");
    let alias = root.path().join("alias.json");
    let original = b"{\"owned\":\"target\"}";
    std::fs::write(&target, original).unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    assert!(save_immutable_json(&alias, &json!({"owned":"target"})).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert!(std::fs::symlink_metadata(alias)
        .unwrap()
        .file_type()
        .is_symlink());
}
