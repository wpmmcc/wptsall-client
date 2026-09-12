// catalog: WEBUI-UI-Sidebar
// oracle: L2
// 状态矩阵：当前页高亮 / local 模式无登出·会话前缀·legacy 入口·权益拉取 / 导航回调 / legacy 模式恢复登出与会话前缀。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import Sidebar from './Sidebar.svelte';
import { status } from '../stores/status';
import type { WebUiStatus } from '../api/types';

// P0-LF-04: the default fixture is the local runtime projection — no session,
// no server control plane. Legacy affordances must be opt-in via runtime_mode.
function makeStatus(overrides: Partial<WebUiStatus> = {}): Partial<WebUiStatus> {
  return {
    runtime_mode: 'local',
    logged_in: false,
    session_token_prefix: null,
    domains: [],
    components: [],
    worker_loop_running: false,
    worker_loop_poll_seconds: 10,
    worker_last_summary: {},
    worker_recent_runs: [],
    ...overrides,
  };
}

describe('components/Sidebar', () => {
  beforeEach(() => {
    status.set(makeStatus() as never);
  });

  afterEach(() => {
    status.set(null);
    cleanup();
  });

  it('highlights current page button', () => {
    render(Sidebar, {
      currentPage: 'sites',
      onNavigate: () => {},
      onLogout: () => {},
    });

    const sitesBtn = screen.getByText('站点').closest('button');
    // Light-theme highlight classes (matches Sidebar.svelte v round 3
    // redesign: the legacy `bg-white/10 text-white` was a dark-theme
    // artifact no longer in the rendered output).
    expect(sitesBtn?.className).toContain('bg-indigo-50');
    expect(sitesBtn?.className).toContain('text-indigo-700');
    // The button also gets aria-current=page when active.
    expect(sitesBtn?.getAttribute('aria-current')).toBe('page');
  });

  it('local mode: no logout, session prefix, legacy entry, or entitlement fetch', async () => {
    const fetchMock = vi.fn(async () => ({
      text: async () => JSON.stringify({ success: true, data: [] }),
    }));
    vi.stubGlobal('fetch', fetchMock);

    render(Sidebar, {
      currentPage: 'overview',
      onNavigate: () => {},
      onLogout: () => {},
    });

    // Local-boundary assertions (P0-LF-04): the default sidebar neither
    // offers login/session affordances nor touches the server control plane.
    expect(screen.queryByText('退出登录')).toBeNull();
    expect(screen.queryByText('sess_abcd1234')).toBeNull();
    expect(screen.queryByText('旧版服务器控制面')).toBeNull();

    // Flush pending microtasks so a stray onMount fetch would have fired.
    await Promise.resolve();
    await Promise.resolve();
    expect(fetchMock).not.toHaveBeenCalled();

    vi.unstubAllGlobals();
  });

  it('triggers navigation callback', async () => {
    const onNavigate = vi.fn();

    render(Sidebar, {
      currentPage: 'overview',
      onNavigate,
      onLogout: () => {},
    });

    await fireEvent.click(screen.getByText('任务'));
    expect(onNavigate).toHaveBeenCalledWith('tasks');
  });

  it('legacy mode: logout, session prefix and legacy entry render', async () => {
    status.set(makeStatus({
      runtime_mode: 'legacy_server_control_plane',
      logged_in: true,
      session_token_prefix: 'sess_abcd1234',
      server_base: 'https://www.wpmm.cc',
    }) as never);

    const onNavigate = vi.fn();
    const onLogout = vi.fn();

    render(Sidebar, {
      currentPage: 'overview',
      onNavigate,
      onLogout,
    });

    expect(screen.getByText('退出登录')).toBeTruthy();
    expect(screen.getByText('sess_abcd1234')).toBeTruthy();
    expect(screen.getByText('旧版服务器控制面')).toBeTruthy();

    await fireEvent.click(screen.getByText('退出登录'));
    expect(onLogout).toHaveBeenCalledTimes(1);
  });
});
