<script lang="ts">
  import { onMount } from 'svelte';
  import { getItemContent, saveTranslated, resubmitItem, retranslateItem, approveItem, saveItemOverride, type SaveItemOverrideResult } from '../lib/api/items';
  import { getLocalComponent, listAllLocalComponents } from '../lib/api/components';
  import { getWpClientApiToastCopy } from '../lib/errors/wpClientApi';
  import { showToast } from '../lib/stores/toast';
  import type { TranslationItem } from '../lib/api/jobs';
  import type { LocalComponent } from '../lib/api/types';
  import { buildItemOverridePayload } from '../lib/review/itemOverrides';
  import { EDITABLE_OVERRIDES_MUST_BE_OBJECT } from '../lib/review/itemOverrides';
  import { _ } from 'svelte-i18n';

  let { itemId, onBack }: { itemId: number; onBack: () => void } = $props();

  // State
  let loading = $state(true);
  let saving = $state(false);
  let resubmitting = $state(false);
  let retranslating = $state(false);
  let item = $state<TranslationItem | null>(null);
  let rawData = $state<Record<string, unknown> | null>(null);
  // editedFields: user edits per key
  let editedFields = $state<Record<string, string>>({});
  let localComponents = $state<LocalComponent[]>([]);
  let selectedComponentId = $state('');
  let effectiveSourceLang = $state('');
  let effectiveTargetLang = $state('');
  let editableOverridesJson = $state('{}');
  let editablePaths = $state<string[]>([]);
  let savingOverride = $state(false);
  type AuditFieldResult = {
    field: string;
    status: string;
    content_format?: string;
    storage?: string;
    detail?: string;
    provider_component?: string;
    merge_target?: string;
    transform_stage?: string;
    fallback_reason?: string;
  };
  let auditFieldResults = $state<AuditFieldResult[]>([]);
  let auditMediaMappingCount = $state(0);

  const componentInputId = 'review-selected-component';
  const sourceLangInputId = 'review-source-lang';
  const targetLangInputId = 'review-target-lang';
  const editableOverridesInputId = 'review-editable-overrides';

  async function loadComponentOverrideContext(componentId: string) {
    editablePaths = [];
    if (!componentId) return;
    try {
      const res = await getLocalComponent(componentId);
      if (!res.success) return;
      const templateJson = (res.data.template_json ?? {}) as Record<string, unknown>;
      const editable = Array.isArray((templateJson as any).editable_params)
        ? (templateJson as any).editable_params
            .map((item: any) => typeof item?.path === 'string' ? item.path.trim() : '')
            .filter(Boolean)
        : [];
      editablePaths = editable;
    } catch {
      editablePaths = [];
    }
  }

  function applySavedOverride(saved: SaveItemOverrideResult) {
    selectedComponentId = saved.component_id ?? '';
    effectiveSourceLang = saved.source_lang ?? '';
    effectiveTargetLang = saved.target_lang ?? '';
    editableOverridesJson = JSON.stringify(saved.editable_overrides ?? {}, null, 2);
    if (!item) return;
    item.selected_component_id = saved.component_id;
    item.effective_source_lang = saved.source_lang;
    item.effective_target_lang = saved.target_lang;
    item.editable_overrides = saved.editable_overrides;
  }

  function extractEditableContent(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
    const obj = value as Record<string, unknown>;
    if (obj.payload && typeof obj.payload === 'object' && !Array.isArray(obj.payload)) {
      const payload = obj.payload as Record<string, unknown>;
      return {
        ...((payload.translated_fields as Record<string, unknown> | undefined) ?? {}),
        ...((payload.translated_meta as Record<string, unknown> | undefined) ?? {}),
      };
    }
    return obj;
  }

  function extractAuditFieldResults(value: unknown): AuditFieldResult[] {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return [];
    const obj = value as Record<string, unknown>;
    const payload =
      obj.payload && typeof obj.payload === 'object' && !Array.isArray(obj.payload)
        ? (obj.payload as Record<string, unknown>)
        : obj;
    const rows = Array.isArray(payload.field_results) ? payload.field_results : [];
    return rows
      .filter((row): row is Record<string, unknown> => !!row && typeof row === 'object' && !Array.isArray(row))
      .map((row) => ({
        field: typeof row.field === 'string' ? row.field : '',
        status: typeof row.status === 'string' ? row.status : '',
        content_format: typeof row.content_format === 'string' ? row.content_format : '',
        storage: typeof row.storage === 'string' ? row.storage : '',
        detail: typeof row.detail === 'string' ? row.detail : '',
        provider_component: typeof row.provider_component === 'string' ? row.provider_component : '',
        merge_target: typeof row.merge_target === 'string' ? row.merge_target : '',
        transform_stage: typeof row.transform_stage === 'string' ? row.transform_stage : '',
        fallback_reason: typeof row.fallback_reason === 'string' ? row.fallback_reason : '',
      }))
      .filter((row) => row.field.length > 0);
  }

  function extractAuditMediaMappingCount(value: unknown): number {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return 0;
    const obj = value as Record<string, unknown>;
    const payload =
      obj.payload && typeof obj.payload === 'object' && !Array.isArray(obj.payload)
        ? (obj.payload as Record<string, unknown>)
        : obj;
    return Array.isArray(payload.media_mappings) ? payload.media_mappings.length : 0;
  }

  function auditStatusColor(status: string): string {
    const map: Record<string, string> = {
      success: 'bg-emerald-100 text-emerald-700',
      skipped: 'bg-gray-100 text-gray-600',
      failed: 'bg-red-100 text-red-700',
    };
    return map[status] ?? 'bg-gray-100 text-gray-600';
  }

  // Load on mount
  onMount(async () => {
    loading = true;
    try {
      const compsRes = await listAllLocalComponents();
      if (compsRes.success) {
        localComponents = compsRes.data.items;
      }
      const res = await getItemContent(itemId);
      if (res.success) {
        item = res.data.item;
        rawData = extractEditableContent(res.data.raw ?? {});
        auditFieldResults = extractAuditFieldResults(res.data.translated ?? {});
        auditMediaMappingCount = extractAuditMediaMappingCount(res.data.translated ?? {});
        selectedComponentId = item.selected_component_id ?? item.component_id ?? '';
        effectiveSourceLang = item.effective_source_lang ?? item.source_lang ?? '';
        effectiveTargetLang = item.effective_target_lang ?? item.target_lang ?? '';
        editableOverridesJson = JSON.stringify(item.editable_overrides ?? {}, null, 2);
        await loadComponentOverrideContext(selectedComponentId);
        // Initialize editable fields from translated (or raw if no translated)
        const src = extractEditableContent(res.data.translated ?? res.data.raw ?? {});
        for (const [k, v] of Object.entries(src)) {
          if (typeof v === 'string') {
            editedFields[k] = v;
          } else {
            editedFields[k] = JSON.stringify(v, null, 2);
          }
        }
      } else {
        showToast('error', $_('review.load_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('login.network_error'), e.message);
    } finally {
      loading = false;
    }
  });

  async function handleSaveOverride() {
    if (!item) return;
    let payload;
    try {
      payload = buildItemOverridePayload({
        componentId: selectedComponentId,
        sourceLang: effectiveSourceLang,
        targetLang: effectiveTargetLang,
        editableOverridesJson,
      });
    } catch (e: any) {
      const message = e?.message ?? $_('common.unknown_error');
      if (message === EDITABLE_OVERRIDES_MUST_BE_OBJECT) {
        showToast('error', $_('review.override_json_error'));
      } else {
        showToast('error', $_('review.override_json_format_error'), message);
      }
      return;
    }

    savingOverride = true;
    try {
      const res = await saveItemOverride(item.id, payload);
      if (res.success) {
        applySavedOverride(res.data);
        await loadComponentOverrideContext(selectedComponentId);
        showToast('success', $_('review.override_saved'));
      } else {
        showToast('error', $_('review.override_save_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('review.override_save_failed'), e.message);
    } finally {
      savingOverride = false;
    }
  }

  // Get all keys from rawData
  let allKeys = $derived(rawData ? Object.keys(rawData) : []);

  // Determine if a key is a string field in raw
  function isStringField(key: string): boolean {
    if (!rawData) return false;
    const v = rawData[key];
    return typeof v === 'string';
  }

  function getRawDisplay(key: string): string {
    if (!rawData) return '';
    const v = rawData[key];
    if (typeof v === 'string') return v;
    return JSON.stringify(v, null, 2);
  }

  // Determine if a field is long (post_content etc.) — use textarea
  function isLongField(key: string): boolean {
    const longKeys = ['post_content', 'content', 'body', 'description', 'excerpt', 'post_excerpt'];
    if (longKeys.includes(key)) return true;
    if (!rawData) return false;
    const v = rawData[key];
    return typeof v === 'string' && v.length > 120;
  }

  async function handleSave() {
    if (!item) return;
    saving = true;
    try {
      // Build modified translated object: start from rawData, overlay edits
      const result: Record<string, unknown> = { ...(rawData ?? {}) };
      for (const [k, v] of Object.entries(editedFields)) {
        if (isStringField(k)) {
          result[k] = v;
        } else {
          try { result[k] = JSON.parse(v); } catch { result[k] = v; }
        }
      }
      const res = await saveTranslated(item.id, result);
      if (res.success) {
        showToast('success', $_('review.saved'));
      } else {
        showToast('error', $_('review.save_failed'), (res as any).error?.message);
      }
    } catch (e: any) {
      showToast('error', $_('review.save_failed'), e.message);
    } finally {
      saving = false;
    }
  }

  let approving = $state(false);

  let canEditReview = $derived(
    item != null && item.status === 'pending_review'
  );

  let canResubmit = $derived(
    item != null && ['failed', 'translated'].includes(item.status)
  );

  let canApprove = $derived(
    item != null && item.status === 'pending_review'
  );

  async function handleApprove() {
    if (!item) return;
    approving = true;
    try {
      const res = await approveItem(item.id);
      if (res.success) {
        item.status = res.data.status;
        showToast('success', $_('review.approved'));
      } else {
        const toast = getWpClientApiToastCopy($_('review.approve_failed'), (res as any).error, 'item_approve');
        showToast('error', toast.message, toast.detail);
      }
    } catch (e: any) {
      showToast('error', $_('review.approve_failed'), e.message);
    } finally {
      approving = false;
    }
  }

  async function handleResubmit() {
    if (!item) return;
    resubmitting = true;
    try {
      const res = await resubmitItem(item.id);
      if (res.success) {
        item.status = res.data.status;
        showToast('success', $_('review.resubmitted'));
      } else {
        const toast = getWpClientApiToastCopy($_('review.resubmit_failed'), (res as any).error, 'item_resubmit');
        showToast('error', toast.message, toast.detail);
      }
    } catch (e: any) {
      showToast('error', $_('review.resubmit_failed'), e.message);
    } finally {
      resubmitting = false;
    }
  }

  async function handleRetranslate() {
    if (!item) return;
    let payload;
    try {
      payload = buildItemOverridePayload({
        componentId: selectedComponentId,
        sourceLang: effectiveSourceLang,
        targetLang: effectiveTargetLang,
        editableOverridesJson,
      });
    } catch (e: any) {
      const message = e?.message ?? $_('common.unknown_error');
      if (message === EDITABLE_OVERRIDES_MUST_BE_OBJECT) {
        showToast('error', $_('review.override_json_error'));
      } else {
        showToast('error', $_('review.override_json_format_error'), message);
      }
      return;
    }

    retranslating = true;
    try {
      const saveRes = await saveItemOverride(item.id, payload);
      if (!saveRes.success) {
        showToast('error', $_('review.override_save_failed'), (saveRes as any).error?.message);
        return;
      }
      applySavedOverride(saveRes.data);
      await loadComponentOverrideContext(selectedComponentId);
      const res = await retranslateItem(item.id);
      if (res.success) {
        item.status = res.data.status;
        showToast('success', $_('review.retranslated'));
      } else {
        const toast = getWpClientApiToastCopy($_('review.retranslate_failed'), (res as any).error, 'item_retranslate');
        showToast('error', toast.message, toast.detail);
      }
    } catch (e: any) {
      showToast('error', $_('review.retranslate_failed'), e.message);
    } finally {
      retranslating = false;
    }
  }

  function itemStatusBadge(status: string): string {
    const map: Record<string, string> = {
      pending:    'bg-gray-100 text-gray-500',
      fetching:   'bg-blue-50 text-blue-500',
      fetched:    'bg-blue-100 text-blue-700',
      translating:'bg-indigo-50 text-indigo-500',
      translated: 'bg-indigo-100 text-indigo-700',
      syncing:    'bg-purple-100 text-purple-600',
      pending_review: 'bg-amber-100 text-amber-700',
      done:       'bg-emerald-100 text-emerald-700',
      failed:     'bg-red-100 text-red-600',
      skipped:    'bg-gray-100 text-gray-400',
    };
    return map[status] ?? 'bg-gray-100 text-gray-500';
  }
</script>

<div class="flex flex-col h-full">
  <!-- Header -->
  <div class="flex items-center justify-between px-6 py-4 border-b border-gray-200 bg-white shrink-0">
    <div class="flex items-center gap-3 min-w-0">
      <button
        onclick={onBack}
        class="flex items-center gap-1.5 text-sm text-gray-500 hover:text-gray-800 transition-colors shrink-0"
      >
        <svg class="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
          <path stroke-linecap="round" stroke-linejoin="round" d="M15 19l-7-7 7-7" />
        </svg>
        {$_('review.back')}
      </button>

      {#if item}
        <span class="text-gray-200">|</span>
        <span class="text-sm font-medium text-gray-700 truncate">
          {$_('review.object_prefix')} #{item.wp_object_id}
        </span>
        <span class="text-xs px-2 py-0.5 rounded-full font-medium {itemStatusBadge(item.status)}">
          {item.status}
        </span>
        <span class="text-xs px-2 py-0.5 rounded-full bg-gray-100 text-gray-600 font-mono">
          {item.task_type}
        </span>
        <span class="text-xs text-gray-400 font-mono shrink-0">
          {(item.effective_source_lang ?? item.source_lang)} → {(item.effective_target_lang ?? item.target_lang)}
        </span>
        <span class="text-xs text-gray-400 font-mono shrink-0">
          comp={(item.selected_component_id ?? item.component_id)}
        </span>
      {/if}
    </div>

    {#if item}
      <div class="flex items-center gap-3 shrink-0 ml-4">
        <span class="text-xs text-gray-400 font-mono hidden sm:block truncate max-w-[220px]" title={item.domain}>
          {item.domain.replace(/^https?:\/\//, '')}
        </span>
        <span class="text-xs text-gray-400">rel={item.relation_id}</span>
      </div>
    {/if}
  </div>

  <!-- Body -->
  <div class="flex-1 overflow-auto bg-gray-50 p-4">
    {#if loading}
      <div class="flex items-center justify-center h-48 text-sm text-gray-400">
        <svg class="animate-spin w-5 h-5 mr-2 text-gray-300" fill="none" viewBox="0 0 24 24">
          <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
          <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
        </svg>
        {$_('review.loading')}
      </div>
    {:else if allKeys.length === 0}
      <div class="bg-white rounded-xl border border-gray-200 p-10 text-center text-sm text-gray-400">
        {$_('review.no_content')}
      </div>
    {:else}
      <div class="mb-4 bg-white border border-gray-200 rounded-xl p-4 shadow-sm">
        <div class="flex items-center justify-between gap-4 mb-3">
          <div>
            <h3 class="text-sm font-semibold text-gray-800">{$_('review.override_title')}</h3>
            <p class="text-xs text-gray-400 mt-0.5">{$_('review.override_hint')}</p>
          </div>
          <button
            onclick={handleSaveOverride}
            disabled={savingOverride || !canEditReview}
            class="px-3 py-1.5 bg-slate-900 hover:bg-slate-800 disabled:bg-slate-400 text-white text-xs font-medium rounded-lg transition-colors"
          >
            {savingOverride ? $_('review.saving_override') : $_('review.save_override')}
          </button>
        </div>

        <div class="grid grid-cols-1 md:grid-cols-3 gap-3">
          <div>
            <label for={componentInputId} class="block text-xs font-medium text-gray-600 mb-1">{$_('review.component_label')}</label>
            <select
              id={componentInputId}
              bind:value={selectedComponentId}
              onchange={() => loadComponentOverrideContext(selectedComponentId)}
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm bg-white"
            >
              <option value="">{$_('review.use_default_component')}</option>
              {#each localComponents as comp}
                <option value={comp.id}>{comp.name} ({comp.id})</option>
              {/each}
            </select>
          </div>
          <div>
            <label for={sourceLangInputId} class="block text-xs font-medium text-gray-600 mb-1">{$_('review.source_lang')}</label>
            <input id={sourceLangInputId} bind:value={effectiveSourceLang} class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          </div>
          <div>
            <label for={targetLangInputId} class="block text-xs font-medium text-gray-600 mb-1">{$_('review.target_lang')}</label>
            <input id={targetLangInputId} bind:value={effectiveTargetLang} class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          </div>
        </div>

        <div class="mt-3">
          <div class="flex items-center justify-between gap-3 mb-1">
            <label for={editableOverridesInputId} class="block text-xs font-medium text-gray-600">{$_('review.editable_overrides_label')}</label>
            <span class="text-[11px] text-gray-400">{$_('review.editable_overrides_hint')}</span>
          </div>
          <textarea
            id={editableOverridesInputId}
            bind:value={editableOverridesJson}
            rows="8"
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-xs font-mono bg-white"
          ></textarea>
          {#if editablePaths.length > 0}
            <div class="mt-2 flex flex-wrap gap-1.5">
              {#each editablePaths as path}
                <span class="px-2 py-0.5 rounded-full bg-blue-50 text-blue-700 text-[11px] font-mono">{path}</span>
              {/each}
            </div>
          {:else}
            <p class="mt-2 text-[11px] text-gray-400">{$_('review.no_editable_params')}</p>
          {/if}
        </div>
      </div>

      {#if auditFieldResults.length > 0}
        <div class="mb-4 bg-white border border-gray-200 rounded-xl p-4 shadow-sm">
          <div class="flex items-start justify-between gap-4 mb-3">
            <div>
              <h3 class="text-sm font-semibold text-gray-800">{$_('review.audit_title')}</h3>
              <p class="text-xs text-gray-400 mt-0.5">{$_('review.audit_subtitle')}</p>
            </div>
            <div class="flex flex-wrap gap-2 text-[11px]">
              <span class="px-2 py-1 rounded-full bg-gray-100 text-gray-600">{$_('review.audit_fields', { values: { count: auditFieldResults.length } })}</span>
              {#if auditMediaMappingCount > 0}
                <span class="px-2 py-1 rounded-full bg-blue-50 text-blue-700">{$_('review.audit_media_mappings', { values: { count: auditMediaMappingCount } })}</span>
              {/if}
            </div>
          </div>

          <div class="overflow-x-auto">
            <table class="w-full text-xs">
              <thead>
                <tr class="text-gray-400 border-b border-gray-100">
                  <th class="text-left py-2 pr-3 font-medium">{$_('review.th_field')}</th>
                  <th class="text-left py-2 pr-3 font-medium">{$_('review.th_status')}</th>
                  <th class="text-left py-2 pr-3 font-medium">{$_('review.th_format')}</th>
                  <th class="text-left py-2 pr-3 font-medium">{$_('review.th_component')}</th>
                  <th class="text-left py-2 pr-3 font-medium">{$_('review.th_stage')}</th>
                  <th class="text-left py-2 pr-3 font-medium">{$_('review.th_merge_target')}</th>
                  <th class="text-left py-2 font-medium">{$_('review.th_detail')}</th>
                </tr>
              </thead>
              <tbody>
                {#each auditFieldResults as row (row.field + ':' + row.status + ':' + (row.transform_stage ?? ''))}
                  <tr class="border-b border-gray-50 align-top">
                    <td class="py-2 pr-3">
                      <code class="text-[11px] font-semibold text-gray-700 bg-gray-100 px-1.5 py-0.5 rounded">{row.field}</code>
                    </td>
                    <td class="py-2 pr-3">
                      <span class="px-2 py-0.5 rounded-full font-medium {auditStatusColor(row.status)}">{row.status}</span>
                    </td>
                    <td class="py-2 pr-3 text-gray-500 font-mono">{row.content_format || '-'}</td>
                    <td class="py-2 pr-3 text-gray-500 font-mono">{row.provider_component || '-'}</td>
                    <td class="py-2 pr-3 text-gray-500 font-mono">{row.transform_stage || '-'}</td>
                    <td class="py-2 pr-3 text-gray-500 font-mono">{row.merge_target || '-'}</td>
                    <td class="py-2 text-gray-500">
                      <div>{row.detail || '-'}</div>
                      {#if row.fallback_reason}
                        <div class="mt-0.5 text-[11px] text-red-500 font-mono">reason={row.fallback_reason}</div>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </div>
      {/if}

      <!-- Two-column comparison table -->
      <div class="border border-gray-200 rounded-xl overflow-hidden shadow-sm">
        <!-- Column headers -->
        <div class="grid grid-cols-2 gap-0">
          <!-- Left header -->
          <div class="bg-gray-50 border-b border-r border-gray-200 px-4 py-3 flex items-center gap-2">
            <svg class="w-4 h-4 text-gray-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
              <path stroke-linecap="round" stroke-linejoin="round" d="M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z" />
            </svg>
            <span class="text-sm font-semibold text-gray-600">{$_('review.source_text')}</span>
            <span class="text-xs text-gray-400 ml-1">{$_('review.readonly')}</span>
          </div>
          <!-- Right header -->
          <div class="bg-white border-b border-gray-200 px-4 py-3 flex items-center justify-between">
            <div class="flex items-center gap-2">
              <svg class="w-4 h-4 text-indigo-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                <path stroke-linecap="round" stroke-linejoin="round" d="M11 5H6a2 2 0 00-2 2v11a2 2 0 002 2h11a2 2 0 002-2v-5m-1.414-9.414a2 2 0 112.828 2.828L11.828 15H9v-2.828l8.586-8.586z" />
              </svg>
              <span class="text-sm font-semibold text-gray-700">{$_('review.translated_text')}</span>
              <span class="text-xs text-gray-400 ml-1">{$_('review.editable')}</span>
            </div>
            <div class="flex items-center gap-2">
              <button
                onclick={handleSave}
                disabled={saving}
                class="flex items-center gap-1.5 px-3 py-1.5 bg-indigo-600 hover:bg-indigo-700 disabled:bg-indigo-300 text-white text-xs font-medium rounded-lg transition-colors"
              >
                {#if saving}
                  <svg class="animate-spin w-3.5 h-3.5" fill="none" viewBox="0 0 24 24">
                    <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                    <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
                  </svg>
                  {$_('review.saving')}
                {:else}
                  <svg class="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                    <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
                  </svg>
                  {$_('review.save_translation')}
                {/if}
              </button>
              {#if canApprove}
                <button
                  data-testid="review-approve"
                  onclick={handleApprove}
                  disabled={approving}
                  class="flex items-center gap-1.5 px-3 py-1.5 bg-amber-600 hover:bg-amber-700 disabled:bg-amber-300 text-white text-xs font-medium rounded-lg transition-colors"
                >
                  {#if approving}
                    <svg class="animate-spin w-3.5 h-3.5" fill="none" viewBox="0 0 24 24">
                      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                      <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
                    </svg>
                    {$_('review.approving')}
                  {:else}
                    <svg class="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                      <path stroke-linecap="round" stroke-linejoin="round" d="M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z" />
                    </svg>
                    {$_('review.approve')}
                  {/if}
                </button>
              {/if}
              {#if canResubmit}
                <button
                  onclick={handleRetranslate}
                  disabled={retranslating}
                  class="flex items-center gap-1.5 px-3 py-1.5 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-300 text-white text-xs font-medium rounded-lg transition-colors"
                >
                  {#if retranslating}
                    <svg class="animate-spin w-3.5 h-3.5" fill="none" viewBox="0 0 24 24">
                      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                      <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
                    </svg>
                    {$_('review.retranslating')}
                  {:else}
                    <svg class="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                      <path stroke-linecap="round" stroke-linejoin="round" d="M12 4v4m0 8v4m8-8h-4M8 12H4m12.364 5.364l-2.828-2.828M10.464 10.464L7.636 7.636m8.728 0l-2.828 2.828M10.464 13.536l-2.828 2.828" />
                    </svg>
                    {$_('review.retranslate')}
                  {/if}
                </button>
                <button
                  onclick={handleResubmit}
                  disabled={resubmitting}
                  class="flex items-center gap-1.5 px-3 py-1.5 bg-emerald-600 hover:bg-emerald-700 disabled:bg-emerald-300 text-white text-xs font-medium rounded-lg transition-colors"
                >
                  {#if resubmitting}
                    <svg class="animate-spin w-3.5 h-3.5" fill="none" viewBox="0 0 24 24">
                      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                      <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
                    </svg>
                    {$_('review.resubmitting')}
                  {:else}
                    <svg class="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                      <path stroke-linecap="round" stroke-linejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
                    </svg>
                    {$_('review.resubmit')}
                  {/if}
                </button>
              {/if}
            </div>
          </div>
        </div>

        <!-- Field rows -->
        {#each allKeys as key, idx (key)}
          {@const isString = isStringField(key)}
          {@const isLong = isLongField(key)}
          {@const rawDisplay = getRawDisplay(key)}
          <!-- Field name row -->
          <div class="grid grid-cols-2 gap-0 border-t border-gray-200 {idx % 2 === 0 ? '' : ''}">
            <div class="col-span-2 bg-gray-50 border-b border-gray-100 px-4 py-1.5 flex items-center gap-2">
              <code class="text-xs font-semibold text-gray-600 bg-gray-100 px-1.5 py-0.5 rounded">{key}</code>
              {#if !isString}
                <span class="text-xs text-gray-400 italic">{$_('review.non_string_readonly')}</span>
              {/if}
            </div>
          </div>
          <!-- Content row -->
          <div class="grid grid-cols-2 gap-0 border-t border-gray-100">
            <!-- Left: raw (read-only) -->
            <div class="border-r border-gray-100 bg-gray-50 p-3">
              {#if isString && isLong}
                <textarea
                  readonly
                  class="w-full text-sm text-gray-600 bg-transparent resize-none border-0 outline-none leading-relaxed font-mono"
                  style="min-height: 6rem; field-sizing: content;"
                  value={rawDisplay}
                ></textarea>
              {:else if isString}
                <p class="text-sm text-gray-600 leading-relaxed whitespace-pre-wrap break-words">{rawDisplay}</p>
              {:else}
                <pre class="text-xs text-gray-500 bg-gray-100 rounded p-2 overflow-x-auto whitespace-pre-wrap break-all leading-relaxed">{rawDisplay}</pre>
              {/if}
            </div>
            <!-- Right: translated (editable for strings) -->
            <div class="bg-white p-3">
              {#if isString && isLong}
                <textarea
                  class="w-full text-sm text-gray-800 bg-transparent resize-none border border-gray-200 rounded-lg px-3 py-2 outline-none focus:border-indigo-400 focus:ring-1 focus:ring-indigo-100 leading-relaxed font-mono transition-colors"
                  style="min-height: 6rem; field-sizing: content;"
                  bind:value={editedFields[key]}
                ></textarea>
              {:else if isString}
                <input
                  type="text"
                  class="w-full text-sm text-gray-800 border border-gray-200 rounded-lg px-3 py-2 outline-none focus:border-indigo-400 focus:ring-1 focus:ring-indigo-100 transition-colors"
                  bind:value={editedFields[key]}
                />
              {:else}
                <pre class="text-xs text-gray-500 bg-gray-50 rounded p-2 overflow-x-auto whitespace-pre-wrap break-all leading-relaxed">{editedFields[key] ?? ''}</pre>
              {/if}
            </div>
          </div>
        {/each}
      </div>

      <!-- Bottom action bar -->
      <div class="mt-4 flex justify-end gap-3">
        <button
          onclick={handleSave}
          disabled={saving || !canEditReview}
          class="flex items-center gap-2 px-5 py-2.5 bg-indigo-600 hover:bg-indigo-700 disabled:bg-indigo-300 text-white text-sm font-medium rounded-lg transition-colors shadow-sm"
        >
          {#if saving}
            <svg class="animate-spin w-4 h-4" fill="none" viewBox="0 0 24 24">
              <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
              <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
            </svg>
            {$_('review.saving')}
          {:else}
            <svg class="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
              <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
            </svg>
            {$_('review.save_translation')}
          {/if}
        </button>
        {#if canResubmit}
          <button
            onclick={handleRetranslate}
            disabled={retranslating}
            class="flex items-center gap-2 px-5 py-2.5 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-300 text-white text-sm font-medium rounded-lg transition-colors shadow-sm"
          >
            {#if retranslating}
              <svg class="animate-spin w-4 h-4" fill="none" viewBox="0 0 24 24">
                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
              </svg>
              {$_('review.retranslating')}
            {:else}
              <svg class="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                <path stroke-linecap="round" stroke-linejoin="round" d="M12 4v4m0 8v4m8-8h-4M8 12H4m12.364 5.364l-2.828-2.828M10.464 10.464L7.636 7.636m8.728 0l-2.828 2.828M10.464 13.536l-2.828 2.828" />
              </svg>
              {$_('review.retranslate')}
            {/if}
          </button>
          <button
            onclick={handleResubmit}
            disabled={resubmitting}
            class="flex items-center gap-2 px-5 py-2.5 bg-emerald-600 hover:bg-emerald-700 disabled:bg-emerald-300 text-white text-sm font-medium rounded-lg transition-colors shadow-sm"
          >
            {#if resubmitting}
              <svg class="animate-spin w-4 h-4" fill="none" viewBox="0 0 24 24">
                <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
                <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v8H4z"></path>
              </svg>
              {$_('review.resubmitting')}
            {:else}
              <svg class="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                <path stroke-linecap="round" stroke-linejoin="round" d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
              </svg>
              {$_('review.resubmit')}
            {/if}
          </button>
        {/if}
      </div>
    {/if}
  </div>
</div>
