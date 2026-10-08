import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  deleteSyncPair,
  listSyncPairs,
  pauseSyncPair,
  resumeSyncPair,
  runSyncPair,
  upsertSyncPair,
  type SyncPairUpsertPayload,
} from './syncPairs';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/syncPairs', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('lists sync pairs', async () => {
    await listSyncPairs();
    expect(apiFetchMock).toHaveBeenCalledWith('/api/sync-pairs');
  });

  it('upserts a sync pair', async () => {
    const payload: SyncPairUpsertPayload = {
      name: 'Site A to B',
      source_domain: 'https://site-a.com',
      target_domain: 'https://site-b.com',
      direction: 'unidirectional',
      sync_mode: 'sync_and_translate',
      source_lang: 'en_US',
      target_lang: 'zh_CN',
      conflict_strategy: 'lww',
      sync_frequency: 'hourly',
      post_types: ['post', 'page'],
    };
    await upsertSyncPair(payload);
    expect(apiFetchMock).toHaveBeenCalledWith('/api/sync-pairs', {
      method: 'POST',
      body: payload,
    });
  });

  it('deletes a sync pair', async () => {
    await deleteSyncPair('pair-123');
    expect(apiFetchMock).toHaveBeenCalledWith('/api/sync-pairs/pair-123', {
      method: 'DELETE',
    });
  });

  it('pauses a sync pair', async () => {
    await pauseSyncPair('pair-123');
    expect(apiFetchMock).toHaveBeenCalledWith('/api/sync-pairs/pair-123/pause', {
      method: 'POST',
    });
  });

  it('resumes a sync pair', async () => {
    await resumeSyncPair('pair-123');
    expect(apiFetchMock).toHaveBeenCalledWith('/api/sync-pairs/pair-123/resume', {
      method: 'POST',
    });
  });

  it('runs a sync pair', async () => {
    await runSyncPair('pair-123');
    expect(apiFetchMock).toHaveBeenCalledWith('/api/sync-pairs/pair-123/run', {
      method: 'POST',
    });
  });
});
