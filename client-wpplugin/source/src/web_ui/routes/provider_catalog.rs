//! Local provider catalog, catalog installation, and portable integration packs.
//!
//! The catalog is deliberately a local cache/bundled document.  It is never
//! fetched from the website control plane.  Public catalog entries contain
//! templates only; credentials are accepted only from the local credential
//! stores and are never part of a catalog entry.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use rand::RngCore;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::bindings::{
    load_proxy_profiles, load_vendor_keys, load_vendor_oauth, numeric_scope_keys,
    pack_has_cross_site_numeric_risk, sanitize_rule_bindings_for_public_pack, save_proxy_profiles,
    save_vendor_keys, save_vendor_oauth, strip_numeric_maps_on_import,
};
use crate::logging::unix_ts;
use crate::types::*;

use super::components::{
    load_local_components_runtime_doc, local_component_capability_id,
    save_component_bindings_runtime_doc, save_local_components_runtime_doc,
    save_rule_component_bindings_runtime_doc, save_task_type_component_bindings_runtime_doc,
};
use super::errors::{
    err_public, write_conflict_response, write_error_response, write_error_response_with_status,
};
use super::http::{parse_query_string, write_http_response};
use super::{proxy_profiles_path, vendor_keys_path, vendor_oauth_path};

mod cache_journal;

const PROVIDER_CATALOG_SCHEMA: &str = "wptsall-provider-catalog-manifest.v1";
const INTEGRATION_PACK_SCHEMA: &str = "wptsall-integration-pack.v1";
const PROVIDER_CATALOG_SIGNATURE_SCOPE: &str = "provider-catalog-entries-json";
const PROVIDER_CATALOG_DOCUMENT_SIGNATURE_SCOPE: &str = "provider-catalog-document-json-v2";
// A compact 2 MiB source can grow when the cache is pretty-printed.
const MAX_CATALOG_CACHE_BYTES: u64 = 16 * 1024 * 1024;
const PRIVATE_BACKUP_SCHEMA: &str = "wptsall-private-backup.v1";
const PRIVATE_BACKUP_KDF_INFO: &[u8] = b"wptsall-integration-pack-private-v1";

fn validate_backup_passphrase(passphrase: Option<&str>) -> anyhow::Result<&str> {
    let passphrase = passphrase
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("private backup passphrase is required"))?;
    if passphrase.chars().count() < 8 {
        anyhow::bail!("private backup passphrase must contain at least 8 characters");
    }
    if passphrase.len() > 4096 {
        anyhow::bail!("private backup passphrase is too long");
    }
    Ok(passphrase)
}

fn encrypt_private_backup(plaintext: &[u8], passphrase: &str) -> anyhow::Result<Vec<u8>> {
    let mut salt = [0u8; 16];
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce);

    let hkdf = Hkdf::<Sha256>::new(Some(&salt), passphrase.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(PRIVATE_BACKUP_KDF_INFO, &mut key)
        .map_err(|_| anyhow!("derive private backup key failed"))?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow!("initialize private backup cipher failed"))?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow!("encrypt private backup failed"))?;

    serde_json::to_vec(&json!({
        "schema": PRIVATE_BACKUP_SCHEMA,
        "algorithm": "AES-256-GCM",
        "kdf": "HKDF-SHA256",
        "kdf_info": String::from_utf8_lossy(PRIVATE_BACKUP_KDF_INFO),
        "salt_base64": BASE64_STANDARD.encode(salt),
        "nonce_base64": BASE64_STANDARD.encode(nonce),
        "ciphertext_base64": BASE64_STANDARD.encode(ciphertext),
    }))
    .context("encode private backup envelope failed")
}

fn decrypt_private_backup(data: &[u8], passphrase: Option<&str>) -> anyhow::Result<String> {
    let passphrase = validate_backup_passphrase(passphrase)?;
    let envelope: Value =
        serde_json::from_slice(data).context("parse private backup envelope failed")?;
    if envelope.get("schema").and_then(Value::as_str) != Some(PRIVATE_BACKUP_SCHEMA) {
        anyhow::bail!("unsupported private backup schema");
    }
    if envelope.get("algorithm").and_then(Value::as_str) != Some("AES-256-GCM")
        || envelope.get("kdf").and_then(Value::as_str) != Some("HKDF-SHA256")
    {
        anyhow::bail!("unsupported private backup encryption algorithm");
    }
    let salt = BASE64_STANDARD
        .decode(
            envelope
                .get("salt_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("private backup salt is missing"))?,
        )
        .context("decode private backup salt failed")?;
    let nonce = BASE64_STANDARD
        .decode(
            envelope
                .get("nonce_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("private backup nonce is missing"))?,
        )
        .context("decode private backup nonce failed")?;
    let ciphertext = BASE64_STANDARD
        .decode(
            envelope
                .get("ciphertext_base64")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("private backup ciphertext is missing"))?,
        )
        .context("decode private backup ciphertext failed")?;
    if salt.len() != 16 || nonce.len() != 12 || ciphertext.len() < 16 {
        anyhow::bail!("private backup envelope has invalid lengths");
    }

    let hkdf = Hkdf::<Sha256>::new(Some(&salt), passphrase.as_bytes());
    let mut key = [0u8; 32];
    hkdf.expand(PRIVATE_BACKUP_KDF_INFO, &mut key)
        .map_err(|_| anyhow!("derive private backup key failed"))?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow!("initialize private backup cipher failed"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
        .map_err(|_| anyhow!("private backup passphrase is incorrect or backup is corrupt"))?;
    String::from_utf8(plaintext).context("decrypted private backup is not valid UTF-8")
}

fn provider_catalog_path() -> String {
    crate::config::provider_catalog_file()
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

/// Manifest envelope schemas this client understands. v1 documents may still
/// exist in user caches from the builtin era; the official template repo
/// publishes v2 (template-schema gate + provenance/evidence/examples required
/// on every template).
const PROVIDER_CATALOG_SCHEMA_V2: &str = "wptsall-provider-catalog-manifest.v2";
const PROVIDER_TEMPLATE_SCHEMA_V2: &str = "wptsall-provider-template.v2";

/// The single official provider-template repository (decision 5: no
/// third-party source switching). Overrides are test/CI-only and require an
/// explicit allow flag.
const OFFICIAL_PROVIDER_CATALOG_SOURCE_URL: &str =
    "https://raw.githubusercontent.com/wpmmcc/wptsall-provider-templates/main/catalog.json";

/// Public verification key of the official template repo catalog. Its private
/// key is held offline and is deliberately separate from the OTA signing key
/// (doc 03 §3 key separation). The env override is test/CI-only.
const OFFICIAL_PROVIDER_CATALOG_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAq7BWh1CTcQr63B5uhSbo\nypIlnmvFoUlsoUVmHPn0qTR7lTk6w2RcsJZ+XuwtGsscM4lmXVnxBMzx2Z47Eqo0\n3bgDTA6UpeBLWbJlV/a+Vf6g9erXHxb/Vjm0ym0AbrRW4Z4YgRdc+Zt2OI9obM7M\nWsoz69/+pz7sxkCfyU88kLcBEXAkYnR1Uj10KhYDSLfq/sRVz6wC+f8lN1+aofzi\nDRIDU3HXWPSbGtU0itg7jkJMGQXAn0aQjoV4dbMaauKuYKSMLtM4xpkJngbbDSeI\nypUBAS6rQ1jgl/SDr2yceBPozWhXPyUNk19beuPU3wzYauIg2eAUqAlj0Ys5+HrZ\n+wIDAQAB\n-----END PUBLIC KEY-----\n";

/// Embedded seed provider templates (WBS 3.4 / Revised Decision 2):
/// 15 core provider templates compiled into binary to provide immediate
/// out-of-the-box offline usability on fresh installations.
static SEED_PROVIDER_CATALOG: &str = include_str!("../../../config/seed-provider-catalog.json");

fn load_catalog_document() -> anyhow::Result<(Value, bool)> {
    load_catalog_document_with_meta().map(|(catalog, builtin, _)| (catalog, builtin))
}

#[derive(Debug, Clone)]
struct CatalogLoadMeta {
    source: String,
    offline: bool,
    state: String,
    catalog_version: Option<String>,
}

fn catalog_cache_paths() -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let legacy = std::path::PathBuf::from(provider_catalog_path());
    let parent = legacy
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let stem = legacy
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("provider-catalog")
        .to_string();
    (
        legacy,
        parent.join(format!("{stem}.current.json")),
        parent.join(format!("{stem}.lkg.json")),
        parent.join(format!("{stem}.metadata.json")),
    )
}

fn read_validate_catalog_file(path: &Path, builtin: bool) -> anyhow::Result<Value> {
    let raw = read_cache_snapshot(path)?.context("provider catalog cache is missing")?;
    let catalog: Value = serde_json::from_slice(&raw)
        .with_context(|| format!("parse provider catalog JSON failed: {}", path.display()))?;
    validate_catalog_document(&catalog, builtin)?;
    Ok(catalog)
}

fn load_refresh_metadata() -> Value {
    let (_, _, _, meta_path) = catalog_cache_paths();
    if !meta_path.exists() {
        return json!({
            "state": "builtin",
            "offline": true,
        });
    }
    fs::read_to_string(&meta_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({ "state": "unknown", "offline": true }))
}

fn write_refresh_metadata(meta: &Value) -> anyhow::Result<()> {
    let (_, current, _, meta_path) = catalog_cache_paths();
    let lease = cache_journal::acquire(&current)?;
    cache_journal::recover(&current, &lease)?;
    let prior = read_cache_snapshot(&meta_path)?;
    let encoded = serde_json::to_vec_pretty(meta)?;
    anyhow::ensure!(
        u64::try_from(encoded.len())? <= MAX_CATALOG_CACHE_BYTES,
        "catalog metadata snapshot exceeds max size"
    );
    crate::bindings::atomic_file::install_siblings_uncredited(
        &current,
        &[(&meta_path, &encoded, prior.as_deref())],
    )
}

fn read_cache_snapshot(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect catalog cache snapshot failed"),
    };
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= MAX_CATALOG_CACHE_BYTES,
        "invalid catalog cache snapshot"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_CATALOG_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        u64::try_from(bytes.len())? == metadata.len(),
        "catalog cache snapshot changed"
    );
    Ok(Some(bytes))
}

fn load_catalog_document_with_meta() -> anyhow::Result<(Value, bool, CatalogLoadMeta)> {
    let (legacy, current, lkg, _) = catalog_cache_paths();
    let _lease = if current.parent().is_some_and(Path::exists) {
        let lease = cache_journal::acquire(&current)?;
        cache_journal::recover(&current, &lease)?;
        Some(lease)
    } else {
        None
    };

    if current.exists() {
        match read_validate_catalog_file(&current, false) {
            Ok(catalog) => {
                let version = catalog
                    .get("catalog_version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Ok((
                    catalog,
                    false,
                    CatalogLoadMeta {
                        source: "current_cache".into(),
                        offline: true,
                        state: "current".into(),
                        catalog_version: version,
                    },
                ));
            }
            Err(err) => {
                // Fall through to LKG; keep error for metadata if LKG also fails.
                let _ = err;
            }
        }
    }

    if lkg.exists() {
        match read_validate_catalog_file(&lkg, false) {
            Ok(catalog) => {
                let version = catalog
                    .get("catalog_version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                return Ok((
                    catalog,
                    false,
                    CatalogLoadMeta {
                        source: "last_known_good".into(),
                        offline: true,
                        state: "last_known_good".into(),
                        catalog_version: version,
                    },
                ));
            }
            Err(_) => {}
        }
    }

    if legacy.exists() {
        let catalog = read_validate_catalog_file(&legacy, false)?;
        let version = catalog
            .get("catalog_version")
            .and_then(Value::as_str)
            .map(str::to_string);
        return Ok((
            catalog,
            false,
            CatalogLoadMeta {
                source: "local_file".into(),
                offline: true,
                state: "local_file".into(),
                catalog_version: version,
            },
        ));
    }

    // Embedded seed fallback (WBS 3.4 / Revised Decision 2):
    // When no local cache exists, load the embedded 15 core provider seed templates
    // so fresh or air-gapped installs are immediately operational.
    if let Ok(seed_catalog) = serde_json::from_str::<Value>(SEED_PROVIDER_CATALOG) {
        if validate_catalog_document(&seed_catalog, true).is_ok() {
            let version = seed_catalog
                .get("catalog_version")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Ok((
                seed_catalog,
                true,
                CatalogLoadMeta {
                    source: "embedded_seed".into(),
                    offline: true,
                    state: "seed".into(),
                    catalog_version: version,
                },
            ));
        }
    }

    // Clean release fallback: if embedded seed fails to parse, return empty document.
    let catalog = json!({
        "schema": PROVIDER_CATALOG_SCHEMA_V2,
        "template_schema": PROVIDER_TEMPLATE_SCHEMA_V2,
        "catalog_version": Value::Null,
        "entries": [],
    });
    Ok((
        catalog,
        false,
        CatalogLoadMeta {
            source: "never_fetched".into(),
            offline: true,
            state: "never_fetched".into(),
            catalog_version: None,
        },
    ))
}

/// Catalog source override is test/CI-only (decision 5: the client loads the
/// official repository and no other). An env override is honored only when an
/// explicit allow flag is set, mirroring the file/loopback source gating.
fn catalog_source_override_allowed() -> bool {
    env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE")
        || env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_LOOPBACK_SOURCE")
        || env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_UNOFFICIAL_SOURCE")
        || false
}

fn catalog_source_url() -> Option<String> {
    if let Some(value) = std::env::var("WPTSALL_PROVIDER_CATALOG_SOURCE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        if !catalog_source_override_allowed() {
            // Silently ignore unofficial sources in production: the official
            // repository is the only supported catalog source.
            return Some(OFFICIAL_PROVIDER_CATALOG_SOURCE_URL.to_string());
        }
        return Some(value);
    }
    Some(OFFICIAL_PROVIDER_CATALOG_SOURCE_URL.to_string())
}

/// Base URL of the catalog source (the repo root behind `catalog.json`);
/// used to resolve `versions.json` and `versions/<v>/catalog.json`.
fn catalog_versions_base_url(source_url: &str) -> String {
    let trimmed = source_url.trim_end_matches('/');
    let base = trimmed.strip_suffix("catalog.json").unwrap_or(trimmed);
    format!("{base}/")
}

fn catalog_versions_index_url() -> Option<String> {
    catalog_source_url()
        .map(|source| format!("{}versions.json", catalog_versions_base_url(&source)))
}

fn catalog_version_url(version: &str) -> Option<String> {
    catalog_source_url().map(|source| {
        format!(
            "{}versions/{version}/catalog.json",
            catalog_versions_base_url(&source)
        )
    })
}

fn assert_catalog_source_url_allowed(raw: &str) -> anyhow::Result<()> {
    let parsed =
        url::Url::parse(raw.trim()).map_err(|_| anyhow!("catalog source URL is invalid"))?;
    if parsed.scheme() == "file" {
        if false || env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_FILE_SOURCE") {
            return Ok(());
        }
        anyhow::bail!("file:// catalog source is not allowed");
    }
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("catalog source URL scheme is not allowed");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        anyhow::bail!("catalog source URL must not contain embedded credentials");
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("catalog source URL host is missing"))?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let allow_loopback = env_flag("WPTSALL_PROVIDER_CATALOG_ALLOW_LOOPBACK_SOURCE")
        || (false
            && parsed.port().is_some()
            && matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"));
    if !allow_loopback {
        // Reuse provider host policy for public HTTPS sources.
        if let Err(reason) = catalog_url_check(raw) {
            anyhow::bail!("catalog source URL rejected: {reason}");
        }
        if parsed.scheme() != "https" {
            anyhow::bail!("catalog source URL must use https");
        }
    }
    Ok(())
}

/// Install admitted private snapshots, preserving prior current as LKG.
/// A checked publication journal repairs interrupted related renames.
fn install_catalog_candidate(catalog: &Value, source_url: &str) -> anyhow::Result<Value> {
    let (_legacy, current, lkg, metadata) = catalog_cache_paths();
    let lease = cache_journal::acquire(&current)?;
    cache_journal::recover(&current, &lease)?;
    let prior_current = read_cache_snapshot(&current)?;
    let prior_lkg = read_cache_snapshot(&lkg)?;
    let prior_metadata = read_cache_snapshot(&metadata)?;
    let encoded = serde_json::to_vec_pretty(catalog)?;
    let meta = json!({
        "state": "current",
        "source_url": source_url,
        "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
        "last_success_at": unix_ts(),
        "last_error": Value::Null,
        "offline": false,
        "template_count": catalog_template_entries(catalog).map(|v| v.len()).unwrap_or(0),
    });
    let encoded_meta = serde_json::to_vec_pretty(&meta)?;
    anyhow::ensure!(
        u64::try_from(encoded.len())? <= MAX_CATALOG_CACHE_BYTES
            && u64::try_from(encoded_meta.len())? <= MAX_CATALOG_CACHE_BYTES,
        "catalog cache snapshot exceeds max size"
    );
    let mut snapshots = Vec::new();
    if let Some(prior) = prior_current.as_ref().filter(|bytes| {
        serde_json::from_slice::<Value>(bytes)
            .ok()
            .is_some_and(|prior| validate_catalog_document(&prior, false).is_ok())
    }) {
        snapshots.push((lkg.as_path(), prior.as_slice(), prior_lkg.as_deref()));
    }
    snapshots.push((
        current.as_path(),
        encoded.as_slice(),
        prior_current.as_deref(),
    ));
    snapshots.push((
        metadata.as_path(),
        encoded_meta.as_slice(),
        prior_metadata.as_deref(),
    ));
    cache_journal::publish(&current, &snapshots, &lease)?;
    Ok(meta)
}

async fn download_catalog_bytes(source_url: &str) -> anyhow::Result<Vec<u8>> {
    assert_catalog_source_url_allowed(source_url)?;
    const MAX_BYTES: usize = 2 * 1024 * 1024;

    if source_url.starts_with("file://") {
        use std::io::Read;
        let path = source_url.trim_start_matches("file://");
        let metadata = fs::symlink_metadata(path)?;
        anyhow::ensure!(metadata.is_file() && metadata.len() <= MAX_BYTES as u64,
            "file catalog source exceeds its bound or is not regular");
        let mut bytes = Vec::new();
        fs::File::open(path)?.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= MAX_BYTES && bytes.len() as u64 == metadata.len(),
            "file catalog source changed or exceeds its bound");
        return Ok(bytes);
    }

    let client = reqwest::Client::builder()
        // 3.8flash A3: 3s, not the old 30s — the first catalog view must never
        // stare at a half-minute black hole while the network is unreachable.
        // The fetch failure arm already degrades honestly (built-in seed +
        // offline refresh metadata); a long timeout only punishes the UI.
        .timeout(std::time::Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error(anyhow!("too many redirects"));
            }
            let next = attempt.url().as_str();
            if let Err(err) = assert_catalog_source_url_allowed(next) {
                return attempt.error(err);
            }
            attempt.follow()
        }))
        .build()
        .context("build catalog download client failed")?;

    let mut response = client
        .get(source_url)
        .header(
            reqwest::header::ACCEPT,
            "application/json, application/octet-stream",
        )
        .send()
        .await
        .map_err(|error| anyhow!("catalog source download failed ({})",
            if error.is_timeout() { "timeout" } else { "transport" }))?;
    if !response.status().is_success() {
        anyhow::bail!("catalog source HTTP {}", response.status());
    }
    if let Some(len) = response.content_length() {
        if len > MAX_BYTES as u64 {
            anyhow::bail!("catalog source Content-Length exceeds max size");
        }
    }
    let final_url = response.url().clone();
    assert_catalog_source_url_allowed(final_url.as_str())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await
        .map_err(|_| anyhow!("catalog source body read failed"))? {
        anyhow::ensure!(bytes.len().checked_add(chunk.len()).is_some_and(|size| size <= MAX_BYTES),
            "catalog source body exceeds max size");
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn refresh_provider_catalog_from_source() -> anyhow::Result<Value> {
    let Some(source_url) = catalog_source_url() else {
        let (catalog, builtin, meta) = load_catalog_document_with_meta()?;
        let count = catalog_template_entries(&catalog)?.len();
        return Ok(json!({
            "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
            "template_count": count,
            "source": meta.source,
            "verified": builtin || catalog.get("signature").is_some(),
            "offline": true,
            "refresh_metadata": {
                "state": meta.state,
                "offline": true,
                "reason": "no_source_configured",
            },
        }));
    };

    match download_catalog_bytes(&source_url).await {
        Ok(bytes) => {
            let catalog: Value = serde_json::from_slice(&bytes)
                .context("parse remote provider catalog JSON failed")?;
            // Remote catalogs are fail-closed: signature always required.
            if let Err(err) = validate_catalog_document_inner(&catalog, false, true) {
                let (_, _, load_meta) = load_catalog_document_with_meta().unwrap_or((
                    Value::Null,
                    false,
                    CatalogLoadMeta {
                        source: "never_fetched".into(),
                        offline: true,
                        state: "never_fetched".into(),
                        catalog_version: None,
                    },
                ));
                let meta = json!({
                    "state": if load_meta.state == "current" { "stale_current" } else { load_meta.state.as_str() },
                    "source_url": source_url,
                    "last_error": format!("{err:#}"),
                    "offline": true,
                    "last_attempt_at": unix_ts(),
                });
                let _ = write_refresh_metadata(&meta);
                return Err(err).context("remote provider catalog validation failed");
            }
            // Browsing/checking must not replace an installed catalog or any
            // user component. Installation is an explicit version-confirmed POST.
            let (current, _, current_meta) = load_catalog_document_with_meta()?;
            let meta = json!({
                "state": current_meta.state, "source_url": source_url,
                "catalog_version": current.get("catalog_version").cloned().unwrap_or(Value::Null),
                "available_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "available_entries_sha256": catalog.get("entries_sha256").cloned().unwrap_or(Value::Null),
                "last_success_at": unix_ts(), "last_attempt_at": unix_ts(),
                "last_error": Value::Null, "offline": false,
            });
            write_refresh_metadata(&meta)?;
            let count = catalog_template_entries(&catalog)?.len();
            Ok(json!({
                "catalog_version": current.get("catalog_version").cloned().unwrap_or(Value::Null),
                "available_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "template_count": count,
                "source": "remote_check",
                "installed": false,
                "verified": true,
                "offline": false,
                "refresh_metadata": meta,
            }))
        }
        Err(err) => {
            let (catalog, builtin, load_meta) = load_catalog_document_with_meta()?;
            let count = catalog_template_entries(&catalog)?.len();
            let fallback_state = if load_meta.state == "current" {
                "stale_current"
            } else {
                load_meta.state.as_str()
            };
            let meta = json!({
                "state": fallback_state,
                "source_url": source_url,
                "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "last_error": format!("{err:#}"),
                "offline": true,
                "last_attempt_at": unix_ts(),
            });
            let _ = write_refresh_metadata(&meta);
            Ok(json!({
                "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                "template_count": count,
                "source": load_meta.source,
                "verified": builtin || catalog.get("signature").is_some(),
                "offline": true,
                "refresh_metadata": meta,
                "fallback": true,
            }))
        }
    }
}

fn hex_sha256(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn catalog_signature_key() -> anyhow::Result<Option<String>> {
    if let Some(path) = std::env::var("WPTSALL_PROVIDER_CATALOG_PUBLIC_KEY_FILE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        // Test/CI-only key override, gated exactly like the source override.
        if catalog_source_override_allowed() {
            return Ok(Some(fs::read_to_string(&path).with_context(|| {
                format!("read provider catalog public key failed: {path}")
            })?));
        }
    }
    // Default: the official template-repo catalog key (embedded at build time
    // so the clean client verifies the official source out of the box).
    Ok(Some(OFFICIAL_PROVIDER_CATALOG_PUBLIC_KEY_PEM.to_string()))
}

fn validate_catalog_document(catalog: &Value, builtin: bool) -> anyhow::Result<()> {
    let require_signature = !builtin && !env_flag("WPTSALL_ALLOW_UNSIGNED_CATALOG");
    validate_catalog_document_contract(catalog, builtin, require_signature, false)
}

fn validate_catalog_document_inner(
    catalog: &Value,
    builtin: bool,
    require_signature: bool,
) -> anyhow::Result<()> {
    validate_catalog_document_contract(catalog, builtin, require_signature, require_signature)
}

fn validate_catalog_document_contract(
    catalog: &Value,
    _builtin: bool,
    require_signature: bool,
    require_metadata_signature: bool,
) -> anyhow::Result<()> {
    let schema = catalog
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_v2 = match schema {
        s if s == PROVIDER_CATALOG_SCHEMA => false,
        s if s == PROVIDER_CATALOG_SCHEMA_V2 => true,
        _ => anyhow::bail!("unsupported provider catalog schema"),
    };
    if is_v2 {
        // Fail-closed on unknown template schema declarations.
        if catalog.get("template_schema").and_then(Value::as_str)
            != Some(PROVIDER_TEMPLATE_SCHEMA_V2)
        {
            anyhow::bail!("unsupported provider catalog template schema");
        }
        let version = catalog
            .get("catalog_version")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !is_semver_3(version) {
            anyhow::bail!("v2 provider catalog catalog_version must be SemVer");
        }
    }
    let entries = catalog
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider catalog entries must be an array"))?;
    if entries.len() > 1000 {
        anyhow::bail!("provider catalog contains too many entries");
    }

    let entries_json = serde_json::to_vec(entries)?;
    if let Some(expected) = catalog.get("entries_sha256").and_then(Value::as_str) {
        if !expected.eq_ignore_ascii_case(&hex_sha256(&entries_json)) {
            anyhow::bail!("provider catalog entries hash mismatch");
        }
    }

    let signature = catalog
        .get("signature")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty());
    if let Some(signature) = signature {
        let scope = catalog
            .get("signature_scope")
            .and_then(Value::as_str)
            .unwrap_or(PROVIDER_CATALOG_SIGNATURE_SCOPE);
        anyhow::ensure!(
            !require_metadata_signature || scope == PROVIDER_CATALOG_DOCUMENT_SIGNATURE_SCOPE,
            "online provider catalog must sign its full version metadata"
        );
        let payload = match scope {
            PROVIDER_CATALOG_SIGNATURE_SCOPE => entries_json.clone(),
            PROVIDER_CATALOG_DOCUMENT_SIGNATURE_SCOPE => {
                anyhow::ensure!(
                    catalog.get("catalog_version").and_then(Value::as_str).is_some_and(is_semver_3)
                        && catalog.get("entries_sha256").and_then(Value::as_str).is_some_and(|value|
                            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
                        && catalog.get("signature_algorithm").and_then(Value::as_str) == Some("RSA-SHA256")
                        && catalog.get("signature_contract_version").and_then(Value::as_str) == Some("2")
                        && catalog.get("signing_key_id").and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()),
                    "provider catalog signed metadata is incomplete"
                );
                let mut document = catalog.clone();
                document.as_object_mut().context("catalog must be an object")?.remove("signature");
                serde_json::to_vec(&document)?
            }
            _ => anyhow::bail!("unsupported provider catalog signature scope"),
        };
        let public_key = catalog_signature_key()?.ok_or_else(|| {
            anyhow!("provider catalog signature is present but no verification key is configured")
        })?;
        crate::crypto::verify_component_signature(&payload, signature, &public_key)
            .context("provider catalog signature verification failed")?;
    } else if require_signature {
        anyhow::bail!("provider catalog signature is required for this catalog source");
    }

    for entry in entries {
        let entry_id = entry
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow!("provider catalog entry id is required"))?;
        let templates = entry_templates(entry, entry_id)?;
        if templates.is_empty() {
            anyhow::bail!("provider catalog entry '{entry_id}' has no templates");
        }
        for template in templates {
            validate_public_catalog_template(template, entry_id)?;
            if is_v2 {
                validate_v2_template_requirements(template, entry_id)?;
            }
        }
    }
    Ok(())
}

fn is_semver_3(value: &str) -> bool {
    let mut parts = value.trim().split('.');
    let mut numeric = 0;
    for part in parts.by_ref() {
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return false;
        }
        numeric += 1;
    }
    numeric == 3
}

/// v2 template gate (decision 7): provenance, evidence, examples, and the
/// client-contract api_version are required on every v2 catalog template, and
/// auth placeholders must be declared in auth.fields (the P7 lesson: an
/// undeclared `{{auth.*}}` renders literally and burns vendor quota with 401s).
fn validate_v2_template_requirements(template: &Value, entry_id: &str) -> anyhow::Result<()> {
    let api_version = template
        .get("api_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("v2 catalog entry '{entry_id}' template requires api_version"))?;
    let major = api_version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
        .ok_or_else(|| {
            anyhow!("v2 catalog entry '{entry_id}' template api_version is malformed")
        })?;
    if !(1..=2).contains(&major) {
        anyhow::bail!(
            "v2 catalog entry '{entry_id}' template api_version major {major} is unsupported"
        );
    }

    if !template
        .get("forked_from")
        .is_some_and(|ff| ff.is_null() || ff.is_object())
    {
        anyhow::bail!(
            "v2 catalog entry '{entry_id}' template requires forked_from (null for repo originals)"
        );
    }
    if let Some(ff) = template.get("forked_from").filter(|v| v.is_object()) {
        for key in ["repo", "entry_id", "template_id", "template_version"] {
            if !ff
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|v| !v.trim().is_empty())
            {
                anyhow::bail!(
                    "v2 catalog entry '{entry_id}' forked_from.{key} must be a non-empty string"
                );
            }
        }
    }

    let evidence = template
        .get("evidence")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("v2 catalog entry '{entry_id}' template requires evidence"))?;
    let tier = evidence
        .get("tier")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(tier, "mock-verified" | "schema-only") {
        anyhow::bail!("v2 catalog entry '{entry_id}' evidence.tier '{tier}' is invalid");
    }
    let cases = evidence
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("v2 catalog entry '{entry_id}' evidence.cases must be an array"))?;
    if cases
        .iter()
        .any(|c| !c.as_str().is_some_and(|s| !s.trim().is_empty()))
    {
        anyhow::bail!("v2 catalog entry '{entry_id}' evidence.cases must be case-id strings");
    }
    if tier == "mock-verified" {
        let verified_at = evidence
            .get("verified_at")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !verified_at.ends_with('Z') || verified_at.len() != 20 {
            anyhow::bail!(
                "v2 catalog entry '{entry_id}' mock-verified evidence requires verified_at ISO-8601 Z"
            );
        }
        if cases.is_empty() {
            anyhow::bail!(
                "v2 catalog entry '{entry_id}' mock-verified evidence requires at least one case id"
            );
        }
    }

    let examples = template
        .get("examples")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("v2 catalog entry '{entry_id}' template requires examples"))?;
    if examples.get("request").map(Value::is_object) != Some(true) {
        anyhow::bail!("v2 catalog entry '{entry_id}' examples.request must be an object");
    }
    // Some vendors reply with a top-level array (e.g. Azure
    // `[{ "translations": [...] }]`) — a structurally faithful example.
    if !examples
        .get("response")
        .is_some_and(|r| r.is_object() || r.is_array())
    {
        anyhow::bail!("v2 catalog entry '{entry_id}' examples.response must be an object or array");
    }

    // Placeholder consistency: every {{auth.X}} used in the request must be
    // declared in auth.fields; computed placeholders require a sign block.
    // X-11 (批 S): the scan covers async_poll too, the non-text input
    // vocabulary mirrors the runner's insert_component_non_text_context,
    // and the async computed keys (job_id/async_job_id) are admitted only
    // when the template declares async_poll — parity with the template
    // repo's wptsall_catalog.check_template_v2.
    let declared: HashSet<String> = template
        .get("auth")
        .and_then(|a| a.get("fields"))
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let request = template.get("request").cloned().unwrap_or(Value::Null);
    let mut used = collect_placeholders(&request, &mut String::new());
    let has_async_poll = template
        .get("async_poll")
        .and_then(Value::as_object)
        .is_some_and(|poll| !poll.is_empty());
    if has_async_poll {
        let async_poll = template.get("async_poll").cloned().unwrap_or(Value::Null);
        used.extend(collect_placeholders(&async_poll, &mut String::new()));
    }
    for name in used {
        if let Some(field) = name.strip_prefix("auth.") {
            if !declared.contains(field) {
                anyhow::bail!(
                    "v2 catalog entry '{entry_id}' uses auth placeholder '{name}' not declared in auth.fields"
                );
            }
        } else if let Some(computed) = name.strip_prefix("computed.") {
            let is_async_key = matches!(computed, "job_id" | "async_job_id");
            if !matches!(computed, "salt" | "sign" | "curtime" | "input_truncated") && !is_async_key
            {
                anyhow::bail!(
                    "v2 catalog entry '{entry_id}' uses unknown computed placeholder '{name}'"
                );
            }
            if is_async_key {
                if !has_async_poll {
                    anyhow::bail!(
                        "v2 catalog entry '{entry_id}' uses async computed placeholder '{name}' without an async_poll block"
                    );
                }
            } else if template.get("sign").and_then(Value::as_object).is_none() {
                anyhow::bail!(
                    "v2 catalog entry '{entry_id}' uses computed placeholder '{name}' without a sign block"
                );
            }
        } else if let Some(input) = name.strip_prefix("input.") {
            let is_non_text_input = matches!(
                input,
                "source_ref"
                    | "src"
                    | "source"
                    | "source_url"
                    | "task_type"
                    | "field_key"
                    | "source_payload_json"
            );
            if !matches!(input, "text" | "source_lang" | "target_lang") && !is_non_text_input {
                anyhow::bail!(
                    "v2 catalog entry '{entry_id}' uses unknown input placeholder '{name}'"
                );
            }
        } else {
            anyhow::bail!(
                "v2 catalog entry '{entry_id}' uses unknown placeholder namespace in '{name}'"
            );
        }
    }
    Ok(())
}

fn collect_placeholders(value: &Value, buffer: &mut String) -> Vec<String> {
    /// Inline placeholder scanner: avoids a full regex dependency and keeps
    /// the scan allocation-free until the final dedup.
    fn scan(text: &str, buffer: &mut String) {
        let bytes = text.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'{' && bytes[i + 1] == b'{' {
                if let Some(end) = text[i + 2..].find("}}") {
                    let inner = &text[i + 2..i + 2 + end];
                    let name = inner.trim();
                    if !name.is_empty()
                        && name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
                    {
                        buffer.push_str(name);
                        buffer.push('\n');
                    }
                    i += 2 + end + 2;
                    continue;
                }
            }
            i += 1;
        }
    }

    fn walk(value: &Value, buffer: &mut String) {
        match value {
            Value::String(text) => scan(text, buffer),
            Value::Object(fields) => {
                for child in fields.values() {
                    walk(child, buffer);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, buffer);
                }
            }
            _ => {}
        }
    }

    walk(value, buffer);
    buffer
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn entry_templates<'a>(entry: &'a Value, entry_id: &str) -> anyhow::Result<Vec<&'a Value>> {
    if let Some(template) = entry.get("template") {
        if !template.is_object() {
            anyhow::bail!("provider catalog entry '{entry_id}' template must be an object");
        }
        return Ok(vec![template]);
    }
    if let Some(templates) = entry.get("templates").and_then(Value::as_array) {
        if templates.iter().any(|template| !template.is_object()) {
            anyhow::bail!("provider catalog entry '{entry_id}' templates must be objects");
        }
        return Ok(templates.iter().collect());
    }
    anyhow::bail!("provider catalog entry '{entry_id}' has no inline template")
}

fn catalog_template_entries(catalog: &Value) -> anyhow::Result<Vec<CatalogTemplateEntry>> {
    let entries = catalog
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("provider catalog entries must be an array"))?;
    let verified = (catalog
        .get("signature_scope").and_then(Value::as_str) == Some(PROVIDER_CATALOG_DOCUMENT_SIGNATURE_SCOPE)
        && catalog
        .get("signature")
        .and_then(Value::as_str)
        .is_some_and(|signature| !signature.trim().is_empty()))
        || catalog.get("source").and_then(Value::as_str) == Some("builtin")
        || catalog
            .get("catalog_version")
            .and_then(Value::as_str)
            .is_some_and(|v| matches!(v, "builtin-1" | "builtin-2" | "builtin-3"));
    let mut output = Vec::new();
    for entry in entries {
        let entry_id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("provider catalog entry id is required"))?;
        let vendor_id = entry
            .get("vendor_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let family = entry
            .get("family")
            .or_else(|| entry.get("family_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("local")
            .to_string();
        for template in entry_templates(entry, entry_id)? {
            let template_id = template
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("catalog template id is required"))?;
            output.push(CatalogTemplateEntry {
                entry_id: entry_id.to_string(),
                template_id: template_id.to_string(),
                vendor_id: if vendor_id.is_empty() {
                    template
                        .get("vendor_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                } else {
                    vendor_id.clone()
                },
                family: family.clone(),
                source: source.clone(),
                verified,
                template: template.clone(),
            });
        }
    }
    Ok(output)
}

#[derive(Debug, Clone)]
struct CatalogTemplateEntry {
    entry_id: String,
    template_id: String,
    vendor_id: String,
    family: String,
    source: String,
    verified: bool,
    template: Value,
}

fn catalog_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    normalized == "authvalues"
        || normalized == "clientsecret"
        || normalized == "cachedtoken"
        || normalized == "refreshtoken"
        || normalized == "wpclienttoken"
        || normalized == "routesecret"
        || normalized == "password"
        || normalized == "pairingcode"
}

fn validate_public_catalog_template(template: &Value, entry_id: &str) -> anyhow::Result<()> {
    if let Some(path) = find_catalog_secret_field(template, "template") {
        anyhow::bail!("provider catalog entry '{entry_id}' contains secret field {path}");
    }
    serde_json::from_value::<ComponentTemplate>(template.clone())
        .with_context(|| format!("invalid component template in catalog entry '{entry_id}'"))?;
    Ok(())
}

fn find_catalog_secret_field(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let child_path = format!("{path}.{key}");
                if catalog_sensitive_key(key) {
                    if !child.is_null()
                        && (!child.is_string() || !child.as_str().unwrap_or("").is_empty())
                    {
                        return Some(child_path);
                    }
                }
                if let Some(found) = find_catalog_secret_field(child, &child_path) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().enumerate().find_map(|(index, child)| {
            find_catalog_secret_field(child, &format!("{path}[{index}]"))
        }),
        _ => None,
    }
}

fn catalog_url_check(raw: &str) -> Result<(), String> {
    let value = raw.trim();
    if value.contains("{{") || value.contains("}}") {
        // Runtime renders config placeholders before the final SSRF check.
        return Ok(());
    }
    let parsed = url::Url::parse(value).map_err(|_| "provider URL is invalid".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("provider URL scheme is not allowed".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("provider URL must not contain embedded credentials".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "provider URL host is missing".to_string())?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    // Product: any http(s) vendor URL may be installed (including loopback
    // mock-api / local LLM). Only block cloud metadata hostnames.
    if host == "metadata.google.internal"
        || host == "metadata"
        || host.ends_with(".metadata.google.internal")
    {
        return Err("provider host is blocked by egress policy".to_string());
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if matches!(ip, std::net::IpAddr::V4(v4) if v4.octets() == [169, 254, 169, 254]) {
            return Err("provider host is blocked by egress policy".to_string());
        }
    }
    Ok(())
}

fn collect_urls(value: &Value, path: &str, output: &mut Vec<(String, String)>) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields {
                let child_path = format!("{path}.{key}");
                if (key.eq_ignore_ascii_case("url")
                    || key.eq_ignore_ascii_case("endpoint")
                    || key.eq_ignore_ascii_case("token_url")
                    || key.eq_ignore_ascii_case("auth_url"))
                    && child.as_str().is_some()
                {
                    output.push((
                        child_path.clone(),
                        child.as_str().unwrap_or_default().to_string(),
                    ));
                }
                collect_urls(child, &child_path, output);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_urls(child, &format!("{path}[{index}]"), output);
            }
        }
        _ => {}
    }
}

fn catalog_url_report(template: &Value) -> Vec<Value> {
    let mut urls = Vec::new();
    collect_urls(template, "template", &mut urls);
    urls.into_iter()
        .map(|(path, url)| match catalog_url_check(&url) {
            Ok(()) => json!({ "path": path, "url": url, "allowed": true }),
            Err(reason) => json!({ "path": path, "url": url, "allowed": false, "reason": reason }),
        })
        .collect()
}

pub(super) async fn handle_provider_catalog_list(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    query: &str,
) -> anyhow::Result<()> {
    let params = parse_query_string(query);
    // Only the optional catalog page asks for a bounded online check on open.
    // Plain lists/searches and local component loading remain entirely local.
    if params.get("check").is_some_and(|value| value == "1") {
        let _ = refresh_provider_catalog_from_source().await;
    }
    let mut load_meta = match load_catalog_document_with_meta() {
        Ok((catalog, _builtin, meta)) => (catalog, meta),
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "500 Internal Server Error",
                "PROVIDER_CATALOG_INVALID",
                &err_public(&err),
            )
            .await;
        }
    };
    let (catalog, meta) = (&load_meta.0, &mut load_meta.1);
    let needle = params.get("q").map(|v| v.to_ascii_lowercase());
    let vendor_filter = params.get("vendor_id").map(|v| v.to_ascii_lowercase());
    let mut items = Vec::new();
    for entry in catalog_template_entries(catalog)? {
        let template_name = entry
            .template
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&entry.template_id);
        let kind = entry
            .template
            .get("type")
            .or_else(|| entry.template.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("text");
        let searchable = format!(
            "{} {} {} {} {}",
            entry.entry_id, entry.template_id, template_name, entry.vendor_id, entry.family
        )
        .to_ascii_lowercase();
        if needle.as_deref().is_some_and(|v| !searchable.contains(v)) {
            continue;
        }
        if vendor_filter
            .as_deref()
            .is_some_and(|v| !entry.vendor_id.to_ascii_lowercase().contains(v))
        {
            continue;
        }
        let formats = entry
            .template
            .get("constraints")
            .and_then(|v| v.get("supported_content_formats"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        // v2 templates carry structured evidence; legacy caches may still
        // have the flat evidence_tier field. Both are displayed, v2 wins.
        let evidence_tier = entry
            .template
            .get("evidence")
            .and_then(|e| e.get("tier"))
            .and_then(Value::as_str)
            .or_else(|| entry.template.get("evidence_tier").and_then(Value::as_str))
            .unwrap_or("schema-only");
        items.push(json!({
            "entry_id": entry.entry_id,
            "template_id": entry.template_id,
            "name": template_name,
            "vendor_id": entry.vendor_id,
            "family": entry.family,
            "kind": kind,
            "supported_content_formats": formats,
            "source": entry.source,
            "verified": entry.verified,
            "evidence_tier": evidence_tier,
            "api_version": entry.template.get("api_version").cloned().unwrap_or(Value::Null),
            "template_version": entry.template.get("version").cloned().unwrap_or(Value::Null),
            "requires_local_credentials": entry.template.get("auth").is_some(),
            "editable_params": entry
                .template
                .get("editable_params")
                .cloned()
                .unwrap_or_else(|| json!([])),
            "template": entry.template,
        }));
    }
    let refresh_metadata = load_refresh_metadata();
    let first_fetch_required = meta.state == "never_fetched";
    let payload = json!({
        "success": true,
        "data": {
            "schema": catalog.get("schema").cloned().unwrap_or_else(|| json!(PROVIDER_CATALOG_SCHEMA)),
            "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
            "items": items,
            "offline": meta.offline,
            "cache_source": meta.source,
            "cache_state": meta.state,
            "first_fetch_required": first_fetch_required,
            "first_fetch_hint": if first_fetch_required {
                "provider catalog is empty: one online fetch from the official template repository is required (POST /api/provider-catalog/refresh); after that the local cache works offline"
            } else if meta.state == "seed" {
                "loaded embedded seed provider templates; online fetch (POST /api/provider-catalog/refresh) can update to the latest template repository"
            } else {
                ""
            },
            "refresh_metadata": refresh_metadata,
            "available_version": refresh_metadata.get("available_version").cloned().unwrap_or(Value::Null),
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_provider_catalog_refresh(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let data = match refresh_provider_catalog_from_source().await {
        Ok(value) => value,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "PROVIDER_CATALOG_REFRESH_FAILED",
                &err_public(&err),
            )
            .await;
        }
    };
    crate::logging::log_event_global(
        "info",
        "catalog.refreshed",
        json!({
            "catalog_version": data.get("catalog_version").cloned().unwrap_or(Value::Null),
            "template_count": data.get("template_count").cloned().unwrap_or(Value::Null),
            "offline": data.get("offline").cloned().unwrap_or(Value::Null),
        }),
    );
    let payload = json!({
        "success": true,
        "data": data,
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// GET /api/provider-catalog/versions — browse the official repository's
/// frozen releases (versions.json). Records are sanitized: SemVer only, no
/// path traversal; the actual catalogs are individually signature-verified
/// when installed, so a tampered index cannot inject content.
pub(super) async fn handle_provider_catalog_versions(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
) -> anyhow::Result<()> {
    let Some(index_url) = catalog_versions_index_url() else {
        return write_error_response(
            socket,
            "PROVIDER_CATALOG_SOURCE_NOT_CONFIGURED",
            "catalog source is not configured",
        )
        .await;
    };
    let payload = match download_catalog_bytes(&index_url).await {
        Ok(bytes) => {
            let doc: Value = serde_json::from_slice(&bytes)
                .context("parse provider catalog versions index failed")?;
            let raw_versions = doc
                .get("versions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut versions: Vec<Value> = Vec::new();
            for record in raw_versions {
                let version = record
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !is_semver_3(&version) || version.contains("..") {
                    continue; // skip malformed/hostile records
                }
                versions.push(json!({
                    "version": version,
                    "released_at": record.get("released_at").cloned().unwrap_or(Value::Null),
                    "entries_count": record.get("entries_count").cloned().unwrap_or(Value::Null),
                    "entries_sha256": record.get("entries_sha256").cloned().unwrap_or(Value::Null),
                }));
            }
            // newest first (release timestamps are ISO-8601 Z, sortable as text)
            versions.reverse();
            json!({
                "success": true,
                "data": {
                    "source": "remote",
                    "versions": versions,
                }
            })
        }
        Err(err) => json!({
            "success": true,
            "data": {
                "source": "unreachable",
                "versions": [],
                "last_error": format!("{err:#}"),
            }
        }),
    };
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

/// POST /api/provider-catalog/install-version {version,confirm:true} — fetch the frozen
/// catalog of a historical release, verify its signature (fail-closed), and
/// atomically install it as the current cache. Install provenance
/// (source_template_*) then comes from that release's entries.
pub(super) async fn handle_provider_catalog_install_version(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/provider-catalog/install-version payload")?;
    if req.get("confirm").and_then(Value::as_bool) != Some(true) {
        return write_error_response(socket, "CATALOG_CONFIRMATION_REQUIRED",
            "confirm the exact catalog version before installation").await;
    }
    let version = req
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if !is_semver_3(&version) || version.contains("..") {
        return write_error_response(socket, "INVALID_CATALOG_VERSION", "version must be SemVer")
            .await;
    }
    let Some(version_url) = catalog_version_url(&version) else {
        return write_error_response(
            socket,
            "PROVIDER_CATALOG_SOURCE_NOT_CONFIGURED",
            "catalog source is not configured",
        )
        .await;
    };

    let result = match download_catalog_bytes(&version_url).await {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(catalog) => match validate_catalog_document_inner(&catalog, false, true).and_then(|()| {
                anyhow::ensure!(
                    catalog.get("catalog_version").and_then(Value::as_str) == Some(version.as_str()),
                    "historical catalog does not match the requested version"
                );
                Ok(())
            }) {
                Ok(()) => match install_catalog_candidate(&catalog, &version_url) {
                    Ok(meta) => {
                        let count = catalog_template_entries(&catalog)
                            .map(|entries| entries.len())
                            .unwrap_or(0);
                        Ok(json!({
                            "catalog_version": catalog.get("catalog_version").cloned().unwrap_or(Value::Null),
                            "installed_version": version,
                            "template_count": count,
                            "source": "remote_version",
                            "verified": true,
                            "offline": false,
                            "refresh_metadata": meta,
                        }))
                    }
                    Err(err) => Err(err.context("install historical catalog version failed")),
                },
                Err(err) => Err(err.context("historical catalog version failed validation")),
            },
            Err(err) => Err(anyhow!("parse historical catalog version failed: {err}")),
        },
        Err(err) => Err(err.context("download historical catalog version failed")),
    };

    match result {
        Ok(data) => {
            crate::task_engine::backoff::clear_structural_all();
            crate::logging::log_event_global(
                "info",
                "catalog.version_installed",
                json!({
                    "version": version,
                    "catalog_version": data.get("catalog_version").cloned().unwrap_or(Value::Null),
                    "template_count": data.get("template_count").cloned().unwrap_or(Value::Null),
                }),
            );
            let payload = json!({ "success": true, "data": data });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "PROVIDER_CATALOG_VERSION_INSTALL_FAILED",
                &format!("{err:#}"),
            )
            .await
        }
    }
}

pub(super) async fn handle_install_from_catalog(
    socket: &mut TcpStream,
    _state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = serde_json::from_slice(body)
        .with_context(|| "invalid POST /api/components/local/install-from-catalog payload")?;
    let requested_id = req
        .get("entry_id")
        .or_else(|| req.get("catalog_id"))
        .or_else(|| req.get("template_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if requested_id.is_empty() {
        return write_error_response(socket, "MISSING_CATALOG_ID", "entry_id is required").await;
    }
    let (catalog, _) = match load_catalog_document() {
        Ok(value) => value,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "PROVIDER_CATALOG_INVALID",
                &err_public(&err),
            )
            .await;
        }
    };
    let entry = catalog_template_entries(&catalog)?
        .into_iter()
        .find(|entry| entry.entry_id == requested_id || entry.template_id == requested_id);
    let Some(entry) = entry else {
        return write_error_response(socket, "CATALOG_ENTRY_NOT_FOUND", "catalog entry not found")
            .await;
    };
    let rejected_urls: Vec<Value> = catalog_url_report(&entry.template)
        .into_iter()
        .filter(|item| item.get("allowed").and_then(Value::as_bool) == Some(false))
        .collect();
    if !rejected_urls.is_empty() {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "PROVIDER_URL_REJECTED",
            &serde_json::to_string(&rejected_urls)?,
        )
        .await;
    }

    let local_id = req
        .get("local_id")
        .or_else(|| req.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(&entry.template_id)
        .to_string();
    if local_id.len() > 160 || local_id.contains('/') || local_id.contains('\\') {
        return write_error_response(socket, "INVALID_ID", "local_id is invalid").await;
    }
    let mut doc = load_local_components_runtime_doc()?;
    if doc.components.contains_key(&local_id)
        && req.get("overwrite").and_then(Value::as_bool) != Some(true)
    {
        return write_conflict_response(
            socket,
            "DUPLICATE_ID",
            "local component already exists; set overwrite=true to replace it",
        )
        .await;
    }
    let template: ComponentTemplate = serde_json::from_value(entry.template.clone())?;
    let now = unix_ts().to_string();
    let name = req
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(&template.name)
        .to_string();
    let kind = template.kind.clone();
    let was_overwrite = doc.components.contains_key(&local_id);
    doc.components.insert(
        local_id.clone(),
        ComponentInstanceLocal {
            name,
            template_id: template.id.clone(),
            source_template_id: format!("catalog:{}", entry.entry_id),
            source_template_updated_at: None,
            source_template_api_version: Some(template.version.clone()),
            vendor_id: entry.vendor_id.clone(),
            vendor_name: req
                .get("vendor_name")
                .and_then(Value::as_str)
                .unwrap_or(&entry.vendor_id)
                .trim()
                .to_string(),
            kind: kind.clone(),
            remarks: req
                .get("remarks")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            // Catalogs never carry credentials.  Keep the new component
            // disabled until the user selects a local key/OAuth binding.
            enabled: false,
            created_at: if was_overwrite {
                doc.components
                    .get(&local_id)
                    .map(|component| component.created_at.clone())
                    .unwrap_or_else(|| now.clone())
            } else {
                now.clone()
            },
            updated_at: Some(now),
            component_overrides: None,
            versions: HashMap::new(),
            active_version: None,
            template_json: Some(entry.template.clone()),
        },
    );
    save_local_components_runtime_doc(&doc)?;
    crate::task_engine::backoff::clear_structural_all();
    crate::logging::log_event_global(
        "info",
        "catalog.installed_from_catalog",
        json!({ "entry_id": entry.entry_id, "template_id": entry.template_id, "local_id": local_id, "overwrite": was_overwrite }),
    );
    let payload = json!({
        "success": true,
        "data": {
            "id": local_id,
            "template_id": template.id,
            "catalog_entry_id": entry.entry_id,
            "vendor_id": entry.vendor_id,
            "kind": local_component_capability_id(&kind),
            "enabled": false,
            "overwrite": was_overwrite,
            "requires_local_credentials": template.auth.is_some(),
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

fn public_pack_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    normalized == "auth"
        || normalized == "authvalues"
        || normalized == "credentials"
        || normalized == "clientsecret"
        || normalized == "cachedtoken"
        || normalized == "refreshtoken"
        || normalized == "wpclienttoken"
        || normalized == "routesecret"
        || normalized == "pairingcode"
        || normalized == "password"
        || normalized == "username"
        || normalized == "user"
        || normalized == "login"
        || normalized == "authorization"
        || normalized == "authextraparameters"
        || normalized == "extraparameters"
        || normalized.contains("apikey")
        || normalized.contains("accesstoken")
        || normalized.contains("privatekey")
        // Defense-in-depth parity with the component-export filter: generic
        // secret-bearing field names (app_secret, api_secret, ...) must be
        // stripped too. Exemptions: boolean indicators (`route_secret_set`)
        // keep a `_set` suffix, and the pack's own `secrets` metadata section
        // carries only mode/required-id lists, never values.
        || (normalized.contains("secret")
            && !normalized.ends_with("set")
            && normalized != "secrets")
}

fn redact_public_value(value: &mut Value, path: &str, redacted_fields: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            let keys: Vec<String> = fields.keys().cloned().collect();
            for key in keys {
                let child_path = format!("{path}.{key}");
                if public_pack_sensitive_key(&key) {
                    fields.remove(&key);
                    redacted_fields.push(child_path);
                } else if let Some(child) = fields.get_mut(&key) {
                    redact_public_value(child, &child_path, redacted_fields);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                redact_public_value(child, &format!("{path}[{index}]"), redacted_fields);
            }
        }
        _ => {}
    }
}

fn workflow_snapshot(conn: &rusqlite::Connection) -> Value {
    let policy = crate::task_engine::workflow_policy::load_workflow_policy(conn, false);
    let dsl = crate::task_engine::workflow_dsl::load_workflow_dsl(conn);
    json!({ "policy": policy, "dsl": dsl })
}

async fn build_integration_pack(
    state: &Arc<Mutex<WebUiState>>,
    private: bool,
    pack_id: Option<&str>,
    name: Option<&str>,
) -> anyhow::Result<(Value, Vec<String>)> {
    let (domain_tokens, component_bindings, task_bindings, rule_bindings, device_id, db) = {
        let guard = state.lock().await;
        (
            guard.domain_token_bindings.clone(),
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
            guard.device_id.clone(),
            Arc::clone(&guard.db),
        )
    };
    let workflow = {
        let conn = db.lock().await;
        workflow_snapshot(&conn)
    };
    let local_components = load_local_components_runtime_doc()?;
    let vendor_keys = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
    let vendor_oauth = load_vendor_oauth(&vendor_oauth_path()).unwrap_or_default();
    let proxy_profiles = load_proxy_profiles(&proxy_profiles_path()).unwrap_or_default();
    let mut redacted_fields = Vec::new();

    let mut site_connections = Vec::new();
    for (api_base_url, binding) in &domain_tokens.domains {
        if private {
            site_connections.push(json!({
                "api_base_url": api_base_url,
                "wp_client_token": binding.wp_client_token,
                "route_secret": binding.route_secret,
                "device_id": device_id,
            }));
        } else {
            site_connections.push(json!({
                "api_base_url": api_base_url,
                "route_secret_set": !binding.route_secret.trim().is_empty(),
                "device_id": "",
                "requires_pairing": binding.wp_client_token.trim().is_empty(),
            }));
            if !binding.wp_client_token.trim().is_empty() {
                redacted_fields.push(format!(
                    "domain_token_bindings.domains.{api_base_url}.wp_client_token"
                ));
            }
            if !binding.route_secret.trim().is_empty() {
                redacted_fields.push(format!(
                    "domain_token_bindings.domains.{api_base_url}.route_secret"
                ));
            }
        }
    }

    let mut local_components_value = serde_json::to_value(&local_components)?;
    let mut component_bindings_value = serde_json::to_value(&component_bindings)?;
    let mut vendor_keys_value = serde_json::to_value(&vendor_keys)?;
    let mut vendor_oauth_value = serde_json::to_value(&vendor_oauth)?;
    let mut proxy_profiles_value = serde_json::to_value(&proxy_profiles)?;
    let mut domain_tokens_value = serde_json::to_value(&domain_tokens)?;
    if !private {
        redact_public_value(
            &mut local_components_value,
            "local_components",
            &mut redacted_fields,
        );
        redact_public_value(
            &mut component_bindings_value,
            "component_bindings",
            &mut redacted_fields,
        );
        redact_public_value(&mut vendor_keys_value, "vendor_keys", &mut redacted_fields);
        redact_public_value(
            &mut vendor_oauth_value,
            "vendor_oauth",
            &mut redacted_fields,
        );
        redact_public_value(
            &mut proxy_profiles_value,
            "proxy_profiles",
            &mut redacted_fields,
        );
        redact_public_value(
            &mut domain_tokens_value,
            "domain_token_bindings",
            &mut redacted_fields,
        );
    }

    let mut required_keys = Vec::new();
    let mut required_oauth = Vec::new();
    for (component_id, binding) in &component_bindings.components {
        for key_id in &binding.key_ids {
            let vendor_id = vendor_keys
                .keys
                .get(key_id)
                .map(|key| key.vendor_id.clone())
                .unwrap_or_default();
            required_keys.push(json!({
                "id": key_id,
                "vendor_id": vendor_id,
                "component_id": component_id,
                "configured": vendor_keys.keys.get(key_id).is_some_and(|key| !key.auth_values.is_empty()),
            }));
        }
        for oauth_id in &binding.oauth_ids {
            let vendor_id = vendor_oauth
                .configs
                .get(oauth_id)
                .map(|config| config.vendor_id.clone())
                .unwrap_or_default();
            required_oauth.push(json!({
                "id": oauth_id,
                "vendor_id": vendor_id,
                "component_id": component_id,
                "configured": vendor_oauth.configs.contains_key(oauth_id),
            }));
        }
    }

    let provider_templates = local_components
        .components
        .iter()
        .filter_map(|(id, component)| {
            component.template_json.as_ref().map(|template| {
                json!({
                    "component_id": id,
                    "template_id": component.template_id,
                    "vendor_id": component.vendor_id,
                    "kind": component.kind,
                    "template": template,
                })
            })
        })
        .collect::<Vec<_>>();
    let mut provider_templates_value = Value::Array(provider_templates);
    if !private {
        redact_public_value(
            &mut provider_templates_value,
            "provider_templates",
            &mut redacted_fields,
        );
    }

    let (rule_bindings_for_pack, pack_binding_warnings) = if private {
        (rule_bindings.clone(), Vec::new())
    } else {
        sanitize_rule_bindings_for_public_pack(&rule_bindings)
    };
    for warning in &pack_binding_warnings {
        redacted_fields.push(format!("rule_component_bindings.{warning}"));
    }

    let pack = json!({
        "schema": INTEGRATION_PACK_SCHEMA,
        "pack_id": pack_id.filter(|v| !v.trim().is_empty()).unwrap_or("local-integration-pack"),
        "name": name.filter(|v| !v.trim().is_empty()).unwrap_or("Local integration pack"),
        "created_at": unix_ts().to_string(),
        "redaction_policy": if private { "private_encrypted" } else { "public_default" },
        "redacted": !private,
        "site_connection": site_connections.first().cloned().unwrap_or(Value::Null),
        "site_connections": site_connections,
        "domain_token_bindings": domain_tokens_value,
        "provider_templates": provider_templates_value,
        "local_components": local_components_value,
        "component_bindings": component_bindings_value,
        "task_type_component_bindings": serde_json::to_value(&task_bindings)?,
        "rule_component_bindings": serde_json::to_value(&rule_bindings_for_pack)?,
        "binding_portability_warnings": pack_binding_warnings,
        "workflow": workflow,
        "vendor_keys": vendor_keys_value,
        "vendor_oauth": vendor_oauth_value,
        "proxy_profiles": proxy_profiles_value,
        "secrets": {
            "mode": if private { "encrypted" } else { "redacted" },
            "required_vendor_keys": required_keys,
            "required_oauth_configs": required_oauth,
        },
        "redacted_fields": redacted_fields,
    });
    let redacted_paths = pack
        .get("redacted_fields")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok((pack, redacted_paths))
}

fn pack_from_request(body: &[u8]) -> anyhow::Result<(Value, bool)> {
    let root: Value = serde_json::from_slice(body).context("invalid integration pack JSON")?;
    let root_passphrase = root
        .get("passphrase")
        .and_then(Value::as_str)
        .map(str::to_string);
    let candidate = root
        .get("integration_pack")
        .or_else(|| root.get("pack"))
        .or_else(|| root.get("data").and_then(|v| v.get("integration_pack")))
        .cloned()
        .unwrap_or(root);
    let passphrase = root_passphrase
        .as_deref()
        .or_else(|| candidate.get("passphrase").and_then(Value::as_str));
    let encrypted = candidate
        .get("encrypted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if encrypted {
        let payload = candidate
            .get("payload_base64")
            .or_else(|| candidate.get("encrypted_payload"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("encrypted integration pack payload is missing"))?;
        let bytes = BASE64_STANDARD
            .decode(payload)
            .context("decode encrypted integration pack failed")?;
        let plain = decrypt_private_backup(&bytes, passphrase)
            .context("decrypt integration pack failed")?;
        let pack: Value =
            serde_json::from_str(&plain).context("parse decrypted integration pack failed")?;
        return Ok((pack, true));
    }
    Ok((candidate, false))
}

fn validate_pack_schema(pack: &Value, encrypted: bool) -> anyhow::Result<bool> {
    if pack.get("schema").and_then(Value::as_str) != Some(INTEGRATION_PACK_SCHEMA) {
        anyhow::bail!("unsupported integration pack schema");
    }
    let private = pack
        .get("redaction_policy")
        .and_then(Value::as_str)
        .is_some_and(|v| v == "private" || v == "private_encrypted")
        || pack.get("redacted").and_then(Value::as_bool) == Some(false);
    if private && !encrypted {
        anyhow::bail!("private integration pack must be encrypted");
    }
    if !private && contains_public_pack_secret(pack) {
        anyhow::bail!("public integration pack contains a secret field");
    }
    Ok(!private)
}

fn contains_public_pack_secret(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, child)| {
            if public_pack_sensitive_key(key) {
                // `auth` is deliberately not used by public exports, but a
                // provider template's auth schema contains no secret values.
                // Treat an auth_values/client_secret/token field as secret;
                // normal template request placeholders are safe.
                return key != "auth";
            }
            contains_public_pack_secret(child)
        }),
        Value::Array(items) => items.iter().any(contains_public_pack_secret),
        _ => false,
    }
}

fn parse_doc<T>(pack: &Value, key: &str) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(value) = pack.get(key) else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(value.clone()).with_context(
        || format!("invalid integration pack field: {key}"),
    )?))
}

fn validate_local_component_template(
    id: &str,
    component: &ComponentInstanceLocal,
) -> anyhow::Result<()> {
    if let Some(template) = component.template_json.as_ref() {
        serde_json::from_value::<ComponentTemplate>(template.clone())
            .with_context(|| format!("invalid template_json for local component '{id}'"))?;
    }
    Ok(())
}

fn parse_local_components(pack: &Value) -> anyhow::Result<Vec<(String, ComponentInstanceLocal)>> {
    let Some(raw_value) = pack.get("local_components") else {
        return Ok(Vec::new());
    };
    // Native exports use ComponentsLocalDoc (`{version, components}`), while
    // hand-authored packs may use a direct id→component map or an array.
    let value = raw_value
        .get("components")
        .filter(|components| components.is_object())
        .unwrap_or(raw_value);
    let mut output = Vec::new();
    match value {
        Value::Object(items) => {
            for (id, item) in items {
                let component: ComponentInstanceLocal = serde_json::from_value(item.clone())
                    .with_context(|| format!("invalid local component '{id}'"))?;
                validate_local_component_template(id, &component)?;
                output.push((id.clone(), component));
            }
        }
        Value::Array(items) => {
            for item in items {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("component_id").and_then(Value::as_str))
                    .ok_or_else(|| anyhow!("local component array item requires id"))?;
                let component_value = item
                    .get("component")
                    .cloned()
                    .unwrap_or_else(|| item.clone());
                let component: ComponentInstanceLocal = serde_json::from_value(component_value)
                    .with_context(|| format!("invalid local component '{id}'"))?;
                validate_local_component_template(id, &component)?;
                output.push((id.to_string(), component));
            }
        }
        _ => anyhow::bail!("local_components must be an object or array"),
    }
    Ok(output)
}

fn component_ids_after_import(
    existing: &ComponentsLocalDoc,
    incoming: &[(String, ComponentInstanceLocal)],
) -> HashSet<String> {
    let mut ids = existing.components.keys().cloned().collect::<HashSet<_>>();
    ids.extend(incoming.iter().map(|(id, _)| id.clone()));
    ids
}

fn binding_conflicts<T>(
    existing: &HashMap<String, T>,
    incoming: &HashMap<String, T>,
) -> Vec<String> {
    incoming
        .keys()
        .filter(|key| existing.contains_key(*key))
        .cloned()
        .collect()
}

async fn integration_preview(
    state: &Arc<Mutex<WebUiState>>,
    pack: &Value,
    encrypted: bool,
) -> anyhow::Result<Value> {
    let redacted = validate_pack_schema(pack, encrypted)?;
    let incoming_components = parse_local_components(pack)?;
    let existing_components = load_local_components_runtime_doc()?;
    let component_ids = component_ids_after_import(&existing_components, &incoming_components);
    let new_components = incoming_components
        .iter()
        .filter(|(id, _)| !existing_components.components.contains_key(id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let overwritten_components = incoming_components
        .iter()
        .filter(|(id, _)| existing_components.components.contains_key(id))
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();

    let mut provider_urls = Vec::new();
    for (id, component) in &incoming_components {
        if let Some(template) = &component.template_json {
            for item in catalog_url_report(template) {
                let mut item = item;
                if let Some(object) = item.as_object_mut() {
                    object.insert("component_id".to_string(), Value::String(id.clone()));
                }
                provider_urls.push(item);
            }
        }
    }
    let blocked_provider_urls = provider_urls
        .iter()
        .filter(|item| item.get("allowed").and_then(Value::as_bool) == Some(false))
        .count();

    let current = {
        let guard = state.lock().await;
        (
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    let incoming_component_bindings: Option<ComponentBindingsDoc> =
        parse_doc(pack, "component_bindings")?;
    let incoming_task_bindings: Option<TaskTypeComponentBindingsDoc> =
        parse_doc(pack, "task_type_component_bindings")?;
    let incoming_rule_bindings: Option<RuleComponentBindingsDoc> =
        parse_doc(pack, "rule_component_bindings")?;

    let cross_site_numeric_keys = incoming_rule_bindings
        .as_ref()
        .map(|doc| {
            let mut keys = numeric_scope_keys(&doc.relation_bindings)
                .into_iter()
                .map(|k| format!("relation:{k}"))
                .collect::<Vec<_>>();
            keys.extend(
                numeric_scope_keys(&doc.rule_bindings)
                    .into_iter()
                    .map(|k| format!("rule:{k}")),
            );
            keys
        })
        .unwrap_or_default();
    let cross_site_numeric_risk = incoming_rule_bindings
        .as_ref()
        .map(pack_has_cross_site_numeric_risk)
        .unwrap_or(false);

    let component_binding_conflicts = incoming_component_bindings
        .as_ref()
        .map(|doc| binding_conflicts(&current.0.components, &doc.components))
        .unwrap_or_default();
    let task_binding_conflicts = incoming_task_bindings
        .as_ref()
        .map(|doc| {
            let mut conflicts = binding_conflicts(&current.1.task_types, &doc.task_types);
            conflicts.extend(
                binding_conflicts(
                    &current.1.business_line_task_types,
                    &doc.business_line_task_types,
                )
                .into_iter()
                .map(|key| format!("business_line:{key}")),
            );
            conflicts
        })
        .unwrap_or_default();
    let rule_binding_conflicts = incoming_rule_bindings
        .as_ref()
        .map(|doc| {
            let mut conflicts = binding_conflicts(&current.2.global_defaults, &doc.global_defaults);
            conflicts.extend(
                binding_conflicts(&current.2.relation_bindings, &doc.relation_bindings)
                    .into_iter()
                    .map(|key| format!("relation:{key}")),
            );
            conflicts.extend(
                binding_conflicts(&current.2.plugin_bindings, &doc.plugin_bindings)
                    .into_iter()
                    .map(|key| format!("plugin:{key}")),
            );
            conflicts.extend(
                binding_conflicts(&current.2.rule_bindings, &doc.rule_bindings)
                    .into_iter()
                    .map(|key| format!("rule:{key}")),
            );
            conflicts
        })
        .unwrap_or_default();

    let mut missing_component_refs = Vec::new();
    if let Some(doc) = &incoming_component_bindings {
        for id in doc.components.keys() {
            if !component_ids.contains(id) {
                missing_component_refs.push(format!("component_bindings:{id}"));
            }
        }
    }
    if let Some(doc) = &incoming_task_bindings {
        for entry in doc
            .task_types
            .values()
            .chain(doc.business_line_task_types.values())
        {
            if !component_ids.contains(&entry.component_id) {
                missing_component_refs.push(format!("task_type:{}", entry.component_id));
            }
        }
    }
    if let Some(doc) = &incoming_rule_bindings {
        for component_id in doc
            .global_defaults
            .values()
            .chain(doc.relation_bindings.values().flat_map(HashMap::values))
            .chain(doc.plugin_bindings.values().flat_map(HashMap::values))
            .chain(doc.rule_bindings.values().flat_map(HashMap::values))
        {
            if !component_ids.contains(component_id) {
                missing_component_refs.push(format!("rule_binding:{component_id}"));
            }
        }
    }

    let incoming_domains: Option<DomainTokenBindingsDoc> =
        parse_doc(pack, "domain_token_bindings")?;
    let site_count = pack
        .get("site_connections")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_else(|| usize::from(pack.get("site_connection").is_some_and(|v| !v.is_null())));
    let domains_with_credentials = incoming_domains
        .as_ref()
        .map(|doc| {
            doc.domains
                .values()
                .filter(|entry| !entry.wp_client_token.trim().is_empty())
                .count()
        })
        .unwrap_or(0);

    let workflow_valid = pack
        .get("workflow")
        .map(|workflow| {
            let policy_ok = workflow
                .get("policy")
                .and_then(|value| serde_json::to_string(value).ok())
                .and_then(|raw| {
                    crate::task_engine::workflow_policy::WorkflowPolicy::parse_json(&raw)
                })
                .is_some();
            let dsl_ok = workflow
                .get("dsl")
                .and_then(|value| serde_json::to_string(value).ok())
                .and_then(|raw| crate::task_engine::workflow_dsl::WorkflowDsl::parse_json(&raw))
                .is_some();
            policy_ok && dsl_ok
        })
        .unwrap_or(true);

    Ok(json!({
        "schema": INTEGRATION_PACK_SCHEMA,
        "redacted": redacted,
        "encrypted": encrypted,
        "new_components": new_components,
        "overwrite_components": overwritten_components,
        "component_binding_conflicts": component_binding_conflicts,
        "task_binding_conflicts": task_binding_conflicts,
        "rule_binding_conflicts": rule_binding_conflicts,
        "missing_component_refs": missing_component_refs,
        "provider_urls": provider_urls,
        "blocked_provider_url_count": blocked_provider_urls,
        "site_connections": {
            "count": site_count,
            "ready_to_import": domains_with_credentials,
            "requires_pairing": site_count.saturating_sub(domains_with_credentials),
            "existing_domains": current.3.domains.len(),
        },
        "workflow_valid": workflow_valid,
        "cross_site_numeric_risk": cross_site_numeric_risk,
        "cross_site_numeric_keys": cross_site_numeric_keys,
        "safe_to_import": blocked_provider_urls == 0
            && missing_component_refs.is_empty()
            && workflow_valid
            && !cross_site_numeric_risk,
    }))
}

pub(super) async fn handle_integration_pack_export(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(body).context("invalid integration pack export request")?
    };
    let mode = req
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("public")
        .trim()
        .to_ascii_lowercase();
    let private = mode == "private" || mode == "private_encrypted";
    if private {
        if req.get("confirm").and_then(Value::as_bool) != Some(true) {
            return write_error_response(
                socket,
                "PRIVATE_BACKUP_CONFIRMATION_REQUIRED",
                "private backup requires confirm=true",
            )
            .await;
        }
        if let Err(err) = validate_backup_passphrase(req.get("passphrase").and_then(Value::as_str))
        {
            return write_error_response(
                socket,
                "PRIVATE_BACKUP_PASSPHRASE_REQUIRED",
                &err_public(&err),
            )
            .await;
        }
    } else if mode != "public" {
        return write_error_response(
            socket,
            "INVALID_EXPORT_MODE",
            "mode must be public or private",
        )
        .await;
    }
    let (pack, redacted_fields) = build_integration_pack(
        state,
        private,
        req.get("pack_id").and_then(Value::as_str),
        req.get("name").and_then(Value::as_str),
    )
    .await?;
    if private {
        let passphrase = validate_backup_passphrase(req.get("passphrase").and_then(Value::as_str))?;
        let encoded = serde_json::to_vec(&pack)?;
        let encrypted = encrypt_private_backup(&encoded, passphrase)?;
        crate::logging::log_event_global(
            "info",
            "catalog.pack_exported",
            json!({ "export_mode": "private_encrypted" }),
        );
        let payload = json!({
            "success": true,
            "data": {
                "schema": PRIVATE_BACKUP_SCHEMA,
                "pack_schema": INTEGRATION_PACK_SCHEMA,
                "export_mode": "private_encrypted",
                "encrypted": true,
                "portable": true,
                "algorithm": "AES-256-GCM",
                "kdf": "HKDF-SHA256",
                "payload_base64": BASE64_STANDARD.encode(encrypted),
            }
        });
        return write_http_response(
            socket,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&payload)?,
        )
        .await;
    }
    crate::logging::log_event_global(
        "info",
        "catalog.pack_exported",
        json!({ "export_mode": "public", "redacted_fields": redacted_fields.len() }),
    );
    let payload = json!({
        "success": true,
        "data": {
            "export_mode": "public",
            "encrypted": false,
            "pack": pack,
            "redacted_fields": redacted_fields,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}

pub(super) async fn handle_integration_pack_preview(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let (pack, encrypted) = match pack_from_request(body) {
        Ok(value) => value,
        Err(err) => {
            return write_error_response(socket, "INVALID_INTEGRATION_PACK", &err_public(&err))
                .await
        }
    };
    match integration_preview(state, &pack, encrypted).await {
        Ok(preview) => {
            let payload = json!({ "success": true, "data": preview });
            write_http_response(
                socket,
                "200 OK",
                "application/json",
                &serde_json::to_vec(&payload)?,
            )
            .await
        }
        Err(err) => {
            write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "INTEGRATION_PACK_PREVIEW_FAILED",
                &err_public(&err),
            )
            .await
        }
    }
}

fn merge_vendor_key(existing: Option<&VendorKey>, mut incoming: VendorKey) -> VendorKey {
    if let Some(existing) = existing {
        if incoming.auth_values.is_empty() {
            incoming.auth_values = existing.auth_values.clone();
        }
    }
    incoming
}

fn merge_oauth_config(existing: Option<&OAuthConfig>, mut incoming: OAuthConfig) -> OAuthConfig {
    if let Some(existing) = existing {
        if incoming.client_secret.is_empty() {
            incoming.client_secret = existing.client_secret.clone();
        }
        if incoming.cached_token.is_none() {
            incoming.cached_token = existing.cached_token.clone();
        }
        if incoming.refresh_token.is_none() {
            incoming.refresh_token = existing.refresh_token.clone();
        }
        if incoming.cached_token_expires_at == 0 {
            incoming.cached_token_expires_at = existing.cached_token_expires_at;
        }
    }
    incoming
}

fn merge_proxy_profile(
    existing: Option<&ProxyProfile>,
    mut incoming: ProxyProfile,
) -> ProxyProfile {
    if let Some(existing) = existing {
        if incoming.username.is_empty() {
            incoming.username = existing.username.clone();
        }
        if incoming.password.is_empty() {
            incoming.password = existing.password.clone();
        }
    }
    incoming
}

fn merge_component_binding(
    existing: Option<&ComponentBindingEntry>,
    mut incoming: ComponentBindingEntry,
) -> ComponentBindingEntry {
    if let Some(existing) = existing {
        if incoming.auth.is_empty() {
            incoming.auth = existing.auth.clone();
        }
    }
    incoming
}

pub(super) async fn handle_integration_pack_import(
    socket: &mut TcpStream,
    state: &Arc<Mutex<WebUiState>>,
    body: &[u8],
) -> anyhow::Result<()> {
    let req: Value = if body.is_empty() {
        json!({})
    } else {
        // Garbage JSON bodies get the same graceful INVALID_INTEGRATION_PACK
        // error as garbage pack content (instead of escaping as a raw
        // handler error) — the import API always answers a defined envelope.
        match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(err) => {
                return write_error_response(
                    socket,
                    "INVALID_INTEGRATION_PACK",
                    &format!("invalid integration pack import request: {err:#}"),
                )
                .await
            }
        }
    };
    let overwrite = req
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pack_input = if req.get("integration_pack").is_some()
        || req.get("pack").is_some()
        || req.get("encrypted").is_some()
    {
        serde_json::to_vec(&req)?
    } else {
        body.to_vec()
    };
    let (pack, encrypted) = match pack_from_request(&pack_input) {
        Ok(value) => value,
        Err(err) => {
            return write_error_response(socket, "INVALID_INTEGRATION_PACK", &err_public(&err))
                .await
        }
    };
    let preview = match integration_preview(state, &pack, encrypted).await {
        Ok(preview) => preview,
        Err(err) => {
            return write_error_response_with_status(
                socket,
                "422 Unprocessable Entity",
                "INTEGRATION_PACK_IMPORT_REJECTED",
                &err_public(&err),
            )
            .await;
        }
    };
    if preview
        .get("blocked_provider_url_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
        || preview
            .get("missing_component_refs")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
        || preview.get("workflow_valid").and_then(Value::as_bool) == Some(false)
        || preview
            .get("cross_site_numeric_risk")
            .and_then(Value::as_bool)
            == Some(true)
        || preview.get("safe_to_import").and_then(Value::as_bool) == Some(false)
    {
        return write_error_response_with_status(
            socket,
            "422 Unprocessable Entity",
            "INTEGRATION_PACK_IMPORT_REJECTED",
            "pack contains blocked provider URLs, missing component references, invalid workflow, or cross-site numeric binding ids",
        )
        .await;
    }

    let incoming_components = parse_local_components(&pack)?;
    let mut components_doc = load_local_components_runtime_doc()?;
    let mut imported_components = Vec::new();
    let mut skipped_components = Vec::new();
    for (id, mut component) in incoming_components {
        if components_doc.components.contains_key(&id) && !overwrite {
            skipped_components.push(id);
            continue;
        }
        if pack.get("redacted").and_then(Value::as_bool) == Some(true) {
            component.enabled = false;
        }
        imported_components.push(id.clone());
        components_doc.components.insert(id, component);
    }
    save_local_components_runtime_doc(&components_doc)?;

    let (
        component_bindings_path,
        task_bindings_path,
        rule_bindings_path,
        db,
        mut current_component_bindings,
        mut current_task_bindings,
        mut current_rule_bindings,
        mut current_domain_tokens,
    ) = {
        let guard = state.lock().await;
        (
            guard.component_bindings_path.clone(),
            guard.task_type_component_bindings_path.clone(),
            guard.rule_component_bindings_path.clone(),
            Arc::clone(&guard.db),
            guard.component_bindings.clone(),
            guard.task_type_component_bindings.clone(),
            guard.rule_component_bindings.clone(),
            guard.domain_token_bindings.clone(),
        )
    };
    let component_ids = components_doc
        .components
        .keys()
        .cloned()
        .collect::<HashSet<_>>();

    let mut imported_component_bindings = Vec::new();
    if let Some(incoming) = parse_doc::<ComponentBindingsDoc>(&pack, "component_bindings")? {
        for (id, entry) in incoming.components {
            if !component_ids.contains(&id) {
                continue;
            }
            if current_component_bindings.components.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = current_component_bindings.components.get(&id);
            current_component_bindings
                .components
                .insert(id.clone(), merge_component_binding(existing, entry));
            imported_component_bindings.push(id);
        }
    }

    let mut imported_task_bindings = Vec::new();
    if let Some(incoming) =
        parse_doc::<TaskTypeComponentBindingsDoc>(&pack, "task_type_component_bindings")?
    {
        for (key, entry) in incoming.task_types {
            if !component_ids.contains(&entry.component_id) {
                continue;
            }
            if current_task_bindings.task_types.contains_key(&key) && !overwrite {
                continue;
            }
            current_task_bindings.task_types.insert(key.clone(), entry);
            imported_task_bindings.push(key);
        }
        for (key, entry) in incoming.business_line_task_types {
            if !component_ids.contains(&entry.component_id) {
                continue;
            }
            if current_task_bindings
                .business_line_task_types
                .contains_key(&key)
                && !overwrite
            {
                continue;
            }
            current_task_bindings
                .business_line_task_types
                .insert(key.clone(), entry);
            imported_task_bindings.push(format!("business_line:{key}"));
        }
    }

    let mut imported_rule_bindings = Vec::new();
    let mut skipped_numeric_rule_bindings = Vec::new();
    if let Some(mut incoming) =
        parse_doc::<RuleComponentBindingsDoc>(&pack, "rule_component_bindings")?
    {
        skipped_numeric_rule_bindings = strip_numeric_maps_on_import(&mut incoming);
        let insert_rule_map = |target: &mut HashMap<String, HashMap<String, String>>,
                               source: HashMap<String, HashMap<String, String>>,
                               prefix: &str,
                               imported: &mut Vec<String>| {
            for (scope_key, slots) in source {
                let filtered = slots
                    .into_iter()
                    .filter(|(_, component_id)| component_ids.contains(component_id))
                    .collect::<HashMap<_, _>>();
                if filtered.is_empty() {
                    continue;
                }
                if target.contains_key(&scope_key) && !overwrite {
                    continue;
                }
                target.insert(scope_key.clone(), filtered);
                imported.push(format!("{prefix}:{scope_key}"));
            }
        };
        let global = incoming
            .global_defaults
            .into_iter()
            .filter(|(_, component_id)| component_ids.contains(component_id))
            .collect::<HashMap<_, _>>();
        if overwrite || current_rule_bindings.global_defaults.is_empty() {
            for (slot, component_id) in global {
                if overwrite || !current_rule_bindings.global_defaults.contains_key(&slot) {
                    current_rule_bindings
                        .global_defaults
                        .insert(slot.clone(), component_id);
                    imported_rule_bindings.push(format!("global:{slot}"));
                }
            }
        }
        insert_rule_map(
            &mut current_rule_bindings.relation_bindings,
            incoming.relation_bindings,
            "relation",
            &mut imported_rule_bindings,
        );
        insert_rule_map(
            &mut current_rule_bindings.plugin_bindings,
            incoming.plugin_bindings,
            "plugin",
            &mut imported_rule_bindings,
        );
        insert_rule_map(
            &mut current_rule_bindings.rule_bindings,
            incoming.rule_bindings,
            "rule",
            &mut imported_rule_bindings,
        );
        // Merge portable v2 site_bindings (semantic only).
        for (site_key, site_entry) in incoming.site_bindings {
            let target = current_rule_bindings
                .site_bindings
                .entry(site_key.clone())
                .or_default();
            if target.site_ref.site_origin.is_empty() {
                target.site_ref = site_entry.site_ref;
            }
            for (rel_key, rel) in site_entry.relations {
                let slots: HashMap<_, _> = rel
                    .slots
                    .into_iter()
                    .filter(|(_, id)| component_ids.contains(id))
                    .collect();
                if slots.is_empty() {
                    continue;
                }
                if target.relations.contains_key(&rel_key) && !overwrite {
                    continue;
                }
                target.relations.insert(
                    rel_key.clone(),
                    RelationBindingEntry {
                        semantic_key: rel.semantic_key,
                        relation_ref: rel.relation_ref,
                        legacy_id_hint: None, // never import foreign numeric hints
                        slots,
                    },
                );
                imported_rule_bindings.push(format!("site_relation:{site_key}:{rel_key}"));
            }
            for (rule_key, rule) in site_entry.rules {
                let slots: HashMap<_, _> = rule
                    .slots
                    .into_iter()
                    .filter(|(_, id)| component_ids.contains(id))
                    .collect();
                if slots.is_empty() {
                    continue;
                }
                if target.rules.contains_key(&rule_key) && !overwrite {
                    continue;
                }
                target.rules.insert(
                    rule_key.clone(),
                    RuleBindingEntry {
                        semantic_key: rule.semantic_key,
                        relation_key: rule.relation_key,
                        rule_ref: rule.rule_ref,
                        legacy_id_hint: None,
                        slots,
                    },
                );
                imported_rule_bindings.push(format!("site_rule:{site_key}:{rule_key}"));
            }
        }
        if current_rule_bindings.version < 2 && !current_rule_bindings.site_bindings.is_empty() {
            current_rule_bindings.version = 2;
        }
    }

    save_component_bindings_runtime_doc(&component_bindings_path, &current_component_bindings)?;
    save_task_type_component_bindings_runtime_doc(&task_bindings_path, &current_task_bindings)?;
    save_rule_component_bindings_runtime_doc(&rule_bindings_path, &current_rule_bindings)?;

    let mut imported_domains = Vec::new();
    if encrypted {
        if let Some(incoming) = parse_doc::<DomainTokenBindingsDoc>(&pack, "domain_token_bindings")?
        {
            for (domain, entry) in incoming.domains {
                if entry.wp_client_token.trim().is_empty() {
                    continue;
                }
                if current_domain_tokens.domains.contains_key(&domain) && !overwrite {
                    continue;
                }
                current_domain_tokens.domains.insert(domain.clone(), entry);
                imported_domains.push(domain);
            }
        }
    }

    let mut vendor_keys = load_vendor_keys(&vendor_keys_path()).unwrap_or_default();
    if let Some(incoming) = parse_doc::<VendorKeysDoc>(&pack, "vendor_keys")? {
        for (id, key) in incoming.keys {
            if vendor_keys.keys.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = vendor_keys.keys.get(&id);
            vendor_keys.keys.insert(id, merge_vendor_key(existing, key));
        }
    }
    let mut vendor_oauth = load_vendor_oauth(&vendor_oauth_path()).unwrap_or_default();
    if let Some(incoming) = parse_doc::<VendorOAuthDoc>(&pack, "vendor_oauth")? {
        for (id, config) in incoming.configs {
            if vendor_oauth.configs.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = vendor_oauth.configs.get(&id);
            vendor_oauth
                .configs
                .insert(id, merge_oauth_config(existing, config));
        }
    }
    let mut proxy_profiles = load_proxy_profiles(&proxy_profiles_path()).unwrap_or_default();
    if let Some(incoming) = parse_doc::<ProxyProfilesDoc>(&pack, "proxy_profiles")? {
        for (id, profile) in incoming.profiles {
            if proxy_profiles.profiles.contains_key(&id) && !overwrite {
                continue;
            }
            let existing = proxy_profiles.profiles.get(&id);
            proxy_profiles
                .profiles
                .insert(id, merge_proxy_profile(existing, profile));
        }
    }
    save_vendor_keys(&vendor_keys_path(), &vendor_keys)?;
    save_vendor_oauth(&vendor_oauth_path(), &vendor_oauth)?;
    save_proxy_profiles(&proxy_profiles_path(), &proxy_profiles)?;

    if let Some(workflow) = pack.get("workflow") {
        let db_guard = db.lock().await;
        if let Some(policy) = workflow.get("policy") {
            crate::db::system::set_system_config(
                &db_guard,
                "workflow_policy",
                &serde_json::to_string(policy)?,
            )?;
        }
        if let Some(dsl) = workflow.get("dsl") {
            crate::db::system::set_system_config(
                &db_guard,
                "workflow_dsl",
                &serde_json::to_string(dsl)?,
            )?;
        }
    }
    {
        let db_guard = db.lock().await;
        crate::db::bindings::save_component_bindings_doc(&db_guard, &current_component_bindings)?;
        crate::db::bindings::save_task_type_component_bindings_doc(
            &db_guard,
            &current_task_bindings,
        )?;
        crate::db::bindings::save_rule_component_bindings_doc(&db_guard, &current_rule_bindings)?;
        crate::db::bindings::save_domain_token_bindings_doc(&db_guard, &current_domain_tokens)?;
        crate::db::components::save_local_components_doc(&db_guard, &components_doc)?;
        crate::db::vendor::save_vendor_keys_doc(&db_guard, &vendor_keys)?;
        crate::db::vendor::save_vendor_oauth_doc(&db_guard, &vendor_oauth)?;
        crate::db::proxy::save_proxy_profiles_doc(&db_guard, &proxy_profiles)?;
    }
    {
        let mut guard = state.lock().await;
        guard.component_bindings = current_component_bindings;
        guard.task_type_component_bindings = current_task_bindings;
        guard.rule_component_bindings = current_rule_bindings;
        guard.domain_token_bindings = current_domain_tokens.clone();
        guard.domains =
            crate::web_ui::local_sites_from_domain_token_bindings(&current_domain_tokens);
        guard.last_error.clear();
        guard.last_event = "integration_pack.imported".to_string();
        guard.updated_at = unix_ts();
    }

    crate::logging::log_event_global(
        "info",
        "catalog.pack_imported",
        json!({
            "imported_components": imported_components.len(),
            "imported_bindings": imported_component_bindings.len() + imported_task_bindings.len() + imported_rule_bindings.len(),
            "imported_domains": imported_domains.len(),
            "encrypted": encrypted,
            "overwrite": overwrite,
        }),
    );
    let payload = json!({
        "success": true,
        "data": {
            "imported_components": imported_components,
            "skipped_components": skipped_components,
            "imported_component_bindings": imported_component_bindings,
            "imported_task_bindings": imported_task_bindings,
            "imported_rule_bindings": imported_rule_bindings,
            "skipped_numeric_rule_bindings": skipped_numeric_rule_bindings,
            "imported_domains": imported_domains,
            "requires_pairing": if encrypted { 0 } else { preview.get("site_connections").and_then(|v| v.get("requires_pairing")).and_then(Value::as_u64).unwrap_or(0) },
            "overwrite": overwrite,
        }
    });
    write_http_response(
        socket,
        "200 OK",
        "application/json",
        &serde_json::to_vec(&payload)?,
    )
    .await
}
