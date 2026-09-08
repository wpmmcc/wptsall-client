// catalog: WEBUI-UI-FileTestModal
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功（表单 + 成功结果展示）/ 空态（open=false 不渲染）/
// 失败态（错误回显）+ 加载中态（按钮禁用）。
// 校验/提交流转由父容器 MyComponentsTab 实现，已在 MyComponentsTab.test.ts 覆盖
//（文件 URL/语言预填、后端结构化错误、请求异常）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import FileTestModal from './FileTestModal.svelte';
import { handleBackdropKeydown, testFileFieldId } from '../helpers';
import type { FileTestModalState } from '../types';

function baseState(overrides: Partial<FileTestModalState> = {}): FileTestModalState {
  return {
    open: true,
    componentId: 'local-image',
    componentName: 'Local Image',
    fileUrl: 'https://example.com/a.png',
    sourceLang: 'en',
    targetLang: 'zh-CN',
    loading: false,
    result: null,
    error: null,
    ...overrides,
  };
}

function noop() {}

function renderModal(props: Record<string, unknown>) {
  return render(FileTestModal, {
    props: {
      testFileModal: baseState(),
      testFileFieldId,
      handleBackdropKeydown,
      onRun: noop,
      onClose: noop,
      ...props,
    },
  });
}

describe('components-page/modals/FileTestModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('renders the form with component name and run/cancel buttons', () => {
    renderModal({});

    expect(screen.getByText('文件翻译测试')).toBeTruthy();
    expect(screen.getByText('Local Image')).toBeTruthy();
    expect(screen.getByLabelText('文件 URL')).toBeTruthy();
    expect(screen.getByLabelText('源语言')).toBeTruthy();
    expect(screen.getByLabelText('目标语言')).toBeTruthy();
    expect(screen.getByText('运行测试')).toBeTruthy();
    expect(screen.getByText('取消')).toBeTruthy();
    expect(
      (screen.getByLabelText('文件 URL') as HTMLInputElement).value,
    ).toBe('https://example.com/a.png');
  });

  it('shows the success result box with translated ref and text', () => {
    renderModal({
      testFileModal: baseState({
        result: {
          translated_ref: 'https://cdn.example.com/out.png',
          translated_text: '图像已翻译',
          elapsed_ms: 321,
        },
      }),
    });

    expect(screen.getByText('测试成功（321ms）')).toBeTruthy();
    expect(screen.getByText('https://cdn.example.com/out.png')).toBeTruthy();
    expect(screen.getByText('图像已翻译')).toBeTruthy();
  });

  it('shows the failure box with the error message', () => {
    renderModal({
      testFileModal: baseState({ error: 'source file expired' }),
    });

    expect(screen.getByText('测试失败')).toBeTruthy();
    expect(screen.getByText('source file expired')).toBeTruthy();
  });

  it('disables buttons and shows running text while loading', () => {
    renderModal({ testFileModal: baseState({ loading: true }) });

    const runButton = screen.getByText('测试中...').closest('button') as HTMLButtonElement;
    expect(runButton.disabled).toBe(true);
    expect((screen.getByText('取消').closest('button') as HTMLButtonElement).disabled).toBe(true);
  });

  it('invokes onRun and onClose callbacks', async () => {
    const onRun = vi.fn();
    const onClose = vi.fn();
    renderModal({ onRun, onClose });

    await fireEvent.click(screen.getByText('运行测试'));
    expect(onRun).toHaveBeenCalledTimes(1);

    const backdrop = screen.getByLabelText('关闭文件翻译测试弹窗');
    await fireEvent.keyDown(backdrop, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('renders nothing when open is false (empty state)', () => {
    renderModal({ testFileModal: baseState({ open: false }) });

    expect(screen.queryByText('文件翻译测试')).toBeNull();
    expect(screen.queryByLabelText('关闭文件翻译测试弹窗')).toBeNull();
  });
});
