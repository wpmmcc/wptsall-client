//! Owned actual-byte and root-wide admission requirements, not a live inventory.
use super::*;
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, Barrier};

fn root_policy(root: &Path, max_bytes: u64) {
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        serde_json::to_vec(&json!({
            "format": "wptsall-storage-v1",
            "max_physical_bytes": max_bytes,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn memory_database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::schema::create_tables(&conn).unwrap();
    conn
}

#[test]
fn physical_capacity_actual_inventory_counts_ciphertext_copies_and_orphans() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let asset = root.path().join("wpa1owned.bin");
    crate::retained_assets::write_bytes(&asset, &[7; 64]).unwrap();
    let copy = root.path().join("unreferenced-copy.bin");
    std::fs::copy(&asset, &copy).unwrap();
    let partial = root.path().join("unfinished.partial");
    std::fs::write(&partial, [0; 37]).unwrap();
    let physical = 2 * asset.metadata().unwrap().len() + 37;
    let value = serde_json::to_value(inventory(&memory_database()).unwrap()).unwrap();
    assert_eq!(
        value["physical_retained_bytes"].as_u64(),
        Some(physical),
        "actual encrypted copies and unreferenced partials are not zero reservations"
    );
    assert_eq!(value["physical_retained_files"].as_u64(), Some(3));
    assert!(physical > 2 * 64 + 37);
}

#[test]
fn physical_capacity_full_root_refuses_before_new_provider_intent_reservation() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(root.path(), 512);
    let paid = root.path().join("retained-unknown-effect.bin");
    std::fs::write(&paid, [3; 1024]).unwrap();
    let conn = memory_database();
    let tx = conn.unchecked_transaction().unwrap();
    assert!(
        reserve_new(&tx, "owned-new-fee", TEXT_RESERVATION_BYTES).is_err(),
        "a physically full root must fence new fees, not just count provider rows"
    );
    assert_eq!(inventory(&tx).unwrap().retained_units, 0);
    assert_eq!(std::fs::read(&paid).unwrap(), vec![3; 1024]);
}

#[test]
fn physical_capacity_binary_writer_counts_authenticated_header_and_tags_before_create() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(root.path(), 200);
    let asset = root.path().join("wpa1owned.bin");
    assert!(
        crate::retained_assets::write_bytes(&asset, &[7; 100]).is_err(),
        "plaintext length cannot omit authenticated header/tag or existing policy bytes"
    );
    assert!(!asset.exists());
}

#[test]
fn physical_capacity_ordinary_json_writer_refuses_new_snapshot_without_partial_publish() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(root.path(), 512);
    let path = root.path().join("new-paid-snapshot.json");
    let payload = json!({"body": "owned-private-physical-budget".repeat(100)});
    assert!(
        crate::task_engine::pipeline::save_immutable_json(&path, &payload).is_err(),
        "the actual encrypted JSON writer must join root admission"
    );
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn physical_capacity_different_databases_cannot_each_take_the_root_last_slot() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let databases = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(root.path(), TEXT_RESERVATION_BYTES + 1024);
    let mut paths = Vec::new();
    for index in 0..2 {
        let path = databases.path().join(format!("owned-{index}.db"));
        drop(crate::db::open_db(path.to_str().unwrap()).unwrap());
        paths.push(path);
    }
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let conn = Connection::open(path).unwrap();
                let tx = conn.unchecked_transaction().unwrap();
                barrier.wait();
                reserve_new(
                    &tx,
                    &format!("owned-root-fee-{index}"),
                    TEXT_RESERVATION_BYTES,
                )
                .is_ok()
                    && tx.commit().is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>(),
        1,
        "one data root has one last slot across distinct SQLite authorities"
    );
}

#[test]
fn physical_capacity_existing_paid_json_replay_stays_read_only_when_full() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("original-paid.json");
    let payload = json!({"body": "owned-original-retained-result"});
    crate::task_engine::pipeline::save_immutable_json(&path, &payload).unwrap();
    let original = std::fs::read(&path).unwrap();
    root_policy(root.path(), 1);
    crate::task_engine::pipeline::save_immutable_json(&path, &payload).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn physical_capacity_damaged_policy_never_looks_empty_or_allows_a_new_writer() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    for policy in [
        b"not-json".as_slice(),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":0}"#,
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":512,"unknown":1}"#,
    ] {
        std::fs::write(root.path().join(".wptsall-storage-v1.json"), policy).unwrap();
        assert!(inventory(&memory_database()).is_err());
        let path = root.path().join("wpa1refused.bin");
        assert!(crate::retained_assets::write_bytes(&path, b"owned").is_err());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(root.path().join(".wptsall-storage-v1.json")).unwrap(),
            policy
        );
    }
}

#[cfg(unix)]
#[test]
fn physical_capacity_alias_is_not_an_empty_inventory_and_does_not_touch_target() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let target = outside.path().join("owned.txt");
    std::fs::write(&target, b"owned-not-a-capacity-root").unwrap();
    std::os::unix::fs::symlink(&target, root.path().join("alias")).unwrap();
    assert!(inventory(&memory_database()).is_err());
    let path = root.path().join("wpa1refused.bin");
    assert!(crate::retained_assets::write_bytes(&path, b"owned").is_err());
    assert!(!path.exists());
    assert_eq!(std::fs::read(target).unwrap(), b"owned-not-a-capacity-root");
}

#[test]
fn physical_capacity_no_database_writers_share_the_same_last_slot() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(root.path(), 320);
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|index| {
            let path = root.path().join(format!("wpa1owned-{index}.bin"));
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                crate::retained_assets::write_bytes(&path, &[7; 100]).is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert!(
        crate::storage_capacity::inventory()
            .unwrap()
            .physical_retained_bytes
            <= 320
    );
}

#[tokio::test]
async fn physical_capacity_paid_result_consumes_only_its_original_booking_after_limit_lowering() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(root.path(), 2048);
    let conn = memory_database();
    let tx = conn.unchecked_transaction().unwrap();
    reserve_new(&tx, "owned-paid", 1024).unwrap();
    tx.commit().unwrap();
    let before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    let credit = crate::storage_capacity::result_credit(&conn, "owned-paid").unwrap();
    root_policy(root.path(), 1);
    let path = root.path().join("wpa1original-paid.bin");
    crate::storage_capacity::with_result_credit(credit, async {
        crate::retained_assets::write_bytes(&path, &[7; 100])
    })
    .await
    .unwrap();
    assert_eq!(
        crate::retained_assets::read(&path, 100).unwrap(),
        vec![7; 100]
    );
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        before - path.metadata().unwrap().len()
    );
    let new_tx = conn.unchecked_transaction().unwrap();
    assert!(reserve_new(&new_tx, "owned-new-fee", 1).is_err());
    assert_eq!(inventory(&new_tx).unwrap().retained_units, 1);
}

#[tokio::test]
async fn physical_capacity_result_credit_cannot_spend_another_root_or_exceed_booking() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let conn = memory_database();
    let tx = conn.unchecked_transaction().unwrap();
    reserve_new(&tx, "owned-paid", 100).unwrap();
    tx.commit().unwrap();
    let credit = crate::storage_capacity::result_credit(&conn, "owned-paid").unwrap();
    for path in [
        root.path().join("wpa1too-large.bin"),
        outside.path().join("wpa1wrong-root.bin"),
    ] {
        assert!(
            crate::storage_capacity::with_result_credit(credit.clone(), async {
                crate::retained_assets::write_bytes(&path, &[7; 100])
            })
            .await
            .is_err()
        );
        assert!(!path.exists());
    }
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        100
    );
}

#[test]
fn physical_capacity_failed_sqlite_commit_keeps_root_booking_without_age_release() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let conn = memory_database();
    {
        let tx = conn.unchecked_transaction().unwrap();
        reserve_new(&tx, "owned-unconfirmed", 1024).unwrap();
    }
    assert_eq!(inventory(&conn).unwrap().retained_units, 0);
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        1024
    );
    let retained: Vec<_> = std::fs::read_dir(root.path().join(".wptsall-storage-bookings-v1"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(retained.len(), 1);
    let before = std::fs::read(&retained[0]).unwrap();
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        1024
    );
    assert_eq!(std::fs::read(&retained[0]).unwrap(), before);
}

#[test]
#[ignore = "owned storage lease child, started by the actual process contract"]
fn physical_capacity_process_child() {
    if std::env::var("WPTSALL_PHYSICAL_CAPACITY_CHILD").as_deref() != Ok("owned") {
        return;
    }
    let root = std::path::PathBuf::from(std::env::var("WPTSALL_DATA_DIR").unwrap());
    let _lease = crate::storage_capacity::StorageLease::acquire(&root, 0).unwrap();
    std::fs::write(root.join("owned-child-ready"), b"owned-ready").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(20));
}

#[test]
fn physical_capacity_actual_child_death_releases_only_liveness_and_retains_bytes() {
    use std::process::{Command, Stdio};
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let asset = root.path().join("wpa1original.bin");
    crate::retained_assets::write_bytes(&asset, b"owned-paid-before-child").unwrap();
    let original = std::fs::read(&asset).unwrap();
    let conn = memory_database();
    let tx = conn.unchecked_transaction().unwrap();
    reserve_new(&tx, "owned-unfinished-child", 1024).unwrap();
    tx.commit().unwrap();
    let booking = std::fs::read_dir(root.path().join(".wptsall-storage-bookings-v1"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original_booking = std::fs::read(&booking).unwrap();
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("db::capacity::physical_retained_capacity::physical_capacity_process_child")
            .arg("--ignored")
            .env("WPTSALL_PHYSICAL_CAPACITY_CHILD", "owned")
            .env("WPTSALL_DATA_DIR", root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !root.path().join("owned-child-ready").exists() {
        if child.0.try_wait().unwrap().is_some() || std::time::Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            panic!("owned capacity child did not acquire its lease");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(crate::storage_capacity::StorageLease::acquire(root.path(), 0).is_err());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let _lease = crate::storage_capacity::StorageLease::acquire(root.path(), 0).unwrap();
    assert_eq!(std::fs::read(asset).unwrap(), original);
    assert_eq!(std::fs::read(booking).unwrap(), original_booking);
    assert!(root.path().join(".wptsall-storage-v1.lock").exists());
}

#[test]
fn physical_capacity_dot_segments_cannot_select_an_unrelated_budget() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":2000}"#,
    )
    .unwrap();
    std::fs::write(
        outside.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    let path = root
        .path()
        .join("..")
        .join(outside.path().file_name().unwrap())
        .join("wpa1outside.bin");
    assert!(crate::retained_assets::write_bytes(&path, b"owned-outside").is_err());
    assert!(!outside.path().join("wpa1outside.bin").exists());
}

#[test]
fn physical_capacity_remaining_media_checkpoint_writer_retains_original_when_full() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let store = crate::component_rt::non_text::recovery_store::RecoveryStore::configured().unwrap();
    store
        .update::<serde_json::Value, ()>("owned-paid", |_| {
            Ok((serde_json::json!({"paid":"original"}), ()))
        })
        .unwrap();
    let directory = root.path().join("media-recovery");
    let receipt = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "receipt")
        })
        .unwrap();
    let before = std::fs::read(&receipt).unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    assert!(store
        .update::<serde_json::Value, ()>("owned-paid", |_| {
            Ok((serde_json::json!({"paid":"replacement"}), ()))
        })
        .is_err());
    assert_eq!(std::fs::read(receipt).unwrap(), before);
}

#[test]
fn physical_capacity_remaining_sync_checkpoint_writer_retains_original_when_full() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("sync-state.json");
    crate::sync_engine::state::save_sync_state(
        path.to_str().unwrap(),
        &crate::sync_engine::state::SyncStateDoc::default(),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    std::fs::write(
        root.path().join(".wptsall-storage-v1.json"),
        br#"{"format":"wptsall-storage-v1","max_physical_bytes":1}"#,
    )
    .unwrap();
    assert!(crate::sync_engine::state::update_pair_state(
        path.to_str().unwrap(),
        "owned-pair",
        |pair| { pair.last_seen_source_id = 10 }
    )
    .is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn physical_capacity_external_file_does_not_claim_an_unconfigured_shared_parent() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let marker = unrelated.path().join("owned-unrelated.txt");
    std::fs::write(&marker, b"not-part-of-file-scope").unwrap();
    std::os::unix::fs::symlink(&marker, outside.path().join("unrelated-alias")).unwrap();
    let path = outside.path().join("owned-config.json");
    crate::bindings::save_encrypted_file(&path, r#"{"owned":true}"#).unwrap();
    assert_eq!(
        crate::bindings::load_encrypted_or_plain(&path).unwrap(),
        r#"{"owned":true}"#
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"not-part-of-file-scope");
    let control = std::fs::read_dir(outside.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".wptsall-storage-file-v1-")
        })
        .unwrap();
    assert_eq!(std::fs::read(&control).unwrap(), b"");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(control).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!outside.path().join(".wptsall-storage-v1.lock").exists());
}

#[test]
fn physical_capacity_external_file_honors_an_explicit_ancestor_root_policy() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    root_policy(outside.path(), 1);
    let directory = outside.path().join("owned-child");
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("wpa1refused.bin");
    assert!(crate::retained_assets::write_bytes(&path, b"owned-under-policy").is_err());
    assert!(!path.exists());
}

#[test]
fn physical_capacity_external_file_scope_still_checks_existing_and_new_peak_bytes() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = outside.path().join("original-owned.bytes");
    std::fs::write(&path, b"original-retained").unwrap();
    assert!(crate::storage_capacity::StorageLease::for_write(
        &path,
        crate::storage_capacity::DEFAULT_MAX_PHYSICAL_BYTES
    )
    .is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"original-retained");
}

#[cfg(unix)]
#[test]
fn physical_capacity_external_alias_cannot_evade_the_target_root_policy() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let target = root.path().join("owned-retained.json");
    std::fs::write(&target, b"retained-original").unwrap();
    root_policy(root.path(), 1);
    let alias = outside.path().join("owned-alias.json");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    assert!(crate::storage_capacity::StorageLease::for_write(&alias, 1).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"retained-original");
    assert!(std::fs::symlink_metadata(alias)
        .unwrap()
        .file_type()
        .is_symlink());
}
