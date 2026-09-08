<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { onMount } from 'svelte';
  import { listWpTranslationProviders } from '../api/keys';
  import type { WpTranslationProviderItem } from '../api/types';

  let items = $state<WpTranslationProviderItem[]>([]);
  let loading = $state(false);
  let error = $state('');

  async function loadItems() {
    loading = true;
    error = '';
    try {
      const res = await listWpTranslationProviders();
      if (res.success) {
        items = res.data.items;
      } else {
        error = (res as { error?: { message?: string } }).error?.message ?? $_('wp_providers_catalog.load_failed');
      }
    } catch (err: unknown) {
      error = (err as { message?: string })?.message ?? $_('wp_providers_catalog.load_failed');
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    loadItems();
  });
</script>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 flex items-center justify-between gap-3">
    <div>
      <h3 class="font-medium text-gray-900 text-sm">{$_('wp_providers_catalog.title')}</h3>
      <p class="text-xs text-gray-500 mt-1">{$_('wp_providers_catalog.subtitle')}</p>
    </div>
    <button
      onclick={loadItems}
      class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg hover:bg-gray-50 text-gray-600">
      {$_('common.refresh')}
    </button>
  </div>

  {#if error}
    <div class="border-b border-red-200 bg-red-50 px-5 py-3 text-sm text-red-700">{error}</div>
  {/if}

  <table class="w-full text-sm">
    <thead>
      <tr class="text-xs text-gray-500 bg-gray-50">
        <th class="px-4 py-2.5 text-left font-medium">{$_('wp_providers_catalog.th_name')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('wp_providers_catalog.th_id')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('wp_providers_catalog.th_kind')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('wp_providers_catalog.th_auth')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('wp_providers_catalog.th_active')}</th>
      </tr>
    </thead>
    <tbody>
      {#if loading}
        <tr><td colspan="5" class="px-4 py-8 text-center text-gray-400">{$_('common.loading')}</td></tr>
      {:else if items.length === 0}
        <tr><td colspan="5" class="px-4 py-8 text-center text-gray-400">{$_('wp_providers_catalog.no_data')}</td></tr>
      {:else}
        {#each items as item (item.id)}
          <tr class="border-t border-gray-50 align-top hover:bg-gray-50/50">
            <td class="px-4 py-3 font-medium text-gray-800">{item.name || item.id}</td>
            <td class="px-4 py-3 font-mono text-xs text-gray-500">{item.id}</td>
            <td class="px-4 py-3 text-xs text-gray-500">{item.provider_kind || '-'}</td>
            <td class="px-4 py-3 text-xs text-gray-500">{item.auth_mode || '-'}</td>
            <td class="px-4 py-3 text-xs text-gray-500">{item.active ? $_('common.yes') : $_('common.no')}</td>
          </tr>
        {/each}
      {/if}
    </tbody>
  </table>
</div>
