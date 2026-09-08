// catalog: WEBUI-UI-OAuthModal
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功（创建/编辑 + authorization_code 分支）/ 空态（open=false 不渲染）/
// 弹窗交互（preset 应用 / 附加参数增删 / 回调）。
// 校验失败与提交成功流转由父容器 OAuthConfigsTab 实现（OAuthModal 为纯展示表单），
// 该流转在 OAuthConfigsTab.test.ts 覆盖（缺 ID/Client ID、缺 auth_url/token_url、提交成功、后端 5xx）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import OAuthModal from './OAuthModal.svelte';
import { handleBackdropKeydown, oauthModalFieldId } from '../helpers';
import type { OAuthFormState, OAuthItem } from '../types';

function baseForm(overrides: Partial<OAuthFormState> = {}): OAuthFormState {
  return {
    id: '',
    vendor_id: '',
    label: '',
    grant_type: 'client_credentials',
    auth_url: '',
    token_url: '',
    client_id: '',
    client_secret: '',
    scopes: '',
    auth_extra_params: [],
    max_concurrent: '5',
    weight: '1',
    max_input_chars: '0',
    max_file_size_mb: '100',
    token_field: 'access_token',
    ...overrides,
  };
}

const editingOAuth: OAuthItem = {
  id: 'oauth-main',
  vendor_id: 'vendor-a',
  label: 'Main OAuth',
  grant_type: 'client_credentials',
  client_id: 'client-1',
  scopes: '',
  has_token: false,
};

const presets = [
  { label: 'Google', params: { access_type: 'offline', prompt: 'consent' } },
  { label: 'Salesforce', params: { prompt: 'consent' } },
];

function noop() {}

function renderModal(props: Record<string, unknown>) {
  return render(OAuthModal, {
    props: {
      open: true,
      editingOAuth: null,
      oauthForm: baseForm(),
      authExtraPresets: presets,
      callbackUrl: 'http://localhost:8977/oauth/vendor/callback',
      oauthModalFieldId,
      handleBackdropKeydown,
      onApplyPreset: noop,
      onAddExtraParam: noop,
      onRemoveExtraParam: noop,
      onCopyCallbackUrl: noop,
      onSave: noop,
      onClose: noop,
      ...props,
    },
  });
}

describe('api-keys-page/modals/OAuthModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders create mode with basic fields', () => {
    renderModal({});

    expect(screen.getByText('创建 OAuth 配置')).toBeTruthy();
    expect(screen.getByPlaceholderText('Config ID（必填）')).toBeTruthy();
    expect(screen.getByPlaceholderText('Vendor ID（如 google、deepl）')).toBeTruthy();
    expect(screen.getByPlaceholderText('Client ID（必填）')).toBeTruthy();
    expect(screen.getByPlaceholderText('Client Secret（留空保留原值）')).toBeTruthy();
    expect(screen.getByPlaceholderText('Scopes（空格分隔，如 openid email）')).toBeTruthy();
    // client_credentials 分支：只有 token url，没有 authorization url / 附加参数块
    expect(screen.getByPlaceholderText('https://example.com/oauth/token')).toBeTruthy();
    expect(screen.queryByPlaceholderText('https://example.com/oauth/authorize')).toBeNull();
    expect(screen.queryByText('授权 URL 附加参数')).toBeNull();
    expect(screen.getByText('保存')).toBeTruthy();
    expect(screen.getByText('取消')).toBeTruthy();
  });

  it('shows authorization_code branch with callback url and extra params area', async () => {
    renderModal({ oauthForm: baseForm({ grant_type: 'authorization_code' }) });

    expect(screen.getByText(/将以下回调地址填入第三方平台/)).toBeTruthy();
    expect(screen.getByText('http://localhost:8977/oauth/vendor/callback')).toBeTruthy();
    expect(screen.getByPlaceholderText('https://example.com/oauth/authorize')).toBeTruthy();
    expect(screen.getByText('授权 URL 附加参数')).toBeTruthy();
    // 无附加参数空态 + preset 按钮
    expect(screen.getByText('无附加参数')).toBeTruthy();
    expect(screen.getByText('Google')).toBeTruthy();
    expect(screen.getByText('Salesforce')).toBeTruthy();
  });

  it('applies a preset, adds and removes extra params via callbacks', async () => {
    const onApplyPreset = vi.fn();
    const onAddExtraParam = vi.fn();
    const onRemoveExtraParam = vi.fn();
    renderModal({
      oauthForm: baseForm({
        grant_type: 'authorization_code',
        auth_extra_params: [{ key: 'access_type', value: 'offline' }],
      }),
      onApplyPreset,
      onAddExtraParam,
      onRemoveExtraParam,
    });

    await fireEvent.click(screen.getByText('Google'));
    expect(onApplyPreset).toHaveBeenCalledWith({ access_type: 'offline', prompt: 'consent' });

    await fireEvent.click(screen.getByText('添加'));
    expect(onAddExtraParam).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByTitle('删除'));
    expect(onRemoveExtraParam).toHaveBeenCalledWith(0);
  });

  it('renders edit mode without the config id input', () => {
    renderModal({
      editingOAuth,
      oauthForm: baseForm({ label: 'Main OAuth', client_id: 'client-1' }),
    });

    expect(screen.getByText('编辑 OAuth 配置')).toBeTruthy();
    expect(screen.queryByPlaceholderText('Config ID（必填）')).toBeNull();
    expect((screen.getByLabelText(/名称 \(label\)/) as HTMLInputElement).value).toBe('Main OAuth');
  });

  it('invokes copy/save/close callbacks', async () => {
    const onCopyCallbackUrl = vi.fn();
    const onSave = vi.fn();
    const onClose = vi.fn();
    renderModal({
      oauthForm: baseForm({ grant_type: 'authorization_code' }),
      onCopyCallbackUrl,
      onSave,
      onClose,
    });

    await fireEvent.click(screen.getByText('复制'));
    expect(onCopyCallbackUrl).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByText('保存'));
    expect(onSave).toHaveBeenCalledTimes(1);

    const backdrop = screen.getByLabelText('关闭 OAuth 配置弹窗');
    await fireEvent.keyDown(backdrop, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('renders nothing when open is false (empty state)', () => {
    renderModal({ open: false });

    expect(screen.queryByText('创建 OAuth 配置')).toBeNull();
    expect(screen.queryByLabelText('关闭 OAuth 配置弹窗')).toBeNull();
  });
});
