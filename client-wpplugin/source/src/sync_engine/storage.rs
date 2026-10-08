use super::types::*;
use crate::logging::unix_ts;

pub fn load_sync_pairs(path: &str) -> anyhow::Result<SyncPairsDoc> {
    super::json_store::load(path, SYNC_PAIRS_SCHEMA_VERSION, "pairs")
}

pub fn save_sync_pairs(path: &str, doc: &SyncPairsDoc) -> anyhow::Result<()> {
    super::json_store::save(path, doc, SYNC_PAIRS_SCHEMA_VERSION, "pairs")
}

pub fn update_sync_pairs<R>(
    path: &str,
    mutate: impl FnOnce(&mut SyncPairsDoc) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    super::json_store::update(path, SYNC_PAIRS_SCHEMA_VERSION, "pairs", mutate)
}

pub fn upsert_pair_in_doc(doc: &mut SyncPairsDoc, mut pair: SyncPair) {
    let now = unix_ts();
    pair.updated_at = now;
    if pair.created_at == 0 {
        pair.created_at = now;
    }

    if let Some(pos) = doc.pairs.iter().position(|p| p.id == pair.id) {
        doc.pairs[pos] = pair;
    } else {
        doc.pairs.push(pair);
    }
    doc.updated_at = now;
}

pub fn delete_pair_in_doc(doc: &mut SyncPairsDoc, id: &str) -> bool {
    let initial_len = doc.pairs.len();
    doc.pairs.retain(|p| p.id != id);
    let deleted = doc.pairs.len() < initial_len;
    if deleted {
        doc.updated_at = unix_ts();
    }
    deleted
}

pub fn find_pair_in_doc<'a>(doc: &'a SyncPairsDoc, id: &str) -> Option<&'a SyncPair> {
    doc.pairs.iter().find(|p| p.id == id)
}

pub fn find_pair_in_doc_mut<'a>(doc: &'a mut SyncPairsDoc, id: &str) -> Option<&'a mut SyncPair> {
    doc.pairs.iter_mut().find(|p| p.id == id)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_storage_missing_read_does_not_create_files_or_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing/config/sync-pairs.json");
        assert!(load_sync_pairs(path.to_str().unwrap())
            .unwrap()
            .pairs
            .is_empty());
        assert!(
            !path.parent().unwrap().exists(),
            "a read must not initialize storage"
        );
    }

    #[test]
    fn sync_storage_corrupt_pairs_cannot_be_read_or_overwritten_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pairs.json");
        for raw in [
            "",
            " \n",
            "{invalid",
            "{}",
            r#"{"schema_version":"future.v9","pairs":[],"updated_at":0}"#,
        ] {
            std::fs::write(&path, raw).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(
                load_sync_pairs(path.to_str().unwrap()).is_err(),
                "corrupt pairs accepted: {raw}"
            );
            assert!(save_sync_pairs(path.to_str().unwrap(), &SyncPairsDoc::default()).is_err());
            assert_eq!(
                std::fs::read(&path).unwrap(),
                before,
                "original bytes must survive"
            );
        }
    }

    #[test]
    fn sync_storage_private_atomic_writer_never_reuses_legacy_temp() {
        let dir = tempfile::tempdir().unwrap();
        let _data =
            crate::db::TestEnvVarGuard::set("WPTSALL_DATA_DIR", dir.path().to_str().unwrap());
        let path = dir.path().join("pairs.json");
        let legacy = path.with_extension("tmp.json");
        std::fs::write(&legacy, b"pre-existing unrelated temporary").unwrap();
        save_sync_pairs(path.to_str().unwrap(), &SyncPairsDoc::default()).unwrap();
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            b"pre-existing unrelated temporary"
        );
        let doc = load_sync_pairs(path.to_str().unwrap()).unwrap();
        assert!(doc.pairs.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter(|entry| {
                    entry.as_ref().unwrap().file_name() != ".wptsall-storage-v1.lock"
                })
                .count(),
            3,
            "only document, lock, and original legacy temporary"
        );
        let root_lock = dir.path().join(".wptsall-storage-v1.lock");
        assert!(std::fs::symlink_metadata(&root_lock).unwrap().is_file());
        assert_eq!(std::fs::read(&root_lock).unwrap(), b"");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root_lock).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 4);
    }

    #[test]
    fn sync_storage_parallel_pair_edits_preserve_every_other_pair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pairs.json").to_str().unwrap().to_string();
        let workers = 10;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let mut tasks = Vec::new();
        for n in 0..workers {
            let path = path.clone();
            let barrier = barrier.clone();
            tasks.push(std::thread::spawn(move || {
                let pair: SyncPair = serde_json::from_value(serde_json::json!({
                    "id": format!("owned-{n}"), "name": format!("Pair {n}"),
                    "source_domain": "https://a.example", "target_domain": "https://b.example",
                    "created_at": 0, "updated_at": 0
                }))
                .unwrap();
                barrier.wait();
                update_sync_pairs(&path, |doc| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    upsert_pair_in_doc(doc, pair);
                    Ok(())
                })
                .unwrap();
            }));
        }
        let failures = tasks
            .into_iter()
            .map(|task| task.join())
            .filter(|result| result.is_err())
            .count();
        assert_eq!(failures, 0);
        assert_eq!(load_sync_pairs(&path).unwrap().pairs.len(), workers);
    }

    #[test]
    fn test_sync_pairs_storage_roundtrip() {
        let temp_dir = std::env::temp_dir().join(format!("sync_pairs_test_{}", unix_ts()));
        let file_path = temp_dir.join("sync-pairs.json");
        let path_str = file_path.to_string_lossy();

        let mut doc = load_sync_pairs(&path_str).expect("initial load");
        assert_eq!(doc.pairs.len(), 0);

        let pair = SyncPair {
            id: "pair-1".to_string(),
            name: "Site A to Site B".to_string(),
            source_domain: "https://site-a.com".to_string(),
            target_domain: "https://site-b.com".to_string(),
            direction: SyncDirection::Unidirectional,
            sync_mode: SyncMode::SyncAndTranslate,
            source_lang: "en_US".to_string(),
            target_lang: "zh_CN".to_string(),
            conflict_strategy: ConflictStrategy::Lww,
            sync_frequency: SyncFrequency::Hourly,
            post_types: vec!["post".to_string()],
            status: SyncPairStatus::Active,
            last_sync_at: None,
            last_seen_source_id: None,
            last_sync_count: None,
            last_error: None,
            translate_component_id: None,
            review_before_push: false,
            created_at: 0,
            updated_at: 0,
        };

        upsert_pair_in_doc(&mut doc, pair);
        assert_eq!(doc.pairs.len(), 1);
        save_sync_pairs(&path_str, &doc).expect("save doc");

        let reloaded = load_sync_pairs(&path_str).expect("reload doc");
        assert_eq!(reloaded.pairs.len(), 1);
        assert_eq!(reloaded.pairs[0].name, "Site A to Site B");
        assert_eq!(reloaded.pairs[0].sync_mode, SyncMode::SyncAndTranslate);

        let deleted = delete_pair_in_doc(&mut doc, "pair-1");
        assert!(deleted);
        assert_eq!(doc.pairs.len(), 0);
    }
}