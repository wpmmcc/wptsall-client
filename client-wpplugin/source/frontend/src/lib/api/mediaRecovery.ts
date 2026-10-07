import { apiFetch } from './client'
import type { ApiResult } from './types'

export interface MediaOperation {
  operation_id: string
  site: string
  source_id: number
  relation_id: number
  task_id: number
  state: string
  attachment_id: number | null
}

export function listMediaOperations(): Promise<ApiResult<{ items: MediaOperation[] }>> {
  return apiFetch('/api/media-recovery')
}

export function reconcileMediaOperation(
  operation_id: string, attachment_id?: number
): Promise<ApiResult<{ attachment_id: number }>> {
  return apiFetch('/api/media-recovery/reconcile', {
    method: 'POST', body: { operation_id, attachment_id },
  })
}
