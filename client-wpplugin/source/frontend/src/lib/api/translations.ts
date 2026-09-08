import { apiFetch } from './client';
import type { ApiResult } from './types';

export interface TranslationQueryParams {
  page?: number;
  limit?: number;
  domain?: string;
  status?: string;
  search?: string;
}

export interface TranslationRecord {
  id: number;
  created_at: number;
  domain: string;
  relation_id?: number;
  object_id?: number;
  object_type?: string;
  business_line?: string;
  source_lang: string;
  target_lang: string;
  status: string;
  execution_ms?: number;
  worker_id?: string;
  idempotency_key?: string;
  callback_sent_at?: number;
  callback_retries: number;
  fields_count: number;
  error_message?: string;
  component_ids: string[];
  media_mappings_count: number;
  failed_fields_count: number;
  primary_failure_reason?: string;
}

export interface TranslationListResponse {
  records: TranslationRecord[];
  total: number;
  page: number;
  limit: number;
}

export async function getTranslations(params: TranslationQueryParams = {}): Promise<ApiResult<TranslationListResponse>> {
  const query = new URLSearchParams();
  if (params.page) query.set('page', String(params.page));
  if (params.limit) query.set('limit', String(params.limit));
  if (params.domain) query.set('domain', params.domain);
  if (params.status) query.set('status', params.status);
  if (params.search) query.set('search', params.search);
  const qs = query.toString();
  return apiFetch<TranslationListResponse>(`/api/translations${qs ? '?' + qs : ''}`);
}

export async function retryTranslation(id: number): Promise<ApiResult<{ queued: boolean }>> {
  return apiFetch<{ queued: boolean }>(`/api/translations/${id}/retry`, { method: 'POST' });
}

export async function batchRetryTranslations(ids: number[]): Promise<ApiResult<{ queued: number; requested: number }>> {
  return apiFetch<{ queued: number; requested: number }>('/api/translations/batch-retry', {
    method: 'POST',
    body: { ids },
  });
}

export async function batchDeleteTranslations(ids: number[]): Promise<ApiResult<{ deleted: number; requested: number }>> {
  return apiFetch<{ deleted: number; requested: number }>('/api/translations/batch-delete', {
    method: 'POST',
    body: { ids },
  });
}
