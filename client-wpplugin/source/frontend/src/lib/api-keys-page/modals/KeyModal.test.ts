// catalog: WEBUI-UI-KeyModal
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功（创建/编辑两形态 + 关键交互元素）/ 空态（open=false 不渲染）/
// 弹窗交互（保存/取消/背景关闭回调）。
// 校验失败与提交成功流转由父容器 VendorKeysTab 实现（KeyModal 为纯展示表单），
// 该流转在 VendorKeysTab.test.ts 覆盖（Auth JSON 非法 / 缺 Key ID / 提交成功）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import KeyModal from './KeyModal.svelte';
import { handleBackdropKeydown, keyModalFieldId } from '../helpers';
import type { KeyFormState, VendorKeyItem } from '../types';

function baseForm(overrides: Partial<KeyFormState> = {}): KeyFormState {
  return {
    id: '',
    vendor_id: '',
    label: '',
    auth_json: '{}',
    max_concurrent: '5',
    requests_per_second: '3',
    weight: '1',
    enabled: true,
    max_input_chars: '0',
    max_file_size_mb: '100',
    ...overrides,
  };
}

const editingKey: VendorKeyItem = {
  id: 'key-main',
  vendor_id: 'vendor-a',
  label: 'Main Key',
  auth_keys: ['api_key'],
  max_concurrent: 5,
  requests_per_second: 3,
  weight: 1,
  enabled: false,
};

function noop() {}

function renderModal(props: Record<string, unknown>) {
  return render(KeyModal, {
    props: {
      open: true,
      editingKey: null,
      keyForm: baseForm(),
      keyModalFieldId,
      handleBackdropKeydown,
      onSave: noop,
      onClose: noop,
      ...props,
    },
  });
}

describe('api-keys-page/modals/KeyModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders create mode with id/vendor inputs and save/cancel', () => {
    renderModal({});

    expect(screen.getByText('添加 Vendor Key')).toBeTruthy();
    expect(screen.getByPlaceholderText('Key ID（必填）')).toBeTruthy();
    expect(screen.getByPlaceholderText('Vendor ID（必填）')).toBeTruthy();
    expect(screen.getByPlaceholderText('便于识别的显示名称')).toBeTruthy();
    expect(screen.getByPlaceholderText('{ "api_key": "sk-xxx" }')).toBeTruthy();
    expect(screen.getByText('保存')).toBeTruthy();
    expect(screen.getByText('取消')).toBeTruthy();
    expect(screen.getByLabelText('并发上限')).toBeTruthy();
    expect(screen.getByLabelText('最大输入字符数')).toBeTruthy();
    expect(screen.getByLabelText('最大文件大小 (MB)')).toBeTruthy();
    expect(screen.getByText('启用')).toBeTruthy();
  });

  it('renders edit mode without id/vendor inputs and prefilled label', () => {
    renderModal({
      editingKey,
      keyForm: baseForm({ label: 'Main Key', enabled: false }),
    });

    expect(screen.getByText('编辑 Vendor Key')).toBeTruthy();
    expect(screen.queryByPlaceholderText('Key ID（必填）')).toBeNull();
    expect(screen.queryByPlaceholderText('Vendor ID（必填）')).toBeNull();
    expect((screen.getByPlaceholderText('便于识别的显示名称') as HTMLInputElement).value).toBe(
      'Main Key',
    );
  });

  it('invokes onSave and onClose callbacks from the footer buttons', async () => {
    const onSave = vi.fn();
    const onClose = vi.fn();
    renderModal({ onSave, onClose });

    await fireEvent.click(screen.getByText('保存'));
    expect(onSave).toHaveBeenCalledTimes(1);

    cleanup();
    const onClose2 = vi.fn();
    renderModal({ onClose: onClose2 });
    await fireEvent.click(screen.getByText('取消'));
    expect(onClose2).toHaveBeenCalledTimes(1);
  });

  it('closes via backdrop click and escape keydown', async () => {
    const onClose = vi.fn();
    renderModal({ onClose });

    const backdrop = screen.getByLabelText('关闭 Vendor Key 弹窗');
    await fireEvent.click(backdrop);
    expect(onClose).toHaveBeenCalledTimes(1);

    await fireEvent.keyDown(backdrop, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(2);
  });

  it('renders nothing when open is false (empty state)', () => {
    renderModal({ open: false });

    expect(screen.queryByText('添加 Vendor Key')).toBeNull();
    expect(screen.queryByLabelText('关闭 Vendor Key 弹窗')).toBeNull();
  });
});
