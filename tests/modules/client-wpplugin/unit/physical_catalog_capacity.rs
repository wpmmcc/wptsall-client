fn owned_catalog_env(root: &Path) -> (crate::db::TestEnvVarGuard, crate::db::TestEnvVarGuard) {
    (
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap()),
        crate::db::TestEnvVarGuard::set(
            "WPTSALL_PROVIDER_CATALOG_FILE",
            root.join("cache/provider-catalog.json").to_str().unwrap(),
        ),
    )
}

#[test]
fn physical_catalog_full_root_keeps_current_lkg_and_metadata() {
    let _key = crate::db::owned_mock_bindings_key();
    let _catalog = CATALOG_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let (_root, _path) = owned_catalog_env(root.path());
    let (_, current, lkg, metadata) = catalog_cache_paths();
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    let old = serde_json::to_vec_pretty(&tiny_unsigned_catalog("owned-old")).unwrap();
    let previous = serde_json::to_vec_pretty(&tiny_unsigned_catalog("owned-lkg")).unwrap();
    let old_meta = br#"{"state":"current","catalog_version":"owned-old"}"#;
    fs::write(&current, &old).unwrap();
    fs::write(&lkg, &previous).unwrap();
    fs::write(&metadata, old_meta).unwrap();
    fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();

    assert!(
        install_catalog_candidate(&tiny_unsigned_catalog("owned-new"), "file://owned-catalog")
            .is_err(),
        "a validated catalog must not bypass the full root"
    );
    assert_eq!(fs::read(&current).unwrap(), old);
    assert_eq!(fs::read(&lkg).unwrap(), previous);
    assert_eq!(fs::read(&metadata).unwrap(), old_meta);
    assert!(fs::read_dir(current.parent().unwrap())
        .unwrap()
        .all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")
        }));
}

#[test]
fn physical_catalog_full_root_keeps_refresh_metadata() {
    let _key = crate::db::owned_mock_bindings_key();
    let _catalog = CATALOG_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let (_root, _path) = owned_catalog_env(root.path());
    let (_, _, _, metadata) = catalog_cache_paths();
    fs::create_dir_all(metadata.parent().unwrap()).unwrap();
    let old = br#"{"state":"current","catalog_version":"owned-original"}"#;
    fs::write(&metadata, old).unwrap();
    fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();

    assert!(
        write_refresh_metadata(&json!({"state":"stale_current","last_error":"owned"})).is_err(),
        "failure metadata is also a bounded writer"
    );
    assert_eq!(fs::read(metadata).unwrap(), old);
}

#[test]
fn physical_catalog_room_preserves_prior_catalog_and_private_snapshots() {
    let _key = crate::db::owned_mock_bindings_key();
    let _catalog = CATALOG_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let (_root, _path) = owned_catalog_env(root.path());
    let _unsigned = crate::db::TestEnvVarGuard::set("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
    let old = tiny_unsigned_catalog("owned-first");
    let new = tiny_unsigned_catalog("owned-second");
    install_catalog_candidate(&old, "file://owned-first").unwrap();
    let (_, current, lkg, metadata) = catalog_cache_paths();
    let old_bytes = fs::read(&current).unwrap();
    let meta = install_catalog_candidate(&new, "file://owned-second").unwrap();
    assert_eq!(fs::read(&lkg).unwrap(), old_bytes);
    assert_eq!(
        fs::read(&current).unwrap(),
        serde_json::to_vec_pretty(&new).unwrap()
    );
    assert_eq!(
        fs::read(&metadata).unwrap(),
        serde_json::to_vec_pretty(&meta).unwrap()
    );
    assert_eq!(
        load_catalog_document_with_meta().unwrap().0["catalog_version"],
        "owned-second"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&current, &lkg, &metadata] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(current.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn physical_catalog_peak_counts_prior_copy_and_metadata_before_publication() {
    let _key = crate::db::owned_mock_bindings_key();
    let _catalog = CATALOG_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let (_root, _path) = owned_catalog_env(root.path());
    let _unsigned = crate::db::TestEnvVarGuard::set("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
    install_catalog_candidate(&tiny_unsigned_catalog("owned-first"), "file://owned-first").unwrap();
    let (_, current, lkg, metadata) = catalog_cache_paths();
    let old_current = fs::read(&current).unwrap();
    let old_metadata = fs::read(&metadata).unwrap();
    assert!(!lkg.exists());
    let new = tiny_unsigned_catalog("owned-second");
    let encoded = serde_json::to_vec_pretty(&new).unwrap();
    let policy = root.path().join(".wptsall-storage-v1.json");
    let write_limit = |limit| {
        let mut encoded = serde_json::to_vec(&json!({
            "format":"wptsall-storage-v1",
            "max_physical_bytes":limit
        }))
        .unwrap();
        assert!(encoded.len() <= 256);
        encoded.resize(256, b' ');
        fs::write(&policy, encoded).unwrap();
    };
    write_limit(crate::storage_capacity::DEFAULT_MAX_PHYSICAL_BYTES);
    let physical = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .physical_retained_bytes;
    // The candidate alone fits. Copying the prior current plus metadata does not.
    write_limit(physical + u64::try_from(encoded.len()).unwrap());
    assert!(install_catalog_candidate(&new, "file://owned-second").is_err());
    assert_eq!(fs::read(current).unwrap(), old_current);
    assert_eq!(fs::read(metadata).unwrap(), old_metadata);
    assert!(!lkg.exists());
}

#[test]
fn physical_catalog_snapshot_authority_change_refuses_all_siblings() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let current = root.path().join("current.json");
    let lkg = root.path().join("lkg.json");
    fs::write(&current, b"changed-by-owned-operator").unwrap();
    fs::write(&lkg, b"keep-original-lkg").unwrap();
    assert!(crate::bindings::atomic_file::install_siblings(
        &current,
        &[
            (&lkg, b"prior-current", Some(b"keep-original-lkg")),
            (&current, b"candidate", Some(b"stale-current"))
        ],
    )
    .is_err());
    assert_eq!(fs::read(current).unwrap(), b"changed-by-owned-operator");
    assert_eq!(fs::read(lkg).unwrap(), b"keep-original-lkg");
}

#[cfg(unix)]
#[test]
fn physical_catalog_snapshot_alias_never_replaces_an_outside_target() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let current = root.path().join("current.json");
    let metadata = root.path().join("metadata.json");
    let target = outside.path().join("retained.json");
    fs::write(&current, b"original-current").unwrap();
    fs::write(&target, b"outside-retained").unwrap();
    std::os::unix::fs::symlink(&target, &metadata).unwrap();
    assert!(crate::bindings::atomic_file::install_siblings(
        &current,
        &[
            (&current, b"candidate", Some(b"original-current")),
            (&metadata, b"new-metadata", Some(b"outside-retained"))
        ],
    )
    .is_err());
    assert_eq!(fs::read(current).unwrap(), b"original-current");
    assert_eq!(fs::read(target).unwrap(), b"outside-retained");
    assert!(fs::symlink_metadata(metadata)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn physical_catalog_corrupt_current_does_not_replace_a_valid_lkg() {
    let _key = crate::db::owned_mock_bindings_key();
    let _catalog = CATALOG_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let (_root, _path) = owned_catalog_env(root.path());
    let _unsigned = crate::db::TestEnvVarGuard::set("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
    let (_, current, lkg, _) = catalog_cache_paths();
    fs::create_dir_all(current.parent().unwrap()).unwrap();
    let retained = serde_json::to_vec_pretty(&tiny_unsigned_catalog("owned-valid-lkg")).unwrap();
    fs::write(&lkg, &retained).unwrap();
    fs::write(&current, b"{owned-corrupt-current").unwrap();
    assert_eq!(
        load_catalog_document_with_meta().unwrap().0["catalog_version"],
        "owned-valid-lkg"
    );
    install_catalog_candidate(&tiny_unsigned_catalog("owned-new"), "file://owned-new").unwrap();
    assert_eq!(
        fs::read(&lkg).unwrap(),
        retained,
        "a corrupt current is not a last-known-good catalog"
    );
    assert_eq!(
        load_catalog_document_with_meta().unwrap().0["catalog_version"],
        "owned-new"
    );
}

#[test]
fn physical_catalog_source_bound_does_not_reject_its_larger_pretty_snapshot() {
    let _key = crate::db::owned_mock_bindings_key();
    let _catalog = CATALOG_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = tempfile::tempdir().unwrap();
    let (_root, _path) = owned_catalog_env(root.path());
    let _unsigned = crate::db::TestEnvVarGuard::set("WPTSALL_ALLOW_UNSIGNED_CATALOG", "1");
    let mut first = tiny_unsigned_catalog("owned-compact-source");
    // Valid extension data stays under the 2 MiB source ceiling but expands
    // beyond it when the existing installer pretty-prints the snapshot.
    first["owned_extension"] = json!(vec![0u8; 350_000]);
    validate_catalog_document(&first, false).unwrap();
    let source = serde_json::to_vec(&first).unwrap();
    let prepared = serde_json::to_vec_pretty(&first).unwrap();
    assert!(source.len() < 2 * 1024 * 1024);
    assert!(prepared.len() > 2 * 1024 * 1024);
    install_catalog_candidate(&first, "file://owned-compact-source").unwrap();
    install_catalog_candidate(&tiny_unsigned_catalog("owned-next"), "file://owned-next").unwrap();
    let (_, current, lkg, _) = catalog_cache_paths();
    assert_eq!(fs::read(lkg).unwrap(), prepared);
    assert_eq!(
        read_validate_catalog_file(&current, false).unwrap()["catalog_version"],
        "owned-next"
    );
}
