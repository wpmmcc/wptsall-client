// catalog: WEBUI-UI-Logs
// oracle: L2
// 状态矩阵：共享 helper 加载 + 关键字/级别/事件前缀过滤 + 下载 + 清空 + 自动刷新；后端失败 toast / 网络错误 toast。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, cleanup } from '@testing-library/svelte';
import Logs from './Logs.svelte';
import * as toastModule from '../lib/stores/toast';
import { clearLogs, loadRecentLogs } from '../lib/api/settings';
import { withEnglishLocale } from '../lib/test-en-locale';

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

vi.mock('../lib/api/settings', () => ({
  loadRecentLogs: vi.fn(),
  clearLogs: vi.fn(),
}));

describe('pages/Logs', () => {
  beforeEach(() => {
    vi.mocked(toastModule.showToast).mockClear();
    vi.mocked(loadRecentLogs).mockReset();
    vi.mocked(clearLogs).mockReset();
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

    await fireEvent.click(screen.getByTestId('logs-load'));

    await waitFor(() => {
      expect(loadRecentLogs).toHaveBeenCalledWith(200, undefined, undefined, '');
    });

    expect(screen.getByText(/显示 2 \/ 2 行/)).toBeTruthy();
    expect(screen.getByText((content) => content.includes('worker.run'))).toBeTruthy();
    expect(screen.getByText((content) => content.includes('plain line keep-me'))).toBeTruthy();

    await fireEvent.input(screen.getByTestId('logs-keyword-filter'), {
      target: { value: 'keep-me' },
    });

    expect(screen.getByText(/显示 1 \/ 2 行/)).toBeTruthy();
  });

  it('passes min_level when level filter is not all', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: true,
      data: { limit: 200, lines: [] },
    });

    render(Logs);
    await fireEvent.change(screen.getByTestId('logs-level-filter'), {
      target: { value: 'error' },
    });
    await fireEvent.click(screen.getByTestId('logs-load'));

    await waitFor(() => {
      expect(loadRecentLogs).toHaveBeenCalledWith(200, 'error', undefined, '');
    });
  });

  it('loads older page when has_more is true', async () => {
    vi.mocked(loadRecentLogs)
      .mockResolvedValueOnce({
        success: true,
        data: {
          limit: 200,
          lines: [JSON.stringify({ ts_ms: 3000, level: 'info', event: 'page-newer-evt' })],
          next_before_ts_ms: 3000,
          has_more: true,
        },
      })
      .mockResolvedValueOnce({
        success: true,
        data: {
          limit: 200,
          lines: [JSON.stringify({ ts_ms: 1000, level: 'info', event: 'page-older-evt' })],
          next_before_ts_ms: 1000,
          has_more: false,
        },
      });

    render(Logs);
    await fireEvent.click(screen.getByTestId('logs-load'));
    await waitFor(() => expect(screen.getByText(/page-newer-evt/)).toBeTruthy());
    expect((screen.getByTestId('logs-load-older') as HTMLButtonElement).disabled).toBe(false);
    await fireEvent.click(screen.getByTestId('logs-load-older'));
    await waitFor(() => {
      expect(loadRecentLogs).toHaveBeenLastCalledWith(200, undefined, 3000, '');
    });
    expect(screen.getByText(/page-older-evt/)).toBeTruthy();
  });

  it('enables download after lines load', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: true,
      data: {
        limit: 200,
        lines: [JSON.stringify({ ts: 1, level: 'info', event: 'x' })],
      },
    });
    const click = vi.fn();
    const createObjectURL = vi.fn(() => 'blob:test');
    const revokeObjectURL = vi.fn();
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL });
    const origCreate = document.createElement.bind(document);
    vi.spyOn(document, 'createElement').mockImplementation((tag: string) => {
      const el = origCreate(tag);
      if (tag === 'a') {
        Object.defineProperty(el, 'click', { value: click });
      }
      return el;
    });

    render(Logs);
    expect((screen.getByTestId('logs-download') as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(screen.getByTestId('logs-load'));
    await waitFor(() =>
      expect((screen.getByTestId('logs-download') as HTMLButtonElement).disabled).toBe(false),
    );
    await fireEvent.click(screen.getByTestId('logs-download'));
    expect(createObjectURL).toHaveBeenCalled();
    expect(click).toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it('shows toast when backend returns failure', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: false,
      error: { code: 'LOGS_FETCH_FAILED', message: 'backend failed' },
    });

    render(Logs);
    await fireEvent.click(screen.getByTestId('logs-load'));

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
    await fireEvent.click(screen.getByTestId('logs-load'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '网络错误',
        expect.stringContaining('network down'),
      );
    });
  });

  it('passes the event-name prefix filter to the helper', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: true,
      data: { limit: 200, lines: [] },
    });

    render(Logs);
    await fireEvent.input(screen.getByTestId('logs-event-filter'), {
      target: { value: 'review.' },
    });
    await fireEvent.click(screen.getByTestId('logs-load'));

    await waitFor(() => {
      expect(loadRecentLogs).toHaveBeenCalledWith(200, undefined, undefined, 'review.');
    });
  });

  it('clears the log file and resets the view', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: true,
      data: {
        limit: 200,
        lines: [JSON.stringify({ ts: 1, level: 'info', event: 'worker.run' })],
      },
    });
    vi.mocked(clearLogs).mockResolvedValue({
      success: true,
      data: { cleared: true },
    });

    render(Logs);
    await fireEvent.click(screen.getByTestId('logs-load'));
    await waitFor(() => expect(screen.getByText(/worker.run/)).toBeTruthy());

    await fireEvent.click(screen.getByTestId('logs-clear'));

    await waitFor(() => {
      expect(clearLogs).toHaveBeenCalledTimes(1);
      expect(toastModule.showToast).toHaveBeenCalledWith('success', '日志已清空');
    });
    await waitFor(() => {
      expect(screen.getByText(/点击「加载日志」/)).toBeTruthy();
    });
  });

  it('shows toast when clear fails', async () => {
    vi.mocked(loadRecentLogs).mockResolvedValue({
      success: true,
      data: { limit: 200, lines: [] },
    });
    vi.mocked(clearLogs).mockResolvedValue({
      success: false,
      error: { code: 'LOG_CLEAR_FAILED', message: 'permission denied' },
    });

    render(Logs);
    await fireEvent.click(screen.getByTestId('logs-clear'));

    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error',
        '日志清空失败',
        'permission denied',
      );
    });
  });

  it('auto-refresh polls the log tail every 10 seconds while enabled', async () => {
    vi.useFakeTimers();
    try {
      vi.mocked(loadRecentLogs).mockResolvedValue({
        success: true,
        data: { limit: 200, lines: [JSON.stringify({ ts: 1, level: 'info', event: 'tick' })] },
      });

      render(Logs);
      await fireEvent.click(screen.getByTestId('logs-load'));
      await waitFor(() => expect(loadRecentLogs).toHaveBeenCalledTimes(1));

      await fireEvent.click(screen.getByTestId('logs-auto-refresh'));
      await vi.advanceTimersByTimeAsync(10_000);
      await waitFor(() => expect(loadRecentLogs).toHaveBeenCalledTimes(2));
      await vi.advanceTimersByTimeAsync(10_000);
      await waitFor(() => expect(loadRecentLogs).toHaveBeenCalledTimes(3));

      // Disabling stops the polling.
      await fireEvent.click(screen.getByTestId('logs-auto-refresh'));
      await vi.advanceTimersByTimeAsync(30_000);
      expect(loadRecentLogs).toHaveBeenCalledTimes(3);
    } finally {
      vi.useRealTimers();
    }
  });
  it('renders its heading in the EN locale (UI-27-04 EN smoke)', async () => {
    await withEnglishLocale(async () => {
      render(Logs);
      expect(
        await screen.findByRole('heading', { level: 2, name: 'Logs' }),
      ).toBeTruthy();
    });
  });

});
