//! Sync documents: missing is empty, damaged is not. Writers lock the whole
//! read-modify-write operation and install a private, flushed sibling file.

use anyhow::{bail, Context};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

fn validate_shape(value: &serde_json::Value, schema: &str, collection: &str) -> anyhow::Result<()> {
    if value.get("schema_version").and_then(|v| v.as_str()) != Some(schema) {
        bail!("unsupported or missing sync document schema");
    }
    if value.get(collection).is_none() {
        bail!("missing sync document collection");
    }
    Ok(())
}

pub(super) fn load<T: DeserializeOwned + Default>(
    path: &str,
    schema: &str,
    collection: &str,
) -> anyhow::Result<T> {
    let raw = match fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                bail!("unreadable sync document target; original file retained");
            }
            return Ok(T::default());
        }
        Err(err) => return Err(err).context("read sync document failed"),
    };
    let plain = crate::bindings::decrypt_from_bytes(&raw)
        .context("authenticate sync document failed; original file retained")?;
    let value: serde_json::Value = serde_json::from_str(&plain)
        .context("parse sync document failed; original file retained")?;
    validate_shape(&value, schema, collection)?;
    serde_json::from_value(value).context("decode sync document failed; original file retained")
}

fn sibling(path: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let name = path
        .file_name()
        .context("sync document path has no file name")?;
    Ok(path.with_file_name(format!(".{}.{suffix}", name.to_string_lossy())))
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn open_private(path: &Path, unique: bool) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if unique {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .context("open private sync storage file failed")
}

fn with_lock<T>(path: &str, action: impl FnOnce(&Path, &crate::bindings::native_lock::NativeLease) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let requested = Path::new(path);
    fs::create_dir_all(parent(requested)).context("create sync storage directory failed")?;
    // Resolve a configured file symlink before choosing the lock and rename
    // destination. Aliases must not get independent locks or lose the link.
    let resolved = if fs::symlink_metadata(requested).is_ok() {
        fs::canonicalize(requested).context("resolve sync document failed")?
    } else {
        fs::canonicalize(parent(requested))?.join(
            requested
                .file_name()
                .context("sync document path has no file name")?,
        )
    };
    let lock_path = sibling(&resolved, "lock")?;
    // The OS releases this exclusive lock on drop/crash. Unlike a process
    // mutex, it also serializes independent clients using the same path.
    let started = std::time::Instant::now();
    let lease = loop {
        match crate::bindings::native_lock::NativeLease::acquire(&lock_path, "sync document is busy") {
            Ok(lease) => break lease,
            Err(error) if error.to_string() == "sync document is busy" => {
                if started.elapsed() >= std::time::Duration::from_secs(2) {
                    bail!("sync document is busy; retry after the current local operation");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("lock sync document failed"),
        }
    };
    lease.assert_owner()?;
    let result = action(&resolved, &lease)?;
    lease.assert_owner()?;
    Ok(result)
}

fn install(path: &Path, encoded: &[u8]) -> anyhow::Result<()> {
    let sealed = crate::bindings::encrypt_for_save(
        std::str::from_utf8(encoded).context("sync document encoding is invalid")?,
    )?;
    let encoded = sealed.as_slice();
    let storage =
        crate::storage_capacity::StorageLease::for_uncredited_write(path, u64::try_from(encoded.len())?)?;
    let tmp = sibling(path, &format!("{}.tmp", uuid::Uuid::new_v4().simple()))?;
    let mut file = open_private(&tmp, true)?;
    let result = (|| {
        file.write_all(encoded)
            .context("write sync document failed")?;
        file.sync_all().context("flush sync document failed")?;
        drop(file);
        fs::rename(&tmp, path).context("install sync document failed")?;
        #[cfg(unix)]
        File::open(parent(path))?
            .sync_all()
            .context("flush sync storage directory failed")?;
        storage.finish()?;
        Ok(())
    })();
    // This unique temporary belongs to this invocation only. A pre-existing
    // legacy .tmp.json is never reused, truncated, or removed.
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn encode<T: Serialize>(doc: &T, schema: &str, collection: &str) -> anyhow::Result<Vec<u8>> {
    let value = serde_json::to_value(doc).context("encode sync document failed")?;
    validate_shape(&value, schema, collection)?;
    serde_json::to_vec_pretty(&value).context("encode sync document failed")
}

pub(super) fn save<T: Serialize + DeserializeOwned + Default>(
    path: &str,
    doc: &T,
    schema: &str,
    collection: &str,
) -> anyhow::Result<()> {
    with_lock(path, |file_path, lease| {
        // Never let a caller's default document silently replace a bad read.
        let _: T = load(path, schema, collection)?;
        lease.assert_owner()?;
        install(file_path, &encode(doc, schema, collection)?)
    })
}

pub(super) fn update<T: Serialize + DeserializeOwned + Default, R>(
    path: &str,
    schema: &str,
    collection: &str,
    mutate: impl FnOnce(&mut T) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    with_lock(path, |file_path, lease| {
        let mut doc: T = load(path, schema, collection)?;
        let before = encode(&doc, schema, collection)?;
        let output = mutate(&mut doc)?;
        let after = encode(&doc, schema, collection)?;
        if before != after {
            lease.assert_owner()?;
            install(file_path, &after)?;
        }
        Ok(output)
    })
}
