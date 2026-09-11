<script lang="ts">
  import type { FileTestModalState } from '../types';
  import { _ } from 'svelte-i18n';

  export let testFileModal: FileTestModalState;
  export let testFileFieldId: (field: string) => string;
  export let handleBackdropKeydown: (
    event: KeyboardEvent,
    close: () => void,
    allowSpace?: boolean
  ) => void;
  export let onRun: () => void | Promise<void>;
  export let onClose: () => void;
</script>

{#if testFileModal.open}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('file_test.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget && !testFileModal.loading) onClose();
    }}
    onkeydown={(e) => {
      if (!testFileModal.loading) handleBackdropKeydown(e, onClose);
    }}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-md">
      <h3 class="font-semibold text-gray-900 mb-1">{$_('file_test.title')}</h3>
      <p class="text-xs text-gray-500 mb-4">{testFileModal.componentName}</p>
      <div class="space-y-3">
        <div>
          <label for={testFileFieldId('url')} class="block text-xs text-gray-500 mb-1">
            {$_('file_test.file_url')}
          </label>
          <input
            id={testFileFieldId('url')}
            bind:value={testFileModal.fileUrl}
            placeholder="http://127.0.0.1:9090/api/v1/test-file/1024"
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
        </div>
        <div class="flex gap-2">
          <div class="flex-1">
            <label for={testFileFieldId('source')} class="block text-xs text-gray-500 mb-1">
              {$_('file_test.source_lang')}
            </label>
            <input
              id={testFileFieldId('source')}
              bind:value={testFileModal.sourceLang}
              placeholder="en"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div class="flex-1">
            <label for={testFileFieldId('target')} class="block text-xs text-gray-500 mb-1">
              {$_('file_test.target_lang')}
            </label>
            <input
              id={testFileFieldId('target')}
              bind:value={testFileModal.targetLang}
              placeholder="zh-CN"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
        </div>
      </div>

      {#if testFileModal.result}
        <div class="mt-4 p-3 bg-green-50 border border-green-200 rounded-lg">
          <div class="text-xs font-medium text-green-700 mb-2">
            {$_('file_test.success', { values: { elapsed_ms: testFileModal.result.elapsed_ms } })}
          </div>
          {#if testFileModal.result.translated_ref}
            <div class="text-xs text-green-600 mb-1">
              <span class="font-medium">translated_ref:</span>
              <span class="font-mono break-all">{testFileModal.result.translated_ref}</span>
            </div>
          {/if}
          {#if testFileModal.result.translated_text}
            <div class="text-xs text-green-600">
              <span class="font-medium">translated_text:</span>
              <span>{testFileModal.result.translated_text}</span>
            </div>
          {/if}
        </div>
      {/if}

      {#if testFileModal.error}
        <div class="mt-4 p-3 bg-red-50 border border-red-200 rounded-lg">
          <div class="text-xs font-medium text-red-700 mb-1">{$_('file_test.failed')}</div>
          <div class="text-xs text-red-600 break-all">{testFileModal.error}</div>
        </div>
      {/if}

      <div class="flex gap-2 mt-5">
        <button
          onclick={onRun}
          disabled={testFileModal.loading}
          class="flex-1 py-2 bg-emerald-600 text-white text-sm rounded-lg hover:bg-emerald-700 disabled:opacity-50 disabled:cursor-not-allowed">
          {testFileModal.loading ? $_('file_test.running') : $_('file_test.run')}
        </button>
        <button
          onclick={onClose}
          disabled={testFileModal.loading}
          class="flex-1 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50 disabled:opacity-50">
          {$_('common.cancel')}
        </button>
      </div>
    </div>
  </div>
{/if}
