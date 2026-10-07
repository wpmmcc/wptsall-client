//! Install prepared bytes without exposing a partial or public file.
//! Encryption or signature verification belongs to the caller.

use anyhow::{ensure, Context};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) fn install(requested: &Path, encoded: &[u8]) -> anyhow::Result<()> {
    install_snapshot(requested, encoded, false, true)
}

pub(crate) fn install_new(requested: &Path, encoded: &[u8]) -> anyhow::Result<()> {
    install_snapshot(requested, encoded, true, true)
}

pub(crate) fn install_uncredited(requested: &Path, encoded: &[u8]) -> anyhow::Result<()> {
    install_snapshot(requested, encoded, false, false)
}

pub(crate) fn install_new_uncredited(requested: &Path, encoded: &[u8]) -> anyhow::Result<()> {
    install_snapshot(requested, encoded, true, false)
}

pub(crate) fn install_original_file_reply(
    requested: &Path,
    encoded: &[u8],
    credit: Option<crate::storage_capacity::StorageCredit>,
) -> anyhow::Result<()> {
    crate::storage_capacity::with_database_credit(credit, || {
        install_snapshot_with_prepayment(requested, encoded, false, true, true)
    })
}

fn private_directory(parent: &Path) -> anyhow::Result<()> {
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory
        .create(parent)
        .context("create private snapshot directory failed")?;
    Ok(())
}

fn snapshot_parent(requested: &Path) -> &Path {
    requested
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn check_expected(path: &Path, expected: Option<&[u8]>) -> anyhow::Result<()> {
    match (fs::symlink_metadata(path), expected) {
        (Err(error), None) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        (Ok(metadata), Some(bytes))
            if metadata.is_file() && metadata.len() == u64::try_from(bytes.len())? =>
        {
            let mut actual = Vec::new();
            File::open(path)?
                .take(
                    u64::try_from(bytes.len())?
                        .checked_add(1)
                        .context("snapshot size overflow")?,
                )
                .read_to_end(&mut actual)?;
            ensure!(actual == bytes, "snapshot authority changed");
            Ok(())
        }
        _ => anyhow::bail!("snapshot authority changed"),
    }
}

/// Admit the complete staging peak before writing related cache snapshots.
/// Each rename is atomic, but the set is not a filesystem transaction.
pub(crate) fn install_siblings(
    anchor: &Path,
    snapshots: &[(&Path, &[u8], Option<&[u8]>)],
) -> anyhow::Result<()> {
    install_siblings_with_credit(anchor, snapshots, true)
}

pub(crate) fn install_siblings_uncredited(
    anchor: &Path,
    snapshots: &[(&Path, &[u8], Option<&[u8]>)],
) -> anyhow::Result<()> {
    install_siblings_with_credit(anchor, snapshots, false)
}

fn install_siblings_with_credit(
    anchor: &Path,
    snapshots: &[(&Path, &[u8], Option<&[u8]>)],
    allow_credit: bool,
) -> anyhow::Result<()> {
    ensure!(
        !snapshots.is_empty() && snapshots.len() <= 16,
        "invalid snapshot set"
    );
    private_directory(snapshot_parent(anchor))?;
    let parent = fs::canonicalize(snapshot_parent(anchor))?;
    let anchor = parent.join(anchor.file_name().context("snapshot anchor name missing")?);
    if let Ok(metadata) = fs::symlink_metadata(&anchor) {
        ensure!(metadata.is_file(), "snapshot anchor is not a regular file");
    }
    let mut paths = Vec::new();
    let mut peak = 0u64;
    for (requested, bytes, expected) in snapshots {
        ensure!(
            fs::canonicalize(snapshot_parent(requested))? == parent,
            "snapshot set must use the same actual parent"
        );
        let path = parent.join(requested.file_name().context("snapshot name missing")?);
        ensure!(!paths.contains(&path), "duplicate snapshot destination");
        check_expected(&path, *expected)?;
        peak = peak
            .checked_add(u64::try_from(bytes.len())?)
            .context("snapshot peak overflow")?;
        paths.push(path);
    }
    let storage = if allow_credit {
        crate::storage_capacity::StorageLease::for_write(&anchor, peak)?
    } else {
        crate::storage_capacity::StorageLease::for_uncredited_write(&anchor, peak)?
    };
    for (path, (_, bytes, expected)) in paths.iter().zip(snapshots) {
        check_expected(path, *expected)?;
        let retained = expected.map_or(0, |bytes| bytes.len());
        ensure!(
            u64::try_from(retained)?
                .checked_add(u64::try_from(bytes.len())?)
                .is_some_and(|bytes| bytes <= crate::storage_capacity::DEFAULT_MAX_PHYSICAL_BYTES),
            "STORAGE_CAPACITY_EXHAUSTED: sibling snapshot peak exceeds its limit"
        );
    }
    let mut temporary: Vec<PathBuf> = Vec::new();
    let result = (|| {
        for (path, (_, bytes, _)) in paths.iter().zip(snapshots) {
            let name = path
                .file_name()
                .context("snapshot name missing")?
                .to_string_lossy();
            let staged = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&staged)
                .context("create private cache snapshot failed")?;
            temporary.push(staged);
            file.write_all(bytes)
                .context("write cache snapshot failed")?;
            file.sync_all().context("flush cache snapshot failed")?;
        }
        for (staged, (_, bytes, _)) in temporary.iter().zip(snapshots) {
            check_expected(staged, Some(bytes))?;
        }
        for (path, (_, _, expected)) in paths.iter().zip(snapshots) {
            check_expected(path, *expected)?;
        }
        for ((staged, path), (_, bytes, _)) in temporary.iter().zip(&paths).zip(snapshots) {
            fs::rename(staged, path).context("install cache snapshot failed")?;
            check_expected(path, Some(bytes))?;
        }
        #[cfg(unix)]
        File::open(&parent)?
            .sync_all()
            .context("flush cache directory failed")?;
        storage.finish()?;
        Ok(())
    })();
    if result.is_err() {
        for staged in &temporary {
            // Only these invocation-owned create-new files may be removed.
            let _ = fs::remove_file(staged);
        }
    }
    result
}

fn install_snapshot(
    requested: &Path,
    encoded: &[u8],
    create_only: bool,
    allow_credit: bool,
) -> anyhow::Result<()> {
    install_snapshot_with_prepayment(requested, encoded, create_only, allow_credit, false)
}

fn install_snapshot_with_prepayment(
    requested: &Path,
    encoded: &[u8],
    create_only: bool,
    allow_credit: bool,
    prepay: bool,
) -> anyhow::Result<()> {
    let parent = snapshot_parent(requested);
    private_directory(parent)?;
    // Keep configured file aliases, and never replace a dangling link.
    let path = match fs::symlink_metadata(requested) {
        Ok(_) if create_only => anyhow::bail!("encrypted backup already exists"),
        Ok(_) => fs::canonicalize(requested).context("resolve encrypted configuration failed")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::canonicalize(parent)?
            .join(
                requested
                    .file_name()
                    .context("configuration file name missing")?,
            ),
        Err(error) => return Err(error).context("inspect encrypted configuration failed"),
    };
    let parent = path.parent().context("configuration directory missing")?;
    let mut _storage = if allow_credit {
        crate::storage_capacity::StorageLease::for_write(&path, u64::try_from(encoded.len())?)?
    } else {
        crate::storage_capacity::StorageLease::for_uncredited_write(
            &path,
            u64::try_from(encoded.len())?,
        )?
    };
    if prepay {
        _storage.prepay_database_growth()?;
    }
    let name = path
        .file_name()
        .context("configuration file name missing")?
        .to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .context("create private configuration snapshot failed")?;
    let result = (|| {
        file.write_all(encoded)
            .context("write encrypted configuration failed")?;
        file.sync_all()
            .context("flush encrypted configuration failed")?;
        drop(file);
        if create_only {
            // Publish the complete private inode without replacing an existing
            // backup, including one created after the initial path inspection.
            fs::hard_link(&temporary, &path).context("publish encrypted backup failed")?;
            fs::remove_file(&temporary).context("remove owned backup temporary failed")?;
        } else {
            fs::rename(&temporary, &path).context("install encrypted configuration failed")?;
        }
        #[cfg(unix)]
        File::open(parent)?
            .sync_all()
            .context("flush configuration directory failed")?;
        _storage.finish()?;
        Ok(())
    })();
    if result.is_err() {
        // Only remove the unique file just created by this invocation.
        let _ = fs::remove_file(&temporary);
    }
    result
}
