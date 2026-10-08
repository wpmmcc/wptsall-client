// catalog: WEBUI-UI-TranslationReview
// oracle: L2
// 状态矩阵：翻译 payload 字段执行审计详情渲染（装载 → 数据态）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
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
  rejectItem: vi.fn(),
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
    localStorage.clear();
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
    vi.restoreAllMocks();
    localStorage.clear();
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

  it('does not save or start a fee request when confirmation is declined', async () => {
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const [button] = await screen.findAllByRole('button', { name: '重新翻译' });
    await fireEvent.click(button);
    expect(confirm).toHaveBeenCalledOnce();
    expect(itemsApi.saveItemOverride).not.toHaveBeenCalled();
    expect(itemsApi.retranslateItem).not.toHaveBeenCalled();
  });

  it('resumes the server request without a new fee confirmation and reloads the saved result', async () => {
    const request = 'c1b05b00-4af3-4a5f-9d88-13b6dc674f11';
    const original = await vi.mocked(itemsApi.getItemContent)(8);
    const content = { ...(original as any).data, manual_request_id: request };
    vi.mocked(itemsApi.getItemContent).mockResolvedValue({ success: true, data: content } as never);
    vi.mocked(itemsApi.saveItemOverride).mockResolvedValue({ success: true, data: content.item } as never);
    vi.mocked(itemsApi.retranslateItem).mockResolvedValue({ success: true, data: { item_id: 8, status: 'pending_review', request_id: request } });
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const [button] = await screen.findAllByRole('button', { name: '重新翻译' });
    await fireEvent.click(button);
    await waitFor(() => expect(itemsApi.retranslateItem).toHaveBeenCalledWith(8, request));
    await waitFor(() => expect(itemsApi.getItemContent).toHaveBeenCalledTimes(3));
    expect(confirm).not.toHaveBeenCalled();
    expect(itemsApi.saveItemOverride).not.toHaveBeenCalled();
    expect(screen.getByText('字段执行审计')).toBeTruthy();
  });

  it('retains a lost reply identity across a page reload and resumes without saving new overrides', async () => {
    const request = '631b8b74-913a-4972-8892-33a892ce07bb';
    localStorage.setItem('wptsall:manual-request:8', request);
    vi.mocked(itemsApi.retranslateItem).mockResolvedValue({ success: false, error: { code: 'NETWORK_ERROR', message: 'owned lost reply' } });
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const first = render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    await fireEvent.click((await screen.findAllByRole('button', { name: '重新翻译' }))[0]);
    await waitFor(() => expect(itemsApi.retranslateItem).toHaveBeenCalledWith(8, request));
    first.unmount();
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    await fireEvent.click((await screen.findAllByRole('button', { name: '重新翻译' }))[0]);
    await waitFor(() => expect(itemsApi.retranslateItem).toHaveBeenCalledTimes(2));
    expect(confirm).not.toHaveBeenCalled();
    expect(itemsApi.saveItemOverride).not.toHaveBeenCalled();
    expect(localStorage.getItem('wptsall:manual-request:8')).toBe(request);
  });

  it('only forgets a browser-only request after confirmation and an independent server read', async () => {
    const request = '631b8b74-913a-4972-8892-33a892ce07bb';
    localStorage.setItem('wptsall:manual-request:8', request);
    vi.mocked(itemsApi.retranslateItem).mockResolvedValue({
      success: false, error: { code: 'INVOKE_ERROR', message: 'MANUAL_REQUEST_UNKNOWN: owned foreign database (HTTP 409)' },
    });
    const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(true).mockReturnValue(false);
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    await fireEvent.click((await screen.findAllByRole('button', { name: '重新翻译' }))[0]);
    await fireEvent.click(await screen.findByRole('button', { name: '清除本浏览器未保存的请求' }));
    await waitFor(() => expect(localStorage.getItem('wptsall:manual-request:8')).toBeNull());
    expect(itemsApi.getItemContent).toHaveBeenCalledTimes(2);
    expect(itemsApi.retranslateItem).toHaveBeenCalledTimes(1);
    await fireEvent.click((await screen.findAllByRole('button', { name: '重新翻译' }))[0]);
    expect(confirm).toHaveBeenCalledTimes(2);
    expect(itemsApi.saveItemOverride).not.toHaveBeenCalled();
    expect(itemsApi.retranslateItem).toHaveBeenCalledTimes(1);
  });

  it('retains the browser request when the independent read finds an unfinished server request', async () => {
    const request = '631b8b74-913a-4972-8892-33a892ce07bb';
    localStorage.setItem('wptsall:manual-request:8', request);
    vi.mocked(itemsApi.retranslateItem).mockResolvedValue({
      success: false, error: { code: 'MANUAL_REQUEST_UNKNOWN', message: 'owned missing request' },
    });
    vi.spyOn(window, 'confirm').mockReturnValue(true);
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    await fireEvent.click((await screen.findAllByRole('button', { name: '重新翻译' }))[0]);
    const reset = await screen.findByRole('button', { name: '清除本浏览器未保存的请求' });
    const current = await vi.mocked(itemsApi.getItemContent)(8);
    vi.mocked(itemsApi.getItemContent).mockResolvedValue({
      success: true, data: { ...(current as any).data, manual_request_id: request },
    } as never);
    await fireEvent.click(reset);
    await waitFor(() => expect(itemsApi.getItemContent).toHaveBeenCalledTimes(3));
    expect(localStorage.getItem('wptsall:manual-request:8')).toBe(request);
    expect(itemsApi.retranslateItem).toHaveBeenCalledTimes(1);
    expect(itemsApi.saveItemOverride).not.toHaveBeenCalled();
  });

  async function usePackContent(extra: Record<string, unknown> = {}) {
    const original = await vi.mocked(itemsApi.getItemContent)(8);
    if (!original.success) throw new Error('Owned review fixture missing');
    const data = {
      ...original.data,
      item: { ...original.data.item, object_type: 'language_pack', business_line: 'plugin_i18n', task_type: 'text' },
      raw: {
        relation_id: 77, business_line: 'plugin_i18n', subtype: 'plugin',
        source_lang: 'en_US', target_lang: 'zh_CN',
        entries: [
          { entry_id: 101, msgstr: '旧按钮 %s', source: {
            object_id: 81, text_domain: 'owned-pack', msgctxt: 'Owned button',
            msgid: 'Hello %s', msgid_plural: 'Hello %s items', plural_index: 1,
          } },
          { entry_id: 102, msgstr: '旧菜单 %s', source: {
            object_id: 82, text_domain: 'owned-pack', msgctxt: 'Owned menu',
            msgid: 'Hello %s', msgid_plural: '', plural_index: 0,
          } },
        ],
      },
      translated: { payload: {
        relation_id: 77, business_line: 'plugin_i18n', source_lang: 'en_US', target_lang: 'zh_CN',
        entries: [
          { entry_id: 102, msgstr: '菜单 %s' },
          { entry_id: 101, msgstr: '按钮 %s' },
        ],
      } },
      delivery_unresolved: false,
      ...extra,
    };
    vi.mocked(itemsApi.getItemContent).mockReset().mockResolvedValue({ success: true, data } as never);
    vi.mocked(itemsApi.saveTranslated).mockResolvedValue({ success: true, data: {} });
    vi.mocked(itemsApi.approveItem).mockResolvedValue({ success: true, data: { item_id: 8, status: 'done' } });
    return data;
  }

  it('edits pack msgstr by entry ID and saves only entries, preserving read-only source and plural context', async () => {
    await usePackContent();
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-entry-101');
    expect((field as HTMLTextAreaElement).value).toBe('按钮 %s');
    expect((screen.getByTestId('review-entry-102') as HTMLTextAreaElement).value).toBe('菜单 %s');
    expect(screen.getByText('Owned button')).toBeTruthy();
    expect(screen.getByText('Owned menu')).toBeTruthy();
    expect(screen.getByText('Hello %s items')).toBeTruthy();
    expect(screen.getAllByText('owned-pack')).toHaveLength(2);
    expect(screen.queryByTestId('review-field-business_line')).toBeNull();
    await fireEvent.input(field, { target: { value: '已审核按钮 %s' } });
    await fireEvent.click(screen.getByTestId('review-save'));
    await waitFor(() => expect(itemsApi.saveTranslated).toHaveBeenCalledWith(8, {
      entries: [
        { entry_id: 101, msgstr: '已审核按钮 %s' },
        { entry_id: 102, msgstr: '菜单 %s' },
      ],
    }));
    expect(screen.getByText('Owned button')).toBeTruthy();
  });

  it('waits for a dirty pack save before approving and freezes edits while either request is running', async () => {
    await usePackContent();
    let finishSave!: (value: { success: true; data: Record<string, unknown> }) => void;
    vi.mocked(itemsApi.saveTranslated).mockImplementation(() => new Promise((resolve) => { finishSave = resolve; }));
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-entry-101');
    await fireEvent.input(field, { target: { value: '先保存 %s' } });
    await fireEvent.click(screen.getByTestId('review-approve'));
    await waitFor(() => expect(itemsApi.saveTranslated).toHaveBeenCalledOnce());
    expect(itemsApi.approveItem).not.toHaveBeenCalled();
    expect((field as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByTestId('review-save') as HTMLButtonElement).disabled).toBe(true);
    finishSave({ success: true, data: {} });
    await waitFor(() => expect(itemsApi.approveItem).toHaveBeenCalledWith(8));
    expect(vi.mocked(itemsApi.saveTranslated).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(itemsApi.approveItem).mock.invocationCallOrder[0]);
    expect((field as HTMLTextAreaElement).disabled).toBe(true);
  });

  it('retains rejected pack edits and never approves when saving fails', async () => {
    await usePackContent();
    vi.mocked(itemsApi.saveTranslated).mockResolvedValue({
      success: false, error: { code: 'REVIEW_ENTRY_SCOPE_CHANGED', message: 'Owned missing interpolation token' },
    });
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-entry-101');
    await fireEvent.input(field, { target: { value: '保留未保存的编辑' } });
    await fireEvent.click(screen.getByTestId('review-approve'));
    await waitFor(() => expect(itemsApi.saveTranslated).toHaveBeenCalledOnce());
    expect(itemsApi.approveItem).not.toHaveBeenCalled();
    expect((field as HTMLTextAreaElement).value).toBe('保留未保存的编辑');
    await waitFor(() => expect((field as HTMLTextAreaElement).disabled).toBe(false));
  });

  it('also saves dirty ordinary fields before approval instead of writing the older server value', async () => {
    vi.mocked(itemsApi.saveTranslated).mockResolvedValue({ success: true, data: {} });
    vi.mocked(itemsApi.approveItem).mockResolvedValue({ success: true, data: { item_id: 8, status: 'done' } });
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-field-post_title');
    await fireEvent.input(field, { target: { value: '最新审核标题' } });
    await fireEvent.click(screen.getByTestId('review-approve'));
    await waitFor(() => expect(itemsApi.approveItem).toHaveBeenCalledOnce());
    expect(itemsApi.saveTranslated).toHaveBeenCalledWith(8, { post_title: '最新审核标题' });
    expect(vi.mocked(itemsApi.saveTranslated).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(itemsApi.approveItem).mock.invocationCallOrder[0]);
  });

  it('makes pack fields and all save controls read-only while the original manual request is unfinished', async () => {
    await usePackContent({ manual_request_id: '631b8b74-913a-4972-8892-33a892ce07bb' });
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-entry-101');
    expect((field as HTMLTextAreaElement).disabled).toBe(true);
    for (const button of screen.getAllByRole('button', { name: '保存译文' })) {
      expect((button as HTMLButtonElement).disabled).toBe(true);
    }
    expect((screen.getByRole('button', { name: '保存覆盖' }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.queryByTestId('review-approve')).toBeNull();
    expect(screen.queryByTestId('review-reject')).toBeNull();
    expect(screen.getAllByRole('button', { name: '重新翻译' })).toHaveLength(2);
    expect(itemsApi.saveTranslated).not.toHaveBeenCalled();
  });

  it('keeps ordinary field inputs and the header save button disabled for an unfinished manual request', async () => {
    const original = await vi.mocked(itemsApi.getItemContent)(8);
    vi.mocked(itemsApi.getItemContent).mockResolvedValue({
      success: true, data: { ...(original as any).data, manual_request_id: '631b8b74-913a-4972-8892-33a892ce07bb' },
    } as never);
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-field-post_title');
    expect((field as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByTestId('review-save') as HTMLButtonElement).disabled).toBe(true);
  });

  it('shows an unresolved delivery and permits only the original approval, not editing, overrides, rejection or new fees', async () => {
    await usePackContent({ delivery_unresolved: true });
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-entry-101');
    expect(screen.getByTestId('review-delivery-unresolved')).toBeTruthy();
    expect((field as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByTestId('review-save') as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByLabelText('组件') as HTMLSelectElement).disabled).toBe(true);
    expect(screen.queryByTestId('review-reject')).toBeNull();
    expect(screen.queryByRole('button', { name: '重新翻译' })).toBeNull();
    await fireEvent.click(screen.getByTestId('review-approve'));
    await waitFor(() => expect(itemsApi.approveItem).toHaveBeenCalledWith(8));
    expect(itemsApi.saveTranslated).not.toHaveBeenCalled();
    expect(itemsApi.saveItemOverride).not.toHaveBeenCalled();
    expect(itemsApi.retranslateItem).not.toHaveBeenCalled();
  });

  it('refuses malformed pack identities without falling back to generic envelope fields', async () => {
    const data = await usePackContent();
    data.raw.entries[1].entry_id = 101;
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    expect(await screen.findByTestId('review-pack-fault')).toBeTruthy();
    expect(screen.queryByTestId('review-entry-101')).toBeNull();
    expect(screen.queryByTestId('review-field-business_line')).toBeNull();
    expect(screen.queryByTestId('review-approve')).toBeNull();
    expect(screen.queryByRole('button', { name: '重新翻译' })).toBeNull();
    expect(itemsApi.saveTranslated).not.toHaveBeenCalled();
  });

  it('refreshes delivery authority after a lost approval acknowledgement and fences changes without erasing the saved result', async () => {
    const data = await usePackContent();
    vi.mocked(itemsApi.getItemContent).mockResolvedValueOnce({ success: true, data } as never)
      .mockResolvedValue({ success: true, data: { ...data, delivery_unresolved: true } } as never);
    vi.mocked(itemsApi.approveItem).mockResolvedValue({
      success: false, error: { code: 'INVOKE_ERROR', message: 'Owned lost callback acknowledgement' },
    });
    render(TranslationReview, { itemId: 8, onBack: vi.fn() });
    const field = await screen.findByTestId('review-entry-101');
    await fireEvent.click(screen.getByTestId('review-approve'));
    await waitFor(() => expect(itemsApi.getItemContent).toHaveBeenCalledTimes(2));
    expect(await screen.findByTestId('review-delivery-unresolved')).toBeTruthy();
    expect((field as HTMLTextAreaElement).value).toBe('按钮 %s');
    expect((field as HTMLTextAreaElement).disabled).toBe(true);
    expect(screen.queryByRole('button', { name: '重新翻译' })).toBeNull();
  });
});
