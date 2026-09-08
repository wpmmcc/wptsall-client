import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import MyComponentsTab from './MyComponentsTab.svelte';
import * as componentsApi from '../api/components';
import * as keysApi from '../api/keys';
import * as toastModule from '../stores/toast';

const hoisted = vi.hoisted(() => ({
  statusStore: (() => {
    let value: any = {
      component_bindings: {
        components: {},
      },
    };
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
      update(updater: (current: any) => any) {
        value = updater(value);
        subscribers.forEach((run) => run(value));
      },
    };
  })(),
  fetchStatusMock: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../api/components', () => ({
  createComponentVersion: vi.fn(),
  createLocalComponent: vi.fn(),
  deleteBinding: vi.fn(),
  deleteComponentVersion: vi.fn(),
  deleteLocalComponent: vi.fn(),
  getLocalComponent: vi.fn(),
  listLocalComponents: vi.fn(),
  loadComponentTemplate: vi.fn(),
  quickTestLocalComponent: vi.fn(),
  refreshLocalComponentSnapshot: vi.fn(),
  testComponentVersion: vi.fn(),
  testLocalComponentFile: vi.fn(),
  updateComponentVersion: vi.fn(),
  updateLocalComponent: vi.fn(),
  upsertBinding: vi.fn(),
}));

vi.mock('../api/keys', () => ({
  listVendorKeys: vi.fn(),
  listOAuthConfigs: vi.fn(),
}));

vi.mock('../stores/status', () => ({
  status: hoisted.statusStore,
  fetchStatus: hoisted.fetchStatusMock,
}));

vi.mock('../stores/toast', () => ({
  showToast: vi.fn(),
}));

const baseComponent = {
  id: 'local-text',
  name: 'Local Text',
  template_id: 'official-text-v1',
  source_template_id: 'official-text-v1',
  source_template_api_version: '1.0.0',
  vendor_id: 'vendor-a',
  vendor_name: 'Vendor A',
  kind: 'text',
  remarks: '',
  created_at: '1',
  updated_at: '2026-03-11T00:00:00Z',
  enabled: true,
  versions: {
    stable: {
      version: 'stable',
      remarks: 'prod',
      key_ids: ['key-old'],
      key_selection_strategy: 'round_robin',
      proxy_profile_id: 'proxy-old',
      auth_type: 'key',
      created_at: '1',
    },
  },
};

const openAiComponent = {
  id: 'local-openai',
  name: 'Local OpenAI',
  template_id: '',
  source_template_id: '',
  source_template_api_version: null,
  vendor_id: 'vendor-openai',
  vendor_name: 'Vendor OpenAI',
  kind: 'openai_compatible',
  remarks: '',
  created_at: '1',
  updated_at: '2026-03-11T00:00:00Z',
  enabled: true,
  versions: {},
};

const imageComponent = {
  id: 'local-image',
  name: 'Local Image',
  template_id: 'official-image-v1',
  source_template_id: 'official-image-v1',
  source_template_api_version: '1.0.0',
  vendor_id: 'vendor-image',
  vendor_name: 'Vendor Image',
  kind: 'image',
  remarks: '',
  created_at: '1',
  updated_at: '2026-03-11T00:00:00Z',
  enabled: true,
  versions: {},
};

describe('components-page/MyComponentsTab', () => {
  beforeEach(() => {
    hoisted.statusStore.set({
      component_bindings: {
        components: {
          'local-text': {
            auth: {
              api_key: '***',
            },
            key_ids: ['key-old'],
            oauth_ids: ['oauth-main'],
            auth_strategy: 'Weighted',
          },
        },
      },
    });

    vi.mocked(componentsApi.listLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [baseComponent],
        page: 1,
        per_page: 50,
        total: 1,
        total_pages: 1,
        kinds: ['text'],
      },
    } as never);
    vi.mocked(componentsApi.loadComponentTemplate).mockResolvedValue({
      success: true,
      data: {
        component_id: 'official-text-v1',
        template_name: 'Official Text',
        template_version: '1.0.0',
        template_type: 'text',
        auth_modes: ['key', 'oauth'],
        auth_fields: [{ name: 'api_key', required: true }],
        template_json: {},
      },
    } as never);
    vi.mocked(componentsApi.upsertBinding).mockResolvedValue({
      success: true,
      data: { component_id: 'local-text' },
    } as never);
    vi.mocked(componentsApi.updateComponentVersion).mockResolvedValue({
      success: true,
      data: { component_id: 'local-text', version: 'stable' },
    } as never);
    vi.mocked(keysApi.listVendorKeys).mockResolvedValue({
      success: true,
      data: {
        items: [
          { id: 'key-old', vendor_id: 'vendor-a', label: 'Old Key' },
          { id: 'key-new', vendor_id: 'vendor-a', label: 'Primary Key' },
        ],
      },
    } as never);
    vi.mocked(keysApi.listOAuthConfigs).mockResolvedValue({
      success: true,
      data: {
        items: [
          { id: 'oauth-main', vendor_id: 'vendor-a', label: 'Main OAuth', grant_type: 'authorization_code' },
        ],
      },
    } as never);

    hoisted.fetchStatusMock.mockClear();
    vi.mocked(toastModule.showToast).mockClear();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('shows quick test request errors for openai-compatible components', async () => {
    vi.mocked(componentsApi.listLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [openAiComponent],
        page: 1,
        per_page: 50,
        total: 1,
        total_pages: 1,
        kinds: ['openai_compatible'],
      },
    } as never);
    vi.mocked(componentsApi.quickTestLocalComponent).mockRejectedValue(
      new Error('provider offline')
    );

    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('Local OpenAI'));

    const apiKeyInput = screen.getByPlaceholderText('API Key (sk-...)');
    await fireEvent.input(apiKeyInput, { target: { value: 'sk-test' } });

    await fireEvent.click(screen.getByText('测试'));

    await waitFor(() => {
      expect(componentsApi.quickTestLocalComponent).toHaveBeenCalledWith('local-openai', {
        api_key: 'sk-test',
        text: 'Hello World',
        source_lang: 'en_US',
        target_lang: 'zh_CN',
      });
    });
    expect(await screen.findByText('provider offline')).toBeTruthy();
  });

  it('runs file test for non-text components and renders translated output', async () => {
    vi.mocked(componentsApi.listLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [imageComponent],
        page: 1,
        per_page: 50,
        total: 1,
        total_pages: 1,
        kinds: ['image'],
      },
    } as never);
    vi.mocked(componentsApi.testLocalComponentFile).mockResolvedValue({
      success: true,
      data: {
        translated_ref: 'https://cdn.example.com/translated-image.png',
        translated_text: '图像已翻译',
        elapsed_ms: 321,
      },
    } as never);

    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('Local Image'));
    await fireEvent.click(screen.getByText('测试文件'));

    expect(await screen.findByText('文件翻译测试')).toBeTruthy();

    const urlInput = screen.getByLabelText('文件 URL') as HTMLInputElement;
    await fireEvent.input(urlInput, {
      target: { value: 'https://example.com/source-image.png' },
    });

    const sourceInput = screen.getByLabelText('源语言') as HTMLInputElement;
    await fireEvent.input(sourceInput, { target: { value: 'en_US' } });

    const targetInput = screen.getByLabelText('目标语言') as HTMLInputElement;
    await fireEvent.input(targetInput, { target: { value: 'ja_JP' } });

    await fireEvent.click(screen.getByText('运行测试'));

    await waitFor(() => {
      expect(componentsApi.testLocalComponentFile).toHaveBeenCalledWith({
        component_id: 'local-image',
        file_url: 'https://example.com/source-image.png',
        source_lang: 'en_US',
        target_lang: 'ja_JP',
      });
    });
    expect(await screen.findByText('测试成功（321ms）')).toBeTruthy();
    expect(screen.getByText('图像已翻译')).toBeTruthy();
    expect(screen.getByText('https://cdn.example.com/translated-image.png')).toBeTruthy();
  });

  it('shows structured file test errors returned by the backend', async () => {
    vi.mocked(componentsApi.listLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [imageComponent],
        page: 1,
        per_page: 50,
        total: 1,
        total_pages: 1,
        kinds: ['image'],
      },
    } as never);
    vi.mocked(componentsApi.testLocalComponentFile).mockResolvedValue({
      success: false,
      error: {
        message: 'source file expired',
      },
    } as never);

    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('Local Image'));
    await fireEvent.click(screen.getByText('测试文件'));
    expect(await screen.findByText('文件翻译测试')).toBeTruthy();

    await fireEvent.click(screen.getByText('运行测试'));

    await waitFor(() => {
      expect(componentsApi.testLocalComponentFile).toHaveBeenCalledWith({
        component_id: 'local-image',
        file_url: 'http://127.0.0.1:9090/api/v1/test-file/1024',
        source_lang: 'en',
        target_lang: 'zh-CN',
      });
    });
    expect(await screen.findByText('测试失败')).toBeTruthy();
    expect(screen.getByText('source file expired')).toBeTruthy();
  });

  it('shows generic request failure when file test throws', async () => {
    vi.mocked(componentsApi.listLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [imageComponent],
        page: 1,
        per_page: 50,
        total: 1,
        total_pages: 1,
        kinds: ['image'],
      },
    } as never);
    vi.mocked(componentsApi.testLocalComponentFile).mockRejectedValue(new Error());

    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('Local Image'));
    await fireEvent.click(screen.getByText('测试文件'));
    expect(await screen.findByText('文件翻译测试')).toBeTruthy();

    await fireEvent.click(screen.getByText('运行测试'));

    expect(await screen.findByText('测试失败')).toBeTruthy();
    expect(screen.getByText('请求失败')).toBeTruthy();
  });

  it('applies openai preset defaults and creates inline openai-compatible components', async () => {
    vi.mocked(componentsApi.createLocalComponent).mockResolvedValue({
      success: true,
      data: { id: 'deepseek' },
    } as never);

    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('+ 创建组件'));

    const kindSelect = screen.getByLabelText('组件类型') as HTMLSelectElement;
    await fireEvent.change(kindSelect, { target: { value: 'openai_compatible' } });
    await fireEvent.click(screen.getByText('DeepSeek'));

    expect((screen.getByLabelText('显示名称') as HTMLInputElement).value).toBe('DeepSeek');
    expect((screen.getByLabelText('组件 ID') as HTMLInputElement).value).toBe('deepseek');
    expect((screen.getByLabelText(/API 地址/) as HTMLInputElement).value).toBe(
      'https://api.deepseek.com'
    );
    expect((screen.getByLabelText(/模型名称/) as HTMLInputElement).value).toBe('deepseek-chat');

    await fireEvent.input(screen.getByLabelText('备注') as HTMLInputElement, {
      target: { value: 'builder' },
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(componentsApi.createLocalComponent).toHaveBeenCalledWith({
        id: 'deepseek',
        name: 'DeepSeek',
        enabled: true,
        vendor_id: undefined,
        vendor_name: undefined,
        kind: 'openai_compatible',
        remarks: 'builder',
        api_base: 'https://api.deepseek.com',
        model: 'deepseek-chat',
        system_prompt: undefined,
        temperature: 0.1,
        max_tokens: undefined,
        response_path: 'choices.0.message.content',
      });
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', '组件已创建');
  });

  it('validates required fields for standard component creation', async () => {
    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('+ 创建组件'));

    await fireEvent.click(screen.getByText('保存'));
    expect(toastModule.showToast).toHaveBeenLastCalledWith('error', '请填写组件名称');
    expect(componentsApi.createLocalComponent).not.toHaveBeenCalled();

    vi.mocked(toastModule.showToast).mockClear();
    await fireEvent.input(screen.getByLabelText('显示名称') as HTMLInputElement, {
      target: { value: 'Standard Text' },
    });
    await fireEvent.input(screen.getByLabelText('组件 ID') as HTMLInputElement, {
      target: { value: '' },
    });
    await fireEvent.click(screen.getByText('保存'));
    expect(toastModule.showToast).toHaveBeenLastCalledWith('error', '请填写组件 ID');
    expect(componentsApi.createLocalComponent).not.toHaveBeenCalled();

    vi.mocked(toastModule.showToast).mockClear();
    await fireEvent.input(screen.getByLabelText('组件 ID') as HTMLInputElement, {
      target: { value: 'standard-text' },
    });
    await fireEvent.click(screen.getByText('保存'));
    expect(toastModule.showToast).toHaveBeenLastCalledWith('error', '请填写 Server 模板 ID');
    expect(componentsApi.createLocalComponent).not.toHaveBeenCalled();
  });

  it('validates openai-compatible specific fields during creation', async () => {
    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('+ 创建组件'));

    const kindSelect = screen.getByLabelText('组件类型') as HTMLSelectElement;
    await fireEvent.change(kindSelect, { target: { value: 'openai_compatible' } });
    await fireEvent.input(screen.getByLabelText('显示名称') as HTMLInputElement, {
      target: { value: 'Inline OpenAI' },
    });
    await fireEvent.input(screen.getByLabelText('组件 ID') as HTMLInputElement, {
      target: { value: 'inline-openai' },
    });

    await fireEvent.click(screen.getByText('保存'));
    expect(toastModule.showToast).toHaveBeenLastCalledWith('error', '请填写 API 地址');
    expect(componentsApi.createLocalComponent).not.toHaveBeenCalled();

    vi.mocked(toastModule.showToast).mockClear();
    await fireEvent.input(screen.getByLabelText(/API 地址/) as HTMLInputElement, {
      target: { value: 'https://api.example.com' },
    });
    await fireEvent.click(screen.getByText('保存'));
    expect(toastModule.showToast).toHaveBeenLastCalledWith('error', '请填写模型名称');
    expect(componentsApi.createLocalComponent).not.toHaveBeenCalled();
  });

  it('loads vendor pool resources by vendor id and preserves pool config on save', async () => {
    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('Local Text'));

    await fireEvent.click(screen.getByText('编辑 Auth'));

    expect(await screen.findByText('Auth 凭据 — Local Text')).toBeTruthy();
    await waitFor(() => {
      expect(keysApi.listVendorKeys).toHaveBeenCalledWith('vendor-a');
      expect(keysApi.listOAuthConfigs).toHaveBeenCalledWith('vendor-a');
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(componentsApi.upsertBinding).toHaveBeenCalledWith({
        component_id: 'local-text',
        auth: { api_key: '' },
        key_ids: ['key-old'],
        oauth_ids: ['oauth-main'],
        auth_strategy: 'Weighted',
        constraints_override: undefined,
        request_overrides: undefined,
      });
    });
    expect(hoisted.fetchStatusMock).toHaveBeenCalled();
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Auth 凭据已保存');
  });

  it('updates component version including proxy profile and auth type', async () => {
    render(MyComponentsTab);

    expect(await screen.findByText('本地组件实例（1）')).toBeTruthy();
    await fireEvent.click(screen.getByText('Local Text'));

    const versionRow = screen.getByText('stable').closest('div.flex.items-center') as HTMLElement;
    await fireEvent.click(within(versionRow).getByText('编辑'));

    expect(await screen.findByText('编辑版本 — Local Text')).toBeTruthy();

    const authType = screen.getByLabelText('Auth 类型') as HTMLSelectElement;
    await fireEvent.change(authType, { target: { value: 'oauth' } });

    const proxyProfile = screen.getByLabelText('代理 Profile ID') as HTMLInputElement;
    await fireEvent.input(proxyProfile, { target: { value: 'proxy-main' } });

    const keyIds = screen.getByLabelText('Key IDs') as HTMLInputElement;
    await fireEvent.input(keyIds, { target: { value: 'key-main, key-backup' } });

    await fireEvent.click(screen.getByText('更新'));

    await waitFor(() => {
      expect(componentsApi.updateComponentVersion).toHaveBeenCalledWith('local-text', 'stable', {
        key_ids: ['key-main', 'key-backup'],
        key_selection_strategy: 'round_robin',
        auth_type: 'oauth',
        proxy_profile_id: 'proxy-main',
        remarks: 'prod',
      });
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', '版本已更新');
  });
});
