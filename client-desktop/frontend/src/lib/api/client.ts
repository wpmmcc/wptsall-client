/**
 * Desktop API client — uses Tauri invoke instead of HTTP fetch.
 * This is the key difference from the WebUI client's api/client.ts.
 *
 * The WebUI version uses: fetch('/api/path')
 * The Desktop version uses: invoke('command_name', { args })
 *
 * Both return the same ApiResult<T> shape, so all page components
 * can be shared without modification.
 */
import { invoke } from '@tauri-apps/api/core';
import type { ApiResult } from './types';

export async function tauriCall<T>(
  command: string,
  args: Record<string, unknown> = {}
): Promise<ApiResult<T>> {
  try {
    const result = await invoke<T>(command, args);
    return { success: true, data: result };
  } catch (e: unknown) {
    const message = typeof e === 'string' ? e : (e as Error)?.message ?? 'Unknown error';
    return {
      success: false,
      error: { code: 'INVOKE_ERROR', message },
    };
  }
}

/**
 * Compatibility shim: maps the same path-based API calls used by
 * shared page components to Tauri invoke commands.
 *
 * Usage in shared components remains:
 *   import { apiFetch } from '$lib/api/client';
 *   const result = await apiFetch<Sites[]>('/api/sites');
 *
 * This adapter translates path → invoke command automatically.
 */
const REQUEST_WRAPPED_PATHS = new Set([
  '/api/components/local/install-from-catalog',
  '/api/integrations/pack/export',
  '/api/integrations/pack/preview',
  '/api/integrations/pack/import',
]);

const PATH_TO_COMMAND: Record<string, string> = {
  '/api/status': 'get_status',
  '/api/auth/login': 'login',
  '/api/oauth/start': 'start_oauth',
  '/api/auth/logout': 'logout',
  '/api/domains/refresh': 'refresh_domains',
  '/api/sites': 'list_sites',
  '/api/sites/add': 'add_site',
  '/api/site-connections/import': 'import_site_connection',
  '/api/sites/remove': 'remove_site',
  '/api/sites/test': 'test_connection',
  '/api/tasks': 'list_tasks',
  '/api/tasks/discover': 'trigger_discover',
  '/api/tasks/detail': 'get_task_detail',
  '/api/tasks/focus-relation': 'focus_discovery_relation',
  '/api/settings': 'get_settings',
  '/api/settings/update': 'update_settings',
  '/api/components': 'list_components',
  '/api/components/configure': 'configure_component',
  '/api/components/configure-mock': 'configure_mock_component',
  '/api/provider-catalog': 'list_provider_catalog',
  '/api/provider-catalog/refresh': 'refresh_provider_catalog',
  '/api/components/local/install-from-catalog': 'install_catalog_template',
  '/api/integrations/pack/export': 'export_integration_pack',
  '/api/integrations/pack/preview': 'preview_integration_pack',
  '/api/integrations/pack/import': 'import_integration_pack',
  '/api/worker/status': 'get_worker_status',
  '/api/worker/start': 'start_worker',
  '/api/worker/stop': 'stop_worker',
  '/api/worker/config': 'configure_worker',
  '/api/worker/run-once': 'run_worker_once',
  '/api/keys': 'list_keys',
  '/api/keys/save': 'save_key',
  '/api/keys/delete': 'delete_key',
  // P1-G: vendor keys / OAuth / proxy profiles (proxied to embedded WebUI).
  '/api/vendor-keys': 'list_vendor_keys',
  '/api/vendor-oauth': 'list_vendor_oauth',
  '/api/proxy-profiles': 'list_proxy_profiles',
  '/api/translations': 'list_translations',
  '/api/translations/batch-retry': 'batch_retry_translations',
  '/api/translations/batch-delete': 'batch_delete_translations',
  '/api/translations/update': 'update_translation',
  '/api/logs': 'get_logs',
  '/api/logs/recent': 'get_recent_logs',
  '/api/jobs': 'list_jobs',
  // P1-A-10: legacy server-control-plane surface; the default local-first
  // Desktop UI never calls this path. Kept for legacy-mode compatibility.
  '/api/platform': 'get_platform_info',
  '/api/update-check': 'check_for_update',
  '/api/perform-update': 'perform_update',
};

/**
 * P1-G: dynamic-path routing for `{id}`-style endpoints.
 *
 * WebUI page components build paths like `/api/vendor-keys/{id}` with PUT/
 * DELETE, and `/api/vendor-oauth/{id}/authorize` + `/api/proxy-profiles/{id}/
 * test` with POST. The static `PATH_TO_COMMAND` map cannot match these, so
 * `resolveDynamicRoute` pattern-matches the path + method and returns the
 * command name plus the args to pass to `invoke`. Returns `null` when no
 * dynamic route matches (caller falls back to the static map / UNKNOWN_PATH).
 */
interface DynamicRoute {
  command: string;
  args: Record<string, unknown>;
}

/**
 * P1-G: collection-level POST (create) routing. A POST to `/api/vendor-keys`,
 * `/api/vendor-oauth`, or `/api/proxy-profiles` must hit the `create_*`
 * command, not the `list_*` command the static map points to. This is
 * checked before the static map lookup.
 */
function resolveCollectionCreate(
  path: string,
  method: string,
  body: Record<string, unknown>
): DynamicRoute | null {
  if (method !== 'POST') return null;
  if (path === '/api/vendor-keys')
    return { command: 'create_vendor_key', args: { request: body } };
  if (path === '/api/vendor-oauth')
    return { command: 'create_vendor_oauth', args: { request: body } };
  if (path === '/api/proxy-profiles')
    return { command: 'create_proxy_profile', args: { request: body } };
  if (path === '/api/logs/recent')
    return { command: 'get_recent_logs', args: { request: body } };
  if (path === '/api/translations/batch-retry')
    return { command: 'batch_retry_translations', args: { request: body } };
  if (path === '/api/translations/batch-delete')
    return { command: 'batch_delete_translations', args: { request: body } };
  return null;
}

function resolveDynamicRoute(
  path: string,
  method: string,
  body: Record<string, unknown>
): DynamicRoute | null {
  const segments = path.split('/').filter((s) => s.length > 0);
  // Expect [api, <resource>, <id>, ...?]
  if (segments.length < 3 || segments[0] !== 'api') return null;
  const resource = segments[1];
  const id = decodeURIComponent(segments[2]);
  const tail = segments.slice(3); // e.g. ['authorize'] or ['test'] or []

  if (resource === 'vendor-keys') {
    if (method === 'PUT')
      return { command: 'update_vendor_key', args: { key_id: id, request: body } };
    if (method === 'DELETE')
      return { command: 'delete_vendor_key', args: { key_id: id } };
  }
  if (resource === 'vendor-oauth') {
    if (tail.length === 1 && tail[0] === 'authorize' && method === 'POST')
      return { command: 'authorize_vendor_oauth', args: { oauth_id: id } };
    if (method === 'PUT')
      return { command: 'update_vendor_oauth', args: { oauth_id: id, request: body } };
    if (method === 'DELETE')
      return { command: 'delete_vendor_oauth', args: { oauth_id: id } };
  }
  if (resource === 'proxy-profiles') {
    if (tail.length === 1 && tail[0] === 'test' && method === 'POST')
      return { command: 'test_proxy_profile', args: { proxy_id: id } };
    if (method === 'PUT')
      return { command: 'update_proxy_profile', args: { proxy_id: id, request: body } };
    if (method === 'DELETE')
      return { command: 'delete_proxy_profile', args: { proxy_id: id } };
  }
  if (resource === 'jobs') {
    if (tail.length === 0 && method === 'GET')
      return { command: 'get_job', args: { jobId: id } };
    if (tail.length === 1 && tail[0] === 'items' && method === 'GET')
      return {
        command: 'list_job_items',
        args: {
          jobId: id,
          status: typeof body.status === 'string' ? body.status : undefined,
        },
      };
  }
  if (resource === 'items') {
    if (tail.length === 1 && tail[0] === 'content' && method === 'GET')
      return { command: 'get_item_content', args: { itemId: id } };
    if (tail.length === 1 && tail[0] === 'translated' && method === 'PUT')
      return { command: 'save_item_translated', args: { itemId: id, request: body } };
    if (tail.length === 1 && tail[0] === 'approve' && method === 'POST')
      return { command: 'approve_item', args: { itemId: id } };
    if (tail.length === 1 && tail[0] === 'resubmit' && method === 'POST')
      return { command: 'resubmit_item', args: { itemId: id } };
    if (tail.length === 1 && tail[0] === 'retranslate' && method === 'POST')
      return { command: 'retranslate_item', args: { itemId: id } };
  }
  if (resource === 'components' && segments[2] === 'local' && segments.length >= 4) {
    const componentId = decodeURIComponent(segments[3]);
    if (segments[4] === 'quick-test' && method === 'POST')
      return { command: 'quick_test_component', args: { componentId, request: body } };
    if (segments[4] === 'versions' && method === 'POST' && segments.length === 5)
      return { command: 'create_component_version', args: { componentId, request: body } };
    if (method === 'PUT' && segments.length === 4)
      return { command: 'configure_component', args: { id: componentId, config: body } };
  }
  if (resource === 'rule-component-bindings' && id === 'upsert' && method === 'POST') {
    return { command: 'upsert_rule_component_binding', args: { request: body } };
  }
  if (resource === 'translations') {
    if (tail.length === 1 && tail[0] === 'retry' && method === 'POST')
      return { command: 'retry_translation', args: { id } };
  }
  return null;
}

function splitPathQuery(path: string): { pathname: string; params: URLSearchParams } {
  const q = path.indexOf('?');
  if (q < 0) return { pathname: path, params: new URLSearchParams() };
  return {
    pathname: path.slice(0, q),
    params: new URLSearchParams(path.slice(q + 1)),
  };
}

export async function apiFetch<T>(
  path: string,
  options: { method?: string; body?: unknown } = {}
): Promise<ApiResult<T>> {
  const method = options.method ?? 'GET';
  const body = options.body && typeof options.body === 'object'
    ? (options.body as Record<string, unknown>)
    : {};
  const { pathname, params } = splitPathQuery(path);

  // P1-G: collection-level POST (create) before static map (which maps these
  // paths to list_* commands).
  const create = resolveCollectionCreate(pathname, method, body);
  if (create) {
    return tauriCall<T>(create.command, create.args);
  }
  // P1-G: dynamic `{id}` routing (PUT/DELETE/POST-authorize/test).
  const dynamic = resolveDynamicRoute(pathname, method, {
    ...body,
    status: params.get('status') ?? body.status,
  });
  if (dynamic) {
    return tauriCall<T>(dynamic.command, dynamic.args);
  }

  const command = PATH_TO_COMMAND[pathname];
  if (!command) {
    return {
      success: false,
      error: { code: 'UNKNOWN_PATH', message: `No command mapped for: ${path}` },
    };
  }

  if (pathname === '/api/jobs' && method === 'GET') {
    return tauriCall<T>(command, {
      domain: params.get('domain') ?? undefined,
      limit: params.get('limit') ? Number(params.get('limit')) : undefined,
      offset: params.get('offset') ? Number(params.get('offset')) : undefined,
    });
  }

  if (pathname === '/api/translations' && method === 'GET') {
    return tauriCall<T>(command, {
      page: params.get('page') ? Number(params.get('page')) : undefined,
      limit: params.get('limit') ? Number(params.get('limit')) : undefined,
      domain: params.get('domain') ?? undefined,
      status: params.get('status') ?? undefined,
      search: params.get('search') ?? undefined,
    });
  }

  if (
    (pathname === '/api/translations/batch-retry' ||
      pathname === '/api/translations/batch-delete') &&
    method === 'POST'
  ) {
    return tauriCall<T>(command, { request: body });
  }

  if (method === 'GET' && params.has('vendor_id')) {
    return tauriCall<T>(command, { vendor_id: params.get('vendor_id') });
  }

  const args = REQUEST_WRAPPED_PATHS.has(pathname) ? { request: body } : body;
  return tauriCall<T>(command, args);
}

export function isOk<T>(r: ApiResult<T>): r is { success: true; data: T } {
  return r.success === true;
}
