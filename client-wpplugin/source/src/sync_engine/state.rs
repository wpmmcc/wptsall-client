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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_storage_corrupt_state_cannot_be_reset_or_mutated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        for raw in [
            "",
            "{invalid",
            "{}",
            r#"{"schema_version":"future.v9","pairs":{},"updated_at":0}"#,
        ] {
            std::fs::write(&path, raw).unwrap();
            assert!(
                load_sync_state(path.to_str().unwrap()).is_err(),
                "corrupt state accepted: {raw}"
            );
            assert!(update_pair_state(path.to_str().unwrap(), "pair-1", |s| {
                s.last_seen_source_id = 999;
            })
            .is_err());
            assert!(save_sync_state(path.to_str().unwrap(), &SyncStateDoc::default()).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn sync_storage_parallel_state_updates_do_not_lose_successful_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let path = path.to_str().unwrap().to_string();
        save_sync_state(&path, &SyncStateDoc::default()).unwrap();
        let workers = 12;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let tasks: Vec<_> = (0..workers)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut acknowledged = 0u64;
                    let mut rejected_busy = 0u64;
                    for _ in 0..20 {
                        let mut applied = false;
                        let result = update_pair_state(&path, "pair-1", |state| {
                            applied = true;
                            state.last_seen_source_id += 1;
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        });
                        match result {
                            Ok(()) => {
                                assert!(applied);
                                acknowledged += 1;
                            }
                            Err(error)
                                if error.to_string().starts_with("sync document is busy;") =>
                            {
                                // The bounded lock contract may reject under
                                // load. A rejected operation is not an ack and
                                // must not execute its mutation.
                                assert!(!applied, "busy rejection applied a mutation");
                                rejected_busy += 1;
                            }
                            Err(error) => panic!("unexpected state mutation error: {error}"),
                        }
                    }
                    (acknowledged, rejected_busy)
                })
            })
            .collect();
        let (acknowledged, rejected_busy) = tasks
            .into_iter()
            .map(|task| task.join().expect("owned mutation worker"))
            .fold((0, 0), |(ack, busy), (next_ack, next_busy)| {
                (ack + next_ack, busy + next_busy)
            });
        assert!(acknowledged > 0, "probe must exercise successful writes");
        assert_eq!(acknowledged + rejected_busy, (workers * 20) as u64);
        eprintln!(
            "owned concurrency probe: {acknowledged} acknowledged, {rejected_busy} rejected busy"
        );
        let doc = load_sync_state(&path).unwrap();
        assert_eq!(doc.pairs["pair-1"].last_seen_source_id, acknowledged);
    }

    #[test]
    fn sync_storage_separate_processes_keep_all_state_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        save_sync_state(path.to_str().unwrap(), &SyncStateDoc::default()).unwrap();
        let mut children = Vec::new();
        for _ in 0..4 {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "sync_engine::state::tests::sync_storage_subprocess_worker",
                        "--ignored",
                    ])
                    .env("WPTSALL_OWNED_SYNC_STORAGE_TEST", &path)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        let outputs: Vec<_> = children
            .into_iter()
            .map(|child| child.wait_with_output().unwrap())
            .collect();
        for output in outputs {
            assert!(
                output.status.success(),
                "owned subprocess failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let doc = load_sync_state(path.to_str().unwrap()).unwrap();
        assert_eq!(doc.pairs["multiprocess"].last_seen_source_id, 60);
    }

    #[test]
    #[ignore = "subprocess fixture; run only via sync_storage_separate_processes_keep_all_state_updates"]
    fn sync_storage_subprocess_worker() {
        let path = std::env::var("WPTSALL_OWNED_SYNC_STORAGE_TEST").expect("owned path required");
        assert!(std::path::Path::new(&path).starts_with(std::env::temp_dir()));
        for _ in 0..15 {
            update_pair_state(&path, "multiprocess", |state| {
                state.last_seen_source_id += 1;
                std::thread::sleep(std::time::Duration::from_millis(1));
            })
            .unwrap();
        }
    }

    #[test]
    fn cli07_old_known_entity_without_rejection_marker_is_still_shipped() {
        let old = serde_json::json!({
            "canonical_uuid": "uuid-old", "source_fingerprint": "fp-old", "vector_clock": 3,
            "post_type": "post", "post_status": "publish", "target_post_id": 123,
            "last_shipped_at": 42,
        });
        let entity: KnownEntity = serde_json::from_value(old).unwrap();
        assert!(!entity.review_rejected);
        assert_eq!(
            (entity.target_post_id, entity.last_shipped_at),
            (Some(123), 42)
        );
        let roundtrip: KnownEntity =
            serde_json::from_value(serde_json::to_value(&entity).unwrap()).unwrap();
        assert_eq!(roundtrip, entity);
    }

    fn known(uuid: &str, fp: &str) -> KnownEntity {
        KnownEntity {
            canonical_uuid: uuid.to_string(),
            source_fingerprint: fp.to_string(),
            vector_clock: 1,
            post_type: "post".to_string(),
            post_status: "publish".to_string(),
            target_post_id: Some(10),
            last_shipped_at: 1,
            review_rejected: false,
        }
    }

    #[test]
    fn state_roundtrip_and_update() {
        let dir = std::env::temp_dir().join(format!("sync_state_test_{}", unix_ts()));
        let path = dir.join("state.json");
        let path_str = path.to_string_lossy().to_string();

        update_pair_state(&path_str, "pair-1", |s| {
            s.last_seen_source_id = 42;
            s.known
                .insert("uuid-a".to_string(), known("uuid-a", "fp-1"));
        })
        .expect("update");

        let doc = load_sync_state(&path_str).unwrap();
        let state = find_pair_state(&doc, "pair-1").unwrap();
        assert_eq!(state.last_seen_source_id, 42);
        assert_eq!(state.known["uuid-a"].source_fingerprint, "fp-1");

        update_pair_state(&path_str, "pair-1", |s| {
            s.last_seen_source_id = 50;
        })
        .unwrap();
        let doc = load_sync_state(&path_str).unwrap();
        assert_eq!(
            find_pair_state(&doc, "pair-1").unwrap().last_seen_source_id,
            50
        );
        assert_eq!(doc.pairs.len(), 1);

        assert!(remove_pair_state(&path_str, "pair-1").unwrap());
        let doc = load_sync_state(&path_str).unwrap();
        assert!(doc.pairs.is_empty());
    }

    #[test]
    fn reconcile_batch_rotates_and_wraps() {
        let mut state = PairSyncState::default();
        for i in 0..(RECONCILE_BATCH_SIZE + 10) {
            let uuid = format!("uuid-{i:04}");
            state.known.insert(uuid.clone(), known(&uuid, "fp"));
        }
        let (batch, next) = reconcile_batch(&state);
        assert_eq!(batch.len(), RECONCILE_BATCH_SIZE);
        assert_eq!(next, RECONCILE_BATCH_SIZE);

        state.reconcile_cursor = next;
        let (batch2, next2) = reconcile_batch(&state);
        assert_eq!(batch2.len(), 10);
        assert_eq!(next2, 0, "cursor wraps after a full rotation");

        // Empty state is a no-op.
        let empty = PairSyncState::default();
        let (b, n) = reconcile_batch(&empty);
        assert!(b.is_empty());
        assert_eq!(n, 0);
    }
}