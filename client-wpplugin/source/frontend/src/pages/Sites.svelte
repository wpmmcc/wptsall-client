<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { isOk } from '../lib/api/client';
  import { deleteDomainToken, importSiteConnection, testDomainToken, upsertDomainToken } from '../lib/api/sites';
  import { getWpClientApiToastCopy } from '../lib/errors/wpClientApi';
  import { status, fetchStatus } from '../lib/stores/status';
  import { showToast } from '../lib/stores/toast';

  interface TokenItem {
    api_base_url: string;
    token_prefix: string;
    token_len: number;
    route_secret_set?: boolean;
  }

  // Modal 状态
  let showModal = $state(false);
  let editingOriginalUrl = $state('');
  let editUrl = $state('');
  let editToken = $state('');
  let editRouteSecret = $state('');
  let saving = $state(false);
  const siteUrlInputId = 'site-modal-url';
  const siteTokenInputId = 'site-modal-token';
  const siteRouteSecretInputId = 'site-modal-route-secret';

  function handleModalBackdropKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' || event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      closeModal();
    }
  }
  let testingUrl = $state<string | null>(null);
  let importPackJson = $state('');
  let importPairingCode = $state('');
  let importDeviceLabel = $state('');
  let importingPack = $state(false);

  function openAdd() {
    editingOriginalUrl = '';
    editUrl = '';
    editToken = '';
    editRouteSecret = '';
    showModal = true;
  }
  function openEdit(item: TokenItem) {
    editingOriginalUrl = item.api_base_url;
    editUrl = item.api_base_url;
    editToken = '';
    // Secrets are write-only: status never sends the existing value back.
    editRouteSecret = '';
    showModal = true;
  }
  function closeModal() {
    showModal = false;
    editingOriginalUrl = '';
  }

  async function saveToken() {
    if (!editUrl.trim() || (!editToken.trim() && !editingOriginalUrl)) {
      showToast('error', $_('sites.fill_domain_token'));
      return;
    }
    saving = true;
    try {
      const body: {
        api_base_url: string;
        existing_api_base_url?: string;
        wp_client_token?: string;
        route_secret?: string;
      } = {
        api_base_url: editUrl.trim(),
      };
      if (editingOriginalUrl) body.existing_api_base_url = editingOriginalUrl;
      if (editToken.trim()) body.wp_client_token = editToken.trim();
      if (editRouteSecret.trim()) body.route_secret = editRouteSecret.trim();
      const r = await upsertDomainToken(body);
      if (isOk(r)) {
        showToast('success', $_('sites.token_saved'));
        closeModal();
        await fetchStatus();
      } else {
        showToast('error', $_('common.save_failed'), r.error?.message);
      }
    } finally { saving = false; }
  }

  async function deleteToken(url: string) {
    const r = await deleteDomainToken(url);
    if (isOk(r)) { showToast('success', $_('sites.token_deleted')); await fetchStatus(); }
    else showToast('error', $_('common.delete_failed'), r.error?.message);
  }

  async function testToken(url: string) {
    testingUrl = url;
    try {
      const r = await testDomainToken(url);
      if (isOk(r)) showToast('success', $_('sites.connection_ok', { values: { url } }));
      else {
        const toast = getWpClientApiToastCopy($_('sites.connection_failed'), r.error, 'site_test');
        showToast('error', toast.message, toast.detail);
      }
    } finally { testingUrl = null; }
  }

  async function importConnectionPack() {
    if (!importPackJson.trim()) {
      showToast('error', $_('sites.import_pack_required'));
      return;
    }
    let pack: unknown;
    try {
      pack = JSON.parse(importPackJson);
    } catch (err) {
      showToast('error', $_('sites.import_pack_invalid_json'), String(err));
      return;
    }
    importingPack = true;
    try {
      const r = await importSiteConnection({
        site_connection_pack: pack,
        pairing_code: importPairingCode.trim() || undefined,
        device_label: importDeviceLabel.trim() || undefined,
      });
      if (isOk(r)) {
        showToast('success', $_('sites.import_pack_success', { values: { url: r.data.api_base_url } }));
        importPackJson = '';
        importPairingCode = '';
        importDeviceLabel = '';
        await fetchStatus();
      } else {
        showToast('error', $_('sites.import_pack_failed'), r.error?.message);
      }
    } finally { importingPack = false; }
  }

  let tokens = $derived(($status?.domain_token_bindings ?? []) as TokenItem[]);
</script>

<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('sites.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('sites.subtitle')}</p>
</div>

<!-- Site connection pack import -->
<div class="bg-white border border-gray-200 rounded-xl p-5 mb-4">
  <div class="flex items-center justify-between gap-3 mb-3">
    <div>
      <h3 class="font-medium text-gray-900 text-sm">{$_('sites.import_pack_title')}</h3>
      <p class="text-xs text-gray-500 mt-1">{$_('sites.import_pack_hint')}</p>
    </div>
  </div>
  <div class="grid grid-cols-1 lg:grid-cols-3 gap-3">
    <textarea bind:value={importPackJson}
      data-testid="sites-import-pack-json"
      class="lg:col-span-3 border border-gray-200 rounded-lg px-3 py-2 text-xs font-mono min-h-28 focus:outline-none focus:ring-2 focus:ring-blue-500"
      placeholder={$_('sites.import_pack_placeholder')}></textarea>
    <input bind:value={importPairingCode}
      data-testid="sites-import-pairing-code"
      class="border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500"
      placeholder={$_('sites.import_pairing_code_placeholder')} />
    <input bind:value={importDeviceLabel}
      data-testid="sites-import-device-label"
      class="border border-gray-200 rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500"
      placeholder={$_('sites.import_device_label_placeholder')} />
    <button onclick={importConnectionPack} disabled={importingPack || !importPackJson.trim()}
      data-testid="sites-import-pack-submit"
      class="px-3 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors">
      {importingPack ? $_('sites.importing_pack') : $_('sites.import_pack_button')}
    </button>
  </div>
</div>

<!-- 已绑定 Token -->
<div class="bg-white border border-gray-200 rounded-xl overflow-hidden mb-4">
  <div class="px-5 py-3 border-b border-gray-100 flex items-center justify-between">
    <h3 class="font-medium text-gray-900 text-sm">{$_('sites.bound_sites', { values: { count: tokens.length } })}</h3>
    <button onclick={openAdd}
      data-testid="sites-add-site"
      class="px-3 py-1.5 bg-blue-600 text-white text-xs rounded-lg hover:bg-blue-700 transition-colors">
      {$_('sites.add_site')}
    </button>
  </div>
  <table class="w-full text-sm">
    <thead>
      <tr class="text-xs text-gray-500 bg-gray-50">
        <th class="px-4 py-2.5 text-left font-medium">{$_('sites.th_domain')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('sites.th_wp_token')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('sites.th_route_secret')}</th>
        <th class="px-4 py-2.5 text-right font-medium">{$_('sites.th_actions')}</th>
      </tr>
    </thead>
    <tbody>
      {#if tokens.length === 0}
        <tr><td colspan="4" class="px-4 py-8 text-center text-gray-400">
          {$_('sites.no_sites')}
        </td></tr>
      {:else}
        {#each tokens as t}
          <tr class="border-t border-gray-50 hover:bg-gray-50/50">
            <td class="px-4 py-3 text-gray-700 font-mono text-xs">{t.api_base_url}</td>
            <td class="px-4 py-3 font-mono text-xs">
              <span class="text-gray-500">{t.token_prefix}</span>
              <span class="text-gray-400 ml-1">({t.token_len} {$_('common.chars')})</span>
            </td>
            <td class="px-4 py-3 text-xs">
              {#if t.route_secret_set}
                <span class="text-green-600">{$_('common.configured')}</span>
              {:else}
                <span class="text-orange-500">{$_('common.not_configured')}</span>
              {/if}
            </td>
            <td class="px-4 py-3 text-right">
              <button onclick={() => testToken(t.api_base_url)}
                data-testid="sites-test-connection"
                disabled={testingUrl === t.api_base_url}
                class="text-xs text-blue-600 hover:text-blue-700 mr-3 disabled:opacity-50">
                {testingUrl === t.api_base_url ? $_('common.testing') : $_('common.test')}
              </button>
              <button onclick={() => openEdit(t)} class="text-xs text-gray-500 hover:text-gray-700 mr-3">{$_('common.edit')}</button>
              <button onclick={() => deleteToken(t.api_base_url)} class="text-xs text-red-500 hover:text-red-600">{$_('common.delete')}</button>
            </td>
          </tr>
        {/each}
      {/if}
    </tbody>
  </table>
</div>

<!-- 添加/编辑 Modal -->
{#if showModal}
  <div class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('sites.close_modal')}
    onclick={(e) => { if (e.target === e.currentTarget) closeModal(); }}
    onkeydown={handleModalBackdropKeydown}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-md">
      <h3 class="font-semibold text-gray-900 mb-4">{editingOriginalUrl ? $_('sites.edit_site') : $_('sites.add_site_title')}</h3>
      <div class="space-y-3">
        <div>
          <label for={siteUrlInputId} class="block text-sm font-medium text-gray-700 mb-1">{$_('sites.label_domain')}</label>
          <input id={siteUrlInputId} data-testid="sites-modal-url" bind:value={editUrl} placeholder="https://example.com"
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500" />
          <p class="text-xs text-gray-400 mt-1">{$_('sites.domain_hint')}</p>
        </div>
        <div>
          <label for={siteTokenInputId} class="block text-sm font-medium text-gray-700 mb-1">{$_('sites.label_wp_token')}</label>
          <input id={siteTokenInputId} data-testid="sites-modal-token" bind:value={editToken} placeholder={editingOriginalUrl ? $_('sites.placeholder_token_edit') : $_('sites.placeholder_token_new')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-blue-500" />
          <p class="text-xs text-gray-400 mt-1">
            {#if editingOriginalUrl}
              {$_('sites.token_hint_edit')}
            {:else}
              {$_('sites.token_hint_new')}
            {/if}
          </p>
        </div>
        <div>
          <label for={siteRouteSecretInputId} class="block text-sm font-medium text-gray-700 mb-1">{$_('sites.label_route_secret')}</label>
          <input id={siteRouteSecretInputId} data-testid="sites-modal-route-secret" bind:value={editRouteSecret} placeholder={$_('sites.placeholder_route_secret')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-500" />
          <p class="text-xs text-gray-400 mt-1">
            {$_('sites.route_secret_hint')}
          </p>
        </div>
      </div>
      <div class="flex gap-2 mt-5">
        <button onclick={saveToken} disabled={saving}
          data-testid="sites-modal-save"
          class="flex-1 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors">
          {saving ? $_('common.saving') : $_('common.save')}
        </button>
        <button onclick={closeModal}
          class="flex-1 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50 transition-colors">
          {$_('common.cancel')}
        </button>
      </div>
    </div>
  </div>
{/if}
