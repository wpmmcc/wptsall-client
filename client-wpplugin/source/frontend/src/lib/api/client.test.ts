import { describe, expect, it, vi } from 'vitest';
import { apiFetch, hasData, isOk } from './client';

describe('api client helpers', () => {
  it('parses success JSON response', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      text: async () => JSON.stringify({ success: true, data: { value: 1 } }),
    });
    vi.stubGlobal('fetch', fetchMock);

    const res = await apiFetch<{ value: number }>('/api/demo');
    expect(isOk(res)).toBe(true);
    expect(hasData(res)).toBe(true);
    if (hasData(res)) {
      expect(res.data.value).toBe(1);
    }
    expect(fetchMock).toHaveBeenCalledWith('/api/demo', {
      method: 'GET',
      headers: { 'Content-Type': 'application/json' },
      body: undefined,
    });
  });

  it('returns PARSE_ERROR when response is not JSON', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      text: async () => 'not-json-response',
    });
    vi.stubGlobal('fetch', fetchMock);

    const res = await apiFetch('/api/demo');
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('PARSE_ERROR');
      expect(res.error.message).toContain('not-json-response');
    }
  });

  it('returns NETWORK_ERROR when fetch throws', async () => {
    const fetchMock = vi.fn().mockRejectedValue(new Error('boom'));
    vi.stubGlobal('fetch', fetchMock);

    const res = await apiFetch('/api/demo', { method: 'POST', body: { id: 1 } });
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('NETWORK_ERROR');
      expect(res.error.message).toContain('boom');
    }
  });

  it('preserves http status on structured error responses', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      status: 403,
      text: async () => JSON.stringify({
        success: false,
        error: { code: 'wptsall_pro_required', message: 'Client API disabled' },
      }),
    });
    vi.stubGlobal('fetch', fetchMock);

    const res = await apiFetch('/api/demo');
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('wptsall_pro_required');
      expect(res.error.status).toBe(403);
    }
  });
});
