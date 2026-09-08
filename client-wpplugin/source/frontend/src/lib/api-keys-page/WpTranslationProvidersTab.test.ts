// catalog: WEBUI-UI-WpTranslationProvidersTab
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功 / 空态 / 失败态（后端 error 回显 + 恢复）。
// 说明：本组件为只读表格，无表单，故无校验/提交流转。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import WpTranslationProvidersTab from './WpTranslationProvidersTab.svelte';
import * as keysApi from '../api/keys';

vi.mock('../api/keys', () => ({
  listWpTranslationProviders: vi.fn(),
}));

const provider = {
  id: 'wp-openai',
  name: 'OpenAI for WP',
  description: 'openai provider',
  vendor_id: 'openai',
  provider_kind: 'rest',
  api_base_url: 'https://api.example.com',
  http_method: 'POST',
  request_path: '/v1/chat/completions',
  response_path: 'choices.0.message.content',
  auth_mode: 'api_key',
  max_input_chars: 9000,
  supported_languages: ['en_US', 'zh_CN'],
  active: true,
};

describe('api-keys-page/WpTranslationProvidersTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('loads WP translation provider rows on mount', async () => {
    vi.mocked(keysApi.listWpTranslationProviders).mockResolvedValue({
      success: true,
      data: { items: [provider] },
    } as never);

    render(WpTranslationProvidersTab);

    expect(await screen.findByText('OpenAI for WP')).toBeTruthy();
    expect(screen.getByText('wp-openai')).toBeTruthy();
    expect(screen.getByText('rest')).toBeTruthy();
    expect(screen.getByText('api_key')).toBeTruthy();
    expect(screen.getByText('是')).toBeTruthy();
    expect(keysApi.listWpTranslationProviders).toHaveBeenCalledTimes(1);
  });

  it('shows empty state when no providers exist', async () => {
    vi.mocked(keysApi.listWpTranslationProviders).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);

    render(WpTranslationProvidersTab);

    // i18n key wp_providers_catalog.no_data 缺失于 locale，$_ 回退渲染 key 本身
    expect(await screen.findByText(/wp_providers_catalog\.no_data/)).toBeTruthy();
  });

  it('shows backend error message and recovers on refresh', async () => {
    vi.mocked(keysApi.listWpTranslationProviders)
      .mockResolvedValueOnce({
        success: false,
        error: { code: 'INTERNAL', message: 'server 5xx: providers unavailable', status: 500 },
      } as never)
      .mockResolvedValueOnce({
        success: true,
        data: { items: [provider] },
      } as never);

    render(WpTranslationProvidersTab);

    expect(await screen.findByText('server 5xx: providers unavailable')).toBeTruthy();

    await fireEvent.click(screen.getByText('刷新'));

    await waitFor(() => {
      expect(screen.getByText('OpenAI for WP')).toBeTruthy();
    });
    expect(keysApi.listWpTranslationProviders).toHaveBeenCalledTimes(2);
  });

  it('shows thrown network error and recovers on refresh', async () => {
    vi.mocked(keysApi.listWpTranslationProviders)
      .mockRejectedValueOnce(new Error('connection refused'))
      .mockResolvedValueOnce({
        success: true,
        data: { items: [] },
      } as never);

    render(WpTranslationProvidersTab);

    expect(await screen.findByText('connection refused')).toBeTruthy();

    await fireEvent.click(screen.getByText('刷新'));

    await waitFor(() => {
      expect(screen.getByText(/wp_providers_catalog\.no_data/)).toBeTruthy();
    });
  });
});
