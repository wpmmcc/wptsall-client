<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { onMount } from 'svelte';
  import { hasData, isOk } from '../api/client';
  import { authorizeOAuth, createOAuthConfig, deleteOAuthConfig, listOAuthConfigs, updateOAuthConfig } from '../api/keys';
  import { showToast } from '../stores/toast';
  import {
    formatExpiry,
    handleBackdropKeydown,
    oauthModalFieldId,
  } from './helpers';
  import type {
    AuthExtraPreset,
    ExtraParam,
    OAuthFormState,
    OAuthItem,
  } from './types';
  import OAuthModal from './modals/OAuthModal.svelte';

  const VENDOR_CALLBACK_URL = `${window.location.protocol}//${window.location.hostname}:${window.location.port}/oauth/vendor/callback`;

  const AUTH_EXTRA_PRESETS: AuthExtraPreset[] = [
    { label: 'Google', params: { access_type: 'offline', prompt: 'consent' } },
    { label: 'Facebook', params: { display: 'popup' } },
    { label: 'Salesforce', params: { prompt: 'consent' } },
  ];

  let oauthList = $state<OAuthItem[]>([]);
  let oauthLoading = $state(false);
  let showOAuthModal = $state(false);
  let editingOAuth = $state<OAuthItem | null>(null);
  let authorizingId = $state<string | null>(null);
  let oauthForm = $state<OAuthFormState>({
    id: '',
    vendor_id: '',
    label: '',
    grant_type: 'client_credentials',
    auth_url: '',
    token_url: '',
    client_id: '',
    client_secret: '',
    scopes: '',
    auth_extra_params: [],
    max_concurrent: '5',
    weight: '1',
    max_input_chars: '0',
    max_file_size_mb: '100',
    token_field: 'access_token',
  });

  function applyExtraParamPreset(preset: Record<string, string>) {
    const existing = new Map(oauthForm.auth_extra_params.map((p) => [p.key, p.value]));
    for (const [k, v] of Object.entries(preset)) {
      existing.set(k, v);
    }
    oauthForm.auth_extra_params = Array.from(existing.entries()).map(([key, value]) => ({
      key,
      value,
    }));
  }

  function addExtraParam() {
    oauthForm.auth_extra_params = [...oauthForm.auth_extra_params, { key: '', value: '' }];
  }

  function removeExtraParam(index: number) {
    oauthForm.auth_extra_params = oauthForm.auth_extra_params.filter((_, i) => i !== index);
  }

  function extraParamsToRecord(pairs: ExtraParam[]): Record<string, string> {
    const rec: Record<string, string> = {};
    for (const { key, value } of pairs) {
      if (key.trim()) rec[key.trim()] = value;
    }
    return rec;
  }

  function recordToExtraParams(rec: Record<string, string> | undefined): ExtraParam[] {
    if (!rec) return [];
    return Object.entries(rec).map(([key, value]) => ({ key, value }));
  }

  async function loadOAuth() {
    oauthLoading = true;
    try {
      const r = await listOAuthConfigs();
      if (hasData(r)) oauthList = r.data.items;
    } finally {
      oauthLoading = false;
    }
  }

  function openCreateOAuth() {
    editingOAuth = null;
    oauthForm = {
      id: '',
      vendor_id: '',
      label: '',
      grant_type: 'client_credentials',
      auth_url: '',
      token_url: '',
      client_id: '',
      client_secret: '',
      scopes: '',
      auth_extra_params: [],
      max_concurrent: '5',
      weight: '1',
      max_input_chars: '0',
      max_file_size_mb: '100',
      token_field: 'access_token',
    };
    showOAuthModal = true;
  }

  function openEditOAuth(o: OAuthItem) {
    editingOAuth = o;
    oauthForm = {
      id: o.id,
      vendor_id: o.vendor_id,
      label: o.label,
      grant_type: o.grant_type,
      auth_url: o.auth_url ?? '',
      token_url: o.token_url ?? '',
      client_id: o.client_id,
      client_secret: '',
      scopes: o.scopes ?? '',
      auth_extra_params: recordToExtraParams(o.auth_extra_params),
      max_concurrent: String(o.max_concurrent ?? 5),
      weight: String(o.weight ?? 1),
      max_input_chars: String(o.max_input_chars ?? 0),
      max_file_size_mb: String(o.max_file_size_mb ?? 0),
      token_field: o.token_field ?? 'access_token',
    };
    showOAuthModal = true;
  }

  async function saveOAuth() {
    if (!oauthForm.id.trim() || !oauthForm.client_id.trim()) {
      showToast('error', $_('oauth.fill_id_client_id'));
      return;
    }
    if (oauthForm.grant_type === 'authorization_code' && !oauthForm.auth_url.trim()) {
      showToast('error', $_('oauth.fill_auth_url'));
      return;
    }
    if (
      (oauthForm.grant_type === 'client_credentials' ||
        oauthForm.grant_type === 'jwt_bearer') &&
      !oauthForm.token_url.trim()
    ) {
      showToast('error', $_('oauth.fill_token_url'));
      return;
    }

    const payload: Record<string, unknown> = {
      id: oauthForm.id.trim(),
      vendor_id: oauthForm.vendor_id.trim(),
      label: oauthForm.label || undefined,
      grant_type: oauthForm.grant_type,
      auth_url: oauthForm.auth_url.trim() || undefined,
      token_url: oauthForm.token_url.trim() || undefined,
      client_id: oauthForm.client_id.trim(),
      client_secret: oauthForm.client_secret || undefined,
      scopes: oauthForm.scopes || undefined,
      auth_extra_params: extraParamsToRecord(oauthForm.auth_extra_params),
      max_concurrent: parseInt(oauthForm.max_concurrent) || 5,
      weight: parseInt(oauthForm.weight) || 1,
      max_input_chars: parseInt(oauthForm.max_input_chars) || 0,
      max_file_size_mb: parseFloat(oauthForm.max_file_size_mb) || 0,
      token_field: oauthForm.token_field.trim() || 'access_token',
    };

    let r;
    if (editingOAuth) {
      r = await updateOAuthConfig(editingOAuth.id, payload);
    } else {
      r = await createOAuthConfig(payload as {
        id: string;
        vendor_id: string;
        label?: string;
        grant_type: string;
        auth_url?: string;
        token_url?: string;
        client_id: string;
        client_secret?: string;
        scopes?: string;
        auth_extra_params?: Record<string, string>;
        max_concurrent?: number;
        weight?: number;
        max_input_chars?: number;
        max_file_size_mb?: number;
        token_field?: string;
      });
    }

    if (isOk(r)) {
      showToast('success', editingOAuth ? $_('oauth.config_updated') : $_('oauth.config_saved'));
      showOAuthModal = false;
      await loadOAuth();
    } else {
      showToast('error', editingOAuth ? $_('oauth.update_failed') : $_('oauth.create_failed'), (r as any).error?.message);
    }
  }

  async function deleteOAuth(id: string) {
    const r = await deleteOAuthConfig(id);
    if (isOk(r)) {
      showToast('success', $_('oauth.config_deleted'));
      await loadOAuth();
    }
  }

  async function authorizeOAuthAction(id: string, grantType: string) {
    authorizingId = id;
    try {
      const r = await authorizeOAuth(id);

      if (!isOk(r)) {
        showToast('error', $_('oauth.auth_failed'), (r as any).error?.message);
        return;
      }

      if (grantType === 'authorization_code' && r.data.authorize_url) {
        const popup = window.open(r.data.authorize_url, 'vendor_oauth', 'width=600,height=700,left=200,top=100');
        if (!popup) {
          showToast('error', $_('oauth.no_popup'), $_('oauth.allow_popup'));
          return;
        }

        let done = false;
        for (let i = 0; i < 90 && !done; i++) {
          await new Promise((res) => setTimeout(res, 2000));
          await loadOAuth();
          const item = oauthList.find((o) => o.id === id);
          if (item?.has_token) {
            showToast('success', $_('oauth.auth_success'));
            done = true;
          } else if (popup.closed) {
            await new Promise((res) => setTimeout(res, 500));
            await loadOAuth();
            const item2 = oauthList.find((o) => o.id === id);
            if (item2?.has_token) {
              showToast('success', $_('oauth.auth_success'));
            } else {
              showToast('error', $_('oauth.auth_incomplete'), $_('oauth.auth_incomplete_detail'));
            }
            done = true;
          }
        }
        if (!done) {
          showToast('error', $_('oauth.auth_timeout'), $_('oauth.auth_timeout_detail'));
        }
      } else if (r.data.token_preview) {
        showToast('success', $_('oauth.auth_success') + ': ' + r.data.token_preview + ' (' + r.data.expires_in + 's)');
        await loadOAuth();
      }
    } finally {
      authorizingId = null;
    }
  }

  async function copyToClipboard(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      showToast('success', $_('oauth.copied'));
    } catch {
      showToast('error', $_('oauth.copy_failed'));
    }
  }

  onMount(() => {
    loadOAuth();
  });
</script>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 flex items-center justify-between">
    <h3 class="font-medium text-gray-900 text-sm">{$_('oauth.title')}</h3>
    <div class="flex gap-2">
      <button
        onclick={loadOAuth}
        class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg hover:bg-gray-50 text-gray-600">
        {$_('common.refresh')}
      </button>
      <button
        onclick={openCreateOAuth}
        class="px-3 py-1.5 bg-blue-600 text-white text-xs rounded-lg hover:bg-blue-700">
        {$_('oauth.add')}
      </button>
    </div>
  </div>
  <table class="w-full text-sm">
    <thead>
      <tr class="text-xs text-gray-500 bg-gray-50">
        <th class="px-4 py-2.5 text-left font-medium">{$_('oauth.th_name')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('oauth_configs.th.vendor')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('oauth.th_grant_type')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('oauth.th_concurrent')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('oauth.th_weight')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('oauth.th_max_chars')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('oauth.th_max_file')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('oauth.th_token_field')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('oauth.th_token_status')}</th>
        <th class="px-4 py-2.5 text-right font-medium">{$_('oauth.th_actions')}</th>
      </tr>
    </thead>
    <tbody>
      {#if oauthLoading}
        <tr><td colspan="10" class="px-4 py-8 text-center text-gray-400">{$_('common.loading')}</td></tr>
      {:else if oauthList.length === 0}
        <tr><td colspan="10" class="px-4 py-8 text-center text-gray-400">{$_('oauth.no_configs')}</td></tr>
      {:else}
        {#each oauthList as o}
          <tr class="border-t border-gray-50 hover:bg-gray-50/50">
            <td class="px-4 py-3">
              {#if o.label}
                <span class="text-sm font-medium text-gray-800">{o.label}</span>
                <div class="font-mono text-xs text-gray-400">{o.id}</div>
              {:else}
                <span class="font-mono text-xs text-gray-600">{o.id}</span>
              {/if}
            </td>
            <td class="px-4 py-3 text-xs text-gray-500">{o.vendor_id}</td>
            <td class="px-4 py-3 text-xs">
              {o.grant_type}
              {#if o.grant_type === 'authorization_code' && o.has_refresh_token}
                <span class="ml-1 text-green-600" title={$_('oauth_configs.has_refresh_token')}>↻</span>
              {/if}
            </td>
            <td class="px-4 py-3 text-center text-xs">{o.max_concurrent ?? 5}</td>
            <td class="px-4 py-3 text-center text-xs">{o.weight ?? 1}</td>
            <td class="px-4 py-3 text-center text-xs">
              {(o.max_input_chars ?? 0) === 0 ? $_('common.no_limit') : (o.max_input_chars ?? 0).toLocaleString()}
            </td>
            <td class="px-4 py-3 text-center text-xs">
              {(o.max_file_size_mb ?? 0) === 0 ? $_('common.no_limit') : o.max_file_size_mb + ' MB'}
            </td>
            <td class="px-4 py-3 text-xs font-mono text-gray-500">{o.token_field || 'access_token'}</td>
            <td class="px-4 py-3 text-center">
              {#if o.has_token}
                <span class="text-xs px-2 py-0.5 rounded-full bg-green-100 text-green-700">
                  {o.token_expires_at ? formatExpiry(o.token_expires_at) : $_('oauth.valid')}
                </span>
              {:else}
                <span class="text-xs px-2 py-0.5 rounded-full bg-gray-100 text-gray-500">{$_('oauth.unauthorized')}</span>
              {/if}
            </td>
            <td class="px-4 py-3 text-right whitespace-nowrap">
              <button
                onclick={() => authorizeOAuthAction(o.id, o.grant_type)}
                disabled={authorizingId === o.id}
                class="text-xs text-blue-600 hover:text-blue-700 mr-2 disabled:opacity-50">
                {authorizingId === o.id
                  ? $_('oauth.authorizing')
                  : o.grant_type === 'authorization_code'
                    ? $_('oauth.login_authorize')
                    : $_('oauth.authorize')}
              </button>
              <button
                onclick={() => openEditOAuth(o)}
                class="text-xs text-gray-500 hover:text-gray-700 mr-2">
                {$_('common.edit')}
              </button>
              <button
                onclick={() => deleteOAuth(o.id)}
                class="text-xs text-red-500 hover:text-red-600">
                {$_('common.delete')}
              </button>
            </td>
          </tr>
        {/each}
      {/if}
    </tbody>
  </table>
</div>

<OAuthModal
  open={showOAuthModal}
  {editingOAuth}
  {oauthForm}
  authExtraPresets={AUTH_EXTRA_PRESETS}
  callbackUrl={VENDOR_CALLBACK_URL}
  {oauthModalFieldId}
  {handleBackdropKeydown}
  onApplyPreset={applyExtraParamPreset}
  onAddExtraParam={addExtraParam}
  onRemoveExtraParam={removeExtraParam}
  onCopyCallbackUrl={() => copyToClipboard(VENDOR_CALLBACK_URL)}
  onSave={saveOAuth}
  onClose={() => {
    showOAuthModal = false;
  }} />
