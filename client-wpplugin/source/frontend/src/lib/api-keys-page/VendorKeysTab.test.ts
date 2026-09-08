// catalog: WEBUI-UI-VendorKeysTab
// oracle: L2
// 状态矩阵（计划 §6.0）：装载成功 / 空态 / 失败态（后端 5xx 回显）；
// 表单（内嵌 KeyModal）：校验失败（Auth JSON 非法 / 缺 Key ID）+ 提交成功流转。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import VendorKeysTab from './VendorKeysTab.svelte';
import * as keysApi from '../api/keys';
import * as toastModule from '../stores/toast';

vi.mock('../api/keys', () => ({
  listVendorKeys: vi.fn(),
  createVendorKey: vi.fn(),
  updateVendorKey: vi.fn(),
  deleteVendorKey: vi.fn(),
}));

vi.mock('../stores/toast', () => ({
  showToast: vi.fn(),
}));

const keyItem = {
  id: 'key-main',
  vendor_id: 'vendor-a',
  label: 'Main Key',
  auth_keys: ['api_key'],
  max_concurrent: 5,
  requests_per_second: 3,
  weight: 1,
  enabled: true,
  max_input_chars: 0,
  max_file_size_mb: 0,
};

function mockList(items = [keyItem]) {
  vi.mocked(keysApi.listVendorKeys).mockResolvedValue({
    success: true,
    data: { items },
  } as never);
}

describe('api-keys-page/VendorKeysTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    mockList();
  });

  afterEach(() => {
    cleanup();
  });

  it('loads vendor key rows on mount', async () => {
    render(VendorKeysTab);

    expect(await screen.findByText('Main Key')).toBeTruthy();
    expect(screen.getByText('key-main')).toBeTruthy();
    expect(screen.getByText('vendor-a')).toBeTruthy();
    expect(screen.getByText('启用')).toBeTruthy();
    expect(screen.getByText('+ 添加 Key')).toBeTruthy();
    expect(keysApi.listVendorKeys).toHaveBeenCalledWith(undefined);
  });

  it('shows empty state when no keys exist', async () => {
    mockList([]);
    render(VendorKeysTab);

    expect(await screen.findByText('暂无 Key，点击「添加 Key」')).toBeTruthy();
  });

  it('rejects create when auth JSON is invalid', async () => {
    render(VendorKeysTab);
    await screen.findByText('Main Key');

    await fireEvent.click(screen.getByText('+ 添加 Key'));
    expect(await screen.findByText('添加 Vendor Key')).toBeTruthy();

    const authJson = screen.getByPlaceholderText('{ "api_key": "sk-xxx" }');
    await fireEvent.input(authJson, { target: { value: '{invalid' } });
    await fireEvent.click(screen.getByText('保存'));

    expect(toastModule.showToast).toHaveBeenCalledWith('error', 'Auth JSON 格式错误');
    expect(keysApi.createVendorKey).not.toHaveBeenCalled();
  });

  it('rejects create when key id is missing', async () => {
    render(VendorKeysTab);
    await screen.findByText('Main Key');

    await fireEvent.click(screen.getByText('+ 添加 Key'));
    expect(await screen.findByText('添加 Vendor Key')).toBeTruthy();

    await fireEvent.click(screen.getByText('保存'));

    expect(toastModule.showToast).toHaveBeenCalledWith('error', '请填写 Key ID');
    expect(keysApi.createVendorKey).not.toHaveBeenCalled();
  });

  it('creates a vendor key and reloads the list on success', async () => {
    vi.mocked(keysApi.createVendorKey).mockResolvedValue({
      success: true,
      data: { id: 'key-new' },
    } as never);

    render(VendorKeysTab);
    await screen.findByText('Main Key');

    await fireEvent.click(screen.getByText('+ 添加 Key'));
    expect(await screen.findByText('添加 Vendor Key')).toBeTruthy();

    await fireEvent.input(screen.getByPlaceholderText('Key ID（必填）'), {
      target: { value: 'key-new' },
    });
    await fireEvent.input(screen.getByPlaceholderText('Vendor ID（必填）'), {
      target: { value: 'vendor-a' },
    });
    await fireEvent.input(screen.getByPlaceholderText('便于识别的显示名称'), {
      target: { value: 'New Key' },
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(keysApi.createVendorKey).toHaveBeenCalledWith({
        id: 'key-new',
        vendor_id: 'vendor-a',
        label: 'New Key',
        auth_values: {},
        max_concurrent: 5,
        requests_per_second: 3,
        weight: 1,
        enabled: true,
        max_input_chars: 0,
        max_file_size_mb: 100,
      });
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Key 已创建');
    await waitFor(() => {
      // modal closes back to the table
      expect(screen.getByText('Main Key')).toBeTruthy();
    });
    expect(keysApi.listVendorKeys).toHaveBeenCalledTimes(2);
  });

  it('shows backend error toast when create fails', async () => {
    vi.mocked(keysApi.createVendorKey).mockResolvedValue({
      success: false,
      error: { code: 'INTERNAL', message: 'server 5xx: duplicate key', status: 500 },
    } as never);

    render(VendorKeysTab);
    await screen.findByText('Main Key');

    await fireEvent.click(screen.getByText('+ 添加 Key'));
    expect(await screen.findByText('添加 Vendor Key')).toBeTruthy();

    await fireEvent.input(screen.getByPlaceholderText('Key ID（必填）'), {
      target: { value: 'key-dup' },
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '创建失败',
        'server 5xx: duplicate key',
      );
    });
    // modal stays open on failure
    expect(screen.getByText('添加 Vendor Key')).toBeTruthy();
  });

  it('opens edit modal prefilled and updates the key on success', async () => {
    vi.mocked(keysApi.updateVendorKey).mockResolvedValue({
      success: true,
      data: { id: 'key-main' },
    } as never);

    render(VendorKeysTab);
    await screen.findByText('Main Key');

    await fireEvent.click(screen.getByText('编辑'));
    expect(await screen.findByText('编辑 Vendor Key')).toBeTruthy();
    // editing mode hides id/vendor inputs
    expect(screen.queryByPlaceholderText('Key ID（必填）')).toBeNull();
    expect((screen.getByPlaceholderText('便于识别的显示名称') as HTMLInputElement).value).toBe(
      'Main Key',
    );

    await fireEvent.input(screen.getByPlaceholderText('便于识别的显示名称'), {
      target: { value: 'Renamed Key' },
    });
    await fireEvent.click(screen.getByText('保存'));

    await waitFor(() => {
      expect(keysApi.updateVendorKey).toHaveBeenCalledWith('key-main', {
        label: 'Renamed Key',
        auth_values: undefined,
        max_concurrent: 5,
        requests_per_second: 3,
        weight: 1,
        enabled: true,
        max_input_chars: 0,
        max_file_size_mb: 0,
      });
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Key 已更新');
  });

  it('deletes a vendor key and reloads the list', async () => {
    vi.mocked(keysApi.deleteVendorKey).mockResolvedValue({
      success: true,
    } as never);

    render(VendorKeysTab);
    await screen.findByText('Main Key');

    await fireEvent.click(screen.getByText('删除'));

    await waitFor(() => {
      expect(keysApi.deleteVendorKey).toHaveBeenCalledWith('key-main');
      expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Key 已删除');
    });
    expect(keysApi.listVendorKeys).toHaveBeenCalledTimes(2);
  });
});
