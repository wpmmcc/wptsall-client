<script lang="ts">
  import { LayoutDashboard, Globe, Puzzle, Key, FileText, ClipboardList, Settings, LogOut, ListTodo, Package } from 'lucide-svelte';
  import { status } from '../stores/status';
  import { _ } from 'svelte-i18n';
  import { locale } from 'svelte-i18n';
  import { setLocale } from '../i18n';

  let { currentPage, onNavigate, onLogout, mobileOpen = false }: {
    currentPage: string;
    onNavigate: (page: string) => void;
    onLogout: () => void;
    mobileOpen?: boolean;
  } = $props();

  let currentLocale = $derived($locale ?? 'en');
  function setLocaleValue(lang: string) { setLocale(lang); }

  // P0-LF-04: the default UI is purely local. Legacy affordances (Products
  // page entry, session prefix, logout) render only in legacy server
  // control-plane mode; no Pro/subscription lookup is ever issued from here.
  let legacyMode = $derived($status?.runtime_mode === 'legacy_server_control_plane');

  type NavItem = { id: string; labelKey: string; icon: typeof LayoutDashboard; legacyOnly?: boolean };
  const navItems: NavItem[] = [
    { id: 'overview',    labelKey: 'nav.overview',    icon: LayoutDashboard },
    { id: 'sites',       labelKey: 'nav.sites',       icon: Globe },
    { id: 'components',  labelKey: 'nav.components',  icon: Puzzle },
    { id: 'apikeys',     labelKey: 'nav.api_keys',    icon: Key },
    { id: 'tasks',       labelKey: 'nav.tasks',       icon: ListTodo },
    { id: 'logs',        labelKey: 'nav.logs',        icon: FileText },
    { id: 'history',     labelKey: 'nav.history',     icon: ClipboardList },
    { id: 'settings',    labelKey: 'nav.settings',    icon: Settings },
    { id: 'products',    labelKey: 'products.legacy_title', icon: Package, legacyOnly: true },
  ];
  const visibleNavItems = $derived(navItems.filter((item) => !item.legacyOnly || legacyMode));
</script>

<nav
  class={`w-[200px] min-h-screen bg-white border-r border-gray-200 text-gray-700 flex flex-col shrink-0
    fixed lg:static inset-0 z-40 transition-transform
    ${mobileOpen ? 'translate-x-0' : '-translate-x-full lg:translate-x-0'}`}
  aria-label={$_('a11y.main_navigation')}>
  <div class="px-4 py-5 border-b border-gray-200">
    <h1 class="text-gray-950 font-semibold text-sm tracking-wide">{$_('app.title')}</h1>
  </div>

  <div class="flex-1 py-3 px-2 space-y-0.5">
    {#each visibleNavItems as item}
      <button
        onclick={() => onNavigate(item.id)}
        aria-current={currentPage === item.id ? 'page' : undefined}
        class="w-full flex items-center gap-3 px-3 py-2 rounded-md text-sm transition-colors
          {currentPage === item.id
            ? 'bg-indigo-50 text-indigo-700 font-medium'
            : 'text-gray-600 hover:bg-gray-50 hover:text-gray-900'}"
      >
        <item.icon size={16} aria-hidden="true" />
        {$_(item.labelKey)}
      </button>
    {/each}
  </div>

  <div class="px-4 py-2 border-t border-gray-200" data-testid="lang-switcher">
    <span class="sr-only">{$_('settings.language')}</span>
    <div role="group" class="flex gap-1 text-xs">
      <button type="button"
        data-locale="en"
        aria-pressed={currentLocale === 'en'}
        onclick={() => setLocaleValue('en')}
        class="flex-1 rounded px-2 py-1.5 border focus:outline-none focus:ring-2 focus:ring-indigo-500 {currentLocale === 'en' ? 'bg-indigo-50 border-indigo-300 text-indigo-700' : 'bg-white border-gray-200 text-gray-700'}">{$_('language.en')}</button>
      <button type="button"
        data-locale="zh-CN"
        aria-pressed={currentLocale === 'zh-CN'}
        onclick={() => setLocaleValue('zh-CN')}
        class="flex-1 rounded px-2 py-1.5 border focus:outline-none focus:ring-2 focus:ring-indigo-500 {currentLocale === 'zh-CN' ? 'bg-indigo-50 border-indigo-300 text-indigo-700' : 'bg-white border-gray-200 text-gray-700'}">{$_('language.zh_CN')}</button>
    </div>
  </div>

  {#if legacyMode}
    <div class="px-4 py-4 border-t border-gray-200 text-xs">
      {#if $status?.session_token_prefix}
        <div class="text-gray-500 mb-2 truncate">{$status.session_token_prefix}</div>
      {/if}
      <button onclick={onLogout}
        class="flex items-center gap-2 text-gray-500 hover:text-red-600 hover:bg-red-50 rounded px-2 py-1 transition-colors focus:outline-none focus:ring-2 focus:ring-red-500">
        <LogOut size={14} aria-hidden="true" /> {$_('nav.logout')}
      </button>
    </div>
  {/if}
</nav>
