use super::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::Path;

const JOURNAL_PREFIX: &str = "integration-save-v1:";



#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    format: String,
    path: String,
    before_digest: String,
    after: Value,
    db_value: String,
}

fn read<D: DeserializeOwned + Default>(path: &Path) -> anyhow::Result<D> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(metadata.file_type().is_file(), "integration configuration is not a regular file; retained");
            let value: Value = serde_json::from_str(&crate::bindings::load_encrypted_or_plain(path)?)
                .context("damaged integration configuration; original retained")?;
            anyhow::ensure!(value.is_object(), "integration configuration must be an object; retained");
            serde_json::from_value(value).context("damaged integration configuration; original retained")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(D::default()),
        Err(error) => Err(error.into()),
    }
}

fn digest<D: Serialize>(doc: &D) -> anyhow::Result<String> {
    crate::db::system::private_json_digest(&serde_json::to_value(doc)?)
}

fn recover<D: Serialize + DeserializeOwned + Default>(
    conn: &mut rusqlite::Connection,
    path: &Path,
    key: &str,
    lease: &crate::bindings::IntegrationDocumentLease,
) -> anyhow::Result<()> {
    lease.assert_owner()?;
    let journal_key = format!("{JOURNAL_PREFIX}{key}");
    let Some(raw) = crate::db::system::get_system_config_checked(conn, &journal_key)? else {
        return Ok(());
    };
    let prepared: Prepared = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)
        .context("damaged integration save receipt; retained")?;
    anyhow::ensure!(
        prepared.format == "integration-save-v1" && path.to_str() == Some(prepared.path.as_str()),
        "integration save receipt scope changed; retained"
    );
    anyhow::ensure!(
        crate::db::system::get_system_config_checked(conn, key)?.as_deref()
            == Some(prepared.db_value.as_str()),
        "integration save database authority changed; retained"
    );
    let after: D = serde_json::from_value(prepared.after.clone())
        .context("damaged integration save snapshot; retained")?;
    let authority: D = serde_json::from_str(&crate::db::system::decrypt_config_value(
        &prepared.db_value,
    )?)
    .context("damaged integration save authority; retained")?;
    anyhow::ensure!(
        digest(&authority)? == digest(&after)?
            && prepared.before_digest.len() == 64
            && prepared
                .before_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "integration save snapshot does not match authority; retained"
    );
    let current: D = read(path)?;
    let current_digest = digest(&current)?;
    let after_digest = digest(&after)?;
    anyhow::ensure!(
        current_digest == prepared.before_digest || current_digest == after_digest,
        "integration save file authority changed; newer data retained"
    );
    if current_digest != after_digest {
        lease.assert_owner()?;
        crate::storage_capacity::with_database_credit(None, || {
            crate::bindings::save_encrypted_file(path, &serde_json::to_string_pretty(&after)?)
        })?;
    }
    anyhow::ensure!(
        digest(&read::<D>(path)?)? == after_digest,
        "integration save file projection was not confirmed; receipt retained"
    );
    ;
    let projected_bytes = std::fs::read(path)?;
    lease.assert_owner()?;
    anyhow::ensure!(
        crate::db::system::get_system_config_checked(conn, key)?.as_deref()
            == Some(prepared.db_value.as_str())
            && crate::db::system::get_system_config_checked(conn, &journal_key)?.as_deref()
                == Some(raw.as_str()),
        "integration save database or journal authority changed; retained"
    );
    let credit = match conn.path().filter(|path| {
        !path.is_empty() && raw.starts_with("V1BUQw") && prepared.db_value.starts_with("V1BUQw")
    }) {
        Some(path) => crate::storage_capacity::database_recovery_credit(Path::new(path), false)?,
        None => None,
    };
    crate::storage_capacity::with_database_credit(credit, || -> anyhow::Result<()> {
        let tx = conn.savepoint()?;
        lease.assert_owner()?;
        anyhow::ensure!(
            crate::db::system::get_system_config_checked(&tx, key)?.as_deref()
                == Some(prepared.db_value.as_str())
                && crate::db::system::get_system_config_checked(&tx, &journal_key)?.as_deref()
                    == Some(raw.as_str())
                && std::fs::read(path)? == projected_bytes,
            "integration save authority or projected file changed; retained"
        );
        let removed = tx.execute(
            "DELETE FROM system_config WHERE key=?1 AND value=?2",
            rusqlite::params![journal_key, raw],
        )?;
        anyhow::ensure!(
            removed == 1
                && crate::db::system::get_system_config_checked(&tx, &journal_key)?.is_none(),
            "integration save receipt cleanup was not confirmed; retained"
        );
        tx.commit()?;
        lease.assert_owner()?;
        Ok(())
    })
}

pub(in crate::web_ui::routes) async fn load_config<D: Serialize + DeserializeOwned + Default>(
    state: &Arc<Mutex<WebUiState>>,
    path: &str,
    key: &str,
) -> anyhow::Result<D> {
    let db = state.lock().await.db.clone();
    let lease = crate::bindings::IntegrationDocumentLease::acquire(path)?;
    let mut conn = db.lock().await;
    recover::<D>(&mut conn, lease.path(), key, &lease)?;
    let doc: D = read(lease.path())?;
    if let Some(raw) = crate::db::system::get_system_config_checked(&conn, key)? {
        let stored: D = serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)
            .context("damaged integration database configuration; retained")?;
        anyhow::ensure!(
            digest(&stored)? == digest(&doc)?,
            "integration file/database authority differs; retained"
        );
    }
    lease.assert_owner()?;
    Ok(doc)
}

pub(in crate::web_ui::routes) async fn load_oauth_for_tokens(
    state: &Arc<Mutex<WebUiState>>,
    path: &str,
) -> anyhow::Result<VendorOAuthDoc> {
    let db = state.lock().await.db.clone();
    let lease = crate::bindings::IntegrationDocumentLease::acquire(path)?;
    let mut conn = db.lock().await;
    recover::<VendorOAuthDoc>(&mut conn, lease.path(), "vendor_oauth_doc", &lease)?;
    if let Some(raw) = crate::db::system::get_system_config_checked(&conn, "vendor_oauth_doc")? {
        let _: VendorOAuthDoc =
            serde_json::from_str(&crate::db::system::decrypt_config_value(&raw)?)
                .context("damaged OAuth operator database configuration; retained")?;
    }
    // A saved Ready token may have only one completed projection. The token
    // manager verifies credentials and repairs that receipt, not this loader.
    let doc = read(lease.path())?;
    lease.assert_owner()?;
    Ok(doc)
}

pub(in crate::web_ui::routes) async fn save_config<D: Serialize + DeserializeOwned + Default>(
    state: &Arc<Mutex<WebUiState>>,
    path: &str,
    key: &str,
    before: &D,
    after: &D,
) -> anyhow::Result<()> {
    let db = state.lock().await.db.clone();
    let lease = crate::bindings::IntegrationDocumentLease::acquire(path)?;
    let mut conn = db.lock().await;
    recover::<D>(&mut conn, lease.path(), key, &lease)?;
    let current: D = read(lease.path())?;
    let before_digest = digest(before)?;
    anyhow::ensure!(
        digest(&current)? == before_digest,
        "integration configuration changed during save; newer data retained"
    );
    crate::storage_capacity::with_database_credit(None, || -> anyhow::Result<()> {
        let tx = conn.savepoint()?;
        lease.assert_owner()?;
        let prior = crate::db::system::get_system_config_checked(&tx, key)?;
        if let Some(raw) = &prior {
            let stored: D = serde_json::from_str(&crate::db::system::decrypt_config_value(raw)?)
                .context("damaged integration database configuration; retained")?;
            anyhow::ensure!(
                digest(&stored)? == before_digest,
                "integration file/database authority differs; reconcile before saving"
            );
        }
        let encoded = crate::db::system::encrypt_config_value(&serde_json::to_string(after)?)?;
        let changed = match &prior {
            Some(raw) => tx.execute(
                "UPDATE system_config SET value=?1 WHERE key=?2 AND value=?3",
                rusqlite::params![encoded, key, raw],
            )?,
            None => tx.execute(
                "INSERT INTO system_config(key,value) VALUES (?1,?2)",
                rusqlite::params![key, encoded],
            )?,
        };
        anyhow::ensure!(
            changed == 1
                && crate::db::system::get_system_config_checked(&tx, key)?.as_deref()
                    == Some(encoded.as_str()),
            "integration database save was not confirmed; original file retained"
        );
        let prepared = Prepared {
            format: "integration-save-v1".into(),
            path: lease
                .path()
                .to_str()
                .context("integration path is not UTF-8")?
                .into(),
            before_digest,
            after: serde_json::to_value(after)?,
            db_value: encoded,
        };
        let journal_key = format!("{JOURNAL_PREFIX}{key}");
        let journal = crate::db::system::encrypt_config_value(&serde_json::to_string(&prepared)?)?;
        let inserted = tx.execute(
            "INSERT INTO system_config(key,value) VALUES (?1,?2)",
            rusqlite::params![journal_key, journal],
        )?;
        anyhow::ensure!(
            inserted == 1
                && crate::db::system::get_system_config_checked(&tx, &journal_key)?.as_deref()
                    == Some(journal.as_str()),
            "integration save receipt was not confirmed; original file retained"
        );
        lease.assert_owner()?;
        tx.commit()?;
        Ok(())
    })?;
    ;
    // This committed snapshot is the sole authority across the SQLite/file
    // boundary. Restart replays it, never a new token or an operator's newer file.
    recover::<D>(&mut conn, lease.path(), key, &lease)?;
    Ok(())
}
