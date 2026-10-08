use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rusqlite::{Connection, OptionalExtension};
use uuid::Uuid;

pub(crate) fn get_system_config(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM system_config WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    )
    .ok()
}

pub(crate) fn get_system_config_checked(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM system_config WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .optional()?)
}

struct ConfigurationWrite<'a> {
    conn: &'a Connection,
    committed: bool,
}

impl<'a> ConfigurationWrite<'a> {
    fn begin(conn: &'a Connection) -> Result<Self> {
        conn.execute_batch("SAVEPOINT checked_configuration_write")?;
        Ok(Self {
            conn,
            committed: false,
        })
    }

    fn commit(mut self) -> Result<()> {
        self.conn
            .execute_batch("RELEASE checked_configuration_write")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for ConfigurationWrite<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch(
                "ROLLBACK TO checked_configuration_write; RELEASE checked_configuration_write",
            );
        }
    }
}

pub(crate) fn set_system_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let write = ConfigurationWrite::begin(conn)?;
    let changed = conn.execute(
        "INSERT OR REPLACE INTO system_config (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    anyhow::ensure!(
        changed == 1 && get_system_config_checked(conn, key)?.as_deref() == Some(value),
        "configuration write was not confirmed; original value retained"
    );
    write.commit()
}

/// Store a JSON value encrypted (AES-256-GCM, WPTC format, base64-encoded).
/// Missing key material is an error. Legacy plaintext remains readable.
pub(crate) fn set_encrypted_config(conn: &Connection, key: &str, json_str: &str) -> Result<()> {
    set_system_config(conn, key, &encrypt_config_value(json_str)?)
}

pub(crate) fn encrypt_config_value(plain: &str) -> Result<String> {
    Ok(BASE64_STANDARD.encode(crate::bindings::encrypt_for_save(plain)?))
}

pub(crate) fn decrypt_config_value(stored: &str) -> Result<String> {
    // Earlier documents could base64-wrap JSON or the legacy JSON envelope,
    // not only WPTC bytes. Decode once, and never retry failed decryption as
    // plaintext (wrong keys and damaged ciphertext must remain errors).
    let bytes = BASE64_STANDARD
        .decode(stored.as_bytes())
        .or_else(|error| {
            if stored.starts_with("V1BUQw") {
                Err(error)
            } else {
                Ok(stored.as_bytes().to_vec())
            }
        })
        .map_err(|_| anyhow::anyhow!("damaged encrypted configuration envelope"))?;
    crate::bindings::decrypt_from_bytes(&bytes)
        .map_err(|_| anyhow::anyhow!("cannot decrypt stored configuration"))
}

/// Indexed site keys must not duplicate route secrets embedded in API URLs.
pub(crate) fn private_site_key(url: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("wptsall-site-v1:{:x}", Sha256::digest(url.as_bytes()))
}

/// Stable private fingerprints must not depend on HashMap iteration order.
pub(crate) fn private_json_digest(value: &serde_json::Value) -> Result<String> {
    fn canonicalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                map.sort_keys();
                map.values_mut().for_each(canonicalize);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(canonicalize),
            _ => {}
        }
    }
    let mut canonical = value.clone();
    canonicalize(&mut canonical);
    Ok(crate::sync_engine::hmac::sha256_hex(
        serde_json::to_string(&canonical)?.as_bytes(),
    ))
}

/// Read a config value that may be encrypted (base64-encoded WPTC) or plain JSON.
/// Returns None if the key doesn't exist. Returns the decrypted JSON string on success.
pub(crate) fn get_decrypted_config(conn: &Connection, key: &str) -> Option<String> {
    get_decrypted_config_checked(conn, key).ok().flatten()
}

pub(crate) fn get_decrypted_config_checked(conn: &Connection, key: &str) -> Result<Option<String>> {
    let Some(stored) = get_system_config_checked(conn, key)? else {
        return Ok(None);
    };
    let plain = decrypt_config_value(&stored)
        .map_err(|_| anyhow::anyhow!("cannot decrypt configuration {key}"))?;
    Ok(Some(plain))
}

pub(crate) fn load_or_create_device_id(conn: &Connection) -> Result<String> {
    if let Some(id) = get_system_config_checked(conn, "device_id")? {
        if !id.trim().is_empty() {
            return Ok(id);
        }
    }
    let new_id = Uuid::new_v4().to_string();
    set_system_config(conn, "device_id", &new_id)?;
    Ok(new_id)
}

pub(crate) fn get_signing_key(conn: &Connection) -> Option<String> {
    get_system_config(conn, "signing_public_key")
}

pub(crate) fn get_signing_key_id(conn: &Connection) -> Option<String> {
    get_system_config(conn, "signing_public_key_id")
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn set_signing_key(conn: &Connection, pem: &str) -> Result<()> {
    set_system_config(conn, "signing_public_key", pem)
}

pub(crate) fn set_signing_key_material(
    conn: &Connection,
    pem: &str,
    key_id: Option<&str>,
) -> Result<()> {
    set_system_config(conn, "signing_public_key", pem)?;
    if let Some(id) = key_id.map(str::trim).filter(|v| !v.is_empty()) {
        set_system_config(conn, "signing_public_key_id", id)?;
    } else {
        // Keep backward compatibility while preventing stale key_id mismatch.
        set_system_config(conn, "signing_public_key_id", "")?;
    }
    Ok(())
}

// Migration helpers - called once on first startup
pub(crate) fn migrate_device_id_from_file(conn: &Connection) -> Result<()> {
    if get_system_config_checked(conn, "device_id")?.is_some() {
        return Ok(());
    }
    if let Ok(id) = std::fs::read_to_string("./runtime/device-id.txt") {
        let id = id.trim().to_string();
        if !id.is_empty() {
            set_system_config(conn, "device_id", &id)?;
        }
    }
    Ok(())
}

pub(crate) fn migrate_signing_key_from_file(conn: &Connection) -> Result<()> {
    if get_system_config_checked(conn, "signing_public_key")?.is_some() {
        return Ok(());
    }
    if let Ok(pem) = std::fs::read_to_string("./config/signing-public-key.pem") {
        let pem = pem.trim().to_string();
        if !pem.is_empty() {
            set_system_config(conn, "signing_public_key", &pem)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::super::TestEnvVarGuard;
    use super::*;

    fn make_db() -> Connection {
        crate::db::open_db(":memory:").expect("in-memory db")
    }

    #[test]
    fn test_set_get_system_config() {
        let conn = make_db();
        set_system_config(&conn, "my_key", "my_value").unwrap();
        let result = get_system_config(&conn, "my_key");
        assert_eq!(result, Some("my_value".to_string()));
    }

    #[test]
    fn configuration_authority_ignored_system_save_does_not_claim_success() {
        let conn = make_db();
        set_system_config(&conn, "owned-config", "original").unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_owned BEFORE INSERT ON system_config
            WHEN NEW.key='owned-config' BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
        assert!(
            set_system_config(&conn, "owned-config", "new").is_err(),
            "ignored configuration writes must not report success"
        );
        assert_eq!(
            get_system_config_checked(&conn, "owned-config")
                .unwrap()
                .as_deref(),
            Some("original")
        );
    }

    #[test]
    fn configuration_authority_redirected_system_save_rolls_back_the_original_value() {
        let conn = make_db();
        set_system_config(&conn, "owned-config", "original").unwrap();
        conn.execute_batch(
            "CREATE TRIGGER redirect_owned AFTER INSERT ON system_config
            WHEN NEW.key='owned-config' BEGIN UPDATE system_config SET value='different'
                WHERE key=NEW.key; END;",
        )
        .unwrap();
        assert!(
            set_system_config(&conn, "owned-config", "new").is_err(),
            "readback disagreement must not commit a different configuration"
        );
        assert_eq!(
            get_system_config_checked(&conn, "owned-config")
                .unwrap()
                .as_deref(),
            Some("original")
        );
    }

    #[test]
    fn test_get_missing_key_returns_none() {
        let conn = make_db();
        let result = get_system_config(&conn, "nonexistent_key");
        assert_eq!(result, None);
    }

    #[test]
    fn test_load_or_create_device_id_creates_new() {
        let conn = make_db();
        let id = load_or_create_device_id(&conn).unwrap();
        assert!(!id.is_empty());
        // Should look like a UUID
        assert_eq!(id.len(), 36);
    }

    #[test]
    fn test_load_or_create_device_id_stable() {
        let conn = make_db();
        let id1 = load_or_create_device_id(&conn).unwrap();
        let id2 = load_or_create_device_id(&conn).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_signing_key_round_trip() {
        let conn = make_db();
        let pem = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkq...\n-----END PUBLIC KEY-----";
        set_signing_key(&conn, pem).unwrap();
        let result = get_signing_key(&conn);
        assert_eq!(result, Some(pem.to_string()));
    }

    #[test]
    fn test_signing_key_material_round_trip_with_key_id() {
        let conn = make_db();
        let pem = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkq...\n-----END PUBLIC KEY-----";
        let key_id = "abcd1234ef567890";
        set_signing_key_material(&conn, pem, Some(key_id)).unwrap();
        assert_eq!(get_signing_key(&conn), Some(pem.to_string()));
        assert_eq!(get_signing_key_id(&conn), Some(key_id.to_string()));
    }

    #[test]
    fn test_encrypted_config_round_trip() {
        let _secret_guard = TestEnvVarGuard::set(
            "WPTSALL_COMPONENT_BINDINGS_SECRET",
            "test-component-bindings-secret",
        );
        let conn = make_db();
        let json_str = r#"{"api_key":"secret-value-123","endpoint":"https://example.com"}"#;
        set_encrypted_config(&conn, "test_encrypted", json_str).unwrap();

        // Raw stored value should be base64 (not plain JSON)
        let raw = get_system_config(&conn, "test_encrypted").unwrap();
        assert_ne!(raw, json_str, "should be encrypted, not plain text");

        // Decrypted value should match original
        let decrypted = get_decrypted_config(&conn, "test_encrypted").unwrap();
        assert_eq!(decrypted, json_str);
    }

    #[test]
    fn test_get_decrypted_config_plain_json_backward_compat() {
        let conn = make_db();
        // Store plain JSON directly (simulates pre-encryption data)
        let json_str = r#"{"key":"plain-value"}"#;
        set_system_config(&conn, "legacy_plain", json_str).unwrap();

        // get_decrypted_config should return it as-is
        let result = get_decrypted_config(&conn, "legacy_plain").unwrap();
        assert_eq!(result, json_str);
    }

    #[test]
    fn test_get_decrypted_config_missing_key() {
        let conn = make_db();
        let result = get_decrypted_config(&conn, "nonexistent");
        assert_eq!(result, None);
    }

    /// Serializes tests that manipulate the process-wide default device
    /// identity (the OnceLock in bindings::crypto) so parallel test
    /// threads never observe another test's registration.
    fn default_device_id_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn encrypted_config_uses_boot_default_device_id_secret() {
        // SEC-02 FLIPPED (07 S3, 12 批 A5): the boot-resolved DB device_id
        // is registered as the default key source
        // (bindings::set_default_device_id — wired in the WebUI and both
        // worker entrypoints), so at-rest config is encrypted even with
        // both env knobs unset, and the DB file carries zero plaintext.
        let _guard = default_device_id_test_lock();
        crate::bindings::clear_default_device_id_for_tests();
        let _unset_secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "");
        let _unset_device = TestEnvVarGuard::set("WPTSALL_DEVICE_ID", "");
        crate::bindings::set_default_device_id("sec02-boot-device-identity");

        let marker = "sec02-plaintext-marker-9876543210";
        let json_str = format!(r#"{{"api_key":"{marker}","endpoint":"https://example.com"}}"#);

        // Stored value: base64-encoded WPTC binary, not plain JSON.
        let conn = make_db();
        set_encrypted_config(&conn, "sec02_pin", &json_str).unwrap();
        let raw = get_system_config(&conn, "sec02_pin").unwrap();
        assert_ne!(
            raw, json_str,
            "with a boot default identity the config must be encrypted, not plain JSON"
        );
        let decoded = BASE64_STANDARD
            .decode(raw.as_bytes())
            .expect("stored value must be base64");
        assert!(
            decoded.len() >= 4 && &decoded[..4] == b"WPTC",
            "stored value must decode to WPTC binary"
        );

        // At-rest probe: zero plaintext marker in the SQLite file bytes.
        let db_path =
            std::env::temp_dir().join(format!("wptsall-sec02-pin-{}.db", std::process::id()));
        let file_conn =
            crate::db::open_db(db_path.to_str().expect("temp path utf-8")).expect("open temp db");
        set_encrypted_config(&file_conn, "sec02_pin", &json_str).unwrap();
        drop(file_conn);

        let bytes = std::fs::read(&db_path).expect("read temp db bytes");
        let marker_bytes = marker.as_bytes();
        assert!(
            !bytes.windows(marker_bytes.len()).any(|w| w == marker_bytes),
            "SEC-02 closed (07 S3): the api_key marker must NOT appear in plaintext in the SQLite file"
        );
        let _ = std::fs::remove_file(&db_path);

        // Round trip decrypts losslessly with the same default identity.
        let decrypted = get_decrypted_config(&conn, "sec02_pin").unwrap();
        assert_eq!(decrypted, json_str);

        crate::bindings::clear_default_device_id_for_tests();
    }

    #[test]
    fn encrypted_config_without_any_identity_source_preserves_old_value() {
        let _guard = default_device_id_test_lock();
        crate::bindings::clear_default_device_id_for_tests();
        let _unset_secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "");
        let _unset_device = TestEnvVarGuard::set("WPTSALL_DEVICE_ID", "");

        let conn = make_db();
        set_system_config(&conn, "sec02_compat", r#"{"api_key":"old"}"#).unwrap();
        assert!(set_encrypted_config(&conn, "sec02_compat", r#"{"api_key":"new"}"#).is_err());
        let raw = get_system_config(&conn, "sec02_compat").unwrap();
        assert_eq!(
            raw, r#"{"api_key":"old"}"#,
            "no-identity-source save must retain the previous bytes"
        );
        let decrypted = get_decrypted_config(&conn, "sec02_compat").unwrap();
        assert_eq!(decrypted, r#"{"api_key":"old"}"#);

        crate::bindings::clear_default_device_id_for_tests();
    }
}