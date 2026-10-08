use super::system::{get_decrypted_config_checked, set_encrypted_config};
use crate::types::{
    ComponentBindingsDoc, DomainTokenBindingsDoc, RuleComponentBindingsDoc,
    TaskTypeComponentBindingsDoc,
};
use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::{de::DeserializeOwned, Serialize};

fn with_runtime_conn<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = crate::config::db_path();
    let conn = crate::db::open_db(&db_path)?;
    crate::db::migrate_from_json_if_needed(&conn)?;
    f(&conn)
}

fn load_doc<T: DeserializeOwned + Default>(conn: &Connection, key: &str) -> Result<T> {
    let Some(raw) = get_decrypted_config_checked(conn, key)? else {
        return Ok(T::default());
    };
    parse_doc(key, &raw)
}

fn parse_doc<T: DeserializeOwned>(key: &str, raw: &str) -> Result<T> {
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| anyhow!("invalid binding document {key}"))?;
    if !value.is_object() {
        return Err(anyhow!("binding document {key} must be an object"));
    }
    let fields: &[&str] = match key {
        "component_bindings_doc" => &["version", "components", "$comment"],
        "domain_token_bindings_doc" => &["version", "domains", "$comment"],
        "task_type_component_bindings_doc" => &[
            "version",
            "task_types",
            "business_line_task_types",
            "$comment",
        ],
        "rule_component_bindings_doc" => &[
            "version",
            "global_defaults",
            "relation_bindings",
            "plugin_bindings",
            "rule_bindings",
            "site_bindings",
            "migration_issues",
            "$comment",
        ],
        _ => return Err(anyhow!("unknown binding document {key}")),
    };
    let map = value.as_object().expect("object checked above");
    if map.keys().any(|name| !fields.contains(&name.as_str())) {
        return Err(anyhow!("unknown root field in binding document {key}"));
    }
    serde_json::from_str(raw).map_err(|_| anyhow!("invalid binding document {key}"))
}

fn save_doc<T: DeserializeOwned + Serialize + Default>(
    conn: &Connection,
    key: &str,
    doc: &T,
) -> Result<()> {
    // The read guard and write share a transaction. Migration/import callers
    // may already own one; otherwise protect this whole read-modify-write.
    let tx = if conn.is_autocommit() {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    let _: T = load_doc(conn, key)?;
    set_encrypted_config(conn, key, &serde_json::to_string(doc)?)?;
    if let Some(tx) = tx {
        tx.commit()?;
    }
    Ok(())
}

pub(crate) fn load_component_bindings_doc(conn: &Connection) -> Result<ComponentBindingsDoc> {
    load_doc(conn, "component_bindings_doc")
}

pub(crate) fn save_component_bindings_doc(
    conn: &Connection,
    doc: &ComponentBindingsDoc,
) -> Result<()> {
    save_doc(conn, "component_bindings_doc", doc)
}

#[allow(dead_code)]
pub(crate) fn load_runtime_component_bindings_doc() -> Result<ComponentBindingsDoc> {
    with_runtime_conn(load_component_bindings_doc)
}

pub(crate) fn save_runtime_component_bindings_doc(doc: &ComponentBindingsDoc) -> Result<()> {
    with_runtime_conn(|conn| save_component_bindings_doc(conn, doc))
}

pub(crate) fn load_domain_token_bindings_doc(conn: &Connection) -> Result<DomainTokenBindingsDoc> {
    let mut doc: DomainTokenBindingsDoc = load_doc(conn, "domain_token_bindings_doc")?;
    // Identity Contract v1.1 §4 (C-1): apply the same v2→v3 identity backfill
    // as the file loader so DB-sourced legacy docs carry the migration
    // default (wpmmcc_ats + pending re-verification) instead of a null
    // identity. Idempotent for already-v3 docs.
    crate::bindings::apply_v3_identity_backfill(&mut doc);
    Ok(doc)
}

pub(crate) fn save_domain_token_bindings_doc(
    conn: &Connection,
    doc: &DomainTokenBindingsDoc,
) -> Result<()> {
    save_doc(conn, "domain_token_bindings_doc", doc)
}

#[allow(dead_code)]
pub(crate) fn load_runtime_domain_token_bindings_doc() -> Result<DomainTokenBindingsDoc> {
    with_runtime_conn(load_domain_token_bindings_doc)
}

pub(crate) fn save_runtime_domain_token_bindings_doc(doc: &DomainTokenBindingsDoc) -> Result<()> {
    with_runtime_conn(|conn| save_domain_token_bindings_doc(conn, doc))
}

pub(crate) fn persist_refreshed_route_secret(
    conn: &Connection,
    domain: &str,
    api_base_url: &str,
    token: &str,
    secret: &str,
) -> Result<DomainTokenBindingsDoc> {
    let tx = conn.unchecked_transaction()?;
    let mut doc = load_domain_token_bindings_doc(&tx)?;
    let legacy_key = crate::bindings::normalize_api_base_url_key(api_base_url);
    if let Some(entry) = doc.domains.get_mut(domain) {
        entry.route_secret = secret.to_string();
    } else if let Some(entry) = doc.domains.get_mut(&legacy_key) {
        entry.route_secret = secret.to_string();
    } else {
        doc.domains.insert(
            domain.to_string(),
            crate::types::DomainTokenBindingEntry {
                wp_client_token: token.to_string(),
                route_secret: secret.to_string(),
                ..Default::default()
            },
        );
    }
    save_domain_token_bindings_doc(&tx, &doc)?;
    tx.commit()?;
    Ok(doc)
}

pub(crate) fn load_task_type_component_bindings_doc(
    conn: &Connection,
) -> Result<TaskTypeComponentBindingsDoc> {
    load_doc(conn, "task_type_component_bindings_doc")
}

pub(crate) fn save_task_type_component_bindings_doc(
    conn: &Connection,
    doc: &TaskTypeComponentBindingsDoc,
) -> Result<()> {
    save_doc(conn, "task_type_component_bindings_doc", doc)
}

#[allow(dead_code)]
pub(crate) fn load_runtime_task_type_component_bindings_doc() -> Result<TaskTypeComponentBindingsDoc>
{
    with_runtime_conn(load_task_type_component_bindings_doc)
}

pub(crate) fn save_runtime_task_type_component_bindings_doc(
    doc: &TaskTypeComponentBindingsDoc,
) -> Result<()> {
    with_runtime_conn(|conn| save_task_type_component_bindings_doc(conn, doc))
}

#[allow(dead_code)]
pub(crate) fn load_rule_component_bindings_doc(
    conn: &Connection,
) -> Result<RuleComponentBindingsDoc> {
    load_doc(conn, "rule_component_bindings_doc")
}

pub(crate) fn save_rule_component_bindings_doc(
    conn: &Connection,
    doc: &RuleComponentBindingsDoc,
) -> Result<()> {
    save_doc(conn, "rule_component_bindings_doc", doc)
}

#[allow(dead_code)]
pub(crate) fn load_runtime_rule_component_bindings_doc() -> Result<RuleComponentBindingsDoc> {
    with_runtime_conn(load_rule_component_bindings_doc)
}

pub(crate) fn save_runtime_rule_component_bindings_doc(
    doc: &RuleComponentBindingsDoc,
) -> Result<()> {
    with_runtime_conn(|conn| save_rule_component_bindings_doc(conn, doc))
}

// Migration: reads JSON files and imports to db (called once on first startup)
fn migrate_doc<T: DeserializeOwned + Serialize + Default>(
    conn: &Connection,
    key: &str,
    path: &str,
    loader: fn(&str) -> Result<T>,
) -> Result<()> {
    if super::system::get_system_config_checked(conn, key)?.is_some() {
        let _: T = load_doc(conn, key)?;
        return Ok(());
    }
    let file = std::path::Path::new(path);
    if file.exists() {
        let raw = crate::bindings::load_encrypted_or_plain(file)?;
        let _: T = parse_doc(key, &raw)?;
        save_doc(conn, key, &loader(path)?)?;
    }
    Ok(())
}

pub(crate) fn migrate_component_bindings_from_json(conn: &Connection) -> Result<()> {
    migrate_doc(
        conn,
        "component_bindings_doc",
        &crate::config::component_bindings_file(),
        crate::bindings::load_component_bindings,
    )
}

pub(crate) fn migrate_domain_token_bindings_from_json(conn: &Connection) -> Result<()> {
    migrate_doc(
        conn,
        "domain_token_bindings_doc",
        &crate::config::domain_token_bindings_file(),
        crate::bindings::load_domain_token_bindings,
    )
}

pub(crate) fn migrate_task_type_bindings_from_json(conn: &Connection) -> Result<()> {
    migrate_doc(
        conn,
        "task_type_component_bindings_doc",
        &crate::config::task_type_component_bindings_file(),
        crate::bindings::load_task_type_component_bindings,
    )
}

pub(crate) fn migrate_rule_bindings_from_json(conn: &Connection) -> Result<()> {
    migrate_doc(
        conn,
        "rule_component_bindings_doc",
        &crate::config::rule_component_bindings_file(),
        crate::bindings::load_rule_component_bindings,
    )
}
#[cfg(test)]
mod tests {
    use super::super::TestEnvVarGuard;
    use super::*;
    use std::collections::HashMap;

    fn make_db() -> rusqlite::Connection {
        crate::db::open_db(":memory:").expect("in-memory db")
    }

    const BINDING_KEYS: [&str; 4] = [
        "component_bindings_doc",
        "domain_token_bindings_doc",
        "task_type_component_bindings_doc",
        "rule_component_bindings_doc",
    ];

    fn loaded_json(conn: &Connection, key: &str) -> Result<serde_json::Value> {
        match key {
            "component_bindings_doc" => {
                Ok(serde_json::to_value(load_component_bindings_doc(conn)?)?)
            }
            "domain_token_bindings_doc" => {
                Ok(serde_json::to_value(load_domain_token_bindings_doc(conn)?)?)
            }
            "task_type_component_bindings_doc" => Ok(serde_json::to_value(
                load_task_type_component_bindings_doc(conn)?,
            )?),
            _ => Ok(serde_json::to_value(load_rule_component_bindings_doc(
                conn,
            )?)?),
        }
    }

    fn save_defaults(conn: &Connection, key: &str) -> Result<()> {
        match key {
            "component_bindings_doc" => {
                save_component_bindings_doc(conn, &ComponentBindingsDoc::default())
            }
            "domain_token_bindings_doc" => {
                save_domain_token_bindings_doc(conn, &DomainTokenBindingsDoc::default())
            }
            "task_type_component_bindings_doc" => save_task_type_component_bindings_doc(
                conn,
                &TaskTypeComponentBindingsDoc::default(),
            ),
            _ => save_rule_component_bindings_doc(conn, &RuleComponentBindingsDoc::default()),
        }
    }

    fn sample_json(key: &str) -> String {
        match key {
            "component_bindings_doc" => r#"{"version":1,"components":{"original":{"auth":{"api_key":"cli03-private-sentinel"}}}}"#,
            "domain_token_bindings_doc" => r#"{"version":1,"domains":{"https://cli03.invalid":{"wp_client_token":"cli03-private-sentinel","route_secret":"initial"}}}"#,
            "task_type_component_bindings_doc" => r#"{"version":1,"task_types":{"text":{"component_id":"original"}},"business_line_task_types":{"post_content.text":{"component_id":"scoped"}}}"#,
            _ => r#"{"version":1,"global_defaults":{"plain_text":"original"},"rule_bindings":{"7":{"plain_text":"scoped"}}}"#,
        }.to_string()
    }

    // catalog: WEBUI-MOD-db-bindings-rs
    // oracle: L2
    #[test]
    fn cli03_corrupt_binding_rows_cannot_be_overwritten_with_defaults() {
        let _env = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
        for key in [
            "component_bindings_doc",
            "domain_token_bindings_doc",
            "task_type_component_bindings_doc",
            "rule_component_bindings_doc",
        ] {
            for raw in [
                "{",
                "",
                " ",
                "null",
                "[2,{}]",
                r#"{"unrecognized":"cli03-private-sentinel"}"#,
                "V1BUQw==",
            ] {
                let conn = make_db();
                super::super::system::set_system_config(&conn, key, raw).unwrap();
                let error =
                    loaded_json(&conn, key).expect_err("present corrupt rows must not default");
                assert!(!error.to_string().contains("cli03-private-sentinel"));
                let result = save_defaults(&conn, key);
                let preserved =
                    super::super::system::get_system_config(&conn, key).as_deref() == Some(raw);
                assert!(
                    result.is_err() && preserved,
                    "{key} corruption must block saves: failed={}, preserved={preserved}",
                    result.is_err()
                );
                assert_eq!(
                    super::super::system::get_system_config(&conn, key).as_deref(),
                    Some(raw),
                    "the original corrupt row must be kept byte-exact"
                );
            }
        }
    }

    #[test]
    fn cli03_wrong_encryption_key_must_not_reset_bindings() {
        let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-key-a");
        for key in BINDING_KEYS {
            let conn = make_db();
            super::super::system::set_encrypted_config(&conn, key, &sample_json(key)).unwrap();
            let before = super::super::system::get_system_config(&conn, key).unwrap();
            let original = loaded_json(&conn, key).unwrap();
            {
                let _wrong_secret =
                    TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-key-b");
                assert!(super::super::system::get_decrypted_config_checked(&conn, key).is_err());
                assert!(loaded_json(&conn, key).is_err());
                let result = save_defaults(&conn, key);
                let preserved = super::super::system::get_system_config(&conn, key).as_deref()
                    == Some(before.as_str());
                assert!(
                    result.is_err() && preserved,
                    "{key} decryption failure must block saves: failed={}, preserved={preserved}",
                    result.is_err()
                );
            }
            assert_eq!(
                loaded_json(&conn, key).unwrap(),
                original,
                "restoring the key restores the original data"
            );
        }
    }

    #[test]
    fn cli03_missing_rows_are_defaults_but_missing_tables_and_blobs_are_errors() {
        let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
        for key in BINDING_KEYS {
            let conn = make_db();
            assert!(loaded_json(&conn, key).is_ok());
            save_defaults(&conn, key).unwrap();
            assert!(loaded_json(&conn, key).is_ok());
            conn.execute(
                "UPDATE system_config SET value = X'FF' WHERE key = ?1",
                [key],
            )
            .unwrap();
            assert!(loaded_json(&conn, key).is_err());
            assert!(save_defaults(&conn, key).is_err());
            let blob: Vec<u8> = conn
                .query_row(
                    "SELECT value FROM system_config WHERE key = ?1",
                    [key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(blob, [255]);

            let conn = Connection::open_in_memory().unwrap();
            assert!(
                loaded_json(&conn, key).is_err(),
                "SQL failure is not a fresh install"
            );
            assert!(save_defaults(&conn, key).is_err());
        }
    }

    #[test]
    fn cli03_runtime_binding_loads_propagate_corrupt_rows_and_database_failures() {
        let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("runtime.db");
        let _path = TestEnvVarGuard::set("WPTSALL_DB_PATH", db_path.to_str().unwrap());
        let conn = crate::db::open_db(db_path.to_str().unwrap()).unwrap();
        super::super::system::set_system_config(&conn, "json_migration_done", "1").unwrap();
        for key in BINDING_KEYS {
            super::super::system::set_system_config(&conn, key, "{").unwrap();
        }
        assert!(load_runtime_component_bindings_doc().is_err());
        assert!(load_runtime_domain_token_bindings_doc().is_err());
        assert!(load_runtime_task_type_component_bindings_doc().is_err());
        assert!(load_runtime_rule_component_bindings_doc().is_err());
        for key in BINDING_KEYS {
            assert_eq!(
                super::super::system::get_system_config(&conn, key).as_deref(),
                Some("{")
            );
        }
        drop(conn);
        let invalid_db = dir.path().join("invalid.db");
        std::fs::write(&invalid_db, b"cli03-not-a-database").unwrap();
        let _invalid_path = TestEnvVarGuard::set("WPTSALL_DB_PATH", invalid_db.to_str().unwrap());
        assert!(load_runtime_component_bindings_doc().is_err());
        assert!(load_runtime_domain_token_bindings_doc().is_err());
        assert!(load_runtime_task_type_component_bindings_doc().is_err());
        assert!(load_runtime_rule_component_bindings_doc().is_err());
        assert_eq!(std::fs::read(&invalid_db).unwrap(), b"cli03-not-a-database");
    }

    #[test]
    fn cli03_plain_and_legacy_encrypted_documents_remain_readable() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
        for key in BINDING_KEYS {
            let mut annotated: serde_json::Value = serde_json::from_str(&sample_json(key)).unwrap();
            annotated["$comment"] = serde_json::json!("supported fixture metadata");
            let raw = annotated.to_string();
            let encrypted = crate::bindings::encrypt_for_save(&raw).unwrap();
            let legacy = serde_json::json!({
                "format": "encrypted",
                "algorithm": crate::config::BINDINGS_CRYPTO_ALGO,
                "nonce": STANDARD.encode(&encrypted[5..17]),
                "payload": STANDARD.encode(&encrypted[17..]),
            })
            .to_string();
            let reference_conn = make_db();
            super::super::system::set_system_config(&reference_conn, key, &raw).unwrap();
            let expected = loaded_json(&reference_conn, key).unwrap();
            for stored in [
                raw.clone(),
                STANDARD.encode(raw.as_bytes()),
                STANDARD.encode(&encrypted),
                legacy.clone(),
                STANDARD.encode(legacy.as_bytes()),
            ] {
                let conn = make_db();
                super::super::system::set_system_config(&conn, key, &stored).unwrap();
                assert_eq!(loaded_json(&conn, key).unwrap(), expected);
                assert_eq!(
                    super::super::system::get_system_config(&conn, key),
                    Some(stored)
                );
                save_defaults(&conn, key).unwrap();
                assert!(loaded_json(&conn, key).is_ok());
            }
        }
    }

    #[test]
    fn cli03_route_secret_recovery_preserves_corruption_and_rolls_back_write_failure() {
        let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
        let conn = make_db();
        let key = "domain_token_bindings_doc";
        super::super::system::set_system_config(&conn, key, "{").unwrap();
        assert!(persist_refreshed_route_secret(
            &conn,
            "https://cli03.invalid",
            "https://cli03.invalid/wp-json/wptsall/v2",
            "new-token",
            "new-secret"
        )
        .is_err());
        assert_eq!(
            super::super::system::get_system_config(&conn, key).as_deref(),
            Some("{")
        );
        super::super::system::set_encrypted_config(&conn, key, &sample_json(key)).unwrap();
        let before = super::super::system::get_system_config(&conn, key).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_binding_update BEFORE INSERT ON system_config
            WHEN NEW.key = 'domain_token_bindings_doc'
            BEGIN SELECT RAISE(ABORT, 'cli03 fixture'); END;",
        )
        .unwrap();
        assert!(persist_refreshed_route_secret(
            &conn,
            "https://cli03.invalid",
            "https://cli03.invalid/wp-json/wptsall/v2",
            "new-token",
            "new-secret"
        )
        .is_err());
        assert_eq!(
            super::super::system::get_system_config(&conn, key).as_deref(),
            Some(before.as_str())
        );
        conn.execute_batch("DROP TRIGGER refuse_binding_update")
            .unwrap();
        let saved = persist_refreshed_route_secret(
            &conn,
            "https://cli03.invalid",
            "https://cli03.invalid/wp-json/wptsall/v2",
            "unused-new-token",
            "new-secret",
        )
        .unwrap();
        assert_eq!(
            saved.domains["https://cli03.invalid"].route_secret,
            "new-secret"
        );
        assert_eq!(
            saved.domains["https://cli03.invalid"].wp_client_token,
            "cli03-private-sentinel"
        );
    }

    #[test]
    fn cli03_binding_save_detects_a_concurrent_change_between_guard_and_write() {
        #[derive(Default, serde::Deserialize)]
        struct RacingDoc {
            #[serde(skip)]
            database: Option<String>,
        }
        impl serde::Serialize for RacingDoc {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                if let Some(path) = &self.database {
                    let writer = crate::db::open_db(path).map_err(serde::ser::Error::custom)?;
                    super::super::system::set_system_config(&writer, "component_bindings_doc", "{")
                        .map_err(serde::ser::Error::custom)?;
                }
                serde_json::json!({}).serialize(serializer)
            }
        }
        let _secret = TestEnvVarGuard::set("WPTSALL_COMPONENT_BINDINGS_SECRET", "cli03-test-key");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");
        let conn = crate::db::open_db(path.to_str().unwrap()).unwrap();
        super::super::system::set_system_config(&conn, "component_bindings_doc", "{}").unwrap();
        // Serialization is a deterministic seam immediately after the guard
        // read. The second WAL connection commits corruption before our write.
        let result = save_doc(
            &conn,
            "component_bindings_doc",
            &RacingDoc {
                database: Some(path.to_str().unwrap().into()),
            },
        );
        assert!(
            result.is_err(),
            "a stale checked snapshot must not overwrite the competing writer"
        );
        assert_eq!(
            super::super::system::get_system_config(&conn, "component_bindings_doc").as_deref(),
            Some("{")
        );
        assert!(
            conn.is_autocommit(),
            "failed save must release its transaction"
        );
    }

    #[test]
    fn test_component_bindings_default_when_empty() {
        let conn = make_db();
        let doc = load_component_bindings_doc(&conn).unwrap();
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
        let loaded = load_component_bindings_doc(&conn).unwrap();

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
            // Synthetic test token (repo convention: wptc1.* fakes, e.g.
            // auth.rs "wptc1.test.123.sig"); must match the round-trip
            // assertion below — a masked placeholder here would test a
            // transform the storage layer does not perform.
            wp_client_token: "wptc1.abc.123.sig".to_string(),
            route_secret: "route-secret-xyz".to_string(),
            ..Default::default()
        };
        doc.domains.insert("https://example.com".to_string(), entry);

        save_domain_token_bindings_doc(&conn, &doc).unwrap();
        let loaded = load_domain_token_bindings_doc(&conn).unwrap();

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

        let loaded = load_task_type_component_bindings_doc(&conn).unwrap();

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

        let loaded = load_rule_component_bindings_doc(&conn).unwrap();

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

        let loaded_task = load_task_type_component_bindings_doc(&conn).unwrap();
        let loaded_rule = load_rule_component_bindings_doc(&conn).unwrap();

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
        let loaded = load_component_bindings_doc(&conn).unwrap();
        let entry = loaded.components.get("comp-A").unwrap();
        assert_eq!(entry.auth.get("key"), Some(&"second".to_string()));
    }
}