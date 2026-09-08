import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { addMessages, init, locale } from 'svelte-i18n';
import ServerTemplatesTab from './ServerTemplatesTab.svelte';
import * as componentsApi from '../api/components';
import * as toastModule from '../stores/toast';
import en from '../../../../locales/en.json';
import zhCN from '../../../../locales/zh-CN.json';

addMessages('en', en);
addMessages('zh-CN', zhCN);
init({
  fallbackLocale: 'en',
  initialLocale: 'en',
});

const hoisted = vi.hoisted(() => ({
  statusStore: (() => {
    let value: any = { components: [] };
    const subscribers = new Set<(next: any) => void>();
    return {
      subscribe(run: (next: any) => void) {
        subscribers.add(run);
        run(value);
        return () => subscribers.delete(run);
      },
      set(next: any) {
        value = next;
        subscribers.forEach((run) => run(value));
      },
    };
  })(),
}));

vi.mock('../api/components', () => ({
  loadComponentTemplate: vi.fn(),
  searchServerComponents: vi.fn(),
}));

vi.mock('../stores/status', () => ({
  status: hoisted.statusStore,
}));

vi.mock('../stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('components-page/ServerTemplatesTab', () => {
  beforeEach(() => {
    locale.set('en');
    hoisted.statusStore.set({
      components: [
        {
          id: 'server-text',
          kind: 'text',
          template_group: 'official',
          supported_content_formats: ['plain_text', 'rich_html'],
          supported_business_lines: ['post_content'],
          size_class: 'S',
        },
      ],
    });
    vi.mocked(componentsApi.searchServerComponents).mockResolvedValue({
      success: true,
      data: {
        items: [
          {
            id: 'server-text',
            name: 'Server Text',
            product_id: 'cloud-api-hub',
            catalog_family: 'text-language',
            catalog_subfamily: 'plain-text-translation',
            capability_tags: ['translation'],
            kind: 'text',
            version: '1.0.0',
            template_group: 'official',
            signing_algorithm: 'hmac_sha256',
            supported_content_formats: ['plain_text', 'rich_html'],
            supported_formats: ['txt'],
            supported_business_lines: ['post_content'],
            auth_modes: ['key'],
            translation_modes: [],
            max_file_size_mb: 1,
            api_docs_url: 'https://docs.example.com/server-text',
          },
        ],
        page: 1,
        per_page: 20,
        total: 1,
        total_pages: 1,
      },
    } as never);
    vi.mocked(componentsApi.loadComponentTemplate).mockResolvedValue({
      success: true,
      data: {
        template_name: 'Server Text Template',
        template_version: '1.0.0',
        auth_fields: [{ name: 'api_key', required: true }],
        auth_modes: ['key'],
        template_json: {
          auth: { fields: [{ name: 'api_key', required: true }] },
          translation_modes: [
            {
              id: 'html_mode',
              label: 'HTML 模式',
              supported_content_formats: ['rich_html'],
            },
          ],
        },
      },
    } as never);
    vi.mocked(toastModule.showToast).mockClear();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('filters server templates and opens the template detail modal', async () => {
    render(ServerTemplatesTab);

    expect(await screen.findByText('Server Text')).toBeTruthy();
    expect(componentsApi.searchServerComponents).toHaveBeenCalledWith({
      page: 1,
      per_page: 20,
        q: '',
        product_id: undefined,
        kind: undefined,
        group: undefined,
        family: undefined,
        subfamily: undefined,
        content_format: undefined,
        size_class: undefined,
        business_line: undefined,
        locale: 'en',
      });

      const selects = screen.getAllByRole('combobox');
      await fireEvent.change(selects[1], { target: { value: 'text' } });

    await waitFor(() => {
      expect(componentsApi.searchServerComponents).toHaveBeenLastCalledWith(
        expect.objectContaining({ kind: 'text' })
      );
    });

    await fireEvent.click(screen.getByText('View'));

    await waitFor(() => {
      expect(componentsApi.loadComponentTemplate).toHaveBeenCalledWith('server-text');
    });
    expect(await screen.findByText('Server Text Template')).toBeTruthy();
    expect(screen.getByText('Auth Fields')).toBeTruthy();
    expect(screen.getByText('api_key *')).toBeTruthy();
    expect(screen.getByText('Standard Compatibility')).toBeTruthy();
    expect(screen.getByText('纯文本 / HTML 富文本')).toBeTruthy();
    expect(screen.getByText('支持 HTML')).toBeTruthy();
    expect(screen.getByText('post_content/Classic HTML/Gutenberg 富文本')).toBeTruthy();
    expect(screen.getByText('HTML 模式')).toBeTruthy();
  });

  it('shows fallback empty state when template search fails', async () => {
    vi.mocked(componentsApi.searchServerComponents).mockResolvedValue({
      success: false,
      error: { message: 'upstream unavailable' },
    } as never);

    render(ServerTemplatesTab);

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        'Template search failed',
        'upstream unavailable'
      );
    });
    expect(await screen.findByText('No templates, please refresh components first')).toBeTruthy();
  });

  it('shows an error toast and closes the modal when template loading fails', async () => {
    vi.mocked(componentsApi.loadComponentTemplate).mockResolvedValue({
      success: false,
      error: { message: 'template missing' },
    } as never);

    render(ServerTemplatesTab);

    expect(await screen.findByText('Server Text')).toBeTruthy();
    await fireEvent.click(screen.getByText('View'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        'Failed to load template',
        'template missing'
      );
    });
    await waitFor(() => {
      expect(screen.queryByText('Server Text Template')).toBeNull();
    });
  });
});
