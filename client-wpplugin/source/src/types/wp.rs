use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

fn default_callback_schema_version() -> u64 {
    crate::config::TASK_CALLBACK_SCHEMA_VERSION
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CallbackFieldResult {
    pub field: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_format: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub storage: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider_component: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub merge_target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transform_stage: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fallback_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TranslationCallbackPayload {
    #[serde(default = "default_callback_schema_version")]
    pub schema_version: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attempt_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub object_snapshot_hash: String,
    /// Immutable source fingerprint supplied by WordPress when this item was
    /// discovered.  It must be echoed back unchanged: recomputing it during
    /// callback would turn an old job into a seemingly fresh one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_revision: String,
    /// Translation-policy fingerprint supplied together with source_revision.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_results: Vec<CallbackFieldResult>,
    pub relation_id: u64,
    pub business_line: String,
    pub object_type: String,
    #[serde(rename = "post_type")]
    pub subtype: String,
    pub object_id: u64,
    pub translated_fields: HashMap<String, String>,
    pub translated_meta: HashMap<String, String>,
    pub media_mappings: Vec<MediaMapping>,
    #[serde(default)]
    pub media_field_sources: HashMap<String, u64>,
    pub client_task_id: String,
    /// Durable WP lifecycle event being executed, if this payload was claimed
    /// from `/client/content-changes` rather than ordinary discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_id: Option<u64>,
    pub worker_id: String,
    pub source_lang: String,
    pub target_lang: String,
    pub execution_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MediaMapping {
    pub source_id: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub translated_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<u64>,
    /// A lifecycle copy of a WordPress attachment, not a provider-produced
    /// external URL.  The submitter only downloads this kind of reference when
    /// it is on the already configured WordPress origin.
    #[serde(default, skip_serializing_if = "is_false")]
    pub source_copy: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct ContentItem {
    pub(crate) object_type: String,
    pub(crate) subtype: String,
    pub(crate) object_id: i64,
    #[serde(default)]
    pub(crate) needs_resync: bool,
    #[serde(default)]
    pub(crate) mapping_id: Option<i64>,
    pub(crate) complete_data: Value,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LanguagePackCompleteData {
    pub(crate) entry_id: i64,
    pub(crate) msgid: String,
    #[serde(default)]
    pub(crate) msgctxt: String,
    #[serde(default)]
    pub(crate) msgid_plural: String,
    #[serde(default)]
    pub(crate) plural_index: Option<u32>,
    #[serde(default)]
    pub(crate) text_domain: String,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LanguagePackItem {
    pub(crate) object_id: i64,
    #[serde(default)]
    pub(crate) text_domain: String,
    pub(crate) complete_data: LanguagePackCompleteData,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct I18nConfig {
    #[serde(default)]
    pub(crate) translate_plugin_i18n: bool,
    #[serde(default)]
    pub(crate) translate_theme_i18n: bool,
    #[serde(default)]
    pub(crate) translate_config_i18n: bool,
    #[serde(default)]
    pub(crate) translate_site_strings: bool,
    #[serde(default)]
    pub(crate) translate_menu_strings: bool,
    #[serde(default)]
    pub(crate) translate_widget_strings: bool,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct SourceGroupConfig {
    #[serde(default)]
    pub(crate) translate_content_objects: bool,
    #[serde(default)]
    pub(crate) translate_taxonomies: bool,
    #[serde(default)]
    pub(crate) translate_media_text: bool,
    #[serde(default)]
    pub(crate) translate_media_files: bool,
    #[serde(default)]
    pub(crate) translate_seo_meta: bool,
    #[serde(default)]
    pub(crate) translate_slug: bool,
    #[serde(default)]
    pub(crate) translate_plugin_i18n: bool,
    #[serde(default)]
    pub(crate) translate_theme_i18n: bool,
    #[serde(default)]
    pub(crate) translate_config_i18n: bool,
    #[serde(default)]
    pub(crate) translate_message_templates: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct DiscoveredRelation {
    pub(crate) id: i64,
    #[serde(default)]
    pub(crate) source_site_id: Value,
    pub(crate) source_lang: String,
    #[serde(default)]
    pub(crate) target_site_id: Value,
    pub(crate) target_site_type: String,
    pub(crate) target_lang: String,
    pub(crate) sync_mode: String,
    #[serde(default)]
    pub(crate) media_handling: String,
    #[serde(default)]
    pub(crate) template: String,
    #[serde(default)]
    pub(crate) models: Vec<DiscoveredModel>,
    #[serde(default)]
    pub(crate) i18n_config: Option<I18nConfig>,
    #[serde(default)]
    pub(crate) source_group_config: Option<SourceGroupConfig>,
    #[serde(default)]
    pub(crate) preflight_policy: String,
    #[serde(default)]
    pub(crate) missing_component_behavior: String,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct DiscoveredModel {
    pub(crate) model_id: i64,
    #[serde(default)]
    pub(crate) plugin_slug: String,
    #[serde(default)]
    pub(crate) plugin_name: String,
    #[serde(default)]
    pub(crate) post_types: Vec<String>,
    #[serde(default)]
    pub(crate) taxonomies: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct DiscoveredRule {
    pub(crate) id: i64,
    pub(crate) model_id: i64,
    #[serde(default)]
    pub(crate) name: String,
    pub(crate) data_type: String,
    pub(crate) object_name: String,
    #[serde(default)]
    pub(crate) field_capabilities: Value,
    #[serde(default)]
    pub(crate) translate_fields: Vec<String>,
    #[serde(default)]
    pub(crate) related_taxonomies: Vec<String>,
    #[serde(default)]
    pub(crate) field_content_formats: HashMap<String, String>,
    #[serde(default)]
    pub(crate) field_storage_map: HashMap<String, String>,
    #[serde(default)]
    pub(crate) source_group: String,
    #[serde(default)]
    pub(crate) routing_profile: String,
    #[serde(default)]
    pub(crate) delivery_target: String,
    #[serde(default)]
    pub(crate) required_component_slots: Vec<String>,
    #[serde(default)]
    pub(crate) required_content_formats: Vec<String>,
    #[serde(default)]
    pub(crate) field_source_roles: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RelationsResponse {
    pub(crate) relations: Vec<DiscoveredRelation>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RulesResponse {
    pub(crate) rules: Vec<DiscoveredRule>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContentResponse {
    pub(crate) items: Vec<ContentItem>,
    pub(crate) total: i64,
    pub(crate) page: i64,
    pub(crate) per_page: i64,
}

/// A leased WordPress lifecycle outbox entry. The payload is a normal content
/// snapshot, so the production worker can run it through the same component
/// and callback pipeline as discovery without a second fetch/race window.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct OutboxContentChange {
    pub(crate) relation_id: i64,
    pub(crate) outbox_id: i64,
    pub(crate) task_id: i64,
    pub(crate) client_task_id: String,
    pub(crate) item: ContentItem,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OutboxContentChangesData {
    #[serde(default)]
    pub(crate) items: Vec<OutboxContentChange>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OutboxContentChangesResponse {
    #[serde(default)]
    pub(crate) success: bool,
    pub(crate) data: OutboxContentChangesData,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LanguagePackContentResponse {
    pub(crate) items: Vec<LanguagePackItem>,
    pub(crate) total: i64,
    pub(crate) page: i64,
    pub(crate) per_page: i64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct ClaimedContentItem {
    #[serde(default)]
    pub(crate) object_id: i64,
    #[serde(default)]
    pub(crate) post_type: String,
    #[serde(default)]
    pub(crate) taxonomy: String,
    #[serde(default)]
    pub(crate) entry_id: i64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub(crate) struct ContentClaimResponse {
    #[serde(default)]
    pub(crate) claimed_count: Option<usize>,
    #[serde(default)]
    pub(crate) claimed_items: Option<Vec<ClaimedContentItem>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct I18nCallbackEntry {
    pub(crate) entry_id: i64,
    pub(crate) msgstr: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct I18nCallbackPayload {
    pub(crate) business_line: String,
    pub(crate) relation_id: u64,
    pub(crate) client_task_id: String,
    pub(crate) worker_id: String,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
    pub(crate) entries: Vec<I18nCallbackEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct I18nTranslatedEnvelope {
    pub(crate) payload_type: String,
    pub(crate) idempotency_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) route_secret: Option<String>,
    pub(crate) payload: I18nCallbackPayload,
    pub(crate) persisted_at: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ContentTranslationResult {
    pub(crate) relation_id: i64,
    pub(crate) object_type: String,
    pub(crate) object_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) post_type: Option<String>,
    pub(crate) business_line: String,
    pub(crate) translated_fields: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub(crate) translated_meta: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub(crate) media_mappings: Value,
    pub(crate) source_lang: String,
    pub(crate) target_lang: String,
}
