import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  authorizeOAuth,
  createVendorKey,
  deleteOAuthConfig,
  deleteVendorKey,
  listOAuthConfigs,
  listVendorKeys,
  listProviderCatalog,
  installProviderCatalogVersion,
  testProvider,
  updateVendorKey,
} from './keys';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/keys', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('lists vendor keys with vendor filter', async () => {
    await listVendorKeys('google/translate');
    expect(apiFetchMock).toHaveBeenCalledWith('/api/vendor-keys?vendor_id=google%2Ftranslate');
  });

  it('checks only on catalog open and confirms the exact installed version', async () => {
    await listProviderCatalog({ q: '  OpenAI  ', check: true });
    await installProviderCatalogVersion('1.2.3');
    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/provider-catalog?q=OpenAI&check=1');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/provider-catalog/install-version', {
      method: 'POST', body: { version: '1.2.3', confirm: true },
    });
  });

  it('creates/updates/deletes vendor key with encoded id', async () => {
    await createVendorKey({ id: 'key-1', vendor_id: 'google', auth_values: { api_key: 'x' } });
    await updateVendorKey('key/1', { enabled: false });
    await deleteVendorKey('key/1');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/vendor-keys', {
      method: 'POST',
      body: { id: 'key-1', vendor_id: 'google', auth_values: { api_key: 'x' } },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/vendor-keys/key%2F1', {
      method: 'PUT',
      body: { enabled: false },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/vendor-keys/key%2F1', {
      method: 'DELETE',
    });
  });

  it('posts provider connectivity probe', async () => {
    await testProvider({
      key_id: 'key-1',
      vendor_id: 'openai',
      text: 'Hi',
      base_url: 'https://api.openai.com',
      model: 'gpt-4o-mini',
    });
    expect(apiFetchMock).toHaveBeenCalledWith('/api/providers/test', {
      method: 'POST',
      body: {
        key_id: 'key-1',
        vendor_id: 'openai',
        text: 'Hi',
        base_url: 'https://api.openai.com',
        model: 'gpt-4o-mini',
      },
    });
  });

  it('handles oauth endpoints', async () => {
    await listOAuthConfigs();
    await deleteOAuthConfig('oauth/1');
    await authorizeOAuth('oauth/1');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/vendor-oauth');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/vendor-oauth/oauth%2F1', {
      method: 'DELETE',
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/vendor-oauth/oauth%2F1/authorize', {
      method: 'POST',
      body: {},
    });
  });
});