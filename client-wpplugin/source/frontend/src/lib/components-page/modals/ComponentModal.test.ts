// catalog: WEBUI-UI-ComponentModal
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功（创建/编辑 + openai_compatible 分支）/ 空态（open=false 不渲染）/
// 弹窗交互（preset 应用 / 保存 / 取消 / 背景关闭）。
// 校验失败与提交成功流转由父容器 MyComponentsTab 实现（ComponentModal 为纯展示表单），
// 该流转已在 MyComponentsTab.test.ts 覆盖（组件名称/ID/Server 模板 ID/API 地址/模型名称校验 + 创建成功）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import ComponentModal from './ComponentModal.svelte';
import { componentModalFieldId, handleBackdropKeydown } from '../helpers';
import type { ComponentFormState, LocalComp } from '../types';

function baseForm(overrides: Partial<ComponentFormState> = {}): ComponentFormState {
  return {
    id: '',
    name: '',
    template_id: '',
    vendor_id: '',
    vendor_name: '',
    kind: 'text',
    remarks: '',
    enabled: true,
    api_base: '',
    model: '',
    system_prompt: '',
    temperature: '0.1',
    max_tokens: '',
    response_path: '',
    ...overrides,
  };
}

const editingComp: LocalComp = {
  id: 'local-text',
  name: 'Local Text',
  template_id: 'official-text-v1',
  vendor_id: 'vendor-a',
  vendor_name: 'Vendor A',
  kind: 'text',
  remarks: 'prod',
  created_at: '1',
  enabled: true,
  versions: {},
};

const presets = [
  { label: 'DeepSeek', api_base: 'https://api.deepseek.com', model: 'deepseek-chat' },
  { label: 'OpenAI', api_base: 'https://api.openai.com', model: 'gpt-4o-mini' },
];

function noop() {}

function renderModal(props: Record<string, unknown>) {
  return render(ComponentModal, {
    props: {
      open: true,
      editingComp: null,
      compForm: baseForm(),
      presets,
      componentModalFieldId,
      handleBackdropKeydown,
      onApplyPreset: noop,
      onSave: noop,
      onClose: noop,
      ...props,
    },
  });
}

describe('components-page/modals/ComponentModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders create mode with id/name/kind and server template field for text kind', () => {
    renderModal({});

    expect(screen.getByText('创建组件')).toBeTruthy();
    expect(screen.getByLabelText('组件 ID')).toBeTruthy();
    expect(screen.getByLabelText('显示名称')).toBeTruthy();
    expect(screen.getByLabelText('组件类型')).toBeTruthy();
    expect(screen.getByLabelText('Server 模板 ID *')).toBeTruthy();
    expect(screen.getByLabelText('备注')).toBeTruthy();
    expect(screen.queryByLabelText('API 地址 *')).toBeNull();
    expect(screen.getByText('保存')).toBeTruthy();
    expect(screen.getByText('取消')).toBeTruthy();
  });

  it('switching kind to openai_compatible shows inline template fields and presets', async () => {
    const onApplyPreset = vi.fn();
    renderModal({ onApplyPreset });

    const kindSelect = screen.getByLabelText('组件类型') as unknown as HTMLSelectElement;
    await fireEvent.change(kindSelect, { target: { value: 'openai_compatible' } });

    expect(screen.getByText('当前类型使用本地 inline template，不关联官网组件模板 ID。')).toBeTruthy();
    expect(screen.getByText('快速选择提供商')).toBeTruthy();
    expect(screen.getByText('DeepSeek')).toBeTruthy();
    expect(screen.getByLabelText('API 地址 *')).toBeTruthy();
    expect(screen.getByLabelText('模型名称 *')).toBeTruthy();
    expect(screen.getByLabelText('System Prompt（可选）')).toBeTruthy();
    expect(screen.getByLabelText('Temperature')).toBeTruthy();
    expect(screen.getByLabelText('Response Path')).toBeTruthy();
    expect(screen.queryByLabelText('Server 模板 ID *')).toBeNull();

    await fireEvent.click(screen.getByText('DeepSeek'));
    expect(onApplyPreset).toHaveBeenCalledWith({
      label: 'DeepSeek',
      api_base: 'https://api.deepseek.com',
      model: 'deepseek-chat',
    });
  });

  it('renders edit mode without id/kind fields and prefilled name', () => {
    renderModal({
      editingComp,
      compForm: baseForm({ name: 'Local Text', remarks: 'prod' }),
    });

    expect(screen.getByText('编辑组件')).toBeTruthy();
    expect(screen.queryByLabelText('组件 ID')).toBeNull();
    expect(screen.queryByLabelText('组件类型')).toBeNull();
    expect((screen.getByLabelText('显示名称') as HTMLInputElement).value).toBe('Local Text');
    expect((screen.getByLabelText('备注') as HTMLInputElement).value).toBe('prod');
  });

  it('invokes save and backdrop-close callbacks', async () => {
    const onSave = vi.fn();
    const onClose = vi.fn();
    renderModal({ onSave, onClose });

    await fireEvent.click(screen.getByText('保存'));
    expect(onSave).toHaveBeenCalledTimes(1);

    const backdrop = screen.getByLabelText('关闭组件编辑弹窗');
    await fireEvent.keyDown(backdrop, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('renders nothing when open is false (empty state)', () => {
    renderModal({ open: false });

    expect(screen.queryByText('创建组件')).toBeNull();
    expect(screen.queryByLabelText('关闭组件编辑弹窗')).toBeNull();
  });
});
