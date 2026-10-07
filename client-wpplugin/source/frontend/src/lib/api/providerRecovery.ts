import { apiFetch } from './client'
import type { ApiResult } from './types'

export interface ProviderOperation {
  operation_id: string
  site: string | null
  component_id: string
  relation_id: number
  object_type: string
  object_id: number
  field_name: string
  chunk_index: number
  lane: string
  source_lang: string
  target_lang: string
  state: string
  can_reconcile: boolean
}

export function listProviderOperations(): Promise<ApiResult<{ items: ProviderOperation[] }>> {
  return apiFetch('/api/provider-recovery')
}

export function reconcileProviderOperation(operation_id: string): Promise<ApiResult<{ state: string }>> {
  return apiFetch('/api/provider-recovery/reconcile', { method: 'POST', body: { operation_id } })
}
