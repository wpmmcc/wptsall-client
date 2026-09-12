// catalog: WEBUI-UI-TranslationReview
// oracle: L2
// 状态矩阵：翻译 payload 字段执行审计详情渲染（装载 → 数据态）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import TranslationReview from './TranslationReview.svelte';
import * as itemsApi from '../lib/api/items';
import * as componentsApi from '../lib/api/components';
import * as toastModule from '../lib/stores/toast';

vi.mock('../lib/api/items', () => ({
  getItemContent: vi.fn(),
  saveTranslated: vi.fn(),
  resubmitItem: vi.fn(),
  retranslateItem: vi.fn(),
  approveItem: vi.fn(),
  saveItemOverride: vi.fn(),
}));

vi.mock('../lib/api/components', () => ({
  listAllLocalComponents: vi.fn(),
  getLocalComponent: vi.fn(),
}));

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('pages/TranslationReview', () => {
  beforeEach(() => {
    vi.mocked(toastModule.showToast).mockClear();
    vi.mocked(componentsApi.listAllLocalComponents).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);
    vi.mocked(componentsApi.getLocalComponent).mockResolvedValue({
      success: true,
      data: {
        template_json: {},
      },
    } as never);
    vi.mocked(itemsApi.getItemContent).mockResolvedValue({
      success: true,
      data: {
        item: {
          id: 8,
          job_id: 1,
          domain: 'https://blog.wpmm.cc',
          relation_id: 77,
          business_line: 'post_content',
          object_type: 'post',
          wp_object_id: 501,
          wp_object_subtype: 'post',
          task_type: 'mixed',
          source_lang: 'en_US',
          target_lang: 'zh_CN',
          component_id: 'comp-text',
          component_ids: ['comp-text', 'comp-image'],
          selected_component_id: 'comp-text',
          raw_path: '/tmp/raw.json',
          translated_path: '/tmp/translated.json',
          status: 'pending_review',
          client_task_id: 'ctask-8',
          upload_id: null,
          wp_attachment_id: null,
          error_message: null,
          retry_count: 0,
          max_retries: 3,
          fetched_at: null,
          translated_at: 100,
          synced_at: null,
          created_at: 1,
          updated_at: 1,
        },
        raw: {
          post_title: 'Hello',
        },
        translated: {
          payload: {
            translated_fields: {
              post_title: '你好',
            },
            field_results: [
              {
                field: 'post_title',
                status: 'success',
                content_format: 'plain_text',
                storage: 'post_column',
                detail: 'translated',
                provider_component: 'comp-text',
                merge_target: 'translated_fields',
                transform_stage: 'direct',
                fallback_reason: '',
              },
              {
                field: 'hero_image_alt',
                status: 'failed',
                content_format: 'media_ref',
                storage: 'post_meta',
                detail: 'unsupported_content_format',
                provider_component: 'comp-text',
                merge_target: 'translated_meta',
                transform_stage: 'media_text_to_plain_text',
                fallback_reason: 'unsupported_content_format',
              },
            ],
            media_mappings: [
              {
                source_id: 123,
                translated_ref: 'https://example.com/image-123-zh.jpg',
              },
            ],
          },
        },
      },
    } as never);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('renders field execution audit details from translated payload', async () => {
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });

    await waitFor(() => {
      expect(screen.getByText('字段执行审计')).toBeTruthy();
    });

    expect(screen.getByText('字段 2')).toBeTruthy();
    expect(screen.getByText('媒体映射 1')).toBeTruthy();
    expect(screen.getAllByText('post_title').length).toBeGreaterThan(0);
    expect(screen.getByText('hero_image_alt')).toBeTruthy();
    expect(screen.getAllByText('comp-text').length).toBeGreaterThan(0);
    expect(screen.getByText('media_text_to_plain_text')).toBeTruthy();
    expect(screen.getByText('translated_fields')).toBeTruthy();
    expect(screen.getByText('translated_meta')).toBeTruthy();
    expect(screen.getByText('reason=unsupported_content_format')).toBeTruthy();
  });
});
