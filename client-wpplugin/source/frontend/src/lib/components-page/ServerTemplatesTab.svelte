<script lang="ts">
  import { onMount } from 'svelte';
  import { locale } from 'svelte-i18n';
  import { installServerTemplateToLocal, loadComponentTemplate, searchServerComponents } from '../api/components';
  import { status } from '../stores/status';
  import { showToast } from '../stores/toast';
  import {
    deriveWpSourceHints,
    deriveFileSizeClass,
    extractArtifactHints,
    formatCapabilityLabel,
    formatArtifactKindLabel,
    extractAuthModes,
    extractTranslationModes,
    fileSizeClassBadgeClass,
    fileSizeLabel,
    handleBackdropKeydown,
    renderArtifactSummary,
    renderContentCapabilitySummary,
    renderHtmlSupportSummary,
  } from './helpers';
  import type {
    ServerComp,
    ServerTemplateModalData,
  } from './types';
  import { _ } from 'svelte-i18n';

  let serverComps = $state<ServerComp[]>([]);
  let serverCompsLoading = $state(false);
  let serverSearchQ = $state('');
  let serverProductFilter = $state('');
  let serverKindFilter = $state('');
  let serverGroupFilter = $state('');
  let serverFamilyFilter = $state('');
  let serverSubfamilyFilter = $state('');
  let serverContentFormatFilter = $state('');
  let serverSizeClassFilter = $state('');
  let serverBusinessLineFilter = $state('');
  let serverPage = $state(1);
  let serverTotalPages = $state(1);
  let serverTotal = $state(0);

  let serverAvailableKinds = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const kinds = new Set(all.map((c) => c.kind));
    return [...kinds].sort();
  })());
  let serverAvailableGroups = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const groups = new Set(
      all.map((c) => (c.template_group || '').trim()).filter(Boolean)
    );
    return [...groups].sort();
  })());
  let serverAvailableProducts = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const products = new Set(
      all.map((c) => (c.product_id || '').trim()).filter(Boolean)
    );
    return [...products].sort();
  })());
  let serverAvailableFamilies = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const families = new Set(
      all.map((c) => (c.catalog_family || '').trim()).filter(Boolean)
    );
    return [...families].sort();
  })());
  let serverAvailableSubfamilies = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const subfamilies = new Set(
      all.map((c) => (c.catalog_subfamily || '').trim()).filter(Boolean)
    );
    return [...subfamilies].sort();
  })());
  let serverAvailableContentFormats = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const fmts = new Set<string>();
    for (const c of all) {
      for (const fmt of c.supported_content_formats ?? []) fmts.add(fmt);
    }
    return [...fmts].sort();
  })());
  let serverAvailableSizeClasses = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const classes = new Set(
      all.map((c) => (c.size_class || '').trim()).filter(Boolean)
    );
    const order = ['S', 'M', 'L', 'XL', 'UNLIMITED'];
    return [...classes].sort((a, b) => {
      const ai = order.indexOf(a);
      const bi = order.indexOf(b);
      if (ai < 0 && bi < 0) return a.localeCompare(b);
      if (ai < 0) return 1;
      if (bi < 0) return -1;
      return ai - bi;
    });
  })());
  let serverAvailableBusinessLines = $derived((() => {
    const all = ($status?.components ?? []) as ServerComp[];
    const lines = new Set<string>();
    for (const c of all) {
      for (const line of c.supported_business_lines ?? []) lines.add(line);
    }
    return [...lines].sort();
  })());

  let showTemplateModal = $state(false);
  let templateModalData = $state<ServerTemplateModalData | null>(null);
  let templateModalLoading = $state(false);

  async function loadServerComps() {
    serverCompsLoading = true;
    try {
        const r = await searchServerComponents({
          page: serverPage,
          per_page: 20,
          q: serverSearchQ.trim(),
          product_id: serverProductFilter || undefined,
          kind: serverKindFilter || undefined,
          group: serverGroupFilter || undefined,
          family: serverFamilyFilter || undefined,
          subfamily: serverSubfamilyFilter || undefined,
          content_format: serverContentFormatFilter || undefined,
          size_class: serverSizeClassFilter || undefined,
          business_line: serverBusinessLineFilter || undefined,
        locale: $locale ?? 'en',
      });
      if (r.success) {
        serverComps = r.data.items as unknown as ServerComp[];
        serverTotalPages = r.data.total_pages ?? 1;
        serverTotal = r.data.total ?? serverComps.length;
      } else {
        showToast('error', $_('server_templates.search_failed'), r.error?.message ?? $_('server_templates.cache_only'));
        serverComps = [];
        serverTotal = 0;
        serverTotalPages = 1;
      }
    } catch {
      showToast('error', $_('server_templates.search_failed'), $_('server_templates.cache_only'));
      serverComps = [];
      serverTotal = 0;
      serverTotalPages = 1;
    } finally {
      serverCompsLoading = false;
    }
  }

  let serverSearchTimer: ReturnType<typeof setTimeout> | null = null;
  function onServerSearchInput() {
    serverPage = 1;
    if (serverSearchTimer) clearTimeout(serverSearchTimer);
    serverSearchTimer = setTimeout(() => loadServerComps(), 300);
  }

  function onServerFilterChange() {
    serverPage = 1;
    loadServerComps();
  }

  async function viewServerTemplate(comp: ServerComp) {
    templateModalLoading = true;
    templateModalData = null;
    showTemplateModal = true;
    try {
      const r = await loadComponentTemplate(comp.id);
      if (r.success && r.data) {
        const artifactHints = extractArtifactHints(r.data.template_json);
        templateModalData = {
          name: r.data.template_name,
          version: r.data.template_version,
          product_id: comp.product_id,
          catalog_family: comp.catalog_family,
          catalog_subfamily: comp.catalog_subfamily,
          capability_tags: comp.capability_tags,
          runtime_tags: comp.runtime_tags,
          auth_fields: r.data.auth_fields,
          template_json: r.data.template_json,
          auth_modes: extractAuthModes(r.data.template_json, r.data.auth_modes),
          signing_algorithm: comp.signing_algorithm,
          supported_content_formats: comp.supported_content_formats,
          supported_formats: comp.supported_formats,
          max_file_size_mb: comp.max_file_size_mb ?? undefined,
          api_docs_url: comp.api_docs_url,
          translation_modes: extractTranslationModes(r.data.template_json),
          client_contract: comp.client_contract,
          input_artifact_kind: artifactHints.input_artifact_kind,
          output_artifact_kinds: artifactHints.output_artifact_kinds,
        };
      } else {
        showToast('error', $_('server_templates.load_template_failed'), r.error?.message);
        // Keep modal open so user can dismiss; do not auto-close.
      }
    } catch {
      showToast('error', $_('server_templates.load_template_failed'));
      // Keep modal open so user can dismiss; do not auto-close.
    } finally {
      templateModalLoading = false;
    }
  }

  let installingId = $state('');
  async function installToLocal(comp: ServerComp) {
    installingId = comp.id;
    try {
      const r = await installServerTemplateToLocal({ template_id: comp.id, name: comp.name });
      if (r.success) {
        showToast('success', $_('server_templates.installed', { values: { id: r.data.id } }) + (r.data.overwrite ? ' - ' + $_('server_templates.overwritten') : ''));
      } else {
        showToast('error', $_('server_templates.install_failed'), (r as any).error?.message);
      }
    } catch {
      showToast('error', $_('server_templates.install_failed'));
    } finally {
      installingId = '';
    }
  }

  onMount(() => {
    loadServerComps();
  });
</script>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 space-y-2">
    <input
      bind:value={serverSearchQ}
      oninput={onServerSearchInput}
      placeholder={$_('server_templates.search_placeholder')}
      class="w-full border border-gray-200 rounded-lg px-3 py-1.5 text-sm" />
    <div class="flex flex-wrap gap-2 items-center">
      <select bind:value={serverProductFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_products')}</option>
        {#each serverAvailableProducts as productId}
          <option value={productId}>{productId}</option>
        {/each}
      </select>
      <select bind:value={serverKindFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_kinds', { values: { count: serverTotal } })}</option>
        {#each serverAvailableKinds as k}
          <option value={k}>{k}</option>
        {/each}
      </select>
      <select bind:value={serverGroupFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_groups')}</option>
        {#each serverAvailableGroups as g}
          <option value={g}>{g}</option>
        {/each}
      </select>
      <select bind:value={serverFamilyFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_families')}</option>
        {#each serverAvailableFamilies as family}
          <option value={family}>{family}</option>
        {/each}
      </select>
      <select bind:value={serverSubfamilyFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_subfamilies')}</option>
        {#each serverAvailableSubfamilies as subfamily}
          <option value={subfamily}>{subfamily}</option>
        {/each}
      </select>
      <select bind:value={serverContentFormatFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_formats')}</option>
        {#each serverAvailableContentFormats as fmt}
          <option value={fmt}>{fmt}</option>
        {/each}
      </select>
      <select bind:value={serverSizeClassFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_sizes')}</option>
        {#each serverAvailableSizeClasses as sz}
          <option value={sz}>{sz}</option>
        {/each}
      </select>
      <select bind:value={serverBusinessLineFilter} onchange={onServerFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('server_templates.filter_all_biz')}</option>
        {#each serverAvailableBusinessLines as bl}
          <option value={bl}>{bl}</option>
        {/each}
      </select>
    </div>
  </div>
  {#if serverCompsLoading}
    <div class="py-8 text-center text-gray-400 text-sm">{$_('common.loading')}</div>
  {:else}
    <table class="w-full text-sm">
      <thead>
        <tr class="text-xs text-gray-500 bg-gray-50">
          <th class="px-4 py-2.5 text-left font-medium">ID</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('server_templates.col_name')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('server_templates.col_type')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('server_templates.col_signing')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('server_templates.col_version')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('server_templates.col_formats')}</th>
          <th class="px-4 py-2.5 text-right font-medium">{$_('server_templates.col_actions')}</th>
        </tr>
      </thead>
      <tbody>
        {#if serverComps.length === 0}
          <tr>
            <td colspan="7" class="px-4 py-8 text-center text-gray-400">
              {serverSearchQ || serverProductFilter || serverKindFilter || serverGroupFilter || serverFamilyFilter || serverSubfamilyFilter || serverContentFormatFilter || serverSizeClassFilter || serverBusinessLineFilter
                ? $_('server_templates.no_match')
                : $_('server_templates.no_templates')}
            </td>
          </tr>
        {:else}
          {#each serverComps as c}
            <tr class="border-t border-gray-50 hover:bg-gray-50/50">
              <td class="px-4 py-3 font-mono text-xs text-gray-600">{c.id}</td>
              <td class="px-4 py-3">
                <div class="text-sm text-gray-900">
                  {c.name}
                  {#if c.api_docs_url}
                    <a href={c.api_docs_url} target="_blank" rel="noopener noreferrer" class="inline-flex items-center ml-1 text-blue-500 hover:text-blue-700" title={c.api_docs_url}>
                      <svg class="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M10 6H6a2 2 0 00-2 2v10a2 2 0 002 2h10a2 2 0 002-2v-4M14 4h6m0 0v6m0-6L10 14"/></svg>
                    </a>
                  {/if}
                  {#if c.template_group}
                    <span class="ml-2 text-[10px] px-1.5 py-0.5 rounded bg-indigo-50 text-indigo-600">{c.template_group}</span>
                  {/if}
                </div>
                <div class="mt-1 flex flex-wrap gap-1">
                  {#if c.product_id}
                    <span class="text-[10px] px-1.5 py-0.5 rounded bg-slate-100 text-slate-700">{c.product_id}</span>
                  {/if}
                  {#if c.catalog_family}
                    <span class="text-[10px] px-1.5 py-0.5 rounded bg-cyan-50 text-cyan-700">{c.catalog_family}</span>
                  {/if}
                  {#if c.catalog_subfamily}
                    <span class="text-[10px] px-1.5 py-0.5 rounded bg-teal-50 text-teal-700">{c.catalog_subfamily}</span>
                  {/if}
                </div>
                {#if c.description}
                  <div class="mt-1 text-xs text-gray-500 max-w-xl">{c.description}</div>
                {/if}
              </td>
              <td class="px-4 py-3 text-xs text-gray-500">{c.kind}</td>
              <td class="px-4 py-3 text-xs text-gray-500 font-mono">{c.signing_algorithm || 'none'}</td>
              <td class="px-4 py-3 text-xs text-gray-500">{c.version}</td>
              <td class="px-4 py-3">
                <div class="flex flex-wrap gap-1">
                  {#each c.supported_content_formats ?? [] as fmt}
                    <span class="text-[10px] px-1.5 py-0.5 bg-blue-50 text-blue-600 rounded">{fmt}</span>
                  {/each}
                  {#each c.supported_formats ?? [] as fmt}
                    <span class="text-[10px] px-1.5 py-0.5 bg-green-50 text-green-600 rounded">.{fmt}</span>
                  {/each}
                  {#each c.translation_modes ?? [] as mode}
                    <span class="text-[10px] px-1.5 py-0.5 bg-indigo-50 text-indigo-600 rounded">mode:{mode.id}</span>
                  {/each}
                  {#each c.auth_modes ?? [] as authMode}
                    <span class="text-[10px] px-1.5 py-0.5 bg-purple-50 text-purple-600 rounded">auth:{authMode}</span>
                  {/each}
                  {#each c.capability_tags ?? [] as tag}
                    <span class="text-[10px] px-1.5 py-0.5 bg-sky-50 text-sky-700 rounded">tag:{tag}</span>
                  {/each}
                  {#each c.runtime_tags ?? [] as tag}
                    <span class="text-[10px] px-1.5 py-0.5 bg-orange-50 text-orange-700 rounded">rt:{tag}</span>
                  {/each}
                  {#if c.max_file_size_mb !== null && c.max_file_size_mb !== undefined}
                    <span class="text-[10px] px-1.5 py-0.5 bg-amber-50 text-amber-600 rounded">{fileSizeLabel(c.max_file_size_mb)}</span>
                    {#if deriveFileSizeClass(c.max_file_size_mb)}
                      <span class="text-[10px] px-1.5 py-0.5 rounded font-semibold {fileSizeClassBadgeClass(deriveFileSizeClass(c.max_file_size_mb)!)}">{deriveFileSizeClass(c.max_file_size_mb)}</span>
                    {/if}
                  {/if}
                </div>
              </td>
              <td class="px-4 py-3 text-right">
                <div class="flex gap-2 justify-end">
                  <button onclick={() => installToLocal(c)} disabled={installingId === c.id} class="text-xs text-emerald-600 hover:text-emerald-700 disabled:opacity-50">
                    {installingId === c.id ? $_('server_templates.installing') : $_('server_templates.install')}
                  </button>
                  <button onclick={() => viewServerTemplate(c)} class="text-xs text-blue-600 hover:text-blue-700">
                    {$_('server_templates.view')}
                  </button>
                </div>
              </td>
            </tr>
          {/each}
        {/if}
      </tbody>
    </table>
  {/if}
</div>

{#if serverTotalPages > 1}
  <div class="flex items-center justify-between px-4 py-3 mt-2">
    <span class="text-xs text-gray-500">{$_('server_templates.pagination', { values: { total: serverTotal, page: serverPage, pages: serverTotalPages } })}</span>
    <div class="flex gap-1">
      <button
        onclick={() => {
          serverPage = Math.max(1, serverPage - 1);
          loadServerComps();
        }}
        disabled={serverPage <= 1}
        class="px-3 py-1 text-xs border border-gray-200 rounded-lg hover:bg-gray-50 disabled:opacity-30 disabled:cursor-not-allowed">
        {$_('common.prev_page')}
      </button>
      <button
        onclick={() => {
          serverPage = Math.min(serverTotalPages, serverPage + 1);
          loadServerComps();
        }}
        disabled={serverPage >= serverTotalPages}
        class="px-3 py-1 text-xs border border-gray-200 rounded-lg hover:bg-gray-50 disabled:opacity-30 disabled:cursor-not-allowed">
        {$_('common.next_page')}
      </button>
    </div>
  </div>
{/if}

{#if showTemplateModal}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('server_templates.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget) showTemplateModal = false;
    }}
    onkeydown={(e) => handleBackdropKeydown(e, () => (showTemplateModal = false))}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-2xl max-h-[80vh] flex flex-col">
      <div class="flex items-start justify-between mb-4">
        <div>
          {#if !templateModalLoading && templateModalData}
            <h4 class="font-semibold text-gray-900">{templateModalData.name}</h4>
            <span class="text-xs text-gray-400 font-mono">v{templateModalData.version}</span>
          {/if}
        </div>
        <button onclick={() => (showTemplateModal = false)} class="text-gray-400 hover:text-gray-600 text-lg leading-none" aria-label={$_('common.close')}>
          {$_('common.close')}
        </button>
      </div>
      <pre data-testid="server-template-json" class="bg-gray-50 border border-gray-200 rounded-lg p-4 text-xs font-mono text-gray-700 whitespace-pre-wrap break-all min-h-[3rem]">{#if templateModalLoading}{$_('common.loading')}{:else}{JSON.stringify(templateModalData?.template_json ?? "(no template_json)", null, 2)}{/if}</pre>
      {#if !templateModalLoading && templateModalData}
        {#if templateModalData.product_id || templateModalData.catalog_family || templateModalData.catalog_subfamily || templateModalData.capability_tags?.length || templateModalData.runtime_tags?.length}
          <div class="mb-4 rounded-lg border border-gray-200 bg-gray-50 px-4 py-3">
            <div class="text-xs font-medium text-gray-600 mb-2">{$_('server_templates.taxonomy')}</div>
            <div class="grid gap-x-4 gap-y-3 md:grid-cols-2 text-xs">
              {#if templateModalData.product_id}
                <div>
                  <div class="text-gray-500 mb-1">{$_('server_templates.product_id')}</div>
                  <div class="text-gray-900 font-mono">{templateModalData.product_id}</div>
                </div>
              {/if}
              {#if templateModalData.catalog_family}
                <div>
                  <div class="text-gray-500 mb-1">{$_('server_templates.catalog_family')}</div>
                  <div class="text-gray-900">{templateModalData.catalog_family}</div>
                </div>
              {/if}
              {#if templateModalData.catalog_subfamily}
                <div>
                  <div class="text-gray-500 mb-1">{$_('server_templates.catalog_subfamily')}</div>
                  <div class="text-gray-900">{templateModalData.catalog_subfamily}</div>
                </div>
              {/if}
              {#if templateModalData.capability_tags?.length}
                <div class="md:col-span-2">
                  <div class="text-gray-500 mb-1">{$_('server_templates.capability_tags')}</div>
                  <div class="flex flex-wrap gap-1">
                    {#each templateModalData.capability_tags as tag}
                      <span class="text-[10px] px-1.5 py-0.5 bg-sky-50 text-sky-700 rounded">{tag}</span>
                    {/each}
                  </div>
                </div>
              {/if}
              {#if templateModalData.runtime_tags?.length}
                <div class="md:col-span-2">
                  <div class="text-gray-500 mb-1">{$_('server_templates.runtime_tags')}</div>
                  <div class="flex flex-wrap gap-1">
                    {#each templateModalData.runtime_tags as tag}
                      <span class="text-[10px] px-1.5 py-0.5 bg-orange-50 text-orange-700 rounded">{tag}</span>
                    {/each}
                  </div>
                </div>
              {/if}
            </div>
          </div>
        {/if}
        {#if templateModalData.signing_algorithm || templateModalData.supported_formats?.length || templateModalData.max_file_size_mb !== undefined || templateModalData.api_docs_url}
          <div class="mb-4 grid grid-cols-2 gap-x-4 gap-y-2 text-xs">
            {#if templateModalData.signing_algorithm && templateModalData.signing_algorithm !== 'none'}
              <div>
                <span class="text-gray-500">{$_('server_templates.signing_algo')}</span>
                <span class="ml-2 font-mono text-gray-900">{templateModalData.signing_algorithm}</span>
              </div>
            {/if}
            {#if templateModalData.supported_formats?.length}
              <div>
                <span class="text-gray-500">{$_('server_templates.file_formats')}</span>
                <span class="ml-2">
                  {#each templateModalData.supported_formats as fmt}
                    <span class="px-1.5 py-0.5 bg-green-50 text-green-600 rounded mr-1">.{fmt}</span>
                  {/each}
                </span>
              </div>
            {/if}
            {#if templateModalData.max_file_size_mb !== undefined}
              <div>
                <span class="text-gray-500">{$_('server_templates.file_size_limit')}</span>
                <span class="ml-2 text-gray-900">{fileSizeLabel(templateModalData.max_file_size_mb)}</span>
              </div>
              <div>
                <span class="text-gray-500">{$_('server_templates.size_class')}</span>
                {#if deriveFileSizeClass(templateModalData.max_file_size_mb)}
                  <span class="ml-2 px-1.5 py-0.5 rounded text-[10px] font-semibold {fileSizeClassBadgeClass(deriveFileSizeClass(templateModalData.max_file_size_mb)!)}">{deriveFileSizeClass(templateModalData.max_file_size_mb)}</span>
                {/if}
              </div>
            {/if}
            {#if templateModalData.api_docs_url}
              <div>
                <span class="text-gray-500">{$_('server_templates.api_docs')}</span>
                <a href={templateModalData.api_docs_url} target="_blank" rel="noopener noreferrer" class="ml-2 text-blue-600 hover:text-blue-700 underline">
                  {templateModalData.api_docs_url}
                </a>
              </div>
            {/if}
          </div>
        {/if}
        {#if templateModalData.supported_content_formats?.length || templateModalData.client_contract || templateModalData.input_artifact_kind || templateModalData.output_artifact_kinds?.length}
          <div class="mb-4 rounded-lg border border-slate-200 bg-slate-50 px-4 py-3">
            <div class="text-xs font-medium text-slate-700 mb-2">{$_('server_templates.standard_compat')}</div>
            <div class="grid gap-x-4 gap-y-3 md:grid-cols-2 text-xs">
              <div>
                <div class="text-slate-500 mb-1">{$_('server_templates.content_cap')}</div>
                <div class="text-slate-900">
                  {renderContentCapabilitySummary(templateModalData.supported_content_formats, templateModalData.client_contract?.task_kind)}
                </div>
                {#if templateModalData.supported_content_formats?.length}
                  <div class="mt-1 flex flex-wrap gap-1">
                    {#each templateModalData.supported_content_formats as fmt}
                      <span class="text-[10px] px-1.5 py-0.5 bg-blue-50 text-blue-600 rounded">{formatCapabilityLabel(fmt)}</span>
                    {/each}
                  </div>
                {/if}
              </div>
              <div>
                <div class="text-slate-500 mb-1">{$_('server_templates.html_support')}</div>
                <div class="text-slate-900">{renderHtmlSupportSummary(templateModalData.supported_content_formats)}</div>
              </div>
              <div>
                <div class="text-slate-500 mb-1">{$_('server_templates.artifact_semantic')}</div>
                <div class="text-slate-900">
                  {renderArtifactSummary(templateModalData.input_artifact_kind, templateModalData.output_artifact_kinds)}
                </div>
                {#if templateModalData.input_artifact_kind || templateModalData.output_artifact_kinds?.length}
                  <div class="mt-1 flex flex-wrap gap-1">
                    {#if templateModalData.input_artifact_kind}
                      <span class="text-[10px] px-1.5 py-0.5 bg-amber-50 text-amber-700 rounded">in:{formatArtifactKindLabel(templateModalData.input_artifact_kind)}</span>
                    {/if}
                    {#each templateModalData.output_artifact_kinds ?? [] as kind}
                      <span class="text-[10px] px-1.5 py-0.5 bg-rose-50 text-rose-700 rounded">out:{formatArtifactKindLabel(kind)}</span>
                    {/each}
                  </div>
                {/if}
              </div>
              <div class="md:col-span-2">
                <div class="text-slate-500 mb-1">{$_('server_templates.wp_source_hint')}</div>
                <div class="flex flex-wrap gap-1">
                  {#each deriveWpSourceHints(templateModalData.supported_content_formats, templateModalData.client_contract?.task_kind) as hint}
                    <span class="text-[10px] px-1.5 py-0.5 bg-emerald-50 text-emerald-700 rounded">{hint}</span>
                  {/each}
                </div>
              </div>
              {#if templateModalData.client_contract}
                <div>
                  <div class="text-slate-500 mb-1">{$_('server_templates.exec_summary')}</div>
                  <div class="text-slate-900">
                    {templateModalData.client_contract.input_mode} -> {templateModalData.client_contract.workflow_mode} -> {templateModalData.client_contract.output_mode}
                  </div>
                </div>
                <div>
                  <div class="text-slate-500 mb-1">{$_('server_templates.exec_stages')}</div>
                  <div class="text-slate-900">{templateModalData.client_contract.stages?.join(' -> ') || 'request'}</div>
                </div>
              {/if}
            </div>
          </div>
        {/if}
        {#if templateModalData.auth_modes && templateModalData.auth_modes.length > 0}
          <div class="mb-4">
            <div class="text-xs font-medium text-gray-600 mb-2">{$_('server_templates.auth_modes')}</div>
            <div class="flex flex-wrap gap-1.5">
              {#each templateModalData.auth_modes as mode}
                <span class="text-[10px] px-1.5 py-0.5 rounded {mode === 'key' ? 'bg-blue-50 text-blue-700' : mode === 'oauth' ? 'bg-purple-50 text-purple-700' : 'bg-emerald-50 text-emerald-700'}">{mode}</span>
              {/each}
            </div>
          </div>
        {/if}
        {#if templateModalData.auth_fields && templateModalData.auth_fields.length > 0}
          <div class="mb-4">
            <div class="text-xs font-medium text-gray-600 mb-2">{$_('server_templates.auth_fields')}</div>
            <div class="flex flex-wrap gap-2">
              {#each templateModalData.auth_fields as f}
                <span class="text-xs px-2 py-1 rounded-full border {f.required ? 'bg-orange-50 border-orange-200 text-orange-700' : 'bg-gray-50 border-gray-200 text-gray-500'}">
                  {f.name}{f.required ? ' *' : ''}
                </span>
              {/each}
            </div>
          </div>
        {/if}
        {#if templateModalData.translation_modes && templateModalData.translation_modes.length > 0}
          <div class="mb-4">
            <div class="text-xs font-medium text-gray-600 mb-2">{$_('server_templates.translation_modes')}</div>
            <div class="space-y-2">
              {#each templateModalData.translation_modes as mode}
                <div class="border border-gray-200 rounded-lg px-3 py-2">
                  <div class="flex items-center gap-2 text-xs">
                    <span class="font-mono text-gray-700">{mode.id}</span>
                    <span class="text-gray-900">{mode.label}</span>
                    {#if mode.api_docs_url}
                      <a href={mode.api_docs_url} target="_blank" rel="noopener noreferrer" class="text-blue-600 hover:text-blue-700 underline">
                        {$_('server_templates.docs_link')}
                      </a>
                    {/if}
                  </div>
                  {#if mode.supported_content_formats.length > 0}
                    <div class="mt-1 flex flex-wrap gap-1">
                      {#each mode.supported_content_formats as fmt}
                        <span class="text-[10px] px-1.5 py-0.5 bg-blue-50 text-blue-600 rounded">{fmt}</span>
                      {/each}
                    </div>
                  {/if}
                </div>
              {/each}
            </div>
          </div>
        {/if}
        <div class="text-xs font-medium text-gray-600 mb-2">{$_('server_templates.template_json')}</div>
        <div class="flex-1 overflow-auto min-h-0">
        </div>
        <div class="mt-4 flex justify-end">
          <button onclick={() => (showTemplateModal = false)} class="px-4 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50">
            {$_('common.close')}
          </button>
        </div>
      {/if}
    </div>
  </div>
{/if}
