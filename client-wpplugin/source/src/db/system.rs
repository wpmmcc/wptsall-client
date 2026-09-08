use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rusqlite::Connection;
use uuid::Uuid;

pub(crate) fn get_system_config(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM system_config WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    )
    .ok()
}

pub(crate) fn set_system_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO system_config (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Store a JSON value encrypted (AES-256-GCM, WPTC format, base64-encoded).
/// Falls back to plain text when no encryption secret is available.
/// Backward-compatible: `get_decrypted_config` handles both encrypted and plain values.
pub(crate) fn set_encrypted_config(conn: &Connection, key: &str, json_str: &str) -> Result<()> {
    let encrypted_bytes = crate::bindings::encrypt_for_save(json_str)?;
    // If encrypt_for_save returned WPTC binary (has secret), base64-encode for TEXT column.
    // If no secret, it returns plain JSON bytes — store as-is.
    if encrypted_bytes.len() >= 4 && &encrypted_bytes[..4] == b"WPTC" {
        let encoded = BASE64_STANDARD.encode(&encrypted_bytes);
        set_system_config(conn, key, &encoded)
    } else {
        // No secret available — store plain JSON
        set_system_config(conn, key, json_str)
    }
}

/// Read a config value that may be encrypted (base64-encoded WPTC) or plain JSON.
/// Returns None if the key doesn't exist. Returns the decrypted JSON string on success.
pub(crate) fn get_decrypted_config(conn: &Connection, key: &str) -> Option<String> {
    let stored = get_system_config(conn, key)?;
    if stored.trim().is_empty() {
        return Some(stored);
    }

    // Try base64 decode → WPTC decrypt
    if let Ok(decoded) = BASE64_STANDARD.decode(stored.as_bytes()) {
        if let Ok(plain) = crate::bindings::decrypt_from_bytes(&decoded) {
            return Some(plain);
        }
    }

    // Not base64 or decrypt failed — treat as plain JSON (backward compat)
    Some(stored)
}

pub(crate) fn load_or_create_device_id(conn: &Connection) -> Result<String> {
    if let Some(id) = get_system_config(conn, "device_id") {
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
    if get_system_config(conn, "device_id").is_some() {
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
    if get_system_config(conn, "signing_public_key").is_some() {
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

    #[test]
    fn encrypted_config_plaintext_fallback_pin_known_red() {
        // KNOWN-RED PIN — SEC-02 default plaintext credentials (plan §7
        // TEST-CREDENTIAL-AT-REST-001; gap doc SEC-02 "真实未闭环"):
        // bindings_secret() resolves from WPTSALL_COMPONENT_BINDINGS_SECRET
        // or WPTSALL_DEVICE_ID, but WebUI/worker never wire the DB-loaded
        // device_id into the environment — with both empty,
        // set_encrypted_config() stores the JSON in PLAIN TEXT. At-rest probe:
        // a real temp-file DB contains the marker string verbatim. Flip when
        // the product lane derives a default secret (device-id binding): the
        // stored value becomes base64 WPTC and the marker disappears from
        // the file bytes.
        let _unset_secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "");
        let _unset_device = TestEnvVarGuard::set("WPTSALL_DEVICE_ID", "");

        let marker = "sec02-plaintext-marker-9876543210";
        let json_str = format!(r#"{{"api_key":"{marker}","endpoint":"https://example.com"}}"#);

        // Stored value pin: plain JSON, not base64-WPTC.
        let conn = make_db();
        set_encrypted_config(&conn, "sec02_pin", &json_str).unwrap();
        let raw = get_system_config(&conn, "sec02_pin").unwrap();
        assert_eq!(
            raw, json_str,
            "KNOWN-RED pin (SEC-02): config is stored as plain JSON today; if it is now base64/WPTC the default secret landed — flip the pin"
        );

        // At-rest probe: the marker is readable straight from the DB file.
        let db_path = std::env::temp_dir().join(format!(
            "wptsall-sec02-pin-{}.db",
            std::process::id()
        ));
        let file_conn = crate::db::open_db(db_path.to_str().expect("temp path utf-8"))
            .expect("open temp db");
        set_encrypted_config(&file_conn, "sec02_pin", &json_str).unwrap();
        drop(file_conn);

        let bytes = std::fs::read(&db_path).expect("read temp db bytes");
        let marker_bytes = marker.as_bytes();
        assert!(
            bytes.windows(marker_bytes.len()).any(|w| w == marker_bytes),
            "KNOWN-RED pin (SEC-02): the api_key marker is present in plaintext in the SQLite file"
        );
        let _ = std::fs::remove_file(&db_path);

        // Round trip stays lossless either way (backward-compat path).
        let decrypted = get_decrypted_config(&conn, "sec02_pin").unwrap();
        assert_eq!(decrypted, json_str);
    }
}
