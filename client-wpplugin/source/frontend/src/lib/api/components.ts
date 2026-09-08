import { apiFetch } from './client';
import type {
  ComponentItem,
  ComponentTemplateData,
  LocalComponentsPage,
  LocalComponent,
} from './types';
import type { RuleDiscoveryResponse } from '../components-page/types';

// --- Server Component Capabilities ---
export interface ComponentCapability {
  id: string;
  name: string;
  kind: string;
  supported_business_lines: string[];
  supported_content_formats: string[];
  supported_formats: string[];
  client_contract?: {
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
  };
  constraints: { max_file_size_mb?: number };
}

export interface CapabilitiesResponse {
  components: ComponentCapability[];
  format_component_map: Record<string, string[]>;
}

export interface LocalComponentsCollectionData {
  items: LocalComponent[];
  total: number;
}

export interface ServerComponentsSearchData {
  items: ComponentItem[];
  page: number;
  per_page: number;
  total: number;
  total_pages: number;
}

export const getComponentCapabilities = () =>
  apiFetch<CapabilitiesResponse>('/api/components/capabilities');

// --- Server Component Bindings ---
export const loadComponentTemplate = (component_id: string) =>
  apiFetch<ComponentTemplateData>('/api/components/template', { method: 'POST', body: { component_id } });

export const upsertBinding = (body: Record<string, unknown>) =>
  apiFetch<{ component_id: string }>('/api/components/bindings/upsert', { method: 'POST', body });

export const deleteBinding = (component_id: string) =>
  apiFetch<{ deleted: boolean }>('/api/components/bindings/delete', { method: 'POST', body: { component_id } });

// --- Task Type Bindings ---
export const upsertTaskTypeBinding = (task_type: string, component_id: string, business_line?: string) =>
  apiFetch<{ business_line?: string; task_type: string; component_id: string }>('/api/task-type-components/upsert', { method: 'POST', body: { task_type, component_id, business_line } });

export const deleteTaskTypeBinding = (task_type: string, business_line?: string) =>
  apiFetch<{ deleted: boolean }>('/api/task-type-components/delete', { method: 'POST', body: { task_type, business_line } });

// --- Rule Component Bindings ---
export const upsertRuleBinding = (
  scope: 'global' | 'plugin' | 'relation' | 'rule',
  slot_key: string,
  component_id: string,
  scope_key?: string,
) =>
  apiFetch<{ scope: string; scope_key?: string; slot_key: string; component_id: string }>('/api/rule-component-bindings/upsert', {
    method: 'POST',
    body: { scope, scope_key, slot_key, component_id },
  });

export const deleteRuleBinding = (
  scope: 'global' | 'plugin' | 'relation' | 'rule',
  slot_key: string,
  scope_key?: string,
) =>
  apiFetch<{ deleted: boolean }>('/api/rule-component-bindings/delete', {
    method: 'POST',
    body: { scope, scope_key, slot_key },
  });

export const getRuleBindingDiscovery = () =>
  apiFetch<RuleDiscoveryResponse>('/api/rule-component-bindings/discovery');

// --- Local Components v2 ---
export const listLocalComponents = (page = 1, q = '', enabled?: boolean, kind?: string) => {
  const enabledQuery = enabled === undefined ? '' : `&enabled=${enabled ? 'true' : 'false'}`;
  const kindQuery = kind ? `&kind=${encodeURIComponent(kind)}` : '';
  return apiFetch<LocalComponentsPage & { kinds?: string[] }>(`/api/components/local?page=${page}&per_page=50${q ? '&q=' + encodeURIComponent(q) : ''}${enabledQuery}${kindQuery}`);
};

export const listAllLocalComponents = async (
  q = '',
  enabled?: boolean,
): Promise<import('./types').ApiResult<LocalComponentsCollectionData>> => {
  const items: LocalComponent[] = [];
  let page = 1;
  let total = 0;
  let totalPages = 1;

  while (page <= totalPages) {
    const res = await listLocalComponents(page, q, enabled);
    if (!res.success) {
      return res as import('./types').ApiResult<LocalComponentsCollectionData>;
    }
    items.push(...res.data.items);
    total = res.data.total;
    totalPages = Math.max(res.data.total_pages || 1, 1);
    page += 1;
  }

  return {
    success: true,
    data: {
      items,
      total,
    },
  };
};

export const getLocalComponent = (id: string) =>
  apiFetch<LocalComponent>(`/api/components/local/${encodeURIComponent(id)}`);

export const searchServerComponents = (params: {
  page?: number;
  per_page?: number;
  q?: string;
  kind?: string;
  group?: string;
  product_id?: string;
  family?: string;
  subfamily?: string;
  content_format?: string;
  size_class?: string;
  business_line?: string;
  locale?: string;
}) => {
  const qs = new URLSearchParams();
  qs.set('page', String(params.page ?? 1));
  qs.set('per_page', String(params.per_page ?? 20));
  if (params.q?.trim()) qs.set('q', params.q.trim());
  if (params.kind) qs.set('kind', params.kind);
  if (params.group) qs.set('group', params.group);
  if (params.product_id) qs.set('product_id', params.product_id);
  if (params.family) qs.set('family', params.family);
  if (params.subfamily) qs.set('subfamily', params.subfamily);
  if (params.content_format) qs.set('content_format', params.content_format);
  if (params.size_class) qs.set('size_class', params.size_class);
  if (params.business_line) qs.set('business_line', params.business_line);
  if (params.locale) qs.set('locale', params.locale);
  return apiFetch<ServerComponentsSearchData>(`/api/components/server-search?${qs.toString()}`);
};

export const createLocalComponent = (data: { id: string; name: string; template_id?: string; vendor_id?: string; vendor_name?: string; kind?: string; remarks?: string; enabled?: boolean; api_base?: string; model?: string; system_prompt?: string; temperature?: number; max_tokens?: number; response_path?: string; openai_compatible?: boolean }) =>
  apiFetch<{ id: string }>('/api/components/local', { method: 'POST', body: data });

export const updateLocalComponent = (id: string, data: {
  name?: string;
  remarks?: string;
  vendor_id?: string;
  vendor_name?: string;
  template_id?: string;
  enabled?: boolean;
  api_base?: string;
  model?: string;
  system_prompt?: string;
  temperature?: number;
  max_tokens?: number;
  response_path?: string;
}) =>
  apiFetch<{ id: string }>(`/api/components/local/${encodeURIComponent(id)}`, { method: 'PUT', body: data });

export const deleteLocalComponent = (id: string) =>
  apiFetch<{ deleted: boolean }>(`/api/components/local/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const refreshLocalComponentSnapshot = (id: string) =>
  apiFetch<{ id?: string }>(`/api/components/local/${encodeURIComponent(id)}/refresh-snapshot`, { method: 'POST', body: {} });

export const createComponentVersion = (id: string, data: { version: string; key_ids?: string[]; key_selection_strategy?: string; proxy_profile_id?: string; auth_type?: string; remarks?: string; config_overrides?: Record<string, unknown> }) =>
  apiFetch<{ component_id: string; version: string }>(`/api/components/local/${encodeURIComponent(id)}/versions`, { method: 'POST', body: data });

export const updateComponentVersion = (id: string, ver: string, data: { key_ids?: string[]; key_selection_strategy?: string; proxy_profile_id?: string; auth_type?: string; remarks?: string; config_overrides?: Record<string, unknown> }) =>
  apiFetch<{ component_id: string; version: string }>(`/api/components/local/${encodeURIComponent(id)}/versions/${encodeURIComponent(ver)}`, { method: 'PUT', body: data });

export const deleteComponentVersion = (id: string, ver: string) =>
  apiFetch<{ deleted: boolean }>(`/api/components/local/${encodeURIComponent(id)}/versions/${encodeURIComponent(ver)}`, { method: 'DELETE' });

export const testComponentVersion = (id: string, ver: string, text = 'Hello World', source_lang = 'en_US', target_lang = 'zh_CN') =>
  apiFetch<{ translated_text?: string; translated_ref?: string; elapsed_ms?: number; error?: string }>(`/api/components/local/${encodeURIComponent(id)}/versions/${encodeURIComponent(ver)}/test`, { method: 'POST', body: { text, source_lang, target_lang } });

// --- Quick Test (any family with template_json; auth_values + config_overrides) ---
export const quickTestLocalComponent = (id: string, data: {
  api_key?: string;
  auth_values?: Record<string, string>;
  config_overrides?: Record<string, string>;
  text?: string;
  source_lang?: string;
  target_lang?: string;
}) => apiFetch<{
  translated_text?: string;
  translated_ref?: string;
  elapsed_ms?: number;
  error?: string;
  hint?: string;
}>(
  `/api/components/local/${encodeURIComponent(id)}/quick-test`,
  { method: 'POST', body: data }
);

export const testLocalComponentFile = (data: {
  component_id: string;
  file_url: string;
  source_lang?: string;
  target_lang?: string;
}) =>
  apiFetch<{ translated_ref?: string; translated_text?: string; elapsed_ms?: number; error?: string }>(
    '/api/components/local/test-file',
    { method: 'POST', body: data }
  );

export const exportLocalComponent = (id: string) =>
  apiFetch<{ export_version: number; component: Record<string, unknown> }>(
    `/api/components/local/${encodeURIComponent(id)}/export`
  );

export const importLocalComponent = (data: {
  export_version?: number;
  component: Record<string, unknown>;
  id?: string;
}) =>
  apiFetch<{ id: string; overwrite: boolean }>(
    '/api/components/local/import',
    { method: 'POST', body: data }
  );

export const installServerTemplateToLocal = (data: {
  template_id: string;
  local_id?: string;
  name?: string;
  remarks?: string;
}) =>
  apiFetch<{ id: string; overwrite: boolean; template_id: string; name: string }>(
    '/api/components/local/install-from-server',
    { method: 'POST', body: data }
  );
