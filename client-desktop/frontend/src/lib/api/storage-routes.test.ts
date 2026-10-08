import { beforeEach, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { invoke } from '@tauri-apps/api/core';
import { apiFetch } from './client';

beforeEach(() => vi.mocked(invoke).mockReset());

it('reads the shared physical inventory through the embedded agent', async () => {
  vi.mocked(invoke).mockResolvedValueOnce({ revision: 'owned-revision' });
  await apiFetch('/api/storage/capacity');
  expect(invoke).toHaveBeenLastCalledWith('proxy_webui_request', {
    method: 'GET', path: '/api/storage/capacity', body: undefined,
  });
});

it('preserves the exact physical policy revision and explicit confirmation', async () => {
  vi.mocked(invoke).mockResolvedValueOnce({ revision: 'owned-new-revision' });
  const body = {
    max_physical_bytes: 20000, expected_revision: 'owned-revision', confirm_change: true,
  };
  await apiFetch('/api/storage/capacity', { method: 'POST', body });
  expect(invoke).toHaveBeenLastCalledWith('proxy_webui_request', {
    method: 'POST', path: '/api/storage/capacity', body,
  });
});
