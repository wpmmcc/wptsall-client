use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TranslationRecord {
    pub(crate) id: i64,
    pub(crate) created_at: i64,
    pub(crate) domain: String,
    pub(crate) relation_id: Option<i64>,
    pub(crate) object_id: Option<i64>,
    pub(crate) object_type: Option<String>,
    pub(crate) business_line: Option<String>,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
    pub(crate) status: String,
    pub(crate) execution_ms: Option<i64>,
    pub(crate) worker_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) callback_sent_at: Option<i64>,
    pub(crate) callback_retries: i32,
    pub(crate) fields_count: i32,
    pub(crate) error_message: Option<String>,
    pub(crate) component_ids: Vec<String>,
    pub(crate) media_mappings_count: i32,
    pub(crate) failed_fields_count: i32,
    pub(crate) primary_failure_reason: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct InsertTranslationRecord {
    pub(crate) domain: String,
    pub(crate) relation_id: Option<i64>,
    pub(crate) object_id: Option<i64>,
    pub(crate) object_type: Option<String>,
    pub(crate) business_line: Option<String>,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
    pub(crate) status: String,
    pub(crate) execution_ms: Option<i64>,
    pub(crate) worker_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) fields_count: i32,
    pub(crate) error_message: Option<String>,
    pub(crate) component_ids: Vec<String>,
    pub(crate) media_mappings_count: i32,
    pub(crate) failed_fields_count: i32,
    pub(crate) primary_failure_reason: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct TranslationQueryParams {
    pub(crate) page: u32,
    pub(crate) limit: u32,
    pub(crate) domain: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) search: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TranslationListResult {
    pub(crate) records: Vec<TranslationRecord>,
    pub(crate) total: i64,
    pub(crate) page: u32,
    pub(crate) limit: u32,
}

pub(crate) fn insert_translation_record(
    conn: &Connection,
    rec: &InsertTranslationRecord,
) -> Result<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "INSERT INTO translation_records
         (created_at, domain, relation_id, object_id, object_type, business_line,
          source_lang, target_lang, status, execution_ms, worker_id, idempotency_key,
          fields_count, error_message, component_ids_json, media_mappings_count,
          failed_fields_count, primary_failure_reason)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        rusqlite::params![
            now,
            rec.domain,
            rec.relation_id,
            rec.object_id,
            rec.object_type,
            rec.business_line,
            rec.source_lang,
            rec.target_lang,
            rec.status,
            rec.execution_ms,
            rec.worker_id,
            rec.idempotency_key,
            rec.fields_count,
            rec.error_message,
            serde_json::to_string(&rec.component_ids).unwrap_or_else(|_| "[]".to_string()),
            rec.media_mappings_count,
            rec.failed_fields_count,
            rec.primary_failure_reason,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(dead_code)]
pub(crate) fn update_translation_status(
    conn: &Connection,
    id: i64,
    status: &str,
    error_message: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE translation_records SET status = ?1, error_message = ?2 WHERE id = ?3",
        rusqlite::params![status, error_message, id],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn mark_callback_sent(conn: &Connection, id: i64) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "UPDATE translation_records SET callback_sent_at = ?1, status = 'success' WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn increment_callback_retries(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE translation_records SET callback_retries = callback_retries + 1 WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

fn parse_component_ids_json(raw: Option<String>) -> Vec<String> {
    raw.and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default()
}

/// A prior translation record is only safe for dedup short-circuit when it
/// carries evidence that a real translation/callback path already happened.
///
/// Historical `NoChanges` runs used to be written as `status=success` with
/// zero translated fields and no component trace, which would permanently
/// suppress later real executions. Treat those rows as non-materialized.
pub(crate) fn has_materialized_success_record(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_id: i64,
    object_type: &str,
) -> bool {
    conn.query_row(
        "SELECT 1
         FROM translation_records
         WHERE domain = ?1
           AND relation_id = ?2
           AND object_id = ?3
           AND object_type = ?4
           AND status = 'success'
           AND (
                callback_sent_at IS NOT NULL
                OR fields_count > 0
                OR media_mappings_count > 0
                OR (component_ids_json IS NOT NULL
                    AND component_ids_json != ''
                    AND component_ids_json != '[]')
           )
         LIMIT 1",
        params![domain, relation_id, object_id, object_type],
        |_| Ok(()),
    )
    .is_ok()
}

pub(crate) fn query_translation_records(
    conn: &Connection,
    params: &TranslationQueryParams,
) -> Result<TranslationListResult> {
    let page = params.page.max(1);
    let limit = if params.limit == 0 {
        20
    } else {
        params.limit.min(100)
    };
    let offset = (page - 1) * limit;

    // Build WHERE clauses
    let mut conditions: Vec<String> = vec![];
    let mut bind_vals: Vec<Box<dyn rusqlite::ToSql>> = vec![];

    if let Some(ref domain) = params.domain {
        if !domain.is_empty() {
            conditions.push(format!("domain = ?{}", bind_vals.len() + 1));
            bind_vals.push(Box::new(domain.clone()));
        }
    }
    if let Some(ref status) = params.status {
        if !status.is_empty() {
            conditions.push(format!("status = ?{}", bind_vals.len() + 1));
            bind_vals.push(Box::new(status.clone()));
        }
    }
    if let Some(ref search) = params.search {
        if !search.is_empty() {
            let pattern = format!("%{}%", search);
            conditions.push(format!(
                "(domain LIKE ?{pos} OR object_type LIKE ?{pos} OR business_line LIKE ?{pos})",
                pos = bind_vals.len() + 1
            ));
            bind_vals.push(Box::new(pattern));
        }
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    // Count total
    let count_sql = format!("SELECT COUNT(*) FROM translation_records {}", where_clause);
    let total: i64 = {
        let mut stmt = conn.prepare(&count_sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = bind_vals.iter().map(|b| b.as_ref()).collect();
        stmt.query_row(refs.as_slice(), |row| row.get(0))?
    };

    // Fetch records
    let select_sql = format!(
        "SELECT id, created_at, domain, relation_id, object_id, object_type, business_line,
                source_lang, target_lang, status, execution_ms, worker_id, idempotency_key,
                callback_sent_at, callback_retries, fields_count, error_message,
                component_ids_json, media_mappings_count, failed_fields_count, primary_failure_reason
         FROM translation_records {}
         ORDER BY created_at DESC, id DESC
         LIMIT ?{} OFFSET ?{}",
        where_clause,
        bind_vals.len() + 1,
        bind_vals.len() + 2,
    );

    bind_vals.push(Box::new(limit as i64));
    bind_vals.push(Box::new(offset as i64));

    let mut stmt = conn.prepare(&select_sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = bind_vals.iter().map(|b| b.as_ref()).collect();
    let records = stmt
        .query_map(refs.as_slice(), |row| {
            Ok(TranslationRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                domain: row.get(2)?,
                relation_id: row.get(3)?,
                object_id: row.get(4)?,
                object_type: row.get(5)?,
                business_line: row.get(6)?,
                source_lang: row.get(7)?,
                target_lang: row.get(8)?,
                status: row.get(9)?,
                execution_ms: row.get(10)?,
                worker_id: row.get(11)?,
                idempotency_key: row.get(12)?,
                callback_sent_at: row.get(13)?,
                callback_retries: row.get(14)?,
                fields_count: row.get(15)?,
                error_message: row.get(16)?,
                component_ids: parse_component_ids_json(row.get(17)?),
                media_mappings_count: row.get(18)?,
                failed_fields_count: row.get(19)?,
                primary_failure_reason: row.get(20)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    Ok(TranslationListResult {
        records,
        total,
        page,
        limit,
    })
}

/// Delete translation records older than `max_age_days` days.
/// Returns the number of deleted rows.
pub(crate) fn prune_old_records(conn: &Connection, max_age_days: i64) -> Result<usize> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        - (max_age_days * 86400);
    let deleted = conn.execute(
        "DELETE FROM translation_records WHERE created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

/// Delete completed retry queue entries older than `max_age_days` days.
pub(crate) fn prune_old_retries(conn: &Connection, max_age_days: i64) -> Result<usize> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        - (max_age_days * 86400);
    let deleted = conn.execute(
        "DELETE FROM retry_queue WHERE status = 'done' AND created_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted)
}

// ---------------------------------------------------------------------------
// Retry queue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RetryQueueEntry {
    pub(crate) id: i64,
    pub(crate) domain: String,
    pub(crate) relation_id: i64,
    pub(crate) object_id: i64,
    pub(crate) object_type: String,
    pub(crate) business_line: String,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
    pub(crate) created_at: i64,
    pub(crate) status: String,
}

fn normalize_retry_object_type(raw: &str) -> &'static str {
    match raw {
        "term" | "taxonomy" => "taxonomy",
        "post" | "post_type" => "post_type",
        _ => "post_type",
    }
}

/// Get a single translation record by ID.
pub(crate) fn get_translation_record_by_id(
    conn: &Connection,
    id: i64,
) -> Option<TranslationRecord> {
    conn.query_row(
        "SELECT id, created_at, domain, relation_id, object_id, object_type, business_line,
                source_lang, target_lang, status, execution_ms, worker_id, idempotency_key,
                callback_sent_at, callback_retries, fields_count, error_message,
                component_ids_json, media_mappings_count, failed_fields_count, primary_failure_reason
         FROM translation_records WHERE id = ?1",
        params![id],
        |row| {
            Ok(TranslationRecord {
                id: row.get(0)?,
                created_at: row.get(1)?,
                domain: row.get(2)?,
                relation_id: row.get(3)?,
                object_id: row.get(4)?,
                object_type: row.get(5)?,
                business_line: row.get(6)?,
                source_lang: row.get(7)?,
                target_lang: row.get(8)?,
                status: row.get(9)?,
                execution_ms: row.get(10)?,
                worker_id: row.get(11)?,
                idempotency_key: row.get(12)?,
                callback_sent_at: row.get(13)?,
                callback_retries: row.get(14)?,
                fields_count: row.get(15)?,
                error_message: row.get(16)?,
                component_ids: parse_component_ids_json(row.get(17)?),
                media_mappings_count: row.get(18)?,
                failed_fields_count: row.get(19)?,
                primary_failure_reason: row.get(20)?,
            })
        },
    )
    .ok()
}

/// Insert a retry queue entry from a failed translation record (INSERT OR REPLACE).
pub(crate) fn insert_retry_queue_entry(
    conn: &Connection,
    record: &TranslationRecord,
) -> Result<()> {
    let object_type = normalize_retry_object_type(record.object_type.as_deref().unwrap_or("post"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    conn.execute(
        "INSERT OR REPLACE INTO retry_queue
         (domain, relation_id, object_id, object_type, business_line, source_lang, target_lang, created_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')",
        params![
            record.domain,
            record.relation_id.unwrap_or(0),
            record.object_id.unwrap_or(0),
            object_type,
            record.business_line.as_deref().unwrap_or(""),
            record.source_lang,
            record.target_lang,
            now,
        ],
    )?;
    Ok(())
}

/// Get pending retry entries for a specific domain + relation.
pub(crate) fn get_pending_retries(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
) -> Vec<RetryQueueEntry> {
    let mut stmt = match conn.prepare(
        "SELECT id, domain, relation_id, object_id, object_type, business_line,
                source_lang, target_lang, created_at, status
         FROM retry_queue
         WHERE domain = ?1 AND relation_id = ?2 AND status = 'pending'
         ORDER BY created_at ASC
         LIMIT 50",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![domain, relation_id], |row| {
        Ok(RetryQueueEntry {
            id: row.get(0)?,
            domain: row.get(1)?,
            relation_id: row.get(2)?,
            object_id: row.get(3)?,
            object_type: row.get(4)?,
            business_line: row.get(5)?,
            source_lang: row.get(6)?,
            target_lang: row.get(7)?,
            created_at: row.get(8)?,
            status: row.get(9)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Batch retry: insert retry queue entries for failed translation records.
/// Only records with status "failed" are eligible. Returns count of queued entries.
pub(crate) fn batch_retry_translations(conn: &Connection, ids: &[i64]) -> Result<usize> {
    let mut queued = 0usize;
    for &id in ids {
        if let Some(record) = get_translation_record_by_id(conn, id) {
            if record.status == "failed"
                && record.relation_id.is_some()
                && record.object_id.is_some()
                && insert_retry_queue_entry(conn, &record).is_ok()
            {
                queued += 1;
            }
        }
    }
    Ok(queued)
}

/// Batch delete translation records by IDs. Returns count of deleted rows.
pub(crate) fn batch_delete_translations(conn: &Connection, ids: &[i64]) -> Result<usize> {
    let mut total_deleted = 0usize;
    for chunk in ids.chunks(500) {
        let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "DELETE FROM translation_records WHERE id IN ({})",
            placeholders.join(",")
        );
        let params: Vec<Box<dyn rusqlite::ToSql>> = chunk
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
            .collect();
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let deleted = conn.execute(&sql, refs.as_slice())?;
        total_deleted += deleted;
    }
    Ok(total_deleted)
}

/// Mark a retry queue entry as done.
pub(crate) fn mark_retry_done(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<()> {
    let normalized_object_type = normalize_retry_object_type(object_type);
    conn.execute(
        "UPDATE retry_queue
         SET status = 'done'
         WHERE domain = ?1 AND relation_id = ?2 AND object_type = ?3 AND object_id = ?4",
        params![domain, relation_id, normalized_object_type, object_id],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Aggregation / Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DomainStats {
    pub(crate) domain: String,
    pub(crate) total: i64,
    pub(crate) success: i64,
    pub(crate) failed: i64,
    pub(crate) fields_total: i64,
}

pub(crate) fn stats_by_domain(conn: &Connection) -> Vec<DomainStats> {
    let mut stmt = match conn.prepare(
        "SELECT domain,
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) AS success,
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) AS failed,
                SUM(fields_count) AS fields_total
         FROM translation_records
         GROUP BY domain
         ORDER BY total DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        Ok(DomainStats {
            domain: row.get(0)?,
            total: row.get(1)?,
            success: row.get(2)?,
            failed: row.get(3)?,
            fields_total: row.get(4)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusDistribution {
    pub(crate) status: String,
    pub(crate) count: i64,
}

pub(crate) fn stats_status_distribution(conn: &Connection) -> Vec<StatusDistribution> {
    let mut stmt = match conn.prepare(
        "SELECT status, COUNT(*) AS cnt
         FROM translation_records
         GROUP BY status
         ORDER BY cnt DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        Ok(StatusDistribution {
            status: row.get(0)?,
            count: row.get(1)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DailyStats {
    pub(crate) date: String,
    pub(crate) count: i64,
    pub(crate) fields: i64,
}

pub(crate) fn stats_daily(conn: &Connection, days: i64) -> Vec<DailyStats> {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        - days * 86400;
    let mut stmt = match conn.prepare(
        "SELECT date(created_at, 'unixepoch') AS day,
                COUNT(*) AS cnt,
                SUM(fields_count) AS fields
         FROM translation_records
         WHERE created_at >= ?1
         GROUP BY day
         ORDER BY day ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(params![cutoff], |row| {
        Ok(DailyStats {
            date: row.get(0)?,
            count: row.get(1)?,
            fields: row.get(2)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

#[cfg(test)]
mod tests;
