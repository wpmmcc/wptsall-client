import { apiFetch } from './client';

export interface ImportedSiteConnection {
  api_base_url: string;
  token_prefix: string;
  token_len: number;
  route_secret_set?: boolean;
  pairing_claimed?: boolean;
}

export const importSiteConnection = (body: {
  site_connection_pack: unknown;
  pairing_code?: string;
  device_label?: string;
}) =>
  apiFetch<ImportedSiteConnection>('/api/site-connections/import', { method: 'POST', body });

export const upsertDomainToken = (body: {
  api_base_url: string;
  existing_api_base_url?: string;
  wp_client_token?: string;
  route_secret?: string;
}) =>
  apiFetch<{ api_base_url: string; token_prefix: string; token_len: number; route_secret_set?: boolean }>(
    '/api/domain-tokens/upsert', { method: 'POST', body }
  );

export const deleteDomainToken = (api_base_url: string) =>
  apiFetch<{ deleted: boolean }>('/api/domain-tokens/delete', { method: 'POST', body: { api_base_url } });

export const testDomainToken = (api_base_url: string) =>
  apiFetch<unknown>('/api/domain-tokens/test', { method: 'POST', body: { api_base_url } });
