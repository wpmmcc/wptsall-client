<script lang="ts">
  import { _ } from 'svelte-i18n';
  import {
    exportIntegrationPack,
    importIntegrationPack,
    previewIntegrationPack,
  } from '../api/keys';

  type ExportMode = 'public' | 'private';
  type JsonObject = Record<string, unknown>;

  let exportMode = $state<ExportMode>('public');
  let exportPassphrase = $state('');
  let confirmPrivate = $state(false);
  let exportBusy = $state(false);
  let exportError = $state('');
  let exportNotice = $state('');
  let exportedJson = $state('');

  let importJson = $state('');
  let importPassphrase = $state('');
  let overwrite = $state(false);
  let previewBusy = $state(false);
  let importBusy = $state(false);
  let importError = $state('');
  let importNotice = $state('');
  let preview = $state<JsonObject | null>(null);
  let previewInput = $state('');

  function errorMessage(error: unknown, fallback: string): string {
    return typeof error === 'string' && error.trim() ? error.trim() : fallback;
  }

  function parsePack(): JsonObject | null {
    if (!importJson.trim()) {
      importError = $_('integration_pack.nothing_to_import');
      return null;
    }
    try {
      const parsed: unknown = JSON.parse(importJson);
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
        throw new Error($_('integration_pack.invalid_json'));
      }
      return parsed as JsonObject;
    } catch (error) {
      importError = errorMessage(error, $_('integration_pack.invalid_json'));
      return null;
    }
  }

  function packWithPassphrase(pack: JsonObject): JsonObject {
    const passphrase = importPassphrase.trim();
    return passphrase ? { ...pack, passphrase } : pack;
  }

  async function exportPack() {
    exportBusy = true;
    exportError = '';
    exportNotice = '';
    try {
      if (exportMode === 'private') {
        if (!confirmPrivate) {
          exportError = $_('integration_pack.confirm_private_required');
          return;
        }
        if (exportPassphrase.trim().length < 8) {
          exportError = $_('integration_pack.passphrase_required');
          return;
        }
      }
      const result = await exportIntegrationPack({
        mode: exportMode,
        ...(exportMode === 'private'
          ? { confirm: true, passphrase: exportPassphrase }
          : {}),
      });
      if (!result.success) {
        exportError = errorMessage(result.error?.message, $_('integration_pack.export_failed'));
        return;
      }
      exportedJson = JSON.stringify(result.data, null, 2);
      exportNotice = $_('integration_pack.export_success');
      if (exportMode === 'private') exportPassphrase = '';
    } catch (error) {
      exportError = errorMessage(error, $_('integration_pack.export_failed'));
    } finally {
      exportBusy = false;
    }
  }

  function downloadExport() {
    if (!exportedJson || typeof document === 'undefined') return;
    const blob = new Blob([exportedJson], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `wptsall-integration-pack-${exportMode}.json`;
    link.click();
    URL.revokeObjectURL(url);
  }

  async function previewPack() {
    const pack = parsePack();
    if (!pack) return;
    previewBusy = true;
    importError = '';
    importNotice = '';
    preview = null;
    try {
      const result = await previewIntegrationPack(packWithPassphrase(pack));
      if (!result.success) {
        importError = errorMessage(result.error?.message, $_('integration_pack.preview_failed'));
        return;
      }
      preview = result.data;
      previewInput = importJson;
      importNotice = $_('integration_pack.preview_ready');
    } catch (error) {
      importError = errorMessage(error, $_('integration_pack.preview_failed'));
    } finally {
      previewBusy = false;
    }
  }

  async function importPack() {
    const pack = parsePack();
    if (!pack) return;
    if (previewInput !== importJson || !preview) {
      await previewPack();
      if (!preview || previewInput !== importJson) return;
    }
    if (preview.safe_to_import !== true) {
      importError = $_('integration_pack.unsafe_import');
      return;
    }

    importBusy = true;
    importError = '';
    importNotice = '';
    try {
      const result = await importIntegrationPack(packWithPassphrase(pack), overwrite);
      if (!result.success) {
        importError = errorMessage(result.error?.message, $_('integration_pack.import_failed'));
        return;
      }
      importNotice = $_('integration_pack.import_success');
      preview = null;
      previewInput = '';
      importJson = '';
      importPassphrase = '';
    } catch (error) {
      importError = errorMessage(error, $_('integration_pack.import_failed'));
    } finally {
      importBusy = false;
    }
  }

  function countPreview(key: string): number {
    const value = preview?.[key];
    return Array.isArray(value) ? value.length : 0;
  }
</script>

<div class="space-y-4">
  <section class="bg-white border border-gray-200 rounded-xl overflow-hidden">
    <div class="px-5 py-4 border-b border-gray-100">
      <h3 class="font-medium text-gray-900 text-sm">{$_('integration_pack.title')}</h3>
      <p class="text-xs text-gray-500 mt-1">{$_('integration_pack.subtitle')}</p>
    </div>

    <div class="p-5 space-y-4">
      <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <div class="border border-gray-100 rounded-lg p-4 space-y-3">
          <h4 class="text-sm font-medium text-gray-800">{$_('integration_pack.export_title')}</h4>
          <label class="block text-xs text-gray-600">
            {$_('integration_pack.export_mode')}
            <select bind:value={exportMode} class="mt-1 w-full text-sm border border-gray-200 rounded-lg px-3 py-2">
              <option value="public">{$_('integration_pack.public_mode')}</option>
              <option value="private">{$_('integration_pack.private_mode')}</option>
            </select>
          </label>
          {#if exportMode === 'private'}
            <label class="block text-xs text-gray-600">
              {$_('integration_pack.passphrase')}
              <input
                bind:value={exportPassphrase}
                type="password"
                autocomplete="new-password"
                class="mt-1 w-full text-sm border border-gray-200 rounded-lg px-3 py-2"
                placeholder={$_('integration_pack.passphrase_placeholder')} />
            </label>
            <label class="flex items-start gap-2 text-xs text-gray-600">
              <input bind:checked={confirmPrivate} type="checkbox" class="mt-0.5" />
              <span>{$_('integration_pack.confirm_private')}</span>
            </label>
            <p class="text-[11px] text-amber-700">{$_('integration_pack.private_hint')}</p>
          {:else}
            <p class="text-[11px] text-gray-500">{$_('integration_pack.public_hint')}</p>
          {/if}
          <div class="flex gap-2">
            <button
              data-testid="pack-export"
              onclick={exportPack}
              disabled={exportBusy}
              class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
              {exportBusy ? $_('common.loading') : $_('integration_pack.export')}
            </button>
            <button
              onclick={downloadExport}
              disabled={!exportedJson}
              class="text-xs border border-gray-200 text-gray-700 px-3 py-1.5 rounded-lg hover:bg-gray-50 disabled:opacity-50">
              {$_('integration_pack.download')}
            </button>
          </div>
          {#if exportError}<p class="text-xs text-red-700">{exportError}</p>{/if}
          {#if exportNotice}<p class="text-xs text-green-700">{exportNotice}</p>{/if}
          {#if exportedJson}
            <textarea readonly value={exportedJson} class="w-full min-h-40 font-mono text-[11px] border border-gray-200 rounded-lg p-2"></textarea>
          {/if}
        </div>

        <div class="border border-gray-100 rounded-lg p-4 space-y-3">
          <h4 class="text-sm font-medium text-gray-800">{$_('integration_pack.import_title')}</h4>
          <p class="text-[11px] text-gray-500">{$_('integration_pack.import_hint')}</p>
          <textarea
            bind:value={importJson}
            class="w-full min-h-40 font-mono text-[11px] border border-gray-200 rounded-lg p-2"
            placeholder={$_('integration_pack.paste_placeholder')}></textarea>
          <input
            bind:value={importPassphrase}
            type="password"
            autocomplete="off"
            class="w-full text-sm border border-gray-200 rounded-lg px-3 py-2"
            placeholder={$_('integration_pack.import_passphrase')} />
          <label class="flex items-center gap-2 text-xs text-gray-600">
            <input bind:checked={overwrite} type="checkbox" />
            {$_('integration_pack.overwrite')}
          </label>
          <div class="flex gap-2">
            <button
              data-testid="pack-preview"
              onclick={previewPack}
              disabled={previewBusy || !importJson.trim()}
              class="text-xs border border-gray-200 text-gray-700 px-3 py-1.5 rounded-lg hover:bg-gray-50 disabled:opacity-50">
              {previewBusy ? $_('common.loading') : $_('integration_pack.preview')}
            </button>
            <button
              data-testid="pack-import"
              onclick={importPack}
              disabled={importBusy || previewBusy || !importJson.trim()}
              class="text-xs bg-blue-600 text-white px-3 py-1.5 rounded-lg hover:bg-blue-700 disabled:opacity-50">
              {importBusy ? $_('common.loading') : $_('integration_pack.import')}
            </button>
          </div>
          {#if importError}<p class="text-xs text-red-700">{importError}</p>{/if}
          {#if importNotice}<p class="text-xs text-green-700">{importNotice}</p>{/if}
          {#if preview}
            <div class="border border-gray-100 rounded-lg p-3 text-xs space-y-1">
              <p class={preview.safe_to_import === true ? 'text-green-700' : 'text-red-700'}>
                {preview.safe_to_import === true ? $_('integration_pack.safe') : $_('integration_pack.unsafe')}
              </p>
              <p>{$_('integration_pack.new_components')}: {countPreview('new_components')}</p>
              <p>{$_('integration_pack.overwrite_components')}: {countPreview('overwrite_components')}</p>
              <p>{$_('integration_pack.binding_conflicts')}: {countPreview('component_binding_conflicts') + countPreview('task_binding_conflicts') + countPreview('rule_binding_conflicts')}</p>
              <p>{$_('integration_pack.blocked_urls')}: {Number(preview.blocked_provider_url_count ?? 0)}</p>
              <p>{$_('integration_pack.missing_refs')}: {countPreview('missing_component_refs')}</p>
              {#if preview.workflow_valid === false}<p class="text-red-700">{$_('integration_pack.workflow_invalid')}</p>{/if}
            </div>
          {/if}
        </div>
      </div>
    </div>
  </section>
</div>
