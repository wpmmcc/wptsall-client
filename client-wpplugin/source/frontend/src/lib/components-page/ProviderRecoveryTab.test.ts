import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ProviderRecoveryTab from './ProviderRecoveryTab.svelte';
import * as api from '../api/providerRecovery';
import { withEnglishLocale } from '../test-en-locale';

vi.mock('../api/providerRecovery', () => ({
  listProviderOperations: vi.fn(),
  reconcileProviderOperation: vi.fn(),
}));

const row = {
  operation_id: '64acb7df-8366-45b5-98a7-944ff8f995ac', site: 'https://owned.example',
  component_id: 'owned-provider', relation_id: 33, object_type: 'post_type', object_id: 77,
  field_name: 'owned-field', chunk_index: 0, lane: 'text', source_lang: 'en', target_lang: 'zh',
  state: 'submit_unknown', can_reconcile: true,
};

describe('provider evidence review', () => {
  beforeEach(() => {
    vi.mocked(api.listProviderOperations).mockResolvedValue({ success: true, data: { items: [row] } });
    vi.mocked(api.reconcileProviderOperation).mockResolvedValue({ success: true, data: { state: 'polling' } });
  });
  afterEach(() => { cleanup(); vi.clearAllMocks(); });

  it('does not query on listing, review selection or cancel', async () => withEnglishLocale(async () => {
    render(ProviderRecoveryTab);
    await waitFor(() => expect(screen.getByText(/submit_unknown/)).toBeTruthy());
    expect(api.reconcileProviderOperation).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Review provider evidence' }));
    expect(screen.getByText(/Stop the worker first/)).toBeTruthy();
    expect(screen.queryByRole('textbox')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(api.reconcileProviderOperation).not.toHaveBeenCalled();
  }));

  it('sends only the selected operation after explicit confirmation', async () => withEnglishLocale(async () => {
    render(ProviderRecoveryTab);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review provider evidence' })).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Review provider evidence' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Verify existing provider job' }));
    await waitFor(() => expect(api.reconcileProviderOperation).toHaveBeenCalledExactlyOnceWith(row.operation_id));
    expect(api.listProviderOperations).toHaveBeenCalledTimes(2);
  }));

  it('retains the row and explicit panel after evidence refusal', async () => withEnglishLocale(async () => {
    vi.mocked(api.reconcileProviderOperation).mockResolvedValue({
      success: false, error: { code: 'PROVIDER_REVIEW_REQUIRED', message: 'Evidence mismatch; no submission was retried.' },
    });
    render(ProviderRecoveryTab);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review provider evidence' })).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Review provider evidence' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Verify existing provider job' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Evidence mismatch'));
    expect(screen.getByText(/submit_unknown/)).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Verify existing provider job' })).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(api.reconcileProviderOperation).toHaveBeenCalledTimes(1);
  }));

  it('shows unsupported evidence without a verify entry or arbitrary job input', async () => withEnglishLocale(async () => {
    vi.mocked(api.listProviderOperations).mockResolvedValue({ success: true, data: { items: [{ ...row, can_reconcile: false }] } });
    render(ProviderRecoveryTab);
    await waitFor(() => expect(screen.getByText(/job ID alone is not evidence/)).toBeTruthy());
    expect(screen.queryByRole('button', { name: 'Review provider evidence' })).toBeNull();
    expect(screen.queryByRole('textbox')).toBeNull();
    expect(api.reconcileProviderOperation).not.toHaveBeenCalled();
  }));

  it('does not report an empty inventory when evidence is unreadable', async () => withEnglishLocale(async () => {
    vi.mocked(api.listProviderOperations).mockResolvedValue({
      success: false, error: { code: 'PROVIDER_RECOVERY_UNREADABLE', message: 'Retained evidence cannot be read.' },
    });
    render(ProviderRecoveryTab);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('cannot be read'));
    expect(screen.queryByText('No retained provider operations.')).toBeNull();
    expect(api.reconcileProviderOperation).not.toHaveBeenCalled();
  }));

  it('blocks repeat clicks while a verification is in flight', async () => withEnglishLocale(async () => {
    let finish!: (value: { success: true; data: { state: string } }) => void;
    vi.mocked(api.reconcileProviderOperation).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
    render(ProviderRecoveryTab);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Review provider evidence' })).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: 'Review provider evidence' }));
    const button = screen.getByRole('button', { name: 'Verify existing provider job' });
    await fireEvent.click(button);
    await fireEvent.click(button);
    expect(api.reconcileProviderOperation).toHaveBeenCalledTimes(1);
    finish({ success: true, data: { state: 'polling' } });
    await waitFor(() => expect(api.listProviderOperations).toHaveBeenCalledTimes(2));
  }));
});
