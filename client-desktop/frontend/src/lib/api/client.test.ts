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

  it('preserves manual request UUID and resume-only intent through the dedicated Desktop command', async () => {
    for (const request of [
      { request_id: '631b8b74-913a-4972-8892-33a892ce07bb' },
      { request_id: '631b8b74-913a-4972-8892-33a892ce07bb', resume_only: true },
    ]) {
      invokeMock.mockResolvedValueOnce({ item_id: 8, request_id: request.request_id, status: 'pending_review' });
      const result = await apiFetch('/api/items/8/retranslate', { method: 'POST', body: request });
      expect(invokeMock).toHaveBeenLastCalledWith('retranslate_item', { itemId: '8', request });
      expect(isOk(result)).toBe(true);
    }
  });

  it('passes pack entries and delivery authority unchanged between the shared review and Desktop commands', async () => {
    const content = { entries: [{ entry_id: 101, msgstr: 'Owned reviewed %s' }] };
    invokeMock.mockResolvedValueOnce({});
    await apiFetch('/api/items/8/translated', { method: 'PUT', body: { content } });
    expect(invokeMock).toHaveBeenLastCalledWith('save_item_translated', { itemId: '8', request: { content } });
    const data = { item: { id: 8 }, raw: {}, translated: {}, delivery_unresolved: true, manual_request_id: null };
    invokeMock.mockResolvedValueOnce(data);
    expect(await apiFetch('/api/items/8/content')).toEqual({ success: true, data });
    expect(invokeMock).toHaveBeenLastCalledWith('get_item_content', { itemId: '8' });
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

  it('passes the full WebUI upsert field set through to add_site without trimming', async () => {
    invokeMock.mockResolvedValueOnce({ id: 'https://example.test' });

    const result = await apiFetch('/api/domain-tokens/upsert', {
      method: 'POST',
      body: {
        api_base_url: 'https://example.test/wp-json/wptsall/v2/route-secret/client',
        wp_client_token: 'device-token',
        route_secret: 'route-secret',
        existing_api_base_url: 'https://old.example.test',
        plugin_identity: 'wpmmcc-ats@2.1.4',
      },
    });

    expect(invokeMock).toHaveBeenCalledWith('add_site', {
      request: {
        wp_url: 'https://example.test/wp-json/wptsall/v2/route-secret/client',
        token: 'device-token',
        route_secret: 'route-secret',
        existing_api_base_url: 'https://old.example.test',
        plugin_identity: 'wpmmcc-ats@2.1.4',
      },
    });
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

  it('preserves an already-pending retry response through the Desktop command', async () => {
    invokeMock.mockResolvedValueOnce({ queued: false });
    const result = await apiFetch('/api/translations/10/retry', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('retry_translation', { id: '10' });
    expect(result).toEqual({ success: true, data: { queued: false } });
  });

  it('maps worker and discovery controls without HTTP compatibility bypasses', async () => {
    invokeMock.mockResolvedValueOnce(undefined);
    await apiFetch('/api/worker/start', { method: 'POST' });
    expect(invokeMock).toHaveBeenLastCalledWith('start_worker', {});

    // 批 N3 / U-7: the Stop Loop control must reach the stop_worker command
    // with an empty arg envelope — the shared caller posts {} and the command
    // takes no parameters. This was previously unasserted anywhere.
    invokeMock.mockResolvedValueOnce(undefined);
    await apiFetch('/api/worker/stop', { method: 'POST', body: {} });
    expect(invokeMock).toHaveBeenLastCalledWith('stop_worker', {});

    invokeMock.mockResolvedValueOnce({ tasks_succeeded: 1 });
    // 批 M / doc21 G1: Overview sends the flat RunOnceRequest ({} or
    // { max_items_per_run }); the bridge wraps it into the command's
    // `request` arg. The previous expectation pre-wrapped the body and so
    // codified the bridge bug (desktop Run Once always failed arg validation).
    await apiFetch('/api/worker/run-once', { method: 'POST', body: {} });
    expect(invokeMock).toHaveBeenLastCalledWith('run_worker_once', { request: {} });
    await apiFetch('/api/worker/run-once', {
      method: 'POST',
      body: { max_iterations: 4, max_items_per_run: 64, max_elapsed_secs: 180 },
    });
    expect(invokeMock).toHaveBeenLastCalledWith('run_worker_once', {
      request: { max_iterations: 4, max_items_per_run: 64, max_elapsed_secs: 180 },
    });

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
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/provider-catalog',
      body: undefined,
    });

    const pack = { schema: 'wptsall-integration-pack.v1', redacted: true };
    invokeMock.mockResolvedValueOnce({ safe_to_import: true });
    await apiFetch('/api/integrations/pack/preview', { method: 'POST', body: pack });
    expect(invokeMock).toHaveBeenLastCalledWith('preview_integration_pack', { request: pack });

    const install = { entry_id: 'catalog-entry', local_id: 'local-entry' };
    invokeMock.mockResolvedValueOnce({ id: 'local-entry', enabled: false });
    await apiFetch('/api/components/local/install-from-catalog', { method: 'POST', body: install });
    expect(invokeMock).toHaveBeenLastCalledWith('install_catalog_template', { request: install });
  });

  it('fails closed for non-API paths; /api/* falls through to agent proxy', async () => {
    const unknown = await apiFetch('/not-a-mapped-desktop-route');
    expect(invokeMock).not.toHaveBeenCalled();
    expect(unknown.success).toBe(false);
    if (!unknown.success) {
      expect(unknown.error.code).toBe('UNKNOWN_PATH');
    }

    invokeMock.mockResolvedValueOnce({});
    const proxied = await apiFetch('/api/legacy-token-login');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/legacy-token-login',
      body: undefined,
    });
    expect(isOk(proxied)).toBe(true);
  });

  it('proxies media receipt listing and explicit reconciliation without altering evidence', async () => {
    invokeMock.mockResolvedValueOnce({ items: [] });
    const list = await apiFetch('/api/media-recovery');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET',
      path: '/api/media-recovery',
      body: undefined,
    });
    expect(isOk(list)).toBe(true);
    const body = { operation_id: '64acb7df-8366-45b5-98a7-944ff8f995ac', attachment_id: 321 };
    invokeMock.mockRejectedValueOnce('MEDIA_REVIEW_REQUIRED: evidence mismatch');
    const refused = await apiFetch('/api/media-recovery/reconcile', { method: 'POST', body });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST',
      path: '/api/media-recovery/reconcile',
      body,
    });
    expect(refused.success).toBe(false);
    if (!refused.success) expect(refused.error.message).toContain('evidence mismatch');
  });

  it('proxies provider evidence listing and explicit review without injecting job IDs', async () => {
    invokeMock.mockResolvedValueOnce({ items: [] });
    const list = await apiFetch('/api/provider-recovery');
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'GET', path: '/api/provider-recovery', body: undefined,
    });
    expect(isOk(list)).toBe(true);
    const body = { operation_id: '64acb7df-8366-45b5-98a7-944ff8f995ac' };
    invokeMock.mockResolvedValueOnce({ state: 'polling' });
    const verified = await apiFetch('/api/provider-recovery/reconcile', { method: 'POST', body });
    expect(invokeMock).toHaveBeenLastCalledWith('proxy_webui_request', {
      method: 'POST', path: '/api/provider-recovery/reconcile', body,
    });
    expect(isOk(verified)).toBe(true);
    invokeMock.mockRejectedValueOnce('PROVIDER_REVIEW_REQUIRED: evidence mismatch');
    const refused = await apiFetch('/api/provider-recovery/reconcile', { method: 'POST', body });
    expect(refused.success).toBe(false);
    if (!refused.success) expect(refused.error.message).toContain('evidence mismatch');
  });
});
