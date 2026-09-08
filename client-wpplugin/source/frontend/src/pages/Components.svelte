<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { status } from '../lib/stores/status';
  import MyComponentsTab from '../lib/components-page/MyComponentsTab.svelte';
  import CapabilitiesTab from '../lib/components-page/CapabilitiesTab.svelte';
  import ServerTemplatesTab from '../lib/components-page/ServerTemplatesTab.svelte';
  import TaskRoutingTab from '../lib/components-page/TaskRoutingTab.svelte';

  type Tab = 'my' | 'capabilities' | 'tasktype' | 'server';
  let activeTab = $state<Tab>('my');

  // P0-LF-04: the Server 模板 tab is a legacy server-control-plane surface;
  // the default local UI exposes only my/capabilities/tasktype tabs.
  let legacyMode = $derived($status?.runtime_mode === 'legacy_server_control_plane');
  const tabs = $derived<[Tab, string][]>(legacyMode
    ? [['my', 'components.tab_my'], ['capabilities', 'components.tab_capabilities'], ['tasktype', 'components.tab_routing'], ['server', 'components.tab_server']]
    : [['my', 'components.tab_my'], ['capabilities', 'components.tab_capabilities'], ['tasktype', 'components.tab_routing']]);
  $effect(() => {
    if (!legacyMode && activeTab === 'server') activeTab = 'my';
  });
</script>

<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('components.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('components.subtitle')}</p>
</div>

<div class="flex border-b border-gray-200 mb-4">
  {#each tabs as [id, label]}
    <button
      data-testid={`components-tab-${id}`}
      onclick={() => (activeTab = id)}
      class="px-4 py-2.5 text-sm font-medium border-b-2 transition-colors mr-1 {activeTab === id
        ? 'border-blue-600 text-blue-600'
        : 'border-transparent text-gray-500 hover:text-gray-700'}">
      {$_(label)}
    </button>
  {/each}
</div>

{#if activeTab === 'my'}
  <MyComponentsTab />
{:else if activeTab === 'capabilities'}
  <CapabilitiesTab />
{:else if activeTab === 'tasktype'}
  <TaskRoutingTab />
{:else if legacyMode}
  <ServerTemplatesTab />
{/if}
