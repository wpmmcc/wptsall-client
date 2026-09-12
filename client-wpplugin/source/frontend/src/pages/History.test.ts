// catalog: WEBUI-UI-History
// oracle: L2
// 状态矩阵：列表级执行摘要 + 展开详情。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import History from './History.svelte';
import * as translationsApi from '../lib/api/translations';

vi.mock('../lib/api/translations', () => ({
  getTranslations: vi.fn(),
  retryTranslation: vi.fn(),
  batchRetryTranslations: vi.fn(),
  batchDeleteTranslations: vi.fn(),
}));

describe('pages/History', () => {
  beforeEach(() => {
    vi.mocked(translationsApi.getTranslations).mockResolvedValue({
      success: true,
      data: {
        records: [
          {
            id: 7,
            created_at: 1,
            domain: 'https://blog.wpmm.cc',
            relation_id: 12,
            object_id: 88,
            object_type: 'post',
            business_line: 'post_content',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            status: 'failed',
            execution_ms: 1350,
            worker_id: 'worker-1',
            idempotency_key: 'idem-7',
            callback_sent_at: undefined,
            callback_retries: 1,
            fields_count: 3,
            error_message: 'callback failed',
            component_ids: ['comp-text', 'comp-image'],
            media_mappings_count: 1,
            failed_fields_count: 2,
            primary_failure_reason: 'unsupported_content_format',
          },
        ],
        total: 1,
        page: 1,
        limit: 20,
      },
    } as never);
    vi.mocked(translationsApi.retryTranslation).mockResolvedValue({
      success: true,
      data: { queued: true },
    } as never);
    vi.mocked(translationsApi.batchRetryTranslations).mockResolvedValue({
      success: true,
      data: { queued: 0, requested: 0 },
    } as never);
    vi.mocked(translationsApi.batchDeleteTranslations).mockResolvedValue({
      success: true,
      data: { deleted: 0, requested: 0 },
    } as never);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('renders execution summary at list level and expanded detail', async () => {
    render(History);

    await waitFor(() => {
      expect(screen.getByText('blog.wpmm.cc')).toBeTruthy();
    });

    expect(screen.getByText('comp-text -> comp-image')).toBeTruthy();
    expect(screen.getByText('媒体 1')).toBeTruthy();
    expect(screen.getByText('失败字段 2')).toBeTruthy();
    expect(screen.getByText('unsupported_content_format')).toBeTruthy();

    await fireEvent.click(screen.getByText('blog.wpmm.cc'));

    await waitFor(() => {
      expect(screen.getByText(/媒体 1,\s*失败字段 2/)).toBeTruthy();
    });

    expect(screen.getByText('主失败原因')).toBeTruthy();
    expect(screen.getByText('执行组件')).toBeTruthy();
  });
});
