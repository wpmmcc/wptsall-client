// ===================== 通用响应 =====================
export interface ApiOk<T> {
  success: true;
  data: T;
  error?: undefined;
}
export interface ApiErr {
  success: false;
  data?: undefined;
  error: {
    code: string;
    message: string;
    status?: number;
  };
}
export type ApiResult<T> = ApiOk<T> | ApiErr;

// ===================== 状态 =====================
export interface DomainStatusItem {
  api_base_url: string;
  site_status: string;
  route_secret_set?: boolean;
  max_relations?: number;
  plan_expires_at?: string | null;
}

export interface ComponentItem {
  id: string;
  name: string;
  description?: string;
  product_id?: string;
  owner_type: string;
  version: string;
  kind: string;
  status?: string;
  supported_business_lines: string[];
  supported_content_formats?: string[];
  supported_formats?: string[];
  max_file_size_mb?: number | null;
  template_group?: string;
  catalog_family?: string;
  catalog_subfamily?: string;
  capability_tags?: string[];
  runtime_tags?: string[];
  vendor_id?: string;
  api_version?: string;
  signing_algorithm?: string;
  api_docs_url?: string;
  auth_modes?: string[];
  translation_modes?: Array<{
    id: string;
    label: string;
    supported_content_formats?: string[];
    api_docs_url?: string;
  }>;
  size_class?: string;
  updated_at?: string | null;
}

export interface ComponentBindingEntry {
  auth: Record<string, string>;
  template_id?: string;
  type?: string;
  name?: string;
  language_map: Record<string, string>;
  constraints_override?: {
    max_input_chars?: number;
    max_input_bytes?: number;
    split_strategy?: string;
    split_separator?: string;
    batch_supported?: boolean;
    rate_limit_rpm?: number;
    rate_limit_qps?: number;
    supported_content_types?: string[];
    max_concurrent_requests?: number;
    max_file_size_mb?: number;
    supported_formats?: string[];
    supported_content_formats?: string[];
  };
  request_overrides?: {
    url?: string;
    headers?: Record<string, string>;
    body?: unknown;
  };
}

export interface ComponentBindingsDoc {
  version: number;
  components: Record<string, ComponentBindingEntry>;
}

export interface DomainTokenBindingStatusItem {
  api_base_url: string;
  token_prefix: string;
  token_len: number;
  route_secret_set?: boolean;
}

export interface TaskTypeComponentBindingStatusItem {
  business_line: string;
  task_type: string;
  component_id: string;
}

export interface RuleComponentBindingStatusItem {
  scope: 'global' | 'plugin' | 'relation' | 'rule' | string;
  scope_key: string;
  slot_key: string;
  component_id: string;
}

export interface WorkerRunSummary {
  domain_count?: number;
  domains_total?: number;
  domains_processed?: number;
  tasks_processed?: number;
  tasks_succeeded?: number;
  tasks_failed?: number;
  total_items?: number;
  pulled?: number;
  processed?: number;
  completed?: number;
  retried?: number;
  failed?: number;
  status_update_failed?: number;
  domains?: DomainRunDetail[];
  summary?: {
    domain_count?: number;
    domains_total?: number;
    skipped_missing_token?: number;
    missing_token_domains?: string[];
    skipped_missing_route_secret?: number;
    missing_route_secret_domains?: string[];
    domains?: DomainRunDetail[];
  };
}

export interface DomainRunDetail {
  api_base_url: string;
  pulled: number;
  processed: number;
  completed: number;
  failed: number;
  status_update_failed: number;
}

export interface WorkerRunRecord {
  ts: number;
  status: string;
  summary: WorkerRunSummary;
  error: string;
}

export interface WebUiStatus {
  /** P0-LF-04: "local" (default) or "legacy_server_control_plane" (§5.1 projection). */
  runtime_mode?: string;
  server_base: string;
  device_id: string;
  logged_in: boolean;
  session_token_prefix: string | null;
  domains: DomainStatusItem[];
  components: ComponentItem[];
  component_bindings_path: string;
  component_bindings: ComponentBindingsDoc;
  domain_token_bindings_path: string;
  domain_token_bindings: DomainTokenBindingStatusItem[];
  task_type_component_bindings_path: string;
  task_type_component_bindings: TaskTypeComponentBindingStatusItem[];
  rule_component_bindings_path: string;
  rule_component_bindings: RuleComponentBindingStatusItem[];
  worker_loop_running: boolean;
  worker_status?: string;
  worker_loop_poll_seconds: number;
  worker_last_summary: WorkerRunSummary;
  worker_recent_runs: WorkerRunRecord[];
  local_components_backfilled?: number;
  local_components_backfill_error?: string;
  last_error: string;
  last_event: string;
  updated_at: number;
}

// ===================== 组件模板 =====================
export interface AuthField { name: string; required: boolean; }

export interface ComponentTemplateData {
  component_id: string;
  template_name: string;
  template_version: string;
  template_type: string;
  auth_modes?: string[];
  auth_fields: AuthField[];
  template_json: unknown;
}

// ===================== 本地组件 v2 =====================
export interface ComponentVersion {
  version: string;
  remarks: string;
  key_ids: string[];
  key_selection_strategy: string;
  proxy_profile_id?: string;
  auth_type: string;
  config_overrides: Record<string, unknown>;
  created_at: string;
}

export interface LocalComponent {
  id: string;
  name: string;
  template_id: string;
  source_template_id?: string;
  source_template_updated_at?: string | null;
  source_template_api_version?: string | null;
  vendor_id: string;
  vendor_name: string;
  kind: string;
  capability?: string;
  remarks: string;
  enabled: boolean;
  created_at: string;
  updated_at?: string | null;
  component_overrides?: {
    constraints_override?: ComponentBindingEntry['constraints_override'];
    request_overrides?: ComponentBindingEntry['request_overrides'];
    default_values_override?: Record<string, unknown>;
  } | null;
  versions: Record<string, ComponentVersion>;
  template_json?: Record<string, unknown> | null;
}

export interface LocalComponentsPage {
  items: LocalComponent[];
  page: number;
  per_page: number;
  total: number;
  total_pages: number;
}

// ===================== Vendor =====================
export interface VendorItem {
  id: string;
  name: string;
  description: string;
  owner_type: string;
  website_url: string;
}

export interface WpTranslationProviderItem {
  id: string;
  name: string;
  description: string;
  vendor_id?: string | null;
  provider_kind: string;
  api_base_url: string;
  http_method: string;
  request_path: string;
  response_path: string;
  auth_mode: string;
  max_input_chars: number;
  supported_languages: string[];
  visibility: string;
  active: boolean;
}

export interface CloudApiTypeItem {
  id: string;
  name: string;
  description: string;
  category: string;
  http_method: string;
  endpoint_pattern: string;
  auth_modes: string[];
  supported_content_formats: string[];
  output_artifact_kinds: string[];
  max_input_chars: number;
  visibility: string;
  active: boolean;
  owner_type: string;
  owner_email?: string | null;
  created_at: string;
  updated_at: string;
}

// ===================== Vendor Keys =====================
export interface VendorKeyItem {
  id: string;
  vendor_id: string;
  label: string;
  auth_keys: string[];
  max_concurrent: number;
  requests_per_second: number;
  weight: number;
  enabled: boolean;
  max_input_chars?: number;
  max_file_size_mb?: number;
}

// ===================== OAuth =====================
export interface OAuthConfigItem {
  id: string;
  vendor_id: string;
  label: string;
  grant_type: string;
  auth_url?: string;
  token_url: string;
  client_id: string;
  scopes: string;
  auth_extra_params?: Record<string, string>;
  extra_params?: Record<string, string>;
  max_concurrent?: number;
  weight?: number;
  max_input_chars?: number;
  max_file_size_mb?: number;
  token_field?: string;
  has_token: boolean;
  has_refresh_token?: boolean;
  token_expires_at?: number;
}

// ===================== Proxy =====================
export interface ProxyProfileItem {
  id: string;
  name: string;
  protocol: string;
  host: string;
  port: number;
  has_auth: boolean;
  enabled: boolean;
}

// ===================== Update =====================
export interface UpdateCheckResult {
  current_version: string;
  latest_version: string;
  min_supported_version: string;
  update_available: boolean;
  mandatory: boolean;
  bump_kind: string;
  release_notes_url: string;
  download_url_template: string;
  signature_url_template: string;
  /** "none" | "binary" | "ui" | "both" */
  update_kind?: string;
  ui_current_version?: string;
  ui_latest_version?: string;
  ui_update_available?: boolean;
  ui_download_url_template?: string;
  ui_signature_url_template?: string;
  ui_release_notes_url?: string;
  ui_subdir?: string;
  product_id?: string;
}
