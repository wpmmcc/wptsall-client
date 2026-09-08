import { beforeEach, describe, expect, it, vi } from 'vitest';
import { deleteDomainToken, testDomainToken, upsertDomainToken } from './sites';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/sites', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('upserts/deletes/tests domain token payloads', async () => {
    await upsertDomainToken({
      api_base_url: 'https://blog.wpmm.cc',
      wp_client_token: 'wptc1.xxx',
    });
    await deleteDomainToken('https://blog.wpmm.cc');
    await testDomainToken('https://blog.wpmm.cc');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/domain-tokens/upsert', {
      method: 'POST',
      body: { api_base_url: 'https://blog.wpmm.cc', wp_client_token: 'wptc1.xxx' },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/domain-tokens/delete', {
      method: 'POST',
      body: { api_base_url: 'https://blog.wpmm.cc' },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/domain-tokens/test', {
      method: 'POST',
      body: { api_base_url: 'https://blog.wpmm.cc' },
    });
  });
});
