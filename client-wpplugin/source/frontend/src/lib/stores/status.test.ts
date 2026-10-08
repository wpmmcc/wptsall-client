import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { fetchStatus, startPolling, status, stopPolling } from './status';
import { getStatus } from '../api/worker';

vi.mock('../api/worker', () => ({
  getStatus: vi.fn(),
}));

describe('stores/status', () => {
  beforeEach(() => {
    status.set(null);
    vi.restoreAllMocks();
    vi.mocked(getStatus).mockReset();
  });

  afterEach(() => {
    stopPolling();
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('stores successful /api/status payload returned by shared helper', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    vi.mocked(getStatus).mockResolvedValue({
      success: true,
      data: { logged_in: true, server_base: 'https://www.wpmm.cc' } as any,
    });

    await fetchStatus();

    expect(getStatus).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(get(status)).toEqual({ logged_in: true, server_base: 'https://www.wpmm.cc' });
  });

  it('clears status when shared helper returns session failure', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    status.set({ logged_in: true, server_base: 'https://www.wpmm.cc' });
    vi.mocked(getStatus).mockResolvedValue({
      success: false,
      error: { code: 'SESSION_EXPIRED', message: 'Session expired', status: 401 },
    });

    await fetchStatus();

    expect(getStatus).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(get(status)).toBeNull();
    expect(errorSpy).toHaveBeenCalledTimes(1);
  });

  it('keeps previous status on non-session structured failure', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    status.set({ logged_in: true, server_base: 'https://www.wpmm.cc' });
    vi.mocked(getStatus).mockResolvedValue({
      success: false,
      error: { code: 'UPSTREAM_TEMP_UNAVAILABLE', message: 'Server busy', status: 503 },
    });

    await fetchStatus();

    expect(getStatus).toHaveBeenCalledTimes(1);
    expect(fetchMock).not.toHaveBeenCalled();
    expect(get(status)).toEqual({ logged_in: true, server_base: 'https://www.wpmm.cc' });
    expect(errorSpy).toHaveBeenCalledTimes(1);
  });

  it('swallows shared helper exceptions and logs once', async () => {
    const errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    vi.mocked(getStatus).mockRejectedValue(new Error('network down'));

    await fetchStatus();

    expect(get(status)).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(errorSpy).toHaveBeenCalledTimes(1);
  });

  it('startPolling starts timer and refreshes listener bindings on restart', async () => {
    vi.useFakeTimers();
    vi.mocked(getStatus).mockResolvedValue({
      success: true,
      data: { logged_in: true } as any,
    });
    const addDocSpy = vi.spyOn(document, 'addEventListener');
    const addWinSpy = vi.spyOn(window, 'addEventListener');
    const removeDocSpy = vi.spyOn(document, 'removeEventListener');
    const removeWinSpy = vi.spyOn(window, 'removeEventListener');
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval');

    startPolling(1111);
    await Promise.resolve();

    expect(setIntervalSpy).toHaveBeenCalledWith(fetchStatus, 1111);
    expect(addDocSpy).toHaveBeenCalledWith('visibilitychange', expect.any(Function));
    expect(addWinSpy).toHaveBeenCalledWith('focus', expect.any(Function));

    startPolling(2222);
    await Promise.resolve();

    expect(setIntervalSpy).toHaveBeenLastCalledWith(fetchStatus, 2222);
    expect(removeDocSpy).toHaveBeenCalledWith('visibilitychange', expect.any(Function));
    expect(removeWinSpy).toHaveBeenCalledWith('focus', expect.any(Function));
  });

  it('visibilitychange hidden clears timer and visible restarts timer', async () => {
    vi.useFakeTimers();
    vi.mocked(getStatus).mockResolvedValue({
      success: true,
      data: { logged_in: true } as any,
    });
    const addDocSpy = vi.spyOn(document, 'addEventListener');
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval');
    const clearIntervalSpy = vi.spyOn(globalThis, 'clearInterval');

    let hidden = false;
    const originalHidden = Object.getOwnPropertyDescriptor(document, 'hidden');
    Object.defineProperty(document, 'hidden', {
      configurable: true,
      get: () => hidden,
    });

    startPolling(1000);
    await Promise.resolve();

    const visibilityCall = addDocSpy.mock.calls.find(([event]) => event === 'visibilitychange');
    const visibilityHandler = visibilityCall?.[1] as EventListener;
    expect(visibilityHandler).toBeTypeOf('function');

    hidden = true;
    visibilityHandler(new Event('visibilitychange'));
    expect(clearIntervalSpy).toHaveBeenCalled();

    const beforeVisibleSetCount = setIntervalSpy.mock.calls.length;
    hidden = false;
    visibilityHandler(new Event('visibilitychange'));
    await Promise.resolve();

    expect(setIntervalSpy.mock.calls.length).toBeGreaterThan(beforeVisibleSetCount);
    expect(getStatus).toHaveBeenCalledTimes(2);

    if (originalHidden) {
      Object.defineProperty(document, 'hidden', originalHidden);
    } else {
      Reflect.deleteProperty(document, 'hidden');
    }
  });

  it('focus event triggers immediate refresh and stopPolling detaches listeners', async () => {
    vi.useFakeTimers();
    vi.mocked(getStatus).mockResolvedValue({
      success: true,
      data: { logged_in: true } as any,
    });
    const addWinSpy = vi.spyOn(window, 'addEventListener');
    const removeDocSpy = vi.spyOn(document, 'removeEventListener');
    const removeWinSpy = vi.spyOn(window, 'removeEventListener');

    startPolling(1500);
    await Promise.resolve();

    const focusCall = addWinSpy.mock.calls.find(([event]) => event === 'focus');
    const focusHandler = focusCall?.[1] as EventListener;
    focusHandler(new Event('focus'));
    await Promise.resolve();

    expect(getStatus).toHaveBeenCalledTimes(2);

    stopPolling();
    expect(removeDocSpy).toHaveBeenCalledWith('visibilitychange', expect.any(Function));
    expect(removeWinSpy).toHaveBeenCalledWith('focus', expect.any(Function));
  });
});
