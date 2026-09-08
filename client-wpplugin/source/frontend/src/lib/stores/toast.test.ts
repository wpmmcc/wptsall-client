import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import { dismissToast, showToast, toasts } from './toast';

describe('stores/toast', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    toasts.set([]);
  });

  it('auto dismisses non-error toast after 3 seconds', () => {
    showToast('success', 'ok');
    expect(get(toasts).length).toBe(1);

    vi.advanceTimersByTime(3000);
    expect(get(toasts).length).toBe(0);
  });

  it('keeps error toast until manual dismiss', () => {
    showToast('error', 'failed');
    const list = get(toasts);
    expect(list.length).toBe(1);

    vi.advanceTimersByTime(3000);
    expect(get(toasts).length).toBe(1);

    dismissToast(list[0].id);
    expect(get(toasts).length).toBe(0);
  });
});
