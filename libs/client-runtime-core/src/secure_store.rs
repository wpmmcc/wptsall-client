use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt};
use uuid::Uuid;

const SECURE_STORE_VERSION: &str = "enc-v1";
const SECURE_STORE_VERSION_V2: &str = "enc-v2";
const HKDF_INFO: &[u8] = b"client-runtime-core-secure-store-v1";
const LEGACY_KEY_ID: &str = "legacy-v1";

fn sanitize_key_segment(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn key_file_path(data_dir: &str, app_id: &str) -> PathBuf {
    let app = sanitize_key_segment(app_id);
    Path::new(data_dir).join(format!(".{}-master.key", app))
}

fn key_ring_file_path(data_dir: &str, app_id: &str) -> PathBuf {
    let app = sanitize_key_segment(app_id);
    Path::new(data_dir).join(format!(".{}-master.keys", app))
}

#[cfg(unix)]
fn enforce_unix_key_permissions(key_path: &Path, just_created: bool) -> anyhow::Result<()> {
    if just_created {
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(key_path, perms).with_context(|| {
            format!(
                "secure_store.key_permission_set_failed: {}",
                key_path.display()
            )
        })?;
    }

    let mode = fs::metadata(key_path)
        .with_context(|| format!("secure_store.key_metadata_failed: {}", key_path.display()))?
        .permissions()
        .mode()
        & 0o777;

    if (mode & 0o077) != 0 {
        return Err(anyhow!(
            "secure_store.key_permission_violation: {} mode={:o}, expected owner-only access",
            key_path.display(),
            mode
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_unix_key_permissions(_key_path: &Path, _just_created: bool) -> anyhow::Result<()> {
    Ok(())
}

#[derive(Debug, Clone)]
struct KeyRecord {
    id: String,
    key: [u8; 32],
}

#[derive(Debug, Clone)]
struct KeyRing {
    active_key_id: String,
    keys: Vec<KeyRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRotationStatus {
    Passed,
    FailedRolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRotationEntry {
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRotationReport {
    pub status: KeyRotationStatus,
    pub old_active_key_id: String,
    pub new_active_key_id: Option<String>,
    pub keys_before: usize,
    pub keys_after: usize,
    pub entries: Vec<KeyRotationEntry>,
}

fn new_master_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn new_key_id() -> String {
    format!("key-{}", Uuid::new_v4().simple())
}

fn find_key<'a>(ring: &'a KeyRing, key_id: &str) -> Option<&'a KeyRecord> {
    ring.keys.iter().find(|record| record.id == key_id)
}

fn active_key(ring: &KeyRing) -> anyhow::Result<&KeyRecord> {
    find_key(ring, &ring.active_key_id).ok_or_else(|| {
        anyhow!(
            "secure_store.key_ring_active_missing: {}",
            ring.active_key_id
        )
    })
}

fn serialize_key_ring(ring: &KeyRing) -> String {
    let mut lines = vec![format!("active:{}", ring.active_key_id)];
    for record in &ring.keys {
        lines.push(format!(
            "key:{}:{}",
            record.id,
            URL_SAFE_NO_PAD.encode(record.key)
        ));
    }
    lines.join("\n") + "\n"
}

fn parse_key_ring(raw: &str) -> anyhow::Result<KeyRing> {
    let mut active_key_id = String::new();
    let mut keys = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("active:") {
            active_key_id = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("key:") {
            let mut parts = rest.splitn(2, ':');
            let id = parts.next().unwrap_or_default().trim().to_string();
            let key_b64 = parts.next().unwrap_or_default().trim();
            let key_bytes = URL_SAFE_NO_PAD
                .decode(key_b64)
                .context("secure_store.key_ring_key_decode_failed")?;
            if id.is_empty() {
                return Err(anyhow!("secure_store.key_ring_key_id_missing"));
            }
            if key_bytes.len() != 32 {
                return Err(anyhow!(
                    "secure_store.key_ring_key_length_invalid: {}",
                    key_bytes.len()
                ));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&key_bytes);
            keys.push(KeyRecord { id, key });
            continue;
        }
        return Err(anyhow!("secure_store.key_ring_line_invalid"));
    }

    if active_key_id.is_empty() {
        return Err(anyhow!("secure_store.key_ring_active_missing"));
    }
    let ring = KeyRing {
        active_key_id,
        keys,
    };
    active_key(&ring)?;
    Ok(ring)
}

fn write_key_ring(data_dir: &str, app_id: &str, ring: &KeyRing) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir).context("create secure-store data dir failed")?;
    let key_path = key_ring_file_path(data_dir, app_id);
    let temp_path = key_path.with_extension("keys.tmp");
    std::fs::write(&temp_path, serialize_key_ring(ring)).with_context(|| {
        format!(
            "secure_store.key_ring_write_failed: {}",
            temp_path.display()
        )
    })?;
    enforce_unix_key_permissions(&temp_path, true)?;
    std::fs::rename(&temp_path, &key_path).with_context(|| {
        format!(
            "secure_store.key_ring_replace_failed: {}",
            key_path.display()
        )
    })?;
    enforce_unix_key_permissions(&key_path, false)?;
    Ok(())
}

fn load_or_create_master_key(data_dir: &str, app_id: &str) -> anyhow::Result<[u8; 32]> {
    std::fs::create_dir_all(data_dir).context("create secure-store data dir failed")?;
    let key_path = key_file_path(data_dir, app_id);

    if key_path.exists() {
        enforce_unix_key_permissions(&key_path, false)?;
        let bytes = std::fs::read(&key_path)
            .with_context(|| format!("read secure-store key failed: {}", key_path.display()))?;
        if bytes.len() != 32 {
            return Err(anyhow!(
                "invalid secure-store key length {}, expect 32 bytes",
                bytes.len()
            ));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        return Ok(key);
    }

    let key = new_master_key();
    std::fs::write(&key_path, key)
        .with_context(|| format!("write secure-store key failed: {}", key_path.display()))?;
    enforce_unix_key_permissions(&key_path, true)?;
    Ok(key)
}

fn load_or_create_key_ring(data_dir: &str, app_id: &str) -> anyhow::Result<KeyRing> {
    std::fs::create_dir_all(data_dir).context("create secure-store data dir failed")?;
    let ring_path = key_ring_file_path(data_dir, app_id);

    if ring_path.exists() {
        enforce_unix_key_permissions(&ring_path, false)?;
        let raw = std::fs::read_to_string(&ring_path).with_context(|| {
            format!("secure_store.key_ring_read_failed: {}", ring_path.display())
        })?;
        return parse_key_ring(&raw);
    }

    let legacy_path = key_file_path(data_dir, app_id);
    let ring = if legacy_path.exists() {
        let legacy_key = load_or_create_master_key(data_dir, app_id)?;
        KeyRing {
            active_key_id: LEGACY_KEY_ID.to_string(),
            keys: vec![KeyRecord {
                id: LEGACY_KEY_ID.to_string(),
                key: legacy_key,
            }],
        }
    } else {
        let id = new_key_id();
        KeyRing {
            active_key_id: id.clone(),
            keys: vec![KeyRecord {
                id,
                key: new_master_key(),
            }],
        }
    };
    write_key_ring(data_dir, app_id, &ring)?;
    Ok(ring)
}

fn derive_data_key(master_key: &[u8; 32], app_id: &str, purpose: &str) -> anyhow::Result<[u8; 32]> {
    let salt = format!("{}:{}", app_id, purpose);
    let hk = Hkdf::<Sha256>::new(Some(salt.as_bytes()), master_key);
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .map_err(|_| anyhow!("secure-store hkdf expand failed"))?;
    Ok(out)
}

fn encrypt_secret_with_key(
    master_key: &[u8; 32],
    app_id: &str,
    purpose: &str,
    plaintext: &str,
) -> anyhow::Result<(String, String)> {
    let data_key = derive_data_key(master_key, app_id, purpose)?;
    let cipher = Aes256Gcm::new_from_slice(&data_key).map_err(|_| anyhow!("cipher init failed"))?;

    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);

    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| anyhow!("secure-store encrypt failed"))?;

    let nonce_b64 = URL_SAFE_NO_PAD.encode(nonce);
    let ciphertext_b64 = URL_SAFE_NO_PAD.encode(ciphertext);
    Ok((
        nonce_b64.clone(),
        format!("{}:{}:{}", SECURE_STORE_VERSION, nonce_b64, ciphertext_b64),
    ))
}

pub fn encrypt_secret(
    data_dir: &str,
    app_id: &str,
    purpose: &str,
    plaintext: &str,
) -> anyhow::Result<String> {
    let ring = load_or_create_key_ring(data_dir, app_id)?;
    let active = active_key(&ring)?;
    let (nonce_b64, legacy_format) =
        encrypt_secret_with_key(&active.key, app_id, purpose, plaintext)?;
    let ciphertext_b64 = legacy_format
        .rsplit(':')
        .next()
        .ok_or_else(|| anyhow!("secure_store.encrypt_format_failed"))?;
    Ok(format!(
        "{}:{}:{}:{}",
        SECURE_STORE_VERSION_V2, active.id, nonce_b64, ciphertext_b64
    ))
}

pub fn is_encrypted_secret(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.starts_with(&format!("{}:", SECURE_STORE_VERSION_V2)) {
        let mut parts = trimmed.splitn(4, ':');
        let version = parts.next().unwrap_or_default();
        let key_id = parts.next().unwrap_or_default();
        let nonce_b64 = parts.next().unwrap_or_default();
        let ciphertext_b64 = parts.next().unwrap_or_default();
        if version != SECURE_STORE_VERSION_V2
            || key_id.is_empty()
            || nonce_b64.is_empty()
            || ciphertext_b64.is_empty()
        {
            return false;
        }
        let nonce_ok = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .map(|bytes| bytes.len() == 12)
            .unwrap_or(false);
        let ciphertext_ok = URL_SAFE_NO_PAD.decode(ciphertext_b64).is_ok();
        return nonce_ok && ciphertext_ok;
    }

    if !trimmed.starts_with(&format!("{}:", SECURE_STORE_VERSION)) {
        return false;
    }

    let mut parts = trimmed.splitn(3, ':');
    let version = parts.next().unwrap_or_default();
    let nonce_b64 = parts.next().unwrap_or_default();
    let ciphertext_b64 = parts.next().unwrap_or_default();
    if version != SECURE_STORE_VERSION || nonce_b64.is_empty() || ciphertext_b64.is_empty() {
        return false;
    }

    let nonce_ok = URL_SAFE_NO_PAD
        .decode(nonce_b64)
        .map(|bytes| bytes.len() == 12)
        .unwrap_or(false);
    let ciphertext_ok = URL_SAFE_NO_PAD.decode(ciphertext_b64).is_ok();
    nonce_ok && ciphertext_ok
}

pub fn encrypted_key_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if !trimmed.starts_with(&format!("{}:", SECURE_STORE_VERSION_V2)) {
        return None;
    }
    let mut parts = trimmed.splitn(4, ':');
    let version = parts.next().unwrap_or_default();
    let key_id = parts.next().unwrap_or_default();
    if version == SECURE_STORE_VERSION_V2 && !key_id.is_empty() {
        Some(key_id.to_string())
    } else {
        None
    }
}

pub fn migrate_legacy_secret(
    data_dir: &str,
    app_id: &str,
    purpose: &str,
    value: &str,
) -> anyhow::Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() || is_encrypted_secret(trimmed) {
        return Ok(None);
    }
    let encrypted = encrypt_secret(data_dir, app_id, purpose, trimmed)?;
    Ok(Some(encrypted))
}

pub fn decrypt_secret(
    data_dir: &str,
    app_id: &str,
    purpose: &str,
    value: &str,
) -> anyhow::Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if trimmed.starts_with(&format!("{}:", SECURE_STORE_VERSION_V2)) {
        let mut parts = trimmed.splitn(4, ':');
        let version = parts.next().unwrap_or_default();
        if version != SECURE_STORE_VERSION_V2 {
            return Err(anyhow!("unsupported secure-store version: {}", version));
        }
        let key_id = parts
            .next()
            .ok_or_else(|| anyhow!("missing key id in secret"))?;
        let nonce_b64 = parts
            .next()
            .ok_or_else(|| anyhow!("missing nonce in secret"))?;
        let ciphertext_b64 = parts
            .next()
            .ok_or_else(|| anyhow!("missing ciphertext in secret"))?;

        let ring = load_or_create_key_ring(data_dir, app_id)?;
        let key = find_key(&ring, key_id).ok_or_else(|| {
            anyhow!(
                "secure_store.key_not_found: encrypted key id {} is not in key ring",
                key_id
            )
        })?;
        let plaintext =
            decrypt_secret_with_key(&key.key, app_id, purpose, nonce_b64, ciphertext_b64)?;
        return Ok(Some(plaintext));
    }

    if !trimmed.starts_with(&format!("{}:", SECURE_STORE_VERSION)) {
        return Ok(Some(trimmed.to_string()));
    }

    let mut parts = trimmed.splitn(3, ':');
    let version = parts.next().unwrap_or_default();
    if version != SECURE_STORE_VERSION {
        return Err(anyhow!("unsupported secure-store version: {}", version));
    }
    let nonce_b64 = parts
        .next()
        .ok_or_else(|| anyhow!("missing nonce in secret"))?;
    let ciphertext_b64 = parts
        .next()
        .ok_or_else(|| anyhow!("missing ciphertext in secret"))?;

    let master_key = load_or_create_master_key(data_dir, app_id)?;
    let plaintext =
        decrypt_secret_with_key(&master_key, app_id, purpose, nonce_b64, ciphertext_b64)?;
    Ok(Some(plaintext))
}

fn decrypt_secret_with_key(
    master_key: &[u8; 32],
    app_id: &str,
    purpose: &str,
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> anyhow::Result<String> {
    let nonce_bytes = URL_SAFE_NO_PAD
        .decode(nonce_b64)
        .context("decode nonce failed")?;
    if nonce_bytes.len() != 12 {
        return Err(anyhow!(
            "invalid nonce length {}, expect 12",
            nonce_bytes.len()
        ));
    }

    let ciphertext = URL_SAFE_NO_PAD
        .decode(ciphertext_b64)
        .context("decode ciphertext failed")?;

    let data_key = derive_data_key(master_key, app_id, purpose)?;
    let cipher = Aes256Gcm::new_from_slice(&data_key).map_err(|_| anyhow!("cipher init failed"))?;

    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|_| anyhow!("secure-store decrypt failed"))?;
    let plaintext = String::from_utf8(plaintext).context("secure-store utf8 decode failed")?;
    Ok(plaintext)
}

pub fn reencrypt_secret(
    data_dir: &str,
    app_id: &str,
    purpose: &str,
    value: &str,
) -> anyhow::Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let plaintext = match decrypt_secret(data_dir, app_id, purpose, trimmed)? {
        Some(value) => value,
        None => return Ok(None),
    };

    let ring = load_or_create_key_ring(data_dir, app_id)?;
    if encrypted_key_id(trimmed)
        .map(|key_id| key_id == ring.active_key_id)
        .unwrap_or(false)
    {
        return Ok(None);
    }

    Ok(Some(encrypt_secret(data_dir, app_id, purpose, &plaintext)?))
}

pub fn rotate_master_key(data_dir: &str, app_id: &str) -> anyhow::Result<KeyRotationReport> {
    rotate_master_key_inner(data_dir, app_id, false)
}

fn rotate_master_key_inner(
    data_dir: &str,
    app_id: &str,
    force_failure_before_commit: bool,
) -> anyhow::Result<KeyRotationReport> {
    let original = load_or_create_key_ring(data_dir, app_id)?;
    let old_active_key_id = original.active_key_id.clone();
    let mut next = original.clone();
    let new_key_id = new_key_id();
    next.active_key_id = new_key_id.clone();
    next.keys.insert(
        0,
        KeyRecord {
            id: new_key_id.clone(),
            key: new_master_key(),
        },
    );

    let mut entries = vec![KeyRotationEntry {
        status: "planned".to_string(),
        message: format!("rotate active key {} -> {}", old_active_key_id, new_key_id),
    }];

    if force_failure_before_commit {
        write_key_ring(data_dir, app_id, &original)?;
        entries.push(KeyRotationEntry {
            status: "rolled_back".to_string(),
            message: "forced failure before commit; original key ring restored".to_string(),
        });
        return Ok(KeyRotationReport {
            status: KeyRotationStatus::FailedRolledBack,
            old_active_key_id,
            new_active_key_id: Some(new_key_id),
            keys_before: original.keys.len(),
            keys_after: original.keys.len(),
            entries,
        });
    }

    write_key_ring(data_dir, app_id, &next)?;
    entries.push(KeyRotationEntry {
        status: "committed".to_string(),
        message: "new active key ring committed".to_string(),
    });

    Ok(KeyRotationReport {
        status: KeyRotationStatus::Passed,
        old_active_key_id,
        new_active_key_id: Some(new_key_id),
        keys_before: original.keys.len(),
        keys_after: next.keys.len(),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decrypt_secret, encrypt_secret, encrypted_key_id, is_encrypted_secret, key_file_path,
        key_ring_file_path, migrate_legacy_secret, reencrypt_secret, rotate_master_key,
        rotate_master_key_inner, KeyRotationStatus, SECURE_STORE_VERSION,
    };
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn secure_store_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let plaintext = "runtime-core-secret-token";

        let encrypted = encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", plaintext)
            .expect("encrypt");
        assert_ne!(encrypted, plaintext);
        assert!(!encrypted.contains(plaintext));

        let decrypted = decrypt_secret(&data_dir, "cloud-api-hub", "session_cache", &encrypted)
            .expect("decrypt")
            .expect("decrypted value");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn legacy_plaintext_is_supported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let value = decrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "legacy-token")
            .expect("decrypt")
            .expect("legacy value");
        assert_eq!(value, "legacy-token");
    }

    #[test]
    fn detect_encrypted_secret_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let encrypted =
            encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "abc").expect("encrypt");
        assert!(is_encrypted_secret(&encrypted));
        assert!(!is_encrypted_secret("legacy-plaintext"));
    }

    #[test]
    fn migrate_legacy_secret_encrypts_plaintext() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let migrated =
            migrate_legacy_secret(&data_dir, "cloud-api-hub", "session_cache", "legacy-token")
                .expect("migrate")
                .expect("migrated value");
        assert!(is_encrypted_secret(&migrated));
        let decrypted = decrypt_secret(&data_dir, "cloud-api-hub", "session_cache", &migrated)
            .expect("decrypt")
            .expect("plaintext");
        assert_eq!(decrypted, "legacy-token");
    }

    #[test]
    fn decrypt_wrong_purpose_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let encrypted =
            encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "abc").expect("encrypt");
        let err = decrypt_secret(&data_dir, "cloud-api-hub", "other-purpose", &encrypted)
            .expect_err("wrong purpose should fail");
        assert!(
            format!("{:#}", err).contains("secure-store decrypt failed"),
            "unexpected error: {:#}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn key_file_permission_is_private_on_unix() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let _ =
            encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "abc").expect("encrypt");
        let key_path = key_ring_file_path(&data_dir, "cloud-api-hub");
        let mode = std::fs::metadata(key_path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode & 0o077, 0, "group/world bits should be zero");
    }

    #[test]
    fn rotate_new_writes_use_new_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let before = encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "abc")
            .expect("encrypt before");
        let before_key_id = encrypted_key_id(&before).expect("before key id");

        let report = rotate_master_key(&data_dir, "cloud-api-hub").expect("rotate");
        assert_eq!(report.status, KeyRotationStatus::Passed);
        let after = encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "def")
            .expect("encrypt after");
        let after_key_id = encrypted_key_id(&after).expect("after key id");

        assert_ne!(before_key_id, after_key_id);
        assert_eq!(Some(after_key_id), report.new_active_key_id);
        assert_eq!(
            decrypt_secret(&data_dir, "cloud-api-hub", "session_cache", &after)
                .expect("decrypt after")
                .expect("value"),
            "def"
        );
    }

    #[test]
    fn old_ciphertext_reads_and_reencrypts_to_active_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let before = encrypt_secret(&data_dir, "github-deployer", "session_token", "token-a")
            .expect("encrypt before");
        let before_key_id = encrypted_key_id(&before).expect("before key id");
        rotate_master_key(&data_dir, "github-deployer").expect("rotate");

        let read = decrypt_secret(&data_dir, "github-deployer", "session_token", &before)
            .expect("decrypt old")
            .expect("value");
        assert_eq!(read, "token-a");

        let rewrapped = reencrypt_secret(&data_dir, "github-deployer", "session_token", &before)
            .expect("reencrypt")
            .expect("rewrapped");
        let rewrapped_key_id = encrypted_key_id(&rewrapped).expect("rewrapped key id");
        assert_ne!(before_key_id, rewrapped_key_id);
        assert_eq!(
            decrypt_secret(&data_dir, "github-deployer", "session_token", &rewrapped)
                .expect("decrypt rewrapped")
                .expect("value"),
            "token-a"
        );
    }

    #[test]
    fn failed_rotation_rolls_back_to_old_active_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let before = encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "abc")
            .expect("encrypt before");
        let before_key_id = encrypted_key_id(&before).expect("before key id");

        let report = rotate_master_key_inner(&data_dir, "cloud-api-hub", true)
            .expect("forced rollback report");
        assert_eq!(report.status, KeyRotationStatus::FailedRolledBack);

        let after = encrypt_secret(&data_dir, "cloud-api-hub", "session_cache", "def")
            .expect("encrypt after rollback");
        let after_key_id = encrypted_key_id(&after).expect("after key id");
        assert_eq!(before_key_id, after_key_id);
    }

    #[test]
    fn legacy_v1_ciphertext_can_be_reencrypted_to_v2_key_ring() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let legacy_key_path = key_file_path(&data_dir, "cloud-api-hub");
        std::fs::create_dir_all(&data_dir).expect("create dir");
        let key = [7u8; 32];
        std::fs::write(&legacy_key_path, key).expect("write legacy key");
        #[cfg(unix)]
        std::fs::set_permissions(&legacy_key_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod legacy key");

        let (nonce, legacy) =
            super::encrypt_secret_with_key(&key, "cloud-api-hub", "session_cache", "legacy")
                .expect("legacy encrypt");
        assert!(legacy.starts_with(&format!("{}:{}:", SECURE_STORE_VERSION, nonce)));
        assert_eq!(
            decrypt_secret(&data_dir, "cloud-api-hub", "session_cache", &legacy)
                .expect("decrypt legacy v1")
                .expect("value"),
            "legacy"
        );

        let rewrapped = reencrypt_secret(&data_dir, "cloud-api-hub", "session_cache", &legacy)
            .expect("reencrypt")
            .expect("rewrapped");
        assert!(rewrapped.starts_with("enc-v2:"));
        assert_eq!(
            decrypt_secret(&data_dir, "cloud-api-hub", "session_cache", &rewrapped)
                .expect("decrypt rewrapped")
                .expect("value"),
            "legacy"
        );
    }
}
