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
  updateComponentVersion: vi.fn(),
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

  it('sends the entered search and renders only the returned catalog rows', async () => {
    const customItem = {
      ...catalogItem,
      entry_id: 'fixture-custom',
      template_id: 'custom-template',
      name: '图片 + &? #=%',
      vendor_id: 'custom_http_mt',
      family: 'http_mt',
    };
    mockCatalogList([catalogItem, customItem]);
    render(VendorCatalogTab);
    await screen.findByText(catalogItem.name);
    expect(screen.getByText(customItem.name)).toBeTruthy();

    mockCatalogList([customItem]);
    const query = '  图片 + &? #=%  ';
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: query } });
    await fireEvent.keyDown(screen.getByRole('textbox'), { key: 'Enter' });
    await waitFor(() => {
      expect(keysApi.listProviderCatalog).toHaveBeenLastCalledWith({ q: query });
      expect(screen.queryByText(catalogItem.name)).toBeNull();
      expect(screen.getByText(customItem.name)).toBeTruthy();
    });

    mockCatalogList([]);
    await fireEvent.input(screen.getByRole('textbox'), {
      target: { value: 'not-present' },
    });
    await fireEvent.keyDown(screen.getByRole('textbox'), { key: 'Enter' });
    expect(await screen.findByText('暂无 Provider 模板')).toBeTruthy();
    expect(screen.queryByText(customItem.name)).toBeNull();
  });

  it('retains the local family and evidence-tier filters', async () => {
    const original = { ...catalogItem, evidence_tier: 'mock-verified' };
    const customItem = {
      ...catalogItem,
      entry_id: 'fixture-custom',
      template_id: 'custom-template',
      name: 'Custom HTTP fixture',
      vendor_id: 'custom_http_mt',
      family: 'http_mt',
      evidence_tier: 'schema-only',
    };
    mockCatalogList([original, customItem]);
    render(VendorCatalogTab);
    await screen.findByText(original.name);
    const [family, tier] = screen.getAllByRole('combobox');
    await fireEvent.change(family, { target: { value: 'http_mt' } });
    expect(screen.queryByText(original.name)).toBeNull();
    expect(screen.getByText(customItem.name)).toBeTruthy();

    await fireEvent.change(family, { target: { value: '' } });
    await fireEvent.change(tier, { target: { value: 'mock-verified' } });
    expect(screen.getByText(original.name)).toBeTruthy();
    expect(screen.queryByText(customItem.name)).toBeNull();
    expect(keysApi.listProviderCatalog).toHaveBeenCalledTimes(1);
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
    // The wizard auto-fills config overrides from the catalog template
    // (request.url / request.body.model), so the enable step first persists
    // them via updateComponentVersion before flipping enabled.
    vi.mocked(componentsApi.updateComponentVersion).mockResolvedValue({
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
      // Config overrides are persisted onto the v1 version first...
      expect(componentsApi.updateComponentVersion).toHaveBeenCalledWith(
        expect.any(String),
        'v1',
        expect.objectContaining({ config_overrides: expect.anything() }),
      );
      // ...then the component is enabled.
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

  it('quick-test sends the user-selected source/target languages', async () => {
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
      data: { translated_text: 'hello world', elapsed_ms: 10 },
    } as never);

    render(VendorCatalogTab);
    await screen.findByText('OpenAI-compatible Chat Completions');
    await fireEvent.click(screen.getByTestId('open-provider-wizard'));
    await fireEvent.click(screen.getByTestId('wizard-install-next'));
    await waitFor(() => screen.getByTestId('wizard-api-key'));
    await fireEvent.input(screen.getByTestId('wizard-api-key'), {
      target: { value: 'sk-test-mock' },
    });
    await fireEvent.click(screen.getByTestId('wizard-key-next'));
    await waitFor(() => screen.getByTestId('wizard-test-next'));

    // Defaults keep the historical en_US/zh_CN pair ...
    expect(screen.getByTestId('wizard-test-source-lang')).toBeTruthy();
    expect(screen.getByTestId('wizard-test-target-lang')).toBeTruthy();
    // ... and the user can override both (e.g. DeepL-style EN/ZH codes).
    await fireEvent.input(screen.getByTestId('wizard-test-source-lang'), {
      target: { value: 'EN' },
    });
    await fireEvent.input(screen.getByTestId('wizard-test-target-lang'), {
      target: { value: 'ZH' },
    });
    await fireEvent.click(screen.getByTestId('wizard-test-next'));

    await waitFor(() => {
      expect(componentsApi.quickTestLocalComponent).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({
          source_lang: 'EN',
          target_lang: 'ZH',
        }),
      );
    });
  });

  // 3.8flash A3: the FIRST catalog view on a clean client runs an inline
  // online fetch (server-side, 3s-bounded). While it is pending the loading
  // cell must say so (a silent "Loading…" read as a hang offline, because
  // the built-in templates only appear after the timeout).
  it('shows the first-fetch loading hint while the initial catalog load is pending', async () => {
    vi.mocked(keysApi.listProviderCatalog).mockReturnValue(new Promise(() => {}) as never);

    render(VendorCatalogTab);

    const loadingCell = screen.getByTestId('apikeys-vendors-loading');
    expect(loadingCell.textContent).toContain('首次获取目录中');
    expect(loadingCell.textContent).toContain('内置模板');
  });

  // ... but a RE-load with items already on screen is just a filter/refresh
  // pass — the plain loading label, not the first-fetch hint.
  it('re-loads show the plain loading label, not the first-fetch hint', async () => {
    mockCatalogList();
    render(VendorCatalogTab);
    await screen.findByText('OpenAI-compatible Chat Completions');

    // Next list call hangs: items are populated, so no first-fetch hint.
    vi.mocked(keysApi.listProviderCatalog).mockReturnValue(new Promise(() => {}) as never);
    await fireEvent.click(screen.getByText('刷新'));

    const loadingCell = await screen.findByTestId('apikeys-vendors-loading');
    expect(loadingCell.textContent).not.toContain('首次获取目录中');
  });
});
