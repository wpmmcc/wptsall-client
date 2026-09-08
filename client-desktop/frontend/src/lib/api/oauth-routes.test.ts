import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch } from './client';

const invokeMock = vi.mocked(invoke);

describe('desktop oauth vendor routes', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('maps oauth authorize and delete', async () => {
    invokeMock.mockResolvedValueOnce({ authorize_url: 'https://example.test/oauth' });
    await apiFetch('/api/vendor-oauth/o1/authorize', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('authorize_vendor_oauth', { oauth_id: 'o1' });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/vendor-oauth/o1', { method: 'DELETE' });
    expect(invokeMock).toHaveBeenLastCalledWith('delete_vendor_oauth', { oauth_id: 'o1' });
  });
});
