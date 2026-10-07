import { apiFetch } from './client'
import type { ApiResult } from './types'
import type { TranslationItem } from './jobs'

export interface ItemContentData {
  item: TranslationItem
  raw: Record<string, unknown> | null
  translated: Record<string, unknown> | null
  manual_request_id?: string | null
  delivery_unresolved?: boolean
}

export async function getItemContent(id: number): Promise<ApiResult<ItemContentData>> {
  return apiFetch<ItemContentData>(`/api/items/${id}/content`)
}

export async function saveTranslated(id: number, content: unknown): Promise<ApiResult<Record<string, unknown>>> {
  return apiFetch<Record<string, unknown>>(`/api/items/${id}/translated`, {
    method: 'PUT',
    body: { content },
  })
}

export interface SaveItemOverridePayload {
  component_id?: string | null
  source_lang?: string | null
  target_lang?: string | null
  editable_overrides?: Record<string, unknown> | null
}

export interface SaveItemOverrideResult {
  item_id: number
  component_id: string | null
  source_lang: string | null
  target_lang: string | null
  editable_overrides: Record<string, unknown> | null
}

export async function saveItemOverride(
  id: number,
  payload: SaveItemOverridePayload
): Promise<ApiResult<SaveItemOverrideResult>> {
  return apiFetch<SaveItemOverrideResult>(`/api/items/${id}/override`, {
    method: 'PUT',
    body: payload,
  })
}

export interface ResubmitResult {
  item_id: number
  status: string
}

export async function resubmitItem(id: number): Promise<ApiResult<ResubmitResult>> {
  return apiFetch<ResubmitResult>(`/api/items/${id}/resubmit`, {
    method: 'POST',
  })
}

export interface RetranslateResult extends ResubmitResult {
  component_id?: string | null
  source_lang?: string | null
  target_lang?: string | null
  translated_path?: string
  request_id: string
  replayed?: boolean
}

export async function retranslateItem(id: number, resumeRequest?: string | null): Promise<ApiResult<RetranslateResult>> {
  const slot = `wptsall:manual-request:${id}`
  let request: string
  let resumeOnly: boolean
  try {
    const saved = localStorage.getItem(slot)
    resumeOnly = resumeRequest != null || saved != null
    request = resumeRequest ?? saved ?? crypto.randomUUID()
    if (request === '00000000-0000-0000-0000-000000000000'
        || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(request)) {
      throw new Error('Saved manual request identity is invalid')
    }
    localStorage.setItem(slot, request)
  } catch {
    return { success: false, error: { code: 'REQUEST_STORAGE_FAILED', message: 'Cannot retain manual request identity. No translation was started.' } }
  }
  const result = await apiFetch<RetranslateResult>(`/api/items/${id}/retranslate`, {
    method: 'POST',
    body: resumeOnly ? { request_id: request, resume_only: true } : { request_id: request },
  })
  if (result.success) {
    if (result.data?.request_id !== request || result.data?.item_id !== id) {
      return { success: false, error: { code: 'REQUEST_RECEIPT_MISMATCH', message: 'Manual request receipt differs. Retry the same retained request.' } }
    }
    try {
      if (localStorage.getItem(slot) === request) localStorage.removeItem(slot)
    } catch { /* Retaining the acknowledged UUID is safe to replay. */ }
  }
  return result
}

export async function approveItem(id: number): Promise<ApiResult<ResubmitResult>> {
  return apiFetch<ResubmitResult>(`/api/items/${id}/approve`, {
    method: 'POST',
  })
}

export interface BatchApproveResult {
  approved: number[]
  failed: Array<{
    id: number
    status?: string
    code?: string
    message?: string
    error?: string
  }>
  skipped: { id: number; reason: string }[]
}

export async function batchApproveItems(ids: number[]): Promise<ApiResult<BatchApproveResult>> {
  return apiFetch<BatchApproveResult>('/api/items/batch-approve', {
    method: 'POST',
    body: { ids },
  })
}

export interface BatchRejectResult {
  rejected: number[]
  failed: Array<{ id: number; message?: string }>
  skipped: { id: number; reason: string }[]
}

export async function batchRejectItems(
  ids: number[],
  reason?: string
): Promise<ApiResult<BatchRejectResult>> {
  return apiFetch<BatchRejectResult>('/api/items/batch-reject', {
    method: 'POST',
    body: { ids, reason },
  })
}

export interface PendingReviewListData {
  total: number
  items: TranslationItem[]
}

export async function listPendingReviewItems(params?: {
  limit?: number
  countOnly?: boolean
}): Promise<ApiResult<PendingReviewListData>> {
  const qs = new URLSearchParams()
  if (params?.limit !== undefined) qs.set('limit', String(params.limit))
  if (params?.countOnly) qs.set('count_only', '1')
  const q = qs.toString()
  return apiFetch<PendingReviewListData>(`/api/items/pending-review${q ? `?${q}` : ''}`)
}

export interface RejectResult {
  item_id: number
  status: string
  reason: string
}

export async function rejectItem(id: number, reason?: string): Promise<ApiResult<RejectResult>> {
  return apiFetch<RejectResult>(`/api/items/${id}/reject`, {
    method: 'POST',
    body: { reason },
  })
}
