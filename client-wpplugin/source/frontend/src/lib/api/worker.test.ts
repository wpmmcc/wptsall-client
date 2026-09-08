import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  logout,
  refreshComponents,
  refreshDomains,
  runWorkerOnce,
  saveWorkerConfig,
  startOAuth,
  startWorkerLoopCheck,
  startWorkerLoop,
  stopWorkerLoop,
} from './worker';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/worker', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('uses refresh and worker control endpoints', async () => {
    await refreshDomains();
    await refreshComponents();
    await runWorkerOnce();
    await startWorkerLoopCheck();
    await startWorkerLoop();
    await stopWorkerLoop();
    await saveWorkerConfig({ poll_seconds: 15 });
    await logout();
    await startOAuth();

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/domains/refresh', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/components/refresh', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/worker/run-once', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(4, '/api/worker/start-check', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(5, '/api/worker/start', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(6, '/api/worker/stop', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(7, '/api/worker/config', {
      method: 'POST',
      body: { poll_seconds: 15 },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(8, '/api/logout', { method: 'POST', body: {} });
    expect(apiFetchMock).toHaveBeenNthCalledWith(9, '/api/oauth/start', { method: 'POST', body: {} });
  });
});
