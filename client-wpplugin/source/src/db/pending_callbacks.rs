//! DB-backed pending callback store for Web UI mode.
//!
//! In Web UI mode (db.is_some()), translation results awaiting delivery to WP
//! are persisted in the `pending_callbacks` SQLite table rather than in the
//! JSON file (`runtime/pending-callbacks.json`).
//!
//! CLI mode continues to use `PendingCallbackStore` from `persistence.rs`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::logging::unix_ts;
use crate::types::TranslationCallbackPayload;

// ---------------------------------------------------------------------------
// Entry struct
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct PendingCallbackEntry {
    pub api_base_url: String,
    pub idempotency_key: String,
    pub payload: TranslationCallbackPayload,
    pub route_secret: Option<String>,
    pub created_at: u64,
    pub retry_count: u32,
    pub last_retry_at: u64,
    /// Redundant column — mirrors payload.relation_id for indexed lookup.
    pub relation_id: i64,
    /// Redundant column — mirrors payload.object_id for indexed lookup.
    pub object_id: i64,
    /// Redundant column — mirrors payload.object_type for indexed lookup.
    pub object_type: String,
}

// ---------------------------------------------------------------------------
// Write operations
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct SavedEndpoint {
    api_base_url: String,
    route_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    callback_scope: Option<CallbackScope>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct CallbackScope {
    idempotency_key: String,
    relation_id: i64,
    object_id: i64,
    object_type: String,
    payload_digest: String,
}

/// Nest beneath a caller's transaction without committing its other work.
struct CallbackMutation<'a> {
    conn: &'a Connection,
    committed: bool,
}

impl<'a> CallbackMutation<'a> {
    fn new(conn: &'a Connection) -> Result<Self> {
        conn.execute_batch("SAVEPOINT pending_callback_mutation")?;
        Ok(Self {
            conn,
            committed: false,
        })
    }

    fn commit(mut self) -> Result<()> {
        self.conn
            .execute_batch("RELEASE pending_callback_mutation")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for CallbackMutation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch(
                "ROLLBACK TO pending_callback_mutation; RELEASE pending_callback_mutation",
            );
        }
    }
}

/// Save a new result, or acknowledge an identical retry without replacing it.
pub(crate) fn add_pending_callback(conn: &Connection, entry: &PendingCallbackEntry) -> Result<()> {
    let scope = callback_scope(entry)?;
    let mutation = CallbackMutation::new(conn)?;
    let existing_id = conn
        .query_row(
            "SELECT api_base_url, idempotency_key, payload_json, route_secret_enc,
                created_at, retry_count, last_retry_at, relation_id, object_id, object_type
         FROM pending_callbacks WHERE idempotency_key = ?1",
            [&entry.idempotency_key],
            row_to_entry,
        )
        .optional()?;
    let existing_unit = find_pending_callback(
        conn,
        &entry.api_base_url,
        entry.relation_id,
        &entry.object_type,
        entry.object_id,
    )?;
    if let Some(existing) = existing_id {
        anyhow::ensure!(
            existing.api_base_url == entry.api_base_url
                && existing.route_secret == entry.route_secret
                && callback_scope(&existing)? == scope
                && existing_unit.as_ref().map(|e| &e.idempotency_key)
                    == Some(&entry.idempotency_key),
            "conflicting pending callback identity or saved result; original row retained"
        );
        return mutation.commit();
    }
    anyhow::ensure!(
        existing_unit.is_none(),
        "conflicting pending callback for saved unit; original row retained"
    );
    let payload_json =
        super::system::encrypt_config_value(&serde_json::to_string(&entry.payload)?)?;
    let endpoint = super::system::encrypt_config_value(&serde_json::to_string(&SavedEndpoint {
        api_base_url: entry.api_base_url.clone(),
        route_secret: entry.route_secret.clone(),
        callback_scope: Some(scope),
    })?)?;
    let affected = conn.execute(
        "INSERT INTO pending_callbacks
         (api_base_url, idempotency_key, payload_json, route_secret_enc,
          created_at, retry_count, last_retry_at, relation_id, object_id, object_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            super::system::private_site_key(&entry.api_base_url),
            entry.idempotency_key,
            payload_json,
            endpoint,
            entry.created_at as i64,
            entry.retry_count as i64,
            entry.last_retry_at as i64,
            entry.relation_id,
            entry.object_id,
            normalize_pending_object_type(&entry.object_type)?,
        ],
    )?;
    anyhow::ensure!(
        affected == 1,
        "pending callback was not saved; original rows retained"
    );
    let saved = conn.query_row(
        "SELECT api_base_url, idempotency_key, payload_json, route_secret_enc,
                created_at, retry_count, last_retry_at, relation_id, object_id, object_type
         FROM pending_callbacks WHERE idempotency_key=?1",
        [&entry.idempotency_key],
        row_to_entry,
    )?;
    let physical: (String, String) = conn.query_row(
        "SELECT payload_json,route_secret_enc FROM pending_callbacks WHERE idempotency_key=?1",
        [&entry.idempotency_key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    anyhow::ensure!(
        physical == (payload_json, endpoint)
            && saved.api_base_url == entry.api_base_url
            && saved.route_secret == entry.route_secret
            && callback_scope(&saved)? == callback_scope(entry)?
            && saved.created_at == entry.created_at
            && saved.retry_count == entry.retry_count
            && saved.last_retry_at == entry.last_retry_at,
        "pending callback readback differs; original rows retained"
    );
    mutation.commit()
}

/// Remove a pending callback by
/// (api_base_url, relation_id, object_type, object_id).
/// Returns `true` if a row was deleted.
pub(crate) fn remove_pending_callback(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<bool> {
    let mutation = CallbackMutation::new(conn)?;
    let Some(entry) =
        find_pending_callback(conn, api_base_url, relation_id, object_type, object_id)?
    else {
        mutation.commit()?;
        return Ok(false);
    };
    let affected = conn.execute(
        "DELETE FROM pending_callbacks WHERE idempotency_key = ?1",
        [&entry.idempotency_key],
    )?;
    anyhow::ensure!(
        affected == 1,
        "pending callback was not removed; original rows retained"
    );
    mutation.commit()?;
    Ok(true)
}

/// Increment `retry_count` and update `last_retry_at` for a pending callback.
pub(crate) fn increment_retry_pending_callback(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<()> {
    let mutation = CallbackMutation::new(conn)?;
    let entry = find_pending_callback(conn, api_base_url, relation_id, object_type, object_id)?
        .ok_or_else(|| anyhow::anyhow!("pending callback not found for retry"))?;
    anyhow::ensure!(
        entry.retry_count < u32::MAX,
        "pending callback retry counter overflow"
    );
    let now = unix_ts() as i64;
    let affected = conn.execute(
        "UPDATE pending_callbacks
         SET retry_count = retry_count + 1, last_retry_at = ?1
         WHERE idempotency_key = ?2",
        params![now, entry.idempotency_key],
    )?;
    anyhow::ensure!(
        affected == 1,
        "pending callback retry was not saved; original rows retained"
    );
    mutation.commit()
}

// ---------------------------------------------------------------------------
// Read operations
// ---------------------------------------------------------------------------

/// Find the unique pending callback matching
/// (api_base_url, relation_id, object_type, object_id).
/// Missing is `None`; damaged credentials/payload or SQL errors are errors.
pub(crate) fn find_pending_callback(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<Option<PendingCallbackEntry>> {
    let normalized_object_type = normalize_pending_object_type(object_type)?;
    let legacy_type = match normalized_object_type {
        "post_type" => "post",
        "taxonomy" => "term",
        other => other,
    };
    let mut stmt = conn.prepare(
        "SELECT api_base_url, idempotency_key, payload_json, route_secret_enc,
                created_at, retry_count, last_retry_at, relation_id, object_id, object_type
         FROM pending_callbacks
         WHERE (api_base_url = ?1 OR api_base_url = ?5) AND relation_id = ?2
           AND object_type IN (?3, ?6) AND object_id = ?4
         LIMIT 2",
    )?;
    let mut rows = stmt.query_map(
        params![
            super::system::private_site_key(api_base_url),
            relation_id,
            normalized_object_type,
            object_id,
            api_base_url,
            legacy_type
        ],
        row_to_entry,
    )?;
    let entry = rows.next().transpose()?;
    anyhow::ensure!(
        rows.next().transpose()?.is_none(),
        "ambiguous pending callback; original rows retained"
    );
    Ok(entry)
}

pub(crate) fn continuation_authority(
    conn: &Connection,
    api_base_url: &str,
    idempotency_key: &str,
    payload: &TranslationCallbackPayload,
    route_secret: Option<&str>,
) -> Result<(String, bool)> {
    let saved = find_pending_callback(
        conn,
        api_base_url,
        i64::try_from(payload.relation_id)?,
        &payload.object_type,
        i64::try_from(payload.object_id)?,
    )?
    .ok_or_else(|| anyhow::anyhow!("original pending callback missing; receipt retained"))?;
    anyhow::ensure!(
        saved.idempotency_key == idempotency_key
            && saved.route_secret.as_deref() == route_secret
            && super::system::private_json_digest(&serde_json::to_value(&saved.payload)?)?
                == super::system::private_json_digest(&serde_json::to_value(payload)?)?,
        "original pending callback differs; receipt retained"
    );
    let physical: (String, String, Option<String>) = conn.query_row(
        "SELECT api_base_url,payload_json,route_secret_enc FROM pending_callbacks
         WHERE idempotency_key=?1",
        [idempotency_key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let encrypted = physical.0 == super::system::private_site_key(api_base_url)
        && physical.1.starts_with("V1BUQw")
        && physical
            .2
            .as_deref()
            .is_some_and(|scope| scope.starts_with("V1BUQw"));
    let fingerprint = super::system::private_json_digest(&serde_json::json!({
        "endpoint": physical.0,
        "payload": physical.1,
        "scope": physical.2,
        "identity": idempotency_key,
    }))?;
    Ok((fingerprint, encrypted))
}

/// Retry/age limits never authorize deleting an unconfirmed result. Kept for
/// callers of the old cleanup hook; explicit storage cleanup excludes this table.
#[allow(dead_code)]
pub(crate) fn cleanup_stale_pending_callbacks(
    _conn: &Connection,
    _max_retries: u32,
    _max_age_secs: u64,
) -> rusqlite::Result<usize> {
    Ok(0)
}

/// Count the number of pending callbacks for a given domain + relation.
///
/// Used by the discoverer to throttle content fetching when too many callbacks
/// are queued (indicating WP delivery backlog).
pub(crate) fn count_pending_for_relation(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM pending_callbacks WHERE (api_base_url = ?1 OR api_base_url = ?3) AND relation_id = ?2",
        params![super::system::private_site_key(api_base_url), relation_id, api_base_url],
        |row| row.get(0),
    )?)
}

/// List all pending callbacks ordered by `created_at` ASC (for startup retry).
#[allow(dead_code)]
pub(crate) fn list_all_pending_callbacks(conn: &Connection) -> Result<Vec<PendingCallbackEntry>> {
    let mut stmt = conn.prepare(
        "SELECT api_base_url, idempotency_key, payload_json, route_secret_enc,
                created_at, retry_count, last_retry_at, relation_id, object_id, object_type
         FROM pending_callbacks
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_entry)?;
    let entries = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut units = HashSet::new();
    for entry in &entries {
        anyhow::ensure!(
            units.insert((
                &entry.api_base_url,
                entry.relation_id,
                &entry.object_type,
                entry.object_id
            )),
            "ambiguous pending callback; original rows retained"
        );
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

fn damaged_row(column: usize) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "damaged pending callback; original row retained",
        )),
    )
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingCallbackEntry> {
    let payload_json: String = row.get(2)?;
    let logical = super::system::decrypt_config_value(&payload_json).map_err(|_| damaged_row(2))?;
    let payload: TranslationCallbackPayload =
        serde_json::from_str(&logical).map_err(|_| damaged_row(2))?;
    let stored_url: String = row.get(0)?;
    let stored_endpoint: Option<String> = row.get(3)?;
    let (api_base_url, route_secret, scope) = if stored_url.starts_with("wptsall-site-v1:") {
        let encrypted = stored_endpoint.ok_or_else(|| damaged_row(3))?;
        if !encrypted.starts_with("V1BUQw") {
            return Err(damaged_row(3));
        }
        let plain = super::system::decrypt_config_value(&encrypted).map_err(|_| damaged_row(3))?;
        let endpoint: SavedEndpoint = serde_json::from_str(&plain).map_err(|_| damaged_row(3))?;
        if super::system::private_site_key(&endpoint.api_base_url) != stored_url {
            return Err(damaged_row(3));
        }
        (
            endpoint.api_base_url,
            endpoint.route_secret,
            endpoint.callback_scope,
        )
    } else {
        // Read legacy rows without silently rewriting or migrating the DB.
        if stored_endpoint
            .as_deref()
            .is_some_and(|secret| secret.starts_with("V1BUQw"))
        {
            return Err(damaged_row(3));
        }
        (stored_url, stored_endpoint, None)
    };
    let entry = PendingCallbackEntry {
        api_base_url,
        idempotency_key: row.get(1)?,
        payload,
        route_secret,
        created_at: row
            .get::<_, i64>(4)?
            .try_into()
            .map_err(|_| damaged_row(4))?,
        retry_count: row
            .get::<_, i64>(5)?
            .try_into()
            .map_err(|_| damaged_row(5))?,
        last_retry_at: row
            .get::<_, i64>(6)?
            .try_into()
            .map_err(|_| damaged_row(6))?,
        relation_id: row.get(7)?,
        object_id: row.get(8)?,
        object_type: normalize_pending_object_type(&row.get::<_, String>(9)?)
            .map_err(|_| damaged_row(9))?
            .to_string(),
    };
    let actual_scope = callback_scope(&entry).map_err(|_| damaged_row(2))?;
    if scope.is_some_and(|saved| saved != actual_scope) {
        return Err(damaged_row(3));
    }
    Ok(entry)
}

pub(crate) fn normalize_pending_object_type(raw: &str) -> Result<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "term" | "taxonomy" => Ok("taxonomy"),
        "post" | "post_type" => Ok("post_type"),
        "language_pack" => Ok("language_pack"),
        "option" => Ok("option"),
        "media" => Ok("media"),
        "menu" => Ok("menu"),
        "custom_table" => Ok("custom_table"),
        _ => anyhow::bail!("unsupported pending callback object type"),
    }
}

fn callback_scope(entry: &PendingCallbackEntry) -> Result<CallbackScope> {
    let object_type = normalize_pending_object_type(&entry.object_type)?;
    anyhow::ensure!(
        !entry.api_base_url.is_empty()
            && !entry.idempotency_key.is_empty()
            && u64::try_from(entry.relation_id)? == entry.payload.relation_id
            && u64::try_from(entry.object_id)? == entry.payload.object_id
            && object_type == normalize_pending_object_type(&entry.payload.object_type)?
            && entry.created_at <= i64::MAX as u64
            && entry.last_retry_at <= i64::MAX as u64,
        "invalid pending callback scope; original rows retained"
    );
    Ok(CallbackScope {
        idempotency_key: entry.idempotency_key.clone(),
        relation_id: entry.relation_id,
        object_id: entry.object_id,
        object_type: object_type.to_string(),
        payload_digest: super::system::private_json_digest(&serde_json::to_value(&entry.payload)?)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
