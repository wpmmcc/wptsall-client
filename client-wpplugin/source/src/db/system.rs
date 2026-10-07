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

#[allow (dead_code)]
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
