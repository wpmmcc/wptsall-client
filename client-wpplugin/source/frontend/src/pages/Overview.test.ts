import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, cleanup } from '@testing-library/svelte';
import Overview from './Overview.svelte';
import * as statusModule from '../lib/stores/status';
import * as toastModule from '../lib/stores/toast';

vi.mock('../lib/stores/status', async () => {
  const { writable } = await import('svelte/store');
  return {
    status: writable<any>(null),
    fetchStatus: vi.fn().mockResolvedValue(undefined),
  };
});

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

function createFetchMock() {
  return vi.fn(async (url: string, _init?: RequestInit) => {
    if (url === '/api/stats/overview') {
      const payload = {
        success: true,
        data: { by_domain: [], by_status: [], daily: [] },
      };
      return {
        text: async () => JSON.stringify(payload),
      };
    }
    if (url === '/api/worker/start-check') {
      const payload = {
        success: true,
        data: {
          can_start: true,
          requires_confirmation: false,
          summary: {
            domains_checked: 0,
            relations_checked: 0,
            rules_checked: 0,
            fields_checked: 0,
            language_pack_lanes_checked: 0,
            blocking_missing_components: 0,
            confirm_missing_components: 0,
            auto_skip_missing_components: 0,
          },
          missing_components: [],
        },
      };
      return {
        text: async () => JSON.stringify(payload),
      };
    }
    const payload = { success: true };
    return {
      text: async () => JSON.stringify(payload),
    };
  });
}

describe('pages/Overview', () => {
  beforeEach(() => {
    vi.mocked(statusModule.fetchStatus).mockClear();
    vi.mocked(toastModule.showToast).mockClear();
    (statusModule.status as any).set({
      runtime_mode: 'local',
      worker_loop_running: false,
      worker_loop_poll_seconds: 20,
      worker_recent_runs: [],
      domains: [],
    });
  });

  afterEach(() => {
    statusModule.status.set(null);
    vi.unstubAllGlobals();
    cleanup();
  });

  it('runs worker once and refreshes status on success', async () => {
    const fetchMock = createFetchMock();
    vi.stubGlobal('fetch', fetchMock);

    render(Overview);
    await fireEvent.click(screen.getByText('运行一次'));

    await waitFor(() => {
      expect(fetchMock.mock.calls.some(([url]) => url === '/api/worker/run-once')).toBe(true);
    });

    const runOnceCall = fetchMock.mock.calls.find(([url]) => url === '/api/worker/run-once') as
      | [string, RequestInit | undefined]
      | undefined;
    expect(runOnceCall).toBeTruthy();
    expect(runOnceCall?.[1]?.method).toBe('POST');

    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Worker 执行完成');
    expect(statusModule.fetchStatus).toHaveBeenCalledTimes(1);
  });

  it('saves poll interval and clamps low/high bounds', async () => {
    const fetchMock = createFetchMock();
    vi.stubGlobal('fetch', fetchMock);

    (statusModule.status as any).set({
      worker_loop_running: false,
      worker_loop_poll_seconds: -5,
      worker_recent_runs: [],
      domains: [],
    });

    const { unmount } = render(Overview);
    await fireEvent.click(screen.getByText('保存'));

    unmount();
    (statusModule.status as any).set({
      worker_loop_running: false,
      worker_loop_poll_seconds: 99999,
      worker_recent_runs: [],
      domains: [],
    });
    render(Overview);
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      const count = fetchMock.mock.calls.filter(([url]) => url === '/api/worker/config').length;
      expect(count).toBe(2);
    });

    const configCalls = fetchMock.mock.calls.filter(([url]) => url === '/api/worker/config') as Array<[string, RequestInit | undefined]>;
    expect(JSON.parse(String(configCalls[0][1]?.body))).toEqual({ poll_seconds: 1 });
    expect(JSON.parse(String(configCalls[1][1]?.body))).toEqual({ poll_seconds: 3600 });

    expect(toastModule.showToast).toHaveBeenCalledWith('success', '轮询间隔已设为 1s');
    expect(toastModule.showToast).toHaveBeenCalledWith('success', '轮询间隔已设为 3600s');
  });

  it('toggles start/stop button by worker_loop_running', () => {
    vi.stubGlobal('fetch', createFetchMock());

    const { unmount } = render(Overview);
    expect(screen.getByText('启动循环')).toBeTruthy();
    unmount();

    (statusModule.status as any).set({
      worker_loop_running: true,
      worker_loop_poll_seconds: 20,
      worker_recent_runs: [],
      domains: [],
    });
    render(Overview);
    expect(screen.getByText('停止循环')).toBeTruthy();
  });

  it('checks missing component coverage before starting worker loop', async () => {
    const fetchMock = vi.fn(async (url: string, _init?: RequestInit) => {
      if (url === '/api/stats/overview') {
        return {
          text: async () =>
            JSON.stringify({ success: true, data: { by_domain: [], by_status: [], daily: [] } }),
        };
      }
      if (url === '/api/worker/start-check') {
        return {
          text: async () =>
            JSON.stringify({
              success: true,
              data: {
                can_start: true,
                requires_confirmation: true,
                summary: {
                  domains_checked: 1,
                  relations_checked: 1,
                  rules_checked: 1,
                  fields_checked: 1,
                  language_pack_lanes_checked: 0,
                  blocking_missing_components: 0,
                  confirm_missing_components: 1,
                  auto_skip_missing_components: 0,
                },
                missing_components: [
                  {
                    api_base_url: 'https://blog.wpmm.cc',
                    business_line: 'post_content',
                    relation_id: 289,
                    rule_id: 1019,
                    source_group: 'content_object',
                    routing_profile: 'post_content_default',
                    delivery_target: 'object_writeback',
                    object_name: 'attachment',
                    field_name: '_wptsall_core_source_file_id',
                    source_role: 'media_file',
                    preflight_policy: 'warn',
                    missing_component_behavior: 'confirm_continue',
                    severity: 'confirm',
                    content_format: 'media_ref',
                    required_slot_key: 'media_ref:document',
                    suggested_task_type: 'document',
                  },
                ],
              },
            }),
        };
      }
      return {
        text: async () => JSON.stringify({ success: true }),
      };
    });
    vi.stubGlobal('fetch', fetchMock);

    render(Overview);
    await fireEvent.click(screen.getByText('启动循环'));

    await waitFor(() => {
      expect(fetchMock.mock.calls.some(([url]) => url === '/api/worker/start-check')).toBe(true);
      expect(screen.getByText('发现缺失组件')).toBeTruthy();
      expect(screen.getByText(/缺少对应组件/)).toBeTruthy();
      expect(screen.getByText('文章内容')).toBeTruthy();
      expect(screen.getByText('内容对象')).toBeTruthy();
      expect(screen.getByText(/对象写回/)).toBeTruthy();
      expect(screen.getByText(/媒体文件/)).toBeTruthy();
      expect(screen.getByText(/文档媒体/)).toBeTruthy();
    });

    await fireEvent.click(screen.getByText('继续执行'));

    await waitFor(() => {
      expect(fetchMock.mock.calls.some(([url]) => url === '/api/worker/start')).toBe(true);
    });

    const startCall = fetchMock.mock.calls.find(([url]) => url === '/api/worker/start') as
      | [string, RequestInit | undefined]
      | undefined;
    expect(JSON.parse(String(startCall?.[1]?.body))).toEqual({ force: true });
  });

  it('does not offer continue button when missing components are blocking by policy', async () => {
    const fetchMock = vi.fn(async (url: string) => {
      if (url === '/api/stats/overview') {
        return {
          text: async () =>
            JSON.stringify({ success: true, data: { by_domain: [], by_status: [], daily: [] } }),
        };
      }
      if (url === '/api/worker/start-check') {
        return {
          text: async () =>
            JSON.stringify({
              success: true,
              data: {
                can_start: false,
                requires_confirmation: false,
                summary: {
                  domains_checked: 1,
                  relations_checked: 1,
                  rules_checked: 1,
                  fields_checked: 1,
                  language_pack_lanes_checked: 0,
                  blocking_missing_components: 1,
                  confirm_missing_components: 0,
                  auto_skip_missing_components: 0,
                },
                missing_components: [
                  {
                    api_base_url: 'https://blog.wpmm.cc',
                    business_line: 'post_content',
                    relation_id: 289,
                    rule_id: 1019,
                    source_group: 'content_object',
                    routing_profile: 'post_content_default',
                    delivery_target: 'object_writeback',
                    object_name: 'attachment',
                    field_name: '_wptsall_core_source_file_id',
                    source_role: 'media_file',
                    preflight_policy: 'block',
                    missing_component_behavior: 'stop_task',
                    severity: 'blocking',
                    content_format: 'media_ref',
                    required_slot_key: 'media_ref:document',
                    suggested_task_type: 'document',
                  },
                ],
              },
            }),
        };
      }
      return {
        text: async () => JSON.stringify({ success: true }),
      };
    });
    vi.stubGlobal('fetch', fetchMock);

    render(Overview);
    await fireEvent.click(screen.getByText('启动循环'));

    await waitFor(() => {
      expect(screen.getByText(/必须阻断/)).toBeTruthy();
    });

    expect(screen.queryByText('继续执行')).toBeNull();
  });

  it('shows clearer guidance when worker run is denied by wp token revoke', async () => {
    const fetchMock = vi.fn(async (url: string) => {
      if (url === '/api/stats/overview') {
        return {
          status: 200,
          text: async () => JSON.stringify({ success: true, data: { by_domain: [], by_status: [], daily: [] } }),
        };
      }
      if (url === '/api/worker/run-once') {
        return {
          status: 401,
          text: async () =>
            JSON.stringify({
              success: false,
              error: { code: 'client_unauthorized', message: 'Client authentication failed' },
            }),
        };
      }
      return {
        status: 200,
        text: async () => JSON.stringify({ success: true }),
      };
    });
    vi.stubGlobal('fetch', fetchMock);

    render(Overview);
    await fireEvent.click(screen.getByText('运行一次'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        'Worker 无法继续访问 WP',
        expect.stringContaining('重新生成 Token'),
      );
    });
  });

  it('hides legacy refresh controls locally and shows them in legacy mode', async () => {
    const fetchMock = createFetchMock();
    vi.stubGlobal('fetch', fetchMock);

    // Local default: /api/domains/refresh and /api/components/refresh are
    // server-control-plane routes; their buttons must not render (P0-LF-04).
    render(Overview);
    await waitFor(() => {
      expect(screen.getByText('运行一次')).toBeTruthy();
    });
    expect(screen.queryByText('刷新域名')).toBeNull();
    expect(screen.queryByText('刷新组件')).toBeNull();

    // Legacy server control plane: the refresh affordances return.
    (statusModule.status as any).set({
      runtime_mode: 'legacy_server_control_plane',
      worker_loop_running: false,
      worker_loop_poll_seconds: 20,
      worker_recent_runs: [],
      domains: [],
    });
    await waitFor(() => {
      expect(screen.getByText('刷新域名')).toBeTruthy();
    });
    expect(screen.getByText('刷新组件')).toBeTruthy();
  });
});
