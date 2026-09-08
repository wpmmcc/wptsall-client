import { apiFetch } from './client';
import type { WorkerRunSummary, WebUiStatus } from './types';

export interface WorkerConfigData {
  poll_seconds?: number;
  auto_start_worker?: boolean;
  domain_concurrency?: number;
  relation_concurrency?: number;
  global_translation_concurrency?: number;
  global_callback_concurrency?: number;
  relation_max_pending_callbacks?: number;
  adaptive_rate_control?: boolean;
  adaptive_max_delay_ms?: number;
  callback_concurrency?: number;
  callback_timeout_secs?: number;
  callback_retry_max?: number;
  fetch_timeout_secs?: number;
  fetch_retry_max?: number;
  review_mode?: boolean;
  workflow_policy?: Record<string, unknown> | null;
  workflow_dsl?: Record<string, unknown> | null;
}

export interface OverviewDomainStat {
  domain: string;
  total: number;
  success: number;
  failed: number;
  fields_total: number;
}

export interface OverviewStatusDist {
  status: string;
  count: number;
}

export interface OverviewDailyStat {
  date: string;
  count: number;
  fields: number;
}

export interface OverviewStatsData {
  by_domain: OverviewDomainStat[];
  by_status: OverviewStatusDist[];
  daily: OverviewDailyStat[];
}

export interface WorkerStartMissingComponent {
  api_base_url: string;
  business_line: string;
  relation_id: number;
  rule_id?: number | null;
  source_group: string;
  routing_profile: string;
  delivery_target: string;
  object_name: string;
  field_name: string;
  source_role: string;
  preflight_policy: string;
  missing_component_behavior: string;
  severity: string;
  content_format: string;
  required_slot_key: string;
  suggested_task_type: string;
  input_artifact_kind?: string | null;
  expected_output_artifact_kind?: string | null;
}

export interface WorkerStartPreflightData {
  can_start: boolean;
  requires_confirmation: boolean;
  summary: {
    domains_checked: number;
    relations_checked: number;
    rules_checked: number;
    fields_checked: number;
    language_pack_lanes_checked: number;
    blocking_missing_components: number;
    confirm_missing_components: number;
    auto_skip_missing_components: number;
  };
  missing_components: WorkerStartMissingComponent[];
}

export const getStatus = () => apiFetch<WebUiStatus>('/api/status');

export const refreshDomains = () =>
  apiFetch<{ domains: unknown[] }>('/api/domains/refresh', { method: 'POST', body: {} });

export const refreshComponents = () =>
  apiFetch<{ components: unknown[]; local_components_backfilled?: number; local_components_backfill_error?: string | null }>('/api/components/refresh', { method: 'POST', body: {} });

export const runWorkerOnce = () =>
  apiFetch<WorkerRunSummary>('/api/worker/run-once', { method: 'POST', body: {} });

export const startWorkerLoopCheck = () =>
  apiFetch<WorkerStartPreflightData>('/api/worker/start-check', { method: 'POST', body: {} });

export const startWorkerLoop = (body: { force?: boolean } = {}) =>
  apiFetch<{ running: boolean; poll_seconds: number }>('/api/worker/start', { method: 'POST', body });

export const stopWorkerLoop = () =>
  apiFetch<{ running: boolean }>('/api/worker/stop', { method: 'POST', body: {} });

export const getWorkerConfig = () =>
  apiFetch<WorkerConfigData>('/api/worker/config');

export const saveWorkerConfig = (body: WorkerConfigData) =>
  apiFetch<Record<string, unknown>>('/api/worker/config', { method: 'POST', body });

export const getOverviewStats = () =>
  apiFetch<OverviewStatsData>('/api/stats/overview');

export const logout = () =>
  apiFetch<{ logged_in: boolean }>('/api/logout', { method: 'POST', body: {} });

export const startOAuth = () =>
  apiFetch<{ authorize_url: string }>('/api/oauth/start', { method: 'POST', body: {} });
