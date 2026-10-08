// catalog: WEBUI-UI-SyncPairsTab
// oracle: L2
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { locale } from 'svelte-i18n';
import SyncPairsTab from './SyncPairsTab.svelte';
import * as reviewApi from '../api/syncReview';
import * as toast from '../stores/toast';

vi.mock('../api/syncPairs', () => ({
  listSyncPairs: vi.fn().mockResolvedValue({ success: true, data: { pairs: [], credentials: [] } }),
  deleteSyncPair: vi.fn(), pairSyncSite: vi.fn(), pauseSyncPair: vi.fn(),
  resumeSyncPair: vi.fn(), runSyncPair: vi.fn(), unpairSyncSite: vi.fn(), upsertSyncPair: vi.fn(),
}));
vi.mock('../api/components', () => ({ listAllLocalComponents: vi.fn() }));
vi.mock('../stores/status', async () => ({
  status: (await import('svelte/store')).writable({ domain_token_bindings: [] }),
  fetchStatus: vi.fn(),
}));
vi.mock('../stores/toast', () => ({ showToast: vi.fn() }));
vi.mock('../api/syncReview', () => ({
  listSyncReview: vi.fn(),
  getSyncReviewItem: vi.fn(),
  updateSyncReviewItem: vi.fn(),
  approveSyncReviewItem: vi.fn(),
  rejectSyncReviewItem: vi.fn(),
}));

const detail = {
  id: 'review-one', pair_id: 'pair-one', canonical_uuid: 'uuid-one', status: 'pending_review',
  source_title: 'Source', source_content: 'Source body', source_excerpt: '',
  proposed_title: 'Original title', proposed_content: 'Original body', proposed_excerpt: 'Original excerpt',
  post_type: 'post', error_message: null,
};
const inbox = {
  success: true as const,
  data: { total: 1, items: [{ ...detail, created_at: 1, updated_at: 1 }] },
};
const failure = (message: string) => ({
  success: false as const, error: { code: 'FIXTURE_FAILURE', message, status: 500 },
});

async function openReview() {
  render(SyncPairsTab);
  await fireEvent.click(await screen.findByText('Review'));
  await screen.findByTestId('sync-review-modal');
}

describe('UI03 sync review feedback and edited approval', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    locale.set('en');
    vi.mocked(reviewApi.listSyncReview).mockResolvedValue(inbox);
    vi.mocked(reviewApi.getSyncReviewItem).mockResolvedValue({ success: true, data: { item: { ...detail } } });
    vi.mocked(reviewApi.updateSyncReviewItem).mockImplementation(async (_id, body) => ({
      success: true, data: { item: { ...detail, ...body } },
    }));
    vi.mocked(reviewApi.approveSyncReviewItem).mockResolvedValue({
      success: true, data: { id: detail.id, status: 'pushed' },
    });
    vi.mocked(reviewApi.rejectSyncReviewItem).mockResolvedValue({
      success: true, data: { id: detail.id, status: 'rejected' },
    });
  });
  afterEach(cleanup);

  it('ui03 renders a failed inbox load instead of silently presenting an empty queue', async () => {
    vi.mocked(reviewApi.listSyncReview).mockResolvedValue(failure('Review storage unavailable'));
    render(SyncPairsTab);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Review storage unavailable'));
  });

  it('ui03 keeps the last inbox visible when refresh fails', async () => {
    await openReview();
    await fireEvent.click(screen.getByText('Close'));
    vi.mocked(reviewApi.listSyncReview).mockResolvedValue(failure('Refresh unavailable'));
    await fireEvent.click(screen.getAllByText('Refresh')[1]);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Refresh unavailable'));
    expect(screen.getByText('Original title')).toBeTruthy();
  });

  it('ui03 reports a rejected transport and clears the inbox error after a successful retry', async () => {
    vi.mocked(reviewApi.listSyncReview).mockRejectedValueOnce(new Error('Offline'));
    render(SyncPairsTab);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Offline'));
    await fireEvent.click(screen.getByText('Refresh'));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
    expect(screen.getByText('Original title')).toBeTruthy();
  });

  it('ui03 saves the current three edited fields before approve and sends only after save completes', async () => {
    await openReview();
    await fireEvent.input(screen.getByLabelText('Title', { selector: '#sync-review-proposed-title' }), { target: { value: 'Edited title' } });
    await fireEvent.input(screen.getByLabelText('Content', { selector: '#sync-review-proposed-content' }), { target: { value: 'Edited body' } });
    await fireEvent.input(screen.getByLabelText('Excerpt'), { target: { value: 'Edited excerpt' } });
    let finish!: (value: any) => void;
    vi.mocked(reviewApi.updateSyncReviewItem).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await fireEvent.click(screen.getByTestId('sync-review-approve'));
    await waitFor(() => expect(reviewApi.updateSyncReviewItem).toHaveBeenCalledWith(detail.id, {
      proposed_title: 'Edited title', proposed_content: 'Edited body', proposed_excerpt: 'Edited excerpt',
    }));
    expect(reviewApi.approveSyncReviewItem).not.toHaveBeenCalled();
    expect((screen.getByTestId('sync-review-approve') as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByLabelText('Title', { selector: '#sync-review-proposed-title' }) as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByText('Close') as HTMLButtonElement).disabled).toBe(true);
    finish({ success: true, data: { item: { ...detail, proposed_title: 'Edited title' } } });
    await waitFor(() => expect(reviewApi.approveSyncReviewItem).toHaveBeenCalledWith(detail.id));
    expect(vi.mocked(reviewApi.updateSyncReviewItem).mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(reviewApi.approveSyncReviewItem).mock.invocationCallOrder[0]);
    await waitFor(() => expect(screen.queryByTestId('sync-review-modal')).toBeNull());
  });

  it('ui03 saves edits without approving when Save alone is clicked', async () => {
    await openReview();
    await fireEvent.input(screen.getByLabelText('Excerpt'), { target: { value: 'Saved excerpt' } });
    await fireEvent.click(screen.getByText('Save'));
    await waitFor(() => expect(toast.showToast).toHaveBeenCalledWith('success', 'Review edits saved'));
    expect(reviewApi.updateSyncReviewItem).toHaveBeenCalledWith(detail.id, {
      proposed_title: detail.proposed_title, proposed_content: detail.proposed_content,
      proposed_excerpt: 'Saved excerpt',
    });
    expect(reviewApi.approveSyncReviewItem).not.toHaveBeenCalled();
    expect(screen.getByTestId('sync-review-modal')).toBeTruthy();
  });

  it('ui03 catches a save transport failure and never pushes the original packet', async () => {
    await openReview();
    vi.mocked(reviewApi.updateSyncReviewItem).mockRejectedValue(new Error('Save transport failed'));
    await fireEvent.click(screen.getByTestId('sync-review-approve'));
    await waitFor(() => expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Save transport failed'));
    expect(reviewApi.approveSyncReviewItem).not.toHaveBeenCalled();
    expect((screen.getByTestId('sync-review-approve') as HTMLButtonElement).disabled).toBe(false);
  });

  it('ui03 reloads detail and inbox after approve is rejected by the server', async () => {
    await openReview();
    vi.mocked(reviewApi.approveSyncReviewItem).mockResolvedValue(failure('Target rejected'));
    vi.mocked(reviewApi.getSyncReviewItem).mockResolvedValue({
      success: true, data: { item: { ...detail, error_message: 'Target post type disabled' } },
    });
    await fireEvent.click(screen.getByTestId('sync-review-approve'));
    await screen.findByText('Target post type disabled');
    expect(reviewApi.listSyncReview).toHaveBeenCalledTimes(2);
    expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Target rejected');
  });

  it('ui03 refreshes server state after approval transport throws but preserves draft when saving throws', async () => {
    await openReview();
    vi.mocked(reviewApi.approveSyncReviewItem).mockRejectedValue(new Error('Approval offline'));
    await fireEvent.click(screen.getByTestId('sync-review-approve'));
    await waitFor(() => expect(reviewApi.getSyncReviewItem).toHaveBeenCalledTimes(2));
    expect(reviewApi.listSyncReview).toHaveBeenCalledTimes(2);
    expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Approval offline');
    await waitFor(() => expect((screen.getByTestId('sync-review-approve') as HTMLButtonElement).disabled).toBe(false));
  });

  it('ui03 does not approve after edited field save fails and preserves the draft', async () => {
    await openReview();
    await fireEvent.input(screen.getByLabelText('Title', { selector: '#sync-review-proposed-title' }), { target: { value: 'Keep my draft' } });
    vi.mocked(reviewApi.updateSyncReviewItem).mockResolvedValue(failure('Save unavailable'));
    await fireEvent.click(screen.getByTestId('sync-review-approve'));
    await waitFor(() => expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Save unavailable'));
    expect(reviewApi.approveSyncReviewItem).not.toHaveBeenCalled();
    expect((screen.getByLabelText('Title', { selector: '#sync-review-proposed-title' }) as HTMLTextAreaElement).value).toBe('Keep my draft');
  });

  it('ui03 refreshes the server error and inbox after rejection fails', async () => {
    await openReview();
    vi.mocked(reviewApi.rejectSyncReviewItem).mockResolvedValue(failure('Reject unavailable'));
    vi.mocked(reviewApi.getSyncReviewItem).mockResolvedValue({
      success: true, data: { item: { ...detail, error_message: 'Server rejection detail' } },
    });
    await fireEvent.click(screen.getByText('Reject'));
    await waitFor(() => expect(screen.getByText('Server rejection detail')).toBeTruthy());
    expect(reviewApi.getSyncReviewItem).toHaveBeenCalledTimes(2);
    expect(reviewApi.listSyncReview).toHaveBeenCalledTimes(2);
    expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Reject unavailable');
  });

  it('ui03 catches detail load exceptions and leaves the inbox available', async () => {
    vi.mocked(reviewApi.getSyncReviewItem).mockRejectedValue(new Error('Detail offline'));
    render(SyncPairsTab);
    await fireEvent.click(await screen.findByText('Review'));
    await waitFor(() => expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Detail offline'));
    expect(screen.queryByTestId('sync-review-modal')).toBeNull();
    expect(screen.getByText('Original title')).toBeTruthy();
  });

  it('ui03 reloads detail after a rejection transport exception and re-enables actions', async () => {
    await openReview();
    vi.mocked(reviewApi.rejectSyncReviewItem).mockRejectedValue(new Error('Reject offline'));
    await fireEvent.click(screen.getByText('Reject'));
    await waitFor(() => expect(reviewApi.getSyncReviewItem).toHaveBeenCalledTimes(2));
    expect(reviewApi.listSyncReview).toHaveBeenCalledTimes(2);
    expect(toast.showToast).toHaveBeenCalledWith('error', expect.any(String), 'Reject offline');
    await waitFor(() => expect((screen.getByTestId('sync-review-approve') as HTMLButtonElement).disabled).toBe(false));
  });

  it('ui03 removes the closed modal and refreshes the inbox after successful rejection', async () => {
    await openReview();
    vi.mocked(reviewApi.listSyncReview).mockResolvedValue({ success: true, data: { total: 0, items: [] } });
    await fireEvent.click(screen.getByText('Reject'));
    await waitFor(() => expect(screen.queryByTestId('sync-review-modal')).toBeNull());
    expect(reviewApi.listSyncReview).toHaveBeenCalledTimes(2);
    expect(screen.queryByTestId('sync-review-inbox')).toBeNull();
    expect(reviewApi.updateSyncReviewItem).not.toHaveBeenCalled();
    expect(reviewApi.approveSyncReviewItem).not.toHaveBeenCalled();
  });
});
