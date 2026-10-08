import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { setApiFetchHandler } from '@webui/lib/api/client';
import {
  approveSyncReviewItem, getSyncReviewItem, listSyncReview,
  rejectSyncReviewItem, updateSyncReviewItem,
} from '@webui/lib/api/syncReview';
import { apiFetch } from './client';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe('shared sync review through Desktop adapter', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue({});
    setApiFetchHandler(apiFetch);
  });
  afterEach(() => setApiFetchHandler(null));

  it('preserves pair and count filters on the inbox GET', async () => {
    await listSyncReview({ pairId: 'pair & 1', countOnly: true });
    expect(invoke).toHaveBeenCalledWith('proxy_webui_request', {
      method: 'GET', path: '/api/sync-review?pair_id=pair+%26+1&count_only=1', body: undefined,
    });
  });

  it('encodes a detail id without changing its route', async () => {
    await getSyncReviewItem('review/中文');
    expect(invoke).toHaveBeenCalledWith('proxy_webui_request', {
      method: 'GET', path: '/api/sync-review/review%2F%E4%B8%AD%E6%96%87', body: undefined,
    });
  });

  it('sends all edited fields through PUT before approval POST', async () => {
    const body = { proposed_title: 'Edited title', proposed_content: '<p>Edited</p>', proposed_excerpt: 'Edited excerpt' };
    await updateSyncReviewItem('review-1', body);
    await approveSyncReviewItem('review-1');
    expect(vi.mocked(invoke).mock.calls).toEqual([
      ['proxy_webui_request', { method: 'PUT', path: '/api/sync-review/review-1', body }],
      ['proxy_webui_request', { method: 'POST', path: '/api/sync-review/review-1/approve', body: undefined }],
    ]);
  });

  it('preserves rejection reason and forwards native failure as ApiResult', async () => {
    vi.mocked(invoke).mockRejectedValueOnce('Review storage unavailable');
    const result = await rejectSyncReviewItem('review-1', 'Not this revision');
    expect(invoke).toHaveBeenCalledWith('proxy_webui_request', {
      method: 'POST', path: '/api/sync-review/review-1/reject', body: { reason: 'Not this revision' },
    });
    expect(result).toEqual({
      success: false, error: { code: 'INVOKE_ERROR', message: 'Review storage unavailable' },
    });
  });
});
