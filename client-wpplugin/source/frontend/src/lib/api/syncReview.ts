import { apiFetch } from './client';
import type { ApiResult } from './types';

export interface SyncReviewListItem {
  id: string;
  pair_id: string;
  canonical_uuid: string;
  status: string;
  source_title: string;
  proposed_title: string;
  post_type: string;
  error_message?: string | null;
  created_at: number;
  updated_at: number;
}

export interface SyncReviewDetail {
  id: string;
  pair_id: string;
  canonical_uuid: string;
  status: string;
  source_title: string;
  source_content: string;
  source_excerpt: string;
  proposed_title: string;
  proposed_content: string;
  proposed_excerpt: string;
  post_type: string;
  error_message?: string | null;
}

export async function listSyncReview(params?: {
  pairId?: string;
  countOnly?: boolean;
}): Promise<ApiResult<{ total: number; items: SyncReviewListItem[] }>> {
  const qs = new URLSearchParams();
  if (params?.pairId) qs.set('pair_id', params.pairId);
  if (params?.countOnly) qs.set('count_only', '1');
  const q = qs.toString();
  return apiFetch(`/api/sync-review${q ? `?${q}` : ''}`);
}

export async function getSyncReviewItem(
  id: string
): Promise<ApiResult<{ item: SyncReviewDetail }>> {
  return apiFetch(`/api/sync-review/${encodeURIComponent(id)}`);
}

export async function updateSyncReviewItem(
  id: string,
  body: {
    proposed_title?: string;
    proposed_content?: string;
    proposed_excerpt?: string;
  }
): Promise<ApiResult<{ item: SyncReviewDetail }>> {
  return apiFetch(`/api/sync-review/${encodeURIComponent(id)}`, {
    method: 'PUT',
    body,
  });
}

export async function approveSyncReviewItem(
  id: string
): Promise<ApiResult<{ id: string; status: string }>> {
  return apiFetch(`/api/sync-review/${encodeURIComponent(id)}/approve`, {
    method: 'POST',
  });
}

export async function rejectSyncReviewItem(
  id: string,
  reason?: string
): Promise<ApiResult<{ id: string; status: string }>> {
  return apiFetch(`/api/sync-review/${encodeURIComponent(id)}/reject`, {
    method: 'POST',
    body: { reason },
  });
}
