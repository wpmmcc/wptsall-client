//! Object Sync Coordinator for the `object_sync_coordination` table.
//!
//! When a WP post has both text fields AND media (image/video/audio/document),
//! all applicable task types must complete before the combined
//! `/translation-callback` is posted. This module tracks per-task-type
//! completion status for each (domain, relation_id, wp_object_id, business_line)
//! combination.
//!
//! Status values per column: `'na'` (not applicable), `'pending'`, `'done'`
#![allow(dead_code)]

use anyhow::Result;
use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Map a task_type string to the corresponding column name.
/// Returns `None` for unrecognised task types.
fn col_for_task_type(task_type: &str) -> Option<&'static str> {
    match task_type {
        "text" => Some("text_status"),
        "image" => Some("image_status"),
        "video" => Some("video_status"),
        "audio" => Some("audio_status"),
        "document" => Some("document_status"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Ensure a coordination row exists for the given coordinates and mark the
/// requested `task_types` as `'pending'` (columns not in `task_types` remain
/// `'na'`; columns already past `'na'` are left unchanged so a concurrent
/// update is not overwritten).
///
/// `task_types` may contain any subset of: "text", "image", "video", "audio",
/// "document".  Unknown values are silently skipped.
pub(crate) fn ensure_coordination(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
    business_line: &str,
    client_task_id: &str,
    task_types: &[&str],
) -> Result<()> {
    // INSERT OR IGNORE so a concurrent caller doesn't wipe an existing row.
    conn.execute(
        "INSERT OR IGNORE INTO object_sync_coordination
             (domain, relation_id, wp_object_id, business_line, client_task_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            domain,
            relation_id,
            wp_object_id,
            business_line,
            client_task_id
        ],
    )?;

    // For each requested task_type, advance the status from 'na' → 'pending'.
    // Columns already at 'pending' or 'done' are left as-is.
    for task_type in task_types {
        if let Some(col) = col_for_task_type(task_type) {
            let sql = format!(
                "UPDATE object_sync_coordination
                 SET {col} = CASE WHEN {col} = 'na' THEN 'pending' ELSE {col} END
                 WHERE domain = ?1 AND relation_id = ?2
                   AND wp_object_id = ?3 AND business_line = ?4",
                col = col,
            );
            conn.execute(
                &sql,
                params![domain, relation_id, wp_object_id, business_line],
            )?;
        }
    }

    Ok(())
}

/// Mark a single task type as `'done'` for the given coordinates.
pub(crate) fn mark_task_type_done(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
    business_line: &str,
    task_type: &str,
) -> Result<()> {
    if let Some(col) = col_for_task_type(task_type) {
        let sql = format!(
            "UPDATE object_sync_coordination
             SET {col} = 'done'
             WHERE domain = ?1 AND relation_id = ?2
               AND wp_object_id = ?3 AND business_line = ?4",
            col = col,
        );
        conn.execute(
            &sql,
            params![domain, relation_id, wp_object_id, business_line],
        )?;
    }
    Ok(())
}

/// Returns `true` if every column that is NOT `'na'` is `'done'`, meaning all
/// applicable task types have finished and the combined callback can be sent.
///
/// Returns `false` if the row does not exist.
pub(crate) fn is_all_done(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
    business_line: &str,
) -> bool {
    let result: rusqlite::Result<(String, String, String, String, String)> = conn.query_row(
        "SELECT text_status, image_status, video_status, audio_status, document_status
         FROM object_sync_coordination
         WHERE domain = ?1 AND relation_id = ?2
           AND wp_object_id = ?3 AND business_line = ?4",
        params![domain, relation_id, wp_object_id, business_line],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    );

    match result {
        Ok((text, image, video, audio, document)) => {
            let statuses = [text, image, video, audio, document];
            // Every non-'na' column must be 'done'.
            statuses.iter().all(|s| s == "na" || s == "done")
                // Guard: at least one column must be non-'na' (otherwise the
                // row was never properly initialised and we should not fire the
                // callback).
                && statuses.iter().any(|s| s != "na")
        }
        Err(_) => false,
    }
}

/// Record the timestamp at which the combined translation-callback was sent.
pub(crate) fn mark_callback_sent(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
    business_line: &str,
) -> Result<()> {
    let now = now_secs();
    conn.execute(
        "UPDATE object_sync_coordination
         SET callback_at = ?1
         WHERE domain = ?2 AND relation_id = ?3
           AND wp_object_id = ?4 AND business_line = ?5",
        params![now, domain, relation_id, wp_object_id, business_line],
    )?;
    Ok(())
}

/// Return the `client_task_id` stored for the given coordinates, or `None` if
/// the row does not exist.
pub(crate) fn get_client_task_id(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
    business_line: &str,
) -> Option<String> {
    conn.query_row(
        "SELECT client_task_id FROM object_sync_coordination
         WHERE domain = ?1 AND relation_id = ?2
           AND wp_object_id = ?3 AND business_line = ?4",
        params![domain, relation_id, wp_object_id, business_line],
        |row| row.get(0),
    )
    .ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
