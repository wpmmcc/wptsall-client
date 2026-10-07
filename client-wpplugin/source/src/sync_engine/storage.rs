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
