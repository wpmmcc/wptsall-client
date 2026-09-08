import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, cleanup } from '@testing-library/svelte';
import Sites from './Sites.svelte';
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

describe('pages/Sites', () => {
  beforeEach(() => {
    vi.mocked(statusModule.fetchStatus).mockClear();
    vi.mocked(toastModule.showToast).mockClear();
    (statusModule.status as any).set({
      domain_token_bindings: [],
      domains: [],
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    cleanup();
  });

  it('validates required fields before save', async () => {
    vi.stubGlobal('fetch', vi.fn());
    render(Sites);

    await fireEvent.click(screen.getByText('+ 添加站点'));
    await fireEvent.click(screen.getByText('保存'));

    expect(toastModule.showToast).toHaveBeenCalledWith('error', '请填写域名和 Token');
  });

  it('saves token then refreshes status', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      text: async () => JSON.stringify({ success: true }),
    });
    vi.stubGlobal('fetch', fetchMock);

    render(Sites);
    await fireEvent.click(screen.getByText('+ 添加站点'));

    await fireEvent.input(screen.getByPlaceholderText('https://example.com'), {
      target: { value: 'https://blog.wpmm.cc' },
    });
    await fireEvent.input(screen.getByPlaceholderText('wptc1.xxx...（必填）'), {
      target: { value: 'wptc1.demo.token' },
    });
    await fireEvent.input(screen.getByPlaceholderText('从 WP Plugin 管理面板获取'), {
      target: { value: 'route-secret-demo' },
    });

    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        '/api/domain-tokens/upsert',
        expect.objectContaining({ method: 'POST' }),
      );
    });

    const upsertCall = fetchMock.mock.calls.find(([url]) => url === '/api/domain-tokens/upsert');
    expect(upsertCall).toBeTruthy();
    const payload = JSON.parse(String((upsertCall?.[1] as RequestInit).body));
    expect(payload).toEqual({
      api_base_url: 'https://blog.wpmm.cc',
      wp_client_token: 'wptc1.demo.token',
      route_secret: 'route-secret-demo',
    });

    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Token 已保存');
    expect(statusModule.fetchStatus).toHaveBeenCalledTimes(1);
  });

  it('tests and deletes existing token rows', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({ text: async () => JSON.stringify({ success: true }) })
      .mockResolvedValueOnce({ text: async () => JSON.stringify({ success: true }) });
    vi.stubGlobal('fetch', fetchMock);

    (statusModule.status as any).set({
      domain_token_bindings: [
        {
          api_base_url: 'https://blog.wpmm.cc',
          token_prefix: 'wptc1.xxx',
          token_len: 60,
          route_secret: 'secret-demo',
        },
      ],
      domains: [],
    });

    render(Sites);

    await fireEvent.click(screen.getByText('测试'));
    await fireEvent.click(screen.getByText('删除'));

    const testCall = fetchMock.mock.calls.find(([url]) => url === '/api/domain-tokens/test');
    const deleteCall = fetchMock.mock.calls.find(([url]) => url === '/api/domain-tokens/delete');

    expect(testCall).toBeTruthy();
    expect(deleteCall).toBeTruthy();
    expect(JSON.parse(String((testCall?.[1] as RequestInit).body))).toEqual({
      api_base_url: 'https://blog.wpmm.cc',
    });
    expect(JSON.parse(String((deleteCall?.[1] as RequestInit).body))).toEqual({
      api_base_url: 'https://blog.wpmm.cc',
    });
  });

  it('edits existing token rows without requiring token re-entry', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      text: async () => JSON.stringify({ success: true }),
    });
    vi.stubGlobal('fetch', fetchMock);

    (statusModule.status as any).set({
      domain_token_bindings: [
        {
          api_base_url: 'https://blog.wpmm.cc',
          token_prefix: 'wptc1.xxx',
          token_len: 60,
          route_secret: 'secret-demo',
        },
      ],
      domains: [],
    });

    render(Sites);

    await fireEvent.click(screen.getByText('编辑'));
    await fireEvent.input(screen.getByPlaceholderText('从 WP Plugin 管理面板获取'), {
      target: { value: 'route-secret-updated' },
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        '/api/domain-tokens/upsert',
        expect.objectContaining({ method: 'POST' }),
      );
    });

    const upsertCall = fetchMock.mock.calls.find(([url]) => url === '/api/domain-tokens/upsert');
    expect(JSON.parse(String((upsertCall?.[1] as RequestInit).body))).toEqual({
      api_base_url: 'https://blog.wpmm.cc',
      existing_api_base_url: 'https://blog.wpmm.cc',
      route_secret: 'route-secret-updated',
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Token 已保存');
  });

  it('shows clearer guidance when wp rejects test due to license gate', async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      status: 403,
      text: async () =>
        JSON.stringify({
          success: false,
          error: { code: 'wptsall_pro_required', message: 'Client API disabled' },
        }),
    });
    vi.stubGlobal('fetch', fetchMock);

    (statusModule.status as any).set({
      domain_token_bindings: [
        {
          api_base_url: 'https://blog.wpmm.cc',
          token_prefix: 'wptc1.xxx',
          token_len: 60,
          route_secret: 'secret-demo',
        },
      ],
      domains: [],
    });

    render(Sites);
    await fireEvent.click(screen.getByText('测试'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '站点已拒绝当前连通测试',
        expect.stringContaining('官网确认授权状态'),
      );
    });
  });
});
