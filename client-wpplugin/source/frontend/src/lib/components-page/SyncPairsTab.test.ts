// catalog: WEBUI-UI-SyncPairsTab
// oracle: L2
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { addMessages, init, locale } from 'svelte-i18n';
import SyncPairsTab from './SyncPairsTab.svelte';
import * as syncPairsApi from '../api/syncPairs';
import * as toastModule from '../stores/toast';
import en from '../../../../locales/en.json';
import zhCN from '../../../../locales/zh-CN.json';

addMessages('en', en);
addMessages('zh-CN', zhCN);
init({
  fallbackLocale: 'en',
  initialLocale: 'en',
});

const hoisted = vi.hoisted(() => ({
  statusStore: (() => {
    let value: any = {
      domain_token_bindings: [
        {
          api_base_url: 'https://site-a.com',
          token_prefix: 'tok_a',
          token_len: 32,
          plugin_identity: 'wpmmcc',
        },
        {
          api_base_url: 'https://site-b.com',
          token_prefix: 'tok_b',
          token_len: 32,
          plugin_identity: 'wpmmcc',
        },
      ],
    };
    const subscribers = new Set<(next: any) => void>();
    return {
      subscribe(run: (next: any) => void) {
        subscribers.add(run);
        run(value);
        return () => subscribers.delete(run);
      },
      set(next: any) {
        value = next;
        subscribers.forEach((run) => run(value));
      },
    };
  })(),
  fetchStatusMock: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../api/syncPairs', () => ({
  listSyncPairs: vi.fn(),
  upsertSyncPair: vi.fn(),
  deleteSyncPair: vi.fn(),
  pauseSyncPair: vi.fn(),
  resumeSyncPair: vi.fn(),
  runSyncPair: vi.fn(),
  pairSyncSite: vi.fn(),
  unpairSyncSite: vi.fn(),
}));

vi.mock('../api/components', () => ({
  listAllLocalComponents: vi.fn().mockResolvedValue({
    success: true,
    data: {
      items: [
        { id: 'comp-translate', name: 'DeepSeek Translate' },
      ],
      total: 1,
    },
  }),
}));

vi.mock('../stores/status', () => ({
  status: hoisted.statusStore,
  fetchStatus: hoisted.fetchStatusMock,
}));

vi.mock('../stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('components-page/SyncPairsTab', () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  beforeEach(() => {
    locale.set('en');
    vi.mocked(syncPairsApi.listSyncPairs).mockResolvedValue({
      success: true,
      data: {
        schema_version: 'wpmmcc-sync-pairs.v1',
        pairs: [],
        credentials: [],
        client_origin_uuid: 'client-uuid-1',
        updated_at: 1000,
      },
    });
  });

  it('renders empty state when no sync pairs exist', async () => {
    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('No sync pairs configured')).toBeTruthy();
    });
  });

  it('renders sync pairs list when pairs exist', async () => {
    vi.mocked(syncPairsApi.listSyncPairs).mockResolvedValue({
      success: true,
      data: {
        schema_version: 'wpmmcc-sync-pairs.v1',
        pairs: [
          {
            id: 'pair-1',
            name: 'Production Multi-Site Sync',
            source_domain: 'https://site-a.com',
            target_domain: 'https://site-b.com',
            direction: 'unidirectional',
            sync_mode: 'sync_and_translate',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            conflict_strategy: 'lww',
            sync_frequency: 'hourly',
            post_types: ['post', 'page'],
            status: 'active',
            last_sync_at: 1720000000,
            last_sync_count: 42,
            created_at: 1710000000,
            updated_at: 1720000000,
          },
        ],
        credentials: [
          {
            domain: 'https://site-a.com',
            peer_uuid: 'uuid-a',
            peer_name: 'Site A',
            key_scheme: 'hmac_v1',
            negotiated_direction: 'push_only',
            paired_as: 'source',
            paired_at: 1710000000,
          },
        ],
        client_origin_uuid: 'client-uuid-1',
        updated_at: 1720000000,
      },
    });

    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('https://site-a.com')).toBeTruthy();
      expect(screen.getByText('https://site-b.com')).toBeTruthy();
      expect(screen.getByText('(Production Multi-Site Sync)')).toBeTruthy();
      expect(screen.getByText('Active')).toBeTruthy();
      expect(screen.getByText('Sync Now')).toBeTruthy();
    });
  });

  it('triggers manual sync run when Sync Now is clicked', async () => {
    vi.mocked(syncPairsApi.listSyncPairs).mockResolvedValue({
      success: true,
      data: {
        schema_version: 'wpmmcc-sync-pairs.v1',
        pairs: [
          {
            id: 'pair-1',
            name: 'Pair 1',
            source_domain: 'https://site-a.com',
            target_domain: 'https://site-b.com',
            direction: 'unidirectional',
            sync_mode: 'sync_only',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            conflict_strategy: 'lww',
            sync_frequency: 'manual',
            post_types: ['post'],
            status: 'active',
            created_at: 1710000000,
            updated_at: 1710000000,
          },
        ],
        credentials: [],
        client_origin_uuid: 'client-uuid-1',
        updated_at: 1710000000,
      },
    });

    vi.mocked(syncPairsApi.runSyncPair).mockResolvedValue({
      success: true,
      data: {
        pair_id: 'pair-1',
        triggered: true,
        async: true,
        timestamp: 1720000000,
      },
    });

    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('Sync Now')).toBeTruthy();
    });

    const runBtn = screen.getByText('Sync Now');
    await fireEvent.click(runBtn);

    expect(syncPairsApi.runSyncPair).toHaveBeenCalledWith('pair-1');
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'success',
        'Sync started in the background; counts will update shortly'
      );
    });
  });

  it('toggles pause and resume on a sync pair', async () => {
    vi.mocked(syncPairsApi.listSyncPairs).mockResolvedValue({
      success: true,
      data: {
        schema_version: 'wpmmcc-sync-pairs.v1',
        pairs: [
          {
            id: 'pair-1',
            name: 'Pair 1',
            source_domain: 'https://site-a.com',
            target_domain: 'https://site-b.com',
            direction: 'unidirectional',
            sync_mode: 'sync_only',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            conflict_strategy: 'lww',
            sync_frequency: 'manual',
            post_types: ['post'],
            status: 'active',
            created_at: 1710000000,
            updated_at: 1710000000,
          },
        ],
        credentials: [],
        client_origin_uuid: 'client-uuid-1',
        updated_at: 1710000000,
      },
    });

    vi.mocked(syncPairsApi.pauseSyncPair).mockResolvedValue({
      success: true,
      data: {
        id: 'pair-1',
        status: 'paused',
      },
    });

    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('Pause')).toBeTruthy();
    });

    const pauseBtn = screen.getByText('Pause');
    await fireEvent.click(pauseBtn);

    expect(syncPairsApi.pauseSyncPair).toHaveBeenCalledWith('pair-1');
  });

  it('opens create modal and saves a new sync pair', async () => {
    vi.mocked(syncPairsApi.upsertSyncPair).mockResolvedValue({
      success: true,
      data: {
        pair: {
          id: 'new-pair',
          name: 'New Pair',
          source_domain: 'https://site-a.com',
          target_domain: 'https://site-b.com',
          direction: 'unidirectional',
          sync_mode: 'sync_only',
          source_lang: 'en_US',
          target_lang: 'zh_CN',
          conflict_strategy: 'lww',
          sync_frequency: 'manual',
          post_types: ['post', 'page'],
          status: 'active',
          created_at: 1710000000,
          updated_at: 1710000000,
        },
      },
    });

    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getAllByText('Create Sync Pair').length).toBeGreaterThan(0);
    });

    // Click create pair button
    const createBtns = screen.getAllByText('Create Sync Pair');
    await fireEvent.click(createBtns[0]);

    // Modal should be open
    await waitFor(() => {
      expect(screen.getByLabelText('Name (Optional)')).toBeTruthy();
    });

    // Click Save
    const saveBtn = screen.getByText('Save');
    await fireEvent.click(saveBtn);

    expect(syncPairsApi.upsertSyncPair).toHaveBeenCalled();
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Sync pair saved successfully');
    });
  });

  it('deletes a sync pair with confirmation modal', async () => {
    vi.mocked(syncPairsApi.listSyncPairs).mockResolvedValue({
      success: true,
      data: {
        schema_version: 'wpmmcc-sync-pairs.v1',
        pairs: [
          {
            id: 'pair-1',
            name: 'Pair 1',
            source_domain: 'https://site-a.com',
            target_domain: 'https://site-b.com',
            direction: 'unidirectional',
            sync_mode: 'sync_only',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            conflict_strategy: 'lww',
            sync_frequency: 'manual',
            post_types: ['post'],
            status: 'active',
            created_at: 1710000000,
            updated_at: 1710000000,
          },
        ],
        credentials: [],
        client_origin_uuid: 'client-uuid-1',
        updated_at: 1710000000,
      },
    });

    vi.mocked(syncPairsApi.deleteSyncPair).mockResolvedValue({
      success: true,
      data: {
        deleted: true,
        id: 'pair-1',
      },
    });

    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('Delete')).toBeTruthy();
    });

    // Click Delete on card
    await fireEvent.click(screen.getByText('Delete'));

    // Confirmation dialog appears
    await waitFor(() => {
      expect(
        screen.getByText('Are you sure you want to delete this sync pair? This cannot be undone.')
      ).toBeTruthy();
    });

    // Confirm delete (there's a Delete button inside the modal)
    const deleteBtns = screen.getAllByText('Delete');
    const confirmBtn = deleteBtns[deleteBtns.length - 1];
    await fireEvent.click(confirmBtn);

    expect(syncPairsApi.deleteSyncPair).toHaveBeenCalledWith('pair-1');
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Sync pair deleted');
    });
  });

  it('pairs a site through the pairing modal with a one-time code', async () => {
    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('Peer Pairing')).toBeTruthy();
    });
    await fireEvent.click(screen.getByText('Peer Pairing'));

    await waitFor(() => {
      expect(screen.getByLabelText(/^Pairing Code/)).toBeTruthy();
    });

    // Invalid code shape is rejected client-side before any request.
    const codeInput = screen.getByLabelText(/^Pairing Code/);
    await fireEvent.input(codeInput, { target: { value: 'short' } });
    await fireEvent.click(screen.getByText('Pair Now'));
    await waitFor(() => {
      expect(
        screen.getByText('Invalid pairing code: expected 32 hex characters')
      ).toBeTruthy();
    });
    expect(syncPairsApi.pairSyncSite).not.toHaveBeenCalled();

    // Valid code → pairSyncSite with the selected role.
    vi.mocked(syncPairsApi.pairSyncSite).mockResolvedValue({
      success: true,
      data: {
        domain: 'https://site-a.com',
        peer_uuid: 'uuid-a',
        peer_name: 'Site A',
        key_scheme: 'hmac_v1',
        negotiated_direction: 'push_only',
        paired_as: 'source',
        paired_at: 1720000000,
      },
    });
    await fireEvent.input(codeInput, {
      target: { value: '0f1a2b3c4d5e6f708192a3b4c5d6e7f8' },
    });
    // X-1 (tasks/5.3falsh2/12 批 B): the pairing-chosen conflict strategy
    // rides the handshake payload (fifth value merge selected here).
    const strategySelect = document.getElementById(
      'pairing-conflict'
    ) as HTMLSelectElement;
    await fireEvent.change(strategySelect, { target: { value: 'merge' } });
    await fireEvent.click(screen.getByText('Pair Now'));

    await waitFor(() => {
      expect(syncPairsApi.pairSyncSite).toHaveBeenCalledWith({
        domain: 'https://site-a.com',
        pairing_code: '0f1a2b3c4d5e6f708192a3b4c5d6e7f8',
        role: 'source',
        conflict_strategy: 'merge',
      });
    });
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Paired: Site A');
    });
  });

  it('shows pairing-required badges when pair ends are unpaired', async () => {
    vi.mocked(syncPairsApi.listSyncPairs).mockResolvedValue({
      success: true,
      data: {
        schema_version: 'wpmmcc-sync-pairs.v1',
        pairs: [
          {
            id: 'pair-1',
            name: 'Pair 1',
            source_domain: 'https://site-a.com',
            target_domain: 'https://site-b.com',
            direction: 'unidirectional',
            sync_mode: 'sync_only',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            conflict_strategy: 'lww',
            sync_frequency: 'manual',
            post_types: ['post'],
            status: 'active',
            created_at: 1710000000,
            updated_at: 1710000000,
          },
        ],
        credentials: [],
        client_origin_uuid: 'client-uuid-1',
        updated_at: 1710000000,
      },
    });

    render(SyncPairsTab);

    await waitFor(() => {
      expect(screen.getByText('Pairing required')).toBeTruthy();
    });
  });
});
