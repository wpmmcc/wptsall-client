// catalog: WEBUI-UI-VendorCatalogTab
// oracle: L2
// 状态矩阵：本地供应商目录加载 + 免登刷新 / 错误态 + 重载恢复 / 安装→密钥→测试→启用→路由向导 / 测试失败停留。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import VendorCatalogTab from './VendorCatalogTab.svelte';
import * as keysApi from '../api/keys';
import * as componentsApi from '../api/components';

vi.mock('../api/keys', () => ({
  listProviderCatalog: vi.fn(),
  refreshProviderCatalog: vi.fn(),
  installCatalogTemplate: vi.fn(),
  createVendorKey: vi.fn(),
}));

vi.mock('../api/components', () => ({
  createComponentVersion: vi.fn(),
  quickTestLocalComponent: vi.fn(),
  updateLocalComponent: vi.fn(),
  upsertRuleBinding: vi.fn(),
}));

const catalogItem = {
  entry_id: 'openai-compatible',
  template_id: 'openai-compatible-chat-completions-v1',
  name: 'OpenAI-compatible Chat Completions',
  vendor_id: 'openai_compatible',
  family: 'openai_compatible',
  kind: 'text',
  supported_content_formats: ['plain_text'],
  source: 'builtin',
  verified: true,
  requires_local_credentials: true,
  template: {
    auth: { fields: [{ name: 'api_key', required: true }] },
  },
};

function mockCatalogList(items = [catalogItem]) {
  vi.mocked(keysApi.listProviderCatalog).mockResolvedValue({
    success: true,
    data: {
      schema: 'wptsall-provider-catalog-manifest.v1',
      catalog_version: 'builtin-3',
      offline: true,
      items,
    },
  } as never);
}

describe('api-keys-page/VendorCatalogTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('loads the local provider catalog and refreshes it without server login', async () => {
    vi.mocked(keysApi.listProviderCatalog)
      .mockResolvedValueOnce({
        success: true,
        data: {
          schema: 'wptsall-provider-catalog-manifest.v1',
          catalog_version: 'builtin-1',
          offline: true,
          items: [catalogItem],
        },
      } as never)
      .mockResolvedValueOnce({
        success: true,
        data: {
          schema: 'wptsall-provider-catalog-manifest.v1',
          catalog_version: 'cache-2',
          offline: true,
          items: [],
        },
      } as never);
    vi.mocked(keysApi.refreshProviderCatalog).mockResolvedValue({
      success: true,
      data: {
        catalog_version: 'cache-2',
        template_count: 0,
        source: 'local_cache',
        verified: true,
        offline: true,
      },
    } as never);

    render(VendorCatalogTab);

    expect(await screen.findByText('OpenAI-compatible Chat Completions')).toBeTruthy();
    expect(screen.getByText(/本地目录；无需官网控制面/)).toBeTruthy();

    await fireEvent.click(screen.getByText('校验本地目录'));

    await waitFor(() => {
      expect(keysApi.refreshProviderCatalog).toHaveBeenCalledTimes(1);
      expect(keysApi.listProviderCatalog).toHaveBeenCalledTimes(2);
    });
    expect(screen.getByText('暂无 Provider 模板')).toBeTruthy();
  });

  it('shows a local catalog error and recovers on reload', async () => {
    vi.mocked(keysApi.listProviderCatalog)
      .mockResolvedValueOnce({
        success: false,
        error: { code: 'CATALOG_INVALID', message: 'catalog unavailable' },
      } as never)
      .mockResolvedValueOnce({
        success: true,
        data: {
          schema: 'wptsall-provider-catalog-manifest.v1',
          catalog_version: 'builtin-1',
          offline: true,
          items: [],
        },
      } as never);

    render(VendorCatalogTab);

    expect(await screen.findByText('catalog unavailable')).toBeTruthy();

    await fireEvent.click(screen.getByText('刷新'));

    await waitFor(() => {
      expect(screen.getByText('暂无 Provider 模板')).toBeTruthy();
    });
    expect(keysApi.listProviderCatalog).toHaveBeenCalledTimes(2);
  });

  it('runs the provider setup wizard through install → key → test → enable → route', async () => {
    mockCatalogList();
    vi.mocked(keysApi.installCatalogTemplate).mockResolvedValue({
      success: true,
      data: {
        id: 'openai-compatible-openai-compatible-chat-completions-v1',
        template_id: catalogItem.template_id,
        catalog_entry_id: catalogItem.entry_id,
        enabled: false,
        overwrite: false,
      },
    } as never);
    vi.mocked(keysApi.createVendorKey).mockResolvedValue({
      success: true,
      data: { id: 'openai-compatible-openai-compatible-chat-completions-v1-key' },
    } as never);
    vi.mocked(componentsApi.createComponentVersion).mockResolvedValue({
      success: true,
      data: {
        component_id: 'openai-compatible-openai-compatible-chat-completions-v1',
        version: 'v1',
      },
    } as never);
    vi.mocked(componentsApi.quickTestLocalComponent).mockResolvedValue({
      success: true,
      data: { translated_text: '你好世界', elapsed_ms: 12 },
    } as never);
    vi.mocked(componentsApi.updateLocalComponent).mockResolvedValue({
      success: true,
      data: { id: 'openai-compatible-openai-compatible-chat-completions-v1' },
    } as never);
    vi.mocked(componentsApi.upsertRuleBinding).mockResolvedValue({
      success: true,
      data: {
        scope: 'global',
        slot_key: 'plain_text',
        component_id: 'openai-compatible-openai-compatible-chat-completions-v1',
      },
    } as never);

    render(VendorCatalogTab);
    expect(await screen.findByText('OpenAI-compatible Chat Completions')).toBeTruthy();

    await fireEvent.click(screen.getByTestId('open-provider-wizard'));
    expect(await screen.findByTestId('provider-setup-wizard')).toBeTruthy();
    expect(screen.getByText('1 安装')).toBeTruthy();

    await fireEvent.click(screen.getByTestId('wizard-install-next'));
    await waitFor(() => {
      expect(keysApi.installCatalogTemplate).toHaveBeenCalledWith(
        expect.objectContaining({ entry_id: 'openai-compatible' }),
      );
      expect(screen.getByTestId('wizard-api-key')).toBeTruthy();
    });

    await fireEvent.input(screen.getByTestId('wizard-api-key'), {
      target: { value: 'sk-test-mock' },
    });
    await fireEvent.click(screen.getByTestId('wizard-key-next'));
    await waitFor(() => {
      expect(keysApi.createVendorKey).toHaveBeenCalled();
      expect(componentsApi.createComponentVersion).toHaveBeenCalled();
      expect(screen.getByTestId('wizard-test-next')).toBeTruthy();
    });

    await fireEvent.click(screen.getByTestId('wizard-test-next'));
    await waitFor(() => {
      expect(componentsApi.quickTestLocalComponent).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          api_key: 'sk-test-mock',
          auth_values: expect.objectContaining({ api_key: 'sk-test-mock' }),
        }),
      );
      expect(screen.getByTestId('wizard-enable-next')).toBeTruthy();
    });

    await fireEvent.click(screen.getByTestId('wizard-enable-next'));
    await waitFor(() => {
      expect(componentsApi.updateLocalComponent).toHaveBeenCalledWith(
        expect.any(String),
        { enabled: true },
      );
      expect(screen.getByTestId('wizard-route-next')).toBeTruthy();
    });

    await fireEvent.click(screen.getByTestId('wizard-route-next'));
    await waitFor(() => {
      expect(componentsApi.upsertRuleBinding).toHaveBeenCalledWith(
        'global',
        'plain_text',
        expect.any(String),
      );
      expect(screen.getByTestId('wizard-done')).toBeTruthy();
    });
  });

  it('keeps the wizard on test step when quick-test fails', async () => {
    mockCatalogList();
    vi.mocked(keysApi.installCatalogTemplate).mockResolvedValue({
      success: true,
      data: {
        id: 'comp-1',
        template_id: catalogItem.template_id,
        catalog_entry_id: catalogItem.entry_id,
        enabled: false,
        overwrite: false,
      },
    } as never);
    vi.mocked(keysApi.createVendorKey).mockResolvedValue({
      success: true,
      data: { id: 'comp-1-key' },
    } as never);
    vi.mocked(componentsApi.createComponentVersion).mockResolvedValue({
      success: true,
      data: { component_id: 'comp-1', version: 'v1' },
    } as never);
    vi.mocked(componentsApi.quickTestLocalComponent).mockResolvedValue({
      success: true,
      data: { error: 'provider 401' },
    } as never);

    render(VendorCatalogTab);
    await screen.findByText('OpenAI-compatible Chat Completions');
    await fireEvent.click(screen.getByTestId('open-provider-wizard'));
    await fireEvent.click(screen.getByTestId('wizard-install-next'));
    await waitFor(() => screen.getByTestId('wizard-api-key'));
    await fireEvent.input(screen.getByTestId('wizard-api-key'), {
      target: { value: 'bad-key' },
    });
    await fireEvent.click(screen.getByTestId('wizard-key-next'));
    await waitFor(() => screen.getByTestId('wizard-test-next'));
    await fireEvent.click(screen.getByTestId('wizard-test-next'));

    await waitFor(() => {
      expect(screen.getByText('provider 401')).toBeTruthy();
      expect(screen.getByTestId('wizard-test-next')).toBeTruthy();
    });
    expect(componentsApi.updateLocalComponent).not.toHaveBeenCalled();
  });
});
