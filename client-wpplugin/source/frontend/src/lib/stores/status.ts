import { writable } from 'svelte/store';
import type { WebUiStatus } from '../api/types';
import { getStatus } from '../api/worker';

export type ApiStatus = Partial<WebUiStatus>;

export const status = writable<ApiStatus | null>(null);
export const loading = writable(false);

let pollTimer: ReturnType<typeof setInterval> | null = null;
let visibilityHandler: (() => void) | null = null;
let focusHandler: (() => void) | null = null;

function clearPollTimer() {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
}

function startPollTimer(intervalMs: number) {
  clearPollTimer();
  pollTimer = setInterval(fetchStatus, intervalMs);
}

function detachPollListeners() {
  if (visibilityHandler) {
    document.removeEventListener('visibilitychange', visibilityHandler);
    visibilityHandler = null;
  }
  if (focusHandler) {
    window.removeEventListener('focus', focusHandler);
    focusHandler = null;
  }
}

export async function fetchStatus() {
  loading.set(true);
  try {
    const res = await getStatus();
    if (res.success) {
      status.set(res.data);
      return;
    }
    if (res.error?.code === 'SESSION_EXPIRED' || res.error?.code === 'SESSION_REQUIRED' || res.error?.code === 'SESSION_REVOKED') {
      status.set(null);
    }
    console.error('status fetch failed', res.error);
  } catch (e) {
    status.set(null);
    console.error('status fetch failed', e);
  } finally {
    loading.set(false);
  }
}

export function startPolling(intervalMs = 10000) {
  void fetchStatus();
  startPollTimer(intervalMs);

  detachPollListeners();
  visibilityHandler = () => {
    if (document.hidden) {
      clearPollTimer();
      return;
    }
    void fetchStatus();
    startPollTimer(intervalMs);
  };
  focusHandler = () => {
    void fetchStatus();
  };

  // OAuth 新标签关闭后主页面重新可见时立即刷新
  document.addEventListener('visibilitychange', visibilityHandler);

  // 补充 focus 事件：OAuth 标签关闭后窗口重新获焦时立即拉状态
  window.addEventListener('focus', focusHandler);
}

export function stopPolling() {
  clearPollTimer();
  detachPollListeners();
}
