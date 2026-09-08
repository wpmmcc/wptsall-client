import { apiFetch } from './client'
import type { ApiResult } from './types'

export interface JobProgress {
  total: number
  done: number
  failed: number
  pending_review: number
  translating: number
}

export interface TranslationJob {
  id: number
  domain: string
  relation_id: number
  business_line: string
  status: string        // pending|running|completed|failed|partial
  total_items: number
  done_items: number
  failed_items: number
  triggered_by: string
  started_at: number | null
  completed_at: number | null
  created_at: number
  updated_at: number
  progress?: JobProgress
}

export interface TranslationItem {
  id: number
  job_id: number
  domain: string
  relation_id: number
  business_line: string
  object_type: string
  wp_object_id: number
  wp_object_subtype: string
  task_type: string
  source_lang: string
  target_lang: string
  component_id: string
  component_ids?: string[]
  selected_component_id?: string | null
  effective_source_lang?: string | null
  effective_target_lang?: string | null
  editable_overrides?: Record<string, unknown> | null
  raw_path: string
  translated_path: string
  status: string        // pending|fetching|fetched|translating|translated|syncing|done|failed|skipped
  client_task_id: string
  upload_id: string | null
  wp_attachment_id: number | null
  error_message: string | null
  retry_count: number
  max_retries: number
  fetched_at: number | null
  translated_at: number | null
  synced_at: number | null
  created_at: number
  updated_at: number
}

export interface JobsListData {
  items: TranslationJob[]
}

export interface JobItemsData {
  items: TranslationItem[]
}

export const listJobs = (params?: {
  domain?: string
  limit?: number
  offset?: number
}): Promise<ApiResult<JobsListData>> => {
  const qs = new URLSearchParams()
  if (params?.domain) qs.set('domain', params.domain)
  if (params?.limit !== undefined) qs.set('limit', String(params.limit))
  if (params?.offset !== undefined) qs.set('offset', String(params.offset))
  const q = qs.toString()
  return apiFetch('/api/jobs' + (q ? '?' + q : ''))
}

export const listAllJobs = async (params?: {
  domain?: string
  pageSize?: number
}): Promise<ApiResult<JobsListData>> => {
  const pageSize = params?.pageSize && params.pageSize > 0 ? params.pageSize : 200
  const items: TranslationJob[] = []
  let offset = 0

  while (true) {
    const res = await listJobs({
      domain: params?.domain,
      limit: pageSize,
      offset,
    })
    if (!res.success) return res
    items.push(...res.data.items)
    if (res.data.items.length < pageSize) break
    offset += res.data.items.length
  }

  return {
    success: true,
    data: { items },
  }
}

export const getJob = (id: number): Promise<ApiResult<TranslationJob>> =>
  apiFetch(`/api/jobs/${id}`)

export const listJobItems = (
  jobId: number,
  status?: string
): Promise<ApiResult<JobItemsData>> => {
  const qs = new URLSearchParams()
  if (status) qs.set('status', status)
  const q = qs.toString()
  return apiFetch(`/api/jobs/${jobId}/items` + (q ? '?' + q : ''))
}
