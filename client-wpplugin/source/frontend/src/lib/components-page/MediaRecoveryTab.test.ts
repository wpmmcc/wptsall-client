import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import MediaRecoveryTab from './MediaRecoveryTab.svelte';
import * as api from '../api/mediaRecovery';
import { withEnglishLocale } from '../test-en-locale';

vi.mock('../api/mediaRecovery', () => ({
  listMediaOperations: vi.fn(),
  reconcileMediaOperation: vi.fn(),
}));

const row = {
  operation_id: '64acb7df-8366-45b5-98a7-944ff8f995ac',
  site: 'https://owned.example', source_id: 77, relation_id: 33,
  task_id: 22, state: 'complete_unknown', attachment_id: null,
};

describe('media recovery review', () => {
  beforeEach(() => {
    vi.mocked(api.listMediaOperations).mockResolvedValue({ success: true, data: { items: [row] } });
    vi.mocked(api.reconcileMediaOperation).mockResolvedValue({ success: true, data: { attachment_id: 321 } });
  });
  afterEach(() => { cleanup(); vi.clearAllMocks(); });

  it('lists retained operations without reconciling automatically', async () => withEnglishLocale(async () => {
    render(MediaRecoveryTab);
    await waitFor(() => expect(screen.getByText(/complete_unknown/)).toBeTruthy());
    expect(api.reconcileMediaOperation).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Review receipt' }));
    expect(api.reconcileMediaOperation).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(api.reconcileMediaOperation).not.toHaveBeenCalled();
  }));

  it('rejects non-positive attachment IDs before making a request', async () => withEnglishLocale(async () => {
    render(MediaRecoveryTab);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review receipt' })).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Review receipt' }));
    await fireEvent.input(screen.getByLabelText('Existing attachment ID (optional)'), { target: { value: '-1' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Verify existing attachment' }));
    expect(screen.getByRole('alert').textContent).toContain('positive');
    expect(api.reconcileMediaOperation).not.toHaveBeenCalled();
  }));

  it('requires explicit confirmation and sends only the operation and selected ID', async () => withEnglishLocale(async () => {
    render(MediaRecoveryTab);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review receipt' })).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Review receipt' }));
    await fireEvent.input(screen.getByLabelText('Existing attachment ID (optional)'), { target: { value: '321' } });
    expect(api.reconcileMediaOperation).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Verify existing attachment' }));
    await waitFor(() => expect(api.reconcileMediaOperation).toHaveBeenCalledExactlyOnceWith(row.operation_id, 321));
  }));

  it('retains unknown state and review context after verification refusal', async () => withEnglishLocale(async () => {
    vi.mocked(api.reconcileMediaOperation).mockResolvedValue({
      success: false, error: { code: 'MEDIA_REVIEW_REQUIRED', message: 'Evidence mismatch; no import was retried.' },
    });
    render(MediaRecoveryTab);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review receipt' })).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Review receipt' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Verify existing attachment' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Evidence mismatch'));
    expect(screen.getByText(/complete_unknown/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Verify existing attachment' })).toBeTruthy();
  }));

  it('does not report an empty store when retained evidence cannot be read', async () => withEnglishLocale(async () => {
    vi.mocked(api.listMediaOperations).mockResolvedValue({
      success: false, error: { code: 'MEDIA_RECOVERY_UNREADABLE', message: 'Retained evidence cannot be read.' },
    });
    render(MediaRecoveryTab);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('cannot be read'));
    expect(screen.queryByText('No retained media operations.')).toBeNull();
    expect(api.reconcileMediaOperation).not.toHaveBeenCalled();
  }));
});
