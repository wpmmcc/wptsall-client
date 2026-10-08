// catalog: WEBUI-ERR-GlobalErrorReporting
// catalog: WEBUI-API-POST-api-logs-client
// oracle: L2
// 状态矩阵：window error / unhandledrejection → POST /api/logs/client（fire-and-forget，后端不可达时静默）；安装幂等。
// logs-client 路由契约：本 spec 断言 POST /api/logs/client 的精确请求形状（payload 字段级，见 toHaveBeenCalledWith）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { apiFetch } from '../api/client';
import {
  handleJsErrorEvent,
  handleUnhandledRejection,
  installGlobalErrorReporting,
  reportClientError,
} from './clientErrorReporting';

vi.mock('../api/client', () => ({
  apiFetch: vi.fn(),
}));

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

function dispatchJsError(payload: {
  message: string;
  filename?: string;
  lineno?: number;
  colno?: number;
  error?: unknown;
}) {
  window.dispatchEvent(Object.assign(new Event('error'), payload));
}

function dispatchRejection(reason: unknown) {
  window.dispatchEvent(Object.assign(new Event('unhandledrejection'), { reason }));
}

describe('lib/errors/clientErrorReporting', () => {
  beforeEach(() => {
    vi.mocked(apiFetch).mockReset();
    vi.mocked(apiFetch).mockResolvedValue({
      success: true,
      data: { recorded: true },
    } as Awaited<ReturnType<typeof apiFetch>>);
  });

  afterEach(() => {
    // Remove the listeners installed by installGlobalErrorReporting so other
    // test files in the same jsdom environment are unaffected.
    window.removeEventListener('error', (evt) => handleJsErrorEvent(evt as never));
    window.removeEventListener('unhandledrejection', (evt) =>
      handleUnhandledRejection((evt as PromiseRejectionEvent).reason),
    );
  });

  it('reports an error payload to /api/logs/client via the shared fetch helper', async () => {
    reportClientError({
      kind: 'js_error',
      message: 'boom',
      source: 'App.svelte',
      lineno: 42,
      colno: 7,
      stack: 'Error: boom\n    at App.svelte:42:7',
    });
    await flush();

    expect(apiFetch).toHaveBeenCalledTimes(1);
    expect(apiFetch).toHaveBeenCalledWith('/api/logs/client', {
      method: 'POST',
      body: {
        kind: 'js_error',
        message: 'boom',
        source: 'App.svelte',
        lineno: 42,
        colno: 7,
        stack: 'Error: boom\n    at App.svelte:42:7',
      },
    });
  });

  it('never throws when the backend is unreachable (fire-and-forget)', async () => {
    vi.mocked(apiFetch).mockRejectedValue(new Error('network down'));

    expect(() =>
      reportClientError({ kind: 'js_error', message: 'boom' }),
    ).not.toThrow();
    // Flush the rejection: an unhandled rejection here would fail the suite.
    await flush();
  });

  it('maps an ErrorEvent-shaped object including the Error stack', async () => {
    handleJsErrorEvent({
      message: 'render failed',
      filename: 'Logs.svelte',
      lineno: 10,
      colno: 3,
      error: new Error('render failed'),
    });
    await flush();

    expect(apiFetch).toHaveBeenCalledWith('/api/logs/client', {
      method: 'POST',
      body: expect.objectContaining({
        kind: 'js_error',
        message: 'render failed',
        source: 'Logs.svelte',
        lineno: 10,
        colno: 3,
        stack: expect.stringContaining('render failed'),
      }),
    });
  });

  it('falls back to a generic message when the error event is bare', async () => {
    handleJsErrorEvent({});
    await flush();

    expect(apiFetch).toHaveBeenCalledWith('/api/logs/client', {
      method: 'POST',
      body: expect.objectContaining({
        kind: 'js_error',
        message: 'unknown error',
      }),
    });
  });

  it('maps unhandled rejections for Error and non-Error reasons', async () => {
    handleUnhandledRejection(new Error('async boom'));
    handleUnhandledRejection('plain string rejection');
    await flush();

    expect(apiFetch).toHaveBeenCalledTimes(2);
    expect(vi.mocked(apiFetch).mock.calls[0][1]?.body).toMatchObject({
      kind: 'unhandled_rejection',
      message: 'async boom',
      stack: expect.any(String),
    });
    expect(vi.mocked(apiFetch).mock.calls[1][1]?.body).toMatchObject({
      kind: 'unhandled_rejection',
      message: 'plain string rejection',
      stack: undefined,
    });
  });

  it('installs window listeners once and reports real dispatched events', async () => {
    installGlobalErrorReporting();
    installGlobalErrorReporting(); // idempotent: must not double-register

    dispatchJsError({ message: 'crash', filename: 'app.js', lineno: 1, colno: 1 });
    dispatchRejection('rejected value');
    await flush();

    expect(apiFetch).toHaveBeenCalledTimes(2);
    expect(vi.mocked(apiFetch).mock.calls[0][1]?.body).toMatchObject({
      kind: 'js_error',
      message: 'crash',
      source: 'app.js',
    });
    expect(vi.mocked(apiFetch).mock.calls[1][1]?.body).toMatchObject({
      kind: 'unhandled_rejection',
      message: 'rejected value',
    });
  });
});
