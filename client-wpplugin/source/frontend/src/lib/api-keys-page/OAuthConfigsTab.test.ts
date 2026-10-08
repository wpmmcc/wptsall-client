// catalog: WEBUI-UI-OAuthConfigsTab
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功 / 空态 / 失败态（后端 5xx 回显）；
// 表单（内嵌 OAuthModal）：校验失败（缺 ID/Client ID、缺 auth_url、缺 token_url）+ 提交成功流转。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import OAuthConfigsTab from './OAuthConfigsTab.svelte';
import * as keysApi from '../api/keys';
import * as toastModule from '../stores/toast';

vi.mock('../api/keys', () => ({
  listOAuthConfigs: vi.fn(),
  createOAuthConfig: vi.fn(),
  updateOAuthConfig: vi.fn(),
  deleteOAuthConfig: vi.fn(),
  authorizeOAuth: vi.fn(),
}));

vi.mock('../stores/toast', () => ({
  showToast: vi.fn(),
}));

const oauthItem = {
  id: 'oauth-main',
  vendor_id: 'vendor-a',
  label: 'Main OAuth',
  grant_type: 'client_credentials',
  token_url: 'https://example.com/token',
  client_id: 'client-1',
  scopes: '',
  max_concurrent: 5,
  weight: 1,
  max_input_chars: 0,
  max_file_size_mb: 0,
  token_field: 'access_token',
  has_token: true,
};

function mockList(items = [oauthItem]) {
  vi.mocked(keysApi.listOAuthConfigs).mockResolvedValue({
    success: true,
    data: { items },
  } as never);
}

describe('api-keys-page/OAuthConfigsTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    mockList();
  });

  afterEach(() => {
    cleanup();
  });

  it('loads oauth config rows on mount', async () => {
    render(OAuthConfigsTab);

    expect(await screen.findByText('Main OAuth')).toBeTruthy();
    expect(screen.getByText('oauth-main')).toBeTruthy();
    expect(screen.getByText('client_credentials')).toBeTruthy();
    expect(screen.getByText('有效')).toBeTruthy();
    expect(screen.getByText('+ 添加')).toBeTruthy();
    expect(keysApi.listOAuthConfigs).toHaveBeenCalledTimes(1);
  });

  it('shows empty state when no oauth configs exist', async () => {
    mockList([]);
    render(OAuthConfigsTab);

    expect(await screen.findByText('暂无 OAuth 配置')).toBeTruthy();
  });

  it('rejects save when id or client id missing', async () => {
    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('+ 添加'));
    expect(await screen.findByText('创建 OAuth 配置')).toBeTruthy();

    await fireEvent.click(screen.getByText('保存'));

    expect(toastModule.showToast).toHaveBeenCalledWith('error', '请填写 ID 和 Client ID');
    expect(keysApi.createOAuthConfig).not.toHaveBeenCalled();
  });

  it('rejects save when authorization_code lacks auth_url', async () => {
    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('+ 添加'));
    expect(await screen.findByText('创建 OAuth 配置')).toBeTruthy();

    const grantSelect = screen.getByLabelText(
      /授权类型/,
    ) as unknown as HTMLSelectElement;
    await fireEvent.change(grantSelect, { target: { value: 'authorization_code' } });

    await fireEvent.input(screen.getByPlaceholderText('Config ID（必填）'), {
      target: { value: 'oauth-new' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Client ID（必填）'), {
      target: { value: 'client-2' },
    });

    await fireEvent.click(screen.getByText('保存'));

    expect(toastModule.showToast).toHaveBeenCalledWith('error', '请填写 Authorization URL');
    expect(keysApi.createOAuthConfig).not.toHaveBeenCalled();
  });

  it('rejects save when client_credentials lacks token_url', async () => {
    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('+ 添加'));
    expect(await screen.findByText('创建 OAuth 配置')).toBeTruthy();

    await fireEvent.input(screen.getByPlaceholderText('Config ID（必填）'), {
      target: { value: 'oauth-new' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Client ID（必填）'), {
      target: { value: 'client-2' },
    });

    await fireEvent.click(screen.getByText('保存'));

    expect(toastModule.showToast).toHaveBeenCalledWith('error', '请填写 Token URL');
    expect(keysApi.createOAuthConfig).not.toHaveBeenCalled();
  });

  it('creates an oauth config with extra params and reloads on success', async () => {
    vi.mocked(keysApi.createOAuthConfig).mockResolvedValue({
      success: true,
      data: { id: 'oauth-new' },
    } as never);

    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('+ 添加'));
    expect(await screen.findByText('创建 OAuth 配置')).toBeTruthy();

    const grantSelect = screen.getByLabelText(/授权类型/) as unknown as HTMLSelectElement;
    await fireEvent.change(grantSelect, { target: { value: 'authorization_code' } });

    // authorization_code shows the callback url + extra params block
    expect(await screen.findByText(/将以下回调地址填入第三方平台/)).toBeTruthy();

    await fireEvent.input(screen.getByPlaceholderText('Config ID（必填）'), {
      target: { value: 'oauth-new' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Vendor ID（如 google、deepl）'), {
      target: { value: 'vendor-a' },
    });
    await fireEvent.input(
      screen.getByPlaceholderText('https://example.com/oauth/authorize'),
      { target: { value: 'https://example.com/authorize' } },
    );
    await fireEvent.input(screen.getByPlaceholderText('https://example.com/oauth/token'), {
      target: { value: 'https://example.com/token' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Client ID（必填）'), {
      target: { value: 'client-2' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Client Secret（留空保留原值）'), {
      target: { value: 'secret-2' },
    });

    // apply the Google extra-param preset then add one custom param
    await fireEvent.click(screen.getByText('Google'));
    await fireEvent.click(screen.getByText('添加'));

    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(keysApi.createOAuthConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          id: 'oauth-new',
          vendor_id: 'vendor-a',
          grant_type: 'authorization_code',
          auth_url: 'https://example.com/authorize',
          token_url: 'https://example.com/token',
          client_id: 'client-2',
          client_secret: 'secret-2',
          auth_extra_params: {
            access_type: 'offline',
            prompt: 'consent',
          },
        }),
      );
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'OAuth 配置已保存');
    expect(keysApi.listOAuthConfigs).toHaveBeenCalledTimes(2);
  });

  it('shows backend error toast when create fails', async () => {
    vi.mocked(keysApi.createOAuthConfig).mockResolvedValue({
      success: false,
      error: { code: 'INTERNAL', message: 'server 5xx: config rejected', status: 500 },
    } as never);

    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('+ 添加'));
    expect(await screen.findByText('创建 OAuth 配置')).toBeTruthy();

    await fireEvent.input(screen.getByPlaceholderText('Config ID（必填）'), {
      target: { value: 'oauth-bad' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Client ID（必填）'), {
      target: { value: 'client-x' },
    });
    await fireEvent.input(screen.getByPlaceholderText('https://example.com/oauth/token'), {
      target: { value: 'https://example.com/token' },
    });

    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '创建失败',
        'server 5xx: config rejected',
      );
    });
    // modal stays open on failure
    expect(screen.getByText('创建 OAuth 配置')).toBeTruthy();
  });

  it('opens edit modal prefilled and updates the config on success', async () => {
    vi.mocked(keysApi.updateOAuthConfig).mockResolvedValue({
      success: true,
      data: { id: 'oauth-main' },
    } as never);

    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('编辑'));
    expect(await screen.findByText('编辑 OAuth 配置')).toBeTruthy();
    // editing mode hides the config id input
    expect(screen.queryByPlaceholderText('Config ID（必填）')).toBeNull();
    expect((screen.getByLabelText(/名称 \(label\)/) as HTMLInputElement).value).toBe('Main OAuth');

    await fireEvent.input(screen.getByLabelText(/名称 \(label\)/), {
      target: { value: 'Renamed OAuth' },
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(keysApi.updateOAuthConfig).toHaveBeenCalledWith(
        'oauth-main',
        expect.objectContaining({ label: 'Renamed OAuth' }),
      );
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'OAuth 配置已更新');
  });

  it('deletes an oauth config and reloads the list', async () => {
    vi.mocked(keysApi.deleteOAuthConfig).mockResolvedValue({
      success: true,
    } as never);

    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('删除'));

    await waitFor(() => {
      expect(keysApi.deleteOAuthConfig).toHaveBeenCalledWith('oauth-main');
      expect(toastModule.showToast).toHaveBeenCalledWith('success', '配置已删除');
    });
    expect(keysApi.listOAuthConfigs).toHaveBeenCalledTimes(2);
  });

  it('authorizes a client_credentials config and reports token preview', async () => {
    vi.mocked(keysApi.authorizeOAuth).mockResolvedValue({
      success: true,
      data: { token_preview: 'tok-123', expires_in: 3600 },
    } as never);

    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('授权'));

    await waitFor(() => {
      expect(keysApi.authorizeOAuth).toHaveBeenCalledWith('oauth-main');
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'success',
        '授权成功，Token 已获取: tok-123 (3600s)',
      );
    });
  });

  it('shows backend error toast when authorize fails', async () => {
    vi.mocked(keysApi.authorizeOAuth).mockResolvedValue({
      success: false,
      error: { code: 'UPSTREAM_401', message: 'invalid client credentials', status: 502 },
    } as never);

    render(OAuthConfigsTab);
    await screen.findByText('Main OAuth');

    await fireEvent.click(screen.getByText('授权'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '授权失败',
        'invalid client credentials',
      );
    });
  });
});
