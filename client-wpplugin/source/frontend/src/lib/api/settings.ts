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

export const loadRecentLogs = (
  limit = 200,
  minLevel?: string,
  beforeTsMs?: number,
  eventPrefix?: string,
) =>
  apiFetch<{
    limit: number;
    lines: string[];
    min_level?: string | null;
    before_ts_ms?: number | null;
    next_before_ts_ms?: number | null;
    has_more?: boolean;
  }>('/api/logs/recent', {
    method: 'POST',
    body: {
      limit,
      ...(minLevel ? { min_level: minLevel } : {}),
      ...(beforeTsMs != null ? { before_ts_ms: beforeTsMs } : {}),
      ...(eventPrefix && eventPrefix.trim() ? { event_prefix: eventPrefix.trim() } : {}),
    },
  });

/** POST /api/logs/clear — truncate the client log file (ops convenience). */
export const clearLogs = () =>
  apiFetch<{ cleared: boolean }>('/api/logs/clear', { method: 'POST', body: {} });

export const loadLogSettings = () =>
  apiFetch<{ enabled: boolean; level: string }>('/api/log-settings');

export const updateLogSettings = (enabled: boolean, level: string) =>
  apiFetch<{ enabled: boolean; level: string }>('/api/log-settings', {
    method: 'POST',
    body: { enabled, level },
  });

export const getAccessControl = () =>
  apiFetch<{
    external_access: boolean;
    allowed_ips: string[];
    current_bind: string;
    /** Actual listen port reported by the backend (env override → bind parse → 8977). */
    current_port?: number;
    /** WebUI access token (S2): required by /api requests from non-loopback
     *  peers while external mode is armed. Echoed to every caller of this
     *  endpoint (local mode, loopback recovery, or token-bearing remote). */
    access_token?: string | null;
  }>('/api/access-control');

/** GET /health — local-only version probe (never contacts the update server). */
export const getHealthVersion = () =>
  apiFetch<{ status: string; version: string; ui_version?: string; uptime_seconds: number }>('/health');

export const updateAccessControl = (external_access: boolean, allowed_ips: string[]) =>
  apiFetch<{
    external_access: boolean;
    allowed_ips: string[];
    restart_required: boolean;
    /** One-time token display when this update arms external mode. */
    access_token?: string | null;
  }>('/api/access-control', {
    method: 'POST',
    body: { external_access, allowed_ips },
  });
