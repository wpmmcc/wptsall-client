import { apiFetch } from './client';
import type {
  CloudApiTypeItem,
  VendorItem,
  VendorKeyItem,
  OAuthConfigItem,
  WpTranslationProviderItem,
} from './types';

export interface ProviderCatalogItem {
  entry_id: string;
  template_id: string;
  name: string;
  vendor_id: string;
  family: string;
  kind: string;
  supported_content_formats: string[];
  source: string;
  verified: boolean;
  evidence_tier?: 'mock-verified' | 'live-verified' | 'schema-only' | string;
  editable_params?: Array<{ path?: string; type?: string; required?: boolean }>;
  requires_local_credentials: boolean;
  template: Record<string, unknown>;
}

export interface ProviderCatalogData {
  schema: string;
  catalog_version: string | null;
  items: ProviderCatalogItem[];
  offline: boolean;
}

export const listProviderCatalog = (params: { q?: string; vendor_id?: string } = {}) => {
  const query = new URLSearchParams();
  if (params.q?.trim()) query.set('q', params.q.trim());
  if (params.vendor_id?.trim()) query.set('vendor_id', params.vendor_id.trim());
  const suffix = query.toString() ? `?${query.toString()}` : '';
  return apiFetch<ProviderCatalogData>(`/api/provider-catalog${suffix}`);
};

export const refreshProviderCatalog = () =>
  apiFetch<{ catalog_version: string | null; template_count: number; source: string; verified: boolean; offline: boolean }>(
    '/api/provider-catalog/refresh',
    { method: 'POST', body: {} },
  );

export const installCatalogTemplate = (data: {
  entry_id: string;
  local_id?: string;
  name?: string;
  vendor_name?: string;
  remarks?: string;
  overwrite?: boolean;
}) =>
  apiFetch<{ id: string; template_id: string; catalog_entry_id: string; enabled: boolean; overwrite: boolean }>(
    '/api/components/local/install-from-catalog',
    { method: 'POST', body: data },
  );

export const exportIntegrationPack = (data: {
  mode?: 'public' | 'private';
  confirm?: boolean;
  passphrase?: string;
  pack_id?: string;
  name?: string;
}) => apiFetch<Record<string, unknown>>('/api/integrations/pack/export', { method: 'POST', body: data });

export const previewIntegrationPack = (pack: Record<string, unknown>) =>
  apiFetch<Record<string, unknown>>('/api/integrations/pack/preview', { method: 'POST', body: pack });

export const importIntegrationPack = (pack: Record<string, unknown>, overwrite = false) =>
  apiFetch<Record<string, unknown>>('/api/integrations/pack/import', {
    method: 'POST',
    body: { ...pack, overwrite },
  });

export const listVendors = () =>
  apiFetch<{ items: VendorItem[] }>('/api/vendors');

export const listWpTranslationProviders = () =>
  apiFetch<{ items: WpTranslationProviderItem[] }>('/api/wp-translation-providers');

export const listCloudApiTypes = () =>
  apiFetch<{ items: CloudApiTypeItem[] }>('/api/cloud-api-types');

export const listVendorKeys = (vendor_id?: string) =>
  apiFetch<{ items: VendorKeyItem[] }>(`/api/vendor-keys${vendor_id ? '?vendor_id=' + encodeURIComponent(vendor_id) : ''}`);

export const createVendorKey = (data: {
  id: string;
  vendor_id: string;
  label?: string;
  auth_values: Record<string, string>;
  max_concurrent?: number;
  requests_per_second?: number;
  weight?: number;
  enabled?: boolean;
  max_input_chars?: number;
  max_file_size_mb?: number;
}) =>
  apiFetch<{ id: string }>('/api/vendor-keys', { method: 'POST', body: data });

export const updateVendorKey = (id: string, data: { label?: string; auth_values?: Record<string, string>; max_concurrent?: number; requests_per_second?: number; weight?: number; enabled?: boolean; max_input_chars?: number; max_file_size_mb?: number }) =>
  apiFetch<{ id: string }>(`/api/vendor-keys/${encodeURIComponent(id)}`, { method: 'PUT', body: data });

export const deleteVendorKey = (id: string) =>
  apiFetch<{ deleted: boolean }>(`/api/vendor-keys/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const listOAuthConfigs = (vendor_id?: string) =>
  apiFetch<{ items: OAuthConfigItem[] }>(`/api/vendor-oauth${vendor_id ? '?vendor_id=' + encodeURIComponent(vendor_id) : ''}`);

export const createOAuthConfig = (data: {
  id: string;
  vendor_id: string;
  label?: string;
  grant_type: string;
  auth_url?: string;
  token_url?: string;
  client_id: string;
  client_secret?: string;
  scopes?: string;
  auth_extra_params?: Record<string, string>;
  max_concurrent?: number;
  weight?: number;
  max_input_chars?: number;
  max_file_size_mb?: number;
  token_field?: string;
}) =>
  apiFetch<{ id: string }>('/api/vendor-oauth', { method: 'POST', body: data });

export const updateOAuthConfig = (id: string, data: Record<string, unknown>) =>
  apiFetch<{ id: string }>(`/api/vendor-oauth/${encodeURIComponent(id)}`, { method: 'PUT', body: data });

export const deleteOAuthConfig = (id: string) =>
  apiFetch<{ deleted: boolean }>(`/api/vendor-oauth/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const authorizeOAuth = (id: string) =>
  apiFetch<{ authorize_url?: string; callback_url?: string; token_preview?: string; expires_in?: number }>(`/api/vendor-oauth/${encodeURIComponent(id)}/authorize`, { method: 'POST', body: {} });
