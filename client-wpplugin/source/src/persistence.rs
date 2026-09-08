use anyhow::Context;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::pending_callbacks::{
    add_pending_callback, cleanup_stale_pending_callbacks, find_pending_callback,
    increment_retry_pending_callback, list_all_pending_callbacks, remove_pending_callback,
    PendingCallbackEntry,
};
use crate::logging::unix_ts;
use crate::types::TranslationCallbackPayload;

// ---------------------------------------------------------------------------
// Pending callback store (DB-backed, replaces former JSON file persistence)
// ---------------------------------------------------------------------------

/// A single pending translation callback awaiting delivery to WP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingCallback {
    pub api_base_url: String,
    pub idempotency_key: String,
    pub payload: TranslationCallbackPayload,
    pub route_secret: Option<String>,
    pub created_at: u64,
    pub retry_count: u32,
    pub last_retry_at: u64,
}

/// DB-backed pending callback store.
///
/// Holds its own SQLite `Connection` (separate from the shared web_ui connection).
/// This is safe because SQLite WAL mode supports concurrent connections.
pub(crate) struct PendingCallbackStore {
    conn: Connection,
}

impl PendingCallbackStore {
    /// Open a DB-backed pending callback store.
    ///
    /// Runs TTL cleanup on open (prunes entries with retry_count >= 10 or older
    /// than 72 hours), mirroring the old JSON file pruning behavior.
    pub(crate) fn open(db_path: &str) -> anyhow::Result<Self> {
        let conn = crate::db::open_db(db_path)
            .with_context(|| format!("open pending callbacks db: {}", db_path))?;
        // Prune stale entries on open
        let _ = cleanup_stale_pending_callbacks(&conn, 10, 72 * 3600);
        Ok(Self { conn })
    }

    pub(crate) fn add(&self, entry: PendingCallback) -> anyhow::Result<()> {
        // Reject entries that have already exhausted all retries.
        if entry.retry_count >= 10 {
            eprintln!(
                "[WARN] pending_callbacks: refusing to add entry '{}' with retry_count={}",
                entry.idempotency_key, entry.retry_count
            );
            return Ok(());
        }
        // Reject entries that are already older than 72 hours.
        let now = unix_ts();
        let max_age_secs: u64 = 72 * 3600;
        if now.saturating_sub(entry.created_at) >= max_age_secs {
            eprintln!(
                "[WARN] pending_callbacks: refusing to add entry '{}' older than 72 hours (created_at={})",
                entry.idempotency_key, entry.created_at
            );
            return Ok(());
        }

        let db_entry = PendingCallbackEntry {
            api_base_url: entry.api_base_url,
            idempotency_key: entry.idempotency_key,
            payload: entry.payload.clone(),
            route_secret: entry.route_secret,
            created_at: entry.created_at,
            retry_count: entry.retry_count,
            last_retry_at: entry.last_retry_at,
            relation_id: entry.payload.relation_id as i64,
            object_id: entry.payload.object_id as i64,
            object_type: normalize_object_type(&entry.payload.object_type).to_string(),
        };
        add_pending_callback(&self.conn, &db_entry)?;
        Ok(())
    }

    pub(crate) fn find(
        &self,
        api_base_url: &str,
        relation_id: u64,
        object_type: &str,
        object_id: u64,
    ) -> Option<PendingCallback> {
        let normalized_object_type = normalize_object_type(object_type);
        let entry = find_pending_callback(
            &self.conn,
            api_base_url,
            relation_id as i64,
            normalized_object_type,
            object_id as i64,
        )?;
        Some(db_entry_to_pending(entry))
    }

    pub(crate) fn remove(
        &self,
        api_base_url: &str,
        relation_id: u64,
        object_type: &str,
        object_id: u64,
    ) -> anyhow::Result<bool> {
        let normalized_object_type = normalize_object_type(object_type);
        remove_pending_callback(
            &self.conn,
            api_base_url,
            relation_id as i64,
            normalized_object_type,
            object_id as i64,
        )
    }

    #[allow(dead_code)]
    pub(crate) fn list_pending(&self, api_base_url: &str) -> Vec<PendingCallback> {
        list_all_pending_callbacks(&self.conn)
            .into_iter()
            .filter(|e| e.api_base_url == api_base_url)
            .map(db_entry_to_pending)
            .collect()
    }

    pub(crate) fn increment_retry(
        &self,
        api_base_url: &str,
        relation_id: u64,
        object_type: &str,
        object_id: u64,
    ) -> anyhow::Result<()> {
        let normalized_object_type = normalize_object_type(object_type);
        increment_retry_pending_callback(
            &self.conn,
            api_base_url,
            relation_id as i64,
            normalized_object_type,
            object_id as i64,
        )
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM pending_callbacks", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }
}

fn db_entry_to_pending(entry: PendingCallbackEntry) -> PendingCallback {
    PendingCallback {
        api_base_url: entry.api_base_url,
        idempotency_key: entry.idempotency_key,
        payload: entry.payload,
        route_secret: entry.route_secret,
        created_at: entry.created_at,
        retry_count: entry.retry_count,
        last_retry_at: entry.last_retry_at,
    }
}

fn normalize_object_type(raw: &str) -> &str {
    match raw {
        "term" | "taxonomy" => "taxonomy",
        "post" | "post_type" => "post_type",
        "language_pack" => "language_pack",
        _ => "post_type",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> String {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.keep().join("test-pending.db");
        path.to_string_lossy().to_string()
    }

    fn make_pending(api_base_url: &str, relation_id: u64, object_id: u64) -> PendingCallback {
        PendingCallback {
            api_base_url: api_base_url.to_string(),
            idempotency_key: format!("discovery-{}-post_type-{}-worker1", relation_id, object_id),
            payload: TranslationCallbackPayload {
                schema_version: crate::config::TASK_CALLBACK_SCHEMA_VERSION,
                attempt_id: format!("att-pending-{}-{}", relation_id, object_id),
                object_snapshot_hash: format!("hash-pending-{}-{}", relation_id, object_id),
                source_revision: String::new(),
                policy_version: String::new(),
                field_results: Vec::new(),
                relation_id,
                business_line: "post_content".to_string(),
                object_type: "post_type".to_string(),
                subtype: "post".to_string(),
                object_id,
                translated_fields: std::collections::HashMap::from([(
                    "post_title".to_string(),
                    "translated title".to_string(),
                )]),
                translated_meta: std::collections::HashMap::new(),
                media_mappings: Vec::new(),
                media_field_sources: std::collections::HashMap::new(),
                client_task_id: format!("discovery_{}_{}", relation_id, object_id),
                outbox_id: None,
                worker_id: "worker1".to_string(),
                source_lang: "en".to_string(),
                target_lang: "zh".to_string(),
                execution_time_ms: 0,
            },
            route_secret: Some("secret123".to_string()),
            // Use current time so entries are not rejected by the 72-hour TTL guard.
            created_at: unix_ts(),
            retry_count: 0,
            last_retry_at: 0,
        }
    }

    #[test]
    fn pending_open_creates_db() {
        let path = temp_db_path();
        let store = PendingCallbackStore::open(&path).expect("open");
        assert_eq!(store.entry_count(), 0);
    }

    #[test]
    fn pending_add_and_find() {
        let path = temp_db_path();
        let store = PendingCallbackStore::open(&path).expect("open");

        store
            .add(make_pending("https://wp.example.com", 1, 100))
            .unwrap();
        store
            .add(make_pending("https://wp.example.com", 1, 200))
            .unwrap();
        assert_eq!(store.entry_count(), 2);

        let found = store.find("https://wp.example.com", 1, "post_type", 100);
        assert!(found.is_some());
        assert_eq!(found.unwrap().payload.object_id, 100);

        let not_found = store.find("https://wp.example.com", 1, "post_type", 999);
        assert!(not_found.is_none());

        let wrong_domain = store.find("https://other.com", 1, "post_type", 100);
        assert!(wrong_domain.is_none());
    }

    #[test]
    fn pending_remove() {
        let path = temp_db_path();
        let store = PendingCallbackStore::open(&path).expect("open");

        store
            .add(make_pending("https://wp.example.com", 1, 100))
            .unwrap();
        store
            .add(make_pending("https://wp.example.com", 1, 200))
            .unwrap();
        assert_eq!(store.entry_count(), 2);

        let removed = store
            .remove("https://wp.example.com", 1, "post_type", 100)
            .unwrap();
        assert!(removed);
        assert_eq!(store.entry_count(), 1);
        assert!(store
            .find("https://wp.example.com", 1, "post_type", 100)
            .is_none());
        assert!(store
            .find("https://wp.example.com", 1, "post_type", 200)
            .is_some());

        let not_removed = store
            .remove("https://wp.example.com", 1, "post_type", 999)
            .unwrap();
        assert!(!not_removed);
    }

    #[test]
    fn pending_list_pending_filters_by_domain() {
        let path = temp_db_path();
        let store = PendingCallbackStore::open(&path).expect("open");

        store.add(make_pending("https://a.com", 1, 10)).unwrap();
        store.add(make_pending("https://a.com", 1, 20)).unwrap();
        store.add(make_pending("https://b.com", 2, 30)).unwrap();

        let a_pending = store.list_pending("https://a.com");
        assert_eq!(a_pending.len(), 2);

        let b_pending = store.list_pending("https://b.com");
        assert_eq!(b_pending.len(), 1);

        let c_pending = store.list_pending("https://c.com");
        assert!(c_pending.is_empty());
    }

    #[test]
    fn pending_increment_retry() {
        let path = temp_db_path();
        let store = PendingCallbackStore::open(&path).expect("open");

        store
            .add(make_pending("https://wp.example.com", 1, 100))
            .unwrap();
        store
            .increment_retry("https://wp.example.com", 1, "post_type", 100)
            .unwrap();
        store
            .increment_retry("https://wp.example.com", 1, "post_type", 100)
            .unwrap();

        let entry = store
            .find("https://wp.example.com", 1, "post_type", 100)
            .unwrap();
        assert_eq!(entry.retry_count, 2);
        assert!(entry.last_retry_at > 0);
    }

    #[test]
    fn pending_data_persists_across_reopen() {
        let path = temp_db_path();

        // Write data
        {
            let store = PendingCallbackStore::open(&path).expect("open");
            store
                .add(make_pending("https://wp.example.com", 1, 100))
                .unwrap();
            store
                .add(make_pending("https://wp.example.com", 2, 200))
                .unwrap();
            store
                .increment_retry("https://wp.example.com", 1, "post_type", 100)
                .unwrap();
        }

        // Reopen and verify
        {
            let store = PendingCallbackStore::open(&path).expect("open");
            assert_eq!(store.entry_count(), 2);

            let entry = store
                .find("https://wp.example.com", 1, "post_type", 100)
                .unwrap();
            assert_eq!(entry.retry_count, 1);
            assert_eq!(
                entry.payload.translated_fields.get("post_title").unwrap(),
                "translated title"
            );

            let entry2 = store
                .find("https://wp.example.com", 2, "post_type", 200)
                .unwrap();
            assert_eq!(entry2.retry_count, 0);
        }
    }

    #[test]
    fn pending_remove_then_reopen_is_clean() {
        let path = temp_db_path();

        {
            let store = PendingCallbackStore::open(&path).expect("open");
            store
                .add(make_pending("https://wp.example.com", 1, 100))
                .unwrap();
            store
                .remove("https://wp.example.com", 1, "post_type", 100)
                .unwrap();
        }

        {
            let store = PendingCallbackStore::open(&path).expect("open");
            assert_eq!(store.entry_count(), 0);
        }
    }

    // -----------------------------------------------------------------------
    // TTL and max-retries enforcement
    // -----------------------------------------------------------------------

    #[test]
    fn test_pending_callback_max_retries_rejected() {
        let path = temp_db_path();
        let store = PendingCallbackStore::open(&path).expect("open");

        // An entry whose retry_count is already at the limit must be silently dropped.
        let mut exhausted = make_pending("https://wp.example.com", 5, 500);
        exhausted.retry_count = 10;
        store.add(exhausted).unwrap();

        // Also verify a count just below the limit IS accepted.
        let mut almost = make_pending("https://wp.example.com", 5, 501);
        almost.retry_count = 9;
        almost.created_at = unix_ts();
        store.add(almost).unwrap();

        assert_eq!(
            store.entry_count(),
            1,
            "entry with retry_count=10 must be rejected; retry_count=9 must be accepted"
        );
        assert!(store
            .find("https://wp.example.com", 5, "post_type", 500)
            .is_none());
        assert!(store
            .find("https://wp.example.com", 5, "post_type", 501)
            .is_some());
    }

    #[test]
    fn test_pending_callback_ttl_cleanup_on_open() {
        let path = temp_db_path();

        // Write entries directly (bypassing add() TTL guard by using DB directly)
        {
            let store = PendingCallbackStore::open(&path).expect("open");

            // Fresh entry
            let fresh = make_pending("https://wp.example.com", 10, 1001);
            store.add(fresh).unwrap();

            // Stale entry — add it directly to DB with old created_at
            let stale_entry = PendingCallbackEntry {
                api_base_url: "https://wp.example.com".to_string(),
                idempotency_key: "discovery-10-post_type-1002-worker1".to_string(),
                payload: make_pending("https://wp.example.com", 10, 1002).payload,
                route_secret: Some("secret123".to_string()),
                created_at: unix_ts().saturating_sub(73 * 3600),
                retry_count: 0,
                last_retry_at: 0,
                relation_id: 10,
                object_id: 1002,
                object_type: "post_type".to_string(),
            };
            add_pending_callback(&store.conn, &stale_entry).unwrap();
            assert_eq!(store.entry_count(), 2);
        }

        // Reopen — cleanup on open must prune the stale entry.
        {
            let store = PendingCallbackStore::open(&path).expect("open");
            assert_eq!(
                store.entry_count(),
                1,
                "stale entry (73 h old) must be pruned on open"
            );
            assert!(store
                .find("https://wp.example.com", 10, "post_type", 1001)
                .is_some());
            assert!(
                store
                    .find("https://wp.example.com", 10, "post_type", 1002)
                    .is_none(),
                "stale entry should have been removed"
            );
        }
    }
}
