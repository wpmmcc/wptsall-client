// catalog: WEBUI-API-GET-api-provider-catalog
// oracle: L2
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import { invoke } from '@tauri-apps/api/core';
import { setApiFetchHandler } from '@webui/lib/api/client';
import { listProviderCatalog } from '@webui/lib/api/keys';
import { apiFetch as desktopApiFetch } from './client';

const invokeMock = vi.mocked(invoke);
const catalog = {
  schema: 'wptsall-provider-catalog-manifest.v1',
  catalog_version: 'ui01-fixture',
  offline: true,
  items: [],
};

describe('UI-01 shared provider catalog request parity', () => {
  beforeEach(() => {
    setApiFetchHandler(null);
    invokeMock.mockReset();
  });

  afterEach(() => {
    setApiFetchHandler(null);
    vi.unstubAllGlobals();
  });

  it.each([
    { q: 'openai' },
    { vendor_id: 'openai' },
    { q: '  图片 + &? #=%  ', vendor_id: ' custom_http_mt ' },
    { q: '', vendor_id: '' },
    { q: '   ', vendor_id: ' OPENAI ' },
    { q: 'openai', check: true },
  ])('preserves the shared WebUI GET for %j', async params => {
    const fetchMock = vi.fn().mockResolvedValue({
      status: 200,
      text: async () => JSON.stringify({ success: true, data: catalog }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const webuiResult = await listProviderCatalog(params);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, options] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(options.method).toBe('GET');
    expect(options.body).toBeUndefined();

    invokeMock.mockResolvedValue(catalog);
    setApiFetchHandler(desktopApiFetch);
    const desktopResult = await listProviderCatalog(params);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('proxy_webui_request', {
      method: options.method,
      path,
      body: undefined,
    });
    expect(desktopResult).toEqual(webuiResult);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('keeps repeated and unknown query fields without double-encoding', async () => {
    invokeMock.mockResolvedValue(catalog);
    const result = await desktopApiFetch(
      '/api/provider-catalog?q=A%20B%2B%26%3F&vendor_id=OPENAI&future=one&future=two'
    );
    expect(invokeMock).toHaveBeenCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/provider-catalog?q=A+B%2B%26%3F&vendor_id=OPENAI&future=one&future=two',
      body: undefined,
    });
    expect(result).toEqual({ success: true, data: catalog });
  });

  it('preserves native agent rejection messages for the shared caller', async () => {
    const message = 'CATALOG_FIXTURE_ERROR: fixture unavailable (HTTP 500 Internal Server Error)';
    invokeMock.mockRejectedValue(message);
    setApiFetchHandler(desktopApiFetch);
    expect(await listProviderCatalog({ q: 'openai' })).toEqual({
      success: false,
      error: { code: 'INVOKE_ERROR', message },
    });
  });

  it('reports invoke failures rather than returning an unfiltered catalog', async () => {
    invokeMock.mockRejectedValue(new Error('fixture transport failure'));
    setApiFetchHandler(desktopApiFetch);
    expect(await listProviderCatalog({ q: 'openai' })).toEqual({
      success: false,
      error: { code: 'INVOKE_ERROR', message: 'fixture transport failure' },
    });
  });
});
