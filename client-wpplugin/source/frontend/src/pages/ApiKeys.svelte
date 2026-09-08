<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { status } from '../lib/stores/status';
  import VendorCatalogTab from '../lib/api-keys-page/VendorCatalogTab.svelte';
  import IntegrationPackTab from '../lib/api-keys-page/IntegrationPackTab.svelte';
  import WpTranslationProvidersTab from '../lib/api-keys-page/WpTranslationProvidersTab.svelte';
  import CloudApiTypesTab from '../lib/api-keys-page/CloudApiTypesTab.svelte';
  import OAuthConfigsTab from '../lib/api-keys-page/OAuthConfigsTab.svelte';
  import VendorKeysTab from '../lib/api-keys-page/VendorKeysTab.svelte';

  type Tab = 'vendors' | 'integration_pack' | 'wp_providers' | 'cloud_api_types' | 'keys' | 'oauth';
  let activeTab = $state<Tab>('vendors');

  // P0-LF-04: the default page keeps the local signed provider catalog,
  // integration pack, vendor keys and OAuth configs. The official website
  // inventories (WP 翻译商 / Cloud API 类型, /api/wp-translation-providers and
  // /api/cloud-api-types) are legacy server-control-plane surfaces only.
  let legacyMode = $derived($status?.runtime_mode === 'legacy_server_control_plane');
  const tabs = $derived<[Tab, string][]>(legacyMode
    ? [
        ['vendors', 'apikeys.tab_vendors'],
        ['integration_pack', 'apikeys.tab_integration_pack'],
        ['wp_providers', 'apikeys.tab_wp_providers'],
        ['cloud_api_types', 'apikeys.tab_cloud_api_types'],
        ['keys', 'apikeys.tab_keys'],
        ['oauth', 'apikeys.tab_oauth'],
      ]
    : [
        ['vendors', 'apikeys.tab_vendors'],
        ['integration_pack', 'apikeys.tab_integration_pack'],
        ['keys', 'apikeys.tab_keys'],
        ['oauth', 'apikeys.tab_oauth'],
      ]);
  $effect(() => {
    if (!legacyMode && (activeTab === 'wp_providers' || activeTab === 'cloud_api_types')) activeTab = 'vendors';
  });
</script>

<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('apikeys.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('apikeys.subtitle')}</p>
</div>

<div class="flex border-b border-gray-200 mb-4 flex-wrap gap-1">
  {#each tabs as [id, label]}
    <button
      data-testid={`apikeys-tab-${id}`}
      onclick={() => (activeTab = id)}
      class="px-4 py-2.5 text-sm font-medium border-b-2 transition-colors mr-1 {activeTab === id
        ? 'border-blue-600 text-blue-600'
        : 'border-transparent text-gray-500 hover:text-gray-700'}">
      {$_(label)}
    </button>
  {/each}
</div>

{#if activeTab === 'vendors'}
  <VendorCatalogTab />
{:else if activeTab === 'integration_pack'}
  <IntegrationPackTab />
{:else if activeTab === 'wp_providers' && legacyMode}
  <WpTranslationProvidersTab />
{:else if activeTab === 'cloud_api_types' && legacyMode}
  <CloudApiTypesTab />
{:else if activeTab === 'keys'}
  <VendorKeysTab />
{:else}
  <OAuthConfigsTab />
{/if}
