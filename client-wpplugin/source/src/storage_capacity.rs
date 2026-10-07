//! Root-wide cooperative byte admission. Unknown bookings and files are retained.
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const POLICY: &str = ".wptsall-storage-v1.json";
const LOCK: &str = ".wptsall-storage-v1.lock";
const BOOKINGS: &str = ".wptsall-storage-bookings-v1";
const MAX_POLICY_BYTES: u64 = 16 * 1024;
const MAX_BOOKING_BYTES: u64 = 1024;
const MAX_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 64;
pub(crate) const DEFAULT_MAX_PHYSICAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct RootCapacityExhausted;

impl std::fmt::Display for RootCapacityExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "STORAGE_CAPACITY_EXHAUSTED: data root is full; new work stopped, existing evidence retained",
        )
    }
}

impl std::error::Error for RootCapacityExhausted {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoragePolicy {
    pub(crate) format: String,
    pub(crate) max_physical_bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct PhysicalInventory {
    pub(crate) physical_retained_bytes: u64,
    pub(crate) physical_retained_files: u64,
    pub(crate) root_booked_bytes: u64,
    pub(crate) max_physical_bytes: u64,
}

#[derive(Serialize)]
pub(crate) struct StorageSnapshot {
    pub(crate) policy: StoragePolicy,
    pub(crate) revision: Option<String>,
    #[serde(flatten)]
    pub(crate) inventory: PhysicalInventory,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Booking {
    format: String,
    identity: String,
    bytes: u64,
}

#[derive(Clone)]
pub(crate) struct StorageCredit {
    root: PathBuf,
    identity: String,
    database: Option<PathBuf>,
    file: Option<PathBuf>,
}

tokio::task_local! {
    static RESULT_CREDIT: Option<StorageCredit>;
}

pub(crate) async fn with_result_credit<T>(
    credit: Option<StorageCredit>,
    result: impl std::future::Future<Output = T>,
) -> T {
    RESULT_CREDIT.scope(credit, result).await
}

pub(crate) fn with_database_credit<T>(
    credit: Option<StorageCredit>,
    result: impl FnOnce() -> T,
) -> T {
    RESULT_CREDIT.sync_scope(credit, result)
}

pub(crate) struct StorageLease {
    root: PathBuf,
    _file: File,
    spent_credit: Option<(PathBuf, Vec<u8>, u64)>,
}

fn configured_root() -> PathBuf {
    crate::config::data_dir().unwrap_or_else(|| {
        PathBuf::from(crate::config::env_or(
            "WPTSALL_DATA_DIR",
            crate::config::DEFAULT_DATA_DIR,
        ))
    })
}

pub(crate) fn sqlite_temp_path() -> PathBuf {
    configured_root().join(format!(
        ".wptsall-sqlite-temp-v1-{}",
        uuid::Uuid::new_v4().simple()
    ))
}

fn private_directory(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).context("STORAGE_CAPACITY_DIRECTORY")?;
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_dir(),
        "STORAGE_CAPACITY_INVALID: root is not a regular directory"
    );
    Ok(())
}

fn checked_bytes(path: &Path, max: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= max,
        "STORAGE_CAPACITY_INVALID: invalid control file"
    );
    let mut bytes = Vec::new();
    File::open(path)?.take(max + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == metadata.len(),
        "STORAGE_CAPACITY_INVALID: control file changed"
    );
    Ok(bytes)
}

fn policy(root: &Path) -> Result<StoragePolicy> {
    let value = match fs::symlink_metadata(root.join(POLICY)) {
        Ok(_) => serde_json::from_slice::<StoragePolicy>(&checked_bytes(
            &root.join(POLICY),
            MAX_POLICY_BYTES,
        )?)
        .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: damaged root policy"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => StoragePolicy {
            format: "wptsall-storage-v1".into(),
            max_physical_bytes: DEFAULT_MAX_PHYSICAL_BYTES,
        },
        Err(_) => return Err(anyhow!("STORAGE_CAPACITY_INVALID: unreadable root policy")),
    };
    ensure!(
        value.format == "wptsall-storage-v1"
            && (1..=crate::db::capacity::MAX_CONFIG_LIMIT).contains(&value.max_physical_bytes),
        "STORAGE_CAPACITY_INVALID: invalid root policy"
    );
    Ok(value)
}

fn add(target: &mut u64, bytes: u64) -> Result<()> {
    *target = target
        .checked_add(bytes)
        .context("STORAGE_CAPACITY_INVALID: inventory overflow")?;
    Ok(())
}

pub(crate) fn inventory_at(root: &Path) -> Result<PhysicalInventory> {
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PhysicalInventory {
                max_physical_bytes: DEFAULT_MAX_PHYSICAL_BYTES,
                ..Default::default()
            });
        }
        Err(_) => return Err(anyhow!("STORAGE_CAPACITY_INVALID: unreadable root")),
        Ok(metadata) => ensure!(
            metadata.file_type().is_dir(),
            "STORAGE_CAPACITY_INVALID: root alias is not an inventory"
        ),
    }
    let limits = policy(root)?;
    let resolved_root = fs::canonicalize(root)?;
    let mut value = PhysicalInventory {
        max_physical_bytes: limits.max_physical_bytes,
        ..Default::default()
    };
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let mut entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        ensure!(
            depth <= MAX_DEPTH,
            "STORAGE_CAPACITY_INVALID: directory depth exceeded"
        );
        let children = fs::read_dir(&directory)
            .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: unreadable directory"))?;
        for child in children {
            let child = child.map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: unreadable entry"))?;
            entries = entries
                .checked_add(1)
                .context("STORAGE_CAPACITY_INVALID: entry overflow")?;
            ensure!(
                entries <= MAX_ENTRIES,
                "STORAGE_CAPACITY_INVALID: entry limit exceeded"
            );
            let path = child.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: entry changed"))?;
            if metadata.is_dir() {
                pending.push((path, depth + 1));
                continue;
            }
            if metadata.file_type().is_symlink() {
                let target = fs::canonicalize(&path)
                    .context("STORAGE_CAPACITY_INVALID: unreadable file alias")?;
                ensure!(
                    target.starts_with(&resolved_root)
                        && fs::symlink_metadata(target)?.is_file()
                        && path != root.join(LOCK)
                        && path != root.join(POLICY)
                        && directory != root.join(BOOKINGS),
                    "STORAGE_CAPACITY_INVALID: outside-root or directory alias"
                );
                // Count the alias inode's bytes once. Its regular target is
                // separately included by the bounded walk, not followed here.
                add(&mut value.physical_retained_bytes, metadata.len())?;
                add(&mut value.physical_retained_files, 1)?;
                continue;
            }
            ensure!(
                metadata.is_file(),
                "STORAGE_CAPACITY_INVALID: aliases and special files are not an empty inventory"
            );
            if path == root.join(LOCK) {
                ensure!(
                    metadata.len() == 0,
                    "STORAGE_CAPACITY_INVALID: damaged root lock"
                );
                continue;
            }
            add(&mut value.physical_retained_bytes, metadata.len())?;
            add(&mut value.physical_retained_files, 1)?;
            if directory == root.join(BOOKINGS) {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                let identity = name.strip_suffix(".json").unwrap_or("");
                ensure!(
                    identity.len() == 64 && identity.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "STORAGE_CAPACITY_INVALID: damaged booking name"
                );
                let booking: Booking =
                    serde_json::from_slice(&checked_bytes(&path, MAX_BOOKING_BYTES)?)
                        .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: damaged booking"))?;
                ensure!(
                    booking.format == "wptsall-storage-booking-v1"
                        && booking.identity == identity
                        && booking.bytes <= crate::db::capacity::MAX_CONFIG_LIMIT,
                    "STORAGE_CAPACITY_INVALID: damaged booking identity"
                );
                add(&mut value.root_booked_bytes, booking.bytes)?;
            }
        }
    }
    Ok(value)
}

pub(crate) fn inventory() -> Result<PhysicalInventory> {
    inventory_at(&configured_root())
}

fn policy_revision(root: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(root.join(POLICY)) {
        Ok(_) => Ok(Some(format!(
            "{:x}",
            Sha256::digest(checked_bytes(&root.join(POLICY), MAX_POLICY_BYTES)?)
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(anyhow!(
            "STORAGE_CAPACITY_INVALID: unreadable policy revision"
        )),
    }
}

pub(crate) fn snapshot() -> Result<StorageSnapshot> {
    let root = configured_root();
    if matches!(fs::symlink_metadata(&root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(StorageSnapshot {
            policy: StoragePolicy {
                format: "wptsall-storage-v1".into(),
                max_physical_bytes: DEFAULT_MAX_PHYSICAL_BYTES,
            },
            revision: None,
            inventory: PhysicalInventory {
                max_physical_bytes: DEFAULT_MAX_PHYSICAL_BYTES,
                ..Default::default()
            },
        });
    }
    let lease = StorageLease::lock(&root)?;
    Ok(StorageSnapshot {
        policy: policy(&lease.root)?,
        revision: policy_revision(&lease.root)?,
        inventory: inventory_at(&lease.root)?,
    })
}

pub(crate) fn save_policy(
    expected_revision: Option<&str>,
    max_physical_bytes: u64,
) -> Result<StorageSnapshot> {
    ensure!(
        (1..=crate::db::capacity::MAX_CONFIG_LIMIT).contains(&max_physical_bytes),
        "STORAGE_CAPACITY_INVALID: physical limit must be a positive safe integer"
    );
    let root = configured_root();
    let lease = StorageLease::lock(&root)?;
    ensure!(
        policy_revision(&lease.root)?.as_deref() == expected_revision,
        "STORAGE_CAPACITY_CHANGED: reload before changing the data-root policy"
    );
    let value = StoragePolicy {
        format: "wptsall-storage-v1".into(),
        max_physical_bytes,
    };
    let encoded = serde_json::to_vec(&value)?;
    let temporary = lease.root.join(format!(
        ".wptsall-storage-policy-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    fs::rename(&temporary, lease.root.join(POLICY))?;
    #[cfg(unix)]
    File::open(&lease.root)?.sync_all()?;
    ensure!(
        checked_bytes(&lease.root.join(POLICY), MAX_POLICY_BYTES)? == encoded,
        "STORAGE_CAPACITY_NOT_COMMITTED: policy readback differs"
    );
    // The exclusive root lease still covers the readback and returned revision.
    Ok(StorageSnapshot {
        policy: policy(&lease.root)?,
        revision: policy_revision(&lease.root)?,
        inventory: inventory_at(&lease.root)?,
    })
}

fn check_room(value: &PhysicalInventory, additional: u64) -> Result<()> {
    ensure!(
        value
            .physical_retained_bytes
            .checked_add(value.root_booked_bytes)
            .and_then(|bytes| bytes.checked_add(additional))
            .is_some_and(|bytes| bytes <= value.max_physical_bytes),
        RootCapacityExhausted
    );
    Ok(())
}

fn declared_root(parent: &Path) -> Result<Option<PathBuf>> {
    let mut directory = Some(parent);
    for _ in 0..=MAX_DEPTH {
        let Some(current) = directory else {
            return Ok(None);
        };
        match fs::symlink_metadata(current.join(POLICY)) {
            Ok(_) => return Ok(Some(current.to_path_buf())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => anyhow::bail!("STORAGE_CAPACITY_INVALID: unreadable ancestor policy"),
        }
        directory = current.parent();
    }
    anyhow::bail!("STORAGE_CAPACITY_INVALID: ancestor policy depth exceeded")
}

impl StorageLease {
    pub(crate) fn acquire(root: &Path, additional: u64) -> Result<Self> {
        // Refuse obvious exhaustion without even creating a control file.
        check_room(&inventory_at(root)?, additional)?;
        let lease = Self::lock(root)?;
        check_room(&inventory_at(&lease.root)?, additional)?;
        Ok(lease)
    }

    fn lock(root: &Path) -> Result<Self> {
        inventory_at(root)?;
        Self::native_lock(root, LOCK)
    }

    fn native_lock(root: &Path, name: &str) -> Result<Self> {
        private_directory(root)?;
        let root = fs::canonicalize(root)?;
        let path = root.join(name);
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            ensure!(
                metadata.is_file() && metadata.len() == 0,
                "STORAGE_CAPACITY_INVALID: invalid root lease"
            );
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).context("STORAGE_CAPACITY_LEASE")?;
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(2))
            .context("STORAGE_CAPACITY_INVALID: lease deadline overflow")?;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => {
                    return Err(anyhow!(
                        "STORAGE_CAPACITY_BUSY: data root belongs to another writer"
                    ));
                }
            }
        }
        Ok(Self {
            root,
            _file: file,
            spent_credit: None,
        })
    }

    fn for_external_file(path: &Path, bytes: u64, peak: bool) -> Result<(Self, u64)> {
        let identity = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
        let lease = Self::native_lock(
            path.parent().context("storage file parent missing")?,
            &format!(".wptsall-storage-file-v1-{identity}.lock"),
        )?;
        let retained = match fs::symlink_metadata(path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file(),
                    "STORAGE_CAPACITY_INVALID: non-file override"
                );
                metadata.len()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(_) => anyhow::bail!("STORAGE_CAPACITY_INVALID: unreadable file override"),
        };
        let bytes = if peak {
            bytes.saturating_sub(retained)
        } else {
            bytes
        };
        ensure!(
            retained
                .checked_add(bytes)
                .is_some_and(|peak| peak <= DEFAULT_MAX_PHYSICAL_BYTES),
            "STORAGE_CAPACITY_EXHAUSTED: external file snapshot peak exceeds its limit"
        );
        Ok((lease, retained))
    }

    pub(crate) fn for_write(path: &Path, bytes: u64) -> Result<Self> {
        Self::for_write_with_credit(
            path,
            bytes,
            RESULT_CREDIT.try_with(Clone::clone).ok().flatten(),
        )
    }

    pub(crate) fn for_uncredited_write(path: &Path, bytes: u64) -> Result<Self> {
        Self::for_write_with_credit(path, bytes, None)
    }

    pub(crate) fn for_database_growth(path: &Path, bytes: u64) -> Result<Self> {
        // An asset-finalization scope must not finance unrelated SQLite work.
        let credit = RESULT_CREDIT
            .try_with(Clone::clone)
            .ok()
            .flatten()
            .filter(|credit| credit.database.is_some());
        Self::for_write_with_credit(path, bytes, credit)
    }

    pub(crate) fn for_database_peak(path: &Path, peak: u64) -> Result<(Self, u64)> {
        let credit = RESULT_CREDIT
            .try_with(Clone::clone)
            .ok()
            .flatten()
            .filter(|credit| credit.database.is_some());
        Self::for_admission(path, peak, true, credit)
    }

    fn for_write_with_credit(
        path: &Path,
        bytes: u64,
        credit: Option<StorageCredit>,
    ) -> Result<Self> {
        Self::for_admission(path, bytes, false, credit).map(|(lease, _)| lease)
    }

    fn for_admission(
        path: &Path,
        requested: u64,
        peak: bool,
        credit: Option<StorageCredit>,
    ) -> Result<(Self, u64)> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let root = configured_root();
        let root_absolute = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        private_directory(parent)?;
        let parent = fs::canonicalize(parent)?;
        let path = match fs::symlink_metadata(path) {
            Ok(_) => fs::canonicalize(path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent.join(path.file_name().context("storage file name missing")?)
            }
            Err(_) => anyhow::bail!("STORAGE_CAPACITY_INVALID: unreadable file override"),
        };
        let parent = path.parent().context("storage file parent missing")?;
        let root_resolved = fs::canonicalize(&root_absolute).ok();
        // Root-wide policy applies only to the configured root or an explicit
        // ancestor policy. A file override never claims an unrelated parent.
        let destination = if root_resolved
            .as_ref()
            .is_some_and(|root| parent.starts_with(root))
        {
            root_absolute
        } else {
            match declared_root(parent)? {
                Some(root) => root,
                None => {
                    ensure!(
                        credit.is_none(),
                        "STORAGE_CAPACITY_INVALID: result credit does not own file override"
                    );
                    return Self::for_external_file(&path, requested, peak);
                }
            }
        };
        if let Some(credit) = credit {
            let root = fs::canonicalize(&destination)?;
            ensure!(
                root == credit.root,
                "STORAGE_CAPACITY_INVALID: result credit does not own destination root"
            );
            if let Some(database) = &credit.database {
                let allowed = path == *database
                    || ["-wal", "-journal", "-shm"].iter().any(|suffix| {
                        let mut sibling = database.as_os_str().to_os_string();
                        sibling.push(suffix);
                        path == PathBuf::from(sibling)
                    });
                ensure!(
                    allowed,
                    "STORAGE_CAPACITY_INVALID: database credit does not own writer"
                );
            }
            if let Some(file) = &credit.file {
                ensure!(
                    path == *file,
                    "STORAGE_CAPACITY_INVALID: file credit does not own writer"
                );
            }
            let mut lease = Self::lock(&root)?;
            let before = if peak { physical_length(&path)? } else { 0 };
            let bytes = requested.saturating_sub(before);
            if peak && bytes == 0 {
                return Ok((lease, before));
            }
            let booking_path = lease.booking_path(&credit.identity);
            let prior = checked_bytes(&booking_path, MAX_BOOKING_BYTES)?;
            let booking: Booking = serde_json::from_slice(&prior)
                .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: result booking damaged"))?;
            ensure!(
                booking.format == "wptsall-storage-booking-v1"
                    && booking.identity == credit.identity
                    && booking.bytes >= bytes,
                "STORAGE_CAPACITY_EXHAUSTED: result exceeds its original root booking"
            );
            // Actual bytes replace the same owned booking, not another unit's.
            // Lowering the policy does not revoke previously admitted paid work.
            inventory_at(&root)?;
            lease.spent_credit = Some((booking_path, prior, bytes));
            Ok((lease, before))
        } else if peak {
            let lease = Self::lock(&destination)?;
            let before = physical_length(&path)?;
            let bytes = requested.saturating_sub(before);
            if bytes != 0 {
                check_room(&inventory_at(&lease.root)?, bytes)?;
            }
            Ok((lease, before))
        } else {
            Self::acquire(&destination, requested).map(|lease| (lease, 0))
        }
    }

    fn booking_path(&self, identity: &str) -> PathBuf {
        self.root.join(BOOKINGS).join(format!("{identity}.json"))
    }

    pub(crate) fn finish(&self) -> Result<()> {
        let Some((path, prior, spent)) = &self.spent_credit else {
            return Ok(());
        };
        ensure!(
            checked_bytes(path, MAX_BOOKING_BYTES)? == *prior,
            "STORAGE_CAPACITY_NOT_COMMITTED: result booking changed; retained"
        );
        if *spent == 0 {
            return Ok(());
        }
        let mut booking: Booking = serde_json::from_slice(prior)?;
        booking.bytes = booking
            .bytes
            .checked_sub(*spent)
            .context("STORAGE_CAPACITY_INVALID: result booking underflow")?;
        let encoded = serde_json::to_vec(&booking)?;
        let temporary = self.root.join(format!(
            ".wptsall-storage-booking-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        ensure!(
            checked_bytes(path, MAX_BOOKING_BYTES)? == encoded,
            "STORAGE_CAPACITY_NOT_COMMITTED: result booking readback differs"
        );
        #[cfg(unix)]
        File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    }

    pub(crate) fn prepay_database_growth(&mut self) -> Result<()> {
        if let Some((_, prior, reserved)) = &self.spent_credit {
            let mut booking: Booking = serde_json::from_slice(prior)?;
            booking.bytes = booking
                .bytes
                .checked_sub(*reserved)
                .context("STORAGE_CAPACITY_INVALID: database booking underflow")?;
            // The private booking snapshot itself must fit the previously
            // admitted remainder while it coexists with the old authority.
            ensure!(
                booking.bytes >= serde_json::to_vec(&booking)?.len() as u64,
                RootCapacityExhausted
            );
        }
        // Persist BEFORE native growth. A short write, panic or process death
        // conservatively consumes the admitted maximum; never refund unknown
        // effects or reuse an already allocated byte's original booking.
        self.finish()?;
        self.spent_credit = None;
        Ok(())
    }
}

fn physical_length(path: &Path) -> Result<u64> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file(),
                "STORAGE_CAPACITY_INVALID: non-file database sibling"
            );
            Ok(metadata.len())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(_) => anyhow::bail!("STORAGE_CAPACITY_INVALID: unreadable database sibling"),
    }
}

fn booking_identity(conn: &rusqlite::Connection, unit: &str) -> Result<String> {
    let database = match conn.path().filter(|path| !path.is_empty()) {
        Some(path) => fs::canonicalize(path)?.to_string_lossy().into_owned(),
        None => {
            const KEY: &str = "storage_memory_store_v1";
            let value = match crate::db::system::get_system_config_checked(conn, KEY)? {
                Some(value) => value,
                None => {
                    ensure!(
                        !conn.is_autocommit(),
                        "STORAGE_CAPACITY_INVALID: transient store has no committed identity"
                    );
                    let value = uuid::Uuid::new_v4().to_string();
                    crate::db::system::set_system_config(conn, KEY, &value)?;
                    value
                }
            };
            ensure!(
                uuid::Uuid::parse_str(&value)
                    .is_ok_and(|id| !id.is_nil() && id.to_string() == value),
                "STORAGE_CAPACITY_INVALID: damaged transient store identity"
            );
            format!("memory:{value}")
        }
    };
    let mut hash = Sha256::new();
    hash.update(b"wptsall-root-booking-v1\0");
    hash.update(database.as_bytes());
    hash.update([0]);
    hash.update(unit.as_bytes());
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn result_credit(
    conn: &rusqlite::Connection,
    unit: &str,
) -> Result<Option<StorageCredit>> {
    if legacy_transient_store(conn)? {
        return Ok(None);
    }
    let root = configured_root();
    let identity = booking_identity(conn, unit)?;
    let path = root.join(BOOKINGS).join(format!("{identity}.json"));
    if !path.try_exists()? {
        return Ok(None);
    }
    let booking: Booking = serde_json::from_slice(&checked_bytes(&path, MAX_BOOKING_BYTES)?)
        .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: result booking damaged"))?;
    ensure!(
        booking.format == "wptsall-storage-booking-v1" && booking.identity == identity,
        "STORAGE_CAPACITY_INVALID: result booking changed"
    );
    Ok(Some(StorageCredit {
        root: fs::canonicalize(root)?,
        identity,
        database: None,
        file: None,
    }))
}

const DATABASE_RECOVERY_BYTES: u64 = 8 * 1024 * 1024;

fn recovery_identity(database: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(b"wptsall-database-recovery-booking-v1\0");
    hash.update(database.as_os_str().as_encoded_bytes());
    format!("{:x}", hash.finalize())
}

fn recovery_destination(database: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    let parent = database
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    private_directory(parent)?;
    let database = if database.exists() {
        fs::canonicalize(database)?
    } else {
        fs::canonicalize(parent)?.join(database.file_name().context("database filename missing")?)
    };
    let root = match fs::canonicalize(configured_root()) {
        Ok(root) if database.starts_with(&root) => root,
        _ => match declared_root(database.parent().context("database parent missing")?)? {
            Some(root) => fs::canonicalize(root)?,
            None => return Ok(None), // Do not claim a shared external parent.
        },
    };
    Ok(Some((root, database)))
}

pub(crate) fn database_recovery_credit(
    database: &Path,
    allow_reservation: bool,
) -> Result<Option<StorageCredit>> {
    let Some((root, database)) = recovery_destination(database)? else {
        return Ok(None);
    };
    let identity = recovery_identity(&database);
    let path = root.join(BOOKINGS).join(format!("{identity}.json"));
    // All authority reads and replenishment share the root lock. A second
    // opener observes the first one's committed booking, not a stale absence.
    // There are deliberately no SQLite calls while this lease is held.
    let _lease = StorageLease::lock(&root)?;
    let prior = if path.try_exists()? {
        Some(checked_bytes(&path, MAX_BOOKING_BYTES)?)
    } else {
        None
    };
    let remaining = match &prior {
        Some(bytes) => {
            let booking: Booking = serde_json::from_slice(bytes)
                .context("STORAGE_CAPACITY_INVALID: database recovery booking damaged")?;
            ensure!(
                booking.format == "wptsall-storage-booking-v1"
                    && booking.identity == identity
                    && booking.bytes <= DATABASE_RECOVERY_BYTES,
                "STORAGE_CAPACITY_INVALID: database recovery booking changed"
            );
            booking.bytes
        }
        None => 0,
    };
    if allow_reservation && remaining < DATABASE_RECOVERY_BYTES {
        let booking = serde_json::to_vec(&Booking {
            format: "wptsall-storage-booking-v1".into(),
            identity: identity.clone(),
            bytes: DATABASE_RECOVERY_BYTES,
        })?;
        let additional = (DATABASE_RECOVERY_BYTES - remaining)
            .checked_add(booking.len() as u64)
            .context("database recovery capacity overflow")?;
        match check_room(&inventory_at(&root)?, additional) {
            Ok(()) => {
                private_directory(&root.join(BOOKINGS))?;
                let current = if path.try_exists()? {
                    Some(checked_bytes(&path, MAX_BOOKING_BYTES)?)
                } else {
                    None
                };
                ensure!(
                    current == prior,
                    "STORAGE_CAPACITY_CHANGED: database recovery booking changed"
                );
                let temporary = root.join(format!(
                    ".wptsall-database-booking-{}.tmp",
                    uuid::Uuid::new_v4().simple()
                ));
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(&temporary)?;
                file.write_all(&booking)?;
                file.sync_all()?;
                fs::rename(&temporary, &path)?;
                ensure!(
                    checked_bytes(&path, MAX_BOOKING_BYTES)? == booking,
                    "STORAGE_CAPACITY_NOT_COMMITTED: database recovery booking readback differs"
                );
                #[cfg(unix)]
                File::open(path.parent().unwrap())?.sync_all()?;
            }
            Err(error) if error.is::<RootCapacityExhausted>() && prior.is_some() => {
                // Lowering policy cannot revoke the previously admitted remainder.
            }
            Err(error) => return Err(error),
        }
    }
    if !path.try_exists()? {
        return Ok(None);
    }
    Ok(Some(StorageCredit {
        root,
        identity,
        database: Some(database),
        file: None,
    }))
}

const FILE_RECOVERY_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn file_recovery_credit(
    file: &Path,
    allow_reservation: bool,
) -> Result<Option<StorageCredit>> {
    let Some((root, file)) = recovery_destination(file)? else {
        return Ok(None);
    };
    let mut hash = Sha256::new();
    hash.update(b"wptsall-file-recovery-booking-v1\0");
    hash.update(file.as_os_str().as_encoded_bytes());
    let identity = format!("{:x}", hash.finalize());
    let lease = StorageLease::lock(&root)?;
    let path = lease.booking_path(&identity);
    let prior = match path.symlink_metadata() {
        Ok(_) => Some(checked_bytes(&path, MAX_BOOKING_BYTES)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let remaining = match &prior {
        Some(bytes) => {
            let booking: Booking = serde_json::from_slice(bytes)
                .context("STORAGE_CAPACITY_INVALID: file recovery booking damaged")?;
            ensure!(
                booking.format == "wptsall-storage-booking-v1"
                    && booking.identity == identity
                    && booking.bytes <= FILE_RECOVERY_BYTES,
                "STORAGE_CAPACITY_INVALID: file recovery booking changed"
            );
            booking.bytes
        }
        None => 0,
    };
    if allow_reservation && remaining < FILE_RECOVERY_BYTES {
        let booking = serde_json::to_vec(&Booking {
            format: "wptsall-storage-booking-v1".into(),
            identity: identity.clone(),
            bytes: FILE_RECOVERY_BYTES,
        })?;
        check_room(
            &inventory_at(&root)?,
            FILE_RECOVERY_BYTES - remaining + booking.len() as u64,
        )?;
        private_directory(&root.join(BOOKINGS))?;
        let temporary = root.join(format!(".wptsall-file-booking-{}.tmp", uuid::Uuid::new_v4().simple()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut staged = options.open(&temporary)?;
        let result = (|| -> Result<()> {
            staged.write_all(&booking)?;
            staged.sync_all()?;
            fs::rename(&temporary, &path)?;
            ensure!(checked_bytes(&path, MAX_BOOKING_BYTES)? == booking,
                "STORAGE_CAPACITY_NOT_COMMITTED: file recovery booking readback differs");
            #[cfg(unix)]
            File::open(path.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
    } else if prior.is_none() {
        return Ok(None);
    }
    Ok(Some(StorageCredit { root, identity, database: None, file: Some(file) }))
}

pub(crate) fn database_result_credit(
    conn: &rusqlite::Connection,
    unit: &str,
) -> Result<Option<StorageCredit>> {
    let mut credit = result_credit(conn, unit)?;
    if let Some(value) = &mut credit {
        if let Some(path) = conn.path().filter(|path| !path.is_empty()) {
            let database = fs::canonicalize(path)?;
            if !database.starts_with(&value.root) {
                return Ok(None); // External file admission, never another root's booking.
            }
            value.database = Some(database);
        } else {
            return Ok(None);
        }
    }
    Ok(credit)
}
pub(crate) fn reserve(conn: &rusqlite::Connection, unit: &str, bytes: u64) -> Result<()> {
    let identity = booking_identity(conn, unit)?;
    let booking = serde_json::to_vec(&Booking {
        format: "wptsall-storage-booking-v1".into(),
        identity: identity.clone(),
        bytes,
    })?;
    let additional = bytes
        .checked_add(booking.len() as u64)
        .context("STORAGE_CAPACITY_INVALID: booking overflow")?;
    let lease = StorageLease::acquire(&configured_root(), additional)?;
    private_directory(&lease.root.join(BOOKINGS))?;
    let path = lease.booking_path(&identity);
    ensure!(
        !path.exists(),
        "STORAGE_CAPACITY_UNCONFIRMED: original root booking retained"
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(&booking)?;
    file.sync_all()?;
    ensure!(
        checked_bytes(&path, MAX_BOOKING_BYTES)? == booking,
        "STORAGE_CAPACITY_NOT_COMMITTED: root booking readback differs"
    );
    #[cfg(unix)]
    File::open(path.parent().unwrap())?.sync_all()?;
    // A failed later SQLite commit retains this booking. Neither age nor an
    // absent row proves that an external effect never happened.
    Ok(())
}

pub(crate) fn release_confirmed_result(conn: &rusqlite::Connection, unit: &str) -> Result<()> {
    if legacy_transient_store(conn)? {
        return Ok(());
    }
    let identity = booking_identity(conn, unit)?;
    let root = configured_root();
    let path = root.join(BOOKINGS).join(format!("{identity}.json"));
    if !path.try_exists()? {
        return Ok(());
    }
    // Existing paid Ready completion is allowed above a newly lowered limit.
    inventory_at(&root)?;
    if let Ok(metadata) = fs::symlink_metadata(root.join(LOCK)) {
        ensure!(
            metadata.is_file() && metadata.len() == 0,
            "STORAGE_CAPACITY_INVALID: original booking retained"
        );
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(root.join(LOCK))?;
    file.try_lock()
        .map_err(|_| anyhow!("STORAGE_CAPACITY_BUSY: original booking retained"))?;
    inventory_at(&root)?;
    let booking: Booking = serde_json::from_slice(&checked_bytes(&path, MAX_BOOKING_BYTES)?)
        .map_err(|_| anyhow!("STORAGE_CAPACITY_INVALID: original booking retained"))?;
    ensure!(
        booking.format == "wptsall-storage-booking-v1" && booking.identity == identity,
        "STORAGE_CAPACITY_INVALID: original booking retained"
    );
    fs::remove_file(&path)?;
    #[cfg(unix)]
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

fn legacy_transient_store(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(conn.path().filter(|path| !path.is_empty()).is_none()
        && crate::db::system::get_system_config_checked(conn, "storage_memory_store_v1")?.is_none())
}
