import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch } from './client';

const invokeMock = vi.mocked(invoke);

describe('desktop history / translations routes', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('maps GET /api/translations query args to list_translations', async () => {
    invokeMock.mockResolvedValueOnce({ records: [], total: 0, page: 1, limit: 20 });
    await apiFetch(
      '/api/translations?page=2&limit=20&domain=blog.example.test&status=failed&search=title'
    );
    expect(invokeMock).toHaveBeenLastCalledWith('list_translations', {
      page: 2,
      limit: 20,
      domain: 'blog.example.test',
      status: 'failed',
      search: 'title',
    });
  });

  it('maps batch retry/delete and single retry', async () => {
    invokeMock.mockResolvedValueOnce({ queued: 2, requested: 2 });
    await apiFetch('/api/translations/batch-retry', {
      method: 'POST',
      body: { ids: [1, 2] },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('batch_retry_translations', {
      request: { ids: [1, 2] },
    });

    invokeMock.mockResolvedValueOnce({ deleted: 2 });
    await apiFetch('/api/translations/batch-delete', {
      method: 'POST',
      body: { ids: [1, 2] },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('batch_delete_translations', {
      request: { ids: [1, 2] },
    });

    invokeMock.mockResolvedValueOnce({ queued: true });
    await apiFetch('/api/translations/9/retry', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('retry_translation', { id: '9' });
  });
});
