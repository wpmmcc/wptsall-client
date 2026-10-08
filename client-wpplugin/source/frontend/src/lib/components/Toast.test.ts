// catalog: WEBUI-UI-Toast
// oracle: L2
// 状态矩阵：store 消息 + 详情渲染 / dismiss 移除（生命周期两态）。
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { fireEvent, render, screen, cleanup, waitFor } from '@testing-library/svelte';
import { get } from 'svelte/store';
import Toast from './Toast.svelte';
import { toasts } from '../stores/toast';

describe('components/Toast', () => {
  beforeEach(() => {
    toasts.set([]);
  });

  afterEach(() => {
    toasts.set([]);
    cleanup();
  });

  it('renders toast message and detail from store', async () => {
    render(Toast);
    toasts.set([{ id: 1, type: 'success', message: 'Saved', detail: 'All good' }]);

    await waitFor(() => {
      expect(screen.getByText('Saved')).toBeTruthy();
      expect(screen.getByText('All good')).toBeTruthy();
    });
  });

  it('dismiss button removes toast from store', async () => {
    const { container } = render(Toast);
    toasts.set([{ id: 11, type: 'error', message: 'Failed' }]);

    await waitFor(() => {
      expect(screen.getByText('Failed')).toBeTruthy();
    });

    const closeBtn = container.querySelector('button');
    expect(closeBtn).toBeTruthy();
    if (closeBtn) {
      await fireEvent.click(closeBtn);
    }

    expect(get(toasts)).toEqual([]);
  });

  // 批 N2 / U-6 a11y: toasts must announce themselves — role=alert for
  // error/warning (assertive), role=status for success/info (polite) — and
  // the icon-only dismiss button needs an accessible name. Before this leg
  // the region had no live semantics at all.
  it('announces error toasts assertively and success toasts politely', async () => {
    render(Toast);
    toasts.set([{ id: 21, type: 'error', message: 'Boom' }]);

    await waitFor(() => {
      expect(screen.getByRole('alert')).toBeTruthy();
    });

    toasts.set([{ id: 22, type: 'success', message: 'Saved' }]);
    await waitFor(() => {
      expect(screen.getByRole('status')).toBeTruthy();
      expect(screen.queryByRole('alert')).toBeNull();
    });
  });

  it('gives the icon-only dismiss button an accessible name', async () => {
    render(Toast);
    toasts.set([{ id: 31, type: 'info', message: 'Note' }]);

    await waitFor(() => {
      expect(screen.getByText('Note')).toBeTruthy();
    });

    // Accessible name present (i18n common.close; the fixture locale may be
    // en or zh-CN, so accept either label) — not an unnamed icon.
    expect(screen.getByRole('button', { name: /^(Close|关闭)$/ })).toBeTruthy();
  });
});
