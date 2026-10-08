fn full_policy(root: &std::path::Path) {
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
}

#[test]
fn physical_logging_disabled_runtime_does_not_create_a_header_or_control() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(false, "debug");
    init_runtime_log_file(root.path().join("disabled.log").to_str().unwrap()).unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn physical_logging_enabled_runtime_full_root_keeps_admission_strict() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    full_policy(root.path());
    let retained = root.path().join("retained-paid-evidence");
    std::fs::write(&retained, b"owned paid evidence").unwrap();
    let log = root.path().join("enabled.log");
    init_runtime_log_file(log.to_str().unwrap()).unwrap();
    assert!(get_log_enabled(), "retain the user's setting");
    assert!(LOG_WRITER.lock().unwrap().is_none());
    assert!(!log.exists());
    assert_eq!(std::fs::read(&retained).unwrap(), b"owned paid evidence");
    let error = init_log_file(log.to_str().unwrap()).unwrap_err();
    assert!(error.is::<crate::storage_capacity::RootCapacityExhausted>());
}

#[test]
fn physical_logging_runtime_does_not_ignore_invalid_capacity_authority() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let policy = root.path().join(".wptsall-storage-v1.json");
    std::fs::write(&policy, b"owned invalid policy").unwrap();
    let log = root.path().join("invalid.log");
    assert!(init_runtime_log_file(log.to_str().unwrap()).is_err());
    assert_eq!(std::fs::read(&policy).unwrap(), b"owned invalid policy");
    assert!(!log.exists());
}

#[test]
fn physical_logging_full_root_refuses_a_new_header() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    full_policy(root.path());
    let log = root.path().join("owned.log");
    assert!(init_log_file(log.to_str().unwrap()).is_err());
    assert!(!log.exists(), "a refused header must not create a log");
}

#[test]
fn physical_logging_full_root_refuses_warning_without_direct_fallback() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    init_log_file(log.to_str().unwrap()).unwrap();
    let original = std::fs::read(&log).unwrap();
    full_policy(root.path());
    assert!(log_event(log.to_str().unwrap(), "warn", "owned", json!({})).is_err());
    assert_eq!(std::fs::read(&log).unwrap(), original);
}

#[test]
fn physical_logging_buffer_flush_respects_a_lowered_limit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    init_log_file(log.to_str().unwrap()).unwrap();
    let original = std::fs::read(&log).unwrap();
    log_event(log.to_str().unwrap(), "info", "owned", json!({})).unwrap();
    full_policy(root.path());
    flush_log();
    assert_eq!(std::fs::read(&log).unwrap(), original);
    assert!(
        LOG_WRITER.lock().unwrap().as_ref().unwrap().pending_bytes > 0,
        "refused buffered bytes must remain pending"
    );
}

#[test]
fn physical_logging_failed_backup_never_truncates_retained_live_bytes() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    let original = b"owned-retained-live-log\n";
    std::fs::write(&log, original).unwrap();
    for slot in 1..=3 {
        let directory = root.path().join(format!("owned.log.{slot}"));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("owned"), b"owned-pin").unwrap();
    }
    assert!(rotate_log(log.to_str().unwrap()).is_err());
    assert_eq!(std::fs::read(&log).unwrap(), original);
    for slot in 1..=3 {
        assert_eq!(
            std::fs::read(root.path().join(format!("owned.log.{slot}/owned"))).unwrap(),
            b"owned-pin"
        );
    }
}

#[test]
fn physical_logging_rotation_keeps_all_three_confirmed_backups() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    for (name, value) in [
        ("owned.log", "live"),
        ("owned.log.1", "one"),
        ("owned.log.2", "two"),
        ("owned.log.3", "three"),
    ] {
        std::fs::write(root.path().join(name), value).unwrap();
    }
    rotate_log(log.to_str().unwrap()).unwrap();
    for (slot, expected) in [(1, "live"), (2, "one"), (3, "two")] {
        assert_eq!(
            std::fs::read_to_string(root.path().join(format!("owned.log.{slot}"))).unwrap(),
            expected
        );
    }
}

#[cfg(unix)]
#[test]
fn physical_logging_new_files_and_parents_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = root.path().join("new/nested/owned.log");
    init_log_file(log.to_str().unwrap()).unwrap();
    for (path, expected) in [
        (log, 0o600),
        (root.path().join("new"), 0o700),
        (root.path().join("new/nested"), 0o700),
        (root.path().to_path_buf(), 0o755),
    ] {
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            expected
        );
    }
}

#[test]
fn physical_logging_drop_cannot_flush_past_a_lowered_limit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    init_log_file(log.to_str().unwrap()).unwrap();
    let original = std::fs::read(&log).unwrap();
    log_event(log.to_str().unwrap(), "info", "owned-pending", json!({})).unwrap();
    full_policy(root.path());
    let writer = LOG_WRITER.lock().unwrap().take();
    drop(writer);
    assert_eq!(std::fs::read(&log).unwrap(), original);
}

#[test]
fn physical_logging_path_switch_keeps_a_refused_buffer_for_later_flush() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let old = root.path().join("old.log");
    let next = root.path().join("next.log");
    init_log_file(old.to_str().unwrap()).unwrap();
    log_event(old.to_str().unwrap(), "info", "owned-pending", json!({})).unwrap();
    let original = std::fs::read(&old).unwrap();
    full_policy(root.path());
    assert!(init_log_file(next.to_str().unwrap()).is_err());
    assert!(!next.exists());
    assert_eq!(std::fs::read(&old).unwrap(), original);
    assert_eq!(
        LOG_WRITER.lock().unwrap().as_ref().unwrap().path,
        old.to_str().unwrap()
    );
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":34359738368}"#,
    )
    .unwrap();
    init_log_file(next.to_str().unwrap()).unwrap();
    let previous = std::fs::read_to_string(old).unwrap();
    assert_eq!(previous.matches("owned-pending").count(), 1);
    assert!(!std::fs::read_to_string(next)
        .unwrap()
        .contains("owned-pending"));
}

#[test]
fn physical_logging_export_refuses_full_root_without_replacing_original() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    let export = root.path().join("export.log");
    init_log_file(log.to_str().unwrap()).unwrap();
    log_event(log.to_str().unwrap(), "warn", "owned-original", json!({})).unwrap();
    std::fs::write(&export, b"original-export").unwrap();
    full_policy(root.path());
    assert!(maybe_export_log(log.to_str().unwrap(), export.to_str().unwrap()).is_err());
    assert_eq!(std::fs::read(export).unwrap(), b"original-export");
}

#[test]
fn physical_logging_backup_copy_is_verified_before_truncation() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    let backup = root.path().join("owned.log.1");
    std::fs::write(&log, b"owned-exact-live").unwrap();
    copy_log_backup_and_truncate(&log, &backup).unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), b"owned-exact-live");
    assert!(std::fs::read(&log).unwrap().is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn physical_logging_backup_peak_refusal_preserves_live_and_backup() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let log = root.path().join("owned.log");
    let backup = root.path().join("owned.log.1");
    std::fs::write(&log, b"owned-exact-live").unwrap();
    std::fs::write(&backup, b"original-backup").unwrap();
    full_policy(root.path());
    assert!(copy_log_backup_and_truncate(&log, &backup).is_err());
    assert_eq!(std::fs::read(log).unwrap(), b"owned-exact-live");
    assert_eq!(std::fs::read(backup).unwrap(), b"original-backup");
}

#[tokio::test]
async fn physical_logging_never_spends_a_paid_result_booking() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::create_tables(&conn).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    crate::db::capacity::reserve_new(&tx, "owned-paid-unit", 256 * 1024).unwrap();
    tx.commit().unwrap();
    let credit = crate::storage_capacity::result_credit(&conn, "owned-paid-unit")
        .unwrap()
        .unwrap();
    let directory = root.path().join(".wptsall-storage-bookings-v1");
    let booking = std::fs::read_dir(directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original = std::fs::read(&booking).unwrap();
    full_policy(root.path());
    let result = crate::storage_capacity::with_result_credit(Some(credit), async {
        init_log_file(root.path().join("unrelated.log").to_str().unwrap())
    })
    .await;
    assert!(result.is_err(), "logging is not the admitted paid result");
    assert!(!root.path().join("unrelated.log").exists());
    assert_eq!(std::fs::read(booking).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn physical_logging_configured_alias_export_uses_its_admitted_target() {
    use std::os::unix::fs::symlink;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let target = root.path().join("actual.log");
    let alias = root.path().join("configured.log");
    let export = root.path().join("export.log");
    std::fs::write(&target, b"owned-existing-log\n").unwrap();
    symlink(&target, &alias).unwrap();
    init_log_file(alias.to_str().unwrap()).unwrap();
    log_event(alias.to_str().unwrap(), "warn", "owned-alias", json!({})).unwrap();
    maybe_export_log(alias.to_str().unwrap(), export.to_str().unwrap()).unwrap();
    assert_eq!(
        std::fs::read(export).unwrap(),
        std::fs::read(target).unwrap()
    );
    assert!(std::fs::symlink_metadata(alias)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(unix)]
#[test]
fn physical_logging_configured_alias_rotation_retains_link_and_target_family() {
    use std::os::unix::fs::symlink;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let target = root.path().join("actual.log");
    let alias = root.path().join("configured.log");
    std::fs::write(&target, b"owned-previous-log\n").unwrap();
    symlink(&target, &alias).unwrap();
    init_log_file(alias.to_str().unwrap()).unwrap();
    LOG_WRITER.lock().unwrap().as_mut().unwrap().file_bytes = MAX_LOG_SIZE;
    log_event(
        alias.to_str().unwrap(),
        "warn",
        "owned-after-rotation",
        json!({}),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.path().join("actual.log.1")).unwrap(),
        b"owned-previous-log\n"
    );
    assert!(std::fs::read_to_string(target)
        .unwrap()
        .contains("owned-after-rotation"));
    assert!(std::fs::symlink_metadata(alias)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!root.path().join("configured.log.1").exists());
}

#[cfg(unix)]
#[test]
fn physical_logging_rotation_keeps_a_secondary_retained_alias_resolvable() {
    use std::os::unix::fs::symlink;
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let (_lock, _state) = acquire_log_state(true, "debug");
    let target = root.path().join("actual.log");
    let alias = root.path().join("retained-view.log");
    std::fs::write(&target, b"owned-previous-log\n").unwrap();
    symlink(&target, &alias).unwrap();
    init_log_file(target.to_str().unwrap()).unwrap();
    LOG_WRITER.lock().unwrap().as_mut().unwrap().file_bytes = MAX_LOG_SIZE;
    log_event(
        target.to_str().unwrap(),
        "warn",
        "owned-after-rotation",
        json!({}),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.path().join("actual.log.1")).unwrap(),
        b"owned-previous-log\n"
    );
    assert!(std::fs::read_to_string(alias)
        .unwrap()
        .contains("owned-after-rotation"));
}
