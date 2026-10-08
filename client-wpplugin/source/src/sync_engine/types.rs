use serde::{Deserialize, Serialize};

pub const SYNC_PAIRS_SCHEMA_VERSION: &str = "wpmmcc-sync-pairs.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncDirection {
    Unidirectional,
    Bidirectional,
}

impl Default for SyncDirection {
    fn default() -> Self {
        Self::Unidirectional
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    SyncOnly,
    SyncAndTranslate,
}

impl Default for SyncMode {
    fn default() -> Self {
        Self::SyncOnly
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStrategy {
    Lww,
    SourceWins,
    TargetWins,
    ManualReview,
    Merge,
}

impl Default for ConflictStrategy {
    fn default() -> Self {
        Self::Lww
    }
}

impl ConflictStrategy {
    /// Canonical wire vocabulary (X-1, tasks/5.3falsh2/12 批 B):
    /// lww / source_wins / target_wins / manual_review / merge.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            ConflictStrategy::Lww => "lww",
            ConflictStrategy::SourceWins => "source_wins",
            ConflictStrategy::TargetWins => "target_wins",
            ConflictStrategy::ManualReview => "manual_review",
            ConflictStrategy::Merge => "merge",
        }
    }

    /// Parse a wire value; unknown strings return None so callers decide
    /// the fallback (protocol default is lww).
    pub fn from_wire_str(raw: &str) -> Option<Self> {
        match raw.trim() {
            "lww" => Some(ConflictStrategy::Lww),
            "source_wins" => Some(ConflictStrategy::SourceWins),
            "target_wins" => Some(ConflictStrategy::TargetWins),
            "manual_review" => Some(ConflictStrategy::ManualReview),
            "merge" => Some(ConflictStrategy::Merge),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncFrequency {
    Manual,
    EveryMinute,
    Hourly,
    Daily,
}

impl Default for SyncFrequency {
    fn default() -> Self {
        Self::Manual
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPairStatus {
    Active,
    Paused,
    Error,
}

impl Default for SyncPairStatus {
    fn default() -> Self {
        Self::Active
    }
}

/// One core field on a sync-and-translate pair.
/// Empty `field_actions` keeps the old behavior: translate title, content
/// and excerpt with `translate_component_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncFieldAction {
    pub field: String,
    /// `translate`, `copy` / `as_is`, or `skip` / `exclude`.
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncPair {
    pub id: String,
    pub name: String,
    pub source_domain: String,
    pub target_domain: String,
    #[serde(default)]
    pub direction: SyncDirection,
    #[serde(default)]
    pub sync_mode: SyncMode,
    #[serde(default = "default_source_lang")]
    pub source_lang: String,
    #[serde(default = "default_target_lang")]
    pub target_lang: String,
    #[serde(default)]
    pub conflict_strategy: ConflictStrategy,
    #[serde(default)]
    pub sync_frequency: SyncFrequency,
    #[serde(default = "default_post_types")]
    pub post_types: Vec<String>,
    #[serde(default)]
    pub status: SyncPairStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<u64>,
    /// Keyset cursor into the source site's `/sync/digest` (highest source
    /// local_id scanned). Renamed from `last_sync_journal_id` — the plugin
    /// contract has no journal-cursor pull; the digest keyset cursor is the
    /// real incremental position.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "last_sync_journal_id"
    )]
    pub last_seen_source_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Default translation component when a field action does not name one.
    /// Required only when at least one field action is `translate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate_component_id: Option<String>,
    /// Per-field translate / copy / skip. Empty means the historical three
    /// text fields are all translated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_actions: Vec<SyncFieldAction>,
    /// When true, pull(+translate) parks packets for human Diff approve
    /// before push. Default false preserves auto-push behavior.
    #[serde(default)]
    pub review_before_push: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

fn default_source_lang() -> String {
    "en_US".to_string()
}

fn default_target_lang() -> String {
    "zh_CN".to_string()
}

fn default_post_types() -> Vec<String> {
    vec!["post".to_string(), "page".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPairsDoc {
    pub schema_version: String,
    pub pairs: Vec<SyncPair>,
    pub updated_at: u64,
}

impl Default for SyncPairsDoc {
    fn default() -> Self {
        Self {
            schema_version: SYNC_PAIRS_SCHEMA_VERSION.to_string(),
            pairs: Vec::new(),
            updated_at: 0,
        }
    }
}
