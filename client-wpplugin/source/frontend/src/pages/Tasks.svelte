<script lang="ts">
  import { onMount } from 'svelte';
  import { listDiscoveryTasks, updateDiscoveryTask, type DiscoveryTask } from '../lib/api/tasks';
  import { getJob, listAllJobs, listJobs, listJobItems, type TranslationJob, type TranslationItem } from '../lib/api/jobs';
  import { listAllLocalComponents } from '../lib/api/components';
  import { batchApproveItems } from '../lib/api/items';
  import { summarizeBatchApproveFailures, summarizeBatchApproveSkipped } from '../lib/errors/wpClientApi';
  import { showToast } from '../lib/stores/toast';
  import type { LocalComponent } from '../lib/api/types';
  import { _ } from 'svelte-i18n';

  // --- Discovery Tasks ---
  let tasks = $state<DiscoveryTask[]>([]);
  let tasksLoading = $state(false);
  let tasksError = $state('');
  let editingCell = $state<{ id: number; field: string } | null>(null);
  let editingValue = $state('');
  let localComponents = $state<LocalComponent[]>([]);
  let editingOverridesTaskId = $state<number | null>(null);
  let editingOverridesValue = $state('{}');

  // --- Translation Jobs ---
  let jobs = $state<TranslationJob[]>([]);
  let jobsLoading = $state(false);
  let jobsError = $state('');
  let expandedJobId = $state<number | null>(null);
  let expandedJobDetail = $state<TranslationJob | null>(null);
  let jobItems = $state<TranslationItem[]>([]);
  let jobItemsLoading = $state(false);

  let { onReviewItem }: { onReviewItem: (id: number) => void } = $props();

  // --- Pending Review ---
  let pendingItems = $state<TranslationItem[]>([]);
  let pendingLoading = $state(false);
  let pendingError = $state('');
  let selectedPendingIds = $state<Set<number>>(new Set());
  let batchApproving = $state(false);

  // Active tab
  type TasksTab = 'jobs' | 'discovery' | 'pending_review';
  let activeTab = $state<TasksTab>('jobs');

  function discoveryFieldId(taskId: number, field: string): string {
    return `discovery-${taskId}-${field}`;
  }

  async function loadTasks() {
    tasksLoading = true;
    tasksError = '';
    try {
      const res = await listDiscoveryTasks();
      if (res.success) {
        tasks = res.data.items;
      } else {
        tasksError = (res as any).error?.message || $_('common.load_failed');
      }
    } catch (e: any) {
      tasksError = e.message || $_('common.load_failed');
    } finally {
      tasksLoading = false;
    }
  }

  async function loadLocalTaskComponents() {
    try {
      const res = await listAllLocalComponents();
      if (res.success) {
        localComponents = res.data.items;
      }
    } catch {
      localComponents = [];
    }
  }

  async function loadJobs() {
    jobsLoading = true;
    jobsError = '';
    try {
      const res = await listJobs({ limit: 50 });
      if (res.success) {
        jobs = res.data.items;
      } else {
        jobsError = (res as any).error?.message || $_('common.load_failed');
      }
    } catch (e: any) {
      jobsError = e.message || $_('common.load_failed');
    } finally {
      jobsLoading = false;
    }
  }

  async function toggleJobExpand(job: TranslationJob) {
    if (expandedJobId === job.id) {
      expandedJobId = null;
      expandedJobDetail = null;
      jobItems = [];
      return;
    }
    expandedJobId = job.id;
    jobItemsLoading = true;
    try {
      const [detailRes, itemsRes] = await Promise.all([getJob(job.id), listJobItems(job.id)]);
      expandedJobDetail = detailRes.success ? detailRes.data : job;
      jobItems = itemsRes.success ? itemsRes.data.items : [];
    } finally {
      jobItemsLoading = false;
    }
  }

  async function loadPendingReview() {
    pendingLoading = true;
    pendingError = '';
    try {
      const res = await listAllJobs({ pageSize: 200 });
      if (res.success) {
        // Collect all items with pending_review status across jobs
        const allPending: TranslationItem[] = [];
        const candidateJobs = res.data.items.filter(
          (job) => job.progress == null || (job.progress.pending_review ?? 0) > 0
        );
        for (const job of candidateJobs) {
          const itemsRes = await listJobItems(job.id, 'pending_review');
          if (itemsRes.success) {
            for (const it of itemsRes.data.items) {
              if (it.status === 'pending_review') allPending.push(it);
            }
          } else {
            pendingError = (itemsRes as any).error?.message || $_('tasks.batch_failed');
          }
        }
        pendingItems = allPending;
      } else {
        pendingError = (res as any).error?.message || $_('common.load_failed');
      }
    } catch (e: any) {
      pendingError = e.message || $_('common.load_failed');
    } finally {
      pendingLoading = false;
    }
  }

  function togglePendingSelect(id: number) {
    const next = new Set(selectedPendingIds);
    if (next.has(id)) next.delete(id); else next.add(id);
    selectedPendingIds = next;
  }

  function toggleSelectAll() {
    if (selectedPendingIds.size === pendingItems.length) {
      selectedPendingIds = new Set();
    } else {
      selectedPendingIds = new Set(pendingItems.map(i => i.id));
    }
  }

  async function handleBatchApprove() {
    if (selectedPendingIds.size === 0) return;
    batchApproving = true;
    try {
      const res = await batchApproveItems([...selectedPendingIds]);
      if (res.success) {
        const d = res.data;
        const failedDetail = summarizeBatchApproveFailures(d.failed);
        const skippedDetail = summarizeBatchApproveSkipped(d.skipped as any);
        const detail = [failedDetail, skippedDetail].filter(Boolean).join('；') || undefined;
        if ((d.failed.length > 0 || d.skipped.length > 0) && d.approved.length === 0) {
          showToast('error', $_('tasks.batch_no_success'), detail);
        } else {
          showToast('success', $_('tasks.approved_count', { values: { count: d.approved.length } }), detail);
        }
        selectedPendingIds = new Set();
        await loadPendingReview();
      } else {
        showToast('error', $_('tasks.batch_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('login.network_error'), e.message);
    } finally {
      batchApproving = false;
    }
  }

  onMount(() => {
    loadTasks();
    loadJobs();
    loadLocalTaskComponents();
  });

  // Discovery editing helpers
  function startEdit(task: DiscoveryTask, field: string) {
    editingCell = { id: task.id, field };
    editingValue = String((task as any)[field] ?? '');
  }

  async function commitEdit(task: DiscoveryTask) {
    if (!editingCell) return;
    const { field } = editingCell;
    editingCell = null;
    let parsed: any;
    if (field === 'enabled') {
      parsed = editingValue === 'true' || editingValue === '1';
    } else if (field === 'selected_component_id' || field === 'effective_source_lang' || field === 'effective_target_lang') {
      parsed = editingValue.trim();
    } else {
      parsed = parseInt(editingValue, 10);
      if (isNaN(parsed)) { showToast('error', $_('tasks.input_invalid'), $_('tasks.input_number')); return; }
    }
    try {
      const res = await updateDiscoveryTask(task.id, { [field]: parsed });
      if (res.success) { showToast('success', $_('tasks.saved')); await loadTasks(); }
      else { showToast('error', $_('common.save_failed'), (res as any).error?.message); }
    } catch (e: any) { showToast('error', $_('login.network_error'), e.message); }
  }

  function openOverridesEditor(task: DiscoveryTask) {
    editingOverridesTaskId = task.id;
    editingOverridesValue = JSON.stringify(task.editable_overrides ?? {}, null, 2);
  }

  async function saveOverrides(task: DiscoveryTask) {
    let parsed: Record<string, unknown> = {};
    try {
      parsed = editingOverridesValue.trim() ? JSON.parse(editingOverridesValue) : {};
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        showToast('error', $_('tasks.override_json_error'));
        return;
      }
    } catch (e: any) {
      showToast('error', $_('tasks.override_json_format_error'), e?.message);
      return;
    }
    try {
      const res = await updateDiscoveryTask(task.id, { editable_overrides: parsed });
      if (res.success) {
        editingOverridesTaskId = null;
        showToast('success', $_('tasks.override_saved'));
        await loadTasks();
      } else {
        showToast('error', $_('tasks.override_save_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('tasks.override_save_failed'), e.message);
    }
  }

  async function toggleEnabled(task: DiscoveryTask) {
    try {
      const res = await updateDiscoveryTask(task.id, { enabled: !task.enabled });
      if (res.success) { showToast('success', task.enabled ? $_('tasks.disabled_label') : $_('tasks.enabled')); await loadTasks(); }
      else { showToast('error', $_('common.operation_failed'), (res as any).error?.message); }
    } catch (e: any) { showToast('error', $_('login.network_error'), e.message); }
  }

  function handleKeydown(e: KeyboardEvent, task: DiscoveryTask) {
    if (e.key === 'Enter') commitEdit(task);
    if (e.key === 'Escape') editingCell = null;
  }

  function formatTs(ts: number | null): string {
    if (!ts) return '-';
    return new Date(ts * 1000).toLocaleString();
  }

  function jobProgress(job: TranslationJob): number {
    if (!job.total_items) return 0;
    return Math.round((job.done_items / job.total_items) * 100);
  }

  function jobStatusColor(status: string): string {
    const map: Record<string, string> = {
      pending: 'bg-gray-100 text-gray-600',
      running: 'bg-blue-100 text-blue-700',
      completed: 'bg-green-100 text-green-700',
      partial: 'bg-yellow-100 text-yellow-700',
      failed: 'bg-red-100 text-red-700',
    };
    return map[status] ?? 'bg-gray-100 text-gray-500';
  }

  function itemStatusColor(status: string): string {
    const map: Record<string, string> = {
      pending: 'text-gray-400',
      fetching: 'text-blue-500',
      fetched: 'text-blue-700',
      translating: 'text-indigo-500',
      translated: 'text-indigo-700',
      syncing: 'text-purple-500',
      pending_review: 'text-amber-600',
      done: 'text-green-600',
      failed: 'text-red-500',
      skipped: 'text-gray-400',
    };
    return map[status] ?? 'text-gray-400';
  }

  function itemDisplayComponent(item: TranslationItem): string {
    return item.selected_component_id ?? item.component_id ?? '-';
  }

  function itemComponentTrace(item: TranslationItem): string[] {
    return Array.isArray(item.component_ids)
      ? item.component_ids.filter((id) => typeof id === 'string' && id.trim().length > 0)
      : [];
  }

  function itemComponentTraceText(item: TranslationItem): string {
    return itemComponentTrace(item).join(' -> ');
  }

  function itemShowsTrace(item: TranslationItem): boolean {
    const trace = itemComponentTrace(item);
    if (trace.length > 1) return true;
    return trace.length === 1 && trace[0] !== itemDisplayComponent(item);
  }

  type NumericField = 'concurrency' | 'batch_parallel' | 'per_page' | 'retry_max' | 'timeout_secs';
  const numericFields: { key: NumericField; labelKey: string; tipKey: string }[] = [
    { key: 'concurrency', labelKey: 'tasks.concurrency_label', tipKey: 'tasks.concurrency_tip' },
    { key: 'batch_parallel', labelKey: 'tasks.batch_parallel_label', tipKey: 'tasks.batch_parallel_tip' },
    { key: 'per_page', labelKey: 'tasks.per_page_label', tipKey: 'tasks.per_page_tip' },
    { key: 'retry_max', labelKey: 'tasks.retry_max_label', tipKey: 'tasks.retry_max_tip' },
    { key: 'timeout_secs', labelKey: 'tasks.timeout_label', tipKey: 'tasks.timeout_tip' },
  ];
  const discoveryColspan = numericFields.length + 8;
</script>

<div>
  <!-- Header + tabs -->
  <div class="flex items-center justify-between mb-5">
    <div>
      <h2 class="text-lg font-semibold text-gray-900">{$_('tasks.title')}</h2>
      <p class="text-sm text-gray-500 mt-0.5">{$_('tasks.subtitle')}</p>
    </div>
    <button
      onclick={() => { loadJobs(); loadTasks(); }}
      class="text-sm px-3 py-1.5 bg-white border border-gray-200 rounded-lg hover:bg-gray-50"
    >
      {$_('common.refresh')}
    </button>
  </div>

  <!-- Tab switcher -->
  <div class="flex gap-1 mb-5 bg-gray-100 p-1 rounded-lg w-fit">
    <button
      onclick={() => activeTab = 'jobs'}
      class="px-4 py-1.5 text-sm rounded-md transition-colors {activeTab === 'jobs'
        ? 'bg-white shadow-sm text-gray-900 font-medium'
        : 'text-gray-500 hover:text-gray-700'}"
    >
      {$_('tasks.tab_jobs')}
    </button>
    <button
      onclick={() => activeTab = 'discovery'}
      class="px-4 py-1.5 text-sm rounded-md transition-colors {activeTab === 'discovery'
        ? 'bg-white shadow-sm text-gray-900 font-medium'
        : 'text-gray-500 hover:text-gray-700'}"
    >
      {$_('tasks.tab_discovery')}
    </button>
    <button
      onclick={() => { activeTab = 'pending_review'; loadPendingReview(); }}
      class="px-4 py-1.5 text-sm rounded-md transition-colors {activeTab === 'pending_review'
        ? 'bg-white shadow-sm text-gray-900 font-medium'
        : 'text-gray-500 hover:text-gray-700'}"
    >
      {$_('tasks.tab_pending')}
    </button>
  </div>

  <!-- ======================== JOBS TAB ======================== -->
  {#if activeTab === 'jobs'}
    {#if jobsError}
      <div class="bg-red-50 border border-red-200 text-red-700 rounded-lg px-4 py-3 mb-4 text-sm">{jobsError}</div>
    {/if}

    {#if jobsLoading}
      <div class="text-sm text-gray-400 py-8 text-center">{$_('common.loading')}</div>
    {:else if jobs.length === 0}
      <div class="text-sm text-gray-500 bg-gray-50 rounded-lg p-8 text-center">
        {$_('tasks.no_jobs')}<br />
        <span class="text-xs text-gray-400 mt-1 block">{$_('tasks.no_jobs_hint')}</span>
      </div>
    {:else}
      <div class="space-y-3">
        {#each jobs as job (job.id)}
          {@const pct = jobProgress(job)}
          <!-- Job card -->
          <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
            <button
              onclick={() => toggleJobExpand(job)}
              class="w-full text-left px-5 py-4 hover:bg-gray-50 transition-colors"
            >
              <div class="flex items-start justify-between gap-4">
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2 flex-wrap">
                    <span class="font-mono text-xs text-gray-500">{job.domain.replace(/^https?:\/\//, '')}</span>
                    <span class="text-gray-300">·</span>
                    <span class="text-xs text-gray-500">rel={job.relation_id}</span>
                    {#if job.business_line}
                      <span class="text-gray-300">·</span>
                      <span class="text-xs text-gray-400">{job.business_line}</span>
                    {/if}
                  </div>
                  <div class="flex items-center gap-3 mt-2">
                    <span class="text-xs px-2 py-0.5 rounded-full font-medium {jobStatusColor(job.status)}">
                      {job.status}
                    </span>
                    <span class="text-xs text-gray-500">
                      {job.done_items}/{job.total_items} {$_('tasks.items_done')}
                      {#if job.progress?.pending_review}
                        <span class="text-amber-600 ml-1">· {job.progress.pending_review} {$_('tasks.items_pending_review')}</span>
                      {/if}
                      {#if job.failed_items > 0}
                        <span class="text-red-500 ml-1">· {job.failed_items} {$_('tasks.items_failed')}</span>
                      {/if}
                    </span>
                    <span class="text-xs text-gray-400">#{job.id}</span>
                  </div>
                </div>
                <div class="flex flex-col items-end gap-1 shrink-0">
                  <span class="text-sm font-semibold text-gray-700">{pct}%</span>
                  <span class="text-xs text-gray-400">{formatTs(job.started_at)}</span>
                </div>
              </div>
              <!-- Progress bar (multi-segment) -->
              {#if job.total_items > 0}
                {@const p = job.progress}
                {@const t = p?.total || job.total_items}
                {@const donePct = Math.round(((p?.done ?? job.done_items) / t) * 100)}
                {@const reviewPct = Math.round(((p?.pending_review ?? 0) / t) * 100)}
                {@const failPct = Math.round(((p?.failed ?? job.failed_items) / t) * 100)}
                <div class="mt-3 h-1.5 bg-gray-100 rounded-full overflow-hidden flex">
                  {#if donePct > 0}
                    <div class="h-full bg-green-500 transition-all" style="width: {donePct}%"></div>
                  {/if}
                  {#if reviewPct > 0}
                    <div class="h-full bg-amber-400 transition-all" style="width: {reviewPct}%"></div>
                  {/if}
                  {#if failPct > 0}
                    <div class="h-full bg-red-400 transition-all" style="width: {failPct}%"></div>
                  {/if}
                </div>
              {/if}
            </button>

            <!-- Expanded item detail -->
            {#if expandedJobId === job.id}
              <div class="border-t border-gray-100 px-5 py-4">
                {#if expandedJobDetail?.id === job.id}
                  <div class="mb-4 grid gap-3 md:grid-cols-4">
                    <div class="rounded-lg bg-gray-50 px-3 py-2">
                      <div class="text-[11px] text-gray-500">{$_('tasks.trigger_method')}</div>
                      <div class="mt-1 text-sm text-gray-800">{expandedJobDetail.triggered_by || '-'}</div>
                    </div>
                    <div class="rounded-lg bg-gray-50 px-3 py-2">
                      <div class="text-[11px] text-gray-500">{$_('tasks.created_time')}</div>
                      <div class="mt-1 text-sm text-gray-800">{formatTs(expandedJobDetail.created_at)}</div>
                    </div>
                    <div class="rounded-lg bg-gray-50 px-3 py-2">
                      <div class="text-[11px] text-gray-500">{$_('tasks.updated_time')}</div>
                      <div class="mt-1 text-sm text-gray-800">{formatTs(expandedJobDetail.updated_at)}</div>
                    </div>
                    <div class="rounded-lg bg-gray-50 px-3 py-2">
                      <div class="text-[11px] text-gray-500">{$_('tasks.completed_time')}</div>
                      <div class="mt-1 text-sm text-gray-800">{formatTs(expandedJobDetail.completed_at)}</div>
                    </div>
                  </div>
                {/if}
                {#if jobItemsLoading}
                  <div class="text-sm text-gray-400 py-3 text-center">{$_('tasks.loading_items')}</div>
                {:else if jobItems.length === 0}
                  <div class="text-sm text-gray-400 py-2 text-center">{$_('tasks.no_items')}</div>
                {:else}
                  <div class="overflow-x-auto">
                    <table class="w-full text-xs">
                      <thead>
                        <tr class="text-gray-400 border-b border-gray-100">
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.th_object_id')}</th>
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.th_type')}</th>
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.th_task_type')}</th>
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.th_component')}</th>
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.th_status')}</th>
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.th_retry')}</th>
                          <th class="text-left py-2 pr-4 font-medium">{$_('tasks.completed_time')}</th>
                          <th class="text-left py-2 font-medium">{$_('tasks.th_actions')}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {#each jobItems as item (item.id)}
                          <tr class="border-b border-gray-50 hover:bg-gray-50">
                            <td class="py-1.5 pr-4 text-gray-600">{item.wp_object_id}</td>
                            <td class="py-1.5 pr-4 text-gray-500">{item.object_type}{item.wp_object_subtype ? `/${item.wp_object_subtype}` : ''}</td>
                            <td class="py-1.5 pr-4 text-gray-500">{item.task_type}</td>
                            <td class="py-1.5 pr-4 text-gray-400">
                              <div class="font-mono">{itemDisplayComponent(item)}</div>
                              {#if itemShowsTrace(item)}
                                <div class="mt-0.5 text-[11px] text-gray-500">
                                  {$_('overview.actual_exec')}: <span class="font-mono">{itemComponentTraceText(item)}</span>
                                </div>
                              {/if}
                            </td>
                            <td class="py-1.5 pr-4 font-medium {itemStatusColor(item.status)}">{item.status}</td>
                            <td class="py-1.5 pr-4 text-gray-400">{item.retry_count}/{item.max_retries}</td>
                            <td class="py-1.5 pr-4 text-gray-400">{formatTs(item.synced_at ?? item.translated_at)}</td>
                            <td class="py-1.5">
                              {#if item.raw_path}
                                <button
                                  onclick={() => onReviewItem(item.id)}
                                  class="text-blue-600 hover:text-blue-800 hover:underline">
                                  {$_('tasks.review')}
                                </button>
                              {/if}
                            </td>
                          </tr>
                          {#if item.error_message}
                            <tr>
                              <td colspan="7" class="pb-2 pl-2 text-red-500 text-xs italic">{item.error_message}</td>
                            </tr>
                          {/if}
                        {/each}
                      </tbody>
                    </table>
                    <p class="text-xs text-gray-400 mt-2">{$_('common.total_items', { values: { count: jobItems.length } })}</p>
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
      <p class="text-xs text-gray-400 mt-4">{$_('tasks.total_jobs', { values: { count: jobs.length } })}</p>
    {/if}

  <!-- ======================== PENDING REVIEW TAB ======================== -->
  {:else if activeTab === 'pending_review'}
    {#if pendingError}
      <div class="bg-red-50 border border-red-200 text-red-700 rounded-lg px-4 py-3 mb-4 text-sm">{pendingError}</div>
    {/if}

    {#if pendingLoading}
      <div class="text-sm text-gray-400 py-8 text-center">{$_('common.loading')}</div>
    {:else if pendingItems.length === 0}
      <div class="text-sm text-gray-500 bg-gray-50 rounded-lg p-8 text-center">
        {$_('tasks.no_pending')}<br />
        <span class="text-xs text-gray-400 mt-1 block">{$_('tasks.no_pending_hint')}</span>
      </div>
    {:else}
      <div class="flex items-center justify-between mb-3">
        <p class="text-sm text-gray-500">{$_('tasks.pending_count', { values: { count: pendingItems.length } })}</p>
        <div class="flex gap-2">
          <button onclick={toggleSelectAll}
            class="text-xs px-3 py-1.5 border border-gray-200 rounded-lg hover:bg-gray-50 text-gray-600">
            {selectedPendingIds.size === pendingItems.length ? $_('tasks.deselect_all') : $_('tasks.select_all')}
          </button>
          <button onclick={handleBatchApprove}
            disabled={selectedPendingIds.size === 0 || batchApproving}
            class="text-xs px-3 py-1.5 bg-amber-600 text-white rounded-lg hover:bg-amber-700 disabled:bg-amber-300 transition-colors">
            {batchApproving ? $_('tasks.submitting') : $_('tasks.batch_approve', { values: { count: selectedPendingIds.size } })}
          </button>
        </div>
      </div>

      <div class="overflow-x-auto rounded-xl border border-gray-200 bg-white">
        <table class="w-full text-sm">
          <thead>
            <tr class="bg-gray-50 border-b border-gray-200 text-xs text-gray-500">
              <th class="px-3 py-2.5 text-center w-10">
                <input type="checkbox" checked={selectedPendingIds.size === pendingItems.length && pendingItems.length > 0}
                  onchange={toggleSelectAll} class="rounded border-gray-300" />
              </th>
              <th class="px-4 py-2.5 text-left font-medium">ID</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('tasks.th_object_type')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('tasks.th_type')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('tasks.th_task_type')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('tasks.th_translation_time')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('tasks.th_actions')}</th>
            </tr>
          </thead>
          <tbody>
            {#each pendingItems as item (item.id)}
              <tr class="border-b border-gray-50 hover:bg-gray-50/50">
                <td class="px-3 py-2.5 text-center">
                  <input type="checkbox" checked={selectedPendingIds.has(item.id)}
                    onchange={() => togglePendingSelect(item.id)} class="rounded border-gray-300" />
                </td>
                <td class="px-4 py-2.5 text-gray-600 font-mono text-xs">#{item.id}</td>
                <td class="px-4 py-2.5 text-gray-700">{item.wp_object_id}</td>
                <td class="px-4 py-2.5 text-gray-500 text-xs">{item.object_type}{item.wp_object_subtype ? `/${item.wp_object_subtype}` : ''}</td>
                <td class="px-4 py-2.5 text-gray-500 text-xs">{item.task_type}</td>
                <td class="px-4 py-2.5 text-gray-400 text-xs">{formatTs(item.translated_at)}</td>
                <td class="px-4 py-2.5">
                  {#if item.raw_path}
                    <button onclick={() => onReviewItem(item.id)}
                      class="text-xs text-blue-600 hover:text-blue-800 hover:underline">{$_('tasks.review')}</button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

  <!-- ======================== DISCOVERY TAB ======================== -->
  {:else}
    <div class="flex items-center justify-between mb-4">
      <p class="text-sm text-gray-500">{$_('tasks.discovery_desc')}</p>
    </div>

    {#if tasksError}
      <div class="bg-red-50 border border-red-200 text-red-700 rounded-lg px-4 py-3 mb-4 text-sm">{tasksError}</div>
    {/if}

    {#if tasks.length === 0 && !tasksLoading}
      <div class="text-sm text-gray-500 bg-gray-50 rounded-lg p-6 text-center">
        {$_('tasks.no_discovery')}
      </div>
    {:else}
      <div class="overflow-x-auto rounded-xl border border-gray-200 bg-white">
        <table class="w-full text-sm">
          <thead>
            <tr class="bg-gray-50 border-b border-gray-200">
              <th class="text-left px-4 py-3 font-medium text-gray-600">{$_('tasks.th_domain')}</th>
              <th class="text-left px-4 py-3 font-medium text-gray-600">{$_('tasks.th_relation_id')}</th>
              {#each numericFields as f}
                <th class="text-center px-3 py-3 font-medium text-gray-600" title={$_(f.tipKey)}>{$_(f.labelKey)}</th>
              {/each}
              <th class="text-left px-4 py-3 font-medium text-gray-600">{$_('tasks.th_component')}</th>
              <th class="text-left px-4 py-3 font-medium text-gray-600">{$_('tasks.th_source_lang')}</th>
              <th class="text-left px-4 py-3 font-medium text-gray-600">{$_('tasks.th_target_lang')}</th>
              <th class="text-center px-3 py-3 font-medium text-gray-600">{$_('tasks.th_overrides')}</th>
              <th class="text-center px-3 py-3 font-medium text-gray-600">{$_('tasks.th_enabled')}</th>
              <th class="text-left px-4 py-3 font-medium text-gray-600">{$_('tasks.th_last_run')}</th>
            </tr>
          </thead>
          <tbody>
            {#each tasks as task (task.id)}
              <tr class="border-b border-gray-100 hover:bg-gray-50 {!task.enabled ? 'opacity-50' : ''}">
                <td class="px-4 py-3 text-gray-700 font-mono text-xs max-w-[200px] truncate" title={task.domain}>
                  {task.domain.replace(/^https?:\/\//, '')}
                </td>
                <td class="px-4 py-3 text-gray-600">{task.relation_id}</td>
                {#each numericFields as f}
                  <td class="px-3 py-3 text-center">
                    {#if editingCell?.id === task.id && editingCell?.field === f.key}
                      <input
                        id={discoveryFieldId(task.id, f.key)}
                        type="number"
                        class="w-16 text-center border border-blue-400 rounded px-1 py-0.5 text-sm focus:outline-none"
                        bind:value={editingValue}
                        onkeydown={(e) => handleKeydown(e, task)}
                        onblur={() => commitEdit(task)}
                      />
                    {:else}
                      <button
                        onclick={() => startEdit(task, f.key)}
                        class="px-2 py-0.5 rounded hover:bg-blue-50 hover:text-blue-700 transition-colors cursor-pointer"
                        title={$_('tasks.click_edit')}
                      >
                        {(task as any)[f.key]}
                      </button>
                    {/if}
                  </td>
                {/each}
                <td class="px-4 py-3 text-xs text-gray-600">
                  {#if editingCell?.id === task.id && editingCell?.field === 'selected_component_id'}
                    <select
                      id={discoveryFieldId(task.id, 'selected_component_id')}
                      bind:value={editingValue}
                      class="w-full border border-blue-400 rounded px-2 py-1 text-xs bg-white"
                      onchange={() => commitEdit(task)}
                      onblur={() => commitEdit(task)}
                    >
                      <option value="">{$_('tasks.default_route')}</option>
                      {#each localComponents as comp}
                        <option value={comp.id}>{comp.name} ({comp.id})</option>
                      {/each}
                    </select>
                  {:else}
                    <button
                      onclick={() => startEdit(task, 'selected_component_id')}
                      class="font-mono hover:text-blue-700 hover:bg-blue-50 rounded px-2 py-0.5 transition-colors"
                      title={$_('tasks.click_edit_component')}
                    >
                      {task.selected_component_id || $_('tasks.default_route')}
                    </button>
                  {/if}
                </td>
                <td class="px-4 py-3 text-xs text-gray-600">
                  {#if editingCell?.id === task.id && editingCell?.field === 'effective_source_lang'}
                    <input
                      id={discoveryFieldId(task.id, 'effective_source_lang')}
                      type="text"
                      class="w-24 border border-blue-400 rounded px-2 py-1 text-xs font-mono focus:outline-none"
                      bind:value={editingValue}
                      onkeydown={(e) => handleKeydown(e, task)}
                      onblur={() => commitEdit(task)}
                    />
                  {:else}
                    <button
                      onclick={() => startEdit(task, 'effective_source_lang')}
                      class="font-mono hover:text-blue-700 hover:bg-blue-50 rounded px-2 py-0.5 transition-colors"
                      title={$_('tasks.click_edit_source_lang')}
                    >
                      {task.effective_source_lang || '-'}
                    </button>
                  {/if}
                </td>
                <td class="px-4 py-3 text-xs text-gray-600">
                  {#if editingCell?.id === task.id && editingCell?.field === 'effective_target_lang'}
                    <input
                      id={discoveryFieldId(task.id, 'effective_target_lang')}
                      type="text"
                      class="w-24 border border-blue-400 rounded px-2 py-1 text-xs font-mono focus:outline-none"
                      bind:value={editingValue}
                      onkeydown={(e) => handleKeydown(e, task)}
                      onblur={() => commitEdit(task)}
                    />
                  {:else}
                    <button
                      onclick={() => startEdit(task, 'effective_target_lang')}
                      class="font-mono hover:text-blue-700 hover:bg-blue-50 rounded px-2 py-0.5 transition-colors"
                      title={$_('tasks.click_edit_target_lang')}
                    >
                      {task.effective_target_lang || '-'}
                    </button>
                  {/if}
                </td>
                <td class="px-3 py-3 text-center">
                  <button
                    onclick={() => openOverridesEditor(task)}
                    class="px-2 py-0.5 rounded text-xs font-medium bg-slate-100 text-slate-700 hover:bg-slate-200 transition-colors"
                  >
                    {task.editable_overrides && Object.keys(task.editable_overrides).length > 0
                      ? $_('tasks.edit_overrides_count', { values: { count: Object.keys(task.editable_overrides).length } })
                      : $_('tasks.edit_json')}
                  </button>
                </td>
                <td class="px-3 py-3 text-center">
                  <button
                    onclick={() => toggleEnabled(task)}
                    class="px-2 py-0.5 rounded text-xs font-medium {task.enabled
                      ? 'bg-green-100 text-green-700 hover:bg-green-200'
                      : 'bg-gray-100 text-gray-500 hover:bg-gray-200'}"
                  >
                    {task.enabled ? $_('tasks.enabled') : $_('tasks.disabled_label')}
                  </button>
                </td>
                <td class="px-4 py-3 text-gray-500 text-xs">{formatTs(task.last_run_at)}</td>
              </tr>
              {#if editingOverridesTaskId === task.id}
                <tr class="border-b border-blue-100 bg-blue-50/40">
                  <td colspan={discoveryColspan} class="px-4 py-4">
                    <div class="flex items-center justify-between gap-3 mb-2">
                      <div>
                        <h4 class="text-sm font-medium text-slate-800">{$_('tasks.overrides_title')}</h4>
                        <p class="text-xs text-slate-500 mt-0.5">{$_('tasks.overrides_desc')}</p>
                      </div>
                      <div class="flex gap-2">
                        <button
                          onclick={() => { editingOverridesTaskId = null; }}
                          class="px-3 py-1.5 text-xs border border-gray-200 rounded-lg hover:bg-white text-gray-600"
                        >
                          {$_('common.cancel')}
                        </button>
                        <button
                          onclick={() => saveOverrides(task)}
                          class="px-3 py-1.5 text-xs bg-slate-900 text-white rounded-lg hover:bg-slate-800"
                        >
                          {$_('tasks.save_overrides')}
                        </button>
                      </div>
                    </div>
                    <textarea
                      bind:value={editingOverridesValue}
                      rows="8"
                      class="w-full border border-blue-200 rounded-lg px-3 py-2 text-xs font-mono bg-white"
                    ></textarea>
                  </td>
                </tr>
              {/if}
            {/each}
          </tbody>
        </table>
      </div>
      <p class="text-xs text-gray-400 mt-3">
        {$_('tasks.discovery_footer', { values: { count: tasks.length } })}
      </p>
    {/if}
  {/if}
</div>
