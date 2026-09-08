use super::system::{get_decrypted_config, set_encrypted_config};
use crate::types::ComponentsLocalDoc;
use anyhow::Result;
use rusqlite::Connection;

fn with_runtime_conn<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = crate::config::env_or("WPTSALL_DB_PATH", "./runtime/wptsall.db");
    let conn = crate::db::open_db(&db_path)?;
    let _ = crate::db::migrate_from_json_if_needed(&conn);
    f(&conn)
}

#[allow(dead_code)]
pub(crate) fn load_local_components_doc(conn: &Connection) -> ComponentsLocalDoc {
    get_decrypted_config(conn, "local_components_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_local_components_doc(conn: &Connection, doc: &ComponentsLocalDoc) -> Result<()> {
    set_encrypted_config(conn, "local_components_doc", &serde_json::to_string(doc)?)
}

pub(crate) fn load_runtime_local_components_doc() -> ComponentsLocalDoc {
    with_runtime_conn(|conn| Ok(load_local_components_doc(conn))).unwrap_or_default()
}

pub(crate) fn save_runtime_local_components_doc(doc: &ComponentsLocalDoc) -> Result<()> {
    with_runtime_conn(|conn| save_local_components_doc(conn, doc))
}

pub(crate) fn migrate_local_components_from_json(conn: &Connection) -> Result<()> {
    let path = std::env::var("WPTSALL_COMPONENTS_LOCAL_FILE")
        .unwrap_or_else(|_| crate::config::DEFAULT_COMPONENTS_LOCAL_FILE.to_string());
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_components_local(&path)?;
        save_local_components_doc(conn, &doc)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::TestEnvVarGuard;
    use super::*;
    use std::collections::HashMap;

    fn make_db() -> rusqlite::Connection {
        crate::db::open_db(":memory:").expect("in-memory db")
    }

    #[test]
    fn test_local_components_default_when_empty() {
        let conn = make_db();
        let doc = load_local_components_doc(&conn);
        assert!(doc.components.is_empty());
    }

    #[test]
    fn test_local_components_round_trip() {
        let _device_guard = TestEnvVarGuard::set(
            "WPTSALL_DEVICE_ID",
            "test-device-id-for-local-components-encryption",
        );
        let conn = make_db();
        let mut doc = ComponentsLocalDoc::default();

        let version = crate::types::ComponentVersion {
            version: "1.0.0".to_string(),
            remarks: "Initial version".to_string(),
            key_ids: vec!["key-001".to_string()],
            key_selection_strategy: "round_robin".to_string(),
            proxy_profile_id: None,
            auth_type: "key".to_string(),
            config_overrides: HashMap::new(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
        };

        let mut versions = HashMap::new();
        versions.insert("1.0.0".to_string(), version);

        let instance = crate::types::ComponentInstanceLocal {
            name: "My OpenAI Translator".to_string(),
            template_id: "tmpl-openai-v1".to_string(),
            source_template_id: "tmpl-openai-v1".to_string(),
            source_template_updated_at: Some("2024-01-01T00:00:00Z".to_string()),
            source_template_api_version: Some("1.0.0".to_string()),
            vendor_id: "openai".to_string(),
            vendor_name: "OpenAI".to_string(),
            kind: "translate".to_string(),
            remarks: "Primary translation component".to_string(),
            enabled: true,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: Some("2024-01-01T00:00:00Z".to_string()),
            component_overrides: None,
            versions,
            active_version: None,
            template_json: None,
        };

        doc.components.insert("inst-001".to_string(), instance);

        save_local_components_doc(&conn, &doc).unwrap();

        let raw = super::super::system::get_system_config(&conn, "local_components_doc").unwrap();
        assert_ne!(
            raw,
            serde_json::to_string(&doc).unwrap(),
            "local components doc should be encrypted when a bindings secret is available"
        );

        let loaded = load_local_components_doc(&conn);

        assert_eq!(loaded.components.len(), 1);
        let loaded_inst = loaded.components.get("inst-001").unwrap();
        assert_eq!(loaded_inst.name, "My OpenAI Translator");
        assert_eq!(loaded_inst.template_id, "tmpl-openai-v1");
        assert_eq!(loaded_inst.vendor_id, "openai");
        assert_eq!(loaded_inst.kind, "translate");
        assert_eq!(loaded_inst.versions.len(), 1);
        assert!(loaded_inst.versions.contains_key("1.0.0"));
    }

    #[test]
    fn test_local_components_plain_json_backward_compat() {
        let conn = make_db();
        let plain = r#"{"version":2,"components":{"legacy":{"name":"Legacy Component","template_id":"tmpl-legacy","source_template_id":"tmpl-legacy","vendor_id":"legacy","vendor_name":"Legacy","kind":"translate","remarks":"","enabled":true,"created_at":"2024-01-01T00:00:00Z","versions":{}}}}"#;

        super::super::system::set_system_config(&conn, "local_components_doc", plain).unwrap();

        let loaded = load_local_components_doc(&conn);
        assert!(loaded.components.contains_key("legacy"));
    }
}
