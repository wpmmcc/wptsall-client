use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::{
    ComponentBindingsDoc, ComponentConstraints, ComponentItem, ComponentRequestOverrides,
    DomainStatusItem, DomainTokenBindingsDoc, KeySelectionStrategy, RuleComponentBindingsDoc,
    TaskTypeComponentBindingsDoc, VendorOAuthPendingEntry,
};

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub task_pull_statuses: Vec<String>,
    pub task_concurrency: usize,
    pub retry_max: u32,
    pub retry_base_ms: u64,
    pub retry_max_ms: u64,
    pub component_fallback_enabled: bool,
    pub default_max_input_chars: u64,
    pub default_split_strategy: String,
    pub discovery_mode: bool,
    pub review_mode: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskRunReport {
    pub(crate) task_id: i64,
    pub(crate) final_status: String,
    pub(crate) elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskTextFieldUnit {
    pub(crate) key: String,
    pub(crate) source_text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskContentSubtaskUnit {
    pub(crate) task_type: String,
    pub(crate) key: String,
    pub(crate) source_text: String,
    pub(crate) source_payload: Value,
    pub(crate) content_format: String,
}

#[derive(Debug, Clone)]
pub(crate) struct FragmentExecutionOutcome {
    pub(crate) translated: String,
    pub(crate) fallback_used: bool,
    pub(crate) component_id: String,
    pub(crate) error_code: String,
    pub(crate) error_message: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NonTextComponentOutcome {
    pub(crate) translated_ref: String,
    pub(crate) translated_text: String,
}

#[derive(Debug, Clone, Default)]
pub struct DomainRunReport {
    pub api_base_url: String,
    pub pulled: usize,
    pub processed: usize,
    pub completed: usize,
    pub retried: usize,
    pub failed: usize,
    pub status_update_failed: usize,
    pub avg_elapsed_ms: u64,
}

pub(crate) struct AtomicRunCounters {
    pub(crate) processed: AtomicUsize,
    pub(crate) completed: AtomicUsize,
    pub(crate) failed: AtomicUsize,
}

impl AtomicRunCounters {
    pub(crate) fn new() -> Self {
        Self {
            processed: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
        }
    }

    pub(crate) fn snapshot(&self, api_base_url: &str) -> DomainRunReport {
        use std::sync::atomic::Ordering::Relaxed;

        DomainRunReport {
            api_base_url: api_base_url.to_string(),
            pulled: 0,
            processed: self.processed.load(Relaxed),
            completed: self.completed.load(Relaxed),
            retried: 0,
            failed: self.failed.load(Relaxed),
            status_update_failed: 0,
            avg_elapsed_ms: 0,
        }
    }
}

#[derive(Clone)]
pub(crate) struct WebUiState {
    pub(crate) server_base: String,
    pub(crate) device_id: String,
    pub(crate) session_token: Option<String>,
    pub(crate) oauth_code_verifier: Option<String>,
    pub(crate) oauth_state: Option<String>,
    pub(crate) domains: Vec<DomainStatusItem>,
    pub(crate) components: Vec<ComponentItem>,
    pub(crate) component_bindings_path: String,
    pub(crate) component_bindings: ComponentBindingsDoc,
    pub(crate) domain_token_bindings_path: String,
    pub(crate) domain_token_bindings: DomainTokenBindingsDoc,
    pub(crate) task_type_component_bindings_path: String,
    pub(crate) task_type_component_bindings: TaskTypeComponentBindingsDoc,
    pub(crate) rule_component_bindings_path: String,
    pub(crate) rule_component_bindings: RuleComponentBindingsDoc,
    pub(crate) worker_loop_running: bool,
    pub(crate) worker_status: String,
    pub(crate) worker_loop_poll_seconds: u64,
    pub(crate) worker_last_summary: Value,
    pub(crate) worker_recent_runs: Vec<WebUiWorkerRunRecord>,
    pub(crate) local_components_backfilled: usize,
    pub(crate) local_components_backfill_error: String,
    pub(crate) last_error: String,
    pub(crate) last_event: String,
    pub(crate) updated_at: u64,
    pub(crate) log_enabled: bool,
    pub(crate) log_min_level: String,
    pub(crate) vendor_oauth_pending: HashMap<String, VendorOAuthPendingEntry>,
    pub(crate) db: Arc<Mutex<rusqlite::Connection>>,
    pub(crate) http_client: reqwest::Client,
    pub(crate) update_in_progress: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiUpsertBindingRequest {
    pub(crate) component_id: Option<String>,
    pub(crate) auth: Option<HashMap<String, String>>,
    #[serde(default)]
    pub(crate) key_ids: Vec<String>,
    #[serde(default)]
    pub(crate) oauth_ids: Vec<String>,
    pub(crate) auth_strategy: Option<KeySelectionStrategy>,
    #[serde(default)]
    pub(crate) constraints_override: Option<ComponentConstraints>,
    #[serde(default)]
    pub(crate) request_overrides: Option<ComponentRequestOverrides>,
    #[serde(default)]
    pub(crate) default_values_override: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiDeleteBindingRequest {
    pub(crate) component_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiUpsertTaskTypeComponentRequest {
    pub(crate) business_line: Option<String>,
    pub(crate) task_type: Option<String>,
    pub(crate) component_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiDeleteTaskTypeComponentRequest {
    pub(crate) business_line: Option<String>,
    pub(crate) task_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiUpsertRuleComponentBindingRequest {
    pub(crate) scope: Option<String>,
    pub(crate) scope_key: Option<String>,
    pub(crate) slot_key: Option<String>,
    pub(crate) component_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiDeleteRuleComponentBindingRequest {
    pub(crate) scope: Option<String>,
    pub(crate) scope_key: Option<String>,
    pub(crate) slot_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiUpsertDomainTokenRequest {
    pub(crate) api_base_url: Option<String>,
    pub(crate) existing_api_base_url: Option<String>,
    pub(crate) wp_client_token: Option<String>,
    pub(crate) route_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiDeleteDomainTokenRequest {
    pub(crate) api_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiWorkerConfigRequest {
    pub(crate) poll_seconds: Option<u64>,
    pub(crate) auto_start_worker: Option<bool>,
    pub(crate) domain_concurrency: Option<u64>,
    pub(crate) relation_concurrency: Option<u64>,
    pub(crate) global_translation_concurrency: Option<u64>,
    pub(crate) global_callback_concurrency: Option<u64>,
    pub(crate) relation_max_pending_callbacks: Option<u64>,
    pub(crate) adaptive_rate_control: Option<bool>,
    pub(crate) adaptive_max_delay_ms: Option<u64>,
    pub(crate) callback_concurrency: Option<u64>,
    pub(crate) callback_timeout_secs: Option<u64>,
    pub(crate) callback_retry_max: Option<u64>,
    pub(crate) fetch_timeout_secs: Option<u64>,
    pub(crate) fetch_retry_max: Option<u64>,
    pub(crate) review_mode: Option<bool>,
    /// Optional scoped policy JSON (workflow-policy-v1). When set, stored as
    /// system_config.workflow_policy and overrides the global review_mode flag
    /// for domain/format/rule resolution.
    #[serde(default)]
    pub(crate) workflow_policy: Option<Value>,
    /// Optional workflow DSL (workflow-dsl-v1). Stored as system_config.workflow_dsl.
    #[serde(default)]
    pub(crate) workflow_dsl: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiUpdateDiscoveryTaskRequest {
    pub(crate) concurrency: Option<i64>,
    pub(crate) batch_parallel: Option<i64>,
    pub(crate) per_page: Option<i64>,
    pub(crate) retry_max: Option<i64>,
    pub(crate) timeout_secs: Option<i64>,
    pub(crate) enabled: Option<bool>,
    pub(crate) include_resync: Option<bool>,
    pub(crate) selected_component_id: Option<String>,
    pub(crate) effective_source_lang: Option<String>,
    pub(crate) effective_target_lang: Option<String>,
    pub(crate) editable_overrides: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiComponentTemplateRequest {
    pub(crate) component_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiLogsRequest {
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiLogSettingsRequest {
    pub(crate) enabled: Option<bool>,
    pub(crate) level: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WebUiBatchTranslationRequest {
    #[serde(default)]
    pub(crate) ids: Vec<i64>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct WebUiWorkerRunRecord {
    pub(crate) ts: u64,
    pub(crate) status: String,
    pub(crate) summary: Value,
    pub(crate) error: String,
}

#[derive(Clone)]
pub(crate) struct WebUiRuntimeControl {
    pub(crate) worker_running: Arc<AtomicBool>,
    pub(crate) worker_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl WebUiRuntimeControl {
    pub(crate) fn new() -> Self {
        Self {
            worker_running: Arc::new(AtomicBool::new(false)),
            worker_handle: Arc::new(Mutex::new(None)),
        }
    }
}
