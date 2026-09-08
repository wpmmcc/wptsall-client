import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  batchDeleteTranslations,
  batchRetryTranslations,
  getTranslations,
  retryTranslation,
} from './translations';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/translations', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('builds translation query params', async () => {
    await getTranslations({ page: 2, limit: 50, domain: 'https://blog.wpmm.cc', status: 'failed', search: 'seo alt' });

    expect(apiFetchMock).toHaveBeenCalledWith(
      '/api/translations?page=2&limit=50&domain=https%3A%2F%2Fblog.wpmm.cc&status=failed&search=seo+alt',
    );
  });

  it('uses retry and batch endpoints', async () => {
    await retryTranslation(10);
    await batchRetryTranslations([1, 2]);
    await batchDeleteTranslations([3, 4]);

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/translations/10/retry', { method: 'POST' });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/translations/batch-retry', {
      method: 'POST',
      body: { ids: [1, 2] },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/translations/batch-delete', {
      method: 'POST',
      body: { ids: [3, 4] },
    });
  });
});
