use anyhow::Context;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::types::*;

use super::{default_bindings_version, encrypt_for_save, load_encrypted_or_plain};

pub(crate) fn load_component_bindings(path: &str) -> anyhow::Result<ComponentBindingsDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = ComponentBindingsDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        };
        save_component_bindings(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        return Ok(ComponentBindingsDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        });
    }

    let mut doc: ComponentBindingsDoc = serde_json::from_str(&decrypted_raw)
        .with_context(|| format!("parse bindings json failed: {}", file_path.display()))?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }

    Ok(doc)
}

pub(crate) fn save_component_bindings(
    path: &str,
    doc: &ComponentBindingsDoc,
) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let encoded = serde_json::to_string_pretty(doc)
        .with_context(|| "encode bindings json failed".to_string())?;
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output)
        .with_context(|| format!("write bindings file failed: {}", file_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub(crate) fn load_components_local(path: &str) -> anyhow::Result<ComponentsLocalDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    if !file_path.exists() {
        let doc = ComponentsLocalDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        };
        save_components_local(path, &doc)?;
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        return Ok(ComponentsLocalDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        });
    }

    let mut doc: ComponentsLocalDoc = serde_json::from_str(&decrypted_raw).with_context(|| {
        format!(
            "parse components local json failed: {}",
            file_path.display()
        )
    })?;
    if doc.version == 0 {
        doc.version = default_bindings_version();
    }
    Ok(doc)
}

pub(crate) fn save_components_local(path: &str, doc: &ComponentsLocalDoc) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let encoded = serde_json::to_string_pretty(doc)
        .with_context(|| "encode components local json failed".to_string())?;
    let output = encrypt_for_save(&encoded)?;
    fs::write(file_path, output).with_context(|| {
        format!(
            "write components local file failed: {}",
            file_path.display()
        )
    })?;
    Ok(())
}
