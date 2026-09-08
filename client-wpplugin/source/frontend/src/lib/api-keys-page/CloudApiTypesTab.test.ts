// catalog: WEBUI-UI-CloudApiTypesTab
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功 / 空态 / 失败态（后端 error 回显 + 恢复）。
// 说明：本组件为只读表格，无表单，故无校验/提交流转。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import CloudApiTypesTab from './CloudApiTypesTab.svelte';
import * as keysApi from '../api/keys';

vi.mock('../api/keys', () => ({
  listCloudApiTypes: vi.fn(),
}));

const item = {
  id: 'openai-text-v1',
  name: 'OpenAI Text',
  description: 'OpenAI text translation',
  category: 'text',
  http_method: 'POST',
  endpoint_pattern: '/v1/chat/completions',
  auth_modes: ['key'],
  supported_content_formats: ['plain_text', 'html'],
  output_artifact_kinds: ['text'],
  max_input_chars: 12000,
  visibility: 'public',
  active: true,
};

describe('api-keys-page/CloudApiTypesTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('loads cloud API type rows on mount', async () => {
    vi.mocked(keysApi.listCloudApiTypes).mockResolvedValue({
      success: true,
      data: { items: [item] },
    } as never);

    render(CloudApiTypesTab);

    expect(await screen.findByText('OpenAI Text')).toBeTruthy();
    expect(screen.getByText('openai-text-v1')).toBeTruthy();
    expect(screen.getByText('plain_text, html')).toBeTruthy();
    // active = 是（zh-CN common.yes）
    expect(screen.getByText('是')).toBeTruthy();
    expect(screen.getByText('刷新')).toBeTruthy();
    expect(keysApi.listCloudApiTypes).toHaveBeenCalledTimes(1);
  });

  it('shows empty state when no cloud API types exist', async () => {
    vi.mocked(keysApi.listCloudApiTypes).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);

    render(CloudApiTypesTab);

    // i18n key cloud_api_types_catalog.no_data 缺失于 locale，$_ 回退渲染 key 本身
    expect(await screen.findByText(/cloud_api_types_catalog\.no_data/)).toBeTruthy();
  });

  it('shows backend error message and recovers on refresh', async () => {
    vi.mocked(keysApi.listCloudApiTypes)
      .mockResolvedValueOnce({
        success: false,
        error: { code: 'INTERNAL', message: 'server 5xx: catalog broken', status: 500 },
      } as never)
      .mockResolvedValueOnce({
        success: true,
        data: { items: [item] },
      } as never);

    render(CloudApiTypesTab);

    expect(await screen.findByText('server 5xx: catalog broken')).toBeTruthy();

    await fireEvent.click(screen.getByText('刷新'));

    await waitFor(() => {
      expect(screen.getByText('OpenAI Text')).toBeTruthy();
    });
    expect(keysApi.listCloudApiTypes).toHaveBeenCalledTimes(2);
  });

  it('shows thrown network error and recovers on refresh', async () => {
    vi.mocked(keysApi.listCloudApiTypes)
      .mockRejectedValueOnce(new Error('network unreachable'))
      .mockResolvedValueOnce({
        success: true,
        data: { items: [] },
      } as never);

    render(CloudApiTypesTab);

    expect(await screen.findByText('network unreachable')).toBeTruthy();

    await fireEvent.click(screen.getByText('刷新'));

    await waitFor(() => {
      expect(screen.getByText(/cloud_api_types_catalog\.no_data/)).toBeTruthy();
    });
  });
});
