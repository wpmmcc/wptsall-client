<script lang="ts">
  import { onMount } from 'svelte';
  import { apiFetch, isOk } from './lib/api/client';
  import { startPolling, fetchStatus, status } from './lib/stores/status';
  import { showToast } from './lib/stores/toast';
  import Sidebar from './lib/components/Sidebar.svelte';
  import Toast from './lib/components/Toast.svelte';
  import Overview from './pages/Overview.svelte';
  import Sites from './pages/Sites.svelte';
  import Components from './pages/Components.svelte';
  import ApiKeys from './pages/ApiKeys.svelte';
  import Tasks from './pages/Tasks.svelte';
  import TranslationReview from './pages/TranslationReview.svelte';
  import Logs from './pages/Logs.svelte';
  import History from './pages/History.svelte';
  import Settings from './pages/Settings.svelte';
  import StoragePaused from './lib/components/StoragePaused.svelte';
  import Products from './pages/Products.svelte';
  import { Menu, X } from 'lucide-svelte';
  import { _ } from 'svelte-i18n';
  import { initializeI18n } from './i18n';

  let currentPage = $state('overview');
  let currentItemId = $state(0);
  let mobileOpen = $state(false);
  let tasksInitialTab = $state<'jobs' | 'discovery' | 'pending_review' | 'sync_pairs'>('jobs');

  // P0-LF-04: Products is a legacy server-control-plane page; it renders only
  // when the backend reports legacy mode (its Sidebar entry is gated the same way).
  let legacyMode = $derived($status?.runtime_mode === 'legacy_server_control_plane');

  onMount(async () => {
    await initializeI18n();
    startPolling(10000);
  });

  async function handleLogout() {
    const r = await apiFetch<unknown>('/api/logout', { method: 'POST' });
    if (isOk(r)) {
      await fetchStatus(); // 立即刷新状态，不等下次轮询
      showToast('success', $_('login.logged_out'));
    } else {
      showToast('error', $_('login.logout_failed'), r.error?.message);
    }
  }

  function handleNavigate(page: string) {
    if (page !== 'tasks') tasksInitialTab = 'jobs';
    currentPage = page;
    mobileOpen = false;
  }

  function openPendingReview() {
    tasksInitialTab = 'pending_review';
    currentPage = 'tasks';
    mobileOpen = false;
  }
</script>

<!-- S12（批 G）：WPTSALL_ALLOW_INSECURE_TLS 开启时的醒目警示横幅。 -->
{#if $status?.allow_insecure_tls}
  <div
    role="alert"
    data-testid="insecure-tls-banner"
    class="w-full border-b border-amber-400 bg-amber-100 px-4 py-2 text-center text-sm font-medium text-amber-900"
  >
    {$_('security.insecure_tls_banner')}
  </div>
{/if}

<!-- 主界面：默认进入本地控制面；官网 OAuth 不再是运行时前置条件。 -->
<div class="relative flex min-h-screen bg-white">
    <!-- 移动菜单按钮 -->
    <button
      onclick={() => mobileOpen = !mobileOpen}
      class="fixed top-4 left-4 z-50 lg:hidden bg-gray-900 text-white p-2.5 rounded-lg hover:bg-gray-800 transition-colors"
      aria-label={$_('a11y.toggle_navigation_menu')}
      aria-expanded={mobileOpen}
    >
      {#if mobileOpen}
        <X size={24} aria-hidden="true" />
      {:else}
        <Menu size={24} aria-hidden="true" />
      {/if}
    </button>

    <!-- 移动菜单背景 -->
    {#if mobileOpen}
      <div
        class="fixed inset-0 z-30 bg-black/30 lg:hidden"
        onclick={() => mobileOpen = false}
        role="presentation"
      ></div>
    {/if}

    <!-- Sidebar 响应式 -->
    <Sidebar
      {currentPage}
      onNavigate={handleNavigate}
      onLogout={handleLogout}
      mobileOpen={mobileOpen}
    />

    <!-- 主内容 -->
    <main class="flex-1 overflow-auto">
      <div class="max-w-5xl mx-auto px-6 pt-20 pb-6 lg:pt-6">
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
          <TranslationReview itemId={currentItemId} onBack={() => currentPage = 'tasks'} />
        {:else if currentPage === 'logs'}
          <Logs />
        {:else if currentPage === 'history'}
          <History />
        {:else if currentPage === 'settings'}
          <Settings />
        {:else if currentPage === 'products' && legacyMode}
          <Products />
        {/if}
      </div>
    </main>
</div>

<Toast />
