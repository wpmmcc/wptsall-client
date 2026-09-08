import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import { invoke } from '@tauri-apps/api/core';
import { apiFetch, isOk } from './client';

const invokeMock = vi.mocked(invoke);

describe('desktop apiFetch Tauri adapter', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('maps status and OAuth requests to Tauri auth commands', async () => {
    invokeMock.mockResolvedValueOnce({ logged_in: false, domains: [] });

    const status = await apiFetch('/api/status');

    expect(invokeMock).toHaveBeenCalledWith('get_status', {});
    expect(isOk(status)).toBe(true);

    invokeMock.mockResolvedValueOnce({ authorize_url: 'http://127.0.0.1:8787/oauth/authorize' });
    const oauth = await apiFetch('/api/oauth/start', { method: 'POST' });

    expect(invokeMock).toHaveBeenLastCalledWith('start_oauth', {});
    expect(isOk(oauth)).toBe(true);
  });

  it('preserves the AddSiteRequest envelope expected by the Tauri command', async () => {
    const body = {
      request: {
        wp_url: 'https://example.test/wp-json/wptsall/v2/route-secret/client',
        token: 'device-token',
        route_secret: null,
      },
    };
    invokeMock.mockResolvedValueOnce({ id: 'https://example.test' });

    const result = await apiFetch('/api/sites/add', { method: 'POST', body });

    expect(invokeMock).toHaveBeenCalledWith('add_site', body);
    expect(isOk(result)).toBe(true);
  });

  it('uses camelCase invoke args for Rust snake_case command parameters', async () => {
    invokeMock.mockResolvedValueOnce(true);

    const result = await apiFetch('/api/sites/test', {
      method: 'POST',
      body: { siteId: 'https://example.test' },
    });

    expect(invokeMock).toHaveBeenCalledWith('test_connection', {
      siteId: 'https://example.test',
    });
    expect(isOk(result)).toBe(true);
  });

  it('maps worker and discovery controls without HTTP compatibility bypasses', async () => {
    invokeMock.mockResolvedValueOnce(undefined);
    await apiFetch('/api/worker/start', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('start_worker', {});

    invokeMock.mockResolvedValueOnce({ tasks_succeeded: 1 });
    const runOnceBody = { request: { max_iterations: 4, max_items_per_run: 64, max_elapsed_secs: 180 } };
    await apiFetch('/api/worker/run-once', { method: 'POST', body: runOnceBody });
    expect(invokeMock).toHaveBeenLastCalledWith('run_worker_once', runOnceBody);

    invokeMock.mockResolvedValueOnce('discovery-tasks-bootstrap');
    await apiFetch('/api/tasks/discover', {
      method: 'POST',
      body: { siteId: '' },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('trigger_discover', { siteId: '' });

    const focusBody = { request: { relation_id: 120, include_resync: true, selected_component_id: 'desktop-local-openai' } };
    invokeMock.mockResolvedValueOnce({ relation_id: 120, updated: 5, enabled: 1 });
    await apiFetch('/api/tasks/focus-relation', { method: 'POST', body: focusBody });
    expect(invokeMock).toHaveBeenLastCalledWith('focus_discovery_relation', focusBody);
  });

  it('maps local mock component configuration to the explicit Tauri command', async () => {
    const body = {
      request: {
        id: 'desktop-local-openai',
        api_base: 'http://127.0.0.1:9090',
        model: 'mock-openai-v1',
        api_key: 'mock-translate-dev-key-2026',
      },
    };
    invokeMock.mockResolvedValueOnce({ id: 'desktop-local-openai', configured: true });

    const result = await apiFetch('/api/components/configure-mock', { method: 'POST', body });

    expect(invokeMock).toHaveBeenCalledWith('configure_mock_component', body);
    expect(isOk(result)).toBe(true);
  });

  it('maps provider catalog and integration-pack requests to safe Tauri envelopes', async () => {
    invokeMock.mockResolvedValueOnce({
      schema: 'wptsall-provider-catalog-manifest.v1',
      catalog_version: 'builtin-1',
      offline: true,
      items: [],
    });
    await apiFetch('/api/provider-catalog');
    expect(invokeMock).toHaveBeenLastCalledWith('list_provider_catalog', {});

    const pack = { schema: 'wptsall-integration-pack.v1', redacted: true };
    invokeMock.mockResolvedValueOnce({ safe_to_import: true });
    await apiFetch('/api/integrations/pack/preview', { method: 'POST', body: pack });
    expect(invokeMock).toHaveBeenLastCalledWith('preview_integration_pack', { request: pack });

    const install = { entry_id: 'catalog-entry', local_id: 'local-entry' };
    invokeMock.mockResolvedValueOnce({ id: 'local-entry', enabled: false });
    await apiFetch('/api/components/local/install-from-catalog', { method: 'POST', body: install });
    expect(invokeMock).toHaveBeenLastCalledWith('install_catalog_template', { request: install });
  });

  it('fails closed for unmapped paths', async () => {
    const result = await apiFetch('/api/legacy-token-login');

    expect(invokeMock).not.toHaveBeenCalled();
    expect(result.success).toBe(false);
    if (!result.success) {
      expect(result.error.code).toBe('UNKNOWN_PATH');
    }
  });
});
