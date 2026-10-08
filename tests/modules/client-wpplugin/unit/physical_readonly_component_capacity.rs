use super::*;

#[test]
fn physical_sqlite_startup_missing_component_documents_are_read_only_at_quota() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let component = root.path().join("absent/component-bindings.json");
    let local = root.path().join("absent/components.json");
    let loaded = read_component_bindings(component.to_str().unwrap()).unwrap();
    assert!(loaded.components.is_empty());
    assert!(loaded.version > 0);
    let loaded = read_components_local(local.to_str().unwrap()).unwrap();
    assert!(loaded.components.is_empty());
    assert!(loaded.version > 0);
    assert!(!component.exists() && !local.exists() && !component.parent().unwrap().exists());
}

#[test]
fn physical_sqlite_startup_existing_component_documents_preserve_exact_bytes_at_quota() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let component = root.path().join("component-bindings.json");
    let local = root.path().join("components.json");
    save_encrypted_file(
        &component,
        r#"{"version":2,"components":{"owned":{"name":"owned","auth":{},"template_id":"owned"}}}"#,
    )
    .unwrap();
    save_encrypted_file(&local, r#"{"version":2,"components":{}}"#).unwrap();
    let component_bytes = std::fs::read(&component).unwrap();
    let local_bytes = std::fs::read(&local).unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let loaded = read_component_bindings(component.to_str().unwrap()).unwrap();
    assert_eq!(loaded.components.len(), 1);
    assert_eq!(loaded.components["owned"].name.as_deref(), Some("owned"));
    assert!(read_components_local(local.to_str().unwrap())
        .unwrap()
        .components
        .is_empty());
    assert_eq!(std::fs::read(&component).unwrap(), component_bytes);
    assert_eq!(std::fs::read(&local).unwrap(), local_bytes);
}

#[test]
fn physical_sqlite_startup_damaged_or_nonregular_component_documents_are_not_missing_defaults() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let component = root.path().join("component-bindings.json");
    let local = root.path().join("components.json");
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    for raw in ["{", "null", "[]", "not-encrypted-json"] {
        std::fs::write(&component, raw).unwrap();
        std::fs::write(&local, raw).unwrap();
        assert!(read_component_bindings(component.to_str().unwrap()).is_err());
        assert!(read_components_local(local.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&component).unwrap(), raw);
        assert_eq!(std::fs::read_to_string(&local).unwrap(), raw);
    }
    std::fs::write(&local, b"").unwrap();
    assert!(read_components_local(local.to_str().unwrap()).is_err());
    let directory = root.path().join("owned-directory");
    std::fs::create_dir(&directory).unwrap();
    assert!(read_component_bindings(directory.to_str().unwrap()).is_err());
    assert!(read_components_local(directory.to_str().unwrap()).is_err());
    #[cfg(unix)]
    {
        let link = root.path().join("owned-dangling-link");
        std::os::unix::fs::symlink(root.path().join("missing-owned-target"), &link).unwrap();
        assert!(read_component_bindings(link.to_str().unwrap()).is_err());
        assert!(read_components_local(link.to_str().unwrap()).is_err());
    }
}
