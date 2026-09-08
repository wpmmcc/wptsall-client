import { apiFetch } from './client'
import type { ApiResult } from './types'
import type { TranslationItem } from './jobs'

export interface ItemContentData {
  item: TranslationItem
  raw: Record<string, unknown> | null
  translated: Record<string, unknown> | null
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
}

export async function retranslateItem(id: number): Promise<ApiResult<RetranslateResult>> {
  return apiFetch<RetranslateResult>(`/api/items/${id}/retranslate`, {
    method: 'POST',
  })
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
