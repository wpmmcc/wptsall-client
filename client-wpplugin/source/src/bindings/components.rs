use anyhow::Context;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::types::*;

use super::{default_bindings_version, load_encrypted_or_plain, save_encrypted_file};

fn read_only_file_exists(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file(),
                "component configuration is not a regular file; retained"
            );
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect component configuration"),
    }
}

fn read_only_document<T: serde::de::DeserializeOwned>(raw: &str) -> anyhow::Result<T> {
    let value: serde_json::Value =
        serde_json::from_str(raw).context("parse component configuration")?;
    anyhow::ensure!(
        value.is_object(),
        "component configuration must be an object; retained"
    );
    serde_json::from_value(value).context("parse component configuration")
}

pub(crate) fn load_component_bindings(path: &str) -> anyhow::Result<ComponentBindingsDoc> {
    load_component_bindings_with_initialization(path, true)
}

pub(crate) fn read_component_bindings(path: &str) -> anyhow::Result<ComponentBindingsDoc> {
    load_component_bindings_with_initialization(path, false)
}

fn load_component_bindings_with_initialization(
    path: &str,
    initialize: bool,
) -> anyhow::Result<ComponentBindingsDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent().filter(|_| initialize) {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let exists = if initialize {
        file_path.exists()
    } else {
        read_only_file_exists(file_path)?
    };
    if !exists {
        let doc = ComponentBindingsDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        };
        if initialize {
            save_component_bindings(path, &doc)?;
        }
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        anyhow::ensure!(
            initialize,
            "empty component bindings configuration is damaged; retained"
        );
        return Ok(ComponentBindingsDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        });
    }

    let mut doc: ComponentBindingsDoc = if initialize {
        serde_json::from_str(&decrypted_raw)
            .with_context(|| format!("parse bindings json failed: {}", file_path.display()))?
    } else {
        read_only_document(&decrypted_raw)?
    };
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
    save_encrypted_file(file_path, &encoded)
}

pub(crate) fn load_components_local(path: &str) -> anyhow::Result<ComponentsLocalDoc> {
    load_components_local_with_initialization(path, true)
}

pub(crate) fn read_components_local(path: &str) -> anyhow::Result<ComponentsLocalDoc> {
    load_components_local_with_initialization(path, false)
}

fn load_components_local_with_initialization(
    path: &str,
    initialize: bool,
) -> anyhow::Result<ComponentsLocalDoc> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent().filter(|_| initialize) {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let exists = if initialize {
        file_path.exists()
    } else {
        read_only_file_exists(file_path)?
    };
    if !exists {
        let doc = ComponentsLocalDoc {
            version: default_bindings_version(),
            components: HashMap::new(),
        };
        if initialize {
            save_components_local(path, &doc)?;
        }
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        anyhow::bail!("empty local components configuration is damaged; retained");
    }

    let mut doc: ComponentsLocalDoc = if initialize {
        serde_json::from_str(&decrypted_raw).with_context(|| {
            format!(
                "parse components local json failed: {}",
                file_path.display()
            )
        })?
    } else {
        read_only_document(&decrypted_raw)?
    };
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
    save_encrypted_file(file_path, &encoded)
}
