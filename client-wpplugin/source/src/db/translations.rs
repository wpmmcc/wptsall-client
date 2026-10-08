use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
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
    let changed = conn.execute(
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
    anyhow::ensure!(
        changed == 1,
        "translation history was not committed; original result retained"
    );
    let id = conn.last_insert_rowid();
    anyhow::ensure!(
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM translation_records WHERE id=?1)",
            [id],
            |row| row.get::<_, bool>(0)
        )?,
        "translation history disappeared during save"
    );
    Ok(id)
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
    let changed = conn.execute(
        "UPDATE translation_records SET callback_sent_at = ?1, status = 'success' WHERE id = ?2",
        rusqlite::params![now, id],
    )?;
    anyhow::ensure!(changed == 1 && conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM translation_records WHERE id=?1 AND status='success' AND callback_sent_at=?2)",
        params![id,now],|row| row.get::<_,bool>(0))?,
        "translation callback history was not committed; original result retained");
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

fn parse_component_ids_json(raw: Option<String>) -> rusqlite::Result<Vec<String>> {
    raw.map(|value| {
        serde_json::from_str::<Vec<String>>(&value).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                17,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "damaged saved translation component trace",
                )),
            )
        })
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

fn row_to_translation_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranslationRecord> {
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
        component_ids: parse_component_ids_json(row.get(17)?)?,
        media_mappings_count: row.get(18)?,
        failed_fields_count: row.get(19)?,
        primary_failure_reason: row.get(20)?,
    })
}

pub(super) struct RetainedMutation<'a> {
    conn: &'a Connection,
    committed: bool,
}

impl<'a> RetainedMutation<'a> {
    pub(super) fn begin(conn: &'a Connection) -> Result<Self> {
        conn.execute_batch("SAVEPOINT checked_retained_mutation")?;
        Ok(Self {
            conn,
            committed: false,
        })
    }

    pub(super) fn commit(mut self) -> Result<()> {
        self.conn
            .execute_batch("RELEASE checked_retained_mutation")?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for RetainedMutation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.conn.execute_batch(
                "ROLLBACK TO checked_retained_mutation; RELEASE checked_retained_mutation",
            );
        }
    }
}

/// A prior translation record is only safe for dedup short-circuit when it
/// carries evidence that a real translation/callback path already happened.
///
/// Historical `NoChanges` runs used to be written as `status=success` with
/// zero translated fields and no component trace, which would permanently
/// suppress later real executions. Treat those rows as non-materialized.
#[cfg(test)]
pub(crate) fn has_materialized_success_record(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_id: i64,
    object_type: &str,
) -> bool {
    has_materialized_success_record_checked(conn, domain, relation_id, object_id, object_type)
        .expect("valid owned translation history fixture")
}

pub(crate) fn has_materialized_success_record_checked(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_id: i64,
    object_type: &str,
) -> Result<bool> {
    Ok(conn
        .query_row(
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
        .optional()?
        .is_some())
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
    let offset = i64::from(page - 1) * i64::from(limit);

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
        .query_map(refs.as_slice(), row_to_translation_record)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

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

fn normalize_retry_object_type(raw: &str) -> &str {
    match raw {
        "term" | "taxonomy" => "taxonomy",
        "post" | "post_type" => "post_type",
        other if !other.is_empty() => other,
        _ => "post_type",
    }
}

/// Get a single translation record by ID.
pub(crate) fn get_translation_record_by_id(
    conn: &Connection,
    id: i64,
) -> Result<Option<TranslationRecord>> {
    Ok(conn.query_row(
        "SELECT id, created_at, domain, relation_id, object_id, object_type, business_line,
                source_lang, target_lang, status, execution_ms, worker_id, idempotency_key,
                callback_sent_at, callback_retries, fields_count, error_message,
                component_ids_json, media_mappings_count, failed_fields_count, primary_failure_reason
         FROM translation_records WHERE id = ?1",
        params![id],
        row_to_translation_record,
    ).optional()?)
}

/// Queue new work without replacing a pending retry's retained identity.
pub(crate) fn insert_retry_queue_entry(
    conn: &Connection,
    record: &TranslationRecord,
) -> Result<bool> {
    let mutation = RetainedMutation::begin(conn)?;
    let object_type = normalize_retry_object_type(record.object_type.as_deref().unwrap_or("post"));
    let relation = record.relation_id.filter(|id| *id > 0).ok_or_else(|| {
        anyhow::anyhow!("retry record has no positive relation identity; retained")
    })?;
    let object = record
        .object_id
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow::anyhow!("retry record has no positive object identity; retained"))?;
    let existing = conn
        .query_row(
            "SELECT id, domain, relation_id, object_id, object_type, business_line,
         source_lang, target_lang, created_at, status FROM retry_queue
         WHERE domain=?1 AND relation_id=?2 AND object_id=?3 AND object_type=?4",
            params![record.domain, relation, object, object_type],
            row_to_retry_entry,
        )
        .optional()?;
    if let Some(existing) = existing {
        anyhow::ensure!(
            matches!(existing.status.as_str(), "pending" | "done"),
            "unknown retry status; original retry retained"
        );
        if existing.status == "pending" {
            anyhow::ensure!(
                existing.business_line == record.business_line.as_deref().unwrap_or("")
                    && existing.source_lang == record.source_lang
                    && existing.target_lang == record.target_lang,
                "unfinished retry scope differs; original retry retained"
            );
            mutation.commit()?;
            return Ok(false);
        }
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let changed = conn.execute(
        "INSERT INTO retry_queue
         (domain, relation_id, object_id, object_type, business_line, source_lang, target_lang, created_at, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')
         ON CONFLICT(domain, relation_id, object_type, object_id) DO UPDATE SET
             business_line=excluded.business_line, source_lang=excluded.source_lang,
             target_lang=excluded.target_lang, created_at=excluded.created_at, status='pending'
         WHERE retry_queue.status='done'",
        params![
            record.domain,
            relation,
            object,
            object_type,
            record.business_line.as_deref().unwrap_or(""),
            record.source_lang,
            record.target_lang,
            now,
        ],
    )?;
    anyhow::ensure!(
        changed == 1
            && conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM retry_queue WHERE domain=?1 AND relation_id=?2
         AND object_id=?3 AND object_type=?4 AND business_line=?5
         AND source_lang=?6 AND target_lang=?7 AND created_at=?8 AND status='pending')",
                params![
                    record.domain,
                    relation,
                    object,
                    object_type,
                    record.business_line.as_deref().unwrap_or(""),
                    record.source_lang,
                    record.target_lang,
                    now
                ],
                |row| row.get::<_, bool>(0)
            )?,
        "retry enqueue was not confirmed; original state retained"
    );
    mutation.commit()?;
    Ok(true)
}

fn row_to_retry_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<RetryQueueEntry> {
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
}

/// Get pending retry entries for a specific domain + relation.
pub(crate) fn get_pending_retries(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
) -> Result<Vec<RetryQueueEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, domain, relation_id, object_id, object_type, business_line,
                source_lang, target_lang, created_at, status
         FROM retry_queue
         WHERE domain = ?1 AND relation_id = ?2 AND status = 'pending'
         ORDER BY created_at ASC
         LIMIT 50",
    )?;
    let rows = stmt.query_map(params![domain, relation_id], row_to_retry_entry)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Batch retry: insert retry queue entries for failed translation records.
/// Only records with status "failed" are eligible. Returns count of queued entries.
pub(crate) fn batch_retry_translations(conn: &Connection, ids: &[i64]) -> Result<usize> {
    anyhow::ensure!(
        ids.iter().all(|id| *id > 0),
        "retry IDs must be positive; original state retained"
    );
    let mutation = RetainedMutation::begin(conn)?;
    let mut queued = 0usize;
    for &id in ids {
        if let Some(record) = get_translation_record_by_id(conn, id)? {
            if record.status == "failed"
                && record.relation_id.is_some()
                && record.object_id.is_some()
            {
                if insert_retry_queue_entry(conn, &record)? {
                    queued += 1;
                }
            }
        }
    }
    mutation.commit()?;
    Ok(queued)
}

/// Batch delete translation records by IDs. Returns count of deleted rows.
pub(crate) fn batch_delete_translations(conn: &Connection, ids: &[i64]) -> Result<usize> {
    anyhow::ensure!(
        ids.iter().all(|id| *id > 0),
        "delete IDs must be positive; original history retained"
    );
    let mutation = RetainedMutation::begin(conn)?;
    let ids: Vec<_> = ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut total_deleted = 0usize;
    for chunk in ids.chunks(500) {
        let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
        let predicate = format!("id IN ({})", placeholders.join(","));
        let sql = format!("DELETE FROM translation_records WHERE {predicate}");
        let params: Vec<Box<dyn rusqlite::ToSql>> = chunk
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::ToSql>)
            .collect();
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let expected: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM translation_records WHERE {predicate}"),
            refs.as_slice(),
            |row| row.get(0),
        )?;
        let deleted = conn.execute(&sql, refs.as_slice())?;
        anyhow::ensure!(
            i64::try_from(deleted)? == expected,
            "history deletion was not confirmed; original history retained"
        );
        let remains: bool = conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM translation_records WHERE {predicate})"),
            refs.as_slice(),
            |row| row.get(0),
        )?;
        anyhow::ensure!(
            !remains,
            "history deletion readback disagrees; original history retained"
        );
        total_deleted = total_deleted.checked_add(deleted).ok_or_else(|| {
            anyhow::anyhow!("history delete count overflow; original history retained")
        })?;
    }
    mutation.commit()?;
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
    let mutation = RetainedMutation::begin(conn)?;
    let normalized_object_type = normalize_retry_object_type(object_type);
    let original: Option<String> = conn
        .query_row(
            "SELECT status FROM retry_queue WHERE domain=?1 AND relation_id=?2
         AND object_type=?3 AND object_id=?4",
            params![domain, relation_id, normalized_object_type, object_id],
            |row| row.get(0),
        )
        .optional()?;
    if original.is_none() || original.as_deref() == Some("done") {
        return mutation.commit();
    }
    anyhow::ensure!(
        original.as_deref() == Some("pending"),
        "unknown retry status; original retry retained"
    );
    let changed = conn.execute(
        "UPDATE retry_queue
         SET status = 'done'
         WHERE domain = ?1 AND relation_id = ?2 AND object_type = ?3 AND object_id = ?4 AND status='pending'",
        params![domain, relation_id, normalized_object_type, object_id],
    )?;
    anyhow::ensure!(
        changed == 1
            && conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM retry_queue WHERE domain=?1 AND relation_id=?2
         AND object_type=?3 AND object_id=?4 AND status='done')",
                params![domain, relation_id, normalized_object_type, object_id],
                |row| row.get::<_, bool>(0)
            )?,
        "retry completion was not confirmed; original retry retained"
    );
    mutation.commit()
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

pub(crate) fn stats_by_domain(conn: &Connection) -> Result<Vec<DomainStats>> {
    let mut stmt = conn.prepare(
        "SELECT domain,
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) AS success,
                SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) AS failed,
                COALESCE(SUM(fields_count), 0) AS fields_total
         FROM translation_records
         GROUP BY domain
         ORDER BY total DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(DomainStats {
            domain: row.get(0)?,
            total: row.get(1)?,
            success: row.get(2)?,
            failed: row.get(3)?,
            fields_total: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusDistribution {
    pub(crate) status: String,
    pub(crate) count: i64,
}

pub(crate) fn stats_status_distribution(conn: &Connection) -> Result<Vec<StatusDistribution>> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) AS cnt
         FROM translation_records
         GROUP BY status
         ORDER BY cnt DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(StatusDistribution {
            status: row.get(0)?,
            count: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DailyStats {
    pub(crate) date: String,
    pub(crate) count: i64,
    pub(crate) fields: i64,
}

pub(crate) fn stats_daily(conn: &Connection, days: i64) -> Result<Vec<DailyStats>> {
    anyhow::ensure!(days > 0, "statistics window must be positive");
    let seconds = days
        .checked_mul(86400)
        .ok_or_else(|| anyhow::anyhow!("statistics window overflow"))?;
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
        - seconds;
    let mut stmt = conn.prepare(
        "SELECT date(created_at, 'unixepoch') AS day,
                COUNT(*) AS cnt,
                COALESCE(SUM(fields_count), 0) AS fields
         FROM translation_records
         WHERE created_at >= ?1
         GROUP BY day
         ORDER BY day ASC",
    )?;
    let rows = stmt.query_map(params![cutoff], |row| {
        Ok(DailyStats {
            date: row.get(0)?,
            count: row.get(1)?,
            fields: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
#[cfg(test)]
mod tests;