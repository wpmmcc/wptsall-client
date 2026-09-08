<script lang="ts">
  import type {
    ComponentFormState,
    LocalComp,
    OpenAiPreset,
  } from '../types';
  import { _ } from 'svelte-i18n';

  export let open = false;
  export let editingComp: LocalComp | null = null;
  export let compForm: ComponentFormState;
  export let presets: OpenAiPreset[] = [];
  export let componentModalFieldId: (field: string) => string;
  export let handleBackdropKeydown: (
    event: KeyboardEvent,
    close: () => void,
    allowSpace?: boolean
  ) => void;
  export let onApplyPreset: (preset: OpenAiPreset) => void;
  export let onSave: () => void | Promise<void>;
  export let onClose: () => void;
</script>

{#if open}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('comp_modal.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    onkeydown={(e) => handleBackdropKeydown(e, onClose)}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-md">
      <h3 class="font-semibold text-gray-900 mb-4">
        {editingComp ? $_('comp_modal.title_edit') : $_('comp_modal.title_create')}
      </h3>
      <div class="space-y-3">
        {#if !editingComp}
          <label for={componentModalFieldId('id')} class="block text-xs text-gray-500 mb-1">
            {$_('comp_modal.label_id')}
          </label>
          <input
            id={componentModalFieldId('id')}
            bind:value={compForm.id}
            placeholder={$_('comp_modal.placeholder_id')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
        {/if}
        <label for={componentModalFieldId('name')} class="block text-xs text-gray-500 mb-1">
          {$_('comp_modal.label_name')}
        </label>
        <input
          id={componentModalFieldId('name')}
          bind:value={compForm.name}
          placeholder={$_('comp_modal.placeholder_name')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        {#if !editingComp}
          <label for={componentModalFieldId('kind')} class="block text-xs text-gray-500 mb-1">
            {$_('comp_modal.label_kind')}
          </label>
          <select
            id={componentModalFieldId('kind')}
            bind:value={compForm.kind}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm">
            {#each ['text', 'image', 'video', 'audio', 'document', 'openai_compatible'] as k}
              <option value={k}>
                {k === 'openai_compatible' ? $_('comp_modal.openai_compat') : k}
              </option>
            {/each}
          </select>
        {/if}
        {#if (editingComp ? editingComp.kind : compForm.kind) === 'openai_compatible'}
          <div>
            <div class="text-xs text-gray-500">
              {$_('comp_modal.inline_template_hint')}
            </div>
          </div>
          <div>
            <div class="block text-xs text-gray-500 mb-1">{$_('comp_modal.quick_select')}</div>
            <div class="flex flex-wrap gap-1.5">
              {#each presets as preset}
                <button
                  type="button"
                  onclick={() => onApplyPreset(preset)}
                  class="text-xs px-2 py-1 border border-gray-200 rounded-full hover:bg-blue-50 hover:border-blue-300 transition-colors">
                  {preset.label}
                </button>
              {/each}
            </div>
          </div>
          <div>
            <label for={componentModalFieldId('api-base')} class="block text-xs text-gray-500 mb-1">
              {$_('comp_modal.api_base')}
            </label>
            <input
              id={componentModalFieldId('api-base')}
              bind:value={compForm.api_base}
              placeholder="https://api.deepseek.com"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          </div>
          <div>
            <label for={componentModalFieldId('model')} class="block text-xs text-gray-500 mb-1">
              {$_('comp_modal.model_name')}
            </label>
            <input
              id={componentModalFieldId('model')}
              bind:value={compForm.model}
              placeholder="deepseek-chat"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          </div>
          <div>
            <label for={componentModalFieldId('system-prompt')} class="block text-xs text-gray-500 mb-1">
              {$_('comp_modal.system_prompt')}
            </label>
            <textarea
              id={componentModalFieldId('system-prompt')}
              bind:value={compForm.system_prompt}
              rows={3}
              placeholder={$_('comp_modal.placeholder_system_prompt')}
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm"></textarea>
          </div>
          <div class="flex gap-2">
            <div class="flex-1">
              <label for={componentModalFieldId('temperature')} class="block text-xs text-gray-500 mb-1">
                {$_('comp_modal.temperature')}
              </label>
              <input
                id={componentModalFieldId('temperature')}
                bind:value={compForm.temperature}
                type="number"
                step="0.1"
                min="0"
                max="2"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            </div>
            <div class="flex-1">
              <label for={componentModalFieldId('max-tokens')} class="block text-xs text-gray-500 mb-1">
                {$_('comp_modal.max_tokens')}
              </label>
              <input
                id={componentModalFieldId('max-tokens')}
                bind:value={compForm.max_tokens}
                type="number"
                placeholder={$_('comp_modal.placeholder_no_limit')}
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            </div>
          </div>
          <div>
            <label for={componentModalFieldId('response-path')} class="block text-xs text-gray-500 mb-1">
              {$_('comp_modal.response_path')}
            </label>
            <input
              id={componentModalFieldId('response-path')}
              bind:value={compForm.response_path}
              placeholder="choices.0.message.content"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          </div>
        {:else}
          <label for={componentModalFieldId('template-id')} class="block text-xs text-gray-500 mb-1">
            {$_('comp_modal.server_template_id')}
          </label>
          <input
            id={componentModalFieldId('template-id')}
            bind:value={compForm.template_id}
            placeholder={$_('comp_modal.placeholder_template_id')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          <div class="flex gap-2">
            <div class="flex-1">
              <label for={componentModalFieldId('vendor-id')} class="block text-xs text-gray-500 mb-1">
                {$_('comp_modal.label_vendor_id')}
              </label>
              <input
                id={componentModalFieldId('vendor-id')}
                bind:value={compForm.vendor_id}
                placeholder={$_('comp_modal.placeholder_vendor_id')}
                class="flex-1 w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            </div>
            <div class="flex-1">
              <label for={componentModalFieldId('vendor-name')} class="block text-xs text-gray-500 mb-1">
                {$_('comp_modal.label_vendor_name')}
              </label>
              <input
                id={componentModalFieldId('vendor-name')}
                bind:value={compForm.vendor_name}
                placeholder={$_('comp_modal.placeholder_vendor_name')}
                class="flex-1 w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            </div>
          </div>
        {/if}
        <label for={componentModalFieldId('remarks')} class="block text-xs text-gray-500 mb-1">
          {$_('comp_modal.label_remarks')}
        </label>
        <input
          id={componentModalFieldId('remarks')}
          bind:value={compForm.remarks}
          placeholder={$_('comp_modal.placeholder_remarks')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
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
