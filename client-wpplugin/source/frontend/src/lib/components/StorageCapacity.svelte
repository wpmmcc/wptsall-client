<script lang="ts">
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { hasData } from '../api/client';
  import { getStorageCapacity, saveStorageCapacity, type StorageSnapshot } from '../api/storage';
  import { showToast } from '../stores/toast';

  let snapshot = $state<StorageSnapshot>();
  let maxBytes = $state('');
  let confirmed = $state(false);
  let busy = $state(false);
  let unavailable = $state(false);
  let stale = $state(false);

  async function load() {
    busy = true;
    try {
      const result = await getStorageCapacity();
      if (!hasData(result)) { unavailable = true; return; }
      snapshot = result.data;
      maxBytes = String(result.data.policy.max_physical_bytes);
      confirmed = false;
      unavailable = false;
      stale = false;
    } catch {
      unavailable = true;
    } finally {
      busy = false;
    }
  }

  async function save() {
    if (!snapshot || !confirmed || stale || busy || unavailable) return;
    const value = Number(maxBytes);
    if (!Number.isSafeInteger(value) || value <= 0) {
      showToast('error', $_('settings.storage_capacity_invalid'));
      return;
    }
    busy = true;
    try {
      const result = await saveStorageCapacity(value, snapshot.revision);
      if (!hasData(result)) {
        stale = true;
        confirmed = false;
        showToast('error', $_('settings.storage_root_save_failed'));
        return;
      }
      snapshot = result.data;
      maxBytes = String(result.data.policy.max_physical_bytes);
      confirmed = false;
      showToast('success', $_('settings.storage_root_saved'));
    } catch {
      stale = true;
      confirmed = false;
      showToast('error', $_('settings.storage_root_save_failed'));
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<section class="col-span-2 rounded-lg border border-gray-200 p-3 space-y-3">
  <h3 class="text-sm font-medium text-gray-700">{$_('settings.storage_root_title')}</h3>
  <p class="text-xs text-gray-500">{$_('settings.storage_root_desc')}</p>
  {#if snapshot}
    <p class="text-xs text-gray-500" data-testid="storage-root-inventory">
      {$_('settings.storage_root_usage', { values: {
        bytes: snapshot.physical_retained_bytes,
        files: snapshot.physical_retained_files,
        booked: snapshot.root_booked_bytes,
      } })}
    </p>
  {/if}
  {#if unavailable}
    <p role="alert" class="text-xs text-red-700">{$_('settings.storage_root_unavailable')}</p>
  {/if}
  {#if stale}
    <p role="alert" class="text-xs text-red-700">{$_('settings.storage_root_save_failed')}</p>
  {/if}
  <div>
    <label for="storage-root-max-bytes" class="block text-xs font-medium text-gray-600 mb-1">
      {$_('settings.storage_root_limit')}
    </label>
    <input id="storage-root-max-bytes" type="number" min="1" max={Number.MAX_SAFE_INTEGER} step="1"
      bind:value={maxBytes} disabled={busy || !snapshot || unavailable}
      class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
  </div>
  <label class="flex gap-2 text-xs text-gray-600">
    <input type="checkbox" bind:checked={confirmed}
      disabled={busy || !snapshot || stale || unavailable} />
    {$_('settings.storage_root_confirm')}
  </label>
  <div class="flex gap-2">
    <button type="button" onclick={save}
      disabled={busy || !snapshot || !confirmed || stale || unavailable}
      class="px-3 py-2 text-xs rounded-lg bg-blue-600 text-white disabled:opacity-50">
      {$_('settings.storage_root_save')}
    </button>
    <button type="button" onclick={load} disabled={busy}
      class="px-3 py-2 text-xs rounded-lg border border-gray-200 disabled:opacity-50">
      {$_('settings.storage_root_reload')}
    </button>
  </div>
</section>
