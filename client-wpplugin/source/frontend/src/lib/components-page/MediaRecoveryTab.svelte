<script lang="ts">
  import { onMount } from 'svelte';
  import { _ } from 'svelte-i18n';
  import { listMediaOperations, reconcileMediaOperation, type MediaOperation } from '../api/mediaRecovery';

  let rows = $state<MediaOperation[]>([]);
  let error = $state('');
  let busy = $state(false);
  let selected = $state<MediaOperation | null>(null);
  let candidate = $state('');

  async function load() {
    busy = true;
    error = '';
    try {
      const result = await listMediaOperations();
      if (result.success) rows = result.data.items;
      else error = result.error.message;
    } catch {
      error = $_('media_recovery.read_failed');
    } finally {
      busy = false;
    }
  }

  async function reconcile() {
    if (!selected) return;
    const id = candidate.trim() ? Number(candidate) : undefined;
    if (id !== undefined && (!Number.isSafeInteger(id) || id <= 0)) {
      error = $_('media_recovery.invalid_id');
      return;
    }
    busy = true;
    error = '';
    try {
      const result = await reconcileMediaOperation(selected.operation_id, id);
      if (result.success) {
        selected = null;
        candidate = '';
        await load();
      } else error = result.error.message;
    } catch {
      error = $_('media_recovery.verify_failed');
    } finally {
      busy = false;
    }
  }

  onMount(load);
</script>

<section aria-label={$_('tasks.tab_media_recovery')}>
  <p class="text-sm text-gray-600 mb-4">{$_('media_recovery.explanation')}</p>
  <button class="px-3 py-2 rounded border mb-4" onclick={load} disabled={busy}>
    {$_('media_recovery.refresh')}
  </button>
  {#if error}<p role="alert" class="text-red-700 mb-4">{error}</p>{/if}
  {#if !busy && !error && rows.length === 0}<p>{$_('media_recovery.empty')}</p>{/if}
  <ul class="space-y-3">
    {#each rows as row (row.operation_id)}
      <li class="border rounded p-3">
        <p class="break-all">{row.site} · {row.operation_id}</p>
        <p>{$_('media_recovery.source')} {row.source_id} · {row.state}
          {#if row.attachment_id} · {$_('media_recovery.attachment')} {row.attachment_id}{/if}</p>
        {#if row.state !== 'result_ready'}
          <button class="px-3 py-2 rounded border mt-2" disabled={busy}
            onclick={() => { selected = row; candidate = ''; error = ''; }}>
            {$_('media_recovery.review')}
          </button>
        {/if}
      </li>
    {/each}
  </ul>
  {#if selected}
    <div class="border rounded p-4 mt-4">
      <p class="break-all">{selected.operation_id}</p>
      <p class="text-sm my-2">{$_('media_recovery.confirmation')}</p>
      <label for="media-recovery-candidate">{$_('media_recovery.candidate')}</label>
      <input id="media-recovery-candidate" class="border rounded px-3 py-2 ml-2"
        bind:value={candidate} inputmode="numeric" disabled={busy} />
      <div class="flex gap-2 mt-3">
        <button class="border rounded px-3 py-2" onclick={reconcile} disabled={busy}>
          {$_('media_recovery.verify')}
        </button>
        <button class="border rounded px-3 py-2" onclick={() => selected = null} disabled={busy}>
          {$_('media_recovery.cancel')}
        </button>
      </div>
    </div>
  {/if}
</section>
