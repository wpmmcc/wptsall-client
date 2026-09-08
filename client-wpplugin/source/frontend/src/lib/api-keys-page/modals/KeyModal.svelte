<script lang="ts">
  import { _ } from 'svelte-i18n';
  import type { KeyFormState, VendorKeyItem } from '../types';

  export let open = false;
  export let editingKey: VendorKeyItem | null = null;
  export let keyForm: KeyFormState;
  export let keyModalFieldId: (field: string) => string;
  export let handleBackdropKeydown: (event: KeyboardEvent, close: () => void) => void;
  export let onSave: () => void | Promise<void>;
  export let onClose: () => void;
</script>

{#if open}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('key_modal.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    onkeydown={(e) => handleBackdropKeydown(e, onClose)}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-md">
      <h3 class="font-semibold text-gray-900 mb-4">
        {editingKey ? $_('key_modal.title_edit') : $_('key_modal.title_create')}
      </h3>
      <div class="space-y-3">
        {#if !editingKey}
          <input
            bind:value={keyForm.id}
            placeholder={$_('key_modal.placeholder_id')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          <input
            bind:value={keyForm.vendor_id}
            placeholder={$_('key_modal.placeholder_vendor_id')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        {/if}
        <div>
          <label for={keyModalFieldId('label')} class="block text-xs font-medium text-gray-700 mb-1">
            {$_('key_modal.label_name')}
          </label>
          <input
            id={keyModalFieldId('label')}
            bind:value={keyForm.label}
            placeholder={$_('key_modal.placeholder_name')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        </div>
        <div>
          <label for={keyModalFieldId('auth-json')} class="block text-xs text-gray-500 mb-1">
            Auth Values (JSON)
          </label>
          <textarea
            id={keyModalFieldId('auth-json')}
            bind:value={keyForm.auth_json}
            rows="3"
            placeholder={'{ "api_key": "sk-xxx" }'}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono"></textarea>
        </div>
        <div class="flex gap-2">
          <div class="flex-1">
            <label for={keyModalFieldId('max-concurrent')} class="block text-xs text-gray-500 mb-1">
              {$_('key_modal.concurrent_limit')}
            </label>
            <input
              id={keyModalFieldId('max-concurrent')}
              bind:value={keyForm.max_concurrent}
              type="number"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div class="flex-1">
            <label for={keyModalFieldId('rps')} class="block text-xs text-gray-500 mb-1">
              RPS
            </label>
            <input
              id={keyModalFieldId('rps')}
              bind:value={keyForm.requests_per_second}
              type="number"
              step="0.1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div class="flex-1">
            <label for={keyModalFieldId('weight')} class="block text-xs text-gray-500 mb-1">
              {$_('oauth.th_weight')}
            </label>
            <input
              id={keyModalFieldId('weight')}
              bind:value={keyForm.weight}
              type="number"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
        </div>
        <div class="flex gap-2">
          <div class="flex-1">
            <label for={keyModalFieldId('max-input-chars')} class="block text-xs text-gray-500 mb-1">
              {$_('key_modal.max_input_chars')}
            </label>
            <input
              id={keyModalFieldId('max-input-chars')}
              bind:value={keyForm.max_input_chars}
              type="number"
              min="0"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.no_limit_hint')}</p>
          </div>
          <div class="flex-1">
            <label for={keyModalFieldId('max-file-size-mb')} class="block text-xs text-gray-500 mb-1">
              {$_('key_modal.max_file_size')}
            </label>
            <input
              id={keyModalFieldId('max-file-size-mb')}
              bind:value={keyForm.max_file_size_mb}
              type="number"
              min="0"
              step="0.1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.no_limit_hint')}</p>
          </div>
        </div>
        <label class="flex items-center gap-2 text-sm">
          <input type="checkbox" bind:checked={keyForm.enabled} /> {$_('common.enabled')}
        </label>
      </div>
      <div class="flex gap-2 mt-5">
        <button
          onclick={onSave}
          class="flex-1 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">
          {$_('common.save')}
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
