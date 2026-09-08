<script lang="ts">
  import { loadRecentLogs } from '../lib/api/settings';
  import { showToast } from '../lib/stores/toast';
  import { _ } from 'svelte-i18n';

  let limit = $state(200);
  let filterText = $state('');
  let rawLines = $state<string[]>([]);
  let loading = $state(false);
  const logsLimitInputId = 'logs-limit-input';

  async function loadLogs() {
    loading = true;
    try {
      const res = await loadRecentLogs(limit);
      if (res.success) {
        rawLines = res.data.lines ?? [];
      } else if (res.error?.code === 'NETWORK_ERROR') {
        showToast('error', $_('login.network_error'), res.error.message);
      } else {
        showToast('error', $_('logs.load_error'), res.error?.message);
      }
    } catch (e) {
      showToast('error', $_('login.network_error'), String(e));
    } finally {
      loading = false;
    }
  }

  let filteredLines = $derived(
    filterText.trim()
      ? rawLines.filter(l => l.toLowerCase().includes(filterText.toLowerCase()))
      : rawLines
  );

  function lineClass(line: string): string {
    const lower = line.toLowerCase();
    if (lower.includes('"level":"error"') || lower.includes('"level":"err"') || lower.includes('"error"')) {
      return 'text-red-400';
    }
    if (lower.includes('"level":"warn"') || lower.includes('"level":"warning"')) {
      return 'text-yellow-400';
    }
    return 'text-gray-300';
  }

  // 尝试解析 JSON 行以提取关键字段
  function formatLine(line: string): string {
    try {
      const obj = JSON.parse(line);
      const ts = obj.ts ? new Date(obj.ts * 1000).toLocaleTimeString('zh-CN') : '';
      const level = obj.level ?? '';
      const event = obj.event ?? obj.msg ?? '';
      const rest = Object.entries(obj)
        .filter(([k]) => !['ts', 'level', 'event', 'msg'].includes(k))
        .map(([k, v]) => `${k}=${typeof v === 'string' ? v : JSON.stringify(v)}`)
        .join(' ');
      return [ts, level.toUpperCase().padEnd(5), event, rest].filter(Boolean).join('  ');
    } catch {
      return line;
    }
  }
</script>

<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('logs.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('logs.subtitle')}</p>
</div>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <!-- 工具栏 -->
  <div class="px-5 py-3 border-b border-gray-100 flex items-center gap-3 flex-wrap">
    <div class="flex items-center gap-2">
      <label for={logsLimitInputId} class="text-sm text-gray-600">{$_('logs.line_count')}</label>
      <input id={logsLimitInputId} bind:value={limit} type="number" min="10" max="1000"
        class="w-20 border border-gray-200 rounded-lg px-2 py-1.5 text-sm text-center" />
    </div>
    <input bind:value={filterText} placeholder={$_('logs.filter_placeholder')}
      class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm w-48" />
    <button onclick={loadLogs} disabled={loading}
      class="px-4 py-1.5 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors">
      {loading ? $_('logs.loading_logs') : $_('logs.load_logs')}
    </button>
    {#if rawLines.length > 0}
      <span class="text-xs text-gray-400 ml-auto">
        {$_('logs.showing', { values: { filtered: filteredLines.length, total: rawLines.length } })}
      </span>
    {/if}
  </div>

  <!-- 日志内容 -->
  <div class="bg-[#0f172a] rounded-b-xl overflow-auto max-h-[600px]" id="log-container">
    {#if rawLines.length === 0}
      <div class="py-16 text-center text-gray-500 text-sm">
        {loading ? $_('common.loading') : $_('logs.click_load')}
      </div>
    {:else}
      <div class="p-4 font-mono text-xs leading-relaxed">
        {#each filteredLines as line, i}
          <div class="py-0.5 hover:bg-white/5 px-2 rounded {lineClass(line)}">
            <span class="select-none text-gray-600 mr-3">{(rawLines.length - filteredLines.length + i + 1).toString().padStart(4, ' ')}</span>{formatLine(line)}
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
