import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  loadRecentLogs,
  testProxyProfile,
  updateAccessControl,
  updateLogSettings,
  updateProxyProfile,
} from './settings';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/settings', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('encodes proxy profile id in update/test endpoints', async () => {
    await updateProxyProfile('proxy/1', { host: '127.0.0.1' });
    await testProxyProfile('proxy/1');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/proxy-profiles/proxy%2F1', {
      method: 'PUT',
      body: { host: '127.0.0.1' },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/proxy-profiles/proxy%2F1/test', {
      method: 'POST',
      body: {},
    });
  });

  it('sends log and access control updates', async () => {
    await loadRecentLogs(300);
    await updateLogSettings(true, 'debug');
    await updateAccessControl(true, ['127.0.0.1']);

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/logs/recent', {
      method: 'POST',
      body: { limit: 300 },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/log-settings', {
      method: 'POST',
      body: { enabled: true, level: 'debug' },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/access-control', {
      method: 'POST',
      body: { external_access: true, allowed_ips: ['127.0.0.1'] },
    });
  });
});
