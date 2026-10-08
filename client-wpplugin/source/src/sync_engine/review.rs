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
