<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { onMount } from 'svelte';
  import {
    createVendorKey,
    installCatalogTemplate,
    listProviderCatalog,
    refreshProviderCatalog,
    type ProviderCatalogItem,
  } from '../api/keys';
  import {
    createComponentVersion,
    quickTestLocalComponent,
    updateLocalComponent,
    upsertRuleBinding,
  } from '../api/components';

  type WizardStep = 'install' | 'key' | 'test' | 'enable' | 'route' | 'done';

  const WIZARD_STEPS: WizardStep[] = ['install', 'key', 'test', 'enable', 'route', 'done'];

  let items = $state<ProviderCatalogItem[]>([]);
  let query = $state('');
  let familyFilter = $state('');
  let tierFilter = $state('');
  let loading = $state(false);
  let refreshing = $state(false);
  let error = $state('');
  let notice = $state('');
  let catalogVersion = $state<string | null>(null);

  let wizardOpen = $state(false);
  let wizardItem = $state<ProviderCatalogItem | null>(null);
  let wizardStep = $state<WizardStep>('install');
  let wizardBusy = $state(false);
  let wizardError = $state('');
  let wizardHint = $state('');
  let wizardNotice = $state('');
  let localComponentId = $state('');
  let keyId = $state('');
  let authFieldValues = $state<Record<string, string>>({});
  let configUrl = $state('');
  let configModel = $state('');
  let configResponsePath = $state('');
  let quickTestText = $state('Hello world');
  let quickTestResult = $state('');
  let routeSlot = $state('plain_text');

  let filteredItems = $derived(
    items.filter((item) => {
      if (familyFilter && item.family !== familyFilter) return false;
      const tier = item.evidence_tier || (item.template as { evidence_tier?: string })?.evidence_tier || '';
      if (tierFilter && tier !== tierFilter) return false;
      return true;
    }),
  );

  function evidenceTier(item: ProviderCatalogItem): string {
    return (
      item.evidence_tier ||
      (item.template as { evidence_tier?: string })?.evidence_tier ||
      'schema-only'
    );
  }

  function tierBadgeClass(tier: string): string {
    if (tier === 'mock-verified') return 'bg-green-100 text-green-800';
    if (tier === 'live-verified') return 'bg-blue-100 text-blue-800';
    return 'bg-amber-100 text-amber-800';
  }

  function isOpenaiFamily(item: ProviderCatalogItem | null): boolean {
    return (item?.family || '') === 'openai_compatible';
  }

  function isCustomHttp(item: ProviderCatalogItem | null): boolean {
    return (item?.vendor_id || '') === 'custom_http_mt' || (item?.entry_id || '') === 'custom-http-mt';
  }

  function templateRequestUrl(item: ProviderCatalogItem): string {
    const req = (item.template as { request?: { url?: string; body?: { model?: string } } })?.request;
    return typeof req?.url === 'string' ? req.url : '';
  }

  function templateModel(item: ProviderCatalogItem): string {
    const body = (item.template as { request?: { body?: { model?: string } } })?.request?.body;
    return typeof body?.model === 'string' ? body.model : 'gpt-4o-mini';
  }

  function templateResponsePath(item: ProviderCatalogItem): string {
    const path = (item.template as { response?: { translated_text_path?: string } })?.response
      ?.translated_text_path;
    return typeof path === 'string' ? path : '';
  }

  async function loadCatalog() {
    loading = true;
    error = '';
    try {
      const res = await listProviderCatalog({ q: query });
      if (res.success) {
        items = res.data.items;
        catalogVersion = res.data.catalog_version;
      } else {
        error = res.error?.message ?? $_('vendor_catalog.load_failed');
      }
    } catch (err: any) {
      error = err?.message ?? $_('vendor_catalog.load_failed');
    } finally {
      loading = false;
    }
  }

  async function refreshCatalog() {
    refreshing = true;
    notice = '';
    error = '';
    try {
      const refreshed = await refreshProviderCatalog();
      if (!refreshed.success) {
        error = refreshed.error?.message ?? $_('vendor_catalog.load_failed');
        return;
      }
      notice = $_('vendor_catalog.refresh_local_success');
      await loadCatalog();
    } catch (err: any) {
      error = err?.message ?? $_('vendor_catalog.load_failed');
    } finally {
      refreshing = false;
    }
  }

  function localId(item: ProviderCatalogItem) {
    return `${item.vendor_id || 'provider'}-${item.template_id}`
      .toLowerCase()
      .replace(/[^a-z0-9_-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 120);
  }

  function authFieldNames(item: ProviderCatalogItem): string[] {
    const auth = (item.template as { auth?: { fields?: Array<{ name?: string }> } })?.auth;
    const fields = Array.isArray(auth?.fields) ? auth.fields : [];
    const names = fields
      .map((f) => (typeof f?.name === 'string' ? f.name.trim() : ''))
      .filter(Boolean);
    return names.length > 0 ? names : ['api_key'];
  }

  function openWizard(item: ProviderCatalogItem) {
    wizardItem = item;
    wizardStep = 'install';
    wizardError = '';
    wizardHint = '';
    wizardNotice = '';
    localComponentId = localId(item);
    keyId = `${localId(item)}-key`.slice(0, 120);
    const fields: Record<string, string> = {};
    for (const name of authFieldNames(item)) fields[name] = '';
    authFieldValues = fields;
    configUrl = templateRequestUrl(item);
    configModel = templateModel(item);
    configResponsePath = templateResponsePath(item);
    quickTestText = 'Hello world';
    quickTestResult = '';
    routeSlot = item.supported_content_formats?.[0] || 'plain_text';
    wizardOpen = true;
  }

  function closeWizard() {
    if (wizardBusy) return;
    wizardOpen = false;
    wizardItem = null;
    wizardError = '';
    wizardHint = '';
    wizardNotice = '';
  }

  function primaryAuthValue(): string {
    const names = wizardItem ? authFieldNames(wizardItem) : ['api_key'];
    for (const name of names) {
      const v = (authFieldValues[name] || '').trim();
      if (v) return v;
    }
    return '';
  }

  function buildAuthValues(): Record<string, string> {
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(authFieldValues)) {
      if (v.trim()) out[k] = v.trim();
    }
    return out;
  }

  function buildConfigOverrides(): Record<string, string> {
    const out: Record<string, string> = {};
    if (configUrl.trim()) out['request.url'] = configUrl.trim();
    if (isOpenaiFamily(wizardItem) && configModel.trim()) {
      out['request.body.model'] = configModel.trim();
    }
    if (configResponsePath.trim()) {
      out['response.translated_text_path'] = configResponsePath.trim();
    }
    return out;
  }

  function stepIndex(step: WizardStep) {
    return WIZARD_STEPS.indexOf(step);
  }

  async function runInstall() {
    if (!wizardItem) return;
    wizardBusy = true;
    wizardError = '';
    wizardNotice = '';
    try {
      const res = await installCatalogTemplate({
        entry_id: wizardItem.entry_id,
        local_id: localComponentId,
        name: wizardItem.name,
      });
      if (!res.success) {
        wizardError = res.error?.message ?? $_('vendor_catalog.install_failed');
        return;
      }
      localComponentId = res.data.id || localComponentId;
      wizardNotice = $_('vendor_catalog.wizard_install_ok');
      wizardStep = 'key';
    } catch (err: any) {
      wizardError = err?.message ?? $_('vendor_catalog.install_failed');
    } finally {
      wizardBusy = false;
    }
  }

  async function runBindKey() {
    if (!wizardItem) return;
    const authValues = buildAuthValues();
    if (Object.keys(authValues).length === 0) {
      wizardError = $_('vendor_catalog.wizard_key_required');
      return;
    }
    wizardBusy = true;
    wizardError = '';
    wizardHint = '';
    wizardNotice = '';
    try {
      const keyRes = await createVendorKey({
        id: keyId,
        vendor_id: wizardItem.vendor_id || 'custom',
        label: `${wizardItem.name || wizardItem.template_id} key`,
        auth_values: authValues,
        enabled: true,
      });
      if (!keyRes.success) {
        wizardError = keyRes.error?.message ?? $_('vendor_catalog.wizard_key_failed');
        return;
      }
      const versionRes = await createComponentVersion(localComponentId, {
        version: 'v1',
        key_ids: [keyId],
        auth_type: 'key',
        remarks: 'created by provider setup wizard',
      });
      if (!versionRes.success) {
        wizardNotice = $_('vendor_catalog.wizard_key_ok_version_skip');
      } else {
        wizardNotice = $_('vendor_catalog.wizard_key_ok');
      }
      wizardStep = 'test';
    } catch (err: any) {
      wizardError = err?.message ?? $_('vendor_catalog.wizard_key_failed');
    } finally {
      wizardBusy = false;
    }
  }

  async function runQuickTest() {
    if (Object.keys(buildAuthValues()).length === 0) {
      wizardError = $_('vendor_catalog.wizard_key_required');
      return;
    }
    wizardBusy = true;
    wizardError = '';
    wizardHint = '';
    wizardNotice = '';
    quickTestResult = '';
    try {
      const authValues = buildAuthValues();
      const res = await quickTestLocalComponent(localComponentId, {
        api_key: primaryAuthValue(),
        auth_values: authValues,
        config_overrides: buildConfigOverrides(),
        text: quickTestText || 'Hello world',
        source_lang: 'en_US',
        target_lang: 'zh_CN',
      });
      if (!res.success) {
        wizardError = res.error?.message ?? $_('vendor_catalog.wizard_test_failed');
        const errCode = (res.error as { code?: string } | undefined)?.code || '';
        const errMsg = `${wizardError} ${res.error?.message || ''}`;
        wizardHint = (res.error as { hint?: string } | undefined)?.hint
          || ( /SSRF|PROVIDER_URL_BLOCKED|allowlist/i.test(`${errCode} ${errMsg}`)
            ? $_('vendor_catalog.wizard_test_hint_ssrf')
            : $_('vendor_catalog.wizard_test_hint_fix') );
        return;
      }
      if (res.data?.error) {
        wizardError = res.data.error;
        wizardHint = res.data.hint
          || ( /SSRF|PROVIDER_URL_BLOCKED|allowlist/i.test(`${res.data.error} ${res.data.hint || ''}`)
            ? $_('vendor_catalog.wizard_test_hint_ssrf')
            : $_('vendor_catalog.wizard_test_hint_fix') );
        return;
      }
      quickTestResult = res.data?.translated_text || res.data?.translated_ref || $_('vendor_catalog.wizard_test_ok');
      wizardNotice = $_('vendor_catalog.wizard_test_ok');
      wizardHint = res.data?.hint || '';
      wizardStep = 'enable';
    } catch (err: any) {
      wizardError = err?.message ?? $_('vendor_catalog.wizard_test_failed');
    } finally {
      wizardBusy = false;
    }
  }

  async function runEnable() {
    wizardBusy = true;
    wizardError = '';
    wizardNotice = '';
    try {
      const res = await updateLocalComponent(localComponentId, { enabled: true });
      if (!res.success) {
        wizardError = res.error?.message ?? $_('vendor_catalog.wizard_enable_failed');
        return;
      }
      wizardNotice = $_('vendor_catalog.wizard_enable_ok');
      wizardStep = 'route';
    } catch (err: any) {
      wizardError = err?.message ?? $_('vendor_catalog.wizard_enable_failed');
    } finally {
      wizardBusy = false;
    }
  }

  async function runRoute() {
    wizardBusy = true;
    wizardError = '';
    wizardNotice = '';
    try {
      const slot = routeSlot.trim() || 'plain_text';
      const res = await upsertRuleBinding('global', slot, localComponentId);
      if (!res.success) {
        wizardError = res.error?.message ?? $_('vendor_catalog.wizard_route_failed');
        return;
      }
      wizardNotice = $_('vendor_catalog.wizard_route_ok');
      wizardStep = 'done';
      notice = $_('vendor_catalog.wizard_complete');
    } catch (err: any) {
      wizardError = err?.message ?? $_('vendor_catalog.wizard_route_failed');
    } finally {
      wizardBusy = false;
    }
  }

  async function installOnly(item: ProviderCatalogItem) {
    notice = '';
    error = '';
    const res = await installCatalogTemplate({
      entry_id: item.entry_id,
      local_id: localId(item),
    });
    if (res.success) {
      notice = $_('vendor_catalog.install_success');
    } else {
      error = res.error?.message ?? $_('vendor_catalog.install_failed');
    }
  }

  onMount(() => {
    loadCatalog();
  });
</script>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 flex items-center justify-between gap-3 flex-wrap">
    <div>
      <h3 class="font-medium text-gray-900 text-sm">{$_('vendor_catalog.title')}</h3>
      <p class="text-xs text-gray-500 mt-1">{$_('vendor_catalog.subtitle')}</p>
      <p class="text-[11px] text-gray-400 mt-1">
        {$_('vendor_catalog.offline')} · {catalogVersion || $_('vendor_catalog.builtin')}
      </p>
    </div>
    <div class="flex items-center gap-2 flex-wrap">
      <input
        bind:value={query}
        onkeydown={(event) => event.key === 'Enter' && loadCatalog()}
        aria-label={$_('vendor_catalog.search')}
        placeholder={$_('vendor_catalog.search')}
        class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg w-48 focus:outline-none focus:ring-1 focus:ring-blue-500" />
      <select
        bind:value={familyFilter}
        aria-label={$_('vendor_catalog.filter_family')}
        class="text-xs border border-gray-200 px-2 py-1.5 rounded-lg">
        <option value="">{$_('vendor_catalog.filter_family_all')}</option>
        <option value="openai_compatible">openai_compatible</option>
        <option value="http_mt">http_mt</option>
      </select>
      <select
        bind:value={tierFilter}
        aria-label={$_('vendor_catalog.filter_tier')}
        class="text-xs border border-gray-200 px-2 py-1.5 rounded-lg">
        <option value="">{$_('vendor_catalog.filter_tier_all')}</option>
        <option value="mock-verified">mock-verified</option>
        <option value="schema-only">schema-only</option>
        <option value="live-verified">live-verified</option>
      </select>
      <button
        onclick={loadCatalog}
        disabled={loading}
        class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg hover:bg-gray-50 text-gray-600 disabled:opacity-50">
        {$_('common.refresh')}
      </button>
      <button
        onclick={refreshCatalog}
        disabled={refreshing}
        class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
        {$_('vendor_catalog.verify_local')}
      </button>
    </div>
  </div>

  {#if error}
    <div class="border-b border-red-200 bg-red-50 px-5 py-3 text-sm text-red-700">{error}</div>
  {/if}
  {#if notice}
    <div class="border-b border-green-200 bg-green-50 px-5 py-3 text-sm text-green-700">{notice}</div>
  {/if}

  {#if wizardOpen && wizardItem}
    <div
      class="border-b border-blue-100 bg-blue-50/40 px-5 py-4"
      data-testid="provider-setup-wizard"
      role="region"
      aria-label={$_('vendor_catalog.wizard_title')}>
      <div class="flex items-start justify-between gap-3 flex-wrap">
        <div>
          <h4 class="text-sm font-medium text-gray-900">{$_('vendor_catalog.wizard_title')}</h4>
          <p class="text-xs text-gray-600 mt-1">
            {wizardItem.name || wizardItem.template_id}
            <span class="font-mono text-gray-400"> · {wizardItem.family || wizardItem.kind}</span>
          </p>
        </div>
        <button
          type="button"
          onclick={closeWizard}
          disabled={wizardBusy}
          data-testid="wizard-close"
          class="text-xs text-gray-500 hover:text-gray-800 disabled:opacity-50">
          {$_('common.cancel')}
        </button>
      </div>

      <ol class="mt-3 flex flex-wrap gap-2 text-[11px]" aria-label={$_('vendor_catalog.wizard_steps')}>
        {#each WIZARD_STEPS as step}
          <li
            class="px-2 py-1 rounded border {stepIndex(step) < stepIndex(wizardStep)
              ? 'border-green-300 bg-green-50 text-green-800'
              : step === wizardStep
                ? 'border-blue-400 bg-white text-blue-800'
                : 'border-gray-200 text-gray-400'}">
            {$_(`vendor_catalog.wizard_step_${step}`)}
          </li>
        {/each}
      </ol>

      {#if wizardError}
        <div class="mt-3 text-sm text-red-700" role="alert">{wizardError}</div>
      {/if}
      {#if wizardHint}
        <div class="mt-2 text-xs text-amber-800 bg-amber-50 border border-amber-100 rounded-lg px-3 py-2" data-testid="wizard-hint">
          {wizardHint}
        </div>
      {/if}
      {#if wizardNotice}
        <div class="mt-3 text-sm text-green-700">{wizardNotice}</div>
      {/if}

      <div class="mt-4 space-y-3">
        {#if wizardStep === 'install'}
          <p class="text-xs text-gray-600">{$_('vendor_catalog.wizard_install_hint')}</p>
          <label class="block text-xs text-gray-500">
            {$_('vendor_catalog.wizard_local_id')}
            <input
              bind:value={localComponentId}
              class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
              data-testid="wizard-local-id" />
          </label>
          <button
            type="button"
            onclick={runInstall}
            disabled={wizardBusy || !localComponentId.trim()}
            data-testid="wizard-install-next"
            class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
            {$_('vendor_catalog.wizard_install_action')}
          </button>
        {:else if wizardStep === 'key'}
          <p class="text-xs text-gray-600">{$_('vendor_catalog.wizard_key_hint')}</p>
          <p class="text-[11px] text-gray-500">
            {$_('vendor_catalog.tier_label')}:
            <span class={tierBadgeClass(evidenceTier(wizardItem)) + ' px-1.5 py-0.5 rounded'}>
              {evidenceTier(wizardItem)}
            </span>
          </p>
          <label class="block text-xs text-gray-500">
            {$_('vendor_catalog.wizard_key_id')}
            <input
              bind:value={keyId}
              class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
              data-testid="wizard-key-id" />
          </label>
          {#each authFieldNames(wizardItem) as fieldName, idx}
            <label class="block text-xs text-gray-500">
              {fieldName}
              <input
                type="password"
                bind:value={authFieldValues[fieldName]}
                autocomplete="off"
                class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
                data-testid={idx === 0 ? 'wizard-api-key' : `wizard-auth-${fieldName}`} />
            </label>
          {/each}
          <label class="block text-xs text-gray-500">
            {$_('vendor_catalog.wizard_request_url')}
            <input
              bind:value={configUrl}
              class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
              data-testid="wizard-request-url" />
          </label>
          {#if isOpenaiFamily(wizardItem)}
            <label class="block text-xs text-gray-500">
              {$_('vendor_catalog.wizard_model')}
              <input
                bind:value={configModel}
                class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
                data-testid="wizard-model" />
            </label>
          {/if}
          {#if isCustomHttp(wizardItem) || configResponsePath}
            <label class="block text-xs text-gray-500">
              {$_('vendor_catalog.wizard_response_path')}
              <input
                bind:value={configResponsePath}
                class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
                data-testid="wizard-response-path" />
            </label>
          {/if}
          <button
            type="button"
            onclick={runBindKey}
            disabled={wizardBusy}
            data-testid="wizard-key-next"
            class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
            {$_('vendor_catalog.wizard_key_action')}
          </button>
        {:else if wizardStep === 'test'}
          <p class="text-xs text-gray-600">{$_('vendor_catalog.wizard_test_hint')}</p>
          <label class="block text-xs text-gray-500">
            {$_('vendor_catalog.wizard_test_text')}
            <input
              bind:value={quickTestText}
              class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg"
              data-testid="wizard-test-text" />
          </label>
          {#if quickTestResult}
            <pre class="text-xs bg-white border border-gray-200 rounded-lg p-2 max-w-xl overflow-auto">{quickTestResult}</pre>
          {/if}
          <button
            type="button"
            onclick={runQuickTest}
            disabled={wizardBusy}
            data-testid="wizard-test-next"
            class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
            {$_('vendor_catalog.wizard_test_action')}
          </button>
        {:else if wizardStep === 'enable'}
          <p class="text-xs text-gray-600">{$_('vendor_catalog.wizard_enable_hint')}</p>
          <button
            type="button"
            onclick={runEnable}
            disabled={wizardBusy}
            data-testid="wizard-enable-next"
            class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
            {$_('vendor_catalog.wizard_enable_action')}
          </button>
        {:else if wizardStep === 'route'}
          <p class="text-xs text-gray-600">{$_('vendor_catalog.wizard_route_hint')}</p>
          <label class="block text-xs text-gray-500">
            {$_('vendor_catalog.wizard_route_slot')}
            <input
              bind:value={routeSlot}
              class="mt-1 block w-full max-w-md text-xs border border-gray-200 px-3 py-1.5 rounded-lg font-mono"
              data-testid="wizard-route-slot" />
          </label>
          <button
            type="button"
            onclick={runRoute}
            disabled={wizardBusy}
            data-testid="wizard-route-next"
            class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
            {$_('vendor_catalog.wizard_route_action')}
          </button>
        {:else}
          <p class="text-sm text-green-800" data-testid="wizard-done">
            {$_('vendor_catalog.wizard_complete')}
          </p>
          <button
            type="button"
            onclick={closeWizard}
            class="text-xs border border-gray-300 px-3 py-1.5 rounded-lg hover:bg-white">
            {$_('common.close')}
          </button>
        {/if}
      </div>
    </div>
  {/if}

  <table class="w-full text-sm">
    <thead>
      <tr class="text-xs text-gray-500 bg-gray-50">
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_catalog.th_name')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_catalog.th_vendor_id')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_catalog.th_kind')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_catalog.th_tier')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_catalog.th_formats')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_catalog.th_source')}</th>
        <th class="px-4 py-2.5 text-right font-medium">{$_('vendor_catalog.th_actions')}</th>
      </tr>
    </thead>
    <tbody>
      {#if loading}
        <tr><td colspan="7" class="px-4 py-8 text-center text-gray-400">{$_('common.loading')}</td></tr>
      {:else if filteredItems.length === 0}
        <tr><td colspan="7" class="px-4 py-8 text-center text-gray-400">{$_('vendor_catalog.no_data')}</td></tr>
      {:else}
        {#each filteredItems as item (item.entry_id + item.template_id)}
          <tr class="border-t border-gray-50 align-top hover:bg-gray-50/50">
            <td class="px-4 py-3">
              <div class="font-medium text-gray-800">{item.name || item.template_id}</div>
              <div class="font-mono text-[11px] text-gray-400 mt-1">{item.template_id}</div>
            </td>
            <td class="px-4 py-3 font-mono text-xs text-gray-500">{item.vendor_id || '-'}</td>
            <td class="px-4 py-3 text-xs text-gray-500">{item.kind || item.family || '-'}</td>
            <td class="px-4 py-3 text-xs">
              <span class="px-1.5 py-0.5 rounded {tierBadgeClass(evidenceTier(item))}">{evidenceTier(item)}</span>
            </td>
            <td class="px-4 py-3 text-xs text-gray-500">
              {item.supported_content_formats?.join(', ') || '-'}
            </td>
            <td class="px-4 py-3 text-xs">
              <span class="text-green-700">{item.verified ? $_('vendor_catalog.verified') : $_('vendor_catalog.unverified')}</span>
              <div class="text-gray-400 mt-1">{item.source}</div>
            </td>
            <td class="px-4 py-3 text-right space-x-2 whitespace-nowrap">
              <button
                onclick={() => openWizard(item)}
                data-testid="open-provider-wizard"
                class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700">
                {$_('vendor_catalog.wizard_open')}
              </button>
              <button
                onclick={() => installOnly(item)}
                class="text-xs border border-blue-200 text-blue-700 px-3 py-1.5 rounded-lg hover:bg-blue-50">
                {$_('vendor_catalog.install')}
              </button>
            </td>
          </tr>
        {/each}
      {/if}
    </tbody>
  </table>
</div>
