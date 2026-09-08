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
    let path = std::env::var("WPTSALL_PROXY_PROFILES_FILE")
        .unwrap_or_else(|_| crate::config::DEFAULT_PROXY_PROFILES_FILE.to_string());
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_proxy_profiles(&path)?;
        save_proxy_profiles_doc(conn, &doc)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::TestEnvVarGuard;
    use super::*;

    fn make_db() -> rusqlite::Connection {
        crate::db::open_db(":memory:").expect("in-memory db")
    }

    #[test]
    fn test_proxy_profiles_default_when_empty() {
        let conn = make_db();
        let doc = load_proxy_profiles_doc(&conn);
        assert!(doc.profiles.is_empty());
    }

    #[test]
    fn test_proxy_profiles_round_trip() {
        let _device_guard =
            TestEnvVarGuard::set("WPTSALL_DEVICE_ID", "test-device-id-for-proxy-encryption");
        let conn = make_db();
        let mut doc = ProxyProfilesDoc::default();
        let profile = crate::types::ProxyProfile {
            name: "My SOCKS5 Proxy".to_string(),
            protocol: "socks5".to_string(),
            host: "127.0.0.1".to_string(),
            port: 1080,
            username: "user".to_string(),
            password: "pass".to_string(),
            enabled: true,
        };
        doc.profiles.insert("proxy-001".to_string(), profile);

        save_proxy_profiles_doc(&conn, &doc).unwrap();

        let raw = super::super::system::get_system_config(&conn, "proxy_profiles_doc").unwrap();
        assert_ne!(
            raw,
            serde_json::to_string(&doc).unwrap(),
            "proxy profiles should be encrypted when a bindings secret is available"
        );

        let loaded = load_proxy_profiles_doc(&conn);

        assert_eq!(loaded.profiles.len(), 1);
        let loaded_profile = loaded.profiles.get("proxy-001").unwrap();
        assert_eq!(loaded_profile.name, "My SOCKS5 Proxy");
        assert_eq!(loaded_profile.protocol, "socks5");
        assert_eq!(loaded_profile.host, "127.0.0.1");
        assert_eq!(loaded_profile.port, 1080);
        assert!(loaded_profile.enabled);
    }

    #[test]
    fn test_proxy_profiles_plain_json_backward_compat() {
        let conn = make_db();
        let plain = r#"{"version":2,"profiles":{"legacy":{"name":"Legacy HTTP","protocol":"http","host":"127.0.0.1","port":8080,"username":"","password":"","enabled":true}}}"#;

        super::super::system::set_system_config(&conn, "proxy_profiles_doc", plain).unwrap();

        let loaded = load_proxy_profiles_doc(&conn);
        assert!(loaded.profiles.contains_key("legacy"));
    }
}
