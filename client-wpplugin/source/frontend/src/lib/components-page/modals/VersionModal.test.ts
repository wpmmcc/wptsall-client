// catalog: WEBUI-UI-VersionModal
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功（添加/编辑两形态 + 关键交互元素）/ 空态（open=false 或
// component=null 不渲染）/ 弹窗交互（保存携带 compId、策略/Auth 类型选择、取消、背景关闭）。
// 校验与提交成功流转由父容器 MyComponentsTab 实现，已在 MyComponentsTab.test.ts 覆盖
//（版本编辑含 proxy profile / auth type 更新成功）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import VersionModal from './VersionModal.svelte';
import { handleBackdropKeydown, versionModalFieldId } from '../helpers';
import type { LocalComp, VersionFormState } from '../types';

function baseForm(overrides: Partial<VersionFormState> = {}): VersionFormState {
  return {
    version: '',
    key_ids: '',
    key_selection_strategy: 'round_robin',
    auth_type: 'key',
    proxy_profile_id: '',
    remarks: '',
    ...overrides,
  };
}

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

function noop() {}

function renderModal(props: Record<string, unknown>) {
  return render(VersionModal, {
    props: {
      open: true,
      component,
      editingVersion: null,
      versionForm: baseForm(),
      versionModalFieldId,
      handleBackdropKeydown,
      onSave: noop,
      onClose: noop,
      ...props,
    },
  });
}

describe('components-page/modals/VersionModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders add mode with version/key/strategy fields', () => {
    renderModal({});

    expect(screen.getByText('添加版本 — Local Text')).toBeTruthy();
    expect(screen.getByLabelText('版本号')).toBeTruthy();
    expect(screen.getByLabelText('Key IDs')).toBeTruthy();
    expect(screen.getByLabelText('选择策略')).toBeTruthy();
    expect(screen.getByLabelText('Auth 类型')).toBeTruthy();
    expect(screen.getByLabelText('代理 Profile ID')).toBeTruthy();
    expect(screen.getByText('保存')).toBeTruthy();
    expect(screen.getByText('取消')).toBeTruthy();
  });

  it('renders edit mode showing the fixed version label and update button', () => {
    renderModal({
      editingVersion: { compId: 'local-text', ver: 'stable' },
      versionForm: baseForm({
        key_ids: 'key-main',
        key_selection_strategy: 'weighted',
        auth_type: 'oauth',
      }),
    });

    expect(screen.getByText('编辑版本 — Local Text')).toBeTruthy();
    // 编辑模式：版本号固定展示，不出现版本号输入
    expect(screen.getByText(/版本号：\s*stable/)).toBeTruthy();
    expect(screen.queryByLabelText('版本号')).toBeNull();
    expect(screen.queryByText('保存')).toBeNull();
    expect(screen.getByText('更新')).toBeTruthy();
    expect(
      (screen.getByLabelText('Key IDs') as HTMLInputElement).value,
    ).toBe('key-main');
  });

  it('invokes save with the component id and closes via backdrop', async () => {
    const onSave = vi.fn();
    const onClose = vi.fn();
    renderModal({ onSave, onClose });

    await fireEvent.click(screen.getByText('保存'));
    expect(onSave).toHaveBeenCalledWith('local-text');

    const backdrop = screen.getByLabelText('关闭版本编辑弹窗');
    await fireEvent.keyDown(backdrop, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('binds strategy and auth type selects', async () => {
    renderModal({});

    const strategy = screen.getByLabelText('选择策略') as unknown as HTMLSelectElement;
    await fireEvent.change(strategy, { target: { value: 'weighted' } });
    expect(strategy.value).toBe('weighted');

    const authType = screen.getByLabelText('Auth 类型') as unknown as HTMLSelectElement;
    await fireEvent.change(authType, { target: { value: 'oauth' } });
    expect(authType.value).toBe('oauth');
  });

  it('renders nothing when open is false or component is null (empty state)', () => {
    const { unmount } = renderModal({ open: false });
    expect(screen.queryByText(/添加版本/)).toBeNull();
    expect(screen.queryByLabelText('关闭版本编辑弹窗')).toBeNull();
    unmount();

    renderModal({ component: null });
    expect(screen.queryByText(/添加版本/)).toBeNull();
    expect(screen.queryByLabelText('关闭版本编辑弹窗')).toBeNull();
  });
});
