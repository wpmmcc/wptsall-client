use super::system::{get_decrypted_config, set_encrypted_config};
use crate::types::{
    ComponentBindingsDoc, DomainTokenBindingsDoc, RuleComponentBindingsDoc,
    TaskTypeComponentBindingsDoc,
};
use anyhow::Result;
use rusqlite::Connection;

fn with_runtime_conn<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = crate::config::db_path();
    let conn = crate::db::open_db(&db_path)?;
    let _ = crate::db::migrate_from_json_if_needed(&conn);
    f(&conn)
}

pub(crate) fn load_component_bindings_doc(conn: &Connection) -> ComponentBindingsDoc {
    get_decrypted_config(conn, "component_bindings_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_component_bindings_doc(
    conn: &Connection,
    doc: &ComponentBindingsDoc,
) -> Result<()> {
    set_encrypted_config(conn, "component_bindings_doc", &serde_json::to_string(doc)?)
}

#[allow(dead_code)]
pub(crate) fn load_runtime_component_bindings_doc() -> ComponentBindingsDoc {
    with_runtime_conn(|conn| Ok(load_component_bindings_doc(conn))).unwrap_or_default()
}

pub(crate) fn save_runtime_component_bindings_doc(doc: &ComponentBindingsDoc) -> Result<()> {
    with_runtime_conn(|conn| save_component_bindings_doc(conn, doc))
}

pub(crate) fn load_domain_token_bindings_doc(conn: &Connection) -> DomainTokenBindingsDoc {
    get_decrypted_config(conn, "domain_token_bindings_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_domain_token_bindings_doc(
    conn: &Connection,
    doc: &DomainTokenBindingsDoc,
) -> Result<()> {
    set_encrypted_config(
        conn,
        "domain_token_bindings_doc",
        &serde_json::to_string(doc)?,
    )
}

#[allow(dead_code)]
pub(crate) fn load_runtime_domain_token_bindings_doc() -> DomainTokenBindingsDoc {
    with_runtime_conn(|conn| Ok(load_domain_token_bindings_doc(conn))).unwrap_or_default()
}

pub(crate) fn save_runtime_domain_token_bindings_doc(doc: &DomainTokenBindingsDoc) -> Result<()> {
    with_runtime_conn(|conn| save_domain_token_bindings_doc(conn, doc))
}

pub(crate) fn load_task_type_component_bindings_doc(
    conn: &Connection,
) -> TaskTypeComponentBindingsDoc {
    get_decrypted_config(conn, "task_type_component_bindings_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_task_type_component_bindings_doc(
    conn: &Connection,
    doc: &TaskTypeComponentBindingsDoc,
) -> Result<()> {
    set_encrypted_config(
        conn,
        "task_type_component_bindings_doc",
        &serde_json::to_string(doc)?,
    )
}

#[allow(dead_code)]
pub(crate) fn load_runtime_task_type_component_bindings_doc() -> TaskTypeComponentBindingsDoc {
    with_runtime_conn(|conn| Ok(load_task_type_component_bindings_doc(conn))).unwrap_or_default()
}

pub(crate) fn save_runtime_task_type_component_bindings_doc(
    doc: &TaskTypeComponentBindingsDoc,
) -> Result<()> {
    with_runtime_conn(|conn| save_task_type_component_bindings_doc(conn, doc))
}

#[allow(dead_code)]
pub(crate) fn load_rule_component_bindings_doc(conn: &Connection) -> RuleComponentBindingsDoc {
    get_decrypted_config(conn, "rule_component_bindings_doc")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

pub(crate) fn save_rule_component_bindings_doc(
    conn: &Connection,
    doc: &RuleComponentBindingsDoc,
) -> Result<()> {
    set_encrypted_config(
        conn,
        "rule_component_bindings_doc",
        &serde_json::to_string(doc)?,
    )
}

#[allow(dead_code)]
pub(crate) fn load_runtime_rule_component_bindings_doc() -> RuleComponentBindingsDoc {
    with_runtime_conn(|conn| Ok(load_rule_component_bindings_doc(conn))).unwrap_or_default()
}

pub(crate) fn save_runtime_rule_component_bindings_doc(
    doc: &RuleComponentBindingsDoc,
) -> Result<()> {
    with_runtime_conn(|conn| save_rule_component_bindings_doc(conn, doc))
}

// Migration: reads JSON files and imports to db (called once on first startup)
pub(crate) fn migrate_component_bindings_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::component_bindings_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_component_bindings(&path)?;
        save_component_bindings_doc(conn, &doc)?;
    }
    Ok(())
}

pub(crate) fn migrate_domain_token_bindings_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::domain_token_bindings_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_domain_token_bindings(&path)?;
        save_domain_token_bindings_doc(conn, &doc)?;
    }
    Ok(())
}

pub(crate) fn migrate_task_type_bindings_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::task_type_component_bindings_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_task_type_component_bindings(&path)?;
        save_task_type_component_bindings_doc(conn, &doc)?;
    }
    Ok(())
}

pub(crate) fn migrate_rule_bindings_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::rule_component_bindings_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_rule_component_bindings(&path)?;
        save_rule_component_bindings_doc(conn, &doc)?;
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
    fn test_component_bindings_default_when_empty() {
        let conn = make_db();
        let doc = load_component_bindings_doc(&conn);
        assert!(doc.components.is_empty());
    }

    #[test]
    fn test_component_bindings_round_trip() {
        let _device_guard = TestEnvVarGuard::set(
            "WPTSALL_DEVICE_ID",
            "test-device-id-for-component-bindings-encryption",
        );
        let conn = make_db();
        let mut doc = ComponentBindingsDoc::default();
        let entry = crate::types::ComponentBindingEntry {
            auth: {
                let mut m = HashMap::new();
                m.insert("api_key".to_string(), "secret123".to_string());
                m
            },
            template_id: Some("tmpl-001".to_string()),
            r#type: Some("translate".to_string()),
            name: Some("My Component".to_string()),
            language_map: HashMap::new(),
            constraints_override: None,
            request_overrides: None,
            default_values_override: None,
            key_ids: vec![],
            oauth_ids: vec![],
            auth_strategy: crate::types::KeySelectionStrategy::RoundRobin,
        };
        doc.components.insert("comp-001".to_string(), entry);

        save_component_bindings_doc(&conn, &doc).unwrap();
        let loaded = load_component_bindings_doc(&conn);

        assert_eq!(loaded.components.len(), 1);
        let loaded_entry = loaded.components.get("comp-001").unwrap();
        assert_eq!(
            loaded_entry.auth.get("api_key"),
            Some(&"secret123".to_string())
        );
        assert_eq!(loaded_entry.template_id, Some("tmpl-001".to_string()));
    }

    #[test]
    fn test_domain_token_bindings_round_trip() {
        let _device_guard = TestEnvVarGuard::set(
            "WPTSALL_DEVICE_ID",
            "test-device-id-for-domain-token-bindings-encryption",
        );
        let conn = make_db();
        let mut doc = DomainTokenBindingsDoc::default();
        let entry = crate::types::DomainTokenBindingEntry {
            wp_client_token: "wptc1.abc.123.sig".to_string(),
            route_secret: "route-secret-xyz".to_string(),
        };
        doc.domains.insert("https://example.com".to_string(), entry);

        save_domain_token_bindings_doc(&conn, &doc).unwrap();
        let loaded = load_domain_token_bindings_doc(&conn);

        assert_eq!(loaded.domains.len(), 1);
        let loaded_entry = loaded.domains.get("https://example.com").unwrap();
        assert_eq!(loaded_entry.wp_client_token, "wptc1.abc.123.sig");
        assert_eq!(loaded_entry.route_secret, "route-secret-xyz");
    }

    #[test]
    fn test_task_type_component_bindings_round_trip() {
        let _device_guard = TestEnvVarGuard::set(
            "WPTSALL_DEVICE_ID",
            "test-device-id-for-task-type-encryption",
        );
        let conn = make_db();
        let mut doc = TaskTypeComponentBindingsDoc::default();
        let entry = crate::types::TaskTypeComponentBindingEntry {
            component_id: "comp-translate".to_string(),
        };
        doc.task_types.insert("translate_post".to_string(), entry);

        save_task_type_component_bindings_doc(&conn, &doc).unwrap();

        let raw =
            super::super::system::get_system_config(&conn, "task_type_component_bindings_doc")
                .unwrap();
        assert_ne!(
            raw,
            serde_json::to_string(&doc).unwrap(),
            "task_type component bindings should be encrypted when a bindings secret is available"
        );

        let loaded = load_task_type_component_bindings_doc(&conn);

        assert_eq!(loaded.task_types.len(), 1);
        let loaded_entry = loaded.task_types.get("translate_post").unwrap();
        assert_eq!(loaded_entry.component_id, "comp-translate");
    }

    #[test]
    fn test_rule_component_bindings_round_trip() {
        let _device_guard = TestEnvVarGuard::set(
            "WPTSALL_DEVICE_ID",
            "test-device-id-for-rule-bindings-encryption",
        );
        let conn = make_db();
        let mut doc = RuleComponentBindingsDoc::default();
        doc.global_defaults
            .insert("default".to_string(), "comp-default".to_string());
        let mut plugin_map = HashMap::new();
        plugin_map.insert("plugin-a".to_string(), "comp-plugin".to_string());
        doc.plugin_bindings
            .insert("post_content".to_string(), plugin_map);

        save_rule_component_bindings_doc(&conn, &doc).unwrap();

        let raw =
            super::super::system::get_system_config(&conn, "rule_component_bindings_doc").unwrap();
        assert_ne!(
            raw,
            serde_json::to_string(&doc).unwrap(),
            "rule component bindings should be encrypted when a bindings secret is available"
        );

        let loaded = load_rule_component_bindings_doc(&conn);

        assert_eq!(
            loaded.global_defaults.get("default"),
            Some(&"comp-default".to_string())
        );
        let pb = loaded.plugin_bindings.get("post_content").unwrap();
        assert_eq!(pb.get("plugin-a"), Some(&"comp-plugin".to_string()));
    }

    #[test]
    fn test_task_type_and_rule_plain_json_backward_compat() {
        let conn = make_db();
        let plain_task =
            r#"{"version":2,"task_types":{"legacy_task":{"component_id":"legacy-comp"}}}"#;
        let plain_rule = r#"{"version":2,"global_defaults":{"legacy":"legacy-comp"}}"#;

        super::super::system::set_system_config(
            &conn,
            "task_type_component_bindings_doc",
            plain_task,
        )
        .unwrap();
        super::super::system::set_system_config(&conn, "rule_component_bindings_doc", plain_rule)
            .unwrap();

        let loaded_task = load_task_type_component_bindings_doc(&conn);
        let loaded_rule = load_rule_component_bindings_doc(&conn);

        assert!(loaded_task.task_types.contains_key("legacy_task"));
        assert!(loaded_rule.global_defaults.contains_key("legacy"));
    }

    #[test]
    fn test_overwrite_updates_doc() {
        let _device_guard = TestEnvVarGuard::set(
            "WPTSALL_DEVICE_ID",
            "test-device-id-for-component-overwrite-encryption",
        );
        let conn = make_db();

        // Save first version
        let mut doc1 = ComponentBindingsDoc::default();
        let entry1 = crate::types::ComponentBindingEntry {
            auth: {
                let mut m = HashMap::new();
                m.insert("key".to_string(), "first".to_string());
                m
            },
            template_id: None,
            r#type: None,
            name: None,
            language_map: HashMap::new(),
            constraints_override: None,
            request_overrides: None,
            default_values_override: None,
            key_ids: vec![],
            oauth_ids: vec![],
            auth_strategy: crate::types::KeySelectionStrategy::RoundRobin,
        };
        doc1.components.insert("comp-A".to_string(), entry1);
        save_component_bindings_doc(&conn, &doc1).unwrap();

        // Save second version (different content)
        let mut doc2 = ComponentBindingsDoc::default();
        let entry2 = crate::types::ComponentBindingEntry {
            auth: {
                let mut m = HashMap::new();
                m.insert("key".to_string(), "second".to_string());
                m
            },
            template_id: None,
            r#type: None,
            name: None,
            language_map: HashMap::new(),
            constraints_override: None,
            request_overrides: None,
            default_values_override: None,
            key_ids: vec![],
            oauth_ids: vec![],
            auth_strategy: crate::types::KeySelectionStrategy::RoundRobin,
        };
        doc2.components.insert("comp-A".to_string(), entry2);
        save_component_bindings_doc(&conn, &doc2).unwrap();

        // Should reflect the second save
        let loaded = load_component_bindings_doc(&conn);
        let entry = loaded.components.get("comp-A").unwrap();
        assert_eq!(entry.auth.get("key"), Some(&"second".to_string()));
    }
}
