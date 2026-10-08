import { beforeEach, describe, expect, it, vi } from 'vitest';
import { getJob, listAllJobs, listJobItems, listJobs } from './jobs';
import { apiFetch } from './client';

// catalog: WEBUI-API-PREFIX-api-jobs
// oracle: L1
// (client-side read contract: exact /api/jobs/{id} and /api/jobs/{id}/items request shapes asserted; F-T1 annotation batch 2026-09-22)

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/jobs', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('lists jobs without params', async () => {
    await listJobs();
    expect(apiFetchMock).toHaveBeenCalledWith('/api/jobs');
  });

  it('builds job list query from params', async () => {
    await listJobs({ domain: 'https://blog.wpmm.cc', limit: 20, offset: 40 });
    expect(apiFetchMock).toHaveBeenCalledWith('/api/jobs?domain=https%3A%2F%2Fblog.wpmm.cc&limit=20&offset=40');
  });

  it('gets single job and filters job items by status', async () => {
    await getJob(99);
    await listJobItems(99, 'failed');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/jobs/99');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/jobs/99/items?status=failed');
  });

  it('walks all job pages with offset pagination', async () => {
    apiFetchMock
      .mockResolvedValueOnce({
        success: true,
        data: {
          items: [{ id: 1 }, { id: 2 }],
        },
      } as never)
      .mockResolvedValueOnce({
        success: true,
        data: {
          items: [{ id: 3 }],
        },
      } as never);

    const res = await listAllJobs({ pageSize: 2 });

    expect(res).toEqual({
      success: true,
      data: {
        items: [{ id: 1 }, { id: 2 }, { id: 3 }],
      },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/jobs?limit=2&offset=0');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/jobs?limit=2&offset=2');
  });
});
