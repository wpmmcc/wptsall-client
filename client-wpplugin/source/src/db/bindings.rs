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
