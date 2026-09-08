<script lang="ts">
  import { _ } from 'svelte-i18n';
  import { onMount } from 'svelte';
  import { hasData, isOk } from '../api/client';
  import { createVendorKey, deleteVendorKey, listVendorKeys, updateVendorKey } from '../api/keys';
  import { showToast } from '../stores/toast';
  import {
    handleBackdropKeydown,
    keyModalFieldId,
  } from './helpers';
  import type {
    KeyFormState,
    VendorKeyItem,
  } from './types';
  import KeyModal from './modals/KeyModal.svelte';

  let vendorKeys = $state<VendorKeyItem[]>([]);
  let keysLoading = $state(false);
  let showKeyModal = $state(false);
  let editingKey = $state<VendorKeyItem | null>(null);
  let filterVendorId = $state('');
  let keyForm = $state<KeyFormState>({
    id: '',
    vendor_id: '',
    label: '',
    auth_json: '{}',
    max_concurrent: '5',
    requests_per_second: '3',
    weight: '1',
    enabled: true,
    max_input_chars: '0',
    max_file_size_mb: '100',
  });

  async function loadKeys() {
    keysLoading = true;
    try {
      const r = await listVendorKeys(filterVendorId || undefined);
      if (hasData(r)) vendorKeys = r.data.items;
    } finally {
      keysLoading = false;
    }
  }

  function openCreateKey() {
    editingKey = null;
    keyForm = {
      id: '',
      vendor_id: '',
      label: '',
      auth_json: '{}',
      max_concurrent: '5',
      requests_per_second: '3',
      weight: '1',
      enabled: true,
      max_input_chars: '0',
      max_file_size_mb: '100',
    };
    showKeyModal = true;
  }

  function openEditKey(k: VendorKeyItem) {
    editingKey = k;
    keyForm = {
      id: k.id,
      vendor_id: k.vendor_id,
      label: k.label,
      auth_json: '{}',
      max_concurrent: String(k.max_concurrent),
      requests_per_second: String(k.requests_per_second),
      weight: String(k.weight),
      enabled: k.enabled,
      max_input_chars: String(k.max_input_chars ?? 0),
      max_file_size_mb: String(k.max_file_size_mb ?? 0),
    };
    showKeyModal = true;
  }

  async function saveKey() {
    let authValues: Record<string, string> = {};
    try {
      authValues = JSON.parse(keyForm.auth_json);
    } catch {
      showToast('error', $_('vendor_keys.auth_json_error'));
      return;
    }
    if (!editingKey && !keyForm.id.trim()) {
      showToast('error', $_('vendor_keys.fill_key_id'));
      return;
    }

    let r;
    if (editingKey) {
      r = await updateVendorKey(editingKey.id, {
        label: keyForm.label || undefined,
        auth_values: Object.keys(authValues).length ? authValues : undefined,
        max_concurrent: parseInt(keyForm.max_concurrent) || undefined,
        requests_per_second: parseFloat(keyForm.requests_per_second) || undefined,
        weight: parseInt(keyForm.weight) || undefined,
        enabled: keyForm.enabled,
        max_input_chars: parseInt(keyForm.max_input_chars) || 0,
        max_file_size_mb: parseFloat(keyForm.max_file_size_mb) || 0,
      });
    } else {
      r = await createVendorKey({
        id: keyForm.id.trim(),
        vendor_id: keyForm.vendor_id.trim(),
        label: keyForm.label || undefined,
        auth_values: authValues,
        max_concurrent: parseInt(keyForm.max_concurrent) || 5,
        requests_per_second: parseFloat(keyForm.requests_per_second) || 3,
        weight: parseInt(keyForm.weight) || 1,
        enabled: keyForm.enabled,
        max_input_chars: parseInt(keyForm.max_input_chars) || 0,
        max_file_size_mb: parseFloat(keyForm.max_file_size_mb) || 0,
      });
    }

    if (isOk(r)) {
      showToast('success', editingKey ? $_('vendor_keys.key_updated') : $_('vendor_keys.key_created'));
      showKeyModal = false;
      await loadKeys();
    } else {
      showToast('error', editingKey ? $_('oauth.update_failed') : $_('oauth.create_failed'), (r as any).error?.message);
    }
  }

  async function deleteKey(id: string) {
    const r = await deleteVendorKey(id);
    if (isOk(r)) {
      showToast('success', $_('vendor_keys.key_deleted'));
      await loadKeys();
    }
  }

  onMount(() => {
    loadKeys();
  });
</script>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 flex items-center justify-between gap-3">
    <div class="flex items-center gap-2">
      <h3 class="font-medium text-gray-900 text-sm">{$_('vendor_keys.title')}</h3>
      <input
        bind:value={filterVendorId}
        onchange={loadKeys}
        placeholder={$_('vendor_keys.filter_vendor')}
        class="border border-gray-200 rounded-lg px-2 py-1 text-xs w-36" />
    </div>
    <div class="flex gap-2">
      <button
        onclick={loadKeys}
        class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg hover:bg-gray-50 text-gray-600">
        {$_('common.refresh')}
      </button>
      <button
        onclick={openCreateKey}
        class="px-3 py-1.5 bg-blue-600 text-white text-xs rounded-lg hover:bg-blue-700">
        {$_('vendor_keys.add_key')}
      </button>
    </div>
  </div>
  <table class="w-full text-sm">
    <thead>
      <tr class="text-xs text-gray-500 bg-gray-50">
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_keys.th_name')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_keys.th_id')}</th>
        <th class="px-4 py-2.5 text-left font-medium">{$_('vendor_keys.th_vendor')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('vendor_keys.th_concurrent')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('vendor_keys.th_rps')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('vendor_keys.th_max_chars')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('vendor_keys.th_max_file')}</th>
        <th class="px-4 py-2.5 text-center font-medium">{$_('vendor_keys.th_status')}</th>
        <th class="px-4 py-2.5 text-right font-medium">{$_('vendor_keys.th_actions')}</th>
      </tr>
    </thead>
    <tbody>
      {#if keysLoading}
        <tr><td colspan="9" class="px-4 py-8 text-center text-gray-400">{$_('common.loading')}</td></tr>
      {:else if vendorKeys.length === 0}
        <tr><td colspan="9" class="px-4 py-8 text-center text-gray-400">{$_('vendor_keys.no_keys')}</td></tr>
      {:else}
        {#each vendorKeys as k}
          <tr class="border-t border-gray-50 hover:bg-gray-50/50">
            <td class="px-4 py-3">
              {#if k.label}
                <span class="text-sm font-medium text-gray-800">{k.label}</span>
              {:else}
                <span class="text-xs text-gray-400 italic">{$_('common.no_name')}</span>
              {/if}
            </td>
            <td class="px-4 py-3 font-mono text-xs text-gray-500">{k.id}</td>
            <td class="px-4 py-3 text-xs text-gray-500">{k.vendor_id}</td>
            <td class="px-4 py-3 text-center text-xs">{k.max_concurrent}</td>
            <td class="px-4 py-3 text-center text-xs">{k.requests_per_second}</td>
            <td class="px-4 py-3 text-center text-xs">
              {(k.max_input_chars ?? 0) === 0 ? $_('common.no_limit') : (k.max_input_chars ?? 0).toLocaleString()}
            </td>
            <td class="px-4 py-3 text-center text-xs">
              {(k.max_file_size_mb ?? 0) === 0 ? $_('common.no_limit') : k.max_file_size_mb + ' MB'}
            </td>
            <td class="px-4 py-3 text-center">
              <span
                class="text-xs px-2 py-0.5 rounded-full {k.enabled
                  ? 'bg-green-100 text-green-700'
                  : 'bg-gray-100 text-gray-500'}">
                {k.enabled ? $_('common.enabled') : $_('common.disabled')}
              </span>
            </td>
            <td class="px-4 py-3 text-right">
              <button
                onclick={() => openEditKey(k)}
                class="text-xs text-gray-500 hover:text-gray-700 mr-2">
                {$_('common.edit')}
              </button>
              <button
                onclick={() => deleteKey(k.id)}
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

<KeyModal
  open={showKeyModal}
  {editingKey}
  {keyForm}
  {keyModalFieldId}
  {handleBackdropKeydown}
  onSave={saveKey}
  onClose={() => {
    showKeyModal = false;
  }} />
