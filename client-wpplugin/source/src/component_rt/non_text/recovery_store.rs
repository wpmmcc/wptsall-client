//! Private file authority shared by SQLite and file-only upload callers.
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) struct RecoveryStore {
    root: PathBuf,
}

impl RecoveryStore {
    pub(crate) fn configured() -> Result<Self> {
        Self::at(
            Path::new(
                &std::env::var("WPTSALL_DATA_DIR")
                    .unwrap_or_else(|_| crate::config::DEFAULT_DATA_DIR.into()),
            )
            .join("media-recovery"),
        )
    }

    pub(crate) fn at(path: PathBuf) -> Result<Self> {
        Self::directory(&path)?;
        Ok(Self {
            root: fs::canonicalize(path)?,
        })
    }

    pub(crate) fn guard(&self, key: &str) -> Result<File> {
        let lock = Self::open_private(&self.path(key).with_extension("active"), true)?;
        lock.try_lock()
            .context("media upload already active; resume after owner exits")?;
        Ok(lock)
    }

    pub(crate) fn assets(&self) -> Result<PathBuf> {
        let path = self.root.join("assets");
        Self::directory(&path)?;
        Ok(fs::canonicalize(path)?)
    }

    pub(crate) fn verify_asset(&self, path: &Path) -> Result<()> {
        let directory = self.assets()?;
        anyhow::ensure!(
            path.parent() == Some(directory.as_path()),
            "retained media asset is outside its private directory"
        );
        Self::open_private(path, false)?;
        Ok(())
    }

    fn directory(path: &Path) -> Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
        let meta = fs::symlink_metadata(path)?;
        anyhow::ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "media recovery directory is a symlink; retained"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                meta.permissions().mode() & 0o077 == 0,
                "media recovery directory is not private; retained"
            );
        }
        Ok(())
    }

    fn private_metadata(path: &Path) -> Result<Option<fs::Metadata>> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "media recovery file is not a regular private file; retained"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            anyhow::ensure!(
                metadata.nlink() == 1 && metadata.permissions().mode() & 0o077 == 0,
                "media recovery file is shared or not private; retained"
            );
        }
        Ok(Some(metadata))
    }

    fn open_private(path: &Path, write: bool) -> Result<File> {
        let before = Self::private_metadata(path)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(write)
            .create_new(write && before.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        let after = Self::private_metadata(path)?.context("media recovery file disappeared")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata()?;
            anyhow::ensure!(
                opened.dev() == after.dev()
                    && opened.ino() == after.ino()
                    && before
                        .is_none_or(|old| old.dev() == opened.dev() && old.ino() == opened.ino()),
                "media recovery file changed during open; retained"
            );
        }
        Ok(file)
    }

    fn read_private(path: &Path) -> Result<Option<String>> {
        if Self::private_metadata(path)?.is_none() {
            return Ok(None);
        }
        let mut raw = String::new();
        Self::open_private(path, false)?.read_to_string(&mut raw)?;
        Ok(Some(raw))
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(format!(
            "{}.receipt",
            crate::sync_engine::hmac::sha256_hex(key.as_bytes())
        ))
    }

    pub(crate) fn load<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let path = self.path(key);
        let Some(raw) = Self::read_private(&path)? else {
            return Ok(None);
        };
        anyhow::ensure!(
            raw.starts_with("V1BUQw"),
            "unencrypted media checkpoint retained"
        );
        Ok(Some(serde_json::from_str(
            &crate::db::system::decrypt_config_value(&raw)?,
        )?))
    }

    pub(crate) fn update<T: Serialize + DeserializeOwned, R>(
        &self,
        key: &str,
        change: impl FnOnce(Option<T>) -> Result<(T, R)>,
    ) -> Result<R> {
        let path = self.path(key);
        let lock_path = path.with_extension("lock");
        let lock = Self::open_private(&lock_path, true)?;
        lock.try_lock().context("media recovery checkpoint busy")?;
        let (next, output) = change(self.load(key)?)?;
        let bytes = crate::db::system::encrypt_config_value(&serde_json::to_string(&next)?)?;
        let storage =
            crate::storage_capacity::StorageLease::for_write(&path, u64::try_from(bytes.len())?)?;
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        let result = (|| {
            file.write_all(bytes.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, &path)?;
            #[cfg(unix)]
            File::open(&self.root)?.sync_all()?;
            storage.finish()?;
            Ok::<_, anyhow::Error>(output)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub(crate) fn list<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        let mut rows = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|p| p.to_str()) != Some("receipt") {
                continue;
            }
            let raw = Self::read_private(&path)?.context("media recovery record disappeared")?;
            anyhow::ensure!(
                raw.starts_with("V1BUQw"),
                "unencrypted media checkpoint retained"
            );
            let value: serde_json::Value =
                serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)?;
            anyhow::ensure!(
                value.is_object()
                    && matches!(
                        value["format"].as_str(),
                        Some("media-session-v1" | "media-upload-receipt-v1")
                    ),
                "damaged media recovery record retained"
            );
            let key = if value["format"] == "media-session-v1" {
                value["key"]
                    .as_str()
                    .context("media checkpoint key is missing")?
                    .to_owned()
            } else {
                format!(
                    "delivery:media-upload-receipt-v1:{}",
                    crate::db::system::private_json_digest(&value["unit"])?
                )
            };
            anyhow::ensure!(
                path == self.path(&key),
                "media recovery file identity mismatch; retained"
            );
            rows.push(serde_json::from_value(value)?);
        }
        Ok(rows)
    }
}
