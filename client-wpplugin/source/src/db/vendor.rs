use super::system::{get_decrypted_config, set_encrypted_config};
use crate::types::{VendorKeysDoc, VendorOAuthDoc};
use anyhow::Result;
use rusqlite::Connection;

pub(crate) fn oauth_profile_lock_suffix(id: &str) -> Result<String> {
    Ok(format!(
        "oauth-authorization-head-v1-{}",
        super::system::private_json_digest(&serde_json::json!({"id":id}))?
    ))
}

pub(crate) fn assert_oauth_authorization_settled(conn: &Connection, id: &str) -> Result<()> {
    let suffix = oauth_profile_lock_suffix(id)?;
    let head_key = suffix.replacen(
        "oauth-authorization-head-v1-",
        "oauth-authorization-head-v1:",
        1,
    );
    let Some(raw) = super::system::get_system_config_checked(conn, &head_key)? else {
        return Ok(());
    };
    anyhow::ensure!(
        raw.starts_with("V1BUQw"),
        "OAuth authorization head is not encrypted; retained"
    );
    let state: String = serde_json::from_str(&super::system::decrypt_config_value(&raw)?)?;
    let flow_key = format!(
        "oauth-authorization-v1:{}",
        super::system::private_json_digest(&serde_json::json!({"state":state}))?
    );
    let raw = super::system::get_system_config_checked(conn, &flow_key)?
        .ok_or_else(|| anyhow::anyhow!("OAuth authorization head has no checkpoint; retained"))?;
    anyhow::ensure!(
        raw.starts_with("V1BUQw"),
        "OAuth authorization checkpoint is not encrypted; retained"
    );
    let flow: serde_json::Value =
        serde_json::from_str(&super::system::decrypt_config_value(&raw)?)?;
    anyhow::ensure!(
        flow["format"] == "oauth-authorization-v1"
            && flow["config_id"] == id
            && flow["oauth_state"] == state,
        "OAuth authorization checkpoint scope differs; retained"
    );
    anyhow::ensure!(
        flow["phase"] == "pending" || flow["phase"] == "applied",
        "OAUTH_AUTHORIZATION_UNKNOWN: original authorization token issue is unresolved; retained"
    );
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn load_vendor_keys_doc(conn: &Connection) -> VendorKeysDoc {
    get_decrypted_config(conn, "vendor_keys_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_vendor_keys_doc(conn: &Connection, doc: &VendorKeysDoc) -> Result<()> {
    set_encrypted_config(conn, "vendor_keys_doc", &serde_json::to_string(doc)?)
}

#[allow(dead_code)]
pub(crate) fn load_vendor_oauth_doc(conn: &Connection) -> VendorOAuthDoc {
    get_decrypted_config(conn, "vendor_oauth_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_vendor_oauth_doc(conn: &Connection, doc: &VendorOAuthDoc) -> Result<()> {
    set_encrypted_config(conn, "vendor_oauth_doc", &serde_json::to_string(doc)?)
}

pub(crate) fn migrate_vendor_keys_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::vendor_keys_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_vendor_keys(&path)?;
        save_vendor_keys_doc(conn, &doc)?;
    }
    Ok(())
}

pub(crate) fn migrate_vendor_oauth_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::vendor_oauth_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_vendor_oauth(&path)?;
        save_vendor_oauth_doc(conn, &doc)?;
    }
    Ok(())
}
