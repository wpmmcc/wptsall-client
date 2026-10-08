use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Deserialize)]
pub(crate) struct ComponentsData {
    pub(crate) items: Vec<ComponentItem>,
    #[serde(default)]
    pub(crate) page: Option<usize>,
    #[serde(default)]
    pub(crate) per_page: Option<usize>,
    #[serde(default)]
    pub(crate) total: Option<usize>,
    #[serde(default)]
    pub(crate) total_pages: Option<usize>,
    /// Versioned server tombstones.  Old servers omit this field, while a
    /// current server signs the exact JSON array before returning it.
    #[serde(default)]
    pub(crate) revocation_catalog_version: Option<String>,
    #[serde(default)]
    pub(crate) revocations: Vec<ComponentRevocationItem>,
    #[serde(default)]
    pub(crate) revocation_signature: Option<String>,
    #[serde(default)]
    pub(crate) revocation_signing_key_id: Option<String>,
    #[serde(default)]
    pub(crate) revocation_signature_algorithm: Option<String>,
    #[serde(default)]
    pub(crate) revocation_signature_scope: Option<String>,
    #[serde(default)]
    pub(crate) revocation_signature_contract_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub(crate) struct ComponentRevocationItem {
    pub(crate) component_id: String,
    pub(crate) version: String,
    pub(crate) content_digest: String,
    pub(crate) key_id: String,
    pub(crate) revoked_at: String,
    #[serde(default)]
    pub(crate) reason: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentItem {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) owner_type: String,
    #[serde(default)]
    pub(crate) version: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) supported_types: Vec<String>,
    #[serde(default)]
    pub(crate) supported_business_lines: Vec<String>,
    #[serde(default)]
    pub(crate) supported_content_formats: Vec<String>,
    #[serde(default)]
    pub(crate) supported_formats: Vec<String>,
    #[serde(default)]
    pub(crate) max_file_size_mb: Option<u32>,
    #[serde(default)]
    pub(crate) size_class: Option<String>,
    #[serde(default)]
    pub(crate) template_group: Option<String>,
    #[serde(default)]
    pub(crate) catalog_family: Option<String>,
    #[serde(default)]
    pub(crate) catalog_subfamily: Option<String>,
    #[serde(default)]
    pub(crate) capability_tags: Vec<String>,
    #[serde(default)]
    pub(crate) runtime_tags: Vec<String>,
    #[serde(default)]
    pub(crate) vendor_id: Option<String>,
    #[serde(default)]
    pub(crate) api_version: Option<String>,
    #[serde(default = "default_component_status")]
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) signing_algorithm: Option<String>,
    #[serde(default)]
    pub(crate) api_docs_url: Option<String>,
    #[serde(default)]
    pub(crate) updated_at: Option<String>,
    #[serde(default)]
    pub(crate) auth_modes: Vec<String>,
    #[serde(default)]
    pub(crate) translation_modes: Vec<ComponentTranslationModeSummary>,
    #[serde(default)]
    pub(crate) client_contract: Option<ComponentClientContractSummary>,
    #[serde(default = "default_component_product_id")]
    pub(crate) product_id: String,
    /// Server-side FK to `wptsall_wp_translation_providers.id`. None means
    /// this component is not bound to a specific WP translation provider
    /// (e.g. deployment templates).
    #[serde(default)]
    pub(crate) provider_id: Option<String>,
}

fn default_component_status() -> String {
    "active".to_string()
}

fn default_component_product_id() -> String {
    "wptsall".to_string()
}

pub(crate) fn default_component_enabled() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct ComponentTranslationModeSummary {
    pub(crate) id: String,
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) supported_content_formats: Vec<String>,
    #[serde(default)]
    pub(crate) api_docs_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct ComponentClientContractSummary {
    pub(crate) schema_version: String,
    pub(crate) task_kind: String,
    pub(crate) input_mode: String,
    pub(crate) workflow_mode: String,
    pub(crate) output_mode: String,
    #[serde(default)]
    pub(crate) stages: Vec<String>,
    #[serde(default)]
    pub(crate) request_body_type: Option<String>,
    #[serde(default)]
    pub(crate) source_upload_body_type: Option<String>,
    #[serde(default)]
    pub(crate) submit_response_type: Option<String>,
    #[serde(default)]
    pub(crate) result_transport: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ComponentManageData {
    pub(crate) component: ComponentItem,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct VendorItem {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) owner_type: String,
    #[serde(default)]
    pub(crate) website_url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VendorsPagedData {
    pub(crate) items: Vec<VendorItem>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WpTranslationProviderItem {
    pub id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) vendor_id: Option<String>,
    #[serde(default)]
    pub(crate) provider_kind: String,
    #[serde(default)]
    pub(crate) api_base_url: String,
    #[serde(default)]
    pub(crate) http_method: String,
    #[serde(default)]
    pub(crate) request_path: String,
    #[serde(default)]
    pub(crate) response_path: String,
    #[serde(default)]
    pub(crate) auth_mode: String,
    #[serde(default)]
    pub(crate) max_input_chars: i32,
    #[serde(default)]
    pub(crate) supported_languages: Vec<String>,
    #[serde(default)]
    pub(crate) visibility: String,
    #[serde(default)]
    pub(crate) active: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WpTranslationProvidersPagedData {
    pub(crate) items: Vec<WpTranslationProviderItem>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CloudApiTypeItem {
    pub id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) category: String,
    #[serde(default)]
    pub(crate) http_method: String,
    #[serde(default)]
    pub(crate) endpoint_pattern: String,
    #[serde(default)]
    pub(crate) auth_modes: Vec<String>,
    #[serde(default)]
    pub(crate) supported_content_formats: Vec<String>,
    #[serde(default)]
    pub(crate) output_artifact_kinds: Vec<String>,
    #[serde(default)]
    pub(crate) max_input_chars: i32,
    #[serde(default)]
    pub(crate) visibility: String,
    #[serde(default)]
    pub(crate) active: bool,
    #[serde(default)]
    pub(crate) owner_type: String,
    #[serde(default)]
    pub(crate) owner_email: Option<String>,
    #[serde(default)]
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) updated_at: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CloudApiTypesPagedData {
    pub(crate) items: Vec<CloudApiTypeItem>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ComponentDownloadData {
    pub(crate) component_id: String,
    #[serde(rename = "version")]
    pub(crate) _version: String,
    pub(crate) owner_type: String,
    pub(crate) template_json: Option<Value>,
    pub(crate) encrypted_payload: Option<String>,
    pub(crate) nonce: Option<String>,
    pub(crate) algorithm: Option<String>,
    pub(crate) kdf_version: Option<String>,
    #[serde(default)]
    pub(crate) signature: Option<String>,
    #[serde(default)]
    pub(crate) signing_key_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentTemplate {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) client_contract: Option<ComponentClientContractSummary>,
    #[serde(default)]
    pub(crate) default_values: Option<Value>,
    pub(crate) auth: Option<ComponentAuth>,
    #[serde(default)]
    pub(crate) prepare: Option<ComponentPrepare>,
    pub(crate) request: ComponentRequest,
    pub(crate) response: ComponentResponse,
    #[serde(default)]
    pub(crate) async_poll: Option<ComponentAsyncPoll>,
    #[serde(default)]
    pub(crate) source_upload: Option<ComponentSourceUpload>,
    #[serde(default)]
    pub(crate) sign: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) constraints: Option<ComponentConstraints>,
    #[serde(default)]
    pub(crate) editable_params: Vec<ComponentEditableParam>,
    #[serde(default)]
    pub(crate) translation_modes: Vec<ComponentTranslationMode>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentEditableParam {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) scope: Option<String>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub(crate) value_type: Option<String>,
    #[serde(default)]
    pub(crate) required: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub(crate) struct ComponentTranslationMode {
    pub(crate) id: String,
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) supported_content_formats: Vec<String>,
    #[serde(default)]
    pub(crate) request_overrides: Option<ComponentRequestOverrides>,
    #[serde(default)]
    pub(crate) constraints_overrides: Option<ComponentConstraints>,
    #[serde(default)]
    pub(crate) default_values_overrides: Option<Value>,
    #[serde(default)]
    pub(crate) api_docs_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentAuth {
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) modes: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) fields: Vec<ComponentAuthField>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentAuthField {
    pub(crate) name: String,
    pub(crate) required: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentRequest {
    pub(crate) method: String,
    pub(crate) url: String,
    pub(crate) headers: Option<HashMap<String, String>>,
    pub(crate) body: Option<Value>,
    #[serde(default)]
    pub(crate) body_type: Option<String>,
    #[serde(default)]
    pub(crate) response_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_limits: Option<ComponentHttpLimits>,
}

/// Optional independent HTTP phase limits. Absence preserves legacy serialization.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentHttpLimits {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_http_limit"
    )]
    pub(crate) max_response_bytes: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_http_limit"
    )]
    pub(crate) timeout_ms: Option<u64>,
}

fn deserialize_http_limit<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("HTTP limit must be an unsigned integer"))
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentPrepare {
    pub(crate) request: ComponentRequest,
    // map key -> JSON path; extracted value may be scalar or structured JSON.
    #[serde(default)]
    pub(crate) extract: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentResponse {
    pub(crate) translated_text_path: Option<String>,
    pub(crate) error_path: Option<String>,
    pub(crate) translated_ref_path: Option<String>,
    pub(crate) translated_media_ref_path: Option<String>,
    pub(crate) translated_image_ref_path: Option<String>,
    pub(crate) translated_video_ref_path: Option<String>,
    pub(crate) translated_audio_ref_path: Option<String>,
    pub(crate) translated_document_ref_path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentAsyncPoll {
    pub(crate) job_id_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reconcile: Option<ComponentAsyncReconcile>,
    #[serde(default)]
    pub(crate) submit_extract: HashMap<String, String>,
    pub(crate) request: ComponentRequest,
    #[serde(default)]
    pub(crate) status_path: Option<String>,
    #[serde(default)]
    pub(crate) pending_values: Vec<String>,
    #[serde(default)]
    pub(crate) done_values: Vec<String>,
    #[serde(default)]
    pub(crate) failed_values: Vec<String>,
    #[serde(default)]
    pub(crate) interval_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) timeout_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) result_ref_path: Option<String>,
    #[serde(default)]
    pub(crate) result_ref_template: Option<String>,
    #[serde(default)]
    pub(crate) result_request: Option<ComponentRequest>,
    #[serde(default)]
    pub(crate) result_download: Option<ComponentAsyncDownload>,
    #[serde(default)]
    pub(crate) result_text_path: Option<String>,
}

/// Optional provider-specific evidence query. This never authorizes a new submit.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComponentAsyncReconcile {
    pub(crate) request: ComponentRequest,
    pub(crate) matches_path: String,
    pub(crate) attempt_id_path: String,
    pub(crate) binding_path: String,
    pub(crate) job_id_path: String,
    pub(crate) status_path: String,
    pub(crate) resumable_values: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentAsyncDownload {
    pub(crate) method: String,
    pub(crate) url: String,
    pub(crate) headers: Option<HashMap<String, String>>,
    pub(crate) body: Option<Value>,
    #[serde(default)]
    pub(crate) body_type: Option<String>,
    #[serde(default)]
    pub(crate) filename: Option<String>,
    #[serde(default)]
    pub(crate) content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_limits: Option<ComponentHttpLimits>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct ComponentSourceUpload {
    pub(crate) method: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub(crate) body_type: Option<String>,
    #[serde(default)]
    pub(crate) success_statuses: Vec<u16>,
    #[serde(default)]
    pub(crate) extract: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_limits: Option<ComponentHttpLimits>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub(crate) struct ComponentConstraints {
    #[serde(default)]
    pub(crate) max_input_chars: Option<u64>,
    #[serde(default)]
    pub(crate) max_input_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) split_strategy: Option<String>,
    #[serde(default)]
    pub(crate) split_separator: Option<String>,
    #[serde(default)]
    pub(crate) batch_supported: Option<bool>,
    #[serde(default)]
    pub(crate) rate_limit_rpm: Option<u32>,
    #[serde(default)]
    pub(crate) rate_limit_qps: Option<u32>,
    #[serde(default)]
    pub(crate) supported_content_types: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub(crate) max_file_size_mb: Option<u32>,
    #[serde(default)]
    pub(crate) supported_formats: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) supported_content_formats: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) input_artifact_kind: Option<String>,
    #[serde(default)]
    pub(crate) output_artifact_kinds: Option<Vec<String>>,
    /// Template-level default language-code mapping (e.g. DeepL `zh` -> `ZH`).
    /// Applied by the runner to `input.*/payload.*` language placeholders; a
    /// binding-level language_map entry overrides any template default.
    #[serde(default)]
    pub(crate) language_map: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct EffectiveConstraints {
    pub(crate) max_input_chars: u64,
    pub(crate) split_strategy: String,
    pub(crate) split_separator: String,
    pub(crate) rate_limit_qps: u32,
    pub(crate) max_concurrent_requests: u32,
}

impl EffectiveConstraints {
    pub(crate) fn resolve(
        template_constraints: Option<&ComponentConstraints>,
        binding_override: Option<&ComponentConstraints>,
        task_params: Option<&serde_json::Value>,
        default_max_input_chars: u64,
        default_split_strategy: &str,
    ) -> Self {
        let max_input_chars = task_params
            .and_then(|p| p.get("max_input_chars"))
            .and_then(|v| v.as_u64())
            .or_else(|| binding_override.and_then(|c| c.max_input_chars))
            .or_else(|| template_constraints.and_then(|c| c.max_input_chars))
            .unwrap_or(default_max_input_chars);

        let split_strategy = task_params
            .and_then(|p| p.get("split_strategy"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| binding_override.and_then(|c| c.split_strategy.clone()))
            .or_else(|| template_constraints.and_then(|c| c.split_strategy.clone()))
            .unwrap_or_else(|| default_split_strategy.to_string());

        let split_separator = task_params
            .and_then(|p| p.get("split_separator"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| binding_override.and_then(|c| c.split_separator.clone()))
            .or_else(|| template_constraints.and_then(|c| c.split_separator.clone()))
            .unwrap_or_else(|| match split_strategy.as_str() {
                "paragraph" => "\n\n".to_string(),
                "sentence" => "".to_string(),
                _ => "".to_string(),
            });

        let rate_limit_qps = task_params
            .and_then(|p| p.get("rate_limit_qps"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .or_else(|| binding_override.and_then(|c| c.rate_limit_qps))
            .or_else(|| template_constraints.and_then(|c| c.rate_limit_qps))
            .unwrap_or(0);

        let max_concurrent_requests = binding_override
            .and_then(|c| c.max_concurrent_requests)
            .or_else(|| template_constraints.and_then(|c| c.max_concurrent_requests))
            .unwrap_or(0);

        Self {
            max_input_chars,
            split_strategy,
            split_separator,
            rate_limit_qps,
            max_concurrent_requests,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ComponentRuntime {
    pub(crate) template: ComponentTemplate,
    pub(crate) auth_values: HashMap<String, String>,
    pub(crate) supported_business_lines: Vec<String>,
    pub(crate) language_map: HashMap<String, String>,
    pub(crate) supported_content_formats: Vec<String>,
    pub(crate) supported_formats: Vec<String>,
    pub(crate) key_pool: Option<std::sync::Arc<crate::component_rt::key_pool::KeyPool>>,
    pub(crate) oauth_pool: Option<std::sync::Arc<crate::component_rt::oauth::OAuthPool>>,
    pub(crate) oauth_manager: Option<std::sync::Arc<crate::component_rt::oauth::OAuthTokenManager>>,
    pub(crate) proxy_profile_id: Option<String>,
    pub(crate) runtime_max_concurrent_requests: u32,
    pub(crate) runtime_min_interval_ms: u64,
    pub(crate) runtime_concurrency_sem: Option<Arc<tokio::sync::Semaphore>>,
    pub(crate) runtime_last_request_at: Option<Arc<Mutex<std::time::Instant>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ComponentRuntimeRegistry {
    pub(crate) runtimes: HashMap<String, ComponentRuntime>,
    pub(crate) ordered_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct ComponentBindingsDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) components: HashMap<String, ComponentBindingEntry>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct ComponentBindingEntry {
    #[serde(default)]
    pub(crate) auth: HashMap<String, String>,
    #[serde(default)]
    pub(crate) template_id: Option<String>,
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) language_map: HashMap<String, String>,
    #[serde(default)]
    pub(crate) constraints_override: Option<ComponentConstraints>,
    #[serde(default)]
    pub(crate) request_overrides: Option<ComponentRequestOverrides>,
    #[serde(default)]
    pub(crate) default_values_override: Option<Value>,
    #[serde(default)]
    pub(crate) key_ids: Vec<String>,
    #[serde(default)]
    pub(crate) oauth_ids: Vec<String>,
    #[serde(default = "default_auth_strategy")]
    pub(crate) auth_strategy: KeySelectionStrategy,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct ComponentRequestOverrides {
    #[serde(default)]
    pub(crate) method: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub(crate) body: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct DomainTokenBindingsDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) domains: HashMap<String, DomainTokenBindingEntry>,
}

/// Identity Contract v1.1 §1 (`tasks/wpmmcc/contract/identity-v1/`):
/// canonical plugin identity. Wire/disk values are byte-exact snake_case
/// strings; parsing is strict — anything outside the enum is `None`
/// (`identity_unknown`, fail-closed). Never build this value at runtime
/// by concatenation or transformation.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginIdentity {
    #[serde(rename = "wpmmcc_ats")]
    WpmmccAts,
    #[serde(rename = "wpmmcc")]
    Wpmmcc,
}

impl PluginIdentity {
    /// Strict wire-string parse (contract §1 rule 2): exact bytes only.
    /// No prefix or fuzzy matching, ever.
    pub(crate) fn from_wire_str(value: &str) -> Option<Self> {
        match value {
            "wpmmcc_ats" => Some(PluginIdentity::WpmmccAts),
            "wpmmcc" => Some(PluginIdentity::Wpmmcc),
            _ => None,
        }
    }

    /// Canonical wire string (matches the serde rename, byte-exact).
    pub(crate) fn as_wire_str(self) -> &'static str {
        match self {
            PluginIdentity::WpmmccAts => "wpmmcc_ats",
            PluginIdentity::Wpmmcc => "wpmmcc",
        }
    }
}

/// Snapshot of the identity verification response (contract §4): kept on the
/// binding entry as version-negotiation input. ATS pings do not carry the
/// protocol fields; those stay absent rather than defaulted.
#[derive(Debug, Serialize, Deserialize, Default, Clone, PartialEq, Eq)]
pub(crate) struct IdentityCapabilities {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) plugin_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) protocol_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) protocol_current: Option<u32>,
}

/// Lenient disk-side parse for `plugin_identity`: an out-of-enum stored value
/// maps to `None` (pending re-verification) instead of failing the whole
/// bindings file. Wire-side unknowns are handled by
/// `PluginIdentity::from_wire_str` and the `identity_unknown` gate code.
fn deserialize_plugin_identity_lenient<'de, D>(
    deserializer: D,
) -> Result<Option<PluginIdentity>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: Option<String> = Option::<String>::deserialize(deserializer)?;
    Ok(raw.as_deref().and_then(PluginIdentity::from_wire_str))
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct DomainTokenBindingEntry {
    #[serde(default)]
    pub(crate) wp_client_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) route_secret: String,
    /// Identity Contract v1.1 §4 (C-1): verified plugin identity for this
    /// binding. `None` = never verified (or unknown stored value).
    #[serde(
        default,
        deserialize_with = "deserialize_plugin_identity_lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) plugin_identity: Option<PluginIdentity>,
    /// RFC3339 UTC timestamp of the last successful identity verification.
    /// `None` = pending verification (contract §4: never written before a
    /// successful verify).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) identity_verified_at: Option<String>,
    /// Snapshot of the verification response (contract §4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) identity_capabilities: Option<IdentityCapabilities>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct DomainTokenBindingStatusItem {
    pub(crate) api_base_url: String,
    pub(crate) token_prefix: String,
    pub(crate) token_len: usize,
    /// Presence only; the route secret is never part of a status response.
    pub(crate) route_secret_set: bool,
    /// Identity Contract v1.1 §4: verified plugin identity of this binding
    /// (wire value, e.g. "wpmmcc_ats" / "wpmmcc"); null while unverified.
    /// Non-credential field — feeds the Sites identity badge (C-6) and the
    /// identity-chain integration gate assertions.
    pub(crate) plugin_identity: Option<String>,
    /// RFC3339 timestamp of the last successful identity verification;
    /// null = pending first verification.
    pub(crate) identity_verified_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct TaskTypeComponentBindingsDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) task_types: HashMap<String, TaskTypeComponentBindingEntry>,
    #[serde(default)]
    pub(crate) business_line_task_types: HashMap<String, TaskTypeComponentBindingEntry>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub(crate) struct TaskTypeComponentBindingEntry {
    #[serde(default)]
    pub(crate) component_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct TaskTypeComponentBindingStatusItem {
    pub(crate) business_line: String,
    pub(crate) task_type: String,
    pub(crate) component_id: String,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct RuleComponentBindingStatusItem {
    pub(crate) scope: String,
    pub(crate) scope_key: String,
    pub(crate) slot_key: String,
    pub(crate) component_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct EncryptedBindingsDoc {
    pub(crate) format: String,
    pub(crate) algorithm: String,
    pub(crate) nonce: String,
    pub(crate) payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct RuleComponentBindingsDoc {
    #[serde(default = "default_bindings_version")]
    pub version: u32,
    #[serde(default)]
    pub(crate) global_defaults: HashMap<String, String>,
    /// v1 numeric relation_id → slots. Cleared on successful v2 migration; never applied from public packs.
    #[serde(default)]
    pub(crate) relation_bindings: HashMap<String, HashMap<String, String>>,
    #[serde(default)]
    pub(crate) plugin_bindings: HashMap<String, HashMap<String, String>>,
    /// v1 numeric rule_id → slots. Cleared on successful v2 migration; never applied from public packs.
    #[serde(default)]
    pub(crate) rule_bindings: HashMap<String, HashMap<String, String>>,
    /// P1-D v2 site-scoped semantic bindings.
    #[serde(default)]
    pub(crate) site_bindings: HashMap<String, SiteBindingEntry>,
    #[serde(default)]
    pub(crate) migration_issues: Vec<BindingMigrationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct SiteBindingEntry {
    #[serde(default)]
    pub(crate) site_ref: SiteRef,
    #[serde(default)]
    pub(crate) relations: HashMap<String, RelationBindingEntry>,
    #[serde(default)]
    pub(crate) rules: HashMap<String, RuleBindingEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct SiteRef {
    #[serde(default)]
    pub(crate) site_origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct RelationBindingEntry {
    pub(crate) semantic_key: String,
    #[serde(default)]
    pub(crate) relation_ref: RelationRef,
    #[serde(default)]
    pub(crate) legacy_id_hint: Option<u64>,
    #[serde(default)]
    pub(crate) slots: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct RelationRef {
    #[serde(default)]
    pub(crate) source_lang: String,
    #[serde(default)]
    pub(crate) target_lang: String,
    #[serde(default)]
    pub(crate) target_site_type: String,
    #[serde(default)]
    pub(crate) template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct RuleBindingEntry {
    pub(crate) semantic_key: String,
    #[serde(default)]
    pub(crate) relation_key: String,
    #[serde(default)]
    pub(crate) rule_ref: RuleRef,
    #[serde(default)]
    pub(crate) legacy_id_hint: Option<u64>,
    #[serde(default)]
    pub(crate) slots: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct RuleRef {
    #[serde(default)]
    pub(crate) plugin_slug: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) data_type: String,
    #[serde(default)]
    pub(crate) object_name: String,
    #[serde(default)]
    pub(crate) source_group: String,
    #[serde(default)]
    pub(crate) routing_profile: String,
    #[serde(default)]
    pub(crate) delivery_target: String,
    #[serde(default)]
    pub(crate) content_format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BindingMigrationIssue {
    pub(crate) code: String,
    pub(crate) scope: String,
    pub(crate) scope_key: String,
    #[serde(default)]
    pub(crate) detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ComponentSemanticRef {
    #[serde(default)]
    pub(crate) vendor_id: String,
    #[serde(default)]
    pub(crate) template_id: String,
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ProxyProfile {
    pub(crate) name: String,
    pub(crate) protocol: String,
    pub(crate) host: String,
    pub(crate) port: u16,
    #[serde(default)]
    pub(crate) username: String,
    #[serde(default)]
    pub(crate) password: String,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ProxyProfilesDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) profiles: HashMap<String, ProxyProfile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct VendorKey {
    pub(crate) vendor_id: String,
    pub(crate) label: String,
    pub(crate) auth_values: HashMap<String, String>,
    #[serde(default = "default_max_concurrent")]
    pub(crate) max_concurrent: usize,
    #[serde(default = "default_rps")]
    pub(crate) requests_per_second: f64,
    #[serde(default = "default_weight")]
    pub(crate) weight: u32,
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) max_input_chars: usize,
    #[serde(default)]
    pub(crate) max_file_size_mb: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct VendorKeysDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) keys: HashMap<String, VendorKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OAuthConfig {
    pub(crate) vendor_id: String,
    pub(crate) label: String,
    pub(crate) grant_type: String,
    #[serde(default)]
    pub(crate) auth_url: String,
    pub(crate) token_url: String,
    pub(crate) client_id: String,
    #[serde(default)]
    pub(crate) client_secret: String,
    #[serde(default)]
    pub(crate) scopes: String,
    #[serde(default)]
    pub(crate) extra_params: HashMap<String, String>,
    #[serde(default)]
    pub(crate) auth_extra_params: HashMap<String, String>,
    #[serde(default)]
    pub(crate) cached_token: Option<String>,
    #[serde(default)]
    pub(crate) cached_token_expires_at: i64,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
    #[serde(default = "default_max_concurrent")]
    pub(crate) max_concurrent: usize,
    #[serde(default = "default_weight")]
    pub(crate) weight: u32,
    #[serde(default)]
    pub(crate) max_input_chars: usize,
    #[serde(default)]
    pub(crate) max_file_size_mb: f64,
    #[serde(default = "default_token_field")]
    pub(crate) token_field: String,
}

#[derive(Debug, Clone)]
pub(crate) struct VendorOAuthPendingEntry {
    pub(crate) config_id: String,
    pub(crate) code_verifier: String,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct VendorOAuthDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) configs: HashMap<String, OAuthConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ComponentVersion {
    pub(crate) version: String,
    #[serde(default)]
    pub(crate) remarks: String,
    #[serde(default)]
    pub(crate) key_ids: Vec<String>,
    #[serde(default = "default_key_strategy")]
    pub(crate) key_selection_strategy: String,
    #[serde(default)]
    pub(crate) proxy_profile_id: Option<String>,
    #[serde(default = "default_auth_type")]
    pub(crate) auth_type: String,
    #[serde(default)]
    pub(crate) config_overrides: HashMap<String, serde_json::Value>,
    pub(crate) created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ComponentInstanceLocal {
    pub(crate) name: String,
    pub(crate) template_id: String,
    #[serde(default)]
    pub(crate) source_template_id: String,
    #[serde(default)]
    pub(crate) source_template_updated_at: Option<String>,
    #[serde(default)]
    pub(crate) source_template_api_version: Option<String>,
    #[serde(default)]
    pub(crate) vendor_id: String,
    #[serde(default)]
    pub(crate) vendor_name: String,
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) remarks: String,
    #[serde(default = "default_component_enabled")]
    pub(crate) enabled: bool,
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) updated_at: Option<String>,
    #[serde(default)]
    pub(crate) component_overrides: Option<ComponentInstanceOverrides>,
    #[serde(default)]
    pub(crate) versions: HashMap<String, ComponentVersion>,
    #[serde(default)]
    pub(crate) active_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) template_json: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ComponentsLocalDoc {
    #[serde(default = "default_bindings_version")]
    pub(crate) version: u32,
    #[serde(default)]
    pub(crate) components: HashMap<String, ComponentInstanceLocal>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ComponentInstanceOverrides {
    #[serde(default)]
    pub(crate) constraints_override: Option<ComponentConstraints>,
    #[serde(default)]
    pub(crate) request_overrides: Option<ComponentRequestOverrides>,
    #[serde(default)]
    pub(crate) default_values_override: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) http_limits_overrides:
        Option<std::collections::BTreeMap<ComponentHttpPhase, ComponentHttpLimits>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum ComponentHttpPhase {
    #[serde(rename = "request")]
    Request,
    #[serde(rename = "prepare.request")]
    Prepare,
    #[serde(rename = "async_poll.request")]
    Poll,
    #[serde(rename = "async_poll.result_request")]
    ResultRequest,
    #[serde(rename = "async_poll.result_download")]
    ResultDownload,
    #[serde(rename = "async_poll.reconcile.request")]
    Reconcile,
    #[serde(rename = "source_upload")]
    SourceUpload,
}

impl ComponentHttpPhase {
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Prepare => "prepare.request",
            Self::Poll => "async_poll.request",
            Self::ResultRequest => "async_poll.result_request",
            Self::ResultDownload => "async_poll.result_download",
            Self::Reconcile => "async_poll.reconcile.request",
            Self::SourceUpload => "source_upload",
        }
    }

    pub(crate) fn from_path(path: &str) -> Option<Self> {
        match path {
            "request" => Some(Self::Request),
            "prepare.request" => Some(Self::Prepare),
            "async_poll.request" => Some(Self::Poll),
            "async_poll.result_request" => Some(Self::ResultRequest),
            "async_poll.result_download" => Some(Self::ResultDownload),
            "async_poll.reconcile.request" => Some(Self::Reconcile),
            "source_upload" => Some(Self::SourceUpload),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for ComponentHttpPhase {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let path = String::deserialize(deserializer)?;
        Self::from_path(&path)
            .ok_or_else(|| serde::de::Error::custom("invalid component HTTP phase"))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum KeySelectionStrategy {
    #[serde(alias = "random")]
    Random,
    #[default]
    #[serde(alias = "round_robin")]
    RoundRobin,
    #[serde(alias = "weighted")]
    Weighted,
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_max_concurrent() -> usize {
    5
}

pub(crate) fn default_rps() -> f64 {
    3.0
}

pub(crate) fn default_weight() -> u32 {
    1
}

pub(crate) fn default_key_strategy() -> String {
    "round_robin".into()
}

pub(crate) fn default_auth_type() -> String {
    "key".into()
}

pub(crate) fn default_token_field() -> String {
    "access_token".to_string()
}

pub(crate) fn default_auth_strategy() -> KeySelectionStrategy {
    KeySelectionStrategy::RoundRobin
}

pub(crate) fn default_bindings_version() -> u32 {
    1
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_selection_strategy_accepts_version_editor_values_and_keeps_snapshot_wire() {
        for (wire, expected) in [
            ("round_robin", KeySelectionStrategy::RoundRobin),
            ("random", KeySelectionStrategy::Random),
            ("weighted", KeySelectionStrategy::Weighted),
        ] {
            let parsed: KeySelectionStrategy = serde_json::from_value(serde_json::json!(wire))
                .expect("version editor strategy must remain usable");
            assert_eq!(parsed, expected);
            let canonical = serde_json::to_value(&expected).unwrap();
            assert_eq!(
                serde_json::from_value::<KeySelectionStrategy>(canonical).unwrap(),
                parsed
            );
        }
        for invalid in ["", "round-robin", "unsupported"] {
            assert!(
                serde_json::from_value::<KeySelectionStrategy>(serde_json::json!(invalid)).is_err()
            );
        }
    }

    #[test]
    fn component_item_decodes_provider_id_when_present() {
        // Server returns a component bound to a specific WP provider.
        let json = r#"{
            "id": "official-baidu-translate-v1",
            "name": "Baidu Translate",
            "owner_type": "official",
            "version": "1.0.0",
            "type": "text_translation",
            "product_id": "wptsall",
            "provider_id": "baidu-translate"
        }"#;
        let item: ComponentItem = serde_json::from_str(json).expect("decode");
        assert_eq!(item.product_id, "wptsall");
        assert_eq!(item.provider_id.as_deref(), Some("baidu-translate"));
    }

    #[test]
    fn component_item_provider_id_defaults_to_none_when_absent() {
        // Backward compat: old server payloads (pre provider_id column)
        // should still decode and report provider_id = None.
        let json = r#"{
            "id": "official-claude-haiku-translate-v1",
            "name": "Claude Haiku Translate",
            "owner_type": "official",
            "version": "1.0.0",
            "type": "text_translation",
            "product_id": "wptsall"
        }"#;
        let item: ComponentItem = serde_json::from_str(json).expect("decode");
        assert_eq!(item.product_id, "wptsall");
        assert!(item.provider_id.is_none());
    }

    #[test]
    fn component_item_product_id_defaults_to_wptsall_when_absent() {
        // Old server payloads (pre product_id column) should default to "wptsall".
        let json = r#"{
            "id": "official-google-translate-v1",
            "name": "Google Translate",
            "owner_type": "official",
            "version": "1.0.0",
            "type": "text_translation"
        }"#;
        let item: ComponentItem = serde_json::from_str(json).expect("decode");
        assert_eq!(item.product_id, "wptsall");
        assert!(item.provider_id.is_none());
    }

    #[test]
    fn component_item_provider_id_null_decodes_as_none() {
        // Explicit JSON null must decode as None, not panic.
        let json = r#"{
            "id": "official-aws-translate-v1",
            "name": "AWS Translate",
            "owner_type": "official",
            "version": "1.0.0",
            "type": "text_translation",
            "product_id": "wptsall",
            "provider_id": null
        }"#;
        let item: ComponentItem = serde_json::from_str(json).expect("decode");
        assert!(item.provider_id.is_none());
    }
}