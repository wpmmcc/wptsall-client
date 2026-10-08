//! Pending sync-content review queue (park after pull/translate, push on approve).

use serde::{Deserialize, Serialize};

use super::packet::SyncPacket;
use crate::logging::unix_ts;

pub const SYNC_REVIEW_SCHEMA_VERSION: &str = "wpmmcc-sync-review.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReviewDelivery {
    pub packet_sha256: String,
    pub authority_sha256: String,
    #[serde(default = "assume_submitted")]
    pub submitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_receipt: Option<super::shipper::ShipResult>,
}

fn assume_submitted() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncReviewStatus {
    PendingReview,
    Pushed,
    Rejected,
}

impl Default for SyncReviewStatus {
    fn default() -> Self {
        Self::PendingReview
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReviewItem {
    pub id: String,
    pub pair_id: String,
    pub canonical_uuid: String,
    pub status: SyncReviewStatus,
    /// Source-side display fields (pre-translate).
    pub source_title: String,
    pub source_content: String,
    pub source_excerpt: String,
    /// Proposed target fields (after optional translate).
    pub proposed_title: String,
    pub proposed_content: String,
    pub proposed_excerpt: String,
    /// Fully prepared relay packet ready to push.
    pub relayed_packet: SyncPacket,
    pub source_fingerprint: String,
    pub vector_clock: u64,
    pub post_type: String,
    pub post_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Once submitted, edits/rejection cannot authorize another target effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<SyncReviewDelivery>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncReviewDoc {
    pub schema_version: String,
    pub items: Vec<SyncReviewItem>,
    pub updated_at: u64,
}

impl Default for SyncReviewDoc {
    fn default() -> Self {
        Self {
            schema_version: SYNC_REVIEW_SCHEMA_VERSION.to_string(),
            items: Vec::new(),
            updated_at: 0,
        }
    }
}

pub fn sync_review_file() -> String {
    crate::config::sync_review_file()
}

pub fn load_sync_review(path: &str) -> anyhow::Result<SyncReviewDoc> {
    super::json_store::load(path, SYNC_REVIEW_SCHEMA_VERSION, "items")
}

pub fn save_sync_review(path: &str, doc: &SyncReviewDoc) -> anyhow::Result<()> {
    super::json_store::save(path, doc, SYNC_REVIEW_SCHEMA_VERSION, "items")
}

pub fn update_sync_review<R>(
    path: &str,
    mutate: impl FnOnce(&mut SyncReviewDoc) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    super::json_store::update(path, SYNC_REVIEW_SCHEMA_VERSION, "items", mutate)
}

pub fn count_pending_for_pair(doc: &SyncReviewDoc, pair_id: &str) -> usize {
    doc.items
        .iter()
        .filter(|i| i.pair_id == pair_id && i.status == SyncReviewStatus::PendingReview)
        .count()
}

pub fn has_pending_uuid(doc: &SyncReviewDoc, pair_id: &str, uuid: &str) -> bool {
    doc.items.iter().any(|i| {
        i.pair_id == pair_id
            && i.canonical_uuid == uuid
            && i.status == SyncReviewStatus::PendingReview
    })
}

pub fn upsert_pending_item(doc: &mut SyncReviewDoc, item: SyncReviewItem) {
    let now = unix_ts();
    if let Some(existing) = doc.items.iter_mut().find(|i| {
        i.pair_id == item.pair_id
            && i.canonical_uuid == item.canonical_uuid
            && i.status == SyncReviewStatus::PendingReview
    }) {
        if existing.delivery.is_some() {
            return;
        }
        let id = existing.id.clone();
        *existing = item;
        existing.id = id;
        existing.updated_at = now;
    } else {
        doc.items.push(item);
    }
    doc.updated_at = now;
}

pub(crate) fn execution_lease(
    path: &str,
    id: &str,
) -> anyhow::Result<crate::bindings::native_lock::NativeLease> {
    use anyhow::{ensure, Context};
    use std::path::Path;
    ensure!(!id.is_empty() && id.len() <= 128, "invalid sync review identity");
    let requested = Path::new(path);
    let parent = requested.parent().context("sync review parent missing")?;
    let mut options = std::fs::DirBuilder::new();
    options.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        options.mode(0o700);
    }
    options.create(parent)?;
    let resolved = if std::fs::symlink_metadata(requested).is_ok() {
        std::fs::canonicalize(requested)?
    } else {
        std::fs::canonicalize(parent)?.join(requested.file_name().context("sync review name missing")?)
    };
    let name = format!(".{}.delivery-{}.lock",
        resolved.file_name().context("sync review name missing")?.to_string_lossy(),
        super::hmac::sha256_hex(id.as_bytes()));
    crate::bindings::native_lock::NativeLease::acquire(
        &resolved.with_file_name(name),
        "SYNC_REVIEW_BUSY: original delivery is active",
    )
}

pub fn find_review_item<'a>(doc: &'a SyncReviewDoc, id: &str) -> Option<&'a SyncReviewItem> {
    doc.items.iter().find(|i| i.id == id)
}

pub fn find_review_item_mut<'a>(
    doc: &'a mut SyncReviewDoc,
    id: &str,
) -> Option<&'a mut SyncReviewItem> {
    doc.items.iter_mut().find(|i| i.id == id)
}

pub fn list_pending(doc: &SyncReviewDoc, pair_id: Option<&str>) -> Vec<SyncReviewItem> {
    doc.items
        .iter()
        .filter(|i| {
            i.status == SyncReviewStatus::PendingReview
                && pair_id.map(|p| i.pair_id == p).unwrap_or(true)
        })
        .cloned()
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_engine::packet::{
        EntityPayload, OriginContext, SyncPacket, PACKET_SCHEMA_VERSION,
    };
    use std::collections::HashMap;

    #[test]
    fn sync_storage_corrupt_review_cannot_be_read_or_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.json");
        for raw in [
            "",
            "{invalid",
            "{}",
            r#"{"schema_version":"future.v9","items":[],"updated_at":0}"#,
        ] {
            std::fs::write(&path, raw).unwrap();
            assert!(
                load_sync_review(path.to_str().unwrap()).is_err(),
                "corrupt review accepted: {raw}"
            );
            assert!(save_sync_review(path.to_str().unwrap(), &SyncReviewDoc::default()).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn sync_storage_parallel_review_parking_preserves_every_item() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("review.json").to_str().unwrap().to_string();
        let workers = 10;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let mut tasks = Vec::new();
        for n in 0..workers {
            let path = path.clone();
            let barrier = barrier.clone();
            tasks.push(std::thread::spawn(move || {
                barrier.wait();
                update_sync_review(&path, |doc| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    upsert_pending_item(
                        doc,
                        pending_item(&format!("r-{n}"), "pair", &format!("u-{n}")),
                    );
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
        assert_eq!(load_sync_review(&path).unwrap().items.len(), workers);
    }

    fn empty_packet() -> SyncPacket {
        SyncPacket {
            schema_version: PACKET_SCHEMA_VERSION.to_string(),
            packet_id: "pkt".into(),
            origin_context: OriginContext {
                origin_site_uuid: "src".into(),
                origin_site_url: "https://src.example".into(),
                origin_permalink: None,
                origin_blog_id: 1,
                origin_lang: "en_US".into(),
                vector_clock: HashMap::new(),
                hop_count: 1,
                dispatch_timestamp: 1,
            },
            action: "upsert".into(),
            sync_mode: "sync_only".into(),
            target_lang: "zh_CN".into(),
            source_fingerprint: "fp".into(),
            entity: EntityPayload {
                guid: "uuid-1".into(),
                object_type: "post".into(),
                subtype: "post".into(),
                source_id: 1,
                slug: "hello".into(),
                status: "publish".into(),
                author_hint: None,
                core_fields: HashMap::new(),
                taxonomies: HashMap::new(),
                meta_fields: HashMap::new(),
                plugin_specific: HashMap::new(),
                changed_fields: None,
            },
            multimodal_manifest: vec![],
        }
    }

    fn pending_item(id: &str, pair_id: &str, uuid: &str) -> SyncReviewItem {
        SyncReviewItem {
            id: id.into(),
            pair_id: pair_id.into(),
            canonical_uuid: uuid.into(),
            status: SyncReviewStatus::PendingReview,
            source_title: "S".into(),
            source_content: "sc".into(),
            source_excerpt: "".into(),
            proposed_title: "P".into(),
            proposed_content: "pc".into(),
            proposed_excerpt: "".into(),
            relayed_packet: empty_packet(),
            source_fingerprint: "fp".into(),
            vector_clock: 1,
            post_type: "post".into(),
            post_status: "publish".into(),
            error_message: None,
            delivery: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn upsert_replaces_same_pending_uuid_and_counts() {
        let mut doc = SyncReviewDoc::default();
        upsert_pending_item(&mut doc, pending_item("a", "pair-1", "u1"));
        assert_eq!(count_pending_for_pair(&doc, "pair-1"), 1);
        upsert_pending_item(&mut doc, {
            let mut next = pending_item("b", "pair-1", "u1");
            next.proposed_title = "Updated".into();
            next
        });
        assert_eq!(doc.items.len(), 1);
        assert_eq!(doc.items[0].id, "a");
        assert_eq!(doc.items[0].proposed_title, "Updated");
        assert!(has_pending_uuid(&doc, "pair-1", "u1"));
        assert!(!has_pending_uuid(&doc, "pair-2", "u1"));
    }

    #[test]
    fn list_pending_filters_by_pair_and_status() {
        let mut doc = SyncReviewDoc::default();
        upsert_pending_item(&mut doc, pending_item("a", "pair-1", "u1"));
        upsert_pending_item(&mut doc, pending_item("b", "pair-2", "u2"));
        doc.items[1].status = SyncReviewStatus::Rejected;
        assert_eq!(list_pending(&doc, None).len(), 1);
        assert_eq!(list_pending(&doc, Some("pair-1")).len(), 1);
        assert!(list_pending(&doc, Some("pair-2")).is_empty());
        assert!(find_review_item(&doc, "a").is_some());
    }

    #[test]
    fn load_save_roundtrip_on_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sync-review.json");
        let path_str = path.to_string_lossy().to_string();
        let mut doc = SyncReviewDoc::default();
        upsert_pending_item(&mut doc, pending_item("a", "pair-1", "u1"));
        save_sync_review(&path_str, &doc).expect("save");
        let loaded = load_sync_review(&path_str).expect("load");
        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.items[0].canonical_uuid, "u1");
    }
}