use super::system::{get_decrypted_config, set_encrypted_config};
use crate::types::{VendorKeysDoc, VendorOAuthDoc};
use anyhow::Result;
use rusqlite::Connection;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_db() -> rusqlite::Connection {
        crate::db::open_db(":memory:").expect("in-memory db")
    }

    #[test]
    fn test_vendor_keys_default_when_empty() {
        let conn = make_db();
        let doc = load_vendor_keys_doc(&conn);
        assert!(doc.keys.is_empty());
    }

    #[test]
    fn test_vendor_keys_round_trip() {
        let conn = make_db();
        let mut doc = VendorKeysDoc::default();
        let key = crate::types::VendorKey {
            vendor_id: "openai".to_string(),
            label: "My OpenAI Key".to_string(),
            auth_values: {
                let mut m = HashMap::new();
                m.insert("api_key".to_string(), "sk-test-abc".to_string());
                m
            },
            max_concurrent: 5,
            requests_per_second: 10.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
        };
        doc.keys.insert("key-001".to_string(), key);

        save_vendor_keys_doc(&conn, &doc).unwrap();
        let loaded = load_vendor_keys_doc(&conn);

        assert_eq!(loaded.keys.len(), 1);
        let loaded_key = loaded.keys.get("key-001").unwrap();
        assert_eq!(loaded_key.vendor_id, "openai");
        assert_eq!(loaded_key.label, "My OpenAI Key");
        assert_eq!(
            loaded_key.auth_values.get("api_key"),
            Some(&"sk-test-abc".to_string())
        );
        assert!(loaded_key.enabled);
    }

    #[test]
    fn test_vendor_oauth_default_when_empty() {
        let conn = make_db();
        let doc = load_vendor_oauth_doc(&conn);
        assert!(doc.configs.is_empty());
    }

    #[test]
    fn test_vendor_oauth_round_trip() {
        let conn = make_db();
        let mut doc = VendorOAuthDoc::default();
        let config = crate::types::OAuthConfig {
            vendor_id: "deepl".to_string(),
            label: "DeepL OAuth".to_string(),
            grant_type: "client_credentials".to_string(),
            auth_url: String::new(),
            token_url: "https://api.deepl.com/oauth/token".to_string(),
            client_id: "client-123".to_string(),
            client_secret: "secret-abc".to_string(),
            scopes: "translate".to_string(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: None,
            cached_token_expires_at: 0,
            refresh_token: None,
            max_concurrent: 5,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 0.0,
            token_field: "access_token".to_string(),
        };
        doc.configs.insert("oauth-001".to_string(), config);

        save_vendor_oauth_doc(&conn, &doc).unwrap();
        let loaded = load_vendor_oauth_doc(&conn);

        assert_eq!(loaded.configs.len(), 1);
        let loaded_config = loaded.configs.get("oauth-001").unwrap();
        assert_eq!(loaded_config.vendor_id, "deepl");
        assert_eq!(loaded_config.grant_type, "client_credentials");
        assert_eq!(loaded_config.token_url, "https://api.deepl.com/oauth/token");
    }

    #[test]
    fn test_vendor_key_max_file_size_mb_round_trip() {
        let conn = make_db();
        let mut doc = VendorKeysDoc::default();
        let key = crate::types::VendorKey {
            vendor_id: "openai".to_string(),
            label: "Key with file limit".to_string(),
            auth_values: std::collections::HashMap::new(),
            max_concurrent: 3,
            requests_per_second: 5.0,
            weight: 1,
            enabled: true,
            max_input_chars: 0,
            max_file_size_mb: 25.5,
        };
        doc.keys.insert("key-fsz".to_string(), key);

        save_vendor_keys_doc(&conn, &doc).unwrap();
        let loaded = load_vendor_keys_doc(&conn);

        let loaded_key = loaded.keys.get("key-fsz").unwrap();
        assert_eq!(loaded_key.max_file_size_mb, 25.5);
    }

    #[test]
    fn test_vendor_oauth_max_file_size_mb_round_trip() {
        let conn = make_db();
        let mut doc = VendorOAuthDoc::default();
        let config = crate::types::OAuthConfig {
            vendor_id: "google".to_string(),
            label: "OAuth with file limit".to_string(),
            grant_type: "client_credentials".to_string(),
            auth_url: String::new(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            client_id: "client-abc".to_string(),
            client_secret: "secret-xyz".to_string(),
            scopes: String::new(),
            extra_params: HashMap::new(),
            auth_extra_params: HashMap::new(),
            cached_token: None,
            cached_token_expires_at: 0,
            refresh_token: None,
            max_concurrent: 2,
            weight: 1,
            max_input_chars: 0,
            max_file_size_mb: 50.0,
            token_field: "access_token".to_string(),
        };
        doc.configs.insert("oauth-fsz".to_string(), config);

        save_vendor_oauth_doc(&conn, &doc).unwrap();
        let loaded = load_vendor_oauth_doc(&conn);

        let loaded_cfg = loaded.configs.get("oauth-fsz").unwrap();
        assert_eq!(loaded_cfg.max_file_size_mb, 50.0);
    }
}
