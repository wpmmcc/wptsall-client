import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import CapabilitiesTab from './CapabilitiesTab.svelte';
import * as componentsApi from '../api/components';

vi.mock('../api/components', () => ({
  getComponentCapabilities: vi.fn(),
}));

describe('components-page/CapabilitiesTab', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders capabilities summary and sorts format map keys', async () => {
    vi.mocked(componentsApi.getComponentCapabilities).mockResolvedValue({
      success: true,
      data: {
        components: [
          {
            id: 'comp-image',
            name: 'Image Translator',
            kind: 'image',
            supported_business_lines: ['post_content'],
            supported_content_formats: ['media_ref'],
            supported_formats: ['media_ref'],
            client_contract: {
              schema_version: 'component-client-contract-v1',
              task_kind: 'image',
              input_mode: 'media_ref',
              workflow_mode: 'sync',
              output_mode: 'translated_image_ref',
              stages: ['request'],
            },
            constraints: { max_file_size_mb: 20 },
          },
          {
            id: 'comp-text',
            name: 'Text Translator',
            kind: 'text',
            supported_business_lines: ['theme_i18n'],
            supported_content_formats: ['plain_text', 'rich_html'],
            supported_formats: ['plain_text'],
            client_contract: {
              schema_version: 'component-client-contract-v1',
              task_kind: 'text',
              input_mode: 'text',
              workflow_mode: 'sync',
              output_mode: 'translated_text',
              stages: ['request'],
            },
            constraints: {},
          },
        ],
        format_component_map: {
          plain_text: ['comp-text'],
          html_fragment: ['comp-text', 'comp-image'],
          audio_ref: ['comp-image'],
        },
      },
    } as never);

    render(CapabilitiesTab);

    expect(await screen.findByText('组件能力矩阵')).toBeTruthy();
    expect(screen.getByText('2')).toBeTruthy();
    expect(screen.getByText('3')).toBeTruthy();
    expect(screen.getByText('Image Translator')).toBeTruthy();
    expect(screen.getByText('Text Translator')).toBeTruthy();
    expect(screen.getByText('20 MB')).toBeTruthy();
    expect(screen.getByText('媒体引用 / 图片文件')).toBeTruthy();
    expect(screen.getByText('纯文本 / HTML 富文本')).toBeTruthy();
    expect(screen.getByText('支持 HTML')).toBeTruthy();
    expect(screen.getByText('标题/摘要/普通 meta 文本')).toBeTruthy();
    expect(screen.getByText('post_content/Classic HTML/Gutenberg 富文本')).toBeTruthy();
    expect(screen.getByText('media_ref -> sync -> translated_image_ref')).toBeTruthy();
    expect(screen.getAllByText('request').length).toBeGreaterThan(0);

    const mappingPanel = screen
      .getByText('格式 → 组件映射')
      .closest('div.rounded-xl') as HTMLElement;
    const formatHeadings = Array.from(
      mappingPanel.querySelectorAll('div.text-xs.font-medium.text-gray-700')
    ).map((node) => node.textContent);
    expect(formatHeadings).toEqual([
      'audio_ref',
      'html_fragment',
      'plain_text',
    ]);
  });

  it('shows error state and can recover on refresh', async () => {
    vi.mocked(componentsApi.getComponentCapabilities)
      .mockRejectedValueOnce(new Error('capability api offline'))
      .mockResolvedValueOnce({
        success: true,
        data: {
          components: [],
          format_component_map: {},
        },
      } as never);

    render(CapabilitiesTab);

    expect(await screen.findByText('capability api offline')).toBeTruthy();

    await fireEvent.click(screen.getByText('刷新'));

    await waitFor(() => {
      expect(screen.getByText('暂无能力数据')).toBeTruthy();
    });
    expect(screen.getByText('暂无映射数据')).toBeTruthy();
    expect(componentsApi.getComponentCapabilities).toHaveBeenCalledTimes(2);
  });
});
