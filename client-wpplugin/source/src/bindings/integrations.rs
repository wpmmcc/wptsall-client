use anyhow::Context;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::types::*;

use super::{default_bindings_version, load_encrypted_or_plain, save_encrypted_file};

pub(crate) fn read_document<D: serde::de::DeserializeOwned + Default>(
    path: &str,
) -> anyhow::Result<D> {
    let path = Path::new(path);
    match path.symlink_metadata() {
        Ok(metadata) => anyhow::ensure!(
            metadata.file_type().is_file(),
            "integration configuration is not a regular file; retained"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(D::default()),
        Err(error) => return Err(error).context("inspect integration configuration"),
    }
    let value: serde_json::Value = serde_json::from_str(&load_encrypted_or_plain(path)?)
        .context("damaged integration configuration; retained")?;
    anyhow::ensure!(
        value.is_object(),
        "integration configuration must be an object; retained"
    );
    serde_json::from_value(value).context("damaged integration configuration; retained")
}

pub(crate) fn load_vendor_keys(path: &str) -> anyhow::Result<VendorKeysDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = VendorKeysDoc {
            version: default_bindings_version(),
            keys: HashMap::new(),
        };
        save_vendor_keys(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        anyhow::bail!("empty vendor keys configuration is damaged; retained");
    }

    let mut doc: VendorKeysDoc = serde_json::from_str(&decrypted_raw)
        .with_context(|| format!("parse vendor keys json failed: {}", file_path.display()))?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    Ok(doc)
}

pub(crate) fn save_vendor_keys(path: &str, doc: &VendorKeysDoc) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let encoded = serde_json::to_string_pretty(doc)
        .with_context(|| "encode vendor keys json failed".to_string())?;
    save_encrypted_file(file_path, &encoded)
}

pub(crate) fn load_vendor_oauth(path: &str) -> anyhow::Result<VendorOAuthDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = VendorOAuthDoc {
            version: default_bindings_version(),
            configs: HashMap::new(),
        };
        save_vendor_oauth(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        anyhow::bail!("empty vendor OAuth configuration is damaged; retained");
    }

    let mut doc: VendorOAuthDoc = serde_json::from_str(&decrypted_raw)
        .with_context(|| format!("parse vendor oauth json failed: {}", file_path.display()))?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    Ok(doc)
}

pub(crate) fn save_vendor_oauth(path: &str, doc: &VendorOAuthDoc) -> anyhow::Result<()> {
    mutate_vendor_oauth(path, |current| {
        *current = doc.clone();
        Ok(())
    })
}

pub(crate) fn mutate_vendor_oauth<R>(
    path: &str,
    update: impl FnOnce(&mut VendorOAuthDoc) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    mutate_vendor_oauth_if_changed(path, |doc| update(doc).map(|result| (result, true)))
}

pub(crate) fn mutate_vendor_oauth_if_changed<R>(
    path: &str,
    update: impl FnOnce(&mut VendorOAuthDoc) -> anyhow::Result<(R, bool)>,
) -> anyhow::Result<R> {
    let lease = IntegrationDocumentLease::acquire(path)?;
    lease.assert_owner()?;
    let file_path = &lease.path;
    let mut doc = if file_path.exists() {
        serde_json::from_str::<VendorOAuthDoc>(&load_encrypted_or_plain(file_path)?)
            .context("damaged OAuth configuration; retained")?
    } else {
        VendorOAuthDoc::default()
    };
    let (result, changed) = update(&mut doc)?;
    lease.assert_owner()?;
    if changed {
        let encoded = serde_json::to_string_pretty(&doc)
            .with_context(|| "encode vendor oauth json failed".to_string())?;
        crate::storage_capacity::with_database_credit(None, || {
            save_encrypted_file(file_path, &encoded)
        })?;
    }
    lease.assert_owner()?;
    Ok(result)
}

pub(crate) struct IntegrationDocumentLease {
    path: std::path::PathBuf,
    _lease: super::native_lock::NativeLease,
}

impl IntegrationDocumentLease {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn assert_owner(&self) -> anyhow::Result<()> {
        self._lease.assert_owner()
    }

    pub(crate) fn acquire(path: &str) -> anyhow::Result<Self> {
        let file_path = Path::new(path);
        if let Some(parent) = file_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
            }
        }

        let file_path = match fs::symlink_metadata(file_path) {
            Ok(_) => fs::canonicalize(file_path).context("resolve integration configuration")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = file_path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                parent.canonicalize()?.join(
                    file_path
                        .file_name()
                        .context("integration configuration filename missing")?,
                )
            }
            Err(error) => return Err(error.into()),
        };
        let lock_path = file_path.with_file_name(format!(
            "{}.mutation.lock",
            file_path
                .file_name()
                .context("integration configuration filename missing")?
                .to_string_lossy()
        ));
        let lease = super::native_lock::NativeLease::acquire(
            &lock_path,
            "integration configuration is being changed; retained",
        )?;
        Ok(Self {
            path: file_path,
            _lease: lease,
        })
    }
}

pub(crate) fn load_proxy_profiles(path: &str) -> anyhow::Result<ProxyProfilesDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = ProxyProfilesDoc {
            version: default_bindings_version(),
            profiles: HashMap::new(),
        };
        save_proxy_profiles(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        return Ok(ProxyProfilesDoc {
            version: default_bindings_version(),
            profiles: HashMap::new(),
        });
    }

    let mut doc: ProxyProfilesDoc = serde_json::from_str(&decrypted_raw)
        .with_context(|| format!("parse proxy profiles json failed: {}", file_path.display()))?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    Ok(doc)
}

pub(crate) fn save_proxy_profiles(path: &str, doc: &ProxyProfilesDoc) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let encoded = serde_json::to_string_pretty(doc)
        .with_context(|| "encode proxy profiles json failed".to_string())?;
    save_encrypted_file(file_path, &encoded)
}
