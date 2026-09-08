import { apiFetch } from './client';
import type { ApiResult } from './types';

export interface DiscoveryTask {
  id: number;
  domain: string;
  relation_id: number;
  concurrency: number;
  batch_parallel: number;
  per_page: number;
  retry_max: number;
  timeout_secs: number;
  enabled: boolean;
  include_resync?: boolean;
  selected_component_id?: string | null;
  effective_source_lang?: string | null;
  effective_target_lang?: string | null;
  editable_overrides?: Record<string, unknown> | null;
  last_run_at: number;
  created_at: number;
  updated_at: number;
}

export const listDiscoveryTasks = (): Promise<ApiResult<{ items: DiscoveryTask[] }>> =>
  apiFetch('/api/discovery-tasks');

export const updateDiscoveryTask = (
  id: number,
  params: Partial<Omit<DiscoveryTask, 'id' | 'domain' | 'relation_id' | 'last_run_at' | 'created_at' | 'updated_at'>>
): Promise<ApiResult<Record<string, never>>> =>
  apiFetch(`/api/discovery-tasks/${id}`, { method: 'PUT', body: params });
