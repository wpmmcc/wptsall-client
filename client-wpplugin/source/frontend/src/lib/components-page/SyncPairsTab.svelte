<script lang="ts">
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { modalA11y } from '../modal-a11y';
  import { status, fetchStatus } from '../stores/status';
  import { showToast } from '../stores/toast';
  import {
    deleteSyncPair,
    listSyncPairs,
    pairSyncSite,
    pauseSyncPair,
    resumeSyncPair,
    runSyncPair,
    unpairSyncSite,
    upsertSyncPair,
    type ConflictStrategy,
    type SyncDirection,
    type SyncFrequency,
    type SyncMode,
    type SyncPair,
    type SyncPairCredential,
    type SyncPairUpsertPayload,
  } from '../api/syncPairs';
  import { listAllLocalComponents } from '../api/components';
  import {
    approveSyncReviewItem,
    getSyncReviewItem,
    listSyncReview,
    rejectSyncReviewItem,
    updateSyncReviewItem,
    type SyncReviewDetail,
    type SyncReviewListItem,
  } from '../api/syncReview';

  let pairs = $state<SyncPair[]>([]);
  let credentials = $state<SyncPairCredential[]>([]);
  let loading = $state(false);
  let error = $state('');
  let runningMap = $state<Record<string, boolean>>({});
  let syncPending = $state<SyncReviewListItem[]>([]);
  let syncPendingTotal = $state(0);
  let syncPendingError = $state('');
  let reviewDetail = $state<SyncReviewDetail | null>(null);
  let reviewBusy = $state(false);

  // Modal State
  let modalOpen = $state(false);
  let isEditing = $state(false);
  let saving = $state(false);
  let formError = $state('');

  // Translate component options (loaded when the translate mode is chosen)
  let componentOptions = $state<{ id: string; name: string }[]>([]);

  // Pairing modal state
  let pairingModalOpen = $state(false);
  let pairingBusy = $state(false);
  let pairingError = $state('');
  let pairingDomain = $state('');
  let pairingRole = $state<'source' | 'target'>('source');
  /** Pairing-chosen conflict strategy (X-1): rides the handshake to the site. */
  let pairingConflictStrategy = $state<ConflictStrategy>('lww');
  let pairingCode = $state('');
  let unpairingDomain = $state<string | null>(null);

  // Delete modal state
  let deleteModalOpen = $state(false);
  let deletingPairId = $state<string | null>(null);
  let deleting = $state(false);

  // Form Fields
  let formId = $state('');
  let formName = $state('');
  let formSourceDomain = $state('');
  let formTargetDomain = $state('');
  let formDirection = $state<SyncDirection>('unidirectional');
  let formSyncMode = $state<SyncMode>('sync_only');
  let formSourceLang = $state('en_US');
  let formTargetLang = $state('zh_CN');
  let formConflictStrategy = $state<ConflictStrategy>('lww');
  let formSyncFrequency = $state<SyncFrequency>('manual');
  let formPostTypes = $state<string[]>(['post', 'page']);
  let formTranslateComponentId = $state('');
  let formTitleAction = $state('translate');
  let formContentAction = $state('translate');
  let formExcerptAction = $state('translate');
  let formReviewBeforePush = $state(false);

  // Post-type values; labels resolve via i18n (`sync_pairs.post_type_*`) so
  // the EN locale never renders Chinese (UI-27-06: these were hard-coded
  // '文章 (post)' style labels that bypassed $_ entirely).
  const POST_TYPE_OPTIONS = ['post', 'page', 'product'];

  // Registered sites from status
  let registeredSites = $derived.by(() => {
    const bindings = $status?.domain_token_bindings ?? [];
    if (bindings.length > 0) {
      return bindings.map((b) => ({
        url: b.api_base_url,
        identity: b.plugin_identity ?? null,
      }));
    }
    const domains = $status?.domains ?? [];
    return domains.map((d) => ({
      url: d.api_base_url,
      identity: null,
    }));
  });

  // Bound sites verified as the WPMMCC plugin (pairing-eligible)
  let wpmmccSites = $derived.by(() =>
    registeredSites.filter((s) => s.identity === 'wpmmcc'),
  );

  let credentialMap = $derived.by(() => {
    const map: Record<string, SyncPairCredential> = {};
    for (const cred of credentials) {
      map[cred.domain.replace(/\/+$/, '')] = cred;
    }
    return map;
  });

  function isPaired(domain: string): boolean {
    return Boolean(credentialMap[domain.replace(/\/+$/, '')]);
  }

  async function loadPairs() {
    loading = true;
    error = '';
    try {
      const res = await listSyncPairs();
      if (res.success && res.data) {
        pairs = res.data.pairs || [];
        credentials = res.data.credentials || [];
      } else {
        error = (res as any).error?.message || $_('common.load_failed');
      }
      await loadSyncPending();
    } catch (e: any) {
      error = e.message || $_('common.load_failed');
    } finally {
      loading = false;
    }
  }

  async function loadSyncPending() {
    try {
      const res = await listSyncReview({ countOnly: false });
      if (res.success) {
        syncPending = res.data.items || [];
        syncPendingTotal = res.data.total ?? syncPending.length;
        syncPendingError = '';
      } else {
        syncPendingError = res.error?.message || $_('sync_pairs.review_load_failed');
      }
    } catch (e: any) {
      syncPendingError = e?.message || $_('sync_pairs.review_load_failed');
    }
  }

  async function refreshSyncReviewDetail(id: string) {
    try {
      const res = await getSyncReviewItem(id);
      if (res.success) reviewDetail = res.data.item;
      else showToast('error', $_('sync_pairs.review_load_failed'), res.error?.message);
    } catch (e: any) {
      showToast('error', $_('sync_pairs.review_load_failed'), e?.message);
    }
  }

  async function openSyncReview(id: string) {
    if (reviewBusy) return;
    reviewBusy = true;
    try {
      await refreshSyncReviewDetail(id);
    } finally {
      reviewBusy = false;
    }
  }

  async function persistSyncReviewEdits(detail: SyncReviewDetail): Promise<boolean> {
    const res = await updateSyncReviewItem(detail.id, {
      proposed_title: detail.proposed_title,
      proposed_content: detail.proposed_content,
      proposed_excerpt: detail.proposed_excerpt,
    });
    if (!res.success) {
      showToast('error', $_('common.save_failed'), res.error?.message);
      return false;
    }
    reviewDetail = res.data.item;
    return true;
  }

  async function saveSyncReviewEdits() {
    if (!reviewDetail || reviewBusy) return;
    reviewBusy = true;
    try {
      if (await persistSyncReviewEdits(reviewDetail)) {
        showToast('success', $_('sync_pairs.review_saved'));
      }
    } catch (e: any) {
      showToast('error', $_('common.save_failed'), e?.message);
    } finally {
      reviewBusy = false;
    }
  }

  async function approveSyncReview() {
    if (!reviewDetail || reviewBusy) return;
    const detail = reviewDetail;
    reviewBusy = true;
    let pushStarted = false;
    try {
      if (!await persistSyncReviewEdits(detail)) return;
      pushStarted = true;
      const res = await approveSyncReviewItem(detail.id);
      if (res.success) {
        showToast('success', $_('sync_pairs.review_approved'));
        reviewDetail = null;
        await loadSyncPending();
      } else {
        showToast('error', $_('sync_pairs.review_approve_failed'), res.error?.message);
        await refreshSyncReviewDetail(detail.id);
        await loadSyncPending();
      }
    } catch (e: any) {
      showToast('error', $_('sync_pairs.review_approve_failed'), e?.message);
      if (pushStarted) {
        await refreshSyncReviewDetail(detail.id);
        await loadSyncPending();
      }
    } finally {
      reviewBusy = false;
    }
  }

  async function rejectSyncReview() {
    if (!reviewDetail || reviewBusy) return;
    const id = reviewDetail.id;
    reviewBusy = true;
    try {
      const res = await rejectSyncReviewItem(id);
      if (res.success) {
        showToast('success', $_('sync_pairs.review_rejected'));
        reviewDetail = null;
        await loadSyncPending();
      } else {
        showToast('error', $_('sync_pairs.review_reject_failed'), res.error?.message);
        await refreshSyncReviewDetail(id);
        await loadSyncPending();
      }
    } catch (e: any) {
      showToast('error', $_('sync_pairs.review_reject_failed'), e?.message);
      await refreshSyncReviewDetail(id);
      await loadSyncPending();
    } finally {
      reviewBusy = false;
    }
  }

  async function loadComponentOptions() {
    if (componentOptions.length > 0) return;
    try {
      const res = await listAllLocalComponents('', true);
      if (res.success && res.data) {
        componentOptions = res.data.items.map((c) => ({
          id: c.id,
          name: c.name || c.id,
        }));
      }
    } catch {
      // Options stay empty; validation still happens server-side.
    }
  }

  function savedFieldAction(pair: SyncPair, field: string): string {
    const action = pair.field_actions?.find((row) => row.field === field)?.action;
    if (action === 'copy' || action === 'as_is') return 'copy';
    if (action === 'skip' || action === 'exclude') return 'skip';
    return 'translate';
  }

  function openCreateModal() {
    isEditing = false;
    formId = '';
    formName = '';
    formSourceDomain = wpmmccSites.length > 0 ? wpmmccSites[0].url : '';
    formTargetDomain = wpmmccSites.length > 1 ? wpmmccSites[1].url : '';
    formDirection = 'unidirectional';
    formSyncMode = 'sync_only';
    formSourceLang = 'en_US';
    formTargetLang = 'zh_CN';
    formConflictStrategy = 'lww';
    formSyncFrequency = 'manual';
    formPostTypes = ['post', 'page'];
    formTranslateComponentId = '';
    formTitleAction = 'translate';
    formContentAction = 'translate';
    formExcerptAction = 'translate';
    formReviewBeforePush = false;
    formError = '';
    modalOpen = true;
  }

  function openEditModal(pair: SyncPair) {
    isEditing = true;
    formId = pair.id;
    formName = pair.name || '';
    formSourceDomain = pair.source_domain;
    formTargetDomain = pair.target_domain;
    formDirection = pair.direction || 'unidirectional';
    formSyncMode = pair.sync_mode || 'sync_only';
    formSourceLang = pair.source_lang || 'en_US';
    formTargetLang = pair.target_lang || 'zh_CN';
    formConflictStrategy = pair.conflict_strategy || 'lww';
    formSyncFrequency = pair.sync_frequency || 'manual';
    formPostTypes = [...(pair.post_types || ['post', 'page'])];
    formTranslateComponentId = pair.translate_component_id || '';
    formTitleAction = savedFieldAction(pair, 'post_title');
    formContentAction = savedFieldAction(pair, 'post_content');
    formExcerptAction = savedFieldAction(pair, 'post_excerpt');
    formReviewBeforePush = !!pair.review_before_push;
    formError = '';
    modalOpen = true;
    if (formSyncMode === 'sync_and_translate') loadComponentOptions();
  }

  function closeModal() {
    modalOpen = false;
    formError = '';
  }

  function openPairingModal() {
    pairingError = '';
    pairingCode = '';
    pairingRole = 'source';
    pairingDomain = wpmmccSites.length > 0 ? wpmmccSites[0].url : '';
    pairingModalOpen = true;
  }

  function togglePostType(val: string) {
    if (formPostTypes.includes(val)) {
      formPostTypes = formPostTypes.filter((t) => t !== val);
    } else {
      formPostTypes = [...formPostTypes, val];
    }
  }

  async function handleSave() {
    formError = '';
    const src = formSourceDomain.trim();
    const tgt = formTargetDomain.trim();
    if (!src) {
      formError = $_('sync_pairs.select_source_placeholder');
      return;
    }
    if (!tgt) {
      formError = $_('sync_pairs.select_target_placeholder');
      return;
    }
    if (src === tgt) {
      formError = $_('sync_pairs.same_site_error');
      return;
    }
    if (formPostTypes.length === 0) {
      formError = '请至少选择一个内容类型 (post types)';
      return;
    }
    const fieldActions = [
      { field: 'post_title', action: formTitleAction },
      { field: 'post_content', action: formContentAction },
      { field: 'post_excerpt', action: formExcerptAction },
    ];
    if (
      formSyncMode === 'sync_and_translate' &&
      fieldActions.some((row) => row.action === 'translate') &&
      !formTranslateComponentId.trim()
    ) {
      formError = $_('sync_pairs.translate_component_required');
      return;
    }

    saving = true;
    try {
      const payload: SyncPairUpsertPayload = {
        name: formName.trim() || undefined,
        source_domain: src,
        target_domain: tgt,
        direction: formDirection,
        sync_mode: formSyncMode,
        source_lang: formSourceLang.trim() || 'en_US',
        target_lang: formTargetLang.trim() || 'zh_CN',
        conflict_strategy: formConflictStrategy,
        sync_frequency: formSyncFrequency,
        post_types: formPostTypes,
      };
      if (formSyncMode === 'sync_and_translate') {
        payload.translate_component_id = formTranslateComponentId.trim();
        payload.field_actions = fieldActions;
      }
      payload.review_before_push = formReviewBeforePush;
      if (isEditing && formId) {
        payload.id = formId;
      }

      const res = await upsertSyncPair(payload);
      if (res.success) {
        showToast('success', $_('sync_pairs.save_success'));
        closeModal();
        await loadPairs();
      } else {
        formError = (res as any).error?.message || $_('common.save_failed');
      }
    } catch (e: any) {
      formError = e.message || $_('login.network_error');
    } finally {
      saving = false;
    }
  }

  async function handlePair() {
    pairingError = '';
    const code = pairingCode.trim();
    if (!pairingDomain) {
      pairingError = $_('sync_pairs.pairing_select_site');
      return;
    }
    if (!/^[0-9a-fA-F]{32}$/.test(code)) {
      pairingError = $_('sync_pairs.pairing_code_invalid');
      return;
    }
    pairingBusy = true;
    try {
      const res = await pairSyncSite({
        domain: pairingDomain,
        pairing_code: code,
        role: pairingRole,
        conflict_strategy: pairingConflictStrategy,
      });
      if (res.success) {
        showToast('success', $_('sync_pairs.pairing_success', { values: { site: res.data.peer_name || pairingDomain } }));
        pairingCode = '';
        await loadPairs();
      } else {
        pairingError = (res as any).error?.message || $_('sync_pairs.pairing_failed');
      }
    } catch (e: any) {
      pairingError = e.message || $_('login.network_error');
    } finally {
      pairingBusy = false;
    }
  }

  async function handleUnpair(domain: string) {
    unpairingDomain = domain;
    try {
      const res = await unpairSyncSite(domain);
      if (res.success) {
        showToast('success', $_('sync_pairs.unpair_success'));
        await loadPairs();
      } else {
        showToast('error', $_('common.operation_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('login.network_error'), e.message);
    } finally {
      unpairingDomain = null;
    }
  }

  async function handleRun(pair: SyncPair) {
    runningMap[pair.id] = true;
    try {
      const res = await runSyncPair(pair.id);
      if (res.success) {
        showToast('success', $_('sync_pairs.run_triggered'));
        // The run executes in the background; poll the pair list for the
        // updated counts / last_error.
        setTimeout(loadPairs, 1500);
        setTimeout(loadPairs, 4000);
      } else {
        showToast('error', $_('common.operation_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('login.network_error'), e.message);
    } finally {
      runningMap[pair.id] = false;
    }
  }

  async function handleTogglePause(pair: SyncPair) {
    try {
      if (pair.status === 'paused') {
        const res = await resumeSyncPair(pair.id);
        if (res.success) {
          showToast('success', $_('sync_pairs.resume_success'));
          await loadPairs();
        } else {
          showToast('error', $_('common.operation_failed'), (res as any).error?.message);
        }
      } else {
        const res = await pauseSyncPair(pair.id);
        if (res.success) {
          showToast('success', $_('sync_pairs.pause_success'));
          await loadPairs();
        } else {
          showToast('error', $_('common.operation_failed'), (res as any).error?.message);
        }
      }
    } catch (e: any) {
      showToast('error', $_('login.network_error'), e.message);
    }
  }

  function openDeleteConfirm(pairId: string) {
    deletingPairId = pairId;
    deleteModalOpen = true;
  }

  async function confirmDelete() {
    if (!deletingPairId) return;
    deleting = true;
    try {
      const res = await deleteSyncPair(deletingPairId);
      if (res.success) {
        showToast('success', $_('sync_pairs.delete_success'));
        deleteModalOpen = false;
        deletingPairId = null;
        await loadPairs();
      } else {
        showToast('error', $_('common.operation_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('login.network_error'), e.message);
    } finally {
      deleting = false;
    }
  }

  function formatTs(ts?: number | null): string {
    if (!ts) return $_('sync_pairs.never_synced');
    return new Date(ts * 1000).toLocaleString();
  }

  function statusBadge(status: string) {
    switch (status) {
      case 'active':
        return { label: $_('sync_pairs.status_active'), cls: 'bg-emerald-50 text-emerald-700 border-emerald-200' };
      case 'paused':
        return { label: $_('sync_pairs.status_paused'), cls: 'bg-amber-50 text-amber-700 border-amber-200' };
      case 'error':
        return { label: $_('sync_pairs.status_error'), cls: 'bg-rose-50 text-rose-700 border-rose-200' };
      default:
        return { label: status, cls: 'bg-gray-50 text-gray-700 border-gray-200' };
    }
  }

  function directionLabel(dir: SyncDirection) {
    return dir === 'bidirectional'
      ? $_('sync_pairs.direction_bidirectional')
      : $_('sync_pairs.direction_unidirectional');
  }

  function modeLabel(mode: SyncMode) {
    return mode === 'sync_and_translate'
      ? $_('sync_pairs.mode_sync_and_translate')
      : $_('sync_pairs.mode_sync_only');
  }

  function conflictLabel(strat: ConflictStrategy) {
    switch (strat) {
      case 'lww':
        return $_('sync_pairs.conflict_lww');
      case 'source_wins':
        return $_('sync_pairs.conflict_source_wins');
      case 'target_wins':
        return $_('sync_pairs.conflict_target_wins');
      case 'manual_review':
        return $_('sync_pairs.conflict_manual');
      case 'merge':
        return $_('sync_pairs.conflict_merge');
      default:
        return strat;
    }
  }

  function frequencyLabel(freq: SyncFrequency) {
    switch (freq) {
      case 'manual':
        return $_('sync_pairs.freq_manual');
      case 'every_minute':
        return $_('sync_pairs.freq_every_minute');
      case 'hourly':
        return $_('sync_pairs.freq_hourly');
      case 'daily':
        return $_('sync_pairs.freq_daily');
      default:
        return freq;
    }
  }

  onMount(() => {
    loadPairs();
    fetchStatus();
  });
</script>

<div class="space-y-4">
  <!-- Sub-header with actions -->
  <div class="flex flex-wrap items-center justify-between gap-3 bg-white p-4 rounded-xl border border-gray-200">
    <div>
      <h3 class="font-semibold text-gray-900">{$_('sync_pairs.title')}</h3>
      <p class="text-xs text-gray-500 mt-0.5">{$_('sync_pairs.subtitle')}</p>
    </div>
    <div class="flex items-center gap-2">
      <button
        type="button"
        onclick={loadPairs}
        class="px-3 py-1.5 text-xs font-medium text-gray-700 bg-gray-50 hover:bg-gray-100 border border-gray-200 rounded-lg transition-colors"
      >
        {$_('common.refresh')}
      </button>
      <button
        type="button"
        onclick={openPairingModal}
        class="px-3.5 py-1.5 text-xs font-medium text-indigo-700 bg-indigo-50 hover:bg-indigo-100 border border-indigo-200 rounded-lg transition-colors flex items-center gap-1.5"
      >
        <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15 7a2 2 0 012 2m4 0a6 6 0 01-7.743 5.743L11 17H9v2H7v2H4a1 1 0 01-1-1v-2.586a1 1 0 01.293-.707l5.964-5.964A6 6 0 1121 7z" />
        </svg>
        {$_('sync_pairs.manage_pairing')}
      </button>
      <button
        type="button"
        onclick={openCreateModal}
        class="px-3.5 py-1.5 text-xs font-medium text-white bg-indigo-600 hover:bg-indigo-700 rounded-lg shadow-sm transition-colors flex items-center gap-1.5"
      >
        <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 4v16m8-8H4" />
        </svg>
        {$_('sync_pairs.create_pair')}
      </button>
    </div>
  </div>

  {#if error}
    <div class="p-4 bg-rose-50 border border-rose-200 text-rose-700 text-sm rounded-xl">
      {error}
    </div>
  {/if}

  {#if syncPendingError}
    <div role="alert" class="p-4 bg-rose-50 border border-rose-200 text-rose-700 text-sm rounded-xl">
      {$_('sync_pairs.review_load_failed')}: {syncPendingError}
    </div>
  {/if}

  {#if syncPendingTotal > 0}
    <div class="bg-amber-50 border border-amber-200 rounded-xl p-4" data-testid="sync-review-inbox">
      <div class="flex items-center justify-between gap-2 mb-2">
        <h4 class="text-sm font-medium text-amber-900">
          {$_('sync_pairs.review_inbox', { values: { count: syncPendingTotal } })}
        </h4>
        <button type="button" class="text-xs text-amber-800 hover:underline" onclick={loadSyncPending}>
          {$_('common.refresh')}
        </button>
      </div>
      <ul class="space-y-1.5">
        {#each syncPending as item (item.id)}
          <li class="flex items-center justify-between gap-2 text-xs bg-white/80 border border-amber-100 rounded-lg px-3 py-2">
            <div class="min-w-0">
              <span class="font-medium text-gray-800 truncate block">{item.proposed_title || item.source_title || item.canonical_uuid}</span>
              <span class="text-gray-400 font-mono">{item.pair_id}</span>
              {#if item.error_message}
                <span class="block text-rose-600 mt-0.5">{item.error_message}</span>
              {/if}
            </div>
            <button
              type="button"
              class="shrink-0 text-amber-800 font-medium hover:underline"
              onclick={() => openSyncReview(item.id)}
            >
              {$_('sync_pairs.review_open')}
            </button>
          </li>
        {/each}
      </ul>
    </div>
  {/if}

  {#if loading}
    <div class="p-12 text-center text-sm text-gray-400 bg-white rounded-xl border border-gray-200">
      {$_('common.loading')}
    </div>
  {:else if pairs.length === 0}
    <div class="p-10 text-center bg-white rounded-xl border border-dashed border-gray-300">
      <div class="w-12 h-12 mx-auto mb-3 rounded-full bg-indigo-50 flex items-center justify-center text-indigo-600">
        <svg class="w-6 h-6" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M8 7h12m0 0l-4-4m4 4l-4 4m0 6H4m0 0l4 4m-4-4l4-4" />
        </svg>
      </div>
      <h4 class="font-medium text-gray-900 text-sm mb-1">{$_('sync_pairs.empty')}</h4>
      <p class="text-xs text-gray-500 max-w-md mx-auto mb-4">{$_('sync_pairs.empty_hint')}</p>
      <button
        type="button"
        onclick={openCreateModal}
        class="px-4 py-2 text-xs font-medium text-white bg-indigo-600 hover:bg-indigo-700 rounded-lg shadow-sm transition-colors"
      >
        {$_('sync_pairs.create_pair')}
      </button>
    </div>
  {:else}
    <div class="grid grid-cols-1 gap-4">
      {#each pairs as pair (pair.id)}
        {@const sb = statusBadge(pair.status)}
        <div class="bg-white rounded-xl border border-gray-200 shadow-xs hover:border-gray-300 transition-all p-5">
          <!-- Card Top: Source -> Target & Status & Actions -->
          <div class="flex flex-wrap items-center justify-between gap-3 pb-4 border-b border-gray-100">
            <div class="flex items-center gap-2.5 flex-wrap">
              <span class="px-2.5 py-1 text-xs font-mono font-medium bg-gray-100 text-gray-800 rounded-md border border-gray-200 flex items-center gap-1.5">
                {pair.source_domain}
                {#if isPaired(pair.source_domain)}
                  <span class="text-emerald-600" title={$_('sync_pairs.paired')}>&#10003;</span>
                {:else}
                  <span class="text-amber-500" title={$_('sync_pairs.not_paired')}>&#9888;</span>
                {/if}
              </span>
              <div class="flex items-center gap-1 text-gray-400">
                {#if pair.direction === 'bidirectional'}
                  <span class="text-sm font-bold text-indigo-600">⇄</span>
                {:else}
                  <span class="text-sm font-bold text-gray-600">→</span>
                {/if}
              </div>
              <span class="px-2.5 py-1 text-xs font-mono font-medium bg-gray-100 text-gray-800 rounded-md border border-gray-200 flex items-center gap-1.5">
                {pair.target_domain}
                {#if isPaired(pair.target_domain)}
                  <span class="text-emerald-600" title={$_('sync_pairs.paired')}>&#10003;</span>
                {:else}
                  <span class="text-amber-500" title={$_('sync_pairs.not_paired')}>&#9888;</span>
                {/if}
              </span>
              {#if !isPaired(pair.source_domain) || !isPaired(pair.target_domain)}
                <span class="px-2 py-0.5 text-xs font-medium rounded-full border bg-amber-50 text-amber-700 border-amber-200">
                  {$_('sync_pairs.pairing_required')}
                </span>
              {/if}
              {#if pair.name}
                <span class="text-xs text-gray-400">({pair.name})</span>
              {/if}
              <span class="px-2 py-0.5 text-xs font-medium rounded-full border {sb.cls}">
                {sb.label}
              </span>
            </div>

            <div class="flex items-center gap-2">
              <button
                type="button"
                disabled={runningMap[pair.id]}
                onclick={() => handleRun(pair)}
                class="px-3 py-1.5 text-xs font-medium text-indigo-700 bg-indigo-50 hover:bg-indigo-100 border border-indigo-200 rounded-lg transition-colors disabled:opacity-50 flex items-center gap-1"
              >
                {#if runningMap[pair.id]}
                  <svg class="animate-spin h-3.5 w-3.5 text-indigo-700" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                    <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                    <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"></path>
                  </svg>
                  {$_('sync_pairs.running')}
                {:else}
                  {$_('sync_pairs.run_now')}
                {/if}
              </button>

              <button
                type="button"
                onclick={() => handleTogglePause(pair)}
                class="px-3 py-1.5 text-xs font-medium text-gray-700 bg-gray-50 hover:bg-gray-100 border border-gray-200 rounded-lg transition-colors"
              >
                {pair.status === 'paused' ? $_('sync_pairs.resume') : $_('sync_pairs.pause')}
              </button>

              <button
                type="button"
                onclick={() => openEditModal(pair)}
                class="px-3 py-1.5 text-xs font-medium text-gray-700 bg-white hover:bg-gray-50 border border-gray-200 rounded-lg transition-colors"
              >
                {$_('common.edit')}
              </button>

              <button
                type="button"
                onclick={() => openDeleteConfirm(pair.id)}
                class="px-3 py-1.5 text-xs font-medium text-rose-600 bg-rose-50 hover:bg-rose-100 border border-rose-200 rounded-lg transition-colors"
              >
                {$_('sync_pairs.delete')}
              </button>
            </div>
          </div>

          <!-- Card Body: Grid of details -->
          <div class="grid grid-cols-2 md:grid-cols-4 gap-4 pt-4 text-xs">
            <div>
              <span class="text-gray-400 block mb-0.5">{$_('sync_pairs.direction')} / {$_('sync_pairs.sync_mode')}</span>
              <span class="font-medium text-gray-700">
                {directionLabel(pair.direction)} · {modeLabel(pair.sync_mode)}
              </span>
            </div>

            <div>
              <span class="text-gray-400 block mb-0.5">{$_('sync_pairs.frequency')} / 冲突策略</span>
              <span class="font-medium text-gray-700">
                {frequencyLabel(pair.sync_frequency)} · {conflictLabel(pair.conflict_strategy)}
              </span>
            </div>

            <div>
              <span class="text-gray-400 block mb-0.5">语言 / 类型</span>
              <span class="font-medium text-gray-700">
                {pair.source_lang} → {pair.target_lang} · {(pair.post_types || []).join(', ')}
              </span>
            </div>

            <div>
              <span class="text-gray-400 block mb-0.5">{$_('sync_pairs.last_sync')} / 计数</span>
              <span class="font-medium text-gray-700">
                {formatTs(pair.last_sync_at)} · {pair.last_sync_count ?? 0} 次
              </span>
            </div>
          </div>

          {#if pair.last_error}
            <div class="mt-3 p-3 bg-rose-50 border border-rose-200 rounded-lg text-xs text-rose-800 flex items-start gap-2">
              <svg class="w-4 h-4 text-rose-600 shrink-0 mt-0.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 8v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
              </svg>
              <div>
                <span class="font-semibold">同步失败：</span>{pair.last_error}
              </div>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<!-- ==================== CREATE / EDIT MODAL ==================== -->
{#if modalOpen}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/40 backdrop-blur-xs"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    aria-label={isEditing ? $_('sync_pairs.edit_pair') : $_('sync_pairs.create_pair')}
    use:modalA11y={{ onClose: closeModal }}
    onclick={(e) => { if (e.target === e.currentTarget) closeModal(); }}
    onkeydown={(e) => { if (e.key === 'Escape') { e.preventDefault(); closeModal(); } }}>
    <div class="bg-white rounded-2xl shadow-xl border border-gray-100 w-full max-w-lg overflow-hidden">
      <!-- Modal Header -->
      <div class="px-6 py-4 border-b border-gray-100 flex items-center justify-between">
        <h3 class="font-semibold text-gray-900 text-base">
          {isEditing ? $_('sync_pairs.edit_pair') : $_('sync_pairs.create_pair')}
        </h3>
        <button
          type="button"
          onclick={closeModal}
          aria-label={$_('common.close')}
          class="text-gray-400 hover:text-gray-600 p-1 rounded-lg"
        >
          <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12" />
          </svg>
        </button>
      </div>

      <!-- Modal Body -->
      <div class="p-6 space-y-4 max-h-[75vh] overflow-y-auto text-sm">
        {#if formError}
          <div class="p-3 bg-rose-50 border border-rose-200 text-rose-700 text-xs rounded-lg">
            {formError}
          </div>
        {/if}

        <div class="text-xs text-amber-700 bg-amber-50 p-3 rounded-lg border border-amber-200">
          {$_('sync_pairs.wpmmcc_only_hint')}
        </div>

        {#if wpmmccSites.length < 2}
          <div class="text-xs text-rose-700 bg-rose-50 p-3 rounded-lg border border-rose-200 flex items-center justify-between">
            <span>{$_('sync_pairs.insufficient_sites_warning', { values: { count: wpmmccSites.length } })}</span>
            <a href="#sites" onclick={() => { closeModal(); window.location.hash = 'sites'; }} class="text-xs font-semibold text-rose-800 underline ml-2 whitespace-nowrap">
              {$_('sync_pairs.go_to_sites')}
            </a>
          </div>
        {/if}

        <div>
          <label for="sync-pair-name" class="block text-xs font-medium text-gray-700 mb-1">
            {$_('sync_pairs.name_label')}
          </label>
          <input
            id="sync-pair-name"
            type="text"
            bind:value={formName}
            placeholder={$_('sync_pairs.name_placeholder')}
            class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
          />
        </div>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div>
            <label for="sync-pair-source" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.source_site')} *
            </label>
            {#if wpmmccSites.length > 0}
              <select
                id="sync-pair-source"
                bind:value={formSourceDomain}
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
              >
                <option value="" disabled>{$_('sync_pairs.select_source_placeholder')}</option>
                {#each wpmmccSites as site}
                  <option value={site.url}>
                    {site.url} (wpmmcc)
                  </option>
                {/each}
              </select>
            {:else}
              <input
                id="sync-pair-source"
                type="text"
                bind:value={formSourceDomain}
                placeholder="https://site-a.com"
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500 font-mono"
              />
            {/if}
          </div>

          <div>
            <label for="sync-pair-target" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.target_site')} *
            </label>
            {#if wpmmccSites.length > 0}
              <select
                id="sync-pair-target"
                bind:value={formTargetDomain}
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
              >
                <option value="" disabled>{$_('sync_pairs.select_target_placeholder')}</option>
                {#each wpmmccSites as site}
                  <option value={site.url}>
                    {site.url} (wpmmcc)
                  </option>
                {/each}
              </select>
            {:else}
              <input
                id="sync-pair-target"
                type="text"
                bind:value={formTargetDomain}
                placeholder="https://site-b.com"
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500 font-mono"
              />
            {/if}
          </div>
        </div>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div>
            <label for="sync-pair-direction" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.direction')}
            </label>
            <select
              id="sync-pair-direction"
              bind:value={formDirection}
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            >
              <option value="unidirectional">{$_('sync_pairs.direction_unidirectional')}</option>
              <option value="bidirectional">{$_('sync_pairs.direction_bidirectional')}</option>
            </select>
          </div>

          <div>
            <label for="sync-pair-mode" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.sync_mode')}
            </label>
            <select
              id="sync-pair-mode"
              bind:value={formSyncMode}
              onchange={() => {
                if (formSyncMode === 'sync_and_translate') loadComponentOptions();
              }}
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            >
              <option value="sync_only">{$_('sync_pairs.mode_sync_only')}</option>
              <option value="sync_and_translate">{$_('sync_pairs.mode_sync_and_translate')}</option>
            </select>
          </div>
        </div>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div>
            <label for="sync-pair-source-lang" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.source_lang')}
            </label>
            <input
              id="sync-pair-source-lang"
              type="text"
              bind:value={formSourceLang}
              placeholder="en_US"
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            />
          </div>

          <div>
            <label for="sync-pair-target-lang" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.target_lang')}
            </label>
            <input
              id="sync-pair-target-lang"
              type="text"
              bind:value={formTargetLang}
              placeholder="zh_CN"
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            />
          </div>
        </div>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
          <div>
            <label for="sync-pair-conflict" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.conflict_strategy')}
            </label>
            <select
              id="sync-pair-conflict"
              bind:value={formConflictStrategy}
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            >
              <option value="lww">{$_('sync_pairs.conflict_lww')}</option>
              <option value="source_wins">{$_('sync_pairs.conflict_source_wins')}</option>
              <option value="target_wins">{$_('sync_pairs.conflict_target_wins')}</option>
              <option value="manual_review">{$_('sync_pairs.conflict_manual')}</option>
              <option value="merge">{$_('sync_pairs.conflict_merge')}</option>
            </select>
          </div>

          <div>
            <label for="sync-pair-frequency" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.frequency')}
            </label>
            <select
              id="sync-pair-frequency"
              bind:value={formSyncFrequency}
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            >
              <option value="manual">{$_('sync_pairs.freq_manual')}</option>
              <option value="every_minute">{$_('sync_pairs.freq_every_minute')}</option>
              <option value="hourly">{$_('sync_pairs.freq_hourly')}</option>
              <option value="daily">{$_('sync_pairs.freq_daily')}</option>
            </select>
          </div>
        </div>

        {#if formSyncMode === 'sync_and_translate'}
          <div>
            <label for="sync-pair-translate-component" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.translate_component')} *
            </label>
            <select
              id="sync-pair-translate-component"
              bind:value={formTranslateComponentId}
              onchange={() => loadComponentOptions()}
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            >
              <option value="">{$_('sync_pairs.translate_component_select')}</option>
              {#each componentOptions as comp}
                <option value={comp.id}>{comp.name} ({comp.id})</option>
              {/each}
            </select>
            <p class="text-xs text-gray-400 mt-1">{$_('sync_pairs.translate_component_hint')}</p>
          </div>
          <div class="grid grid-cols-1 sm:grid-cols-3 gap-3">
            {#each [
              ['post_title', 'sync_pairs.field_action_title', formTitleAction],
              ['post_content', 'sync_pairs.field_action_content', formContentAction],
              ['post_excerpt', 'sync_pairs.field_action_excerpt', formExcerptAction],
            ] as [field, label, selected]}
              <label class="block text-xs font-medium text-gray-700">
                {$_(label)}
                <select
                  value={selected}
                  onchange={(event) => {
                    const value = (event.currentTarget as HTMLSelectElement).value;
                    if (field === 'post_title') formTitleAction = value;
                    else if (field === 'post_content') formContentAction = value;
                    else formExcerptAction = value;
                  }}
                  class="mt-1 w-full px-3 py-2 border border-gray-300 rounded-lg text-sm"
                >
                  <option value="translate">{$_('sync_pairs.field_action_translate')}</option>
                  <option value="copy">{$_('sync_pairs.field_action_copy')}</option>
                  <option value="skip">{$_('sync_pairs.field_action_skip')}</option>
                </select>
              </label>
            {/each}
          </div>
        {/if}

        <label class="flex items-start gap-2 text-sm text-gray-700">
          <input type="checkbox" class="mt-0.5" bind:checked={formReviewBeforePush} data-testid="sync-review-before-push" />
          <span>
            <span class="font-medium">{$_('sync_pairs.review_before_push')}</span>
            <span class="block text-xs text-gray-400 mt-0.5">{$_('sync_pairs.review_before_push_hint')}</span>
          </span>
        </label>

        <div>
          <span class="block text-xs font-medium text-gray-700 mb-1.5">
            {$_('sync_pairs.post_types')}
          </span>
          <div class="flex flex-wrap gap-3">
            {#each POST_TYPE_OPTIONS as opt}
              <label class="flex items-center gap-1.5 text-xs text-gray-700 cursor-pointer">
                <input
                  type="checkbox"
                  checked={formPostTypes.includes(opt)}
                  onchange={() => togglePostType(opt)}
                  class="rounded text-indigo-600 focus:ring-indigo-500"
                />
                <span>{$_(`sync_pairs.post_type_${opt}`)}</span>
              </label>
            {/each}
          </div>
        </div>
      </div>

      <!-- Modal Footer -->
      <div class="px-6 py-3.5 bg-gray-50 border-t border-gray-100 flex items-center justify-end gap-2">
        <button
          type="button"
          onclick={closeModal}
          class="px-4 py-2 text-xs font-medium text-gray-700 bg-white hover:bg-gray-100 border border-gray-200 rounded-lg transition-colors"
        >
          {$_('common.cancel')}
        </button>
        <button
          type="button"
          disabled={saving}
          onclick={handleSave}
          class="px-4 py-2 text-xs font-medium text-white bg-indigo-600 hover:bg-indigo-700 rounded-lg shadow-sm transition-colors disabled:opacity-50"
        >
          {saving ? $_('common.saving') : $_('common.save')}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- ==================== PAIRING MANAGEMENT MODAL ==================== -->
{#if pairingModalOpen}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/40 backdrop-blur-xs"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    aria-label={$_('sync_pairs.manage_pairing')}
    use:modalA11y={{ onClose: () => (pairingModalOpen = false) }}
    onclick={(e) => { if (e.target === e.currentTarget) pairingModalOpen = false; }}
    onkeydown={(e) => { if (e.key === 'Escape') { e.preventDefault(); pairingModalOpen = false; } }}>
    <div class="bg-white rounded-2xl shadow-xl border border-gray-100 w-full max-w-lg overflow-hidden">
      <div class="px-6 py-4 border-b border-gray-100 flex items-center justify-between">
        <h3 class="font-semibold text-gray-900 text-base">{$_('sync_pairs.manage_pairing')}</h3>
        <button
          type="button"
          onclick={() => (pairingModalOpen = false)}
          aria-label={$_('common.close')}
          class="text-gray-400 hover:text-gray-600 p-1 rounded-lg"
        >
          <svg class="w-5 h-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M6 18L18 6M6 6l12 12" />
          </svg>
        </button>
      </div>

      <div class="p-6 space-y-4 max-h-[75vh] overflow-y-auto text-sm">
        {#if pairingError}
          <div class="p-3 bg-rose-50 border border-rose-200 text-rose-700 text-xs rounded-lg">
            {pairingError}
          </div>
        {/if}

        {#if wpmmccSites.length === 0}
          <div class="p-3 bg-amber-50 border border-amber-200 text-amber-700 text-xs rounded-lg">
            {$_('sync_pairs.no_wpmmcc_sites')}
          </div>
        {:else}
          <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div>
              <label for="pairing-domain" class="block text-xs font-medium text-gray-700 mb-1">
                {$_('sync_pairs.pairing_site')} *
              </label>
              <select
                id="pairing-domain"
                bind:value={pairingDomain}
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
              >
                <option value="" disabled>{$_('sync_pairs.pairing_select_site')}</option>
                {#each wpmmccSites as site}
                  <option value={site.url}>{site.url}</option>
                {/each}
              </select>
            </div>
            <div>
              <label for="pairing-role" class="block text-xs font-medium text-gray-700 mb-1">
                {$_('sync_pairs.pairing_role')}
              </label>
              <select
                id="pairing-role"
                bind:value={pairingRole}
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
              >
                <option value="source">{$_('sync_pairs.role_source')}</option>
                <option value="target">{$_('sync_pairs.role_target')}</option>
              </select>
            </div>
            <div>
              <label for="pairing-conflict" class="block text-xs font-medium text-gray-700 mb-1">
                {$_('sync_pairs.conflict_strategy')}
              </label>
              <select
                id="pairing-conflict"
                bind:value={pairingConflictStrategy}
                class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
              >
                <option value="lww">{$_('sync_pairs.conflict_lww')}</option>
                <option value="source_wins">{$_('sync_pairs.conflict_source_wins')}</option>
                <option value="target_wins">{$_('sync_pairs.conflict_target_wins')}</option>
                <option value="manual_review">{$_('sync_pairs.conflict_manual')}</option>
                <option value="merge">{$_('sync_pairs.conflict_merge')}</option>
              </select>
            </div>
          </div>

          <div>
            <label for="pairing-code" class="block text-xs font-medium text-gray-700 mb-1">
              {$_('sync_pairs.pairing_code')} *
            </label>
            <input
              id="pairing-code"
              type="text"
              bind:value={pairingCode}
              placeholder="0123456789abcdef0123456789abcdef"
              maxlength="32"
              class="w-full px-3 py-2 border border-gray-300 rounded-lg text-sm font-mono focus:outline-hidden focus:ring-2 focus:ring-indigo-500"
            />
            <p class="text-xs text-gray-400 mt-1">{$_('sync_pairs.pairing_code_hint')}</p>
          </div>

          <button
            type="button"
            disabled={pairingBusy}
            onclick={handlePair}
            class="px-4 py-2 text-xs font-medium text-white bg-indigo-600 hover:bg-indigo-700 rounded-lg shadow-sm transition-colors disabled:opacity-50"
          >
            {pairingBusy ? $_('common.saving') : $_('sync_pairs.pair_now')}
          </button>
        {/if}

        <!-- Existing credentials -->
        <div class="pt-2 border-t border-gray-100">
          <h4 class="text-xs font-semibold text-gray-700 mb-2">{$_('sync_pairs.paired_sites')}</h4>
          {#if credentials.length === 0}
            <p class="text-xs text-gray-400">{$_('sync_pairs.no_paired_sites')}</p>
          {:else}
            <div class="space-y-2">
              {#each credentials as cred (cred.domain)}
                <div class="flex items-center justify-between gap-3 p-2.5 bg-gray-50 border border-gray-200 rounded-lg">
                  <div class="min-w-0">
                    <p class="text-xs font-mono text-gray-800 truncate">{cred.domain}</p>
                    <p class="text-xs text-gray-400">
                      {cred.paired_as === 'source' ? $_('sync_pairs.role_source') : $_('sync_pairs.role_target')}
                      · {cred.negotiated_direction}
                      · {new Date(cred.paired_at * 1000).toLocaleString()}
                    </p>
                  </div>
                  <button
                    type="button"
                    disabled={unpairingDomain === cred.domain}
                    onclick={() => handleUnpair(cred.domain)}
                    class="px-3 py-1 text-xs font-medium text-rose-600 bg-rose-50 hover:bg-rose-100 border border-rose-200 rounded-lg transition-colors disabled:opacity-50 shrink-0"
                  >
                    {$_('sync_pairs.unpair')}
                  </button>
                </div>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    </div>
  </div>
{/if}

<!-- ==================== DELETE CONFIRM MODAL ==================== -->
{#if deleteModalOpen}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/40 backdrop-blur-xs"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    aria-label={$_('sync_pairs.delete')}
    use:modalA11y={{ onClose: () => { deleteModalOpen = false; deletingPairId = null; } }}
    onclick={(e) => { if (e.target === e.currentTarget) { deleteModalOpen = false; deletingPairId = null; } }}
    onkeydown={(e) => { if (e.key === 'Escape') { e.preventDefault(); deleteModalOpen = false; deletingPairId = null; } }}>
    <div class="bg-white rounded-2xl shadow-xl border border-gray-100 w-full max-w-sm p-5 text-sm">
      <h3 class="font-semibold text-gray-900 text-base mb-2">{$_('sync_pairs.delete')}</h3>
      <p class="text-xs text-gray-600 mb-5">{$_('sync_pairs.delete_confirm')}</p>
      <div class="flex items-center justify-end gap-2">
        <button
          type="button"
          onclick={() => { deleteModalOpen = false; deletingPairId = null; }}
          class="px-3.5 py-1.5 text-xs font-medium text-gray-700 bg-white hover:bg-gray-100 border border-gray-200 rounded-lg transition-colors"
        >
          {$_('common.cancel')}
        </button>
        <button
          type="button"
          disabled={deleting}
          onclick={confirmDelete}
          class="px-3.5 py-1.5 text-xs font-medium text-white bg-rose-600 hover:bg-rose-700 rounded-lg shadow-sm transition-colors disabled:opacity-50"
        >
          {deleting ? $_('common.deleting') : $_('sync_pairs.delete')}
        </button>
      </div>
    </div>
  </div>
{/if}

{#if reviewDetail}
  <div
    class="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/40 backdrop-blur-xs"
    data-testid="sync-review-modal"
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    aria-label={$_('sync_pairs.review_diff_title')}
    use:modalA11y={{ onClose: () => { if (!reviewBusy) reviewDetail = null; } }}
    onclick={(e) => { if (!reviewBusy && e.target === e.currentTarget) reviewDetail = null; }}
    onkeydown={(e) => { if (e.key === 'Escape') { e.preventDefault(); if (!reviewBusy) reviewDetail = null; } }}
  >
    <div class="bg-white rounded-2xl shadow-xl border border-gray-100 w-full max-w-3xl max-h-[90vh] overflow-y-auto p-5 text-sm">
      <div class="flex items-start justify-between gap-3 mb-4">
        <div>
          <h3 class="font-semibold text-gray-900 text-base">{$_('sync_pairs.review_diff_title')}</h3>
          <p class="text-xs text-gray-500 mt-0.5 font-mono">{reviewDetail.canonical_uuid}</p>
        </div>
        <button type="button" class="text-xs text-gray-500 hover:text-gray-800" disabled={reviewBusy} onclick={() => (reviewDetail = null)}>
          {$_('common.close')}
        </button>
      </div>
      <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div>
          <p class="text-xs font-medium text-gray-500 mb-1">{$_('sync_pairs.review_source')}</p>
          <label class="block text-xs text-gray-400 mb-0.5" for="sync-review-source-title">Title</label>
          <textarea id="sync-review-source-title" class="w-full border border-gray-200 rounded-lg px-2 py-1.5 text-xs font-mono bg-gray-50" rows="2" readonly value={reviewDetail.source_title}></textarea>
          <label class="block text-xs text-gray-400 mb-0.5 mt-2" for="sync-review-source-content">Content</label>
          <textarea id="sync-review-source-content" class="w-full border border-gray-200 rounded-lg px-2 py-1.5 text-xs font-mono bg-gray-50" rows="8" readonly value={reviewDetail.source_content}></textarea>
        </div>
        <div>
          <p class="text-xs font-medium text-gray-500 mb-1">{$_('sync_pairs.review_proposed')}</p>
          <label class="block text-xs text-gray-400 mb-0.5" for="sync-review-proposed-title">Title</label>
          <textarea id="sync-review-proposed-title" class="w-full border border-gray-200 rounded-lg px-2 py-1.5 text-xs font-mono" rows="2" disabled={reviewBusy} bind:value={reviewDetail.proposed_title}></textarea>
          <label class="block text-xs text-gray-400 mb-0.5 mt-2" for="sync-review-proposed-content">Content</label>
          <textarea id="sync-review-proposed-content" class="w-full border border-gray-200 rounded-lg px-2 py-1.5 text-xs font-mono" rows="8" disabled={reviewBusy} bind:value={reviewDetail.proposed_content}></textarea>
          <label class="block text-xs text-gray-400 mb-0.5 mt-2" for="sync-review-proposed-excerpt">Excerpt</label>
          <textarea id="sync-review-proposed-excerpt" class="w-full border border-gray-200 rounded-lg px-2 py-1.5 text-xs font-mono" rows="2" disabled={reviewBusy} bind:value={reviewDetail.proposed_excerpt}></textarea>
        </div>
      </div>
      {#if reviewDetail.error_message}
        <p class="mt-3 text-xs text-rose-600 bg-rose-50 border border-rose-100 rounded px-2 py-1.5">{reviewDetail.error_message}</p>
      {/if}
      <div class="flex flex-wrap justify-end gap-2 mt-5">
        <button type="button" class="px-3 py-1.5 text-xs border border-gray-200 rounded-lg" disabled={reviewBusy} onclick={saveSyncReviewEdits}>
          {$_('common.save')}
        </button>
        <button type="button" class="px-3 py-1.5 text-xs border border-rose-200 text-rose-700 rounded-lg" disabled={reviewBusy} onclick={rejectSyncReview}>
          {$_('review.reject')}
        </button>
        <button type="button" class="px-3 py-1.5 text-xs bg-amber-600 text-white rounded-lg" disabled={reviewBusy} onclick={approveSyncReview} data-testid="sync-review-approve">
          {$_('sync_pairs.review_approve_push')}
        </button>
      </div>
    </div>
  </div>
{/if}
