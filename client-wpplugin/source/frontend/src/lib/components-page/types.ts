export interface ComponentVersion {
  version: string;
  remarks: string;
  key_ids: string[];
  key_selection_strategy: string;
  proxy_profile_id?: string;
  auth_type: string;
  created_at: string;
}

export interface LocalComp {
  id: string;
  name: string;
  template_id: string;
  vendor_id: string;
  source_template_id?: string;
  source_template_updated_at?: string | null;
  source_template_api_version?: string | null;
  vendor_name: string;
  kind: string;
  capability?: string;
  remarks: string;
  created_at: string;
  updated_at?: string | null;
  enabled: boolean;
  versions: Record<string, ComponentVersion>;
}

export interface LocalCompsPage {
  items: LocalComp[];
  total: number;
}

export interface TemplateTranslationMode {
  id: string;
  label: string;
  supported_content_formats: string[];
  api_docs_url?: string;
}

export interface ComponentClientContractSummary {
  schema_version: string;
  task_kind: string;
  input_mode: string;
  workflow_mode: string;
  output_mode: string;
  stages?: string[];
  request_body_type?: string;
  source_upload_body_type?: string;
  submit_response_type?: string;
  result_transport?: string;
}

export interface ServerComp {
  id: string;
  name: string;
  description?: string;
  product_id?: string;
  owner_type: string;
  version: string;
  kind: string;
  status?: string;
  supported_business_lines: string[];
  supported_content_formats: string[];
  supported_formats: string[];
  max_file_size_mb: number | null;
  size_class?: string;
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
  translation_modes?: TemplateTranslationMode[];
  client_contract?: ComponentClientContractSummary;
}

// VendorKeyItem / OAuthItem describe the same wire objects the api-keys
// page manages (vendor-keys / oauth API items). They were re-declared here
// in a reduced shape that drifted from the runtime data — MyComponentsTab
// feeds full API items into them (double cast) and the modal test fixtures
// model the real wire shape, so svelte-check flagged 3 fixture type errors
// (PUBLIC-REPO-DEEP-E2E-20260911 台账 §6-2, 2026-09-12 修复). Reuse the
// canonical api-keys-page definitions instead of maintaining duplicates.
export type { VendorKeyItem, OAuthItem } from '../api-keys-page/types';

export type AuthStrategy = 'RoundRobin' | 'Random' | 'Weighted';
export type FileSizeClass = 'S' | 'M' | 'L' | 'XL' | 'UNLIMITED';

export interface RuleBinding {
  scope: string;
  scope_key: string;
  slot_key: string;
  component_id: string;
}

export interface RuleSlotOption {
  key: string;
  label: string;
  hint: string;
}

export interface RuleDiscoveryFieldSummary {
  field_name: string;
  content_format: string;
  source_role: string;
  storage: string;
  required_slot_key: string;
  suggested_task_type: string;
}

export interface RuleDiscoveryItem {
  api_base_url: string;
  relation_id: number;
  source_lang: string;
  target_lang: string;
  plugin_slug: string;
  plugin_name: string;
  rule_id: number;
  rule_name: string;
  data_type: string;
  object_name: string;
  business_line: string;
  source_group: string;
  routing_profile: string;
  delivery_target: string;
  required_component_slots: string[];
  required_content_formats: string[];
  fields: RuleDiscoveryFieldSummary[];
}

export interface RuleDiscoveryIssue {
  api_base_url: string;
  relation_id?: number | null;
  stage: string;
  error: string;
}

export interface RuleDiscoveryResponse {
  summary: {
    domains_checked: number;
    relations_checked: number;
    rules_checked: number;
    fields_checked: number;
    issues: number;
  };
  items: RuleDiscoveryItem[];
  issues: RuleDiscoveryIssue[];
}

export interface ComponentFormState {
  id: string;
  name: string;
  template_id: string;
  vendor_id: string;
  vendor_name: string;
  kind: string;
  remarks: string;
  enabled: boolean;
  api_base: string;
  model: string;
  system_prompt: string;
  temperature: string;
  max_tokens: string;
  response_path: string;
}

export interface VersionFormState {
  version: string;
  key_ids: string;
  key_selection_strategy: string;
  auth_type: string;
  proxy_profile_id: string;
  remarks: string;
}

export interface QuickTestState {
  compId: string;
  apiKey: string;
  text: string;
  targetLang: string;
  loading: boolean;
  result: {
    translated_text?: string;
    translated_ref?: string;
    elapsed_ms?: number;
    error?: string;
  } | null;
  error: string | null;
}

export interface FileTestModalState {
  open: boolean;
  componentId: string;
  componentName: string;
  fileUrl: string;
  sourceLang: string;
  targetLang: string;
  loading: boolean;
  result: {
    translated_ref?: string;
    translated_text?: string;
    elapsed_ms?: number;
    error?: string;
  } | null;
  error: string | null;
}

export interface ServerTemplateModalData {
  name: string;
  version: string;
  product_id?: string;
  catalog_family?: string;
  catalog_subfamily?: string;
  capability_tags?: string[];
  runtime_tags?: string[];
  auth_modes?: string[];
  auth_fields: { name: string; required: boolean }[];
  template_json: unknown;
  signing_algorithm?: string;
  supported_content_formats?: string[];
  supported_formats?: string[];
  max_file_size_mb?: number;
  api_docs_url?: string;
  translation_modes?: TemplateTranslationMode[];
  client_contract?: ComponentClientContractSummary;
  input_artifact_kind?: string;
  output_artifact_kinds?: string[];
}

export interface OpenAiPreset {
  label: string;
  api_base: string;
  model: string;
}

export interface OpenAiCompatibleConfig {
  api_base: string;
  model: string;
  system_prompt: string;
  temperature: string;
  max_tokens: string;
  response_path: string;
}
