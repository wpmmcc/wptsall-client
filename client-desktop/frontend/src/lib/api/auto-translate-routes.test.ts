/**
 * Desktop API parity for auto-translate paths used by shared WebUI pages.
 *
 * Shared pages call `/api/discovery-tasks`, `/api/sync-pairs`, `/api/components/local`,
 * `/api/worker/config` (review_mode), and batch-approve. Desktop must proxy these
 * to the embedded agent — not return UNKNOWN_PATH / drop review_mode.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch, isOk } from './client';

const invokeMock = vi.mocked(invoke);

describe('desktop auto-translate API parity (proxy fallback)', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('proxies discovery-tasks list / bootstrap / update', async () => {
    invokeMock.mockResolvedValueOnce({ items: [] });
    await apiFetch('/api/discovery-tasks');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/discovery-tasks',
      body: undefined,
    });

    invokeMock.mockResolvedValueOnce({ created: 1 });
    await apiFetch('/api/discovery-tasks/bootstrap', { method: 'POST', body: {} });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/discovery-tasks/bootstrap',
      body: undefined,
    });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/discovery-tasks/42', {
      method: 'PUT',
      body: { enabled: true, selected_component_id: 'comp-1' },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'PUT',
      path: '/api/discovery-tasks/42',
      body: { enabled: true, selected_component_id: 'comp-1' },
    });
  });

  it('proxies GET/POST worker/config including review_mode (not configure_worker)', async () => {
    invokeMock.mockResolvedValueOnce({ review_mode: false });
    await apiFetch('/api/worker/config');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/worker/config',
      body: undefined,
    });

    invokeMock.mockResolvedValueOnce({ review_mode: true });
    const body = { review_mode: true, poll_seconds: 20 };
    await apiFetch('/api/worker/config', { method: 'POST', body });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/worker/config',
      body,
    });
  });

  it('proxies components/local create and bindings upsert with query strings', async () => {
    invokeMock.mockResolvedValueOnce({ items: [] });
    await apiFetch('/api/components/local?page=1&per_page=50&q=auto');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/components/local?page=1&per_page=50&q=auto',
      body: undefined,
    });

    invokeMock.mockResolvedValueOnce({ id: 'c1' });
    await apiFetch('/api/components/local', {
      method: 'POST',
      body: { id: 'c1', name: 'Auto', kind: 'text' },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/components/local',
      body: { id: 'c1', name: 'Auto', kind: 'text' },
    });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/components/bindings/upsert', {
      method: 'POST',
      body: { component_id: 'c1', auth: { api_key: 'k' } },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/components/bindings/upsert',
      body: { component_id: 'c1', auth: { api_key: 'k' } },
    });
  });

  it('proxies sync-pairs CRUD and run for WPMMCC dual-plugin flows', async () => {
    invokeMock.mockResolvedValueOnce({ pairs: [] });
    await apiFetch('/api/sync-pairs');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/sync-pairs',
      body: undefined,
    });

    invokeMock.mockResolvedValueOnce({ id: 'p1', status: 'active' });
    await apiFetch('/api/sync-pairs/p1/run', { method: 'POST', body: {} });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/sync-pairs/p1/run',
      body: undefined,
    });
  });

  it('proxies batch-approve for pending review auto-translate ops', async () => {
    invokeMock.mockResolvedValueOnce({ approved: 2 });
    const result = await apiFetch('/api/items/batch-approve', {
      method: 'POST',
      body: { item_ids: [1, 2] },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/items/batch-approve',
      body: { item_ids: [1, 2] },
    });
    expect(isOk(result)).toBe(true);
  });
});
