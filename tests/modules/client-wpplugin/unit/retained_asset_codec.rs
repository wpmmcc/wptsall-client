//! Synthetic local files only. Exercise the actual asset codec, not a model.
use super::*;

fn bytes(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| ((index * 31 + index / CHUNK_BYTES) % 251) as u8)
        .collect()
}

#[test]
fn encrypted_spool_legacy_provider_suffix_is_not_an_encryption_marker() {
    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join("00000000-0000-4000-8000-000000000077-original.wpta");
    fs::write(&path, b"owned historical paid content").unwrap();
    assert_eq!(read(&path, 128).unwrap(), b"owned historical paid content");
    assert_eq!(fs::read(&path).unwrap(), b"owned historical paid content");
    let bytes = b"WPTSAS01owned historical binary magic";
    fs::write(&path, bytes).unwrap();
    assert_eq!(read(&path, 128).unwrap(), bytes);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn encrypted_spool_roundtrip_exact_boundaries_and_plaintext_digest() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    for size in [
        0,
        1,
        CHUNK_BYTES - 1,
        CHUNK_BYTES,
        CHUNK_BYTES + 1,
        2 * CHUNK_BYTES + 137,
    ] {
        let path = root.path().join(format!("wpa1{size}.bin"));
        let bytes = bytes(size);
        write_bytes(&path, &bytes).unwrap();
        let original = fs::read(&path).unwrap();
        assert_eq!(original.len() as u64, encoded_size(size as u64).unwrap());
        assert!(original.starts_with(MAGIC));
        let mut reader = AssetReader::open(&path, size as u64).unwrap();
        assert_eq!(reader.len(), size as u64);
        assert_eq!(reader.read_all().unwrap(), bytes);
        assert_eq!(
            reader.digest().unwrap(),
            (crate::sync_engine::hmac::sha256_hex(&bytes), size as u64)
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "read is not a migration"
        );
    }
}

#[test]
fn encrypted_spool_ranges_cross_records_and_keep_final_short_chunk() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    let bytes = bytes(5 * CHUNK_BYTES + 123);
    write_bytes(&path, &bytes).unwrap();
    let mut reader = AssetReader::open(&path, bytes.len() as u64).unwrap();
    for (offset, length) in [
        (0, 5 * CHUNK_BYTES),
        (CHUNK_BYTES - 7, 2 * CHUNK_BYTES + 79),
        (5 * CHUNK_BYTES, 5 * CHUNK_BYTES),
        (bytes.len(), 1),
    ] {
        let end = bytes.len().min(offset + length);
        assert_eq!(
            reader.read_range(offset as u64, length).unwrap(),
            bytes[offset..end]
        );
    }
    assert!(reader.read_range(0, 0).is_err());
    assert!(reader.read_range(0, 5 * CHUNK_BYTES + 1).is_err());
    assert!(reader.read_range(u64::MAX, 1).is_err());
    assert!(AssetReader::open(&path, bytes.len() as u64 - 1).is_err());
}

#[test]
fn encrypted_spool_identical_plaintext_uses_distinct_file_keys_and_records() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let one = root.path().join("one");
    let two = root.path().join("two");
    fs::create_dir(&one).unwrap();
    fs::create_dir(&two).unwrap();
    let one = one.join("wpa1owned.bin");
    let two = two.join("wpa1owned.bin");
    let bytes = b"owned-private-content-marker";
    write_bytes(&one, bytes).unwrap();
    write_bytes(&two, bytes).unwrap();
    let first = fs::read(&one).unwrap();
    let second = fs::read(&two).unwrap();
    assert_ne!(&first[8..24], &second[8..24]);
    assert_ne!(&first[HEADER_BYTES..], &second[HEADER_BYTES..]);
    assert!(!first.windows(bytes.len()).any(|window| window == bytes));
    assert_eq!(read(&one, MAX_BYTES).unwrap(), bytes);
    assert_eq!(read(&two, MAX_BYTES).unwrap(), bytes);
}

#[test]
fn encrypted_spool_wrong_key_refuses_and_retains_original_ciphertext() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    write_bytes(&path, b"owned-paid-private").unwrap();
    let original = fs::read(&path).unwrap();
    {
        let _wrong = crate::db::TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "owned-wrong-spool-key",
        );
        let error = read(&path, MAX_BYTES).unwrap_err();
        let safe = format!("{error:#}");
        assert!(safe.contains("authentication failed"));
        for private in [
            "owned-paid-private",
            "owned-wrong-spool-key",
            path.to_str().unwrap(),
        ] {
            assert!(!safe.contains(private));
        }
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    assert_eq!(read(&path, MAX_BYTES).unwrap(), b"owned-paid-private");
}

#[test]
fn encrypted_spool_missing_key_does_not_create_or_downgrade() {
    const PROBE: &str = "WPTSALL_OWNED_ASSET_NO_KEY_PROBE";
    if let Ok(root) = std::env::var(PROBE) {
        let root = PathBuf::from(root);
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(crate::bindings::bindings_secret().is_none());
        let path = root.join("wpa1missing-key.bin");
        assert!(write_bytes(&path, b"owned no-key").is_err());
        assert!(!path.exists());
        let legacy = root.join("legacy.bin");
        fs::write(&legacy, b"original legacy paid bytes").unwrap();
        let mut source = AssetReader::open(&legacy, MAX_BYTES).unwrap();
        assert_eq!(source.read_all().unwrap(), b"original legacy paid bytes");
        assert!(copy(&path, &mut source).is_err());
        assert!(!path.exists());
        assert!(read(&root.join("wpa1encrypted.bin"), MAX_BYTES).is_err());
        assert_eq!(fs::read(legacy).unwrap(), b"original legacy paid bytes");
        return;
    }
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let encrypted = root.path().join("wpa1encrypted.bin");
    write_bytes(&encrypted, b"owned retained original").unwrap();
    let original = fs::read(&encrypted).unwrap();
    let executable = if cfg!(target_os = "linux") {
        PathBuf::from("/proc/self/exe")
    } else {
        std::env::current_exe().unwrap()
    };
    let output = std::process::Command::new(executable)
        .args([
            "--exact",
            "retained_assets::tests::encrypted_spool_missing_key_does_not_create_or_downgrade",
            "--nocapture",
        ])
        .env(PROBE, root.path())
        .env_remove("WPTSALL_COMPONENT_BINDINGS_SECRET")
        .env_remove("WPTSALL_DEVICE_ID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "owned fresh missing-key worker failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(fs::read(encrypted).unwrap(), original);
}

#[test]
fn encrypted_spool_unfinished_encrypted_copy_is_not_readable_plaintext_or_published() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1source.bin");
    let marker = b"owned-unpublished-private-content";
    write_bytes(&path, marker).unwrap();
    let mut damaged = fs::read(&path).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    fs::write(&path, &damaged).unwrap();
    let output = root.path().join("wpa1unpublished.bin");
    let mut source = AssetReader::open(&path, MAX_BYTES).unwrap();
    assert!(copy(&output, &mut source).is_err());
    assert!(read(&output, MAX_BYTES).is_err());
    let unpublished = fs::read(&output).unwrap();
    assert!(!unpublished
        .windows(marker.len())
        .any(|window| window == marker));
    assert_eq!(fs::read(path).unwrap(), damaged);
}

#[test]
fn encrypted_spool_damaged_magic_metadata_and_records_never_fall_back_to_plaintext() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    write_bytes(&path, &bytes(CHUNK_BYTES + 137)).unwrap();
    let original = fs::read(&path).unwrap();
    for offset in [
        0,
        7,
        8,
        23,
        24,
        HEADER_BYTES - 1,
        HEADER_BYTES,
        original.len() - 1,
    ] {
        let mut damaged = original.clone();
        damaged[offset] ^= 0x40;
        fs::write(&path, &damaged).unwrap();
        assert!(
            read(&path, MAX_BYTES).is_err(),
            "corruption at {offset} was accepted"
        );
        assert_eq!(fs::read(&path).unwrap(), damaged);
    }
    fs::write(&path, original).unwrap();
    assert_eq!(read(&path, MAX_BYTES).unwrap(), bytes(CHUNK_BYTES + 137));
}

#[test]
fn encrypted_spool_record_reorder_and_duplication_fail_authentication() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    write_bytes(&path, &bytes(CHUNK_BYTES * 2 + 77)).unwrap();
    let original = fs::read(&path).unwrap();
    let a = HEADER_BYTES;
    let b = a + CHUNK_BYTES + TAG_BYTES;
    let end = b + CHUNK_BYTES + TAG_BYTES;
    for duplicate in [false, true] {
        let mut damaged = original.clone();
        damaged[a..b].copy_from_slice(&original[b..end]);
        if !duplicate {
            damaged[b..end].copy_from_slice(&original[a..b]);
        }
        fs::write(&path, &damaged).unwrap();
        assert!(read(&path, MAX_BYTES).is_err());
    }
}

#[test]
fn encrypted_spool_truncation_append_and_authenticated_bad_bounds_are_refused() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    write_bytes(&path, b"owned").unwrap();
    let original = fs::read(&path).unwrap();
    for length in [0, 7, HEADER_BYTES - 1, original.len() - 1] {
        fs::write(&path, &original[..length]).unwrap();
        assert!(read(&path, MAX_BYTES).is_err());
    }
    let mut appended = original.clone();
    appended.push(0);
    fs::write(&path, appended).unwrap();
    assert!(read(&path, MAX_BYTES).is_err());
    let id = original[8..24].try_into().unwrap();
    for (size, chunk_size, chunks) in [
        (u64::MAX, CHUNK_BYTES as u32, 1),
        (MAX_BYTES + 1, CHUNK_BYTES as u32, 1),
        (5, 0, 1),
        (5, CHUNK_BYTES as u32, 2),
    ] {
        let mut meta = Vec::new();
        meta.extend_from_slice(&size.to_be_bytes());
        meta.extend_from_slice(&[0; 32]);
        meta.extend_from_slice(&chunk_size.to_be_bytes());
        meta.extend_from_slice(&(chunks as u64).to_be_bytes());
        let metadata = cipher(&id)
            .unwrap()
            .encrypt(
                Nonce::from_slice(&nonce(0)),
                Payload {
                    msg: &meta,
                    aad: &aad(&path, &id).unwrap(),
                },
            )
            .unwrap();
        let mut damaged = original.clone();
        damaged[24..HEADER_BYTES].copy_from_slice(&metadata);
        fs::write(&path, damaged).unwrap();
        assert!(read(&path, MAX_BYTES).is_err());
    }
}

#[test]
fn encrypted_spool_filename_transplant_and_create_new_collision_are_refused() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    write_bytes(&path, b"original paid bytes").unwrap();
    let original = fs::read(&path).unwrap();
    let transplanted = root.path().join("wpa1other.bin");
    fs::write(&transplanted, &original).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&transplanted, fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(read(&transplanted, MAX_BYTES).is_err());
    assert!(write_bytes(&path, b"competitor").is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(write_chunks(
        &root.path().join("wpa1too-large.bin"),
        MAX_BYTES + 1,
        |_| panic!("source must not be read"),
        None
    )
    .is_err());
    assert!(!root.path().join("wpa1too-large.bin").exists());
}

#[cfg(unix)]
#[test]
fn encrypted_spool_private_file_permissions_and_aliases_fail_closed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wpa1owned.bin");
    write_bytes(&path, b"owned").unwrap();
    let alias = root.path().join("wpa1alias.bin");
    symlink(&path, &alias).unwrap();
    assert!(read(&alias, MAX_BYTES).is_err());
    fs::remove_file(&alias).unwrap();
    fs::hard_link(&path, &alias).unwrap();
    assert!(read(&path, MAX_BYTES).is_err());
    fs::remove_file(&alias).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read(&path, MAX_BYTES).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read(&path, MAX_BYTES).unwrap(), b"owned");
}

#[test]
fn encrypted_spool_legacy_and_encrypted_copy_preserve_original_paid_files() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("original.bin");
    let bytes = bytes(CHUNK_BYTES + 17);
    fs::write(&legacy, &bytes).unwrap();
    let old_meta = fs::metadata(&legacy).unwrap();
    let staged = root.path().join("wpa1staged.bin");
    let mut source = AssetReader::open(&legacy, bytes.len() as u64).unwrap();
    copy(&staged, &mut source).unwrap();
    assert_eq!(fs::read(&legacy).unwrap(), bytes);
    assert_eq!(
        fs::metadata(&legacy).unwrap().modified().unwrap(),
        old_meta.modified().unwrap()
    );
    assert_eq!(read(&staged, MAX_BYTES).unwrap(), bytes);
    let original_encrypted = fs::read(&staged).unwrap();
    let next = root.path().join("wpa1next.bin");
    copy(&next, &mut AssetReader::open(&staged, MAX_BYTES).unwrap()).unwrap();
    assert_eq!(read(&next, MAX_BYTES).unwrap(), bytes);
    assert_eq!(fs::read(staged).unwrap(), original_encrypted);
}

#[test]
fn encrypted_spool_legacy_shrink_growth_and_nonregular_reads_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("original.bin");
    for changed in [0, 7] {
        fs::write(&legacy, b"owned").unwrap();
        let mut reader = AssetReader::open(&legacy, 5).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&legacy)
            .unwrap()
            .set_len(changed)
            .unwrap();
        assert!(reader.digest().is_err());
    }
    assert!(AssetReader::open(root.path(), MAX_BYTES).is_err());
    assert!(AssetReader::open(&legacy, 0).is_err());
}
