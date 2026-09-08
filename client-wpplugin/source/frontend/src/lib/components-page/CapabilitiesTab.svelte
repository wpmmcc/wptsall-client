<script lang="ts">
  import { onMount } from 'svelte';
  import {
    getComponentCapabilities,
    type CapabilitiesResponse,
    type ComponentCapability,
  } from '../api/components';
  import {
    deriveWpSourceHints,
    formatCapabilityLabel,
    renderContentCapabilitySummary,
    renderHtmlSupportSummary,
  } from './helpers';
  import { _ } from 'svelte-i18n';

  let capabilities = $state<ComponentCapability[]>([]);
  let formatComponentMap = $state<Record<string, string[]>>({});
  let loading = $state(false);
  let error = $state('');

  const formatEntries = $derived(
    Object.entries(formatComponentMap).sort((a, b) => a[0].localeCompare(b[0]))
  );

  async function loadCapabilities() {
    loading = true;
    error = '';
    try {
      const res = await getComponentCapabilities();
      if (res.success) {
        const data = res.data as CapabilitiesResponse;
        capabilities = data.components;
        formatComponentMap = data.format_component_map;
      } else {
        error = (res as any).error?.message ?? $_('capabilities.load_failed');
      }
    } catch (err: any) {
      error = err?.message ?? $_('capabilities.load_failed');
    } finally {
      loading = false;
    }
  }

  function renderList(items: string[] | undefined, empty = $_('common.not_declared')) {
    return items && items.length > 0 ? items.join(', ') : empty;
  }

  function renderContractSummary(item: ComponentCapability) {
    const contract = item.client_contract;
    if (!contract) return $_('common.not_declared');
    return `${contract.input_mode} -> ${contract.workflow_mode} -> ${contract.output_mode}`;
  }

  function renderContractStages(item: ComponentCapability) {
    const stages = item.client_contract?.stages ?? [];
    return stages.length > 0 ? stages.join(' -> ') : 'request';
  }

  function renderWpHints(item: ComponentCapability) {
    const hints = deriveWpSourceHints(item.supported_content_formats, item.kind);
    return hints.length > 0 ? hints : [$_('common.not_declared')];
  }

  onMount(() => {
    loadCapabilities();
  });
</script>

<div class="space-y-4">
  <div class="flex items-center justify-between gap-3">
    <div>
      <h3 class="font-medium text-gray-900 text-sm">{$_('capabilities.title')}</h3>
      <p class="text-xs text-gray-500 mt-1">{$_('capabilities.subtitle')}</p>
    </div>
    <button
      onclick={loadCapabilities}
      class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg hover:bg-gray-50 text-gray-600">
      {$_('common.refresh')}
    </button>
  </div>

  {#if error}
    <div class="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
      {error}
    </div>
  {/if}

  <div class="grid gap-3 md:grid-cols-3">
    <div class="rounded-xl border border-gray-200 bg-white px-4 py-3">
      <div class="text-xs text-gray-500">{$_('capabilities.component_count')}</div>
      <div class="mt-1 text-2xl font-semibold text-gray-900">{capabilities.length}</div>
    </div>
    <div class="rounded-xl border border-gray-200 bg-white px-4 py-3">
      <div class="text-xs text-gray-500">{$_('capabilities.format_count')}</div>
      <div class="mt-1 text-2xl font-semibold text-gray-900">{formatEntries.length}</div>
    </div>
    <div class="rounded-xl border border-gray-200 bg-white px-4 py-3">
      <div class="text-xs text-gray-500">{$_('capabilities.biz_coverage')}</div>
      <div class="mt-1 text-sm text-gray-700">
        {renderList(
          Array.from(
            new Set(capabilities.flatMap((item) => item.supported_business_lines ?? []))
          )
        )}
      </div>
    </div>
  </div>

  <div class="grid gap-4 xl:grid-cols-[minmax(0,2fr)_minmax(320px,1fr)]">
    <div class="rounded-xl border border-gray-200 bg-white overflow-hidden">
      <div class="border-b border-gray-100 px-4 py-3">
        <h4 class="text-sm font-medium text-gray-900">{$_('capabilities.detail')}</h4>
      </div>
      <div class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead>
            <tr class="bg-gray-50 text-xs text-gray-500">
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_component')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_kind')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_biz_line')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_capability')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_wp_source')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_standard')}</th>
              <th class="px-4 py-2.5 text-left font-medium">{$_('capabilities.th_limits')}</th>
            </tr>
          </thead>
          <tbody>
            {#if loading}
              <tr><td colspan="7" class="px-4 py-8 text-center text-gray-400">{$_('common.loading')}</td></tr>
            {:else if capabilities.length === 0}
              <tr><td colspan="7" class="px-4 py-8 text-center text-gray-400">{$_('capabilities.no_data')}</td></tr>
            {:else}
              {#each capabilities as item (item.id)}
                <tr class="border-t border-gray-50 align-top hover:bg-gray-50/60">
                  <td class="px-4 py-3">
                    <div class="font-medium text-gray-800">{item.name}</div>
                    <div class="font-mono text-xs text-gray-400 mt-1">{item.id}</div>
                  </td>
                  <td class="px-4 py-3 text-xs text-gray-600">{item.kind}</td>
                  <td class="px-4 py-3 text-xs text-gray-600">{renderList(item.supported_business_lines)}</td>
                  <td class="px-4 py-3 text-xs text-gray-600">
                    <div>{renderContentCapabilitySummary(item.supported_content_formats, item.kind)}</div>
                    <div class="mt-1 flex flex-wrap gap-1">
                      {#each item.supported_content_formats ?? [] as fmt}
                        <span class="text-[10px] px-1.5 py-0.5 rounded bg-blue-50 text-blue-600">{formatCapabilityLabel(fmt)}</span>
                      {/each}
                    </div>
                    <div class="mt-1 text-[11px] text-gray-400">{renderHtmlSupportSummary(item.supported_content_formats)}</div>
                  </td>
                  <td class="px-4 py-3 text-xs text-gray-600">
                    <div class="flex flex-wrap gap-1">
                      {#each renderWpHints(item) as hint}
                        <span class="text-[10px] px-1.5 py-0.5 rounded bg-emerald-50 text-emerald-700">{hint}</span>
                      {/each}
                    </div>
                  </td>
                  <td class="px-4 py-3 text-xs text-gray-600">
                    <div>{renderContractSummary(item)}</div>
                    <div class="mt-1 text-[11px] text-gray-400">{renderContractStages(item)}</div>
                  </td>
                  <td class="px-4 py-3 text-xs text-gray-600">
                    {item.constraints?.max_file_size_mb ? `${item.constraints.max_file_size_mb} MB` : $_('capabilities.no_file_limit')}
                  </td>
                </tr>
              {/each}
            {/if}
          </tbody>
        </table>
      </div>
    </div>

    <div class="rounded-xl border border-gray-200 bg-white overflow-hidden">
      <div class="border-b border-gray-100 px-4 py-3">
        <h4 class="text-sm font-medium text-gray-900">{$_('capabilities.format_map')}</h4>
      </div>
      <div class="divide-y divide-gray-100">
        {#if loading}
          <div class="px-4 py-8 text-center text-sm text-gray-400">{$_('common.loading')}</div>
        {:else if formatEntries.length === 0}
          <div class="px-4 py-8 text-center text-sm text-gray-400">{$_('capabilities.no_map_data')}</div>
        {:else}
          {#each formatEntries as [format, componentIds]}
            <div class="px-4 py-3">
              <div class="text-xs font-medium text-gray-700">{format}</div>
              <div class="mt-1 text-xs text-gray-500 break-all">{componentIds.join(', ')}</div>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  </div>
</div>
