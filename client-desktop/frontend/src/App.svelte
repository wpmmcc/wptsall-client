<script lang="ts">
  /**
   * Desktop shell: shared WebUI Sidebar + pages (parity with client-wpplugin WebUI).
   * Nav IA matches WebUI local mode: 8 primary entries; Review is reached from Tasks.
   */
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { Menu, X } from 'lucide-svelte';
  import { initializeI18n } from './i18n';
  import { startPolling, fetchStatus, status } from '@webui/lib/stores/status';
  import Sidebar from '@webui/lib/components/Sidebar.svelte';
  import Toast from '@webui/lib/components/Toast.svelte';
  import StoragePaused from '@webui/lib/components/StoragePaused.svelte';

  import Overview from '@webui/pages/Overview.svelte';
  import Sites from '@webui/pages/Sites.svelte';
  import ApiKeys from '@webui/pages/ApiKeys.svelte';
  import Tasks from '@webui/pages/Tasks.svelte';
  import TranslationReview from '@webui/pages/TranslationReview.svelte';
  import Components from '@webui/pages/Components.svelte';
  import History from '@webui/pages/History.svelte';
  import Logs from '@webui/pages/Logs.svelte';
  import Settings from '@webui/pages/Settings.svelte';

  /** Same page ids as WebUI App.svelte (local mode). */
  type Page =
    | 'overview'
    | 'sites'
    | 'components'
    | 'apikeys'
    | 'tasks'
    | 'review'
    | 'logs'
    | 'history'
    | 'settings';

  type TasksTab = 'jobs' | 'discovery' | 'pending_review' | 'sync_pairs';

  let currentPage = $state<Page>('overview');
  let currentItemId = $state(0);
  let refreshing = $state(false);
  let tasksInitialTab = $state<TasksTab>('jobs');
  // 窗口完整性批：Tauri minWidth=800 < Sidebar 的 lg(1024px) 静态断点 →
  // 窗口拖到 800–1023px 时共享 Sidebar 落入 fixed 离屏态。WebUI 壳带完整
  // 移动侧栏控制（汉堡+遮罩+mobileOpen），桌面壳此前漏接 → 侧栏永久消失
  // 且无法导航。此处补齐同构逻辑。
  let mobileOpen = $state(false);

  async function refreshAll() {
    refreshing = true;
    try {
      await fetchStatus();
    } finally {
      refreshing = false;
    }
  }

  function handleNavigate(page: string) {
    if (page !== 'tasks') tasksInitialTab = 'jobs';
    currentPage = page as Page;
    mobileOpen = false;
  }

  function openPendingReview() {
    tasksInitialTab = 'pending_review';
    currentPage = 'tasks';
  }

  function handleLogout() {
    // Local-first desktop has no cloud session; Sidebar only shows logout in legacy mode.
  }

  onMount(() => {
    initializeI18n();
    startPolling(10000);
  });
</script>

<div class="relative flex h-screen bg-white">
  <!-- 移动菜单背景（<lg 窗口宽）：遮罩点击收起，与 WebUI 壳同构 -->
  {#if mobileOpen}
    <div
      class="fixed inset-0 z-30 bg-black/30 lg:hidden"
      onclick={() => { mobileOpen = false; }}
      role="presentation"
    ></div>
  {/if}
  <Sidebar
    currentPage={currentPage}
    onNavigate={handleNavigate}
    onLogout={handleLogout}
    mobileOpen={mobileOpen}
  />

  <main class="flex-1 overflow-auto flex flex-col min-w-0">
    <header class="bg-white border-b border-gray-200 px-6 py-3 flex items-center justify-between gap-4 shrink-0">
      <div class="flex items-center gap-2 min-w-0">
        <!-- 汉堡钮内联在 header 流内（WebUI 壳无 header 用 fixed；桌面壳有
             runtime_note 文案，fixed 会盖字）：<lg 窗口宽时唤回离屏侧栏 -->
        <button
          type="button"
          class="lg:hidden p-2 -ml-2 rounded-lg text-gray-600 hover:bg-gray-100 transition-colors shrink-0"
          onclick={() => { mobileOpen = !mobileOpen; }}
          aria-label={$_('a11y.toggle_navigation_menu')}
          aria-expanded={mobileOpen}
        >
          {#if mobileOpen}
            <X size={20} aria-hidden="true" />
          {:else}
            <Menu size={20} aria-hidden="true" />
          {/if}
        </button>
        <p class="text-xs text-gray-500 truncate">{$_('shell.runtime_note')}</p>
      </div>
      <button
        type="button"
        class="text-xs px-3 py-1.5 border border-gray-200 rounded-lg hover:bg-gray-50 disabled:opacity-50 font-medium text-gray-700 shrink-0"
        disabled={refreshing}
        onclick={refreshAll}
      >
        {refreshing ? $_('common.refreshing') : $_('common.refresh')}
      </button>
    </header>

    <div class="flex-1 overflow-y-auto">
      <div class="max-w-5xl mx-auto px-6 py-6">
        {#if $status?.storage_paused && $status.database_available === false}
          <StoragePaused />
        {:else if currentPage === 'overview'}
          <Overview onOpenPendingReview={openPendingReview} />
        {:else if currentPage === 'sites'}
          <Sites />
        {:else if currentPage === 'components'}
          <Components />
        {:else if currentPage === 'apikeys'}
          <ApiKeys />
        {:else if currentPage === 'tasks'}
          <Tasks
            initialTab={tasksInitialTab}
            onReviewItem={(id) => { currentItemId = id; currentPage = 'review'; }}
          />
        {:else if currentPage === 'review'}
          <TranslationReview itemId={currentItemId} onBack={() => { currentPage = 'tasks'; }} />
        {:else if currentPage === 'logs'}
          <Logs />
        {:else if currentPage === 'history'}
          <History />
        {:else if currentPage === 'settings'}
          <Settings />
        {/if}
      </div>
    </div>
  </main>
</div>

<Toast />
