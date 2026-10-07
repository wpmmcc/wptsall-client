//! CRUD operations for `translation_jobs` and `translation_items` tables.
#![allow(dead_code)]

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Keep an item mutation and its job projection atomic, including when the
/// caller already owns a transaction. SQLite savepoints may nest by name.
struct JobMutation<'a> {
    conn: &'a Connection,
    committed: bool,
}

impl<'a> JobMutation<'a> {
    fn new(conn: &'a Connection) -> Result<Self> {
        conn.execute_batch("SAVEPOINT translation_job_mutation")?;
        Ok(Self {
            conn,
            committed: false,
        })
    }

    fn commit(mut self) -> Result<()> {
        self.conn
            .execute_batch("RELEASE translation_job_mutation")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for JobMutation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch(
                "ROLLBACK TO translation_job_mutation; RELEASE translation_job_mutation",
            );
        }
    }
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
    let damaged = |index| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "damaged saved item policy",
            )),
        )
    };
    let component_id: String = row.get(11)?;
    let component_ids = row
        .get::<_, Option<String>>(12)?
        .map(|s| serde_json::from_str::<Vec<String>>(&s).map_err(|_| damaged(12)))
        .transpose()?
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
            .map(|s| serde_json::from_str(&s).map_err(|_| damaged(16)))
            .transpose()?,
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
    let changed = match status {
        "running" => conn.execute(
            "UPDATE translation_jobs
                 SET status = ?1, started_at = COALESCE(started_at, ?2), updated_at = ?2
                 WHERE id = ?3",
            params![status, now, job_id],
        )?,
        "completed" | "failed" | "partial" => conn.execute(
            "UPDATE translation_jobs
                 SET status = ?1, completed_at = ?2, updated_at = ?2
                 WHERE id = ?3",
            params![status, now, job_id],
        )?,
        _ => conn.execute(
            "UPDATE translation_jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, now, job_id],
        )?,
    };
    anyhow::ensure!(
        changed == 1,
        "job status was not committed; original state retained"
    );
    Ok(())
}

/// Project persisted item rows, never attempted lane deltas. Open jobs retain
/// their lifecycle until finalization; closed jobs track review/retry changes.
/// `empty_status` preserves a scan failure that could not create an item row.
pub(crate) fn project_job_from_items(
    conn: &Connection,
    job_id: i64,
    finalize: bool,
    empty_status: &str,
) -> Result<String> {
    let status = conn.query_row(
        "WITH counts AS (
            SELECT COUNT(*) AS total,
                COALESCE(SUM(status IN ('done', 'skipped')), 0) AS done,
                COALESCE(SUM(status IN ('failed', 'rejected')), 0) AS failed,
                COALESCE(SUM(status NOT IN ('done', 'skipped', 'failed', 'rejected')), 0) AS pending
            FROM translation_items WHERE job_id = ?1
         ), projection AS (
            SELECT total, done, failed,
                CASE WHEN total = 0 THEN ?3
                     WHEN pending > 0 THEN 'partial'
                     WHEN failed = 0 THEN 'completed'
                     WHEN done = 0 THEN 'failed'
                     ELSE 'partial' END AS status
            FROM counts
         )
         UPDATE translation_jobs
         SET total_items = (SELECT total FROM projection),
             done_items = (SELECT done FROM projection),
             failed_items = (SELECT failed FROM projection),
             status = CASE
                 WHEN ?2 OR status IN ('completed', 'failed', 'partial')
                 THEN (SELECT status FROM projection) ELSE status END,
             completed_at = CASE
                 WHEN ?2 OR status IN ('completed', 'failed', 'partial')
                 THEN COALESCE(completed_at, ?4) ELSE completed_at END,
             updated_at = ?4
         WHERE id = ?1
         RETURNING status",
        params![job_id, finalize, empty_status, now_ts()],
        |row| row.get(0),
    )?;
    Ok(status)
}



pub(crate) fn get_job_checked(conn: &Connection, job_id: i64) -> Result<Option<TranslationJob>> {
    Ok(conn
        .query_row(
            "SELECT id, domain, relation_id, business_line, status,
                total_items, done_items, failed_items, triggered_by,
                started_at, completed_at, created_at, updated_at
         FROM translation_jobs WHERE id = ?1",
            params![job_id],
            row_to_job,
        )
        .optional()?)
}

/// SELECT jobs with optional domain filter, ordered by `created_at DESC`.
pub(crate) fn list_jobs(
    conn: &Connection,
    domain: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<TranslationJob>> {
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

    let mut stmt = conn.prepare(&sql)?;

    let rows = if use_domain {
        stmt.query_map(params![domain.unwrap_or(""), limit, offset], row_to_job)
    } else {
        stmt.query_map(params![limit, offset], row_to_job)
    };

    let rows = rows?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
pub(crate) fn get_job_progress(conn: &Connection, job_id: i64) -> Result<JobProgress> {
    Ok(conn.query_row(
        "SELECT
            COUNT(*) AS total,
            COALESCE(SUM(CASE WHEN status IN ('done', 'skipped') THEN 1 ELSE 0 END), 0) AS done,
            COALESCE(SUM(CASE WHEN status IN ('failed', 'rejected') THEN 1 ELSE 0 END), 0) AS failed,
            COALESCE(SUM(CASE WHEN status = 'pending_review' THEN 1 ELSE 0 END), 0) AS pending_review,
            COALESCE(SUM(CASE WHEN status NOT IN ('done', 'skipped', 'failed', 'rejected', 'pending_review') THEN 1 ELSE 0 END), 0) AS translating
         FROM translation_items WHERE job_id = ?1",
        params![job_id],
        |row| {
            Ok(JobProgress {
                total: row.get(0)?,
                done: row.get(1)?,
                failed: row.get(2)?,
                pending_review: row.get(3)?,
                translating: row.get(4)?,
            })
        },
    )?)
}

pub(crate) fn count_items_by_status_for_relation(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    status: &str,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE domain = ?1 AND relation_id = ?2 AND status = ?3",
        params![domain, relation_id, status],
        |row| row.get(0),
    )?)
}

/// Legacy pipeline items use a filesystem slug, which omits the WP path.
/// Require the owning job's full client base before counting that alias.
pub(crate) fn count_legacy_items_by_status_for_relation(
    conn: &Connection,
    client_base: &str,
    legacy_domain: &str,
    relation_id: i64,
    status: &str,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM translation_items i
         JOIN translation_jobs j ON j.id = i.job_id
         WHERE i.domain = ?1 AND j.domain = ?2 AND i.relation_id = ?3 AND i.status = ?4",
        params![legacy_domain, client_base, relation_id, status],
        |row| row.get(0),
    )?)
}

pub(crate) fn count_items_by_status(conn: &Connection, domain: &str, status: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE domain = ?1 AND status = ?2",
        params![domain, status],
        |row| row.get(0),
    )?)
}

pub(crate) fn count_all_items_by_status(conn: &Connection, status: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE status = ?1",
        params![status],
        |row| row.get(0),
    )?)
}

// ---------------------------------------------------------------------------
// Item functions
// ---------------------------------------------------------------------------

/// INSERT OR IGNORE a translation item (de-duplicated by the UNIQUE constraint on
/// `(domain, relation_id, object_type, wp_object_id, task_type)`).
/// Returns the `id` of the inserted or already-existing row.
pub(crate) fn create_item(conn: &Connection, req: &CreateItemRequest) -> Result<i64> {
    let mutation = JobMutation::new(conn)?;
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
    let (id, owner_job_id): (i64, i64) = conn.query_row(
        "SELECT id, job_id FROM translation_items
         WHERE domain = ?1 AND relation_id = ?2
           AND object_type = ?3 AND wp_object_id = ?4 AND task_type = ?5",
        params![
            req.domain,
            req.relation_id,
            req.object_type,
            req.wp_object_id,
            req.task_type,
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    project_job_from_items(conn, owner_job_id, false, "completed")?;
    mutation.commit()?;
    Ok(id)
}

/// Persist a lane outcome, its component/path metadata and owner projection
/// together. A failed write must not leave a new pending row or partial edits.
pub(crate) fn record_item_outcome(
    conn: &Connection,
    req: &CreateItemRequest,
    translated_path: &str,
    status: &str,
    error: Option<&str>,
) -> Result<i64> {
    let mutation = JobMutation::new(conn)?;
    let item_id = create_item(conn, req)?;
    update_item_component_trace(conn, item_id, &req.component_id, &req.component_ids)?;
    update_item_translated_path(conn, item_id, translated_path)?;
    update_item_status(conn, item_id, status, error)?;
    mutation.commit()?;
    Ok(item_id)
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
    let mutation = JobMutation::new(conn)?;
    let now = now_ts();
    let changed = match status {
        "fetched" => conn.execute(
            "UPDATE translation_items
                 SET status = ?1, error_message = ?2, fetched_at = ?3, updated_at = ?3
                 WHERE id = ?4",
            params![status, error, now, item_id],
        )?,
        "translated" => conn.execute(
            "UPDATE translation_items
                 SET status = ?1, error_message = ?2, translated_at = ?3, updated_at = ?3
                 WHERE id = ?4",
            params![status, error, now, item_id],
        )?,
        "done" => conn.execute(
            "UPDATE translation_items
                 SET status = ?1, error_message = ?2, synced_at = ?3, updated_at = ?3
                 WHERE id = ?4",
            params![status, error, now, item_id],
        )?,
        _ => conn.execute(
            "UPDATE translation_items
                 SET status = ?1, error_message = ?2, updated_at = ?3
                 WHERE id = ?4",
            params![status, error, now, item_id],
        )?,
    };
    anyhow::ensure!(
        changed == 1,
        "item status was not committed; original state retained"
    );
    let owner_job_id = conn.query_row(
        "SELECT job_id FROM translation_items WHERE id = ?1",
        params![item_id],
        |row| row.get::<_, i64>(0),
    );
    match owner_job_id {
        Ok(job_id) => {
            project_job_from_items(conn, job_id, false, "completed")?;
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            anyhow::bail!("item disappeared during status save")
        }
        Err(error) => return Err(error.into()),
    }
    mutation.commit()?;
    Ok(())
}

/// Update the translated_path for a translation item after the translated file is written.
pub(crate) fn update_item_translated_path(
    conn: &Connection,
    item_id: i64,
    translated_path: &str,
) -> Result<()> {
    let now = crate::logging::unix_ts() as i64;
    let changed = conn.execute(
        "UPDATE translation_items SET translated_path = ?1, updated_at = ?2 WHERE id = ?3",
        rusqlite::params![translated_path, now, item_id],
    )?;
    anyhow::ensure!(
        changed == 1,
        "item result path was not committed; original state retained"
    );
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
    let changed = conn.execute(
        "UPDATE translation_items
         SET retry_count = retry_count + 1, updated_at = ?1
         WHERE id = ?2",
        params![now, item_id],
    )?;
    anyhow::ensure!(
        changed == 1,
        "item retry count was not committed; original state retained"
    );
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

/// Commit an acknowledged saved callback and the scan's materialized-success
/// marker together. A failed local write must retain the item and its queue.
pub(crate) fn complete_saved_callback(
    conn: &Connection,
    item_id: i64,
    client_base: &str,
    idempotency_key: &str,
    response_json: &str,
) -> Result<()> {
    let mutation = JobMutation::new(conn)?;
    let item = get_item_checked(conn, item_id)?
        .ok_or_else(|| anyhow::anyhow!("saved callback item not found: {item_id}"))?;
    let object_type = match item.object_type.as_str() {
        "post" => "post_type",
        "term" => "taxonomy",
        other => other,
    };
    if let Some(saved) = crate::db::pending_callbacks::find_pending_callback(
        conn,
        client_base,
        item.relation_id,
        object_type,
        item.wp_object_id,
    )? {
        anyhow::ensure!(
            saved.idempotency_key == idempotency_key
                && saved.payload.client_task_id == item.client_task_id
                && saved.payload.business_line == item.business_line
                && saved.payload.source_lang
                    == item
                        .effective_source_lang
                        .as_deref()
                        .unwrap_or(&item.source_lang)
                && saved.payload.target_lang
                    == item
                        .effective_target_lang
                        .as_deref()
                        .unwrap_or(&item.target_lang)
                && (item.wp_object_subtype.is_empty()
                    || saved.payload.subtype == item.wp_object_subtype),
            "saved callback completion scope mismatch; original item and result retained"
        );
    }
    let record_id = crate::db::translations::insert_translation_record(
        conn,
        &crate::db::translations::InsertTranslationRecord {
            domain: client_base.to_string(),
            relation_id: Some(item.relation_id),
            object_id: Some(item.wp_object_id),
            object_type: Some(object_type.to_string()),
            business_line: Some(item.business_line.clone()),
            source_lang: item.effective_source_lang.unwrap_or(item.source_lang),
            target_lang: item.effective_target_lang.unwrap_or(item.target_lang),
            status: "pending".to_string(),
            idempotency_key: Some(idempotency_key.to_string()),
            component_ids: item.component_ids,
            ..Default::default()
        },
    )?;
    crate::db::translations::mark_callback_sent(conn, record_id)?;
    update_item_sync_response(conn, item_id, response_json)?;
    update_item_status(conn, item_id, "done", None)?;
    crate::db::pending_callbacks::remove_pending_callback(
        conn,
        client_base,
        item.relation_id,
        object_type,
        item.wp_object_id,
    )?;
    mutation.commit()
}

/// SELECT all items for a job. Optionally filter by status.
pub(crate) fn list_items_by_job(
    conn: &Connection,
    job_id: i64,
    status_filter: Option<&str>,
) -> Result<Vec<TranslationItem>> {
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

    let mut stmt = conn.prepare(&sql)?;

    let rows = if use_status {
        stmt.query_map(params![job_id, status_filter.unwrap_or("")], row_to_item)
    } else {
        stmt.query_map(params![job_id], row_to_item)
    };

    let rows = rows?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// SELECT all items for a specific (domain, relation_id, wp_object_id) tuple
/// (may span multiple task_types).
pub(crate) fn get_items_by_object(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
) -> Result<Vec<TranslationItem>> {
    let mut stmt = conn.prepare(
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
    )?;
    let rows = stmt.query_map(params![domain, relation_id, wp_object_id], row_to_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// SELECT items across all jobs that match a given status, optionally filtered
/// by max_retries (items where retry_count < max_retries are eligible).
pub(crate) fn list_items_by_status(
    conn: &Connection,
    status: &str,
    max_retries: i64,
) -> Result<Vec<TranslationItem>> {
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
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![status, max_retries], row_to_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Pending-review inbox: all `pending_review` items (no retry_count gate), newest first.
pub(crate) fn list_pending_review_items(
    conn: &Connection,
    limit: i64,
) -> Result<Vec<TranslationItem>> {
    let lim = limit.clamp(1, 500);
    let sql = "SELECT id, job_id, domain, relation_id, business_line, object_type,
                      wp_object_id, wp_object_subtype, task_type, source_lang, target_lang,
                      component_id, component_ids_json, selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                      raw_path, translated_path, content_hash, status,
                      client_task_id, upload_id, wp_attachment_id, sync_response_json,
                      error_message, retry_count, max_retries, fetched_at, translated_at,
                      synced_at, created_at, updated_at
               FROM translation_items
               WHERE status = 'pending_review'
               ORDER BY
                 CASE WHEN error_message IS NOT NULL AND TRIM(error_message) != '' THEN 0 ELSE 1 END,
                 updated_at DESC
               LIMIT ?1";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![lim], row_to_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Count of items in `pending_review` (for overview / nav badges).
pub(crate) fn count_pending_review_items(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM translation_items WHERE status = 'pending_review'",
        [],
        |row| row.get(0),
    )?)
}



pub(crate) fn get_item_checked(conn: &Connection, id: i64) -> Result<Option<TranslationItem>> {
    Ok(conn.query_row(
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
    .optional()?)
}

/// SELECT items in intermediate states (fetched/translated) for a given domain,
/// suitable for crash-recovery resume. These items have been partially processed
/// and can be continued from their current state.
pub(crate) fn list_resumable_items(
    conn: &Connection,
    domain: &str,
    statuses: &[&str],
) -> Result<Vec<TranslationItem>> {
    if statuses.is_empty() {
        return Ok(Vec::new());
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

    let mut stmt = conn.prepare(&sql)?;

    // Build params dynamically
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(domain.to_string()));
    for s in statuses {
        param_values.push(Box::new(s.to_string()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), row_to_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn list_resumable_items_for_client_base(
    conn: &Connection,
    client_base: &str,
    legacy_domain: &str,
    statuses: &[&str],
) -> Result<Vec<TranslationItem>> {
    let mut items = list_resumable_items(conn, client_base, statuses)?;
    if legacy_domain != client_base {
        for item in list_resumable_items(conn, legacy_domain, statuses)? {
            let job = get_job_checked(conn, item.job_id)?
                .ok_or_else(|| anyhow::anyhow!("retained item has no owning job; retained"))?;
            if job.domain == client_base {
                items.push(item);
            }
        }
    }
    items.sort_unstable_by_key(|item| item.id);
    Ok(items)
}



pub(crate) fn list_items_by_domain_object_checked(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
) -> Result<Vec<TranslationItem>> {
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
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![domain, relation_id, wp_object_id], row_to_item)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// COUNT items for a job that are not yet in a terminal state
/// (`done`, `skipped`, `failed`).
pub(crate) fn count_pending_items(conn: &Connection, job_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM translation_items
         WHERE job_id = ?1 AND status NOT IN ('done', 'skipped', 'failed', 'rejected')",
        params![job_id],
        |row| row.get(0),
    )?)
}



pub(crate) fn has_pending_review_item_checked(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    wp_object_id: i64,
) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM translation_items
         WHERE domain = ?1
           AND relation_id = ?2
           AND wp_object_id = ?3
           AND status = 'pending_review'
         LIMIT 1",
            params![domain, relation_id, wp_object_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
