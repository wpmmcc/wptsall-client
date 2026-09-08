//! Discovery task configuration and in-progress dedup for concurrent translation pipelines.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::Value;

use crate::logging::unix_ts;

// ---------------------------------------------------------------------------
// Default constants
// ---------------------------------------------------------------------------

pub(crate) const DEFAULT_CONCURRENCY: i64 = 4;
pub(crate) const DEFAULT_BATCH_PARALLEL: i64 = 1;
pub(crate) const DEFAULT_PER_PAGE: i64 = 20;
pub(crate) const DEFAULT_RETRY_MAX: i64 = 3;
pub(crate) const DEFAULT_TIMEOUT_SECS: i64 = 60;

// ---------------------------------------------------------------------------
// Structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct DiscoveryTaskParams {
    pub(crate) concurrency: i64,
    pub(crate) batch_parallel: i64,
    pub(crate) per_page: i64,
    pub(crate) retry_max: i64,
    pub(crate) timeout_secs: i64,
    pub(crate) enabled: bool,
    pub(crate) include_resync: bool,
    pub(crate) selected_component_id: Option<String>,
    pub(crate) effective_source_lang: Option<String>,
    pub(crate) effective_target_lang: Option<String>,
    pub(crate) editable_overrides: Option<Value>,
}

impl Default for DiscoveryTaskParams {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            batch_parallel: DEFAULT_BATCH_PARALLEL,
            per_page: DEFAULT_PER_PAGE,
            retry_max: DEFAULT_RETRY_MAX,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            enabled: true,
            include_resync: true,
            selected_component_id: None,
            effective_source_lang: None,
            effective_target_lang: None,
            editable_overrides: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DiscoveryTask {
    pub(crate) id: i64,
    pub(crate) domain: String,
    pub(crate) relation_id: i64,
    pub(crate) concurrency: i64,
    pub(crate) batch_parallel: i64,
    pub(crate) per_page: i64,
    pub(crate) retry_max: i64,
    pub(crate) timeout_secs: i64,
    pub(crate) enabled: bool,
    pub(crate) include_resync: bool,
    pub(crate) selected_component_id: Option<String>,
    pub(crate) effective_source_lang: Option<String>,
    pub(crate) effective_target_lang: Option<String>,
    pub(crate) editable_overrides: Option<Value>,
    pub(crate) last_run_at: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct UpdateDiscoveryTaskRequest {
    pub(crate) concurrency: Option<i64>,
    pub(crate) batch_parallel: Option<i64>,
    pub(crate) per_page: Option<i64>,
    pub(crate) retry_max: Option<i64>,
    pub(crate) timeout_secs: Option<i64>,
    pub(crate) enabled: Option<bool>,
    pub(crate) include_resync: Option<bool>,
    pub(crate) selected_component_id: Option<String>,
    pub(crate) effective_source_lang: Option<String>,
    pub(crate) effective_target_lang: Option<String>,
    pub(crate) editable_overrides: Option<Value>,
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Ensure tasks exist (INSERT OR IGNORE)
// ---------------------------------------------------------------------------

/// Auto-create a discovery_tasks row for each (domain, relation_id) if not present.
pub(crate) fn ensure_discovery_tasks(
    conn: &Connection,
    domain: &str,
    relation_ids: &[i64],
) -> Result<()> {
    let now = unix_ts() as i64;
    for &relation_id in relation_ids {
        conn.execute(
            "INSERT OR IGNORE INTO discovery_tasks
	             (domain, relation_id, concurrency, batch_parallel, per_page, retry_max, timeout_secs,
	              enabled, include_resync, last_run_at, created_at, updated_at)
	             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, 1, 0, ?8, ?8)",
            params![
                domain,
                relation_id,
                DEFAULT_CONCURRENCY,
                DEFAULT_BATCH_PARALLEL,
                DEFAULT_PER_PAGE,
                DEFAULT_RETRY_MAX,
                DEFAULT_TIMEOUT_SECS,
                now,
            ],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Read params
// ---------------------------------------------------------------------------

/// Get task params for a specific (domain, relation_id). Returns defaults if not found.
pub(crate) fn get_discovery_task_params(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
) -> DiscoveryTaskParams {
    conn.query_row(
        "SELECT concurrency, batch_parallel, per_page, retry_max, timeout_secs, enabled, include_resync,
                selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json
         FROM discovery_tasks WHERE domain = ?1 AND relation_id = ?2",
        params![domain, relation_id],
        |row| {
            Ok(DiscoveryTaskParams {
                concurrency: row.get::<_, i64>(0).unwrap_or(DEFAULT_CONCURRENCY),
                batch_parallel: row.get::<_, i64>(1).unwrap_or(DEFAULT_BATCH_PARALLEL),
                per_page: row.get::<_, i64>(2).unwrap_or(DEFAULT_PER_PAGE),
                retry_max: row.get::<_, i64>(3).unwrap_or(DEFAULT_RETRY_MAX),
                timeout_secs: row.get::<_, i64>(4).unwrap_or(DEFAULT_TIMEOUT_SECS),
                enabled: row.get::<_, i64>(5).unwrap_or(1) != 0,
                include_resync: row.get::<_, i64>(6).unwrap_or(0) != 0,
                selected_component_id: normalize_optional_text(
                    row.get::<_, Option<String>>(7).ok().flatten().as_deref(),
                ),
                effective_source_lang: normalize_optional_text(
                    row.get::<_, Option<String>>(8).ok().flatten().as_deref(),
                ),
                effective_target_lang: normalize_optional_text(
                    row.get::<_, Option<String>>(9).ok().flatten().as_deref(),
                ),
                editable_overrides: row
                    .get::<_, Option<String>>(10)
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok()),
            })
        },
    )
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// List all discovery tasks
// ---------------------------------------------------------------------------

pub(crate) fn list_discovery_tasks(conn: &Connection) -> Vec<DiscoveryTask> {
    let mut stmt = match conn.prepare(
        "SELECT id, domain, relation_id, concurrency, batch_parallel, per_page,
                retry_max, timeout_secs, enabled, include_resync,
                selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                last_run_at, created_at, updated_at
         FROM discovery_tasks ORDER BY domain ASC, relation_id ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        Ok(DiscoveryTask {
            id: row.get(0)?,
            domain: row.get(1)?,
            relation_id: row.get(2)?,
            concurrency: row.get(3)?,
            batch_parallel: row.get(4)?,
            per_page: row.get(5)?,
            retry_max: row.get(6)?,
            timeout_secs: row.get(7)?,
            enabled: row.get::<_, i64>(8)? != 0,
            include_resync: row.get::<_, i64>(9)? != 0,
            selected_component_id: normalize_optional_text(
                row.get::<_, Option<String>>(10)?.as_deref(),
            ),
            effective_source_lang: normalize_optional_text(
                row.get::<_, Option<String>>(11)?.as_deref(),
            ),
            effective_target_lang: normalize_optional_text(
                row.get::<_, Option<String>>(12)?.as_deref(),
            ),
            editable_overrides: row
                .get::<_, Option<String>>(13)?
                .and_then(|s| serde_json::from_str(&s).ok()),
            last_run_at: row.get(14)?,
            created_at: row.get(15)?,
            updated_at: row.get(16)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub(crate) fn get_discovery_task_by_id(conn: &Connection, id: i64) -> Option<DiscoveryTask> {
    conn.query_row(
        "SELECT id, domain, relation_id, concurrency, batch_parallel, per_page,
                retry_max, timeout_secs, enabled, include_resync,
                selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                last_run_at, created_at, updated_at
         FROM discovery_tasks WHERE id = ?1",
        params![id],
        |row| {
            Ok(DiscoveryTask {
                id: row.get(0)?,
                domain: row.get(1)?,
                relation_id: row.get(2)?,
                concurrency: row.get(3)?,
                batch_parallel: row.get(4)?,
                per_page: row.get(5)?,
                retry_max: row.get(6)?,
                timeout_secs: row.get(7)?,
                enabled: row.get::<_, i64>(8)? != 0,
                include_resync: row.get::<_, i64>(9)? != 0,
                selected_component_id: normalize_optional_text(
                    row.get::<_, Option<String>>(10)?.as_deref(),
                ),
                effective_source_lang: normalize_optional_text(
                    row.get::<_, Option<String>>(11)?.as_deref(),
                ),
                effective_target_lang: normalize_optional_text(
                    row.get::<_, Option<String>>(12)?.as_deref(),
                ),
                editable_overrides: row
                    .get::<_, Option<String>>(13)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                last_run_at: row.get(14)?,
                created_at: row.get(15)?,
                updated_at: row.get(16)?,
            })
        },
    )
    .ok()
}

// ---------------------------------------------------------------------------
// Update a discovery task
// ---------------------------------------------------------------------------

pub(crate) fn update_discovery_task(
    conn: &Connection,
    id: i64,
    req: &UpdateDiscoveryTaskRequest,
) -> Result<()> {
    let now = unix_ts() as i64;
    if let Some(v) = req.concurrency {
        conn.execute(
            "UPDATE discovery_tasks SET concurrency = ?1, updated_at = ?2 WHERE id = ?3",
            params![v.max(1), now, id],
        )?;
    }
    if let Some(v) = req.batch_parallel {
        conn.execute(
            "UPDATE discovery_tasks SET batch_parallel = ?1, updated_at = ?2 WHERE id = ?3",
            params![v.max(1), now, id],
        )?;
    }
    if let Some(v) = req.per_page {
        conn.execute(
            "UPDATE discovery_tasks SET per_page = ?1, updated_at = ?2 WHERE id = ?3",
            params![v.max(1), now, id],
        )?;
    }
    if let Some(v) = req.retry_max {
        conn.execute(
            "UPDATE discovery_tasks SET retry_max = ?1, updated_at = ?2 WHERE id = ?3",
            params![v.max(0), now, id],
        )?;
    }
    if let Some(v) = req.timeout_secs {
        conn.execute(
            "UPDATE discovery_tasks SET timeout_secs = ?1, updated_at = ?2 WHERE id = ?3",
            params![v.max(5), now, id],
        )?;
    }
    if let Some(v) = req.enabled {
        conn.execute(
            "UPDATE discovery_tasks SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
            params![if v { 1i64 } else { 0i64 }, now, id],
        )?;
    }
    if let Some(v) = req.include_resync {
        conn.execute(
            "UPDATE discovery_tasks SET include_resync = ?1, updated_at = ?2 WHERE id = ?3",
            params![if v { 1i64 } else { 0i64 }, now, id],
        )?;
    }
    if req.selected_component_id.is_some() {
        let normalized = normalize_optional_text(req.selected_component_id.as_deref());
        conn.execute(
            "UPDATE discovery_tasks SET selected_component_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![normalized, now, id],
        )?;
    }
    if req.effective_source_lang.is_some() {
        let normalized = normalize_optional_text(req.effective_source_lang.as_deref());
        conn.execute(
            "UPDATE discovery_tasks SET effective_source_lang = ?1, updated_at = ?2 WHERE id = ?3",
            params![normalized, now, id],
        )?;
    }
    if req.effective_target_lang.is_some() {
        let normalized = normalize_optional_text(req.effective_target_lang.as_deref());
        conn.execute(
            "UPDATE discovery_tasks SET effective_target_lang = ?1, updated_at = ?2 WHERE id = ?3",
            params![normalized, now, id],
        )?;
    }
    if let Some(v) = req.editable_overrides.as_ref() {
        let serialized = serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string());
        conn.execute(
            "UPDATE discovery_tasks SET editable_overrides_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![serialized, now, id],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Update last_run_at
// ---------------------------------------------------------------------------

pub(crate) fn touch_last_run_at(conn: &Connection, domain: &str, relation_id: i64) {
    let now = unix_ts() as i64;
    let _ = conn.execute(
        "UPDATE discovery_tasks SET last_run_at = ?1 WHERE domain = ?2 AND relation_id = ?3",
        params![now, domain, relation_id],
    );
}

// ---------------------------------------------------------------------------
// In-progress dedup
// ---------------------------------------------------------------------------

/// Try to claim an object for in-progress translation. Returns true if claimed,
/// false if already claimed by another pipeline.
pub(crate) fn try_claim_in_progress(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> bool {
    let normalized_object_type = normalize_object_type_key(object_type);
    let now = unix_ts() as i64;
    conn.execute(
        "INSERT OR IGNORE INTO translation_in_progress (domain, relation_id, object_type, object_id, claimed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![domain, relation_id, normalized_object_type, object_id, now],
    )
    .map(|affected| affected > 0)
    .unwrap_or(false) // On error, do NOT claim — conservative: skip rather than double-translate
}

/// Release an in-progress claim.
pub(crate) fn release_in_progress(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) {
    let normalized_object_type = normalize_object_type_key(object_type);
    let _ = conn.execute(
        "DELETE FROM translation_in_progress
         WHERE domain = ?1 AND relation_id = ?2 AND object_type = ?3 AND object_id = ?4",
        params![domain, relation_id, normalized_object_type, object_id],
    );
}

/// Clean up expired in-progress entries (older than max_age_secs).
pub(crate) fn cleanup_expired_in_progress(conn: &Connection, max_age_secs: i64) {
    let cutoff = unix_ts() as i64 - max_age_secs;
    let _ = conn.execute(
        "DELETE FROM translation_in_progress WHERE claimed_at < ?1",
        params![cutoff],
    );
}

fn normalize_object_type_key(raw: &str) -> &str {
    match raw {
        "term" | "taxonomy" => "taxonomy",
        "post" | "post_type" => "post_type",
        other if !other.is_empty() => other,
        _ => "post_type",
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_db;

    fn make_db() -> Connection {
        let conn = open_db(":memory:").expect("in-memory db");
        conn
    }

    #[test]
    fn ensure_creates_rows() {
        let conn = make_db();
        ensure_discovery_tasks(&conn, "https://example.com", &[1, 2, 3]).unwrap();
        let tasks = list_discovery_tasks(&conn);
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn ensure_is_idempotent() {
        let conn = make_db();
        ensure_discovery_tasks(&conn, "https://example.com", &[1]).unwrap();
        ensure_discovery_tasks(&conn, "https://example.com", &[1]).unwrap();
        let tasks = list_discovery_tasks(&conn);
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn get_params_returns_defaults_when_missing() {
        let conn = make_db();
        let params = get_discovery_task_params(&conn, "https://example.com", 99);
        assert_eq!(params.concurrency, DEFAULT_CONCURRENCY);
        assert_eq!(params.per_page, DEFAULT_PER_PAGE);
        assert!(params.enabled);
        assert!(params.include_resync);
        assert!(params.selected_component_id.is_none());
        assert!(params.effective_source_lang.is_none());
        assert!(params.effective_target_lang.is_none());
        assert!(params.editable_overrides.is_none());
    }

    #[test]
    fn update_task_persists() {
        let conn = make_db();
        ensure_discovery_tasks(&conn, "https://example.com", &[1]).unwrap();
        let tasks = list_discovery_tasks(&conn);
        let id = tasks[0].id;
        update_discovery_task(
            &conn,
            id,
            &UpdateDiscoveryTaskRequest {
                concurrency: Some(5),
                batch_parallel: Some(2),
                per_page: None,
                retry_max: None,
                timeout_secs: None,
                enabled: Some(false),
                include_resync: None,
                selected_component_id: Some("comp-text".to_string()),
                effective_source_lang: Some("zh_CN".to_string()),
                effective_target_lang: Some("en_US".to_string()),
                editable_overrides: Some(serde_json::json!({
                    "default_values.system_prompt": "Translate for WooCommerce"
                })),
            },
        )
        .unwrap();
        let params = get_discovery_task_params(&conn, "https://example.com", 1);
        assert_eq!(params.concurrency, 5);
        assert_eq!(params.batch_parallel, 2);
        assert!(!params.enabled);
        // include_resync was None → should keep the current default (true)
        assert!(params.include_resync);
        assert_eq!(params.selected_component_id.as_deref(), Some("comp-text"));
        assert_eq!(params.effective_source_lang.as_deref(), Some("zh_CN"));
        assert_eq!(params.effective_target_lang.as_deref(), Some("en_US"));
        assert_eq!(
            params
                .editable_overrides
                .as_ref()
                .and_then(|v| v.get("default_values.system_prompt"))
                .and_then(|v| v.as_str()),
            Some("Translate for WooCommerce")
        );
    }

    #[test]
    fn update_include_resync_persists() {
        let conn = make_db();
        ensure_discovery_tasks(&conn, "https://example.com", &[1]).unwrap();
        let tasks = list_discovery_tasks(&conn);
        let id = tasks[0].id;
        // Enable include_resync
        update_discovery_task(
            &conn,
            id,
            &UpdateDiscoveryTaskRequest {
                concurrency: None,
                batch_parallel: None,
                per_page: None,
                retry_max: None,
                timeout_secs: None,
                enabled: None,
                include_resync: Some(true),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
            },
        )
        .unwrap();
        let params = get_discovery_task_params(&conn, "https://example.com", 1);
        assert!(params.include_resync);
        // Also verify list_discovery_tasks reflects the change
        let tasks2 = list_discovery_tasks(&conn);
        assert!(tasks2[0].include_resync);
        // Disable it again
        update_discovery_task(
            &conn,
            id,
            &UpdateDiscoveryTaskRequest {
                concurrency: None,
                batch_parallel: None,
                per_page: None,
                retry_max: None,
                timeout_secs: None,
                enabled: None,
                include_resync: Some(false),
                selected_component_id: None,
                effective_source_lang: None,
                effective_target_lang: None,
                editable_overrides: None,
            },
        )
        .unwrap();
        let params2 = get_discovery_task_params(&conn, "https://example.com", 1);
        assert!(!params2.include_resync);
    }

    #[test]
    fn try_claim_and_release() {
        let conn = make_db();
        let claimed = try_claim_in_progress(&conn, "https://example.com", 1, "post_type", 100);
        assert!(claimed);
        // Second claim should fail
        let claimed2 = try_claim_in_progress(&conn, "https://example.com", 1, "post_type", 100);
        assert!(!claimed2);
        // Release
        release_in_progress(&conn, "https://example.com", 1, "post_type", 100);
        // Should be claimable again
        let claimed3 = try_claim_in_progress(&conn, "https://example.com", 1, "post_type", 100);
        assert!(claimed3);
        release_in_progress(&conn, "https://example.com", 1, "post_type", 100);
    }

    #[test]
    fn cleanup_expired() {
        let conn = make_db();
        // Insert an "old" entry directly
        conn.execute(
            "INSERT OR IGNORE INTO translation_in_progress (domain, relation_id, object_type, object_id, claimed_at)
             VALUES ('x', 1, 'post_type', 42, 0)",
            [],
        )
        .unwrap();
        cleanup_expired_in_progress(&conn, 60);
        // Should be gone (claimed_at=0 is ancient)
        let claimed = try_claim_in_progress(&conn, "x", 1, "post_type", 42);
        assert!(claimed);
        release_in_progress(&conn, "x", 1, "post_type", 42);
    }

    /// Verifies that the UNIQUE constraint on translation_in_progress prevents two workers
    /// from claiming the same (domain, relation_id, object_type, object_id)
    /// tuple simultaneously.
    /// Only the first INSERT succeeds; the second is rejected by the unique constraint,
    /// and try_claim_in_progress returns false (conservative skip, not double-translate).
    #[test]
    fn test_unique_claim_concurrent() {
        let conn = make_db();
        let domain = "https://example.com";
        let relation_id = 7_i64;
        let object_id = 999_i64;

        // First claim — should succeed
        let first = try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id);
        assert!(first, "first claim must succeed");

        // Second claim for identical key — unique constraint must reject it
        let second = try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id);
        assert!(!second, "second claim must fail due to unique constraint");

        // A different object_id on the same relation should still be claimable
        let other_object =
            try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id + 1);
        assert!(
            other_object,
            "a different object_id must be claimable independently"
        );

        // Same object_id but different object_type should also be claimable.
        let taxonomy_object =
            try_claim_in_progress(&conn, domain, relation_id, "taxonomy", object_id);
        assert!(
            taxonomy_object,
            "same object_id should not collide across object_type"
        );

        // After releasing the original, it should become claimable again
        release_in_progress(&conn, domain, relation_id, "post_type", object_id);
        let after_release =
            try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id);
        assert!(after_release, "claim must succeed after release");

        release_in_progress(&conn, domain, relation_id, "post_type", object_id);
        release_in_progress(&conn, domain, relation_id, "post_type", object_id + 1);
        release_in_progress(&conn, domain, relation_id, "taxonomy", object_id);
    }
}
