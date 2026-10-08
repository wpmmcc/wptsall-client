import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  approveSyncReviewItem,
  getSyncReviewItem,
  listSyncReview,
  rejectSyncReviewItem,
  updateSyncReviewItem,
} from './syncReview';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/syncReview', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('lists pending sync review with filters', async () => {
    await listSyncReview({ pairId: 'pair/1', countOnly: true });
    expect(apiFetchMock).toHaveBeenCalledWith(
      '/api/sync-review?pair_id=pair%2F1&count_only=1'
    );
  });

  it('loads detail and edits proposed fields', async () => {
    await getSyncReviewItem('rev/1');
    await updateSyncReviewItem('rev/1', {
      proposed_title: 'T',
      proposed_content: 'C',
      proposed_excerpt: 'E',
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/sync-review/rev%2F1');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/sync-review/rev%2F1', {
      method: 'PUT',
      body: {
        proposed_title: 'T',
        proposed_content: 'C',
        proposed_excerpt: 'E',
      },
    });
  });

  it('approves and rejects review items', async () => {
    await approveSyncReviewItem('rev/1');
    await rejectSyncReviewItem('rev/1', 'nope');
    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/sync-review/rev%2F1/approve', {
      method: 'POST',
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/sync-review/rev%2F1/reject', {
      method: 'POST',
      body: { reason: 'nope' },
    });
  });
});
