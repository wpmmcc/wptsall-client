import { apiFetch } from './client';

export type SyncDirection = 'unidirectional' | 'bidirectional';
export type SyncMode = 'sync_only' | 'sync_and_translate';
export type ConflictStrategy = 'lww' | 'source_wins' | 'target_wins' | 'manual_review' | 'merge';
export type SyncFrequency = 'manual' | 'every_minute' | 'hourly' | 'daily';
export type SyncPairStatus = 'active' | 'paused' | 'error';

export interface SyncPair {
  id: string;
  name: string;
  source_domain: string;
  target_domain: string;
  direction: SyncDirection;
  sync_mode: SyncMode;
  source_lang: string;
  target_lang: string;
  conflict_strategy: ConflictStrategy;
  sync_frequency: SyncFrequency;
  post_types: string[];
  status: SyncPairStatus;
  last_sync_at?: number;
  last_seen_source_id?: number;
  last_sync_count?: number;
  last_error?: string;
  translate_component_id?: string;
  review_before_push?: boolean;
  created_at: number;
  updated_at: number;
}

export interface SyncPairCredential {
  domain: string;
  peer_uuid: string;
  peer_name: string;
  key_scheme: string;
  negotiated_direction: string;
  paired_as: string;
  paired_at: number;
}

export interface SyncPairsResponse {
  schema_version: string;
  pairs: SyncPair[];
  credentials: SyncPairCredential[];
  client_origin_uuid: string;
  updated_at: number;
}

export interface SyncPairUpsertPayload {
  id?: string;
  name?: string;
  source_domain: string;
  target_domain: string;
  direction?: SyncDirection;
  sync_mode?: SyncMode;
  source_lang?: string;
  target_lang?: string;
  conflict_strategy?: ConflictStrategy;
  sync_frequency?: SyncFrequency;
  post_types?: string[];
  status?: SyncPairStatus;
  translate_component_id?: string;
  review_before_push?: boolean;
}

export interface SyncPairPairPayload {
  domain: string;
  pairing_code: string;
  role: 'source' | 'target';
  source_lang?: string;
  target_lang?: string;
  sync_mode?: string;
  /** Canonical conflict strategy declared on the handshake (X-1). */
  conflict_strategy?: ConflictStrategy;
}

export interface SyncPairRunResult {
  id: string;
  status: SyncPairStatus;
  last_sync_at?: number;
  sync_count?: number;
  last_error?: string;
}

export const listSyncPairs = () =>
  apiFetch<SyncPairsResponse>('/api/sync-pairs');

export const upsertSyncPair = (payload: SyncPairUpsertPayload) =>
  apiFetch<{ pair: SyncPair }>('/api/sync-pairs', {
    method: 'POST',
    body: payload,
  });

export const deleteSyncPair = (id: string) =>
  apiFetch<{ deleted: boolean; id: string }>(`/api/sync-pairs/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });

export const pauseSyncPair = (id: string) =>
  apiFetch<{ id: string; status: SyncPairStatus }>(`/api/sync-pairs/${encodeURIComponent(id)}/pause`, {
    method: 'POST',
  });

export const resumeSyncPair = (id: string) =>
  apiFetch<{ id: string; status: SyncPairStatus }>(`/api/sync-pairs/${encodeURIComponent(id)}/resume`, {
    method: 'POST',
  });

/**
 * Real engine trigger: the backend validates preconditions (pairing,
 * translate component) and spawns an async run. Poll `listSyncPairs` for
 * the outcome (last_sync_at / last_sync_count / last_error).
 */
export const runSyncPair = (id: string) =>
  apiFetch<{ pair_id: string; triggered: boolean; async: boolean; timestamp: number }>(
    `/api/sync-pairs/${encodeURIComponent(id)}/run`,
    { method: 'POST' },
  );

/** Pair a bound site as a WPMMCC peer using a one-time pairing code. */
export const pairSyncSite = (payload: SyncPairPairPayload) =>
  apiFetch<SyncPairCredential>('/api/sync-pairs/pair', {
    method: 'POST',
    body: payload,
  });

/** Forget a domain's stored peer credential (local only). */
export const unpairSyncSite = (domain: string) =>
  apiFetch<{ domain: string; deleted: boolean }>(
    `/api/sync-pairs/credentials/${encodeURIComponent(domain)}`,
    { method: 'DELETE' },
  );
