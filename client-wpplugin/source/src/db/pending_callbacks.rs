//! DB-backed pending callback store for Web UI mode.
//!
//! In Web UI mode (db.is_some()), translation results awaiting delivery to WP
//! are persisted in the `pending_callbacks` SQLite table rather than in the
//! JSON file (`runtime/pending-callbacks.json`).
//!
//! CLI mode continues to use `PendingCallbackStore` from `persistence.rs`.

use anyhow::Result;
use rusqlite::{params, Connection};

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

/// Insert or replace a pending callback entry (keyed on `idempotency_key`).
pub(crate) fn add_pending_callback(conn: &Connection, entry: &PendingCallbackEntry) -> Result<()> {
    let payload_json = serde_json::to_string(&entry.payload)?;
    let normalized_object_type = normalize_pending_object_type(&entry.object_type);
    conn.execute(
        "INSERT OR REPLACE INTO pending_callbacks
         (api_base_url, idempotency_key, payload_json, route_secret_enc,
          created_at, retry_count, last_retry_at, relation_id, object_id, object_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            entry.api_base_url,
            entry.idempotency_key,
            payload_json,
            entry.route_secret,
            entry.created_at as i64,
            entry.retry_count as i64,
            entry.last_retry_at as i64,
            entry.relation_id,
            entry.object_id,
            normalized_object_type,
        ],
    )?;
    Ok(())
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
    let normalized_object_type = normalize_pending_object_type(object_type);
    let affected = conn.execute(
        "DELETE FROM pending_callbacks
         WHERE api_base_url = ?1 AND relation_id = ?2 AND object_type = ?3 AND object_id = ?4",
        params![api_base_url, relation_id, normalized_object_type, object_id],
    )?;
    Ok(affected > 0)
}

/// Increment `retry_count` and update `last_retry_at` for a pending callback.
pub(crate) fn increment_retry_pending_callback(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<()> {
    let normalized_object_type = normalize_pending_object_type(object_type);
    let now = unix_ts() as i64;
    conn.execute(
        "UPDATE pending_callbacks
         SET retry_count = retry_count + 1, last_retry_at = ?1
         WHERE api_base_url = ?2 AND relation_id = ?3 AND object_type = ?4 AND object_id = ?5",
        params![
            now,
            api_base_url,
            relation_id,
            normalized_object_type,
            object_id
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Read operations
// ---------------------------------------------------------------------------

/// Find the first pending callback matching
/// (api_base_url, relation_id, object_type, object_id).
/// Returns `None` if not found or if `payload_json` cannot be deserialized.
pub(crate) fn find_pending_callback(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Option<PendingCallbackEntry> {
    let normalized_object_type = normalize_pending_object_type(object_type);
    conn.query_row(
        "SELECT api_base_url, idempotency_key, payload_json, route_secret_enc,
                created_at, retry_count, last_retry_at, relation_id, object_id, object_type
         FROM pending_callbacks
         WHERE api_base_url = ?1 AND relation_id = ?2 AND object_type = ?3 AND object_id = ?4
         LIMIT 1",
        params![api_base_url, relation_id, normalized_object_type, object_id],
        row_to_entry,
    )
    .ok()
    .flatten()
}

/// Delete pending callbacks that have exceeded `max_retries` or are older than
/// `max_age_secs` seconds.  Returns the number of rows deleted.
///
/// Callers should invoke this periodically (e.g. at the start of each worker
/// cycle) to prevent unbounded growth of the `pending_callbacks` table.
#[allow(dead_code)]
pub(crate) fn cleanup_stale_pending_callbacks(
    conn: &Connection,
    max_retries: u32,
    max_age_secs: u64,
) -> rusqlite::Result<usize> {
    let cutoff = unix_ts().saturating_sub(max_age_secs) as i64;
    let affected = conn.execute(
        "DELETE FROM pending_callbacks
         WHERE retry_count >= ?1 OR created_at < ?2",
        params![max_retries as i64, cutoff],
    )?;
    Ok(affected)
}

/// Count the number of pending callbacks for a given domain + relation.
///
/// Used by the discoverer to throttle content fetching when too many callbacks
/// are queued (indicating WP delivery backlog).
pub(crate) fn count_pending_for_relation(
    conn: &Connection,
    api_base_url: &str,
    relation_id: i64,
) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM pending_callbacks WHERE api_base_url = ?1 AND relation_id = ?2",
        params![api_base_url, relation_id],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

/// List all pending callbacks ordered by `created_at` ASC (for startup retry).
///
/// Note: call `cleanup_stale_pending_callbacks` periodically to remove entries
/// that have exceeded max retries or the 72-hour TTL before listing.
#[allow(dead_code)]
pub(crate) fn list_all_pending_callbacks(conn: &Connection) -> Vec<PendingCallbackEntry> {
    let mut stmt = match conn.prepare(
        "SELECT api_base_url, idempotency_key, payload_json, route_secret_enc,
                created_at, retry_count, last_retry_at, relation_id, object_id, object_type
         FROM pending_callbacks
         ORDER BY created_at ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], row_to_entry)
        .map(|rows| rows.filter_map(|r| r.ok().flatten()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<PendingCallbackEntry>> {
    let payload_json: String = row.get(2)?;
    let payload: TranslationCallbackPayload = match serde_json::from_str(&payload_json) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    Ok(Some(PendingCallbackEntry {
        api_base_url: row.get(0)?,
        idempotency_key: row.get(1)?,
        payload,
        route_secret: row.get(3)?,
        created_at: row.get::<_, i64>(4)? as u64,
        retry_count: row.get::<_, i64>(5)? as u32,
        last_retry_at: row.get::<_, i64>(6)? as u64,
        relation_id: row.get(7)?,
        object_id: row.get(8)?,
        object_type: row.get::<_, String>(9)?,
    }))
}

fn normalize_pending_object_type(raw: &str) -> &'static str {
    match raw {
        "term" | "taxonomy" => "taxonomy",
        "post" | "post_type" => "post_type",
        "language_pack" => "language_pack",
        _ => "post_type",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
