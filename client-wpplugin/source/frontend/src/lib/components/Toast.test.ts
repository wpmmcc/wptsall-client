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
});
