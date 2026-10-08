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

  // 批 N2 / U-6: the runtime language switch — the lang-switcher toggle must
  // flip the live svelte-i18n locale, the document lang, the versioned
  // storage key, AND the aria-pressed state of both toggle buttons in one
  // click (screen-reader state stays truthful mid-switch).
  it('runtime language switch flips locale, document lang, storage key and aria-pressed', async () => {
    const { locale } = await import('svelte-i18n');
    const { get } = await import('svelte/store');

    render(Sidebar, {
      currentPage: 'overview',
      onNavigate: () => {},
      onLogout: () => {},
    });

    const before = String(get(locale));
    const target = before === 'zh-CN' ? 'en' : 'zh-CN';
    const sourceLabel = before === 'zh-CN' ? '站点' : 'Sites';
    const targetLabel = before === 'zh-CN' ? 'Sites' : '站点';

    expect(screen.getByText(sourceLabel)).toBeTruthy();
    const switcher = document.querySelector('[data-testid="lang-switcher"]');
    expect(switcher).toBeTruthy();
    const targetBtn = switcher!.querySelector(`[data-locale="${target}"]`) as HTMLButtonElement;
    const sourceBtn = switcher!.querySelector(`[data-locale="${before}"]`) as HTMLButtonElement;
    expect(sourceBtn.getAttribute('aria-pressed')).toBe('true');
    expect(targetBtn.getAttribute('aria-pressed')).toBe('false');

    await fireEvent.click(targetBtn);

    expect(get(locale)).toBe(target);
    expect(document.documentElement.lang).toBe(target);
    expect(localStorage.getItem('wptsall_locale.v1')).toBe(target);
    // The nav labels re-render in the switched language immediately.
    expect(screen.getByText(targetLabel)).toBeTruthy();
    expect(screen.queryByText(sourceLabel)).toBeNull();
    // The pressed state moved with the locale.
    expect(targetBtn.getAttribute('aria-pressed')).toBe('true');
    expect(sourceBtn.getAttribute('aria-pressed')).toBe('false');
  });
});
