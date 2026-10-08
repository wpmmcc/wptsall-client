use anyhow::Context;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::pending_callbacks::{
    add_pending_callback, find_pending_callback, increment_retry_pending_callback,
    list_all_pending_callbacks, normalize_pending_object_type as normalize_object_type,
    remove_pending_callback, PendingCallbackEntry,
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
    conn: std::sync::Arc<tokio::sync::Mutex<Connection>>,
}

impl PendingCallbackStore {
    /// Open a DB-backed pending callback store.
    ///
    /// Opening is read-only with respect to existing paid results and queues.
    pub(crate) fn open(db_path: &str) -> anyhow::Result<Self> {
        let conn = crate::db::open_db(db_path)
            .with_context(|| format!("open pending callbacks db: {}", db_path))?;
        Ok(Self {
            conn: std::sync::Arc::new(tokio::sync::Mutex::new(conn)),
        })
    }

    pub(crate) fn database(&self) -> std::sync::Arc<tokio::sync::Mutex<Connection>> {
        self.conn.clone()
    }

    fn connection(&self) -> anyhow::Result<tokio::sync::MutexGuard<'_, Connection>> {
        self.conn
            .try_lock()
            .context("pending callback database is active; retained")
    }

    pub(crate) fn add(&self, entry: PendingCallback) -> anyhow::Result<()> {
        let db_entry = PendingCallbackEntry {
            api_base_url: entry.api_base_url,
            idempotency_key: entry.idempotency_key,
            payload: entry.payload.clone(),
            route_secret: entry.route_secret,
            created_at: entry.created_at,
            retry_count: entry.retry_count,
            last_retry_at: entry.last_retry_at,
            relation_id: entry.payload.relation_id.try_into()?,
            object_id: entry.payload.object_id.try_into()?,
            object_type: normalize_object_type(&entry.payload.object_type)?.to_string(),
        };
        add_pending_callback(&*self.connection()?, &db_entry)?;
        Ok(())
    }

    pub(crate) fn find(
        &self,
        api_base_url: &str,
        relation_id: u64,
        object_type: &str,
        object_id: u64,
    ) -> anyhow::Result<Option<PendingCallback>> {
        let normalized_object_type = normalize_object_type(object_type)?;
        let entry = find_pending_callback(
            &*self.connection()?,
            api_base_url,
            relation_id.try_into()?,
            normalized_object_type,
            object_id.try_into()?,
        )?;
        Ok(entry.map(db_entry_to_pending))
    }

    pub(crate) fn remove(
        &self,
        api_base_url: &str,
        relation_id: u64,
        object_type: &str,
        object_id: u64,
    ) -> anyhow::Result<bool> {
        let normalized_object_type = normalize_object_type(object_type)?;
        remove_pending_callback(
            &*self.connection()?,
            api_base_url,
            relation_id.try_into()?,
            normalized_object_type,
            object_id.try_into()?,
        )
    }

    #[allow(dead_code)]
    pub(crate) fn list_pending(&self, api_base_url: &str) -> anyhow::Result<Vec<PendingCallback>> {
        Ok(list_all_pending_callbacks(&*self.connection()?)?
            .into_iter()
            .filter(|e| e.api_base_url == api_base_url)
            .map(db_entry_to_pending)
            .collect())
    }

    pub(crate) fn increment_retry(
        &self,
        api_base_url: &str,
        relation_id: u64,
        object_type: &str,
        object_id: u64,
    ) -> anyhow::Result<()> {
        let normalized_object_type = normalize_object_type(object_type)?;
        increment_retry_pending_callback(
            &*self.connection()?,
            api_base_url,
            relation_id.try_into()?,
            normalized_object_type,
            object_id.try_into()?,
        )
    }

    pub(crate) fn entry_count_checked(&self) -> anyhow::Result<usize> {
        let count =
            self.connection()?
                .query_row("SELECT COUNT(*) FROM pending_callbacks", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        Ok(usize::try_from(count)?)
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
