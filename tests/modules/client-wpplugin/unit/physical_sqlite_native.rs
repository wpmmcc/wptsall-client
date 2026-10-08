use super::*;

#[test]
fn physical_sqlite_native_readonly_probe_spends_only_prior_database_booking() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("owned.sqlite");
    let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
    conn.execute(
        "INSERT INTO system_config(key,value) VALUES ('owned-reader-receipt','retained')",
        [],
    )
    .unwrap();
    let mut persist = 1i32;
    assert_eq!(
        unsafe {
            ffi::sqlite3_file_control(
                conn.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_PERSIST_WAL,
                (&mut persist as *mut i32).cast(),
            )
        },
        ffi::SQLITE_OK
    );
    drop(conn);
    let shm = root.path().join("owned.sqlite-shm");
    std::fs::remove_file(&shm).unwrap();
    let before = std::fs::read(&path).unwrap();
    let booked = crate::storage_capacity::inventory_at(root.path())
        .unwrap()
        .root_booked_bytes;
    assert!(booked > 32768);
    policy(root.path(), 1);
    let value = crate::db::with_read_only_db(path.to_str().unwrap(), |reader| {
        assert!(reader.is_readonly(rusqlite::DatabaseName::Main)?);
        let value = reader.query_row(
            "SELECT value FROM system_config WHERE key='owned-reader-receipt'",
            [],
            |row| row.get::<_, String>(0),
        )?;
        assert!(reader
            .execute(
                "UPDATE system_config SET value='unowned-write' WHERE key='owned-reader-receipt'",
                [],
            )
            .is_err());
        Ok(value)
    })
    .unwrap();
    assert_eq!(value, "retained");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(shm.metadata().unwrap().len(), 32768);
    assert_eq!(
        crate::storage_capacity::inventory_at(root.path())
            .unwrap()
            .root_booked_bytes,
        booked - 32768,
        "read-only SHM initialization may only debit the prior DB booking"
    );
}

#[test]
fn physical_sqlite_native_nonextending_shm_prepays_only_three_initial_bytes() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("booked.sqlite");
    let credit = crate::storage_capacity::database_recovery_credit(&path, true).unwrap();
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    let before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    policy(root.path(), 1);
    let mut output = ptr::null_mut();
    let code = crate::storage_capacity::with_database_credit(credit, || unsafe {
        ((*(*file.file).pMethods).xShmMap.unwrap())(file.file, 0, 32768, 0, &mut output)
    });
    assert_eq!(code, ffi::SQLITE_OK);
    assert!(
        output.is_null(),
        "nonextending lookup must not map a new page"
    );
    assert_eq!(
        root.path()
            .join("booked.sqlite-shm")
            .metadata()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        before - 3
    );
    assert_eq!(
        unsafe { ((*(*file.file).pMethods).xShmUnmap.unwrap())(file.file, 0) },
        ffi::SQLITE_OK
    );
}

#[test]
fn physical_sqlite_native_nonextending_shm_cannot_initialize_above_full_root() {
    let _key = crate::db::owned_mock_bindings_key();
    for initial in [None, Some(0u64), Some(1u64)] {
        let root = tempfile::Builder::new()
            .prefix("sqlite-nonextending-shm-")
            .tempdir()
            .unwrap()
            .keep();
        let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.to_str().unwrap());
        let path = root.join("raw.sqlite");
        let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
        let shm = root.join("raw.sqlite-shm");
        if let Some(bytes) = initial {
            std::fs::write(&shm, vec![0u8; bytes as usize]).unwrap();
        }
        policy(&root, 1);
        let mut mapped = ptr::null_mut();
        let code = unsafe {
            let methods = &*(*file.file).pMethods;
            let code = methods.xShmMap.unwrap()(file.file, 0, 32768, 0, &mut mapped);
            if code == ffi::SQLITE_OK {
                // Retain the created file for fault evidence, rather than
                // deleting it through the native unmap helper.
                assert_eq!(methods.xShmUnmap.unwrap()(file.file, 0), ffi::SQLITE_OK);
            }
            code
        };
        let after = shm.metadata().ok().map(|metadata| metadata.len());
        assert!(
            code == ffi::SQLITE_FULL && mapped.is_null() && after == initial,
            "nonextending SHM must not create/initialize bytes above quota: \
             root={} initial={initial:?} after={after:?} code={code}",
            root.display()
        );
    }
}

fn policy(root: &Path, max: u64) {
    std::fs::write(
        root.join(".wptsall-storage-v1.json"),
        serde_json::to_vec(
            &serde_json::json!({"format":"wptsall-storage-v1","max_physical_bytes":max}),
        )
        .unwrap(),
    )
    .unwrap();
}

// Exercise the actual registered callbacks and native file implementation.
struct NativeFile {
    file: *mut ffi::sqlite3_file,
    filename: *const c_char,
}

impl NativeFile {
    fn open(path: Option<&Path>, kind: c_int) -> std::result::Result<Self, c_int> {
        register().unwrap();
        unsafe {
            let vfs = ffi::sqlite3_vfs_find(C_NAME.as_ptr().cast());
            let filename = path.map_or(ptr::null(), |path| {
                let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                ffi::sqlite3_create_filename(
                    name.as_ptr(),
                    c"".as_ptr(),
                    c"".as_ptr(),
                    0,
                    ptr::null_mut(),
                )
            });
            let file = ffi::sqlite3_malloc64((*vfs).szOsFile as u64).cast::<ffi::sqlite3_file>();
            assert!(!file.is_null());
            ptr::write_bytes(file.cast::<u8>(), 0, (*vfs).szOsFile as usize);
            let code = (*vfs).xOpen.unwrap()(
                vfs,
                filename,
                file,
                ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | kind,
                ptr::null_mut(),
            );
            if code != ffi::SQLITE_OK {
                if !(*file).pMethods.is_null() {
                    (*(*file).pMethods).xClose.unwrap()(file);
                }
                ffi::sqlite3_free(file.cast());
                ffi::sqlite3_free_filename(filename);
                return Err(code);
            }
            Ok(Self { file, filename })
        }
    }

    fn write(&self, offset: i64, bytes: &[u8]) -> c_int {
        unsafe {
            (*(*self.file).pMethods).xWrite.unwrap()(
                self.file,
                bytes.as_ptr().cast(),
                bytes.len() as c_int,
                offset,
            )
        }
    }

    fn truncate(&self, size: i64) -> c_int {
        unsafe { (*(*self.file).pMethods).xTruncate.unwrap()(self.file, size) }
    }

    fn control<T>(&self, code: c_int, value: &mut T) -> c_int {
        unsafe {
            (*(*self.file).pMethods).xFileControl.unwrap()(
                self.file,
                code,
                (value as *mut T).cast(),
            )
        }
    }
}

impl Drop for NativeFile {
    fn drop(&mut self) {
        unsafe {
            (*(*self.file).pMethods).xClose.unwrap()(self.file);
            ffi::sqlite3_free(self.file.cast());
            ffi::sqlite3_free_filename(self.filename);
        }
    }
}

#[test]
fn physical_sqlite_native_chunk_hint_and_truncate_admit_rounded_growth() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("raw.sqlite");
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    assert_eq!(
        file.control(ffi::SQLITE_FCNTL_CHUNK_SIZE, &mut 4096i32),
        ffi::SQLITE_OK
    );
    policy(root.path(), 1);
    assert_eq!(
        file.control(ffi::SQLITE_FCNTL_SIZE_HINT, &mut 1i64),
        ffi::SQLITE_FULL
    );
    assert_eq!(file.truncate(1), ffi::SQLITE_FULL);
    assert_eq!(path.metadata().unwrap().len(), 0);
    policy(root.path(), 16 * 1024 * 1024);
    assert_eq!(
        file.control(ffi::SQLITE_FCNTL_SIZE_HINT, &mut 1i64),
        ffi::SQLITE_OK
    );
    assert_eq!(path.metadata().unwrap().len(), 4096);
    policy(root.path(), 1);
    assert_eq!(file.write(0, b"owned-in-place"), ffi::SQLITE_OK);
    assert_eq!(file.truncate(0), ffi::SQLITE_OK);
    assert_eq!(path.metadata().unwrap().len(), 0);
}

#[test]
fn physical_sqlite_native_shm_extension_refuses_without_changing_mapping() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("raw.sqlite");
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    unsafe {
        let methods = &*(*file.file).pMethods;
        let mut mapped = ptr::null_mut();
        assert_eq!(
            methods.xShmMap.unwrap()(file.file, 0, 32768, 1, &mut mapped),
            ffi::SQLITE_OK
        );
        assert!(!mapped.is_null());
        let shm = root.path().join("raw.sqlite-shm");
        let before = shm.metadata().unwrap().len();
        policy(root.path(), 1);
        let mut refused = ptr::null_mut();
        assert_eq!(
            methods.xShmMap.unwrap()(file.file, 1, 32768, 1, &mut refused),
            ffi::SQLITE_FULL
        );
        assert!(refused.is_null());
        assert_eq!(shm.metadata().unwrap().len(), before);
        assert_eq!(
            methods.xShmMap.unwrap()(file.file, 0, 32768, 0, &mut refused),
            ffi::SQLITE_OK
        );
        assert_eq!(refused, mapped);
        assert_eq!(methods.xShmUnmap.unwrap()(file.file, 1), ffi::SQLITE_OK);
    }
}

#[test]
fn physical_sqlite_native_named_temporary_cannot_escape_full_root() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    policy(root.path(), 1);
    let path = outside.path().join("owned-external-temp");
    let opened = NativeFile::open(
        Some(&path),
        ffi::SQLITE_OPEN_TEMP_DB | ffi::SQLITE_OPEN_DELETEONCLOSE,
    );
    assert!(
        matches!(opened, Err(ffi::SQLITE_FULL)),
        "named temporary files must not become external quota overrides"
    );
    assert!(!path.exists());
}

#[test]
fn physical_sqlite_native_null_temporary_is_visible_private_and_owned_close_only() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let file = NativeFile::open(
        None,
        ffi::SQLITE_OPEN_TEMP_DB | ffi::SQLITE_OPEN_DELETEONCLOSE,
    )
    .unwrap();
    let path = unsafe { state(file.file).path.clone() };
    assert!(path.starts_with(root.path()));
    assert_eq!(file.write(0, b"owned temporary"), ffi::SQLITE_OK);
    assert!(
        crate::storage_capacity::inventory()
            .unwrap()
            .physical_retained_bytes
            >= 15
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            path.metadata().unwrap().permissions().mode() & 0o777,
            0o600,
            "new SQLite temp bytes must not be world-readable"
        );
    }
    policy(root.path(), 1);
    assert_eq!(file.write(15, b"unadmitted"), ffi::SQLITE_FULL);
    drop(file);
    assert!(!path.exists());
}

#[tokio::test]
async fn physical_sqlite_native_result_asset_credit_cannot_pay_for_unrelated_database() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let db = crate::db::open_db(root.path().join("booked.sqlite").to_str().unwrap()).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    crate::db::capacity::reserve_new(&tx, "owned-unit", 64 * 1024).unwrap();
    tx.commit().unwrap();
    let credit = crate::storage_capacity::result_credit(&db, "owned-unit").unwrap();
    let path = root.path().join("other.sqlite");
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    policy(root.path(), 1);
    let code = crate::storage_capacity::with_result_credit(credit, async {
        file.write(0, b"unrelated database")
    })
    .await;
    assert_eq!(
        code,
        ffi::SQLITE_FULL,
        "generic result asset scope is not a paid SQLite commit scope"
    );
    assert_eq!(path.metadata().unwrap().len(), 0);
}

#[test]
fn physical_sqlite_native_recovery_credit_is_database_specific_and_consumed_exactly() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("booked.sqlite");
    let credit = crate::storage_capacity::database_recovery_credit(&path, true).unwrap();
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    let other = NativeFile::open(
        Some(&root.path().join("other.sqlite")),
        ffi::SQLITE_OPEN_MAIN_DB,
    )
    .unwrap();
    let before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    policy(root.path(), 1);
    crate::storage_capacity::with_database_credit(credit, || {
        assert_eq!(
            other.write(0, b"not this database") & 255,
            ffi::SQLITE_IOERR
        );
        assert_eq!(file.write(0, b"paid"), ffi::SQLITE_OK);
        assert_eq!(file.write(0, b"done"), ffi::SQLITE_OK);
    });
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        before - 4
    );
    assert_eq!(std::fs::read(path).unwrap(), b"done");
}

#[test]
fn physical_sqlite_native_moved_handle_cannot_write_through_unowned_path() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("raw.sqlite");
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    std::fs::rename(&path, root.path().join("owned-original")).unwrap();
    std::fs::write(&path, b"replacement authority").unwrap();
    assert_ne!(
        file.write(0, b"must refuse"),
        ffi::SQLITE_OK,
        "native open inode and admitted destination must remain the same authority"
    );
    assert!(std::fs::read(root.path().join("owned-original"))
        .unwrap()
        .is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement authority");
}

#[test]
fn physical_sqlite_native_process_exit_after_growth_cannot_reuse_original_credit() {
    const CHILD: &str = "WPTSALL_OWNED_SQLITE_NATIVE_CREDIT_EXIT";
    if std::env::var_os(CHILD).is_some() {
        let root = PathBuf::from(std::env::var_os("WPTSALL_DATA_DIR").unwrap());
        let path = root.join("booked.sqlite");
        let credit = crate::storage_capacity::database_recovery_credit(&path, false).unwrap();
        let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
        crate::storage_capacity::with_database_credit(credit, || unsafe {
            let opened = state(file.file);
            growth(opened, 4096, || {
                assert_eq!(
                    (*(*opened.native).pMethods).xWrite.unwrap()(
                        opened.native,
                        [7u8; 4096].as_ptr().cast(),
                        4096,
                        0
                    ),
                    ffi::SQLITE_OK
                );
                // Exact native bytes exist, but no Rust/SQLite destructor or
                // post-write settlement is allowed to run in this child.
                std::process::exit(88);
            });
        });
        panic!("owned growth child unexpectedly returned");
    }
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("booked.sqlite");
    crate::storage_capacity::database_recovery_credit(&path, true).unwrap();
    drop(NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap());
    let before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    policy(root.path(), 1);
    let test = format!(
        "{}::physical_sqlite_native_process_exit_after_growth_cannot_reuse_original_credit",
        module_path!()
            .split("::")
            .skip(1)
            .collect::<Vec<_>>()
            .join("::")
    );
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--test-threads=1"])
        .env(CHILD, "owned-child-only")
        .env("WPTSALL_DATA_DIR", root.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(88));
    assert_eq!(path.metadata().unwrap().len(), 4096);
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        before - 4096,
        "a crashed writer's observed bytes cannot retain reusable original credit"
    );
}

#[test]
fn physical_sqlite_native_short_error_and_panic_keep_admitted_debit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("booked.sqlite");
    let credit = crate::storage_capacity::database_recovery_credit(&path, true).unwrap();
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    let before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    policy(root.path(), 1);
    crate::storage_capacity::with_database_credit(credit, || unsafe {
        let opened = state(file.file);
        assert_eq!(
            growth(opened, 4096, || {
                assert_eq!(
                    (*(*opened.native).pMethods).xWrite.unwrap()(
                        opened.native,
                        b"short".as_ptr().cast(),
                        5,
                        0
                    ),
                    ffi::SQLITE_OK
                );
                ffi::SQLITE_IOERR_WRITE
            }),
            ffi::SQLITE_IOERR_WRITE
        );
        assert_eq!(
            protect(ffi::SQLITE_IOERR_WRITE, || growth(opened, 4101, || {
                panic!("owned callback-local fault, no FFI unwind")
            })),
            ffi::SQLITE_IOERR_WRITE
        );
    });
    assert_eq!(path.metadata().unwrap().len(), 5);
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        before - 8192
    );
}

#[test]
fn physical_sqlite_native_recovery_booking_refills_only_when_new_space_is_admitted() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("booked.sqlite");
    let credit = crate::storage_capacity::database_recovery_credit(&path, true).unwrap();
    let file = NativeFile::open(Some(&path), ffi::SQLITE_OPEN_MAIN_DB).unwrap();
    let initial = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    crate::storage_capacity::with_database_credit(credit, || {
        assert_eq!(file.write(0, &[1; 4096]), ffi::SQLITE_OK);
    });
    policy(root.path(), 1);
    assert!(
        crate::storage_capacity::database_recovery_credit(&path, true)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        initial - 4096
    );
    policy(root.path(), 32 * 1024 * 1024);
    assert!(
        crate::storage_capacity::database_recovery_credit(&path, true)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        initial
    );
    assert_eq!(std::fs::read(path).unwrap(), [1; 4096]);
}

#[test]
fn physical_sqlite_native_concurrent_same_database_booking_opens_do_not_lose_authority() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = root.path().join("booked.sqlite");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let barrier = barrier.clone();
        let path = path.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            crate::db::open_db_with_busy_timeout(
                path.to_str().unwrap(),
                std::time::Duration::from_secs(2),
            )
            .map(|db| {
                db.query_row("SELECT COUNT(*) FROM system_config", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
            })
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert!(
        results.iter().all(|value| value.is_ok()),
        "same original booking is safe under native root locking: {results:?}"
    );
    let inventory = crate::storage_capacity::inventory().unwrap();
    assert!(inventory.root_booked_bytes > 0 && inventory.root_booked_bytes <= 8 * 1024 * 1024);
}

#[test]
fn physical_sqlite_native_external_database_cannot_borrow_root_paid_credit() {
    let _key = crate::db::owned_mock_bindings_key();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let _root = crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", root.path().to_str().unwrap());
    let path = outside.path().join("owned-external.sqlite");
    let db = crate::db::open_db(path.to_str().unwrap()).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    crate::db::capacity::reserve_new(&tx, "owned-external-result", 64 * 1024).unwrap();
    tx.commit().unwrap();
    let before = crate::storage_capacity::inventory()
        .unwrap()
        .root_booked_bytes;
    let credit =
        crate::storage_capacity::database_result_credit(&db, "owned-external-result").unwrap();
    assert!(
        credit.is_none(),
        "external SQLite uses its own file admission, never the asset root's paid booking"
    );
    policy(root.path(), 1);
    crate::storage_capacity::with_database_credit(credit, || {
        crate::db::system::set_system_config(&db, "owned-external-ready", &"r".repeat(16384))
    })
    .unwrap();
    assert_eq!(
        crate::storage_capacity::inventory()
            .unwrap()
            .root_booked_bytes,
        before
    );
}
