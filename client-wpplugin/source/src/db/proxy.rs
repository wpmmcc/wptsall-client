use super::system::{get_decrypted_config, set_encrypted_config};
use crate::types::ProxyProfilesDoc;
use anyhow::Result;
use rusqlite::Connection;

#[allow(dead_code)]
pub(crate) fn load_proxy_profiles_doc(conn: &Connection) -> ProxyProfilesDoc {
    get_decrypted_config(conn, "proxy_profiles_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_proxy_profiles_doc(conn: &Connection, doc: &ProxyProfilesDoc) -> Result<()> {
    set_encrypted_config(conn, "proxy_profiles_doc", &serde_json::to_string(doc)?)
}

pub(crate) fn migrate_proxy_profiles_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::proxy_profiles_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_proxy_profiles(&path)?;
        save_proxy_profiles_doc(conn, &doc)?;
    }
    Ok(())
}
