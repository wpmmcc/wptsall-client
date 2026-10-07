<script lang="ts">
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { listProviderOperations, reconcileProviderOperation, type ProviderOperation } from '../api/providerRecovery';

  let rows = $state<ProviderOperation[]>([]);
  let error = $state('');
  let busy = $state(false);
  let selected = $state<ProviderOperation | null>(null);

  async function load() {
    busy = true;
    error = '';
    selected = null;
    try {
      const result = await listProviderOperations();
      if (result.success) rows = result.data.items;
      else error = result.error.message;
    } catch {
      error = $_('provider_recovery.read_failed');
    } finally {
      busy = false;
    }
  }

  async function reconcile() {
    if (!selected || !selected.can_reconcile || busy) return;
    busy = true;
    error = '';
    try {
      const result = await reconcileProviderOperation(selected.operation_id);
      if (result.success) await load();
      else error = result.error.message;
    } catch {
      error = $_('provider_recovery.verify_failed');
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<section aria-label={$_('tasks.tab_provider_recovery')}>
  <p class="text-sm text-gray-600 mb-4">{$_('provider_recovery.explanation')}</p>
  <button class="px-3 py-2 rounded border mb-4" onclick={load} disabled={busy}>
    {$_('provider_recovery.refresh')}
  </button>
  {#if error}<p role="alert" class="text-red-700 mb-4">{error}</p>{/if}
  {#if !busy && !error && rows.length === 0}<p>{$_('provider_recovery.empty')}</p>{/if}
  <ul class="space-y-3">
    {#each rows as row (row.operation_id)}
      <li class="border rounded p-3">
        <p class="break-all">{row.site ?? $_('provider_recovery.site_unavailable')} · {row.operation_id}</p>
        <p class="break-all">{row.component_id} · {row.object_type}:{row.object_id} · {row.field_name}</p>
        <p>{row.source_lang} → {row.target_lang} · {row.state}</p>
        {#if row.state === 'submit_unknown'}
          {#if row.can_reconcile}
            <button class="px-3 py-2 rounded border mt-2" disabled={busy}
              onclick={() => { selected = row; error = ''; }}>
              {$_('provider_recovery.review')}
            </button>
          {:else}
            <p class="text-sm text-gray-600 mt-2">{$_('provider_recovery.unsupported')}</p>
          {/if}
        {/if}
      </li>
    {/each}
  </ul>
  {#if selected}
    <div class="border rounded p-4 mt-4">
      <p class="break-all">{selected.operation_id}</p>
      <p class="text-sm my-2">{$_('provider_recovery.confirmation')}</p>
      <div class="flex flex-wrap gap-2 mt-3">
        <button class="border rounded px-3 py-2" onclick={reconcile} disabled={busy}>
          {$_('provider_recovery.verify')}
        </button>
        <button class="border rounded px-3 py-2" onclick={() => selected = null} disabled={busy}>
          {$_('provider_recovery.cancel')}
        </button>
      </div>
    </div>
  {/if}
</section>
