<script lang="ts">
  import { getTranslations, retryTranslation, batchRetryTranslations, batchDeleteTranslations, type TranslationRecord, type TranslationQueryParams } from '../lib/api/translations';
  import { _ } from 'svelte-i18n';

  let records = $state<TranslationRecord[]>([]);
  let total = $state(0);
  let page = $state(1);
  let limit = $state(20);
  let loading = $state(false);
  let error = $state('');
  let expandedId = $state<number | null>(null);
  let retryingId = $state<number | null>(null);
  let selectedIds = $state<Set<number>>(new Set());
  let batchProcessing = $state(false);

  // Filters
  let filterDomain = $state('');
  let filterStatus = $state('');
  let filterSearch = $state('');

  async function load() {
    loading = true;
    error = '';
    try {
      const params: TranslationQueryParams = { page, limit };
      if (filterDomain) params.domain = filterDomain;
      if (filterStatus) params.status = filterStatus;
      if (filterSearch) params.search = filterSearch;
      const res = await getTranslations(params);
      if (res.success) {
        records = res.data.records;
        total = res.data.total;
      } else {
        error = res.error?.message || $_('common.load_failed');
      }
    } catch (e: any) {
      error = e.message || $_('common.load_failed');
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    // Reactive: reload when filters or page change
    page;
    limit;
    filterDomain;
    filterStatus;
    filterSearch;
    load();
  });

  function clearFilters() {
    filterDomain = '';
    filterStatus = '';
    filterSearch = '';
    page = 1;
  }

  function formatTime(ts: number): string {
    return new Date(ts * 1000).toLocaleString(undefined, {
      year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    });
  }

  function formatMs(ms: number | undefined): string {
    if (ms === undefined || ms === null) return '-';
    if (ms >= 1000) return (ms / 1000).toFixed(2) + 's';
    return ms + 'ms';
  }

  function toggleExpand(id: number) {
    expandedId = expandedId === id ? null : id;
  }

  async function handleRetry(id: number) {
    retryingId = id;
    try {
      const res = await retryTranslation(id);
      if (res.success) {
        await load();
      } else {
        error = res.error?.message || $_('history.retry_failed');
      }
    } catch (e: any) {
      error = e.message || $_('history.retry_failed');
    } finally {
      retryingId = null;
    }
  }

  const totalPages = $derived(Math.ceil(total / limit) || 1);
  let selectedCount = $derived(selectedIds.size);
  let allSelected = $derived(records.length > 0 && records.every(r => selectedIds.has(r.id)));

  function toggleSelect(id: number) {
    const next = new Set(selectedIds);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    selectedIds = next;
  }

  function toggleSelectAll() {
    if (allSelected) {
      selectedIds = new Set();
    } else {
      selectedIds = new Set(records.map(r => r.id));
    }
  }

  // Clear selection when page/filter changes
  $effect(() => {
    page;
    limit;
    filterDomain;
    filterStatus;
    filterSearch;
    selectedIds = new Set();
  });

  async function handleBatchRetry() {
    const failedIds = [...selectedIds].filter(id => {
      const r = records.find(rec => rec.id === id);
      return r && r.status === 'failed';
    });
    if (failedIds.length === 0) {
      error = $_('history.no_failed_selected');
      return;
    }
    batchProcessing = true;
    error = '';
    try {
      const res = await batchRetryTranslations(failedIds);
      if (res.success) {
        selectedIds = new Set();
        await load();
      } else {
        error = res.error?.message || $_('history.batch_retry_error');
      }
    } catch (e: any) {
      error = e.message || $_('history.batch_retry_error');
    } finally {
      batchProcessing = false;
    }
  }

  async function handleBatchDelete() {
    const ids = [...selectedIds];
    if (ids.length === 0) return;
    if (!confirm($_('history.confirm_batch_delete', { values: { count: ids.length } }))) return;
    batchProcessing = true;
    error = '';
    try {
      const res = await batchDeleteTranslations(ids);
      if (res.success) {
        selectedIds = new Set();
        await load();
      } else {
        error = res.error?.message || $_('history.batch_delete_error');
      }
    } catch (e: any) {
      error = e.message || $_('history.batch_delete_error');
    } finally {
      batchProcessing = false;
    }
  }

  function statusBadgeClass(status: string): string {
    switch (status) {
      case 'success':          return 'bg-green-100 text-green-700';
      case 'failed':           return 'bg-red-100 text-red-700';
      case 'pending_callback': return 'bg-yellow-100 text-yellow-700';
      default:                 return 'bg-gray-100 text-gray-600';
    }
  }

  function statusLabel(status: string): string {
    switch (status) {
      case 'success':          return $_('history.status_success');
      case 'failed':           return $_('history.status_failed');
      case 'pending_callback': return $_('history.status_pending_callback');
      default:                 return status;
    }
  }

  function formatComponentTrace(componentIds: string[]): string {
    if (!componentIds || componentIds.length === 0) return $_('history.no_component');
    return componentIds.join(' -> ');
  }

  function summaryBadgeClass(kind: 'component' | 'media' | 'failed' | 'reason'): string {
    switch (kind) {
      case 'component': return 'bg-slate-100 text-slate-700';
      case 'media': return 'bg-blue-100 text-blue-700';
      case 'failed': return 'bg-red-100 text-red-700';
      case 'reason': return 'bg-amber-100 text-amber-700';
    }
  }
</script>

<!-- 页面标题 -->
<div class="mb-6 flex items-center justify-between">
  <div>
    <h2 class="text-xl font-semibold text-gray-900">{$_('history.title')}</h2>
    <p class="text-sm text-gray-500 mt-1">{$_('history.subtitle')}</p>
  </div>
  <button
    onclick={() => load()}
    disabled={loading}
    class="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors"
  >
    {loading ? $_('common.refreshing') : $_('common.refresh')}
  </button>
</div>

<!-- 过滤栏 -->
<div class="bg-white border border-gray-200 rounded-xl p-4 mb-4">
  <div class="flex items-center gap-3 flex-wrap">
    <input
      bind:value={filterDomain}
      placeholder={$_('history.filter_domain')}
      class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm w-52"
      oninput={() => { page = 1; }}
    />
    <select data-testid="status-filter"
      bind:value={filterStatus}
      class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm"
      onchange={() => { page = 1; }}
    >
      <option value="">{$_('history.filter_status')}</option>
      <option value="success">{$_('history.status_success')}</option>
      <option value="failed">{$_('history.status_failed')}</option>
      <option value="pending_callback">{$_('history.status_pending_callback')}</option>
    </select>
    <input
      bind:value={filterSearch}
      placeholder={$_('history.filter_search')}
      class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm w-48"
      oninput={() => { page = 1; }}
    />
    <button
      onclick={clearFilters}
      class="px-3 py-1.5 border border-gray-200 text-gray-600 text-sm rounded-lg hover:bg-gray-50 transition-colors"
    >
      {$_('history.clear_filters')}
    </button>
    {#if total > 0}
      <span class="text-xs text-gray-400 ml-auto">{$_('history.total_records', { values: { count: total } })}</span>
    {/if}
  </div>
</div>

<!-- 错误提示 -->
{#if error}
  <div class="mb-4 px-4 py-3 bg-red-50 border border-red-200 rounded-xl text-sm text-red-700">
    {error}
  </div>
{/if}

<!-- 批量操作栏 -->
{#if selectedCount > 0}
  <div class="mb-4 px-4 py-3 bg-blue-50 border border-blue-200 rounded-xl flex items-center gap-3">
    <span class="text-sm text-blue-700 font-medium">{$_('history.selected_count', { values: { count: selectedCount } })}</span>
    <button
      onclick={handleBatchRetry}
      disabled={batchProcessing}
      class="px-3 py-1.5 bg-orange-500 text-white text-xs rounded-lg hover:bg-orange-600 disabled:opacity-60 transition-colors"
    >
      {batchProcessing ? $_('history.processing') : $_('history.batch_retry', { values: { count: selectedCount } })}
    </button>
    <button
      onclick={handleBatchDelete}
      disabled={batchProcessing}
      class="px-3 py-1.5 bg-red-500 text-white text-xs rounded-lg hover:bg-red-600 disabled:opacity-60 transition-colors"
    >
      {batchProcessing ? $_('history.processing') : $_('history.batch_delete', { values: { count: selectedCount } })}
    </button>
    <button
      onclick={() => { selectedIds = new Set(); }}
      class="px-3 py-1.5 border border-gray-200 text-gray-600 text-xs rounded-lg hover:bg-gray-50 transition-colors"
    >
      {$_('history.cancel_select')}
    </button>
  </div>
{/if}

<!-- 表格 -->
<div class="bg-white border border-gray-200 rounded-xl overflow-hidden mb-4">
  <div class="overflow-auto">
    <table class="w-full text-sm">
      <thead>
        <tr class="text-xs text-gray-500 bg-gray-50">
          <th class="px-3 py-2.5 w-8">
            <input
              type="checkbox"
              checked={allSelected}
              onchange={toggleSelectAll}
              class="w-3.5 h-3.5 rounded border-gray-300 text-blue-600 cursor-pointer"
            />
          </th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('history.th_time')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('history.th_domain')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('history.th_object_type')}</th>
          <th class="px-4 py-2.5 text-center font-medium">{$_('history.th_object_id')}</th>
          <th class="px-4 py-2.5 text-center font-medium">{$_('history.th_lang')}</th>
          <th class="px-4 py-2.5 text-center font-medium">{$_('history.th_field_count')}</th>
          <th class="px-4 py-2.5 text-center font-medium">{$_('history.th_duration')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('history.th_summary')}</th>
          <th class="px-4 py-2.5 text-center font-medium">{$_('history.th_status')}</th>
        </tr>
      </thead>
      <tbody>
        {#if loading && records.length === 0}
          <tr>
            <td colspan="10" class="px-4 py-12 text-center text-gray-400 text-sm">
              <div class="flex items-center justify-center gap-2">
                <svg class="animate-spin w-4 h-4 text-blue-500" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
                  <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                  <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"></path>
                </svg>
                {$_('common.loading')}
              </div>
            </td>
          </tr>
        {:else if records.length === 0}
          <tr>
            <td colspan="10" class="px-4 py-12 text-center text-gray-400 text-sm">
              {$_('history.no_records')}
            </td>
          </tr>
        {:else}
          {#each records as record}
            <!-- 主行 -->
            <tr
              onclick={() => toggleExpand(record.id)}
              class="border-t border-gray-50 cursor-pointer transition-colors
                {expandedId === record.id ? 'bg-blue-50' : 'hover:bg-gray-50'}"
            >
              <td class="px-3 py-2.5 w-8" onclick={(e) => e.stopPropagation()}>
                <input
                  type="checkbox"
                  checked={selectedIds.has(record.id)}
                  onchange={() => toggleSelect(record.id)}
                  class="w-3.5 h-3.5 rounded border-gray-300 text-blue-600 cursor-pointer"
                />
              </td>
              <td class="px-4 py-2.5 text-xs text-gray-600 whitespace-nowrap">
                {formatTime(record.created_at)}
              </td>
              <td class="px-4 py-2.5 text-xs text-gray-700 max-w-[160px] truncate" title={record.domain}>
                {record.domain.replace(/^https?:\/\//, '')}
              </td>
              <td class="px-4 py-2.5 text-xs text-gray-600">
                {record.object_type ?? '-'}
              </td>
              <td class="px-4 py-2.5 text-center text-xs text-gray-600">
                {record.object_id ?? '-'}
              </td>
              <td class="px-4 py-2.5 text-center text-xs text-gray-600 whitespace-nowrap">
                {record.source_lang} → {record.target_lang}
              </td>
              <td class="px-4 py-2.5 text-center text-xs text-gray-700">
                {record.fields_count}
              </td>
              <td class="px-4 py-2.5 text-center text-xs text-gray-600 whitespace-nowrap">
                {formatMs(record.execution_ms)}
              </td>
              <td class="px-4 py-2.5 text-xs text-gray-600 min-w-[240px]">
                <div class="flex flex-wrap items-center gap-1.5">
                  <span
                    class="inline-flex max-w-[220px] items-center rounded-full px-2 py-0.5 text-[11px] truncate {summaryBadgeClass('component')}"
                    title={formatComponentTrace(record.component_ids)}
                  >
                    {formatComponentTrace(record.component_ids)}
                  </span>
                  {#if record.media_mappings_count > 0}
                    <span class="inline-flex items-center rounded-full px-2 py-0.5 text-[11px] {summaryBadgeClass('media')}">
                      {$_('history.media_badge', { values: { count: record.media_mappings_count } })}
                    </span>
                  {/if}
                  {#if record.failed_fields_count > 0}
                    <span class="inline-flex items-center rounded-full px-2 py-0.5 text-[11px] {summaryBadgeClass('failed')}">
                      {$_('history.failed_fields_badge', { values: { count: record.failed_fields_count } })}
                    </span>
                  {/if}
                  {#if record.primary_failure_reason}
                    <span
                      class="inline-flex max-w-[220px] items-center rounded-full px-2 py-0.5 text-[11px] truncate {summaryBadgeClass('reason')}"
                      title={record.primary_failure_reason}
                    >
                      {record.primary_failure_reason}
                    </span>
                  {/if}
                </div>
              </td>
              <td class="px-4 py-2.5 text-center">
                <span class="text-xs px-2 py-0.5 rounded-full {statusBadgeClass(record.status)}">
                  {statusLabel(record.status)}
                </span>
              </td>
            </tr>

            <!-- 展开详情行 -->
            {#if expandedId === record.id}
              <tr class="bg-blue-50 border-t border-blue-100">
                <td colspan="10" class="px-6 py-3">
                  <div class="grid grid-cols-2 gap-x-8 gap-y-1 text-xs text-gray-600">
                    <div class="flex gap-2">
                      <span class="text-gray-400 min-w-[80px]">{$_('history.record_id')}</span>
                      <span>{record.id}</span>
                    </div>
                    <div class="flex gap-2">
                      <span class="text-gray-400 min-w-[80px]">{$_('history.th_lang')}</span>
                      <span>{record.source_lang} → {record.target_lang}</span>
                    </div>
                    {#if record.relation_id !== undefined && record.relation_id !== null}
                      <div class="flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.relation_id')}</span>
                        <span>{record.relation_id}</span>
                      </div>
                    {/if}
                    {#if record.business_line}
                      <div class="flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.business_line')}</span>
                        <span>{record.business_line}</span>
                      </div>
                    {/if}
                    <div class="flex gap-2">
                      <span class="text-gray-400 min-w-[80px]">{$_('history.exec_component')}</span>
                      <span class="break-all">{formatComponentTrace(record.component_ids)}</span>
                    </div>
                    <div class="flex gap-2">
                      <span class="text-gray-400 min-w-[80px]">{$_('history.th_summary')}</span>
                      <span>
                        {$_('history.media_badge', { values: { count: record.media_mappings_count } })},
                        {$_('history.failed_fields_badge', { values: { count: record.failed_fields_count } })}
                      </span>
                    </div>
                    {#if record.worker_id}
                      <div class="flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.worker_id')}</span>
                        <span class="font-mono truncate max-w-[200px]" title={record.worker_id}>{record.worker_id}</span>
                      </div>
                    {/if}
                    {#if record.idempotency_key}
                      <div class="flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.idempotency_key')}</span>
                        <span class="font-mono truncate max-w-[200px]" title={record.idempotency_key}>{record.idempotency_key}</span>
                      </div>
                    {/if}
                    {#if record.callback_sent_at !== undefined && record.callback_sent_at !== null}
                      <div class="flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.callback_time')}</span>
                        <span>{formatTime(record.callback_sent_at)}</span>
                      </div>
                    {/if}
                    {#if record.callback_retries > 0}
                      <div class="flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.callback_retries')}</span>
                        <span>{record.callback_retries} {$_('history.times_unit')}</span>
                      </div>
                    {/if}
                    {#if record.primary_failure_reason}
                      <div class="col-span-2 flex gap-2">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.primary_failure')}</span>
                        <span class="text-amber-700 break-all">{record.primary_failure_reason}</span>
                      </div>
                    {/if}
                    {#if record.error_message}
                      <div class="col-span-2 flex gap-2 mt-1">
                        <span class="text-gray-400 min-w-[80px]">{$_('history.error_msg')}</span>
                        <span class="text-red-600 break-all">{record.error_message}</span>
                      </div>
                    {/if}
                    {#if record.status === 'failed'}
                      <div class="col-span-2 mt-2">
                        <button
                          data-testid="history-retry"
                          data-record-id={record.id}
                          onclick={() => handleRetry(record.id)}
                          disabled={retryingId === record.id}
                          class="px-3 py-1 bg-orange-500 text-white text-xs rounded-lg hover:bg-orange-600 disabled:opacity-60 transition-colors"
                        >
                          {retryingId === record.id ? $_('history.retrying') : $_('history.retry_this')}
                        </button>
                      </div>
                    {/if}
                  </div>
                </td>
              </tr>
            {/if}
          {/each}
        {/if}
      </tbody>
    </table>
  </div>
</div>

<!-- 分页 -->
{#if totalPages > 1 || total > 0}
  <div class="flex items-center justify-between">
    <div class="text-xs text-gray-500">
      {$_('history.page_info', { values: { page, totalPages, total } })}
    </div>
    <div class="flex items-center gap-1">
      <button
        onclick={() => { page = 1; }}
        disabled={page <= 1 || loading}
        class="px-3 py-1.5 text-xs border border-gray-200 rounded-lg disabled:opacity-40 hover:bg-gray-50 transition-colors"
      >
        {$_('history.first_page')}
      </button>
      <button
        onclick={() => { page = page - 1; }}
        disabled={page <= 1 || loading}
        class="px-3 py-1.5 text-xs border border-gray-200 rounded-lg disabled:opacity-40 hover:bg-gray-50 transition-colors"
      >
        {$_('history.prev_page')}
      </button>
      <span class="px-3 py-1.5 text-xs text-gray-600">{page}</span>
      <button
        onclick={() => { page = page + 1; }}
        disabled={page >= totalPages || loading}
        class="px-3 py-1.5 text-xs border border-gray-200 rounded-lg disabled:opacity-40 hover:bg-gray-50 transition-colors"
      >
        {$_('history.next_page')}
      </button>
      <button
        onclick={() => { page = totalPages; }}
        disabled={page >= totalPages || loading}
        class="px-3 py-1.5 text-xs border border-gray-200 rounded-lg disabled:opacity-40 hover:bg-gray-50 transition-colors"
      >
        {$_('history.last_page')}
      </button>
    </div>
    <div class="flex items-center gap-2 text-xs text-gray-500">
      {$_('history.per_page')}
      <select
        bind:value={limit}
        onchange={() => { page = 1; }}
        class="border border-gray-200 rounded-lg px-2 py-1 text-xs"
      >
        <option value={10}>10</option>
        <option value={20}>20</option>
        <option value={50}>50</option>
        <option value={100}>100</option>
      </select>
      {$_('history.items_unit')}
    </div>
  </div>
{/if}
