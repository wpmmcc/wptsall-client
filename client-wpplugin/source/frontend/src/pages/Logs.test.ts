// catalog: WEBUI-UI-Logs
// oracle: L2
// 状态矩阵：共享 helper 加载 + 关键字过滤；后端失败 toast / 网络错误 toast。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, cleanup } from '@testing-library/svelte';
import Logs from './Logs.svelte';
import * as toastModule from '../lib/stores/toast';
import { loadRecentLogs } from '../lib/api/settings';

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

vi.mock('../lib/api/settings', () => ({
  loadRecentLogs: vi.fn(),
}));

describe('pages/Logs', () => {
  beforeEach(() => {
    vi.mocked(toastModule.showToast).mockClear();
    vi.mocked(loadRecentLogs).mockReset();
  });

  afterEach(() => {
    cleanup();
  });

  it('loads logs through shared helper and filters lines by keyword', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: true,
      data: {
        limit: 200,
        lines: [
          JSON.stringify({ ts: 1700000000, level: 'info', event: 'worker.run' }),
          'plain line keep-me',
        ],
      },
    });

    render(Logs);

    await fireEvent.click(screen.getByText('加载日志'));

    await waitFor(() => {
      expect(loadRecentLogs).toHaveBeenCalledWith(200);
    });

    expect(screen.getByText(/显示 2 \/ 2 行/)).toBeTruthy();
    expect(screen.getByText((content) => content.includes('worker.run'))).toBeTruthy();
    expect(screen.getByText((content) => content.includes('plain line keep-me'))).toBeTruthy();

    await fireEvent.input(screen.getByPlaceholderText('关键词过滤...'), {
      target: { value: 'keep-me' },
    });

    expect(screen.getByText(/显示 1 \/ 2 行/)).toBeTruthy();
  });

  it('shows toast when backend returns failure', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: false,
      error: { code: 'LOGS_FETCH_FAILED', message: 'backend failed' },
    });

    render(Logs);
    await fireEvent.click(screen.getByText('加载日志'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '加载日志失败',
        'backend failed',
      );
    });
  });

  it('shows network toast when shared helper returns network error', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: false,
      error: { code: 'NETWORK_ERROR', message: 'network down' },
    });

    render(Logs);
    await fireEvent.click(screen.getByText('加载日志'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '网络错误',
        expect.stringContaining('network down'),
      );
    });
  });
});
