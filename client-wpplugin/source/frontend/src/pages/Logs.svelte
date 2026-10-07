<script lang="ts">
  import { clearLogs, loadRecentLogs } from '../lib/api/settings';
  import { showToast } from '../lib/stores/toast';
  import { _ } from 'svelte-i18n';

  let limit = $state(200);
  let filterText = $state('');
  let levelFilter = $state('all');
  let eventPrefix = $state('');
  let autoRefresh = $state(false);
  let rawLines = $state<string[]>([]);
  let nextBeforeTsMs = $state<number | null>(null);
  let hasMore = $state(false);
  let loading = $state(false);
  let loadingOlder = $state(false);
  let clearing = $state(false);
  const logsLimitInputId = 'logs-limit-input';
  const logsLevelSelectId = 'logs-level-select';

  async function loadLogs(opts?: { older?: boolean }) {
    const older = !!opts?.older;
    if (older) {
      if (!hasMore || nextBeforeTsMs == null) return;
      loadingOlder = true;
    } else {
      if (loading) return;
      loading = true;
    }
    try {
      const minLevel = levelFilter === 'all' ? undefined : levelFilter;
      const res = await loadRecentLogs(
        limit,
        minLevel,
        older ? (nextBeforeTsMs ?? undefined) : undefined,
        eventPrefix,
      );
      if (res.success) {
        const lines = res.data.lines ?? [];
        if (older) {
          rawLines = [...lines, ...rawLines];
        } else {
          rawLines = lines;
        }
        nextBeforeTsMs =
          typeof res.data.next_before_ts_ms === 'number' ? res.data.next_before_ts_ms : null;
        hasMore = !!res.data.has_more;
      } else if (res.error?.code === 'NETWORK_ERROR') {
        showToast('error', $_('login.network_error'), res.error.message);
      } else {
        showToast('error', $_('logs.load_error'), res.error?.message);
      }
    } catch (e) {
      showToast('error', $_('login.network_error'), String(e));
    } finally {
      loading = false;
      loadingOlder = false;
    }
  }

  async function clearAllLogs() {
    clearing = true;
    try {
      const res = await clearLogs();
      if (res.success) {
        rawLines = [];
        nextBeforeTsMs = null;
        hasMore = false;
        showToast('success', $_('logs.clear_ok'));
      } else {
        showToast('error', $_('logs.clear_error'), res.error?.message);
      }
    } catch (e) {
      showToast('error', $_('logs.clear_error'), String(e));
    } finally {
      clearing = false;
    }
  }

  // Auto-refresh: poll the tail every 10s while enabled (mirrors the
  // Overview status poll cadence). Paused automatically while a load is
  // already in flight; cleaned up on component destroy.
  $effect(() => {
    if (!autoRefresh) return;
    const timer = setInterval(() => {
      void loadLogs();
    }, 10_000);
    return () => clearInterval(timer);
  });

  let filteredLines = $derived(
    filterText.trim()
      ? rawLines.filter((l) => l.toLowerCase().includes(filterText.toLowerCase()))
      : rawLines
  );

  function lineClass(line: string): string {
    const lower = line.toLowerCase();
    if (lower.includes('"level":"error"') || lower.includes('"level":"err"')) {
      return 'text-red-400';
    }
    if (lower.includes('"level":"warn"') || lower.includes('"level":"warning"')) {
      return 'text-yellow-400';
    }
    if (lower.includes('"level":"debug"')) {
      return 'text-gray-500';
    }
    return 'text-gray-300';
  }

  function formatLine(line: string): string {
    try {
      const obj = JSON.parse(line);
      const ts = obj.ts ? new Date(obj.ts * 1000).toLocaleTimeString('zh-CN') : '';
      const level = obj.level ?? '';
      const event = obj.event ?? obj.msg ?? '';
      const rest = Object.entries(obj)
        .filter(([k]) => !['ts', 'ts_ms', 'level', 'event', 'msg'].includes(k))
        .map(([k, v]) => `${k}=${typeof v === 'string' ? v : JSON.stringify(v)}`)
        .join(' ');
      return [ts, level.toUpperCase().padEnd(5), event, rest].filter(Boolean).join('  ');
    } catch {
      return line;
    }
  }

  function downloadLogs() {
    const lines = filteredLines.length ? filteredLines : rawLines;
    if (!lines.length) {
      showToast('warning', $_('logs.download_empty'));
      return;
    }
    const blob = new Blob([lines.join('\n') + '\n'], { type: 'application/x-ndjson' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `wptsall-client-logs-${new Date().toISOString().replace(/[:.]/g, '-')}.jsonl`;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('logs.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('logs.subtitle')}</p>
</div>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 flex items-center gap-3 flex-wrap">
    <div class="flex items-center gap-2">
      <label for={logsLimitInputId} class="text-sm text-gray-600">{$_('logs.line_count')}</label>
      <input
        id={logsLimitInputId}
        bind:value={limit}
        type="number"
        min="10"
        max="1000"
        class="w-20 border border-gray-200 rounded-lg px-2 py-1.5 text-sm text-center" />
    </div>
    <div class="flex items-center gap-2">
      <label for={logsLevelSelectId} class="text-sm text-gray-600">{$_('logs.level')}</label>
      <select
        id={logsLevelSelectId}
        bind:value={levelFilter}
        data-testid="logs-level-filter"
        class="border border-gray-200 rounded-lg px-2 py-1.5 text-sm">
        <option value="all">{$_('logs.level_all')}</option>
        <option value="debug">debug</option>
        <option value="info">info</option>
        <option value="warn">warn</option>
        <option value="error">error</option>
      </select>
    </div>
    <input
      bind:value={filterText}
      data-testid="logs-keyword-filter"
      placeholder={$_('logs.filter_placeholder')}
      class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm w-48" />
    <input
      bind:value={eventPrefix}
      data-testid="logs-event-filter"
      placeholder={$_('logs.event_filter_placeholder')}
      title={$_('logs.event_filter_hint')}
      class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm w-44 font-mono" />
    <label class="flex items-center gap-1.5 text-sm text-gray-600 select-none">
      <input
        type="checkbox"
        bind:checked={autoRefresh}
        data-testid="logs-auto-refresh"
        class="accent-blue-600" />
      {$_('logs.auto_refresh')}
    </label>
    <button
      onclick={() => loadLogs()}
      disabled={loading}
      data-testid="logs-load"
      class="px-4 py-1.5 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors">
      {loading ? $_('logs.loading_logs') : $_('logs.load_logs')}
    </button>
    <button
      onclick={() => loadLogs({ older: true })}
      disabled={loadingOlder || !hasMore}
      data-testid="logs-load-older"
      class="px-4 py-1.5 border border-gray-300 text-sm rounded-lg hover:bg-gray-50 disabled:opacity-60 transition-colors">
      {loadingOlder ? $_('logs.loading_logs') : $_('logs.load_older')}
    </button>
    <button
      onclick={downloadLogs}
      disabled={rawLines.length === 0}
      data-testid="logs-download"
      class="px-4 py-1.5 border border-gray-300 text-sm rounded-lg hover:bg-gray-50 disabled:opacity-60 transition-colors">
      {$_('logs.download')}
    </button>
    <button
      onclick={clearAllLogs}
      disabled={clearing}
      data-testid="logs-clear"
      class="px-4 py-1.5 border border-red-300 text-red-600 text-sm rounded-lg hover:bg-red-50 disabled:opacity-60 transition-colors">
      {clearing ? $_('common.loading') : $_('logs.clear')}
    </button>
    {#if rawLines.length > 0}
      <span class="text-xs text-gray-400 ml-auto" data-testid="logs-showing">
        {$_('logs.showing', { values: { filtered: filteredLines.length, total: rawLines.length } })}
      </span>
    {/if}
  </div>

  <div class="bg-[#0f172a] rounded-b-xl overflow-auto max-h-[600px]" id="log-container">
    {#if rawLines.length === 0}
      <div class="py-16 text-center text-gray-500 text-sm" data-testid="logs-empty">
        {loading ? $_('common.loading') : $_('logs.click_load')}
      </div>
    {:else}
      <div class="p-4 font-mono text-xs leading-relaxed">
        {#each filteredLines as line, i}
          <div class="py-0.5 hover:bg-white/5 px-2 rounded {lineClass(line)}">
            <span class="select-none text-gray-600 mr-3"
              >{(rawLines.length - filteredLines.length + i + 1).toString().padStart(4, ' ')}</span
            >{formatLine(line)}
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
