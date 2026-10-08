//! Per-pair incremental sync state: the client-side mirror of the source
//! site's digest. For every canonical UUID the client has already shipped
//! it remembers the source fingerprint (as reported by `/sync/digest`),
//! the vector clock, the target-side post id, and the trash flag. A keyset
//! cursor (`last_seen_source_id`) tracks how far the digest scan advanced.
//!
//! Together with `/sync/reconcile-digest` this gives honest bookkeeping:
//!   - new/changed at source → fingerprint mismatch → pull + ship;
//!   - trashed at source     → trash packet to target;
//!   - missing at source     → (reconcile verdict) delete packet to target;
//!   - in_sync               → skip.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::logging::unix_ts;

pub const SYNC_STATE_SCHEMA_VERSION: &str = "wpmmcc-sync-state.v1";

/// How many known fingerprints are re-checked against the source per run
/// via `/sync/reconcile-digest` (detects deletions and out-of-band edits).
/// The check rotates: each run continues from `reconcile_cursor` through
/// the known map, so every known entity is re-verified over time.
pub const RECONCILE_BATCH_SIZE: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownEntity {
    pub canonical_uuid: String,
    /// Fingerprint as last reported by the source digest.
    pub source_fingerprint: String,
    pub vector_clock: u64,
    pub post_type: String,
    pub post_status: String,
    /// Post id on the target site (from the push ack), when known.
    pub target_post_id: Option<u64>,
    /// Unix seconds of the last successful ship.
    pub last_shipped_at: u64,
    /// This source revision was rejected, not shipped. Existing target
    /// metadata stays intact so later deletion can still remove an older copy.
    #[serde(default)]
    pub review_rejected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PairSyncState {
    pub pair_id: String,
    /// Keyset cursor: highest source `local_id` included in the last digest scan.
    #[serde(default)]
    pub last_seen_source_id: u64,
    /// Rotation cursor for the periodic reconcile pass (index into the
    /// sorted known-uuid list).
    #[serde(default)]
    pub reconcile_cursor: usize,
    #[serde(default)]
    pub known: HashMap<String, KnownEntity>,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStateDoc {
    pub schema_version: String,
    #[serde(default)]
    pub pairs: HashMap<String, PairSyncState>,
    pub updated_at: u64,
}

impl Default for SyncStateDoc {
    fn default() -> Self {
        Self {
            schema_version: SYNC_STATE_SCHEMA_VERSION.to_string(),
            pairs: HashMap::new(),
            updated_at: 0,
        }
    }
}

pub fn load_sync_state(path: &str) -> anyhow::Result<SyncStateDoc> {
    super::json_store::load(path, SYNC_STATE_SCHEMA_VERSION, "pairs")
}

pub fn save_sync_state(path: &str, doc: &SyncStateDoc) -> anyhow::Result<()> {
    super::json_store::save(path, doc, SYNC_STATE_SCHEMA_VERSION, "pairs")
}

pub fn find_pair_state_mut<'a>(doc: &'a mut SyncStateDoc, pair_id: &str) -> &'a mut PairSyncState {
    doc.pairs.entry(pair_id.to_string()).or_default()
}

pub fn find_pair_state<'a>(doc: &'a SyncStateDoc, pair_id: &str) -> Option<&'a PairSyncState> {
    doc.pairs.get(pair_id)
}

/// Persist a mutation helper: load → mutate → save with updated_at stamp.
pub fn update_pair_state<F>(path: &str, pair_id: &str, mutate: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut PairSyncState),
{
    super::json_store::update(
        path,
        SYNC_STATE_SCHEMA_VERSION,
        "pairs",
        |doc: &mut SyncStateDoc| {
            let now = unix_ts();
            let state = find_pair_state_mut(doc, pair_id);
            mutate(state);
            state.pair_id = pair_id.to_string();
            state.updated_at = now;
            doc.updated_at = now;
            Ok(())
        },
    )
}

/// Remove a pair's state (on pair delete).
pub fn remove_pair_state(path: &str, pair_id: &str) -> anyhow::Result<bool> {
    super::json_store::update(
        path,
        SYNC_STATE_SCHEMA_VERSION,
        "pairs",
        |doc: &mut SyncStateDoc| {
            let removed = doc.pairs.remove(pair_id).is_some();
            if removed {
                doc.updated_at = unix_ts();
            }
            Ok(removed)
        },
    )
}

/// The slice of known entities covered by this run's reconcile rotation.
/// Returns (batch, next_cursor).
pub fn reconcile_batch(state: &PairSyncState) -> (Vec<&KnownEntity>, usize) {
    let mut uuids: Vec<&String> = state.known.keys().collect();
    uuids.sort();
    if uuids.is_empty() {
        return (Vec::new(), 0);
    }
    let start = state.reconcile_cursor.min(uuids.len() - 1);
    let end = (start + RECONCILE_BATCH_SIZE).min(uuids.len());
    let batch: Vec<&KnownEntity> = uuids[start..end]
        .iter()
        .filter_map(|uuid| state.known.get(*uuid))
        .collect();
    let next_cursor = if end >= uuids.len() { 0 } else { end };
    (batch, next_cursor)
}
