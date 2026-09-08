import { apiFetch } from './client';
import type { ProxyProfileItem } from './types';

export const listProxyProfiles = () =>
  apiFetch<{ items: ProxyProfileItem[] }>('/api/proxy-profiles');

export const createProxyProfile = (data: { id: string; name: string; protocol?: string; host: string; port?: number; username?: string; password?: string; enabled?: boolean }) =>
  apiFetch<{ id: string }>('/api/proxy-profiles', { method: 'POST', body: data });

export const updateProxyProfile = (id: string, data: { name?: string; protocol?: string; host?: string; port?: number; username?: string; password?: string; enabled?: boolean }) =>
  apiFetch<{ id: string }>(`/api/proxy-profiles/${encodeURIComponent(id)}`, { method: 'PUT', body: data });

export const deleteProxyProfile = (id: string) =>
  apiFetch<{ deleted: boolean }>(`/api/proxy-profiles/${encodeURIComponent(id)}`, { method: 'DELETE' });

export const testProxyProfile = (id: string) =>
  apiFetch<{ reachable: boolean; ip?: string; proxy_url: string }>(`/api/proxy-profiles/${encodeURIComponent(id)}/test`, { method: 'POST', body: {} });

export const loadRecentLogs = (limit = 200) =>
  apiFetch<{ limit: number; lines: string[] }>('/api/logs/recent', { method: 'POST', body: { limit } });

export const loadLogSettings = () =>
  apiFetch<{ enabled: boolean; level: string }>('/api/log-settings');

export const updateLogSettings = (enabled: boolean, level: string) =>
  apiFetch<{ enabled: boolean; level: string }>('/api/log-settings', {
    method: 'POST',
    body: { enabled, level },
  });

export const getAccessControl = () =>
  apiFetch<{ external_access: boolean; allowed_ips: string[]; current_bind: string }>('/api/access-control');

export const updateAccessControl = (external_access: boolean, allowed_ips: string[]) =>
  apiFetch<{ external_access: boolean; allowed_ips: string[]; restart_required: boolean }>('/api/access-control', {
    method: 'POST',
    body: { external_access, allowed_ips },
  });
