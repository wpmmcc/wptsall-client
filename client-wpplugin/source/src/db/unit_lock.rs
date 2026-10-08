//! Cooperative per-unit liveness shared by runners, review and delivery.
//! Lock files are never unlinked and record age never authorizes takeover.
use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

enum Owner {
    Persistent {
        _file: File,
        lock: PathBuf,
        identity: [u64; 2],
    },
    Memory {
        _owner: Arc<()>,
    },
}

fn native_identity(file: &File) -> Result<[u64; 2]> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "unit lock is not a regular file");
        Ok([metadata.dev(), metadata.ino()])
    }
    #[cfg(windows)]
    {
        use std::ffi::{c_int, c_void};
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        struct Information {
            attributes: u32,
            created: [u32; 2],
            accessed: [u32; 2],
            modified: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileInformationByHandle(handle: *mut c_void, result: *mut Information) -> c_int;
        }
        let mut information = std::mem::MaybeUninit::<Information>::uninit();
        ensure!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
                != 0,
            "inspect native unit lock identity"
        );
        let information = unsafe { information.assume_init() };
        Ok([
            information.volume as u64,
            ((information.index_high as u64) << 32) | information.index_low as u64,
        ])
    }
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("native unit lock identity is unavailable")
}

type Claims = HashMap<(usize, String), Weak<()>>;

fn claims() -> &'static Mutex<Claims> {
    static CLAIMS: OnceLock<Mutex<Claims>> = OnceLock::new();
    CLAIMS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct UnitLease {
    path: Option<PathBuf>,
    memory: usize,
    suffix: String,
    item_claim: Option<(String, String)>,
    content_claim: Option<(String, String)>,
    _owner: Owner,
    _runtime: Option<Arc<super::runtime::RuntimeLease>>,
    _content: Option<Arc<UnitLease>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemClaim {
    format: String,
    item_id: i64,
    owner: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContentClaim {
    format: String,
    suffix: String,
    owner: String,
}

pub(crate) struct ContentExecution {
    _root: Arc<UnitLease>,
    items: Vec<UnitLease>,
}

impl ContentExecution {
    pub(crate) async fn acquire(
        db: &Arc<tokio::sync::Mutex<Connection>>,
        domain: &str,
        relation: i64,
        object_type: &str,
        object_id: i64,
    ) -> Result<Arc<Self>> {
        let root = UnitLease::content(db, domain, relation, object_type, object_id).await?;
        let ids = {
            let conn = db.lock().await;
            let mut stmt = conn.prepare(
                "SELECT id FROM translation_items
                WHERE domain=?1 AND relation_id=?2 AND wp_object_id=?3 AND object_type IN (?4,?5)",
            )?;
            let canonical = super::pending_callbacks::normalize_pending_object_type(object_type)?;
            let alias = match canonical {
                "post_type" => "post",
                "taxonomy" => "term",
                other => other,
            };
            let rows = stmt.query_map(
                params![domain, relation, object_id, canonical, alias],
                |row| row.get::<_, i64>(0),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut items = Vec::new();
        for id in ids {
            items.push(UnitLease::item_with_content(db, id, Some(root.clone())).await?);
        }
        Ok(Arc::new(Self { _root: root, items }))
    }

    pub(crate) fn assert_owner(
        &self,
        conn: &Connection,
        db: &Arc<tokio::sync::Mutex<Connection>>,
    ) -> Result<()> {
        self._root.assert_content(conn, db)?;
        for lease in &self.items {
            let id = lease
                .suffix
                .strip_prefix("review-")
                .context("invalid item authority")?
                .parse()?;
            lease.assert_item(conn, db, id)?;
        }
        Ok(())
    }

    pub(crate) async fn mutate<R>(
        &self,
        db: &Arc<tokio::sync::Mutex<Connection>>,
        change: impl FnOnce(&Connection) -> Result<R>,
    ) -> Result<R> {
        let mut conn = db.lock().await;
        let tx = conn.savepoint()?;
        self.assert_owner(&tx, db)?;
        let result = change(&tx)?;
        self.assert_owner(&tx, db)?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
#[path = "../../../../tests/modules/client-wpplugin/unit/physical_unit_reclaim_capacity.rs"]
mod physical_unit_reclaim_capacity;

impl UnitLease {
    pub(crate) fn acquire(
        conn: &Connection,
        db: &Arc<tokio::sync::Mutex<Connection>>,
        suffix: &str,
        busy: &'static str,
    ) -> Result<Self> {
        ensure!(
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
            "invalid unit lock namespace"
        );
        let path = conn
            .path()
            .filter(|p| !p.is_empty())
            .map(|p| Path::new(p).canonicalize())
            .transpose()?;
        let memory = Arc::as_ptr(db) as usize;
        let owner = if let Some(path) = &path {
            let mut name = path
                .file_name()
                .context("unit database has no filename")?
                .to_os_string();
            name.push(format!(".{suffix}.lock"));
            let lock = path.with_file_name(name);
            match lock.symlink_metadata() {
                Ok(meta) => ensure!(
                    meta.file_type().is_file(),
                    "unit lock is not a regular file"
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("inspect unit lock"),
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&lock).context("open unit lock")?;
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!("{busy}"),
                Err(std::fs::TryLockError::Error(error)) => return Err(error).context("lock unit"),
            }
            let identity = native_identity(&file)?;
            ensure!(
                lock.symlink_metadata()?.file_type().is_file()
                    && native_identity(&File::open(&lock)?)? == identity,
                "unit lock path changed; retained"
            );
            Owner::Persistent {
                _file: file,
                lock,
                identity,
            }
        } else {
            let mut registry = claims().lock().unwrap_or_else(|error| error.into_inner());
            registry.retain(|_, owner| owner.strong_count() > 0);
            let key = (memory, suffix.to_string());
            ensure!(
                registry.get(&key).and_then(Weak::upgrade).is_none(),
                "{busy}"
            );
            let owner = Arc::new(());
            registry.insert(key, Arc::downgrade(&owner));
            Owner::Memory { _owner: owner }
        };
        Ok(Self {
            path,
            memory,
            suffix: suffix.to_string(),
            item_claim: None,
            _owner: owner,
            content_claim: None,
            _runtime: super::runtime::RuntimeLease::for_connection(conn),
            _content: None,
        })
    }

    pub(crate) async fn item(db: &Arc<tokio::sync::Mutex<Connection>>, id: i64) -> Result<Self> {
        Self::item_with_content(db, id, None).await
    }

    pub(crate) fn assert_native_owner(&self) -> Result<()> {
        match &self._owner {
            Owner::Persistent {
                _file,
                lock,
                identity,
            } => {
                ensure!(
                    native_identity(_file)? == *identity
                        && lock.symlink_metadata()?.file_type().is_file()
                        && native_identity(&File::open(lock)?)? == *identity,
                    "unit lock path changed; original evidence retained"
                );
            }
            Owner::Memory { _owner } => {
                let registry = claims().lock().unwrap_or_else(|error| error.into_inner());
                ensure!(
                    registry
                        .get(&(self.memory, self.suffix.clone()))
                        .and_then(Weak::upgrade)
                        .is_some_and(|owner| Arc::ptr_eq(&owner, _owner)),
                    "unit memory owner changed; original evidence retained"
                );
            }
        }
        Ok(())
    }

    pub(crate) fn content_suffix(
        domain: &str,
        relation_id: i64,
        object_type: &str,
        object_id: i64,
    ) -> Result<String> {
        let mut site = url::Url::parse(domain).context("invalid content site")?;
        ensure!(
            matches!(site.scheme(), "http" | "https")
                && site.username().is_empty()
                && site.password().is_none()
                && site.query().is_none()
                && site.fragment().is_none(),
            "invalid content site"
        );
        let path = site
            .path()
            .split("/wp-json/")
            .next()
            .unwrap_or("/")
            .trim_end_matches('/')
            .to_string();
        site.set_path(&format!("{path}/"));
        Ok(format!(
            "content-{}",
            super::system::private_json_digest(&serde_json::json!({
                "site":site.as_str(), "relation":relation_id,
                "object_type":super::pending_callbacks::normalize_pending_object_type(object_type)?,
                "object_id":object_id,
            }))?
        ))
    }

    pub(crate) async fn content(
        db: &Arc<tokio::sync::Mutex<Connection>>,
        domain: &str,
        relation_id: i64,
        object_type: &str,
        object_id: i64,
    ) -> Result<Arc<Self>> {
        let suffix = Self::content_suffix(domain, relation_id, object_type, object_id)?;
        let mut conn = db.lock().await;
        let mut lease = Self::acquire(&conn, db, &suffix, "REVIEW_ITEM_BUSY: content is active")?;
        crate::storage_capacity::with_database_credit(None, || -> Result<()> {
            let tx = conn.savepoint()?;
            lease.claim_content(&tx)?;
            lease.assert_content(&tx, db)?;
            tx.commit()?;
            Ok(())
        })?;
        Ok(Arc::new(lease))
    }

    fn claim_content(&mut self, conn: &Connection) -> Result<()> {
        self.assert_native_owner()?;
        let suffix = &self.suffix;
        let key = format!("content-execution-claim-v1:{suffix}");
        let prior = super::system::get_system_config_checked(conn, &key)?;
        if let Some(raw) = &prior {
            ensure!(
                raw.starts_with("V1BUQw"),
                "content claim is not encrypted; retained"
            );
            let saved: ContentClaim =
                serde_json::from_str(&super::system::decrypt_config_value(raw)?)
                    .context("damaged content claim; retained")?;
            ensure!(
                saved.format == "content-execution-claim-v1"
                    && saved.suffix == *suffix
                    && uuid::Uuid::parse_str(&saved.owner)
                        .is_ok_and(|owner| !owner.is_nil() && owner.to_string() == saved.owner),
                "content claim scope differs; retained"
            );
            // The native lock, not a fresh UUID write, proves current liveness.
            self.content_claim = Some((key, raw.clone()));
            return Ok(());
        }
        let raw = super::system::encrypt_config_value(&serde_json::to_string(&ContentClaim {
            format: "content-execution-claim-v1".into(),
            suffix: suffix.clone(),
            owner: uuid::Uuid::new_v4().to_string(),
        })?)?;
        let changed = match prior {
            Some(prior) => conn.execute(
                "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                params![raw, key, prior],
            )?,
            None => conn.execute(
                "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                params![key, raw],
            )?,
        };
        ensure!(
            changed == 1
                && super::system::get_system_config_checked(conn, &key)?.as_deref()
                    == Some(raw.as_str()),
            "content claim was not committed; retained"
        );
        self.content_claim = Some((key, raw));
        Ok(())
    }

    fn assert_content(
        &self,
        conn: &Connection,
        db: &Arc<tokio::sync::Mutex<Connection>>,
    ) -> Result<()> {
        self.assert_native_owner()?;
        let path = conn
            .path()
            .filter(|path| !path.is_empty())
            .map(|path| Path::new(path).canonicalize())
            .transpose()?;
        ensure!(
            self.suffix.starts_with("content-")
                && self.path == path
                && (path.is_some() || self.memory == Arc::as_ptr(db) as usize),
            "content execution authority does not own this database"
        );
        let (key, raw) = self
            .content_claim
            .as_ref()
            .context("content owner claim missing")?;
        ensure!(
            super::system::get_system_config_checked(conn, key)?.as_deref() == Some(raw.as_str()),
            "content execution owner changed; original evidence retained"
        );
        Ok(())
    }

    pub(crate) async fn item_with_content(
        db: &Arc<tokio::sync::Mutex<Connection>>,
        id: i64,
        parent: Option<Arc<Self>>,
    ) -> Result<Self> {
        ensure!(id > 0, "invalid review item identity");
        let mut conn = db.lock().await;
        let mut lease = Self::acquire(&conn, db, &format!("review-{id}"), "REVIEW_ITEM_BUSY")?;
        crate::storage_capacity::with_database_credit(None, || -> Result<()> {
            let tx = conn.savepoint()?;
            if let Some(item) = super::jobs::get_item_checked(&tx, id)? {
                let domain = if url::Url::parse(&item.domain).is_ok() {
                    item.domain.clone()
                } else {
                    super::jobs::get_job_checked(&tx, item.job_id)?
                        .context("legacy item owner job missing")?
                        .domain
                };
                let suffix = Self::content_suffix(
                    &domain,
                    item.relation_id,
                    &item.object_type,
                    item.wp_object_id,
                )?;
                lease._content = Some(match parent {
                    Some(parent) => {
                        ensure!(
                            parent.suffix == suffix
                                && parent.path == lease.path
                                && (parent.path.is_some() || parent.memory == lease.memory),
                            "content authority differs from review item"
                        );
                        parent
                    }
                    None => {
                        let mut parent =
                            Self::acquire(&tx, db, &suffix, "REVIEW_ITEM_BUSY: content is active")?;
                        parent.claim_content(&tx)?;
                        Arc::new(parent)
                    }
                });
            }
            let key = format!("review-execution-claim-v1:{id}");
            let prior = super::system::get_system_config_checked(&tx, &key)?;
            if let Some(prior) = &prior {
                ensure!(
                    prior.starts_with("V1BUQw"),
                    "item claim is not encrypted; retained"
                );
                let saved: ItemClaim =
                    serde_json::from_str(&super::system::decrypt_config_value(prior)?)
                        .context("damaged item claim; retained")?;
                ensure!(
                    saved.format == "review-execution-claim-v1"
                        && saved.item_id == id
                        && uuid::Uuid::parse_str(&saved.owner)
                            .is_ok_and(|owner| !owner.is_nil() && owner.to_string() == saved.owner),
                    "item claim scope differs; retained"
                );
            }
            let raw = if let Some(prior) = prior {
                prior
            } else {
                let raw =
                    super::system::encrypt_config_value(&serde_json::to_string(&ItemClaim {
                        format: "review-execution-claim-v1".into(),
                        item_id: id,
                        owner: uuid::Uuid::new_v4().to_string(),
                    })?)?;
                let changed = tx.execute(
                    "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                    params![key, raw],
                )?;
                ensure!(changed == 1, "item claim was not committed; no execution");
                raw
            };
            lease.item_claim = Some((key, raw));
            lease.assert_item(&tx, db, id)?;
            if let Some(content) = &lease._content {
                content.assert_content(&tx, db)?;
            }
            tx.commit()?;
            Ok(())
        })?;
        Ok(lease)
    }

    pub(crate) fn assert_item(
        &self,
        conn: &Connection,
        db: &Arc<tokio::sync::Mutex<Connection>>,
        id: i64,
    ) -> Result<()> {
        self.assert_native_owner()?;
        let path = conn
            .path()
            .filter(|p| !p.is_empty())
            .map(|p| Path::new(p).canonicalize())
            .transpose()?;
        ensure!(
            self.suffix == format!("review-{id}")
                && self.path == path
                && (path.is_some() || self.memory == Arc::as_ptr(db) as usize),
            "review execution authority does not own this item"
        );
        let (key, raw) = self
            .item_claim
            .as_ref()
            .context("item owner claim missing")?;
        ensure!(
            super::system::get_system_config_checked(conn, key)?.as_deref() == Some(raw.as_str()),
            "item execution owner changed; original state retained"
        );
        if let Some(content) = &self._content {
            content.assert_content(conn, db)?;
        }
        Ok(())
    }

    pub(crate) async fn mutate_item<R>(
        &self,
        db: &Arc<tokio::sync::Mutex<Connection>>,
        id: i64,
        mutate: impl FnOnce(&Connection) -> Result<R>,
    ) -> Result<R> {
        let mut conn = db.lock().await;
        let tx = conn.savepoint()?;
        self.assert_item(&tx, db, id)?;
        let result = mutate(&tx)?;
        self.assert_item(&tx, db, id)?;
        tx.commit()?;
        Ok(result)
    }
}
