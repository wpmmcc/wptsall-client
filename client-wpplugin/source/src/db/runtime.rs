//! One runtime owns a persistent database. Ordinary readers never acquire this
//! lease or run recovery. OS locks survive task aborts and vanish on process exit.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

pub(crate) struct RuntimeLease {
    _file: Option<File>,
    path: Option<PathBuf>,
}

fn leases() -> &'static Mutex<HashMap<PathBuf, Weak<RuntimeLease>>> {
    static LEASES: OnceLock<Mutex<HashMap<PathBuf, Weak<RuntimeLease>>>> = OnceLock::new();
    LEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn database_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path.canonicalize().context("resolve runtime database path");
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let name = path
        .file_name()
        .context("runtime database path has no filename")?;
    Ok(parent.join(name))
}

impl RuntimeLease {
    /// Called once at a runtime entry point, before opening or migrating the DB.
    /// Another runtime, even in the same process, must fail rather than sweep
    /// work it does not own. The lock file remains; deleting it would split locks.
    pub(crate) fn acquire(path: &str) -> Result<Arc<Self>> {
        if path == ":memory:" {
            return Ok(Arc::new(Self {
                _file: None,
                path: None,
            }));
        }
        let path = database_path(Path::new(path))?;
        let mut filename = path.file_name().unwrap().to_os_string();
        filename.push(".runtime.lock");
        let lock_path = path.with_file_name(filename);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&lock_path)
            .context("open client runtime lease")?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!("CLIENT_RUNTIME_ALREADY_RUNNING: this database already belongs to an active client");
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).context("lock client runtime database");
            }
        }
        let lease = Arc::new(Self {
            _file: Some(file),
            path: Some(path.clone()),
        });
        let mut registry = leases().lock().unwrap_or_else(|error| error.into_inner());
        registry.retain(|_, lease| lease.strong_count() > 0);
        registry.insert(path, Arc::downgrade(&lease));
        Ok(lease)
    }

    /// Spawned descendants keep the lease until they actually finish, including
    /// the short cancellation window after their parent future has been dropped.
    /// This lookup neither acquires a new OS lock nor runs another boot sweep.
    pub(crate) fn for_connection(conn: &Connection) -> Option<Arc<Self>> {
        let path = Path::new(conn.path()?);
        let path = path.canonicalize().ok()?;
        leases()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&path)
            .and_then(Weak::upgrade)
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct RecoveryReport {
    pub(crate) reset_items: usize,
    pub(crate) finalized_jobs: usize,
    pub(crate) released_claims: usize,
}

/// Only the lease owner may recover. All rows were orphaned by an earlier
/// runtime, not by a wall-clock age guess. Paid artifacts and async jobs stay.
pub(crate) fn recover_interrupted_work(
    conn: &Connection,
    lease: &RuntimeLease,
) -> Result<RecoveryReport> {
    let path = conn
        .path()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .map(Path::canonicalize)
        .transpose()?;
    if path != lease.path {
        bail!("runtime lease does not own this database");
    }
    let transaction = conn.unchecked_transaction()?;
    let ids = {
        let mut statement = transaction.prepare(
            "SELECT id FROM translation_jobs WHERE status = 'running'
             OR id IN (SELECT job_id FROM translation_items
                       WHERE status IN ('fetching', 'fetched', 'translating', 'syncing'))",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let reset_items = transaction.execute(
        "UPDATE translation_items SET
             status = CASE WHEN TRIM(translated_path) != '' THEN 'translated' ELSE 'pending' END,
             updated_at = ?1
         WHERE status IN ('fetching', 'fetched', 'translating', 'syncing')",
        params![crate::logging::unix_ts() as i64],
    )?;
    for id in &ids {
        crate::db::jobs::project_job_from_items(&transaction, *id, true, "failed")?;
    }
    let released_claims = transaction.execute("DELETE FROM translation_in_progress", [])?;
    transaction.commit()?;
    let report = RecoveryReport {
        reset_items,
        finalized_jobs: ids.len(),
        released_claims,
    };
    if report.reset_items > 0 || report.finalized_jobs > 0 || report.released_claims > 0 {
        crate::logging::log_event_global(
            "info",
            "runtime.interrupted_work_recovered",
            serde_json::to_value(&report)?,
        );
    }
    Ok(report)
}
#[cfg(test)]
#[path = "tests/runtime.rs"]
mod tests;