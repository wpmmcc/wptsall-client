// catalog: WEBUI-UI-AuthBindingModal
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功（字段编辑 + 认证池展开 + 覆盖参数区）/
// 空态（open=false 或 component=null 不渲染）/
// 失败态（模板未声明 Key/OAuth 绑定 → 警告 + 控件禁用；无 Vendor ID 警告；资源加载中态）。
// 校验与提交成功流转由父容器 MyComponentsTab 实现，已在 MyComponentsTab.test.ts 覆盖
//（编辑 Auth → 保存 → upsertBinding 成功 + fetchStatus）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import AuthBindingModal from './AuthBindingModal.svelte';
import { authModalFieldId, handleBackdropKeydown } from '../helpers';
import type { LocalComp, OAuthItem, VendorKeyItem } from '../types';

const component: LocalComp = {
  id: 'local-text',
  name: 'Local Text',
  template_id: 'official-text-v1',
  vendor_id: 'vendor-a',
  vendor_name: 'Vendor A',
  kind: 'text',
  remarks: '',
  created_at: '1',
  enabled: true,
  versions: {},
};

const availableKeys: VendorKeyItem[] = [
  {
    id: 'key-1',
    vendor_id: 'vendor-a',
    label: 'Primary Key',
    auth_keys: ['api_key'],
    max_concurrent: 5,
    requests_per_second: 3,
    weight: 1,
    enabled: true,
  },
];

const availableOAuth: OAuthItem[] = [
  {
    id: 'oauth-1',
    vendor_id: 'vendor-a',
    label: 'Main OAuth',
    grant_type: 'client_credentials',
    client_id: 'client-1',
    scopes: '',
    has_token: true,
  },
];

function noop() {}

function renderModal(props: Record<string, unknown>) {
  return render(AuthBindingModal, {
    props: {
      open: true,
      component,
      authModalSupportedModes: ['key', 'oauth'],
      authFields: [{ key: 'api_key', value: 'sk-1' }],
      editableParamPaths: ['request.url', 'constraints.max_input_chars'],
      overrideRequestUrl: '',
      overrideRequestHeadersJson: '',
      overrideRequestBodyJson: '',
      overrideMaxInputChars: '',
      overrideRateLimitQps: '',
      overrideMaxConcurrentRequests: '',
      overrideMaxFileSizeMb: '',
      authPoolKeyIds: [],
      authPoolOAuthIds: [],
      authPoolStrategy: 'RoundRobin',
      authPoolExpanded: false,
      availableKeys,
      availableOAuthConfigs: availableOAuth,
      poolResourcesLoading: false,
      keyDropdownOpen: false,
      oauthDropdownOpen: false,
      authModeEnabled: (mode: string) => mode === 'key' || mode === 'oauth',
      toggleKeyId: noop,
      toggleOAuthId: noop,
      isEditableParam: (path: string) =>
        ['request.url', 'constraints.max_input_chars'].includes(path),
      authModalFieldId,
      handleBackdropKeydown,
      onLoadPoolResources: noop,
      onSave: noop,
      onClear: noop,
      onClose: noop,
      ...props,
    },
  });
}

describe('components-page/modals/AuthBindingModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders auth field editor with add-field and save/clear/cancel', () => {
    renderModal({});

    expect(screen.getByText(/Auth 凭据 — Local Text/)).toBeTruthy();
    const keyInput = screen.getByPlaceholderText('字段名（key）') as HTMLInputElement;
    expect(keyInput.value).toBe('api_key');
    expect((screen.getByPlaceholderText('值（value）') as HTMLInputElement).value).toBe('sk-1');
    expect(screen.getByText('+ 添加字段')).toBeTruthy();
    expect(screen.getByText('保存')).toBeTruthy();
    expect(screen.getByText('清除绑定')).toBeTruthy();
    expect(screen.getByText('取消')).toBeTruthy();
  });

  it('expands pool config and renders vendor/mode info with dropdown toggles', async () => {
    const toggleKeyId = vi.fn();
    renderModal({ authPoolExpanded: true, toggleKeyId });

    expect(screen.getByText('动态认证池配置')).toBeTruthy();
    expect(screen.getByText(/当前 Vendor/)).toBeTruthy();
    expect(screen.getByText('vendor-a')).toBeTruthy();
    expect(screen.getByText('key')).toBeTruthy();
    expect(screen.getByText('oauth')).toBeTruthy();
    expect(screen.getByText('选择 API Key...')).toBeTruthy();
    expect(screen.getByText('选择 OAuth 配置...')).toBeTruthy();
    // label 文案 = "{$_('auth_modal.round_robin')} (RoundRobin)" → 双后缀
    expect(screen.getByText(/轮询 \(RoundRobin\).*RoundRobin/)).toBeTruthy();
    expect(screen.getByText(/随机 \(Random\).*Random/)).toBeTruthy();
    expect(screen.getByText(/加权 \(Weighted\).*Weighted/)).toBeTruthy();

    await fireEvent.click(screen.getByText('选择 API Key...'));
    expect(await screen.findByText('Primary Key')).toBeTruthy();
    await fireEvent.click(screen.getByText('Primary Key'));
    expect(toggleKeyId).toHaveBeenCalledWith('key-1');
  });

  it('shows editable param area only for template-declared paths', () => {
    renderModal({});

    expect(screen.getByText('模板允许覆盖参数')).toBeTruthy();
    expect(screen.getByLabelText('request.url')).toBeTruthy();
    expect(screen.getByLabelText('constraints.max_input_chars')).toBeTruthy();
    // 未声明路径不渲染
    expect(screen.queryByLabelText('request.headers')).toBeNull();
    expect(screen.queryByLabelText('request.body')).toBeNull();
  });

  it('shows unsupported-mode warning and disables pool dropdown (failure state)', async () => {
    renderModal({
      authPoolExpanded: true,
      authModalSupportedModes: ['key'],
      authModeEnabled: (mode: string) => mode === 'key',
    });

    expect(screen.getByText('当前模板未声明支持 OAuth 绑定')).toBeTruthy();
    const oauthToggle = screen.getByText('选择 OAuth 配置...').closest('button') as HTMLButtonElement;
    expect(oauthToggle.disabled).toBe(true);
    expect(screen.queryByText('当前模板未声明支持 Key 绑定')).toBeNull();
  });

  it('warns when component has no vendor id', async () => {
    renderModal({
      authPoolExpanded: true,
      component: { ...component, vendor_id: '' },
    });

    expect(
      screen.getByText(/当前组件还没有 Vendor ID，Key \/ OAuth 资源池绑定会被拒绝/),
    ).toBeTruthy();
  });

  it('triggers pool resource load on first expand and shows loading state', async () => {
    const onLoadPoolResources = vi.fn();
    const { unmount } = renderModal({
      availableKeys: [],
      availableOAuthConfigs: [],
      onLoadPoolResources,
    });

    await fireEvent.click(screen.getByText('动态认证池配置'));
    await waitFor(() => {
      expect(onLoadPoolResources).toHaveBeenCalledTimes(1);
    });
    expect(screen.getByText('暂无可用 Key')).toBeTruthy();
    expect(screen.getByText('暂无可用 OAuth 配置')).toBeTruthy();
    unmount();

    renderModal({
      authPoolExpanded: true,
      availableKeys: [],
      availableOAuthConfigs: [],
      poolResourcesLoading: true,
    });
    expect(screen.getByText('加载中...')).toBeTruthy();
  });

  it('invokes save/clear/close callbacks', async () => {
    const onSave = vi.fn();
    const onClear = vi.fn();
    const onClose = vi.fn();
    renderModal({ onSave, onClear, onClose });

    await fireEvent.click(screen.getByText('保存'));
    expect(onSave).toHaveBeenCalledTimes(1);
    await fireEvent.click(screen.getByText('清除绑定'));
    expect(onClear).toHaveBeenCalledTimes(1);

    const backdrop = screen.getByLabelText('关闭组件认证弹窗');
    await fireEvent.keyDown(backdrop, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('renders nothing when open is false or component is null (empty state)', () => {
    const { unmount } = renderModal({ open: false });
    expect(screen.queryByText(/Auth 凭据/)).toBeNull();
    expect(screen.queryByLabelText('关闭组件认证弹窗')).toBeNull();
    unmount();

    renderModal({ component: null });
    expect(screen.queryByText(/Auth 凭据/)).toBeNull();
    expect(screen.queryByLabelText('关闭组件认证弹窗')).toBeNull();
  });
});
