import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch } from './client';

const invokeMock = vi.mocked(invoke);

// G-12b: when the loopback proxy (proxy_webui_request → embedded WebUI
// service on :8977) is unavailable, unmapped /api/* paths must surface a
// structured ApiResult error — the tauriCall catch normalizes any invoke
// rejection into { code: 'INVOKE_ERROR', message } — never a thrown
// rejection, crash, or hang.
describe('desktop apiFetch loopback-proxy failure handling', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('routes unmapped paths through proxy_webui_request and normalizes a rejected bridge call', async () => {
    invokeMock.mockRejectedValueOnce(
      new Error('loopback 127.0.0.1:8977 connection refused')
    );

    const result = await apiFetch('/api/sync-pairs', {
      method: 'POST',
      body: { relation_id: 120 },
    });

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/sync-pairs',
      body: { relation_id: 120 },
    });
    // Settled with a structured error, not a thrown rejection.
    expect(result.success).toBe(false);
    if (!result.success) {
      expect(result.error.code).toBe('INVOKE_ERROR');
      expect(result.error.message).toBe(
        'loopback 127.0.0.1:8977 connection refused'
      );
    }
  });

  it('keeps the structured error shape for string rejections from the invoke bridge', async () => {
    invokeMock.mockRejectedValueOnce('embedded WebUI service unavailable');

    const result = await apiFetch('/api/sync-pairs', {
      method: 'POST',
      body: { relation_id: 441 },
    });

    expect(result.success).toBe(false);
    if (!result.success) {
      expect(result.error.code).toBe('INVOKE_ERROR');
      expect(result.error.message).toBe('embedded WebUI service unavailable');
    }
  });

  it('falls back to the Unknown error message for non-string, non-Error rejections', async () => {
    invokeMock.mockRejectedValueOnce({ unexpected: 'shape' });

    const result = await apiFetch('/api/sync-pairs', { method: 'POST' });

    expect(result.success).toBe(false);
    if (!result.success) {
      expect(result.error.code).toBe('INVOKE_ERROR');
      expect(result.error.message).toBe('Unknown error');
    }
  });
});
