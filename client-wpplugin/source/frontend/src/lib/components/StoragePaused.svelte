<script lang="ts">
  import { _ } from 'svelte-i18n';
  import StorageCapacity from './StorageCapacity.svelte';
</script>

<div class="space-y-4" data-testid="storage-paused-view">
  <section role="alert" class="rounded-lg border border-amber-300 bg-amber-50 p-4">
    <h2 class="text-lg font-semibold text-amber-900">{$_('settings.storage_paused_title')}</h2>
    <p class="mt-2 text-sm text-amber-900">{$_('settings.storage_paused_desc')}</p>
  </section>
  <StorageCapacity />
</div>
