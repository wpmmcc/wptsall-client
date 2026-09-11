<script lang="ts">
  import { onMount } from 'svelte';
  import { hasData, isOk } from '../lib/api/client';
  import {
    getOverviewStats,
    refreshComponents as reloadComponents,
    refreshDomains as reloadDomains,
    runWorkerOnce,
    startWorkerLoopCheck,
    saveWorkerConfig,
    startWorkerLoop,
    stopWorkerLoop,
    type OverviewStatsData,
    type WorkerStartPreflightData,
  } from '../lib/api/worker';
  import { getWpClientApiToastCopy } from '../lib/errors/wpClientApi';
  import {
    formatBusinessLineLabel,
    formatCapabilityLabel,
    formatDeliveryTargetLabel,
    formatMissingComponentBehaviorLabel,
    formatPreflightPolicyLabel,
    formatRoutingProfileLabel,
    formatRuleSlotLabel,
    formatSeverityLabel,
    formatSourceGroupLabel,
    formatSourceRoleLabel,
  } from '../lib/components-page/helpers';
  import type { WorkerRunRecord } from '../lib/api/types';
  import { status, fetchStatus } from '../lib/stores/status';
  import { showToast } from '../lib/stores/toast';
  import { _ } from 'svelte-i18n';

  let pollInput = $state('20');
  let runLoading = $state(false);
  let startLoading = $state(false);
  let stopLoading = $state(false);
  let selectedRunIdx = $state(0);
  let stats = $state<OverviewStatsData | null>(null);
  let showPreflightModal = $state(false);
  let preflightModalTitle = $state('');
  let preflightModalData = $state<WorkerStartPreflightData | null>(null);
  let preflightResolver: ((value: boolean | null) => void) | null = null;
  let showDetails = $state(true);

  let runs = $derived(($status?.worker_recent_runs ?? []) as WorkerRunRecord[]);

  function workerStatusLabel(status?: string, running?: boolean): string {
    switch (status) {
      case 'running_manual': return $_('overview.status_manual_running');
      case 'running_auto': return $_('overview.status_auto_running');
      case 'waiting': return $_('overview.status_waiting');
      case 'completed': return $_('overview.status_completed');
      case 'error': return $_('overview.status_error');
      case 'stopping': return $_('overview.status_stopping');
      case 'idle': return $_('overview.status_idle');
      default: return running ? $_('overview.status_running') : $_('overview.status_stopped');
    }
  }
  let selectedRun = $derived(runs[selectedRunIdx] ?? null);

  async function runOnce() {
    runLoading = true;
    try {
      const preflight = await startWorkerLoopCheck();
      if (!isOk(preflight)) {
        showToast('error', $_('overview.preflight_failed'), preflight.error?.message);
        return;
      }
      const forceStart = await confirmWorkerStartPreflight(preflight.data);
      if (forceStart === null) return;

      const r = await runWorkerOnce();
      if (isOk(r)) { showToast('success', $_('overview.worker_done')); void fetchStatus(); }
      else {
        const toast = getWpClientApiToastCopy($_('overview.run_failed'), r.error, 'worker_run');
        showToast('error', toast.message, toast.detail);
      }
    } finally { runLoading = false; }
  }

  async function startLoop() {
    startLoading = true;
    try {
      const preflight = await startWorkerLoopCheck();
      if (!isOk(preflight)) {
        showToast('error', $_('overview.start_check_failed'), preflight.error?.message);
        return;
      }

      const forceStart = await confirmWorkerStartPreflight(preflight.data);
      if (forceStart === null) return;

      const r = await startWorkerLoop({ force: forceStart });
      if (isOk(r)) { showToast('success', $_('overview.worker_started')); await fetchStatus(); }
      else showToast('error', $_('overview.start_failed'), r.error?.message);
    } finally { startLoading = false; }
  }

  async function confirmWorkerStartPreflight(data: WorkerStartPreflightData | undefined) {
    if (!data || data.missing_components.length === 0) return false;
    if (data.can_start && !data.requires_confirmation) return false;
    preflightModalTitle = $_('overview.preflight_title');
    preflightModalData = data;
    showPreflightModal = true;
    return await new Promise<boolean | null>((resolve) => {
      preflightResolver = resolve;
    });
  }

  function closePreflightModal(result: boolean | null) {
    showPreflightModal = false;
    const resolver = preflightResolver;
    preflightResolver = null;
    if (result === null || result === false) {
      showToast('info', $_('overview.preflight_cancel'), $_('overview.preflight_cancel_detail'));
    }
    resolver?.(result);
  }

  function renderArtifactSummary(item: {
    input_artifact_kind?: string | null;
    expected_output_artifact_kind?: string | null;
  }) {
    const input = item.input_artifact_kind?.trim();
    const output = item.expected_output_artifact_kind?.trim();
    if (!input && !output) return '-';
    return `${input || '-'} -> ${output || '-'}`;
  }

  async function stopLoop() {
    stopLoading = true;
    try {
      const r = await stopWorkerLoop();
      if (isOk(r)) { showToast('info', $_('overview.worker_stopped')); await fetchStatus(); }
      else showToast('error', $_('overview.stop_failed'), r.error?.message);
    } finally { stopLoading = false; }
  }

  async function savePoll() {
    const secs = Math.max(1, Math.min(3600, parseInt(pollInput) || 20));
    const r = await saveWorkerConfig({ poll_seconds: secs });
    if (isOk(r)) showToast('success', $_('overview.poll_saved', { values: { secs } }));
    else showToast('error', $_('common.save_failed'), r.error?.message);
  }

  async function refreshDomains() {
    const r = await reloadDomains();
    if (isOk(r)) {
      await fetchStatus();
      showToast('success', $_('overview.domains_refreshed'));
    } else {
      showToast('error', $_('overview.domains_refresh_failed'), r.error?.message);
    }
  }

  async function refreshComponents() {
    const r = await reloadComponents();
    if (isOk(r)) {
      await fetchStatus();
      const backfillError = r.data?.local_components_backfill_error;
      const backfilled = r.data?.local_components_backfilled ?? 0;
      if (backfillError) {
        showToast('error', $_('overview.components_refresh_backfill_error'), backfillError);
      } else {
        showToast('success', backfilled > 0 ? $_('overview.components_refreshed_backfill', { values: { count: backfilled } }) : $_('overview.components_refreshed'));
      }
    } else {
      showToast('error', $_('overview.components_refresh_failed'), r.error?.message);
    }
  }

  async function loadStats() {
    const r = await getOverviewStats();
    if (hasData(r)) stats = r.data;
  }

  onMount(() => {
    if ($status?.worker_loop_poll_seconds) pollInput = String($status.worker_loop_poll_seconds);
    loadStats();
  });
</script>

<!-- 页面标题 -->
<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('overview.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('overview.subtitle')}</p>
</div>

{#if showPreflightModal && preflightModalData}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('overview.preflight_title')}
    onclick={(e) => {
      if (e.target === e.currentTarget) closePreflightModal(null);
    }}
    onkeydown={(e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        closePreflightModal(null);
      }
    }}>
    <div class="w-full max-w-6xl bg-white rounded-2xl shadow-xl border border-gray-200 overflow-hidden">
      <div class="px-5 py-4 border-b border-gray-100 flex items-start justify-between gap-4">
        <div>
          <h3 class="text-lg font-semibold text-gray-900">{preflightModalTitle}</h3>
          <p class="mt-1 text-sm text-gray-500">
            {$_('overview.preflight_desc_blocking', { values: { count: preflightModalData.missing_components.length, blocking: preflightModalData.summary.blocking_missing_components } })}
            {#if preflightModalData.summary.blocking_missing_components > 0}
              {$_('overview.preflight_desc_must_block')}
            {:else if preflightModalData.requires_confirmation}
              {$_('overview.preflight_desc_confirm')}
            {:else}
              {$_('overview.preflight_desc_auto_skip')}
            {/if}
          </p>
        </div>
        <button
          class="text-sm text-gray-400 hover:text-gray-600"
          onclick={() => closePreflightModal(null)}>
          {$_('common.close')}
        </button>
      </div>

      <div class="px-5 py-4 border-b border-gray-100 bg-slate-50">
        <div class="grid gap-3 md:grid-cols-8 text-xs">
          <div class="rounded-lg bg-white border border-slate-200 px-3 py-2">
            <div class="text-slate-500">{$_('overview.th_domain')}</div>
            <div class="mt-1 text-slate-900 font-medium">{preflightModalData.summary.domains_checked}</div>
          </div>
          <div class="rounded-lg bg-white border border-slate-200 px-3 py-2">
            <div class="text-slate-500">{$_('overview.relation')}</div>
            <div class="mt-1 text-slate-900 font-medium">{preflightModalData.summary.relations_checked}</div>
          </div>
          <div class="rounded-lg bg-white border border-slate-200 px-3 py-2">
            <div class="text-slate-500">{$_('overview.rule')}</div>
            <div class="mt-1 text-slate-900 font-medium">{preflightModalData.summary.rules_checked}</div>
          </div>
          <div class="rounded-lg bg-white border border-slate-200 px-3 py-2">
            <div class="text-slate-500">{$_('overview.th_fields')}</div>
            <div class="mt-1 text-slate-900 font-medium">{preflightModalData.summary.fields_checked}</div>
          </div>
          <div class="rounded-lg bg-white border border-slate-200 px-3 py-2">
            <div class="text-slate-500">{$_('overview.th_lang_pack_lane')}</div>
            <div class="mt-1 text-slate-900 font-medium">{preflightModalData.summary.language_pack_lanes_checked}</div>
          </div>
          <div class="rounded-lg bg-white border border-rose-200 px-3 py-2">
            <div class="text-rose-500">{$_('overview.th_blocking')}</div>
            <div class="mt-1 text-rose-700 font-medium">{preflightModalData.summary.blocking_missing_components}</div>
          </div>
          <div class="rounded-lg bg-white border border-amber-200 px-3 py-2">
            <div class="text-amber-500">{$_('overview.th_confirm')}</div>
            <div class="mt-1 text-amber-700 font-medium">{preflightModalData.summary.confirm_missing_components}</div>
          </div>
          <div class="rounded-lg bg-white border border-sky-200 px-3 py-2">
            <div class="text-sky-500">{$_('overview.th_auto_skip')}</div>
            <div class="mt-1 text-sky-700 font-medium">{preflightModalData.summary.auto_skip_missing_components}</div>
          </div>
        </div>
      </div>

      <div class="max-h-[60vh] overflow-auto">
        <table class="w-full text-sm">
          <thead class="sticky top-0 bg-gray-50 text-xs text-gray-500">
            <tr>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_domain_biz')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_relation_rule')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_source_semantic')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_object_field')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_policy')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_format_slot')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_artifact')}</th>
            </tr>
          </thead>
          <tbody>
            {#each preflightModalData.missing_components as item}
              <tr class="border-t border-gray-100 align-top">
                <td class="px-4 py-3">
                  <div class="text-gray-900 break-all">{item.api_base_url}</div>
                  <div class="mt-1 text-xs text-gray-700">{formatBusinessLineLabel(item.business_line)}</div>
                  <div class="mt-1 font-mono text-[11px] text-gray-400">{item.business_line}</div>
                </td>
                <td class="px-4 py-3">
                  <div class="text-gray-900">{$_('task_routing.relation_label')} {item.relation_id}</div>
                  <div class="mt-1 text-xs text-gray-500">
                    {#if item.rule_id}
                      {$_('task_routing.rule_fallback', { values: { id: item.rule_id } })}
                    {:else}
                      {$_('overview.lang_pack')}
                    {/if}
                  </div>
                </td>
                <td class="px-4 py-3">
                  <div class="text-gray-900">{formatSourceGroupLabel(item.source_group)}</div>
                  <div class="mt-1 font-mono text-[11px] text-gray-400">{item.source_group}</div>
                  <div class="mt-1 text-xs text-gray-500">{$_('overview.routing')}: {formatRoutingProfileLabel(item.routing_profile)}</div>
                  <div class="mt-1 font-mono text-[11px] text-gray-400">{item.routing_profile}</div>
                  <div class="mt-1 text-xs text-gray-400">{$_('overview.writeback')}: {formatDeliveryTargetLabel(item.delivery_target)}</div>
                </td>
                <td class="px-4 py-3">
                  <div class="text-gray-900">{item.object_name}</div>
                  <div class="mt-1 font-mono text-xs text-gray-500 break-all">{item.field_name}</div>
                  <div class="mt-1 text-xs text-gray-400">{$_('overview.role')}: {formatSourceRoleLabel(item.source_role)}</div>
                </td>
                <td class="px-4 py-3">
                  <div class="text-gray-900">{formatPreflightPolicyLabel(item.preflight_policy)}</div>
                  <div class="mt-1 text-gray-900">{formatMissingComponentBehaviorLabel(item.missing_component_behavior)}</div>
                  <div class="mt-1 font-mono text-[11px] text-gray-400">
                    {item.preflight_policy} / {item.missing_component_behavior}
                  </div>
                  <div class="mt-1 text-xs {item.severity === 'blocking' ? 'text-rose-600' : item.severity === 'auto_skip' ? 'text-sky-600' : 'text-amber-600'}">
                    {formatSeverityLabel(item.severity)}
                  </div>
                </td>
                <td class="px-4 py-3">
                  <div class="text-gray-900">{formatCapabilityLabel(item.content_format)}</div>
                  <div class="mt-1 font-mono text-[11px] text-gray-400">{item.content_format}</div>
                  <div class="mt-1 text-xs text-gray-500">{$_('task_routing.slot_label')}: {formatRuleSlotLabel(item.required_slot_key)}</div>
                  <div class="mt-1 font-mono text-[11px] text-gray-400">{item.required_slot_key}</div>
                  <div class="mt-1 text-xs text-gray-400">{$_('task_routing.field_task')}: {item.suggested_task_type}</div>
                </td>
                <td class="px-4 py-3 text-xs text-gray-600 break-all">
                  {renderArtifactSummary(item)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      <div class="px-5 py-4 border-t border-gray-100 flex items-center justify-between gap-3 bg-white">
        <div class="text-xs text-gray-500">
          {#if preflightModalData.summary.blocking_missing_components > 0}
            {$_('overview.preflight_footer_blocking')}
          {:else if preflightModalData.requires_confirmation}
            {$_('overview.preflight_footer_confirm')}
          {:else}
            {$_('overview.preflight_footer_auto_skip')}
          {/if}
        </div>
        <div class="flex items-center gap-2">
          <button
            data-testid="overview-preflight-cancel"
            class="px-4 py-2 rounded-lg border border-gray-200 text-sm text-gray-700 hover:bg-gray-50"
            onclick={() => closePreflightModal(null)}>
            {$_('common.cancel')}
          </button>
          {#if preflightModalData.summary.blocking_missing_components === 0 && preflightModalData.requires_confirmation}
            <button
              data-testid="overview-preflight-continue"
              class="px-4 py-2 rounded-lg bg-amber-600 text-sm text-white hover:bg-amber-700"
              onclick={() => closePreflightModal(true)}>
              {$_('overview.preflight_continue')}
            </button>
          {/if}
        </div>
      </div>
    </div>
  </div>
{/if}

{#if $status?.local_components_backfill_error}
  <div class="mb-4 rounded-xl border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
    <div class="font-medium">{$_('overview.backfill_error_title')}</div>
    <div class="mt-1 break-all">{$status.local_components_backfill_error}</div>
    {#if ($status.local_components_backfilled ?? 0) > 0}
      <div class="mt-1 text-xs text-amber-700">{$_('overview.backfill_partial', { values: { count: $status.local_components_backfilled } })}</div>
    {/if}
  </div>
{/if}

<div class="mb-4 rounded-xl border border-slate-200 bg-white px-4 py-3 text-sm text-slate-700">
  <div class="flex items-center justify-between gap-3">
    <div>
      <div class="font-medium text-slate-900">{$_('overview.device_id')}</div>
      <div class="mt-1 font-mono text-xs break-all">{$status?.device_id || '-'}</div>
    </div>
  </div>
</div>

<!-- Worker 状态横幅（固定，始终可见） -->
<div class="bg-white border border-gray-200 rounded-xl p-4 mb-4 sticky top-0 z-10 shadow-sm">
  <div class="flex items-center justify-between gap-4">
    <div class="flex items-center gap-3 flex-1">
      <h3 class="font-medium text-gray-900">{$_('overview.worker_status')}</h3>
      {#if $status?.worker_loop_running}
        <span class="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-green-100 text-green-700">
          <span class="w-1.5 h-1.5 bg-green-500 rounded-full animate-pulse"></span>{workerStatusLabel($status?.worker_status, $status?.worker_loop_running)}
        </span>
      {:else}
        <span class="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-gray-100 text-gray-500">
          <span class="w-1.5 h-1.5 bg-gray-400 rounded-full"></span>{workerStatusLabel($status?.worker_status, $status?.worker_loop_running)}
        </span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      <button onclick={runOnce} disabled={runLoading}
        data-testid="overview-run-once"
        class="px-3 py-1.5 bg-blue-600 text-white text-xs rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors whitespace-nowrap">
        {runLoading ? $_('overview.run_once') + '…' : $_('overview.run_once')}
      </button>
      {#if !$status?.worker_loop_running}
        <button onclick={startLoop} disabled={startLoading}
          data-testid="overview-start-loop"
          class="px-3 py-1.5 bg-green-600 text-white text-xs rounded-lg hover:bg-green-700 disabled:opacity-60 transition-colors whitespace-nowrap">
          {startLoading ? $_('overview.start_loop') + '…' : $_('overview.start_loop')}
        </button>
      {:else}
        <button onclick={stopLoop} disabled={stopLoading}
          data-testid="overview-stop-loop"
          class="px-3 py-1.5 bg-gray-200 text-gray-700 text-xs rounded-lg hover:bg-gray-300 disabled:opacity-60 transition-colors whitespace-nowrap">
          {stopLoading ? $_('overview.stop_loop') + '…' : $_('overview.stop_loop')}
        </button>
      {/if}
      <button
        onclick={() => showDetails = !showDetails}
        class="px-3 py-1.5 border border-gray-200 text-gray-700 text-xs rounded-lg hover:bg-gray-50 transition-colors">
        {showDetails ? $_('common.hide') : $_('common.show')}
      </button>
    </div>
  </div>
</div>

<!-- 详情面板（可折叠） -->
{#if showDetails}
<div class="bg-white border border-gray-200 rounded-xl p-5 mb-4">
  <div class="flex items-center justify-between mb-4">
    <div class="flex items-center gap-3">
      <h3 class="font-medium text-gray-900">{$_('overview.worker_controls')}</h3>
    </div>
    <div class="flex items-center gap-2 text-sm text-gray-500">
      <span>{$_('overview.poll_interval')}</span>
      <input bind:value={pollInput} class="w-16 border border-gray-200 rounded-lg px-2 py-1 text-center text-sm" />
      <span>{$_('overview.seconds')}</span>
      <button onclick={savePoll} class="text-blue-600 hover:text-blue-700 text-sm font-medium">{$_('common.save')}</button>
    </div>
  </div>
  <!-- P0-LF-04: domain/component refresh hits the legacy server control plane
       (/api/domains/refresh, /api/components/refresh); render only in legacy mode. -->
  {#if $status?.runtime_mode === 'legacy_server_control_plane'}
    <div class="flex gap-2 flex-wrap">
      <button onclick={refreshDomains}
        class="px-4 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50 transition-colors">
        {$_('overview.refresh_domains')}
      </button>
      <button onclick={refreshComponents}
        class="px-4 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50 transition-colors">
        {$_('overview.refresh_components')}
      </button>
    </div>
  {/if}
</div>
{/if}

<!-- 最近运行 + 域名详情 两列布局 -->
<div class="grid grid-cols-1 lg:grid-cols-2 gap-4 mb-4">
  <!-- 最近运行记录 -->
  <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
    <div class="px-5 py-3 border-b border-gray-100">
      <h3 class="font-medium text-gray-900 text-sm">{$_('overview.recent_runs')}</h3>
    </div>
    <div class="overflow-auto">
      <table class="w-full text-sm">
        <thead>
          <tr class="text-xs text-gray-500 bg-gray-50">
            <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_time')}</th>
            <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_status')}</th>
            <th class="px-4 py-2.5 text-center font-medium">{$_('overview.success')}</th>
            <th class="px-4 py-2.5 text-center font-medium">{$_('overview.failed')}</th>
          </tr>
        </thead>
        <tbody>
          {#if runs.length === 0}
            <tr><td colspan="4" class="px-4 py-8 text-center text-gray-400 text-sm">{$_('overview.no_runs')}</td></tr>
          {:else}
            {#each runs as run, i}
              <tr onclick={() => selectedRunIdx = i}
                class="border-t border-gray-50 cursor-pointer transition-colors
                  {selectedRunIdx === i ? 'bg-blue-50' : 'hover:bg-gray-50'}">
                <td class="px-4 py-2.5 text-xs text-gray-600">
                  {run.ts ? new Date(run.ts * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) : '-'}
                </td>
                <td class="px-4 py-2.5">
                  {#if run.status === 'ok' || run.status === 'success'}
                    <span class="text-xs px-2 py-0.5 bg-green-100 text-green-700 rounded-full">{$_('overview.success')}</span>
                  {:else}
                    <span class="text-xs px-2 py-0.5 bg-red-100 text-red-700 rounded-full">{$_('overview.failed')}</span>
                  {/if}
                </td>
                <td class="px-4 py-2.5 text-center text-gray-700">{run.summary?.tasks_succeeded ?? run.summary?.completed ?? 0}</td>
                <td class="px-4 py-2.5 text-center text-gray-700">{run.summary?.tasks_failed ?? run.summary?.failed ?? 0}</td>
              </tr>
            {/each}
          {/if}
        </tbody>
      </table>
    </div>
  </div>

  <!-- 选中运行的域名详情 -->
  <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
    <div class="px-5 py-3 border-b border-gray-100">
      <h3 class="font-medium text-gray-900 text-sm">{$_('overview.domain_detail')}
        {#if selectedRun}<span class="text-gray-400 font-normal ml-2">
          {new Date(selectedRun.ts * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })}
        </span>{/if}
      </h3>
    </div>
    <div class="overflow-auto">
      {#if selectedRun?.error}
        <div class="px-5 py-3 text-sm text-red-600 bg-red-50">{selectedRun.error}</div>
      {/if}
      <table class="w-full text-sm">
        <thead>
          <tr class="text-xs text-gray-500 bg-gray-50">
            <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_domain')}</th>
            <th class="px-4 py-2.5 text-center font-medium">{$_('overview.completed')}</th>
            <th class="px-4 py-2.5 text-center font-medium">{$_('overview.failed')}</th>
          </tr>
        </thead>
        <tbody>
          {#if !selectedRun?.summary?.summary?.domains?.length && !selectedRun?.summary?.domains?.length}
            <tr><td colspan="3" class="px-4 py-8 text-center text-gray-400 text-sm">
              {runs.length === 0 ? $_('common.no_data') : $_('overview.click_to_view')}
            </td></tr>
          {:else}
            {#each (selectedRun.summary.summary?.domains ?? selectedRun.summary.domains ?? []) as d}
              <tr class="border-t border-gray-50">
                <td class="px-4 py-2.5 text-xs text-gray-600 max-w-[160px] truncate" title={d.api_base_url}>
                  {d.api_base_url.replace(/^https?:\/\//, '')}
                </td>
                <td class="px-4 py-2.5 text-center text-green-700">{d.completed}</td>
                <td class="px-4 py-2.5 text-center text-red-600">{d.failed}</td>
              </tr>
            {/each}
          {/if}
        </tbody>
      </table>
    </div>
  </div>
</div>

<!-- 翻译统计 -->
{#if stats && (stats.by_domain.length > 0 || stats.by_status.length > 0)}
  <div class="grid grid-cols-1 lg:grid-cols-2 gap-4 mb-4">
    <!-- 域名翻译进度 -->
    {#if stats.by_domain.length > 0}
      <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
        <div class="px-5 py-3 border-b border-gray-100">
          <h3 class="font-medium text-gray-900 text-sm">{$_('overview.domain_stats')}</h3>
        </div>
        <div class="p-4 space-y-3">
          {#each stats.by_domain as d}
            {@const successRate = d.total > 0 ? Math.round((d.success / d.total) * 100) : 0}
            <div>
              <div class="flex items-center justify-between mb-1">
                <span class="text-xs font-mono text-gray-600 truncate max-w-[200px]" title={d.domain}>
                  {d.domain.replace(/^https?:\/\//, '')}
                </span>
                <span class="text-xs text-gray-500">{d.success}/{d.total} ({successRate}%)</span>
              </div>
              <div class="h-2 bg-gray-100 rounded-full overflow-hidden flex">
                {#if successRate > 0}
                  <div class="h-full bg-green-500" style="width: {successRate}%"></div>
                {/if}
                {#if d.failed > 0}
                  <div class="h-full bg-red-400" style="width: {Math.round((d.failed / d.total) * 100)}%"></div>
                {/if}
              </div>
              <div class="text-xs text-gray-400 mt-0.5">{d.fields_total} {$_('overview.fields_translated')}</div>
            </div>
          {/each}
        </div>
      </div>
    {/if}

    <!-- 状态分布 + 近 30 天趋势 -->
    <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
      <div class="px-5 py-3 border-b border-gray-100">
        <h3 class="font-medium text-gray-900 text-sm">{$_('overview.status_distribution')}</h3>
      </div>
      <div class="p-4">
        {#if stats.by_status.length > 0}
          {@const statusTotal = stats.by_status.reduce((a, b) => a + b.count, 0)}
          <div class="space-y-2 mb-4">
            {#each stats.by_status as s}
              {@const color = s.status === 'success' ? 'bg-green-500' : s.status === 'failed' ? 'bg-red-400' : 'bg-gray-300'}
              {@const pct = statusTotal > 0 ? Math.round((s.count / statusTotal) * 100) : 0}
              <div class="flex items-center gap-3">
                <div class="w-2.5 h-2.5 rounded-full {color}"></div>
                <span class="text-xs text-gray-600 w-20">{s.status}</span>
                <div class="flex-1 h-1.5 bg-gray-100 rounded-full overflow-hidden">
                  <div class="h-full {color} rounded-full" style="width: {pct}%"></div>
                </div>
                <span class="text-xs text-gray-500 w-10 text-right">{s.count}</span>
              </div>
            {/each}
          </div>
        {/if}
        {#if stats.daily.length > 0}
          {@const maxCount = Math.max(...stats.daily.map(d => d.count), 1)}
          <div class="border-t border-gray-100 pt-3">
            <p class="text-xs text-gray-500 mb-2">{$_('overview.daily_volume')}</p>
            <div class="flex items-end gap-px h-16">
              {#each stats.daily as day}
                <div
                  class="flex-1 bg-blue-400 hover:bg-blue-500 rounded-t-sm transition-colors cursor-default"
                  style="height: {Math.max(2, Math.round((day.count / maxCount) * 100))}%"
                  title="{day.date}: {day.count}, {day.fields}"
                ></div>
              {/each}
            </div>
          </div>
        {/if}
      </div>
    </div>
  </div>
{/if}

<!-- 已连接域名快速状态 -->
{#if $status?.domains?.length}
  <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
    <div class="px-5 py-3 border-b border-gray-100">
      <h3 class="font-medium text-gray-900 text-sm">{$_('overview.authorized_domains', { values: { count: $status.domains.length } })}</h3>
    </div>
    <table class="w-full text-sm">
      <thead>
        <tr class="text-xs text-gray-500 bg-gray-50">
          <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_api_address')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('overview.th_site_status')}</th>
        </tr>
      </thead>
      <tbody>
        {#each $status.domains as d}
          <tr class="border-t border-gray-50">
            <td class="px-4 py-2.5 text-gray-700">{d.api_base_url}</td>
            <td class="px-4 py-2.5">
              <span class="text-xs px-2 py-0.5 rounded-full
                {(d as any).site_status === 'active' ? 'bg-green-100 text-green-700' : 'bg-gray-100 text-gray-600'}">
                {(d as any).site_status ?? '-'}
              </span>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
