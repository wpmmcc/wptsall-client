use anyhow::Context;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::types::*;

use super::{default_bindings_version, encrypt_for_save, load_encrypted_or_plain};

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
        return Ok(VendorKeysDoc {
            version: default_bindings_version(),
            keys: HashMap::new(),
        });
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
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output)
        .with_context(|| format!("write vendor keys file failed: {}", file_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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
        return Ok(VendorOAuthDoc {
            version: default_bindings_version(),
            configs: HashMap::new(),
        });
    }

    let mut doc: VendorOAuthDoc = serde_json::from_str(&decrypted_raw)
        .with_context(|| format!("parse vendor oauth json failed: {}", file_path.display()))?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    Ok(doc)
}

pub(crate) fn save_vendor_oauth(path: &str, doc: &VendorOAuthDoc) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let encoded = serde_json::to_string_pretty(doc)
        .with_context(|| "encode vendor oauth json failed".to_string())?;
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output)
        .with_context(|| format!("write vendor oauth file failed: {}", file_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output)
        .with_context(|| format!("write proxy profiles file failed: {}", file_path.display()))?;
    Ok(())
}
