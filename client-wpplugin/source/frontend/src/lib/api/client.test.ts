import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { apiFetch, hasData, isOk, storeWebUiToken } from './client';

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

describe('webui access token (S2 external-mode gate)', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  it('attaches a stored token to every request', async () => {
    storeWebUiToken('tok-1234');
    const fetchMock = vi.fn().mockResolvedValue({
      text: async () => JSON.stringify({ success: true, data: { ok: 1 } }),
    });
    vi.stubGlobal('fetch', fetchMock);

    const res = await apiFetch<{ ok: number }>('/api/demo');
    expect(isOk(res)).toBe(true);
    expect(fetchMock).toHaveBeenCalledWith('/api/demo', {
      method: 'GET',
      headers: {
        'Content-Type': 'application/json',
        'X-WPTSALL-WebUI-Token': 'tok-1234',
      },
      body: undefined,
    });
  });

  it('prompts once for the token on WEBUI_TOKEN_REQUIRED, stores it and retries', async () => {
    const tokenRequired = {
      status: 401,
      text: async () => JSON.stringify({
        success: false,
        error: { code: 'WEBUI_TOKEN_REQUIRED', message: 'web ui access token required' },
      }),
    };
    const success = {
      status: 200,
      text: async () => JSON.stringify({ success: true, data: { ok: 1 } }),
    };
    const fetchMock = vi.fn().mockResolvedValueOnce(tokenRequired).mockResolvedValueOnce(success);
    vi.stubGlobal('fetch', fetchMock);
    const promptSpy = vi.spyOn(window, 'prompt').mockReturnValue('entered-token');

    const res = await apiFetch<{ ok: number }>('/api/demo');
    expect(isOk(res)).toBe(true);
    expect(promptSpy).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    // The retry must carry the token entered in the prompt.
    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/demo', {
      method: 'GET',
      headers: {
        'Content-Type': 'application/json',
        'X-WPTSALL-WebUI-Token': 'entered-token',
      },
      body: undefined,
    });
    // The token is persisted for subsequent requests.
    expect(localStorage.getItem('wptsall_webui_token')).toBe('entered-token');
  });

  it('returns the token error without retry when the prompt is cancelled', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      status: 401,
      text: async () => JSON.stringify({
        success: false,
        error: { code: 'WEBUI_TOKEN_REQUIRED', message: 'web ui access token required' },
      }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const promptSpy = vi.spyOn(window, 'prompt').mockReturnValue(null);

    const res = await apiFetch('/api/demo');
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('WEBUI_TOKEN_REQUIRED');
      expect(res.error.status).toBe(401);
    }
    expect(promptSpy).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  // 批 N2 / U-6: a 401 that is NOT the S2 token gate must pass straight
  // through — no prompt, no retry, status preserved.
  it('passes a non-token 401 through without prompting', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      status: 401,
      text: async () => JSON.stringify({
        success: false,
        error: { code: 'invalid_signature', message: 'request signature rejected' },
      }),
    });
    vi.stubGlobal('fetch', fetchMock);
    const promptSpy = vi.spyOn(window, 'prompt').mockReturnValue('should-not-be-used');

    const res = await apiFetch('/api/demo');
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('invalid_signature');
      expect(res.error.status).toBe(401);
    }
    expect(promptSpy).not.toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  // 批 N2 / U-6: aborted fetches (user navigation, timeout teardown) map to
  // the NETWORK_ERROR envelope — never an unhandled rejection — and the
  // failure is terminal: no retry attempt is made for transport errors.
  it('maps a fetch AbortError to NETWORK_ERROR without retry', async () => {
    const abortError = new DOMException('The operation was aborted', 'AbortError');
    const fetchMock = vi.fn().mockRejectedValue(abortError);
    vi.stubGlobal('fetch', fetchMock);

    const res = await apiFetch('/api/demo');
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('NETWORK_ERROR');
      expect(res.error.message).toContain('AbortError');
    }
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  // 批 N2 / U-6: when the freshly entered token is still rejected the
  // retry cap kicks in — exactly one prompt per request, then passthrough.
  it('does not prompt a second time when the entered token is rejected', async () => {
    const tokenRequired = {
      status: 401,
      text: async () => JSON.stringify({
        success: false,
        error: { code: 'WEBUI_TOKEN_REQUIRED', message: 'web ui access token required' },
      }),
    };
    const fetchMock = vi.fn().mockResolvedValue(tokenRequired);
    vi.stubGlobal('fetch', fetchMock);
    const promptSpy = vi.spyOn(window, 'prompt').mockReturnValue('wrong-token');

    const res = await apiFetch('/api/demo');
    expect(res.success).toBe(false);
    if (!res.success) {
      expect(res.error.code).toBe('WEBUI_TOKEN_REQUIRED');
      expect(res.error.status).toBe(401);
    }
    expect(promptSpy).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledTimes(2);
    // The retry carried the (rejected) entered token.
    expect(fetchMock).toHaveBeenNthCalledWith(2, '/api/demo', {
      method: 'GET',
      headers: {
        'Content-Type': 'application/json',
        'X-WPTSALL-WebUI-Token': 'wrong-token',
      },
      body: undefined,
    });
  });
});
