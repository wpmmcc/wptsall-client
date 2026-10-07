use super::*;
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    name: String,
    prior_sha256: Option<String>,
    next_base64: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    format: String,
    anchor: String,
    targets: Vec<Target>,
}

fn journal_path(anchor: &Path) -> PathBuf {
    anchor.with_extension("publish-journal.json")
}

pub(super) fn acquire(anchor: &Path) -> anyhow::Result<crate::bindings::native_lock::NativeLease> {
    let parent = anchor.parent().context("catalog parent is missing")?;
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(parent)?;
    crate::bindings::native_lock::NativeLease::acquire(
        &anchor.with_extension("publish.lock"),
        "catalog publication is active; previous cache retained",
    )
}

pub(super) fn recover(
    anchor: &Path,
    lease: &crate::bindings::native_lock::NativeLease,
) -> anyhow::Result<()> {
    use std::io::Read;
    lease.assert_owner()?;
    let path = journal_path(anchor);
    let metadata = match path.symlink_metadata() {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    ensure!(metadata.file_type().is_file() && metadata.len() <= MAX_JOURNAL_BYTES,
        "catalog publication journal is invalid; retained");
    let mut raw = Vec::new();
    fs::File::open(&path)?.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut raw)?;
    ensure!(raw.len() as u64 == metadata.len(), "catalog publication journal changed; retained");
    let journal: Journal = serde_json::from_slice(&raw).context("damaged catalog publication journal; retained")?;
    let parent = fs::canonicalize(anchor.parent().context("catalog parent is missing")?)?;
    let canonical = parent.join(anchor.file_name().context("catalog filename missing")?);
    ensure!(journal.format == "catalog-publication-v1"
        && journal.anchor == canonical.to_str().context("catalog path is not UTF-8")?
        && (1..=3).contains(&journal.targets.len()),
        "catalog publication journal scope differs; retained");
    let (_, current, lkg, metadata_path) = catalog_cache_paths();
    let allowed = [current, lkg, metadata_path];
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for target in journal.targets {
        let name = Path::new(&target.name);
        ensure!(name.components().count() == 1 && name.file_name() == Some(name.as_os_str())
            && seen.insert(target.name.clone()),
            "catalog publication target is invalid; retained");
        let target_path = parent.join(name);
        ensure!(allowed.iter().any(|value| value.file_name() == name.file_name()),
            "catalog publication target is foreign; retained");
        ensure!(target.next_base64.len() as u64 <= MAX_CATALOG_CACHE_BYTES.div_ceil(3) * 4,
            "catalog publication target encoding exceeds its bound");
        let next = BASE64_STANDARD.decode(&target.next_base64)?;
        ensure!(next.len() as u64 <= MAX_CATALOG_CACHE_BYTES, "catalog publication target exceeds its bound");
        let prior = read_cache_snapshot(&target_path)?;
        ensure!(prior.as_ref().map(|bytes| hex_sha256(bytes)) == target.prior_sha256
            || prior.as_deref() == Some(next.as_slice()),
            "catalog publication authority changed; newer cache retained");
        values.push((target_path, next, prior));
    }
    let pending = values.iter().filter(|(_, next, prior)| prior.as_deref() != Some(next.as_slice()))
        .map(|(path, next, prior)| (path.as_path(), next.as_slice(), prior.as_deref())).collect::<Vec<_>>();
    lease.assert_owner()?;
    if !pending.is_empty() {
        crate::bindings::atomic_file::install_siblings_uncredited(anchor, &pending)?;
    }
    ensure!(fs::read(&path)? == raw, "catalog publication journal authority changed; retained");
    for (target, next, _) in &values {
        ensure!(read_cache_snapshot(target)?.as_deref() == Some(next.as_slice()),
            "catalog publication projection differs; journal retained");
    }
    lease.assert_owner()?;
    fs::remove_file(&path)?;
    #[cfg(unix)]
    fs::File::open(&parent)?.sync_all()?;
    Ok(())
}

pub(super) fn publish(
    anchor: &Path,
    snapshots: &[(&Path, &[u8], Option<&[u8]>)],
    lease: &crate::bindings::native_lock::NativeLease,
) -> anyhow::Result<()> {
    recover(anchor, lease)?;
    let parent = fs::canonicalize(anchor.parent().context("catalog parent missing")?)?;
    let canonical = parent.join(anchor.file_name().context("catalog filename missing")?);
    let targets = snapshots.iter().map(|(path, next, prior)| {
        ensure!(read_cache_snapshot(path)?.as_deref() == *prior,
            "catalog publication authority changed; retained");
        Ok(Target {
            name: path.file_name().and_then(|name| name.to_str()).context("catalog target filename is invalid")?.into(),
            prior_sha256: prior.map(hex_sha256),
            next_base64: BASE64_STANDARD.encode(next),
        })
    }).collect::<anyhow::Result<Vec<_>>>()?;
    let encoded = serde_json::to_vec(&Journal {
        format: "catalog-publication-v1".into(),
        anchor: canonical.to_str().context("catalog path is not UTF-8")?.into(),
        targets,
    })?;
    ensure!(encoded.len() as u64 <= MAX_JOURNAL_BYTES, "catalog publication journal exceeds its bound");
    let peak = snapshots.iter().try_fold(encoded.len() as u64, |sum, (_, bytes, _)| {
        sum.checked_add(bytes.len() as u64).context("catalog publication peak overflow")
    })?;
    // Check the full peak before publishing any recoverable authority. Each
    // actual write independently re-admits, without borrowing paid DB credit.
    drop(crate::storage_capacity::StorageLease::for_uncredited_write(anchor, peak)?);
    lease.assert_owner()?;
    crate::bindings::atomic_file::install_new_uncredited(&journal_path(anchor), &encoded)?;
    recover(anchor, lease)
}
