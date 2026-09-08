<script lang="ts">
  import type {
    LocalComp,
    VersionFormState,
  } from '../types';
  import { _ } from 'svelte-i18n';

  export let open = false;
  export let component: LocalComp | null = null;
  export let editingVersion: { compId: string; ver: string } | null = null;
  export let versionForm: VersionFormState;
  export let versionModalFieldId: (compId: string, field: string) => string;
  export let handleBackdropKeydown: (
    event: KeyboardEvent,
    close: () => void,
    allowSpace?: boolean
  ) => void;
  export let onSave: (compId: string) => void | Promise<void>;
  export let onClose: () => void;
</script>

{#if open && component}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('version_modal.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    onkeydown={(e) => handleBackdropKeydown(e, onClose)}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-sm">
      <h4 class="font-semibold text-gray-900 mb-4">
        {editingVersion ? `${$_('version_modal.title_edit')} — ${component.name}` : `${$_('version_modal.title_add')} — ${component.name}`}
      </h4>
      <div class="space-y-3">
        {#if !editingVersion}
          <label for={versionModalFieldId(component.id, 'version')} class="block text-xs text-gray-500 mb-1">
            {$_('version_modal.label_version')}
          </label>
          <input
            id={versionModalFieldId(component.id, 'version')}
            bind:value={versionForm.version}
            placeholder={$_('version_modal.placeholder_version')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        {:else}
          <div class="px-3 py-2 bg-gray-50 rounded-lg border border-gray-200 text-sm font-mono text-gray-500">
            {$_('version_modal.version_display')} {editingVersion.ver}
          </div>
        {/if}
        <label for={versionModalFieldId(component.id, 'key-ids')} class="block text-xs text-gray-500 mb-1">
          Key IDs
        </label>
        <input
          id={versionModalFieldId(component.id, 'key-ids')}
          bind:value={versionForm.key_ids}
          placeholder={$_('version_modal.placeholder_key_ids')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
        <div class="flex gap-2">
          <div class="flex-1">
            <label for={versionModalFieldId(component.id, 'strategy')} class="block text-xs text-gray-500 mb-1">
              {$_('version_modal.strategy')}
            </label>
            <select
              id={versionModalFieldId(component.id, 'strategy')}
              bind:value={versionForm.key_selection_strategy}
              class="flex-1 w-full border border-gray-200 rounded-lg px-3 py-2 text-sm">
              <option value="round_robin">round_robin</option>
              <option value="random">random</option>
              <option value="weighted">weighted</option>
            </select>
          </div>
          <div class="flex-1">
            <label for={versionModalFieldId(component.id, 'auth-type')} class="block text-xs text-gray-500 mb-1">
              {$_('version_modal.auth_type')}
            </label>
            <select
              id={versionModalFieldId(component.id, 'auth-type')}
              bind:value={versionForm.auth_type}
              class="flex-1 w-full border border-gray-200 rounded-lg px-3 py-2 text-sm">
              <option value="key">key</option>
              <option value="oauth">oauth</option>
            </select>
          </div>
        </div>
        <label for={versionModalFieldId(component.id, 'proxy-profile')} class="block text-xs text-gray-500 mb-1">
          {$_('version_modal.proxy_profile')}
        </label>
        <input
          id={versionModalFieldId(component.id, 'proxy-profile')}
          bind:value={versionForm.proxy_profile_id}
          placeholder={$_('version_modal.placeholder_proxy')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
      </div>
      <div class="flex gap-2 mt-4">
        <button
          onclick={() => onSave(component.id)}
          class="flex-1 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">
          {editingVersion ? $_('version_modal.update') : $_('common.save')}
        </button>
        <button
          onclick={onClose}
          class="flex-1 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50">
          {$_('common.cancel')}
        </button>
      </div>
    </div>
  </div>
{/if}
