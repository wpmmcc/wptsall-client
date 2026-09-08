import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch, isOk } from './client';

const invokeMock = vi.mocked(invoke);

describe('desktop P1-G-2 jobs/items/logs/quick-test routes', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('maps vendor-key CRUD through create/update/delete commands', async () => {
    invokeMock.mockResolvedValueOnce({ items: [] });
    await apiFetch('/api/vendor-keys');
    expect(invokeMock).toHaveBeenLastCalledWith('list_vendor_keys', {});

    invokeMock.mockResolvedValueOnce({ id: 'k1' });
    await apiFetch('/api/vendor-keys', {
      method: 'POST',
      body: { vendor_id: 'openai', api_key: 'sk-test' },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('create_vendor_key', {
      request: { vendor_id: 'openai', api_key: 'sk-test' },
    });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/vendor-keys/k1', { method: 'DELETE' });
    expect(invokeMock).toHaveBeenLastCalledWith('delete_vendor_key', { key_id: 'k1' });
  });

  it('maps jobs list/detail/items with query args', async () => {
    invokeMock.mockResolvedValueOnce({ items: [] });
    await apiFetch('/api/jobs?limit=20&domain=https%3A%2F%2Fexample.test');
    expect(invokeMock).toHaveBeenLastCalledWith('list_jobs', {
      domain: 'https://example.test',
      limit: 20,
      offset: undefined,
    });

    invokeMock.mockResolvedValueOnce({ id: 9 });
    await apiFetch('/api/jobs/9');
    expect(invokeMock).toHaveBeenLastCalledWith('get_job', { jobId: '9' });

    invokeMock.mockResolvedValueOnce({ items: [] });
    await apiFetch('/api/jobs/9/items?status=pending_review');
    expect(invokeMock).toHaveBeenLastCalledWith('list_job_items', {
      jobId: '9',
      status: 'pending_review',
    });
  });

  it('maps item review actions', async () => {
    invokeMock.mockResolvedValueOnce({ item: { id: 5 } });
    await apiFetch('/api/items/5/content');
    expect(invokeMock).toHaveBeenLastCalledWith('get_item_content', { itemId: '5' });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/items/5/translated', {
      method: 'PUT',
      body: { content: 'hello' },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('save_item_translated', {
      itemId: '5',
      request: { content: 'hello' },
    });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/items/5/approve', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('approve_item', { itemId: '5' });

    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/items/5/retranslate', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('retranslate_item', { itemId: '5' });
  });

  it('maps logs recent and component quick-test', async () => {
    invokeMock.mockResolvedValueOnce({ lines: ['a'] });
    const logs = await apiFetch('/api/logs/recent', { method: 'POST', body: { limit: 50 } });
    expect(invokeMock).toHaveBeenLastCalledWith('get_recent_logs', {
      request: { limit: 50 },
    });
    expect(isOk(logs)).toBe(true);

    invokeMock.mockResolvedValueOnce({ ok: true });
    await apiFetch('/api/components/local/desktop-local-openai/quick-test', {
      method: 'POST',
      body: { api_key: 'sk' },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('quick_test_component', {
      componentId: 'desktop-local-openai',
      request: { api_key: 'sk' },
    });
  });
});
