//! CRUD operations for `translation_jobs` and `translation_items` tables.
#![allow(dead_code)]

use anyhow::Result;
use rusqlite::{params, Connection};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TranslationJob {
    pub id: i64,
    pub domain: String,
    pub relation_id: i64,
    pub business_line: String,
    /// pending | running | completed | failed | partial
    pub status: String,
    pub total_items: i64,
    pub done_items: i64,
    pub failed_items: i64,
    /// schedule | manual | api
    pub triggered_by: String,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TranslationItem {
    pub id: i64,
    pub job_id: i64,
    pub domain: String,
    pub relation_id: i64,
    pub business_line: String,
    pub object_type: String,
    pub wp_object_id: i64,
    pub wp_object_subtype: String,
    /// text | image | video | audio | document
    pub task_type: String,
    pub source_lang: String,
    pub target_lang: String,
    pub component_id: String,
    pub component_ids: Vec<String>,
    pub selected_component_id: Option<String>,
    pub effective_source_lang: Option<String>,
    pub effective_target_lang: Option<String>,
    pub editable_overrides: Option<serde_json::Value>,
    pub raw_path: String,
    pub translated_path: String,
    pub content_hash: String,
    /// pending | fetching | fetched | translating | translated | syncing | done | failed | skipped
    pub status: String,
    pub client_task_id: String,
    pub upload_id: Option<String>,
    pub wp_attachment_id: Option<i64>,
    pub sync_response_json: Option<String>,
    pub error_message: Option<String>,
    pub retry_count: i64,
    pub max_retries: i64,
    pub fetched_at: Option<i64>,
    pub translated_at: Option<i64>,
    pub synced_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug)]
pub(crate) struct CreateJobRequest {
    pub domain: String,
    pub relation_id: i64,
    pub business_line: String,
    pub triggered_by: String,
}

#[derive(Debug)]
pub(crate) struct CreateItemRequest {
    pub job_id: i64,
    pub domain: String,
    pub relation_id: i64,
    pub business_line: String,
    pub object_type: String,
    pub wp_object_id: i64,
    pub wp_object_subtype: String,
    pub task_type: String,
    pub source_lang: String,
    pub target_lang: String,
    pub component_id: String,
    pub component_ids: Vec<String>,
    pub selected_component_id: Option<String>,
    pub effective_source_lang: Option<String>,
    pub effective_target_lang: Option<String>,
    pub editable_overrides: Option<serde_json::Value>,
    pub raw_path: String,
    pub client_task_id: String,
    pub max_retries: i64,
}

// ---------------------------------------------------------------------------
// Row mapper helpers
// ---------------------------------------------------------------------------

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranslationJob> {
    Ok(TranslationJob {
        id: row.get(0)?,
        domain: row.get(1)?,
        relation_id: row.get(2)?,
        business_line: row.get(3)?,
        status: row.get(4)?,
        total_items: row.get(5)?,
        done_items: row.get(6)?,
        failed_items: row.get(7)?,
        triggered_by: row.get(8)?,
        started_at: row.get(9)?,
        completed_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranslationItem> {
    let component_id: String = row.get(11)?;
    let component_ids = row
        .get::<_, Option<String>>(12)?
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            if component_id.trim().is_empty() {
                Vec::new()
            } else {
                vec![component_id.clone()]
            }
        });
    Ok(TranslationItem {
        id: row.get(0)?,
        job_id: row.get(1)?,
        domain: row.get(2)?,
        relation_id: row.get(3)?,
        business_line: row.get(4)?,
        object_type: row.get(5)?,
        wp_object_id: row.get(6)?,
        wp_object_subtype: row.get(7)?,
        task_type: row.get(8)?,
        source_lang: row.get(9)?,
        target_lang: row.get(10)?,
        component_id,
        component_ids,
        selected_component_id: row.get(13)?,
        effective_source_lang: row.get(14)?,
        effective_target_lang: row.get(15)?,
        editable_overrides: row
            .get::<_, Option<String>>(16)?
            .and_then(|s| serde_json::from_str(&s).ok()),
        raw_path: row.get(17)?,
        translated_path: row.get(18)?,
        content_hash: row.get(19)?,
        status: row.get(20)?,
        client_task_id: row.get(21)?,
        upload_id: row.get(22)?,
        wp_attachment_id: row.get(23)?,
        sync_response_json: row.get(24)?,
        error_message: row.get(25)?,
        retry_count: row.get(26)?,
        max_retries: row.get(27)?,
        fetched_at: row.get(28)?,
        translated_at: row.get(29)?,
        synced_at: row.get(30)?,
        created_at: row.get(31)?,
        updated_at: row.get(32)?,
    })
}

// ---------------------------------------------------------------------------
// Job functions
// ---------------------------------------------------------------------------

/// INSERT a new translation job and return its id.
pub(crate) fn create_job(conn: &Connection, req: &CreateJobRequest) -> Result<i64> {
    let now = now_ts();
    conn.execute(
        "INSERT INTO translation_jobs
         (domain, relation_id, business_line, status, total_items, done_items, failed_items,
          triggered_by, started_at, completed_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'pending', 0, 0, 0, ?4, NULL, NULL, ?5, ?5)",
        params![
            req.domain,
            req.relation_id,
            req.business_line,
            req.triggered_by,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// UPDATE job status. Also sets `started_at` when transitioning to `running`,
/// and `completed_at` when transitioning to `completed`, `failed`, or `partial`.
pub(crate) fn update_job_status(conn: &Connection, job_id: i64, status: &str) -> Result<()> {
    let now = now_ts();
    match status {
        "running" => {
            conn.execute(
                "UPDATE translation_jobs
                 SET status = ?1, started_at = COALESCE(started_at, ?2), updated_at = ?2
                 WHERE id = ?3",
                params![status, now, job_id],
            )?;
        }
        "completed" | "failed" | "partial" => {
            conn.execute(
                "UPDATE translation_jobs
                 SET status = ?1, completed_at = ?2, updated_at = ?2
                 WHERE id = ?3",
                params![status, now, job_id],
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE translation_jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status, now, job_id],
            )?;
        }
    }
    Ok(())
}

/// Atomically increment job counters (done, failed, total) and touch `updated_at`.
pub(crate) fn increment_job_counters(
    conn: &Connection,
    job_id: i64,
    done_delta: i64,
    failed_delta: i64,
    total_delta: i64,
) -> Result<()> {
    let now = now_ts();
    conn.execute(
        "UPDATE translation_jobs
         SET done_items   = done_items   + ?1,
             failed_items = failed_items + ?2,
             total_items  = total_items  + ?3,
             updated_at   = ?4
         WHERE id = ?5",
        params![done_delta, failed_delta, total_delta, now, job_id],
    )?;
    Ok(())
}

/// SELECT a single job by id. Returns `None` if not found.
pub(crate) fn get_job(conn: &Connection, job_id: i64) -> Option<TranslationJob> {
    conn.query_row(
        "SELECT id, domain, relation_id, business_line, status,
                total_items, done_items, failed_items, triggered_by,
                started_at, completed_at, created_at, updated_at
         FROM translation_jobs WHERE id = ?1",
        params![job_id],
        row_to_job,
    )
    .ok()
}

/// SELECT jobs with optional domain filter, ordered by `created_at DESC`.
pub(crate) fn list_jobs(
    conn: &Connection,
    domain: Option<&str>,
    limit: i64,
    offset: i64,
) -> Vec<TranslationJob> {
    let (sql, use_domain) = match domain {
        Some(_) => (
            "SELECT id, domain, relation_id, business_line, status,
                    total_items, done_items, failed_items, triggered_by,
                    started_at, completed_at, created_at, updated_at
             FROM translation_jobs
             WHERE domain = ?1
             ORDER BY created_at DESC
             LIMIT ?2 OFFSET ?3"
                .to_string(),
            true,
        ),
        None => (
            "SELECT id, domain, relation_id, business_line, status,
                    total_items, done_items, failed_items, triggered_by,
                    started_at, completed_at, created_at, updated_at
             FROM translation_jobs
             ORDER BY created_at DESC
             LIMIT ?1 OFFSET ?2"
                .to_string(),
            false,
        ),
    };

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let rows = if use_domain {
        stmt.query_map(params![domain.unwrap_or(""), limit, offset], row_to_job)
    } else {
        stmt.query_map(params![limit, offset], row_to_job)
    };

    rows.map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Progress stats for a single job.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct JobProgress {
    pub total: i64,
    pub done: i64,
    pub failed: i64,
    pub pending_review: i64,
    pub translating: i64,
}

/// Get progress stats for a specific job by counting item statuses.
pub(crate) fn get_job_progress(conn: &Connection, job_id: i64) -> JobProgress {
    let mut progress = JobProgress {
        total: 0,
        done: 0,
        failed: 0,
        pending_review: 0,
        translating: 0,
    };
    let _ = conn.query_row(
        "SELECT
            COUNT(*) AS total,
            SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END) AS done,
            SUM(CASE WHEN status = 'failed' OR (status = 'translated' AND error_message IS NOT NULL AND error_message != '') THEN 1 ELSE 0 END) AS failed,
            SUM(CASE WHEN status = 'pending_review' THEN 1 ELSE 0 END) AS pending_review,
            SUM(CASE WHEN status IN ('translating','syncing','fetching','fetched') OR (status = 'translated' AND (error_message IS NULL OR error_message = '')) THEN 1 ELSE 0 END) AS translating
         FROM translation_items WHERE job_id = ?1",
        params![job_id],
        |row| {
            progress.total = row.get(0).unwrap_or(0);
            progress.done = row.get(1).unwrap_or(0);
            progress.failed = row.get(2).unwrap_or(0);
            progress.pending_review = row.get(3).unwrap_or(0);
            progress.translating = row.get(4).unwrap_or(0);
            Ok(())
        },
    );
    progress
}

pub(crate) fn count_items_by_status_for_relation(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    status: &str,
) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE domain = ?1 AND relation_id = ?2 AND status = ?3",
        params![domain, relation_id, status],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

pub(crate) fn count_items_by_status(conn: &Connection, domain: &str, status: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE domain = ?1 AND status = ?2",
        params![domain, status],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

pub(crate) fn count_all_items_by_status(conn: &Connection, status: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE status = ?1",
        params![status],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Item functions
// ---------------------------------------------------------------------------

/// INSERT OR IGNORE a translation item (de-duplicated by the UNIQUE constraint on
/// `(domain, relation_id, object_type, wp_object_id, task_type)`).
/// Returns the `id` of the inserted or already-existing row.
pub(crate) fn create_item(conn: &Connection, req: &CreateItemRequest) -> Result<i64> {
    let now = now_ts();
    let component_ids = if req.component_ids.is_empty() {
        if req.component_id.trim().is_empty() {
            Vec::new()
        } else {
            vec![req.component_id.clone()]
        }
    } else {
        req.component_ids.clone()
    };
    let component_ids_json =
        serde_json::to_string(&component_ids).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT OR IGNORE INTO translation_items
         (job_id, domain, relation_id, business_line, object_type, wp_object_id,
          wp_object_subtype, task_type, source_lang, target_lang, component_id, component_ids_json,
          selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
          raw_path, translated_path, content_hash, status, client_task_id,
          upload_id, wp_attachment_id, sync_response_json, error_message,
          retry_count, max_retries, fetched_at, translated_at, synced_at,
          created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,'','','pending',?18,
                 NULL,NULL,NULL,NULL,0,?19,NULL,NULL,NULL,?20,?20)",
        params![
            req.job_id,
            req.domain,
            req.relation_id,
            req.business_line,
            req.object_type,
            req.wp_object_id,
            req.wp_object_subtype,
            req.task_type,
            req.source_lang,
            req.target_lang,
            req.component_id,
            component_ids_json,
            req.selected_component_id
                .clone()
                .unwrap_or_else(|| req.component_id.clone()),
            req.effective_source_lang
                .clone()
                .unwrap_or_else(|| req.source_lang.clone()),
            req.effective_target_lang
                .clone()
                .unwrap_or_else(|| req.target_lang.clone()),
            req.editable_overrides
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())),
            req.raw_path,
            req.client_task_id,
            req.max_retries,
            now,
        ],
    )?;

    // Retrieve the id whether the row was just inserted or already existed.
    let id: i64 = conn.query_row(
        "SELECT id FROM translation_items
         WHERE domain = ?1 AND relation_id = ?2
           AND object_type = ?3 AND wp_object_id = ?4 AND task_type = ?5",
        params![
            req.domain,
            req.relation_id,
            req.object_type,
            req.wp_object_id,
            req.task_type,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// UPDATE item status and optional error message. Sets timestamp columns based
/// on the target status:
/// - `fetched`     → fetched_at
/// - `translated`  → translated_at
/// - `done`        → synced_at
pub(crate) fn update_item_status(
    conn: &Connection,
    item_id: i64,
    status: &str,
    error: Option<&str>,
) -> Result<()> {
    let now = now_ts();
    match status {
        "fetched" => {
            conn.execute(
                "UPDATE translation_items
                 SET status = ?1, error_message = ?2, fetched_at = ?3, updated_at = ?3
                 WHERE id = ?4",
                params![status, error, now, item_id],
            )?;
        }
        "translated" => {
            conn.execute(
                "UPDATE translation_items
                 SET status = ?1, error_message = ?2, translated_at = ?3, updated_at = ?3
                 WHERE id = ?4",
                params![status, error, now, item_id],
            )?;
        }
        "done" => {
            conn.execute(
                "UPDATE translation_items
                 SET status = ?1, error_message = ?2, synced_at = ?3, updated_at = ?3
                 WHERE id = ?4",
                params![status, error, now, item_id],
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE translation_items
                 SET status = ?1, error_message = ?2, updated_at = ?3
                 WHERE id = ?4",
                params![status, error, now, item_id],
            )?;
        }
    }
    Ok(())
}

/// Update the translated_path for a translation item after the translated file is written.
pub(crate) fn update_item_translated_path(
    conn: &Connection,
    item_id: i64,
    translated_path: &str,
) -> Result<()> {
    let now = crate::logging::unix_ts() as i64;
    conn.execute(
        "UPDATE translation_items SET translated_path = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![translated_path, now, item_id],
    )?;
    Ok(())
}

/// UPDATE upload_id for an item.
pub(crate) fn update_item_upload_id(
    conn: &Connection,
    item_id: i64,
    upload_id: Option<&str>,
) -> Result<()> {
    let now = now_ts();
    conn.execute(
        "UPDATE translation_items SET upload_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![upload_id, now, item_id],
    )?;
    Ok(())
}

/// UPDATE wp_attachment_id for an item.
pub(crate) fn update_item_wp_attachment_id(
    conn: &Connection,
    item_id: i64,
    attachment_id: i64,
) -> Result<()> {
    let now = now_ts();
    conn.execute(
        "UPDATE translation_items
         SET wp_attachment_id = ?1, updated_at = ?2
         WHERE id = ?3",
        params![attachment_id, now, item_id],
    )?;
    Ok(())
}

/// Update task-level override metadata for an item.
pub(crate) fn update_item_task_override(
    conn: &Connection,
    item_id: i64,
    selected_component_id: Option<&str>,
    effective_source_lang: Option<&str>,
    effective_target_lang: Option<&str>,
    editable_overrides: Option<&serde_json::Value>,
) -> Result<()> {
    let now = now_ts();
    let editable_overrides_json =
        editable_overrides.map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()));
    conn.execute(
        "UPDATE translation_items
         SET selected_component_id = ?1,
             effective_source_lang = ?2,
             effective_target_lang = ?3,
             editable_overrides_json = ?4,
             updated_at = ?5
         WHERE id = ?6",
        params![
            selected_component_id,
            effective_source_lang,
            effective_target_lang,
            editable_overrides_json,
            now,
            item_id
        ],
    )?;
    Ok(())
}

/// Persist actual execution component trace for an item without breaking the
/// legacy single-component `component_id` field.
pub(crate) fn update_item_component_trace(
    conn: &Connection,
    item_id: i64,
    component_id: &str,
    component_ids: &[String],
) -> Result<()> {
    let now = now_ts();
    let effective_component_ids: Vec<String> = if component_ids.is_empty() {
        component_id
            .trim()
            .is_empty()
            .then(Vec::new)
            .unwrap_or_else(|| vec![component_id.to_string()])
    } else {
        component_ids.to_vec()
    };
    let component_ids_json =
        serde_json::to_string(&effective_component_ids).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "UPDATE translation_items
         SET component_id = ?1,
             component_ids_json = ?2,
             updated_at = ?3
         WHERE id = ?4",
        params![component_id, component_ids_json, now, item_id],
    )?;
    Ok(())
}

/// Increment `retry_count` by 1 for an item.
pub(crate) fn increment_item_retry(conn: &Connection, item_id: i64) -> Result<()> {
    let now = now_ts();
    conn.execute(
        "UPDATE translation_items
         SET retry_count = retry_count + 1, updated_at = ?1
         WHERE id = ?2",
        params![now, item_id],
    )?;
    Ok(())
}

/// Persist the last successful WP sync/callback response JSON for audit/debugging.
pub(crate) fn update_item_sync_response(
    conn: &Connection,
    item_id: i64,
    response_json: &str,
) -> Result<()> {
    let now = now_ts();
    conn.execute(
        "UPDATE translation_items
         SET sync_response_json = ?1, updated_at = ?2
         WHERE id = ?3",
        params![response_json, now, item_id],
    )?;
    Ok(())
}

/// SELECT all items for a job. Optionally filter by status.
pub(crate) fn list_items_by_job(
    conn: &Connection,
    job_id: i64,
    status_filter: Option<&str>,
) -> Vec<TranslationItem> {
    let (sql, use_status) = match status_filter {
        Some(_) => (
            "SELECT id, job_id, domain, relation_id, business_line, object_type,
                    wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                    component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                    raw_path, translated_path, content_hash, status,
                    client_task_id, upload_id, wp_attachment_id, sync_response_json,
                    error_message, retry_count, max_retries, fetched_at, translated_at,
                    synced_at, created_at, updated_at
             FROM translation_items
             WHERE job_id = ?1 AND status = ?2
             ORDER BY id ASC"
                .to_string(),
            true,
        ),
        None => (
            "SELECT id, job_id, domain, relation_id, business_line, object_type,
                    wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                    component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                    raw_path, translated_path, content_hash, status,
                    client_task_id, upload_id, wp_attachment_id, sync_response_json,
                    error_message, retry_count, max_retries, fetched_at, translated_at,
                    synced_at, created_at, updated_at
             FROM translation_items
             WHERE job_id = ?1
             ORDER BY id ASC"
                .to_string(),
            false,
        ),
    };

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let rows = if use_status {
        stmt.query_map(params![job_id, status_filter.unwrap_or("")], row_to_item)
    } else {
        stmt.query_map(params![job_id], row_to_item)
    };

    rows.map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// SELECT all items for a specific (domain, relation_id, wp_object_id) tuple
/// (may span multiple task_types).
pub(crate) fn get_items_by_object(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
) -> Vec<TranslationItem> {
    let mut stmt = match conn.prepare(
        "SELECT id, job_id, domain, relation_id, business_line, object_type,
                wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                raw_path, translated_path, content_hash, status,
                client_task_id, upload_id, wp_attachment_id, sync_response_json,
                error_message, retry_count, max_retries, fetched_at, translated_at,
                synced_at, created_at, updated_at
         FROM translation_items
         WHERE domain = ?1 AND relation_id = ?2 AND wp_object_id = ?3
         ORDER BY id ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![domain, relation_id, wp_object_id], row_to_item)
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// SELECT items across all jobs that match a given status, optionally filtered
/// by max_retries (items where retry_count < max_retries are eligible).
pub(crate) fn list_items_by_status(
    conn: &Connection,
    status: &str,
    max_retries: i64,
) -> Vec<TranslationItem> {
    let sql = "SELECT id, job_id, domain, relation_id, business_line, object_type,
                      wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                      component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                      raw_path, translated_path, content_hash, status,
                      client_task_id, upload_id, wp_attachment_id, sync_response_json,
                      error_message, retry_count, max_retries, fetched_at, translated_at,
                      synced_at, created_at, updated_at
               FROM translation_items
               WHERE status = ?1 AND retry_count < ?2
               ORDER BY created_at ASC
               LIMIT 500";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            stmt.query_map(params![status, max_retries], row_to_item)
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

/// Get a single translation item by its primary key id.
pub(crate) fn get_item(conn: &Connection, id: i64) -> Option<TranslationItem> {
    conn.query_row(
        "SELECT id, job_id, domain, relation_id, business_line, object_type,
                wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                raw_path, translated_path, content_hash, status,
                client_task_id, upload_id, wp_attachment_id, sync_response_json,
                error_message, retry_count, max_retries,
                fetched_at, translated_at, synced_at, created_at, updated_at
         FROM translation_items WHERE id = ?1",
        rusqlite::params![id],
        row_to_item,
    )
    .ok()
}

/// SELECT items in intermediate states (fetched/translated) for a given domain,
/// suitable for crash-recovery resume. These items have been partially processed
/// and can be continued from their current state.
pub(crate) fn list_resumable_items(
    conn: &Connection,
    domain: &str,
    statuses: &[&str],
) -> Vec<TranslationItem> {
    if statuses.is_empty() {
        return Vec::new();
    }
    // Build a dynamic IN clause
    let placeholders: Vec<String> = statuses
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "SELECT id, job_id, domain, relation_id, business_line, object_type,
                wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                raw_path, translated_path, content_hash, status,
                client_task_id, upload_id, wp_attachment_id, sync_response_json,
                error_message, retry_count, max_retries, fetched_at, translated_at,
                synced_at, created_at, updated_at
         FROM translation_items
         WHERE domain = ?1 AND status IN ({})
           AND (max_retries <= 0 OR retry_count < max_retries)
         ORDER BY id ASC
         LIMIT 500",
        placeholders.join(", ")
    );

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Build params dynamically
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(domain.to_string()));
    for s in statuses {
        param_values.push(Box::new(s.to_string()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    stmt.query_map(param_refs.as_slice(), row_to_item)
        .map(|iter| iter.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Find items matching a specific domain + relation + WP object ID.
/// Used by review mode to locate items for status update.
pub(crate) fn list_items_by_domain_object(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
) -> Vec<TranslationItem> {
    let sql = "SELECT id, job_id, domain, relation_id, business_line, object_type,
                      wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                      component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                      raw_path, translated_path, content_hash, status,
                      client_task_id, upload_id, wp_attachment_id, sync_response_json,
                      error_message, retry_count, max_retries, fetched_at, translated_at,
                      synced_at, created_at, updated_at
               FROM translation_items
               WHERE domain = ?1 AND relation_id = ?2 AND wp_object_id = ?3
               ORDER BY id ASC
               LIMIT 50";
    conn.prepare(sql)
        .and_then(|mut stmt| {
            stmt.query_map(params![domain, relation_id, wp_object_id], row_to_item)
                .map(|iter| iter.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
}

/// COUNT items for a job that are not yet in a terminal state
/// (`done`, `skipped`, `failed`).
pub(crate) fn count_pending_items(conn: &Connection, job_id: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM translation_items
         WHERE job_id = ?1 AND status NOT IN ('done', 'skipped', 'failed')",
        params![job_id],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
