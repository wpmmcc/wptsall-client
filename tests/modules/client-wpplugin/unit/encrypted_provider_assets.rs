//! Owned actual provider result writer; original plaintext bytes stay private.
use super::*;

#[test]
fn encrypted_spool_provider_writer_never_persists_plaintext() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _data =
        crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().display().to_string());
    let bytes = b"owned-provider-spool-private-content";
    let path = persist_downloaded_provider_asset(bytes, Some("owned.pdf")).unwrap();
    let stored = std::fs::read(&path).unwrap();
    assert!(
        !stored.windows(bytes.len()).any(|window| window == bytes),
        "provider-assets must not retain raw paid content"
    );
    assert_eq!(
        crate::retained_assets::read(std::path::Path::new(&path), 128).unwrap(),
        bytes
    );
    assert!(
        crate::retained_assets::filename(std::path::Path::new(&path))
            .unwrap()
            .ends_with(".pdf")
    );
}
