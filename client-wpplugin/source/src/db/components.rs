use super::system::{get_decrypted_config_checked, set_encrypted_config};
use crate::types::ComponentsLocalDoc;
use anyhow::Result;
use rusqlite::Connection;

fn with_runtime_conn<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db_path = crate::config::db_path();
    let conn = crate::db::open_db(&db_path)?;
    crate::db::migrate_from_json_if_needed(&conn)?;
    f(&conn)
}

#[allow(dead_code)]
pub(crate) fn load_local_components_doc(conn: &Connection) -> Result<ComponentsLocalDoc> {
    match get_decrypted_config_checked(conn, "local_components_doc")? {
        Some(json) => serde_json::from_str(&json)
            .map_err(|_| anyhow::anyhow!("stored local components configuration is damaged")),
        None => Ok(ComponentsLocalDoc::default()),
    }
}

pub(crate) fn save_local_components_doc(conn: &Connection, doc: &ComponentsLocalDoc) -> Result<()> {
    let transaction = if conn.is_autocommit() {
        Some(conn.unchecked_transaction()?)
    } else {
        None
    };
    load_local_components_doc(conn)?;
    set_encrypted_config(conn, "local_components_doc", &serde_json::to_string(doc)?)?;
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(())
}

pub(crate) fn load_runtime_local_components_doc() -> Result<ComponentsLocalDoc> {
    with_runtime_conn(load_local_components_doc).map_err(|_| {
        crate::component_rt::loader::RuntimeConfigurationFault::error("local components database")
    })
}

pub(crate) fn save_runtime_local_components_doc(doc: &ComponentsLocalDoc) -> Result<()> {
    with_runtime_conn(|conn| save_local_components_doc(conn, doc))
}

pub(crate) fn migrate_local_components_from_json(conn: &Connection) -> Result<()> {
    let path = crate::config::components_local_file();
    if std::path::Path::new(&path).exists() {
        let doc = crate::bindings::load_components_local(&path)?;
        save_local_components_doc(conn, &doc)?;
    }
    Ok(())
}
