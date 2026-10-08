import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch } from './client';

const invokeMock = vi.mocked(invoke);

describe('desktop proxy profile routes', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('maps create list test and delete', async () => {
    invokeMock.mockResolvedValueOnce({ id: 'p1' });
    await apiFetch('/api/proxy-profiles', {
      method: 'POST',
      body: { id: 'p1', host: '127.0.0.1', port: 7890 },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('create_proxy_profile', {
      request: { id: 'p1', host: '127.0.0.1', port: 7890 },
    });

    invokeMock.mockResolvedValueOnce({ items: [] });
    await apiFetch('/api/proxy-profiles');
    expect(invokeMock).toHaveBeenLastCalledWith('list_proxy_profiles', {});

    invokeMock.mockResolvedValueOnce({ reachable: true });
    await apiFetch('/api/proxy-profiles/p1/test', { method: 'POST', body: {} });
    expect(invokeMock).toHaveBeenLastCalledWith('test_proxy_profile', { proxy_id: 'p1' });

    invokeMock.mockResolvedValueOnce({ deleted: true });
    await apiFetch('/api/proxy-profiles/p1', { method: 'DELETE' });
    expect(invokeMock).toHaveBeenLastCalledWith('delete_proxy_profile', { proxy_id: 'p1' });
  });
});
