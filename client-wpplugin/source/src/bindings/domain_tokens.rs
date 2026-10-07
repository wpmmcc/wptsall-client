use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::Context;

use super::{save_encrypted_file, load_encrypted_or_plain};
use crate::logging::session_token_prefix;
use crate::types::{
    DomainStatusItem, DomainTokenBindingEntry, DomainTokenBindingStatusItem,
    DomainTokenBindingsDoc, PluginIdentity,
};

/// Current persisted schema version of `DomainTokenBindingsDoc`
/// (Identity Contract v1.1 §4).
pub(super) const DOMAIN_BINDINGS_VERSION: u32 = 3;

pub(super) fn load_domain_token_bindings(path: &str) -> anyhow::Result<DomainTokenBindingsDoc> {
    let file_path = Path::new(path);
    // Reading missing configuration does not publish a new document. In
    // particular, recovery at full quota must remain a read-only operation.
    if matches!(fs::symlink_metadata(file_path), Err(ref error)
        if error.kind() == std::io::ErrorKind::NotFound)
    {
        let doc = DomainTokenBindingsDoc {
            version: DOMAIN_BINDINGS_VERSION,
            domains: std::collections::HashMap::new(),
        };
        return Ok(doc);
    }

    let decrypted_raw = load_encrypted_or_plain(file_path)?;
    if decrypted_raw.trim().is_empty() {
        return Ok(DomainTokenBindingsDoc {
            version: DOMAIN_BINDINGS_VERSION,
            domains: std::collections::HashMap::new(),
        });
    }

    let mut doc: DomainTokenBindingsDoc =
        serde_json::from_str(&decrypted_raw).with_context(|| {
            format!(
                "parse domain token bindings failed: {}",
                file_path.display()
            )
        })?;
    if doc.version == 0 {
        doc.version = 1;
    }
    if doc.version < 2 {
        let old_entries: Vec<(String, DomainTokenBindingEntry)> = doc.domains.drain().collect();
        for (old_key, entry) in old_entries {
            let new_key = normalize_domain_base(&old_key);
            if new_key.is_empty() || entry.wp_client_token.trim().is_empty() {
                continue;
            }
            doc.domains.entry(new_key).or_insert(entry);
        }
        doc.version = 2;
    }
    // Identity Contract v1.1 §4 (C-1): v2 -> v3 migration via the shared
    // backfill (same semantics as the SQLite cache loader).
    if doc.version < 3 {
        apply_v3_identity_backfill(&mut doc);
    }
    doc.domains.retain(|key, entry| {
        let normalized = normalize_domain_base(key);
        !normalized.is_empty() && !entry.wp_client_token.trim().is_empty()
    });
    Ok(doc)
}

/// Identity Contract v1.1 §4 (C-1): backfill v3 identity fields on a pre-v3
/// doc. Legacy entries can only be wpmmcc-ats sites (the wpmmcc plugin did
/// not exist when they were bound), so the identity defaults to `wpmmcc_ats`
/// with `identity_verified_at = None` — the next worker heartbeat re-verifies
/// and overwrites with the live value. Idempotent: a v3 doc is a no-op.
/// Shared by the file loader and the SQLite cache loader so both persistence
/// sources produce identical v3 semantics.
pub(super) fn apply_v3_identity_backfill(doc: &mut DomainTokenBindingsDoc) {
    if doc.version >= 3 {
        return;
    }
    for entry in doc.domains.values_mut() {
        if entry.plugin_identity.is_none() {
            entry.plugin_identity = Some(PluginIdentity::WpmmccAts);
        }
        entry.identity_verified_at = None;
        entry.identity_capabilities = None;
    }
    doc.version = 3;
}

pub(super) fn save_domain_token_bindings(
    path: &str,
    doc: &DomainTokenBindingsDoc,
) -> anyhow::Result<()> {
    let file_path = Path::new(path);
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create bindings dir failed: {}", parent.display()))?;
        }
    }

    let mut normalized = doc.clone();
    let entries: Vec<(String, DomainTokenBindingEntry)> = normalized
        .domains
        .drain()
        .map(|(key, entry)| (normalize_domain_base(&key), entry))
        .collect();
    for (key, entry) in entries {
        if key.is_empty() || entry.wp_client_token.trim().is_empty() {
            continue;
        }
        normalized.domains.insert(
            key,
            DomainTokenBindingEntry {
                wp_client_token: entry.wp_client_token.trim().to_string(),
                route_secret: entry.route_secret.trim().to_string(),
                // Identity fields (contract §4) are preserved verbatim on save.
                plugin_identity: entry.plugin_identity,
                identity_verified_at: entry.identity_verified_at,
                identity_capabilities: entry.identity_capabilities,
            },
        );
    }

    let encoded = serde_json::to_string_pretty(&normalized)
        .with_context(|| "encode domain token bindings json failed".to_string())?;
    save_encrypted_file(file_path, &encoded)
}

pub(super) fn normalize_api_base_url_key(input: &str) -> String {
    input.trim().trim_end_matches('/').to_lowercase()
}

pub(super) fn normalize_domain_base(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let (scheme, rest) = if let Some(pos) = trimmed.find("://") {
        (trimmed[..pos].to_lowercase(), &trimmed[pos + 3..])
    } else {
        ("https".to_string(), trimmed)
    };
    let host = if let Some(slash) = rest.find('/') {
        &rest[..slash]
    } else {
        rest
    };
    let host_lower = host.trim().to_lowercase();
    if host_lower.is_empty() {
        return String::new();
    }
    format!("{}://{}", scheme, host_lower)
}

pub(super) fn has_any_domain_token_bindings(doc: &DomainTokenBindingsDoc) -> bool {
    doc.domains
        .values()
        .any(|entry| !entry.wp_client_token.trim().is_empty())
}

pub(super) fn resolve_wp_client_token_for_domain(
    api_base_url: &str,
    bindings_doc: &DomainTokenBindingsDoc,
    fallback_wp_client_token: &str,
) -> Option<String> {
    let domain_base = normalize_domain_base(api_base_url);
    if let Some(entry) = bindings_doc.domains.get(&domain_base) {
        let token = entry.wp_client_token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    let legacy_key = normalize_api_base_url_key(api_base_url);
    if let Some(entry) = bindings_doc.domains.get(&legacy_key) {
        let token = entry.wp_client_token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    let fallback = fallback_wp_client_token.trim();
    if fallback.is_empty() {
        None
    } else {
        Some(fallback.to_string())
    }
}

pub(super) fn resolve_route_secret_for_domain(
    api_base_url: &str,
    bindings_doc: &DomainTokenBindingsDoc,
) -> Option<String> {
    let domain_base = normalize_domain_base(api_base_url);
    if let Some(entry) = bindings_doc.domains.get(&domain_base) {
        let secret = entry.route_secret.trim();
        if !secret.is_empty() {
            return Some(secret.to_string());
        }
    }
    let legacy_key = normalize_api_base_url_key(api_base_url);
    if let Some(entry) = bindings_doc.domains.get(&legacy_key) {
        let secret = entry.route_secret.trim();
        if !secret.is_empty() {
            return Some(secret.to_string());
        }
    }
    None
}

/// Resolve the full binding entry (including identity fields, contract §4)
/// for a domain, mirroring the token/secret resolution key order.
pub(super) fn resolve_entry_for_domain<'a>(
    api_base_url: &str,
    bindings_doc: &'a DomainTokenBindingsDoc,
) -> Option<&'a DomainTokenBindingEntry> {
    let domain_base = normalize_domain_base(api_base_url);
    if let Some(entry) = bindings_doc.domains.get(&domain_base) {
        if !entry.wp_client_token.trim().is_empty() {
            return Some(entry);
        }
    }
    let legacy_key = normalize_api_base_url_key(api_base_url);
    if let Some(entry) = bindings_doc.domains.get(&legacy_key) {
        if !entry.wp_client_token.trim().is_empty() {
            return Some(entry);
        }
    }
    None
}

pub(super) fn build_wp_base_url(domain_base: &str, route_secret: &str) -> Option<String> {
    let secret = route_secret.trim();
    if secret.is_empty() {
        return None;
    }
    let base = domain_base.trim().trim_end_matches('/');
    Some(format!("{}/wp-json/wptsall/v2/{}/client", base, secret))
}

/// Identity-aware verification base (Identity Contract v1.1 §2/§3): the
/// binding's expected identity selects the endpoint family the live ping
/// verifies against. ATS sites answer on the secret-bearing wptsall client
/// base; wpmmcc sites on the secret-prefixed wpmmcc/v1 sync base. The
/// response shape (`data.plugin_identity` + capability fields) is the same
/// by contract, so [`verify_identity`] works against either.
pub(crate) fn build_verify_base_url(
    domain_base: &str,
    route_secret: &str,
    identity: &crate::types::PluginIdentity,
) -> Option<String> {
    use crate::types::PluginIdentity;
    match identity {
        PluginIdentity::WpmmccAts => build_wp_base_url(domain_base, route_secret),
        PluginIdentity::Wpmmcc => {
            let secret = route_secret.trim();
            if secret.is_empty() {
                return None;
            }
            let base = domain_base.trim().trim_end_matches('/');
            Some(format!("{}/wp-json/wpmmcc/v1/{}/sync", base, secret))
        }
    }
}

pub(super) fn domain_token_binding_status_items(
    doc: &DomainTokenBindingsDoc,
) -> Vec<DomainTokenBindingStatusItem> {
    let mut items: Vec<DomainTokenBindingStatusItem> = doc
        .domains
        .iter()
        .filter_map(|(domain_key, entry)| {
            let token = entry.wp_client_token.trim();
            if token.is_empty() {
                return None;
            }
            Some(DomainTokenBindingStatusItem {
                api_base_url: domain_key.clone(),
                token_prefix: session_token_prefix(token),
                token_len: token.len(),
                route_secret_set: !entry.route_secret.trim().is_empty(),
                plugin_identity: entry
                    .plugin_identity
                    .as_ref()
                    .map(|identity| identity.as_wire_str().to_string()),
                identity_verified_at: entry.identity_verified_at.clone(),
            })
        })
        .collect();
    items.sort_by(|a, b| a.api_base_url.cmp(&b.api_base_url));
    items
}

pub(super) fn domain_token_binding_local_sites(
    doc: &DomainTokenBindingsDoc,
) -> Vec<DomainStatusItem> {
    let mut seen = HashSet::new();
    let mut items: Vec<DomainStatusItem> = doc
        .domains
        .iter()
        .filter_map(|(domain_key, entry)| {
            if entry.wp_client_token.trim().is_empty() {
                return None;
            }
            let domain_base = normalize_domain_base(domain_key);
            let api_base_url = if domain_base.is_empty() {
                normalize_api_base_url_key(domain_key)
            } else {
                domain_base
            };
            if api_base_url.is_empty() || !seen.insert(api_base_url.clone()) {
                return None;
            }
            let route_secret = entry.route_secret.trim().to_string();
            Some(DomainStatusItem {
                api_base_url,
                site_status: "active".to_string(),
                route_secret: if route_secret.is_empty() {
                    None
                } else {
                    Some(route_secret)
                },
                max_relations: None,
                plan_expires_at: None,
            })
        })
        .collect();
    items.sort_by(|a, b| a.api_base_url.cmp(&b.api_base_url));
    items
}

pub(super) fn resolve_local_dev_binding(
    bindings_doc: &DomainTokenBindingsDoc,
    occupied_domain_bases: &HashSet<String>,
) -> Option<(String, String, Option<String>)> {
    let mut keys: Vec<String> = bindings_doc.domains.keys().cloned().collect();
    keys.sort();

    for key in keys {
        let domain_base = normalize_domain_base(&key);
        if domain_base.is_empty() || occupied_domain_bases.contains(&domain_base) {
            continue;
        }
        if let Some(entry) = bindings_doc.domains.get(&key) {
            let token = entry.wp_client_token.trim();
            if token.is_empty() {
                continue;
            }
            let route_secret = {
                let secret = entry.route_secret.trim();
                if secret.is_empty() {
                    None
                } else {
                    Some(secret.to_string())
                }
            };
            return Some((domain_base, token.to_string(), route_secret));
        }
    }

    None
}
