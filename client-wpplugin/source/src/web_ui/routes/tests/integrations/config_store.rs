use super::*;
use crate::web_ui::routes::integrations::config_store::{load_config, save_config};

fn changed_oauth_doc() -> VendorOAuthDoc {
    VendorOAuthDoc {
        version: 1,
        configs: HashMap::from([(
            "owned".into(),
            make_test_oauth_config("http://127.0.0.1:1/token".into()),
        )]),
    }
}

#[tokio::test]
async fn integration_durable_ignored_save_receipt_rolls_back_document_without_changing_file() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let state = build_test_web_ui_state("", None);
    state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch(
            "CREATE TRIGGER refuse_receipt BEFORE INSERT ON system_config
         WHEN NEW.key LIKE 'integration-save-v1:%' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    assert!(save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &changed_oauth_doc()
    )
    .await
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(crate::db::system::get_system_config_checked(
        &*state.lock().await.db.lock().await,
        "vendor_oauth_doc",
    )
    .unwrap()
    .is_none());
}

#[tokio::test]
async fn integration_durable_stale_snapshot_cannot_replace_newer_configuration() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    let state = build_test_web_ui_state("", None);
    let after = changed_oauth_doc();
    save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &after,
    )
    .await
    .unwrap();
    assert!(save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &before
    )
    .await
    .is_err());
    let actual: VendorOAuthDoc = load_config(&state, path.to_str().unwrap(), "vendor_oauth_doc")
        .await
        .unwrap();
    assert!(actual.configs.contains_key("owned"));
}

#[cfg(unix)]
#[tokio::test]
async fn integration_durable_file_projection_refusal_recovers_committed_snapshot_on_restart() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    let state = build_test_web_ui_state("", None);
    let _: VendorOAuthDoc = load_config(&state, path.to_str().unwrap(), "vendor_oauth_doc")
        .await
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let restore = Restore(root.path().to_path_buf());
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let after = changed_oauth_doc();
    assert!(save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &after
    )
    .await
    .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let db = state.lock().await.db.clone();
    let raw = crate::db::system::get_system_config_checked(
        &*db.lock().await,
        "integration-save-v1:vendor_oauth_doc",
    )
    .unwrap()
    .expect("committed authority must survive the failed file projection");
    assert!(raw.starts_with("V1BUQw"));
    assert!(!raw.contains("secret-xyz"));
    drop(restore);
    drop(state);
    let restarted = build_test_web_ui_state("", None);
    restarted.lock().await.db = db.clone();
    let recovered: VendorOAuthDoc =
        load_config(&restarted, path.to_str().unwrap(), "vendor_oauth_doc")
            .await
            .unwrap();
    assert!(recovered.configs.contains_key("owned"));
    assert!(crate::db::system::get_system_config_checked(
        &*db.lock().await,
        "integration-save-v1:vendor_oauth_doc",
    )
    .unwrap()
    .is_none());
    let file = crate::bindings::load_vendor_oauth(path.to_str().unwrap()).unwrap();
    assert!(file.configs.contains_key("owned"));
}

#[tokio::test]
async fn integration_durable_newer_operator_file_is_not_replaced_by_pending_projection() {
    let _env = components_env_lock().lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("owned-oauth.json");
    let before = VendorOAuthDoc::default();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &before).unwrap();
    let state = build_test_web_ui_state("", None);
    state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch(
            "CREATE TRIGGER retain_receipt BEFORE DELETE ON system_config
         WHEN OLD.key LIKE 'integration-save-v1:%' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let after = changed_oauth_doc();
    assert!(save_config(
        &state,
        path.to_str().unwrap(),
        "vendor_oauth_doc",
        &before,
        &after
    )
    .await
    .is_err());
    let mut operator = after;
    operator.configs.get_mut("owned").unwrap().client_id = "owned-newer-operator".into();
    crate::bindings::save_vendor_oauth(path.to_str().unwrap(), &operator).unwrap();
    let original = std::fs::read(&path).unwrap();
    state
        .lock()
        .await
        .db
        .lock()
        .await
        .execute_batch("DROP TRIGGER retain_receipt;")
        .unwrap();
    assert!(
        load_config::<VendorOAuthDoc>(&state, path.to_str().unwrap(), "vendor_oauth_doc")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        crate::db::system::get_system_config_checked(
            &*state.lock().await.db.lock().await,
            "integration-save-v1:vendor_oauth_doc",
        )
        .unwrap()
        .is_some(),
        "conflicting committed authority is retained"
    );
}
