//! Discovery task configuration and in-progress dedup for concurrent translation pipelines.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::logging::unix_ts;

#[cfg(test)]
#[path = "../../../../tests/modules/client-wpplugin/unit/physical_discovery_tasks_capacity.rs"]
mod physical_discovery_tasks_capacity;

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

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
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

#[derive(Debug, Default, serde::Deserialize)]
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
    crate::storage_capacity::with_database_credit(None, || {
        let mutation = super::translations::RetainedMutation::begin(conn)?;
        anyhow::ensure!(
            !domain.trim().is_empty(),
            "discovery domain is missing; original settings retained"
        );
        let now = unix_ts() as i64;
        for &relation_id in relation_ids {
            anyhow::ensure!(
                relation_id > 0,
                "discovery relation identity must be positive"
            );
            let existing = conn
                .query_row(
                    "SELECT id FROM discovery_tasks WHERE domain=?1 AND relation_id=?2",
                    params![domain, relation_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(id) = existing {
                get_discovery_task_by_id(conn, id)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "discovery task projection disappeared; original settings retained"
                    )
                })?;
                continue;
            }
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
            let confirmed = conn
                .query_row(
                    "SELECT id FROM discovery_tasks WHERE domain=?1 AND relation_id=?2",
                    params![domain, relation_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let id = confirmed.ok_or_else(|| {
                anyhow::anyhow!(
                    "discovery task initialization was not confirmed; original settings retained"
                )
            })?;
            get_discovery_task_by_id(conn, id)?.ok_or_else(|| {
                anyhow::anyhow!("discovery task projection disappeared; original settings retained")
            })?;
        }
        mutation.commit()
    })
}

// ---------------------------------------------------------------------------
// Read params
// ---------------------------------------------------------------------------

/// Get task params for a specific (domain, relation_id). Returns defaults if not found.
pub(crate) fn get_discovery_task_params(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
) -> Result<DiscoveryTaskParams> {
    let task = conn.query_row(
        "SELECT concurrency, batch_parallel, per_page, retry_max, timeout_secs, enabled, include_resync,
                selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json
         FROM discovery_tasks WHERE domain = ?1 AND relation_id = ?2",
        params![domain, relation_id],
        |row| {
            Ok(DiscoveryTaskParams {
                concurrency: row.get(0)?,
                batch_parallel: row.get(1)?,
                per_page: row.get(2)?,
                retry_max: row.get(3)?,
                timeout_secs: row.get(4)?,
                enabled: read_policy_bool(row, 5)?,
                include_resync: read_policy_bool(row, 6)?,
                selected_component_id: normalize_optional_text(
                    row.get::<_, Option<String>>(7)?.as_deref(),
                ),
                effective_source_lang: normalize_optional_text(
                    row.get::<_, Option<String>>(8)?.as_deref(),
                ),
                effective_target_lang: normalize_optional_text(
                    row.get::<_, Option<String>>(9)?.as_deref(),
                ),
                editable_overrides: row.get::<_, Option<String>>(10)?
                    .map(|s| read_policy_overrides(&s, 10)).transpose()?,
            })
        },
    )
    .optional()?;
    let task = task.unwrap_or_default();
    anyhow::ensure!(
        task.concurrency > 0
            && task.batch_parallel > 0
            && task.per_page > 0
            && task.retry_max >= 0
            && task.timeout_secs > 0,
        "invalid discovery task limits; stored policy retained"
    );
    Ok(task)
}

fn read_policy_bool(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<bool> {
    match row.get::<_, i64>(index)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid policy boolean",
            )),
        )),
    }
}

fn read_policy_overrides(raw: &str, index: usize) -> rusqlite::Result<Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "damaged saved discovery policy",
                )),
            )
        })
}

fn row_to_discovery_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<DiscoveryTask> {
    let task = DiscoveryTask {
        id: row.get(0)?,
        domain: row.get(1)?,
        relation_id: row.get(2)?,
        concurrency: row.get(3)?,
        batch_parallel: row.get(4)?,
        per_page: row.get(5)?,
        retry_max: row.get(6)?,
        timeout_secs: row.get(7)?,
        enabled: read_policy_bool(row, 8)?,
        include_resync: read_policy_bool(row, 9)?,
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
            .map(|raw| read_policy_overrides(&raw, 13))
            .transpose()?,
        last_run_at: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    };
    if task.concurrency <= 0
        || task.batch_parallel <= 0
        || task.per_page <= 0
        || task.retry_max < 0
        || task.timeout_secs <= 0
    {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid saved discovery limits",
            )),
        ));
    }
    Ok(task)
}

/// FL-9 claim guard: component ids that are the explicit selection of a
/// discovery task belonging to a DIFFERENT (domain, relation_id). The
/// anonymous component-selection fallback tier excludes these ids so an
/// unbound task can never silently execute on another relation's component
/// (e.g. a component whose provider was already stopped). Selections of
/// disabled tasks still count as claimed: the selection expresses relation
/// ownership regardless of whether the task currently runs.
pub(crate) fn selected_components_claimed_by_other_relations(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT selected_component_id FROM discovery_tasks
         WHERE selected_component_id IS NOT NULL
           AND TRIM(selected_component_id) != ''
           AND NOT (domain = ?1 AND relation_id = ?2)",
    )?;
    let rows = stmt.query_map(params![domain, relation_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
}

// ---------------------------------------------------------------------------
// List all discovery tasks
// ---------------------------------------------------------------------------

pub(crate) fn list_discovery_tasks(conn: &Connection) -> Result<Vec<DiscoveryTask>> {
    let mut stmt = conn.prepare(
        "SELECT id, domain, relation_id, concurrency, batch_parallel, per_page,
                retry_max, timeout_secs, enabled, include_resync,
                selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                last_run_at, created_at, updated_at
         FROM discovery_tasks ORDER BY domain ASC, relation_id ASC",
    )?;
    let rows = stmt.query_map([], row_to_discovery_task)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn get_discovery_task_by_id(
    conn: &Connection,
    id: i64,
) -> Result<Option<DiscoveryTask>> {
    Ok(conn.query_row(
        "SELECT id, domain, relation_id, concurrency, batch_parallel, per_page,
                retry_max, timeout_secs, enabled, include_resync,
                selected_component_id, effective_source_lang, effective_target_lang, editable_overrides_json,
                last_run_at, created_at, updated_at
         FROM discovery_tasks WHERE id = ?1",
        params![id],
        row_to_discovery_task,
    ).optional()?)
}

// ---------------------------------------------------------------------------
// Update a discovery task
// ---------------------------------------------------------------------------

pub(crate) fn update_discovery_task(
    conn: &Connection,
    id: i64,
    req: &UpdateDiscoveryTaskRequest,
) -> Result<()> {
    let mutation = super::translations::RetainedMutation::begin(conn)?;
    let mut expected = get_discovery_task_by_id(conn, id)?
        .ok_or_else(|| anyhow::anyhow!("discovery task not found; no configuration changed"))?;
    if let Some(v) = req.concurrency {
        expected.concurrency = v.max(1);
    }
    if let Some(v) = req.batch_parallel {
        expected.batch_parallel = v.max(1);
    }
    if let Some(v) = req.per_page {
        expected.per_page = v.max(1);
    }
    if let Some(v) = req.retry_max {
        expected.retry_max = v.max(0);
    }
    if let Some(v) = req.timeout_secs {
        expected.timeout_secs = v.max(5);
    }
    if let Some(v) = req.enabled {
        expected.enabled = v;
    }
    if let Some(v) = req.include_resync {
        expected.include_resync = v;
    }
    if req.selected_component_id.is_some() {
        expected.selected_component_id =
            normalize_optional_text(req.selected_component_id.as_deref());
    }
    if req.effective_source_lang.is_some() {
        expected.effective_source_lang =
            normalize_optional_text(req.effective_source_lang.as_deref());
    }
    if req.effective_target_lang.is_some() {
        expected.effective_target_lang =
            normalize_optional_text(req.effective_target_lang.as_deref());
    }
    if let Some(v) = req.editable_overrides.as_ref() {
        anyhow::ensure!(
            v.is_object(),
            "discovery overrides must be an object; original policy retained"
        );
        expected.editable_overrides = Some(v.clone());
    }
    expected.updated_at = unix_ts() as i64;
    let overrides = expected
        .editable_overrides
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let changed = conn.execute(
        "UPDATE discovery_tasks SET concurrency=?1, batch_parallel=?2, per_page=?3,
         retry_max=?4, timeout_secs=?5, enabled=?6, include_resync=?7,
         selected_component_id=?8, effective_source_lang=?9, effective_target_lang=?10,
         editable_overrides_json=?11, updated_at=?12 WHERE id=?13",
        params![
            expected.concurrency,
            expected.batch_parallel,
            expected.per_page,
            expected.retry_max,
            expected.timeout_secs,
            expected.enabled,
            expected.include_resync,
            expected.selected_component_id,
            expected.effective_source_lang,
            expected.effective_target_lang,
            overrides,
            expected.updated_at,
            id
        ],
    )?;
    anyhow::ensure!(
        changed == 1 && get_discovery_task_by_id(conn, id)? == Some(expected),
        "discovery configuration write was not confirmed; original policy retained"
    );
    mutation.commit()
}

// ---------------------------------------------------------------------------
// Update last_run_at
// ---------------------------------------------------------------------------

pub(crate) fn touch_last_run_at(conn: &Connection, domain: &str, relation_id: i64) -> Result<()> {
    let mutation = super::translations::RetainedMutation::begin(conn)?;
    let now = unix_ts() as i64;
    let changed = conn.execute(
        "UPDATE discovery_tasks SET last_run_at = ?1 WHERE domain = ?2 AND relation_id = ?3",
        params![now, domain, relation_id],
    )?;
    anyhow::ensure!(changed == 1 && conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM discovery_tasks WHERE domain=?1 AND relation_id=?2 AND last_run_at=?3)",
        params![domain, relation_id, now], |row| row.get::<_, bool>(0))?,
        "discovery last-run projection was not confirmed; original settings retained");
    mutation.commit()
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
) -> Result<bool> {
    let mutation = super::translations::RetainedMutation::begin(conn)?;
    let normalized_object_type = normalize_object_type_key(object_type);
    if in_progress_stamp(conn, domain, relation_id, normalized_object_type, object_id)?.is_some() {
        mutation.commit()?;
        return Ok(false);
    }
    let now = unix_ts() as i64;
    let changed = conn.execute(
        "INSERT OR IGNORE INTO translation_in_progress (domain, relation_id, object_type, object_id, claimed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![domain, relation_id, normalized_object_type, object_id, now],
    )?;
    anyhow::ensure!(
        changed == 1
            && in_progress_stamp(conn, domain, relation_id, normalized_object_type, object_id)?
                == Some(now),
        "discovery claim was not confirmed; no new execution authorized"
    );
    mutation.commit()?;
    Ok(true)
}

fn in_progress_stamp(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT claimed_at FROM translation_in_progress
        WHERE domain=?1 AND relation_id=?2 AND object_type=?3 AND object_id=?4",
            params![domain, relation_id, object_type, object_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Release an in-progress claim.
pub(crate) fn release_in_progress(
    conn: &Connection,
    domain: &str,
    relation_id: i64,
    object_type: &str,
    object_id: i64,
) -> Result<()> {
    let mutation = super::translations::RetainedMutation::begin(conn)?;
    let normalized_object_type = normalize_object_type_key(object_type);
    if in_progress_stamp(conn, domain, relation_id, normalized_object_type, object_id)?.is_none() {
        return mutation.commit();
    }
    let changed = conn.execute(
        "DELETE FROM translation_in_progress
         WHERE domain = ?1 AND relation_id = ?2 AND object_type = ?3 AND object_id = ?4",
        params![domain, relation_id, normalized_object_type, object_id],
    )?;
    anyhow::ensure!(
        changed == 1
            && in_progress_stamp(conn, domain, relation_id, normalized_object_type, object_id)?
                .is_none(),
        "discovery claim release was not confirmed; original claim retained"
    );
    mutation.commit()
}

/// Age is not ownership evidence. Only the exclusive runtime boot recovery
/// clears abandoned claims; normal scans merely report the retained count.
pub(crate) fn in_progress_inventory(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM translation_in_progress", [], |row| {
        row.get(0)
    })?;
    Ok(usize::try_from(count)?)
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
        let tasks = list_discovery_tasks(&conn).unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn claim_guard_reports_other_relations_selections_only() {
        let conn = make_db();
        // (d1, r1) selected comp-x; (d1, r2) unbound; (d2, r1) selected comp-y.
        ensure_discovery_tasks(&conn, "https://d1.example", &[1, 2]).unwrap();
        ensure_discovery_tasks(&conn, "https://d2.example", &[1]).unwrap();
        let tasks = list_discovery_tasks(&conn).unwrap();
        for task in &tasks {
            let selected = if task.domain == "https://d1.example" && task.relation_id == 1 {
                Some("comp-x".to_string())
            } else if task.domain == "https://d2.example" {
                Some("comp-y".to_string())
            } else {
                None
            };
            update_discovery_task(
                &conn,
                task.id,
                &UpdateDiscoveryTaskRequest {
                    selected_component_id: selected,
                    ..Default::default()
                },
            )
            .unwrap();
        }

        // The unbound (d1, r2) task sees BOTH foreign selections claimed.
        let claimed =
            selected_components_claimed_by_other_relations(&conn, "https://d1.example", 2).unwrap();
        assert!(claimed.contains("comp-x"), "claimed={claimed:?}");
        assert!(claimed.contains("comp-y"), "claimed={claimed:?}");

        // (d1, r1)'s own selection is NOT claimed from its own perspective.
        let own =
            selected_components_claimed_by_other_relations(&conn, "https://d1.example", 1).unwrap();
        assert!(!own.contains("comp-x"), "own={own:?}");
        assert!(own.contains("comp-y"));

        // A task with no other selections anywhere sees an empty claim set.
        let fresh = make_db();
        let none = selected_components_claimed_by_other_relations(&fresh, "https://d3.example", 7)
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn claim_guard_counts_disabled_task_selections_as_claimed() {
        let conn = make_db();
        ensure_discovery_tasks(&conn, "https://d1.example", &[1]).unwrap();
        ensure_discovery_tasks(&conn, "https://d2.example", &[2]).unwrap();
        let tasks = list_discovery_tasks(&conn).unwrap();
        for task in &tasks {
            let (selected, enabled) = if task.domain == "https://d1.example" {
                (Some("comp-x".to_string()), Some(false))
            } else {
                (None, None)
            };
            update_discovery_task(
                &conn,
                task.id,
                &UpdateDiscoveryTaskRequest {
                    selected_component_id: selected,
                    enabled,
                    ..Default::default()
                },
            )
            .unwrap();
        }
        // Disabled (d1, r1) still owns comp-x: an unbound task on another
        // relation must not grab it while its owner is merely paused.
        let claimed =
            selected_components_claimed_by_other_relations(&conn, "https://d2.example", 2).unwrap();
        assert!(claimed.contains("comp-x"), "claimed={claimed:?}");
    }

    #[test]
    fn ensure_is_idempotent() {
        let conn = make_db();
        ensure_discovery_tasks(&conn, "https://example.com", &[1]).unwrap();
        ensure_discovery_tasks(&conn, "https://example.com", &[1]).unwrap();
        let tasks = list_discovery_tasks(&conn).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn get_params_returns_defaults_when_missing() {
        let conn = make_db();
        let params = get_discovery_task_params(&conn, "https://example.com", 99).unwrap();
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
        let tasks = list_discovery_tasks(&conn).unwrap();
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
        let params = get_discovery_task_params(&conn, "https://example.com", 1).unwrap();
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
        let tasks = list_discovery_tasks(&conn).unwrap();
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
        let params = get_discovery_task_params(&conn, "https://example.com", 1).unwrap();
        assert!(params.include_resync);
        // Also verify list_discovery_tasks reflects the change
        let tasks2 = list_discovery_tasks(&conn).unwrap();
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
        let params2 = get_discovery_task_params(&conn, "https://example.com", 1).unwrap();
        assert!(!params2.include_resync);
    }

    #[test]
    fn try_claim_and_release() {
        let conn = make_db();
        let claimed =
            try_claim_in_progress(&conn, "https://example.com", 1, "post_type", 100).unwrap();
        assert!(claimed);
        // Second claim should fail
        let claimed2 =
            try_claim_in_progress(&conn, "https://example.com", 1, "post_type", 100).unwrap();
        assert!(!claimed2);
        // Release
        release_in_progress(&conn, "https://example.com", 1, "post_type", 100).unwrap();
        // Should be claimable again
        let claimed3 =
            try_claim_in_progress(&conn, "https://example.com", 1, "post_type", 100).unwrap();
        assert!(claimed3);
        release_in_progress(&conn, "https://example.com", 1, "post_type", 100).unwrap();
    }

    #[test]
    fn inventory_preserves_old_claim() {
        let conn = make_db();
        // Insert an "old" entry directly
        conn.execute(
            "INSERT OR IGNORE INTO translation_in_progress (domain, relation_id, object_type, object_id, claimed_at)
             VALUES ('x', 1, 'post_type', 42, 0)",
            [],
        )
        .unwrap();
        assert_eq!(in_progress_inventory(&conn).unwrap(), 1);
        let claimed = try_claim_in_progress(&conn, "x", 1, "post_type", 42).unwrap();
        assert!(!claimed, "only an owned release can clear the old claim");
        release_in_progress(&conn, "x", 1, "post_type", 42).unwrap();
    }

    #[test]
    fn retention_aged_in_progress_claim_does_not_authorize_takeover() {
        let conn = make_db();
        conn.execute(
            "INSERT INTO translation_in_progress
             (domain, relation_id, object_type, object_id, claimed_at)
             VALUES ('retained', 1, 'post_type', 42, 0)",
            [],
        )
        .unwrap();
        assert_eq!(in_progress_inventory(&conn).unwrap(), 1);
        assert!(
            !try_claim_in_progress(&conn, "retained", 1, "post_type", 42).unwrap(),
            "elapsed time cannot prove that the original owner has exited"
        );
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
        let first =
            try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id).unwrap();
        assert!(first, "first claim must succeed");

        // Second claim for identical key — unique constraint must reject it
        let second =
            try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id).unwrap();
        assert!(!second, "second claim must fail due to unique constraint");

        // A different object_id on the same relation should still be claimable
        let other_object =
            try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id + 1).unwrap();
        assert!(
            other_object,
            "a different object_id must be claimable independently"
        );

        // Same object_id but different object_type should also be claimable.
        let taxonomy_object =
            try_claim_in_progress(&conn, domain, relation_id, "taxonomy", object_id).unwrap();
        assert!(
            taxonomy_object,
            "same object_id should not collide across object_type"
        );

        // After releasing the original, it should become claimable again
        release_in_progress(&conn, domain, relation_id, "post_type", object_id).unwrap();
        let after_release =
            try_claim_in_progress(&conn, domain, relation_id, "post_type", object_id).unwrap();
        assert!(after_release, "claim must succeed after release");

        release_in_progress(&conn, domain, relation_id, "post_type", object_id).unwrap();
        release_in_progress(&conn, domain, relation_id, "post_type", object_id + 1).unwrap();
        release_in_progress(&conn, domain, relation_id, "taxonomy", object_id).unwrap();
    }
}