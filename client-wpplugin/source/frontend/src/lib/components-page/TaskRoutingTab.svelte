<script lang="ts">
  import { onMount } from 'svelte';
  import {
    deleteRuleBinding,
    deleteTaskTypeBinding,
    getRuleBindingDiscovery,
    listAllLocalComponents,
    upsertRuleBinding,
    upsertTaskTypeBinding,
  } from '../api/components';
  import { status, fetchStatus } from '../stores/status';
  import { showToast } from '../stores/toast';
  import {
    getRuleScopePlaceholder,
    normalizeRuleScopeKey,
    slotSupportsKind,
    type RuleBindingScope,
    validateRuleScopeKey,
  } from '../rule-bindings';
  import {
    formatBusinessLineLabel,
    formatCapabilityLabel,
    formatDeliveryTargetLabel,
    formatRoutingProfileLabel,
    formatRuleSlotLabel,
    formatSourceGroupLabel,
    formatSourceRoleLabel,
    ruleBindFieldId,
  } from './helpers';
  import { _ } from 'svelte-i18n';

  function fieldActionLabel(action: string): string {
    if (action === 'copy') return $_('task_routing.field_action_copy');
    if (action === 'skip') return $_('task_routing.field_action_skip');
    return $_('task_routing.field_action_translate');
  }
  import type {
    LocalComp,
    RuleDiscoveryIssue,
    RuleDiscoveryItem,
    RuleBinding,
    RuleSlotOption,
    ServerComp,
    TaskTypeComponentBinding,
  } from './types';

  const RULE_SLOT_OPTIONS: RuleSlotOption[] = [
    { key: 'plain_text', label: 'plain_text', hint: 'task_routing.slot_hint.plain_text' },
    { key: 'rich_html', label: 'rich_html', hint: 'task_routing.slot_hint.rich_html' },
    { key: 'json_structured', label: 'json_structured', hint: 'task_routing.slot_hint.json_structured' },
    { key: 'serialized_php', label: 'serialized_php', hint: 'task_routing.slot_hint.serialized_php' },
    { key: 'media_ref', label: 'media_ref', hint: 'task_routing.slot_hint.media_ref' },
    { key: 'media_ref:image', label: 'media_ref:image', hint: 'task_routing.slot_hint.media_ref_image' },
    { key: 'media_ref:video', label: 'media_ref:video', hint: 'task_routing.slot_hint.media_ref_video' },
    { key: 'media_ref:audio', label: 'media_ref:audio', hint: 'task_routing.slot_hint.media_ref_audio' },
    { key: 'media_ref:document', label: 'media_ref:document', hint: 'task_routing.slot_hint.media_ref_document' },
    { key: 'slug', label: 'slug', hint: 'task_routing.slot_hint.slug' },
    { key: 'code', label: 'code', hint: 'task_routing.slot_hint.code' },
  ];

  const SLOT_QUICK_PICKS = [
    { key: 'plain_text', title: 'task_routing.quick.plain_text_title', hint: 'task_routing.quick.plain_text_hint' },
    { key: 'rich_html', title: 'task_routing.quick.rich_html_title', hint: 'task_routing.quick.rich_html_hint' },
    { key: 'json_structured', title: 'task_routing.quick.json_title', hint: 'task_routing.quick.json_hint' },
    { key: 'serialized_php', title: 'task_routing.quick.serialized_title', hint: 'task_routing.quick.serialized_hint' },
    { key: 'code', title: 'task_routing.quick.code_title', hint: 'task_routing.quick.code_hint' },
  ] as const;

  const RULE_SCOPE_EXAMPLES: Record<RuleBindingScope, string> = {
    global: 'task_routing.scope_example.global',
    plugin: 'task_routing.scope_example.plugin',
    relation: 'task_routing.scope_example.relation',
    rule: 'task_routing.scope_example.rule',
  };

  // Task-type component bindings: business_line × task_type → component.
  // The worker's component selector resolves these before per-rule slots
  // (e.g. config_i18n:text is what content translation tasks query).
  const TASK_TYPE_OPTIONS = ['text', 'image', 'video', 'audio', 'document', 'mixed'] as const;
  const BUSINESS_LINE_OPTIONS = [
    'post_content',
    'taxonomy_content',
    'plugin_i18n',
    'config_i18n',
    'theme_i18n',
    'site_strings',
    'menu_strings',
    'widget_strings',
    'custom_model',
  ] as const;

  const SLOT_USAGE_HINTS: Record<string, string> = {
    plain_text: 'task_routing.slot_usage.plain_text',
    rich_html: 'task_routing.slot_usage.rich_html',
    json_structured: 'task_routing.slot_usage.json_structured',
    serialized_php: 'task_routing.slot_usage.serialized_php',
    code: 'task_routing.slot_usage.code',
    slug: 'task_routing.slot_usage.slug',
    'media_ref:image': 'task_routing.slot_usage.media_ref_image',
    'media_ref:video': 'task_routing.slot_usage.media_ref_video',
    'media_ref:audio': 'task_routing.slot_usage.media_ref_audio',
    'media_ref:document': 'task_routing.slot_usage.media_ref_document',
  };

  let localComps = $state<LocalComp[]>([]);
  let ruleDiscoveryLoading = $state(false);
  let ruleDiscoveryItems = $state<RuleDiscoveryItem[]>([]);
  let ruleDiscoveryIssues = $state<RuleDiscoveryIssue[]>([]);
  let ruleForm = $state({
    scope: 'global' as RuleBindingScope,
    scope_key: '',
    slot_key: 'plain_text',
    component_id: '',
  });

  let ruleBindings = $derived(($status?.rule_component_bindings ?? []) as RuleBinding[]);
  let taskTypeBindings = $derived(
    (($status?.task_type_component_bindings ?? []) as TaskTypeComponentBinding[])
  );
  let taskTypeForm = $state({
    business_line: 'config_i18n',
    task_type: 'text',
    component_id: '',
  });
  let ruleScopePlaceholder = $derived(getRuleScopePlaceholder(ruleForm.scope));
  let selectedSlotHint = $derived(
    SLOT_USAGE_HINTS[ruleForm.slot_key] ?? 'task_routing.slot_usage.default'
  );
  let discoveredRulesCount = $derived(ruleDiscoveryItems.length);

  let ruleCandidateComponents = $derived((() => {
    const seen = new Set<string>();
    const out: string[] = [];
    const candidates: Array<{ id: string; kind: string }> = [
      ...localComps.map((c) => ({ id: c.id, kind: c.kind })),
      ...(($status?.components ?? []) as ServerComp[]).map((c) => ({
        id: c.id,
        kind: c.kind,
      })),
    ];
    for (const c of candidates) {
      if (!c.id || seen.has(c.id)) continue;
      if (!slotSupportsKind(ruleForm.slot_key, c.kind)) continue;
      seen.add(c.id);
      out.push(c.id);
    }
    return out.sort();
  })());

  async function loadLocalComps() {
    const r = await listAllLocalComponents();
    if (r.success) {
      localComps = r.data.items as unknown as LocalComp[];
    }
  }

  async function loadRuleDiscovery() {
    ruleDiscoveryLoading = true;
    try {
      const r = await getRuleBindingDiscovery();
      if (r.success) {
        ruleDiscoveryItems = r.data.items ?? [];
        ruleDiscoveryIssues = r.data.issues ?? [];
      } else {
        ruleDiscoveryItems = [];
        ruleDiscoveryIssues = [];
        showToast('error', $_('task_routing.discovery_load_failed'), r.error?.message);
      }
    } finally {
      ruleDiscoveryLoading = false;
    }
  }

  async function saveRuleBind() {
    if (!ruleForm.slot_key.trim() || !ruleForm.component_id.trim()) {
      showToast('error', $_('task_routing.fill_slot_component'));
      return;
    }
    const scopeError = validateRuleScopeKey(ruleForm.scope, ruleForm.scope_key);
    if (scopeError) {
      showToast('error', scopeError);
      return;
    }
    const r = await upsertRuleBinding(
      ruleForm.scope,
      ruleForm.slot_key.trim(),
      ruleForm.component_id.trim(),
      normalizeRuleScopeKey(ruleForm.scope, ruleForm.scope_key),
    );
    if (r.success) {
      showToast('success', $_('task_routing.binding_saved'));
      await fetchStatus();
    } else {
      showToast('error', $_('common.save_failed'), (r as any).error?.message);
    }
  }

  async function deleteRuleBind(item: RuleBinding) {
    const r = await deleteRuleBinding(
      item.scope as RuleBindingScope,
      item.slot_key,
      item.scope_key || undefined,
    );
    if (r.success) {
      showToast('success', $_('task_routing.binding_deleted'));
      await fetchStatus();
    }
  }

  async function saveTaskTypeBind() {
    if (!taskTypeForm.component_id.trim()) {
      showToast('error', $_('task_routing.fill_task_type_component'));
      return;
    }
    const r = await upsertTaskTypeBinding(
      taskTypeForm.task_type,
      taskTypeForm.component_id.trim(),
      taskTypeForm.business_line,
    );
    if (r.success) {
      showToast('success', $_('task_routing.task_type_binding_saved'));
      await fetchStatus();
    } else {
      showToast('error', $_('common.save_failed'), (r as any).error?.message);
    }
  }

  async function deleteTaskTypeBind(item: TaskTypeComponentBinding) {
    const r = await deleteTaskTypeBinding(item.task_type, item.business_line || undefined);
    if (r.success) {
      showToast('success', $_('task_routing.task_type_binding_deleted'));
      await fetchStatus();
    }
  }

  interface FieldMatrixRow {
    id: string;
    fieldKey: string;
    titleKey: string;
    descKey: string;
    slotKey: string;
    requiredKind: string;
  }

  const FIELD_MATRIX_ROWS: FieldMatrixRow[] = [
    {
      id: 'post_title',
      fieldKey: 'title',
      titleKey: 'task_routing.matrix_field_title',
      descKey: 'task_routing.matrix_field_title_desc',
      slotKey: 'plain_text',
      requiredKind: 'text',
    },
    {
      id: 'post_content',
      fieldKey: 'content',
      titleKey: 'task_routing.matrix_field_content',
      descKey: 'task_routing.matrix_field_content_desc',
      slotKey: 'rich_html',
      requiredKind: 'text',
    },
    {
      id: 'post_excerpt',
      fieldKey: 'excerpt',
      titleKey: 'task_routing.matrix_field_excerpt',
      descKey: 'task_routing.matrix_field_excerpt_desc',
      slotKey: 'plain_text',
      requiredKind: 'text',
    },
    {
      id: 'gutenberg_blocks',
      fieldKey: 'blocks',
      titleKey: 'task_routing.matrix_field_blocks',
      descKey: 'task_routing.matrix_field_blocks_desc',
      slotKey: 'json_structured',
      requiredKind: 'text',
    },
    {
      id: 'media_assets',
      fieldKey: 'media',
      titleKey: 'task_routing.matrix_field_media',
      descKey: 'task_routing.matrix_field_media_desc',
      slotKey: 'media_ref:image',
      requiredKind: 'image',
    },
  ];

  function getGlobalBoundComponent(slotKey: string): string | null {
    const found = ruleBindings.find((b) => b.scope === 'global' && b.slot_key === slotKey);
    return found?.component_id || null;
  }

  let matrixSelections = $state<Record<string, string>>({});

  $effect(() => {
    const next: Record<string, string> = {};
    for (const row of FIELD_MATRIX_ROWS) {
      next[row.slotKey] = getGlobalBoundComponent(row.slotKey) || '';
    }
    matrixSelections = next;
  });

  function getComponentsForSlot(slotKey: string): string[] {
    const seen = new Set<string>();
    const out: string[] = [];
    const candidates = [
      ...localComps.map((c) => ({ id: c.id, kind: c.kind })),
      ...((($status?.components ?? []) as ServerComp[]).map((c) => ({ id: c.id, kind: c.kind }))),
    ];
    for (const c of candidates) {
      if (!c.id || seen.has(c.id)) continue;
      if (!slotSupportsKind(slotKey, c.kind)) continue;
      seen.add(c.id);
      out.push(c.id);
    }
    return out.sort();
  }

  async function applyMatrixRow(slotKey: string, componentId: string) {
    if (!componentId) return;
    const r = await upsertRuleBinding('global', slotKey, componentId);
    if (r.success) {
      showToast('success', $_('task_routing.matrix_save_success'));
      await fetchStatus();
    } else {
      showToast('error', $_('common.save_failed'), (r as any).error?.message);
    }
  }

  async function clearMatrixRow(slotKey: string) {
    const r = await deleteRuleBinding('global', slotKey);
    if (r.success) {
      showToast('success', $_('task_routing.matrix_clear_success'));
      await fetchStatus();
    } else {
      showToast('error', $_('common.operation_failed'), (r as any).error?.message);
    }
  }

  onMount(() => {
    loadLocalComps();
    loadRuleDiscovery();
  });
</script>

<!-- WordPress Field-to-Provider Routing Matrix -->
<div class="bg-white border border-gray-200 rounded-xl overflow-hidden mb-4 shadow-xs">
  <div class="px-5 py-4 border-b border-gray-100 bg-gradient-to-r from-blue-50/50 to-indigo-50/30">
    <h3 class="font-semibold text-gray-900 text-sm flex items-center gap-2">
      <span class="inline-block w-2 h-2 rounded-full bg-blue-600"></span>
      {$_('task_routing.matrix_title')}
    </h3>
    <p class="text-xs text-gray-500 mt-1">
      {$_('task_routing.matrix_desc')}
    </p>
  </div>

  <div class="overflow-x-auto">
    <table class="w-full text-sm">
      <thead>
        <tr class="text-xs text-gray-500 bg-gray-50/75 border-b border-gray-200">
          <th class="px-4 py-3 text-left font-medium">{$_('task_routing.matrix_field')}</th>
          <th class="px-4 py-3 text-left font-medium">{$_('task_routing.matrix_slot')}</th>
          <th class="px-4 py-3 text-left font-medium">{$_('task_routing.matrix_current')}</th>
          <th class="px-4 py-3 text-left font-medium">{$_('task_routing.matrix_select')}</th>
          <th class="px-4 py-3 text-right font-medium">{$_('task_routing.th_actions')}</th>
        </tr>
      </thead>
      <tbody class="divide-y divide-gray-100">
        {#each FIELD_MATRIX_ROWS as row (row.id)}
          {@const boundComp = getGlobalBoundComponent(row.slotKey)}
          {@const availableComps = getComponentsForSlot(row.slotKey)}
          <tr class="hover:bg-gray-50/60 transition-colors">
            <td class="px-4 py-3.5">
              <div class="font-medium text-gray-900 text-xs sm:text-sm">{$_(row.titleKey)}</div>
              <div class="text-xs text-gray-400 mt-0.5">{$_(row.descKey)}</div>
            </td>
            <td class="px-4 py-3.5 whitespace-nowrap">
              <span class="inline-flex items-center px-2 py-0.5 rounded text-xs font-mono font-medium bg-slate-100 text-slate-700">
                {row.slotKey}
              </span>
            </td>
            <td class="px-4 py-3.5 whitespace-nowrap">
              {#if boundComp}
                <span class="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-emerald-50 text-emerald-700 border border-emerald-200">
                  <span class="w-1.5 h-1.5 rounded-full bg-emerald-500"></span>
                  <span class="font-mono">{boundComp}</span>
                </span>
              {:else}
                <span class="inline-flex items-center px-2 py-0.5 rounded-full text-xs text-gray-400 bg-gray-100">
                  {$_('task_routing.matrix_unassigned')}
                </span>
              {/if}
            </td>
            <td class="px-4 py-3.5">
              <select
                bind:value={matrixSelections[row.slotKey]}
                class="w-full max-w-xs text-xs border border-gray-200 rounded-lg px-2.5 py-1.5 bg-white focus:ring-1 focus:ring-blue-500 outline-none"
              >
                <option value="">{$_('task_routing.matrix_select_placeholder')}</option>
                {#each availableComps as cid}
                  <option value={cid}>{cid}</option>
                {/each}
              </select>
            </td>
            <td class="px-4 py-3.5 text-right whitespace-nowrap">
              <div class="flex items-center justify-end gap-2">
                <button
                  type="button"
                  onclick={() => applyMatrixRow(row.slotKey, matrixSelections[row.slotKey])}
                  disabled={!matrixSelections[row.slotKey] || matrixSelections[row.slotKey] === boundComp}
                  class="px-2.5 py-1 text-xs font-medium bg-blue-600 hover:bg-blue-700 disabled:bg-gray-100 disabled:text-gray-400 text-white rounded-lg transition-colors"
                >
                  {$_('task_routing.matrix_apply')}
                </button>
                {#if boundComp}
                  <button
                    type="button"
                    onclick={() => clearMatrixRow(row.slotKey)}
                    class="px-2 py-1 text-xs text-rose-600 hover:text-rose-800 hover:bg-rose-50 rounded-lg transition-colors"
                  >
                    {$_('task_routing.matrix_clear')}
                  </button>
                {/if}
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden mb-4">
  <div class="px-5 py-3 border-b border-gray-100">
    <h3 class="font-medium text-gray-900 text-sm">{$_('task_routing.heading')}</h3>
    <p class="text-xs text-gray-500 mt-1">
      {$_('task_routing.priority_hint')}
    </p>
  </div>
  <div class="px-4 pt-4 grid gap-3 lg:grid-cols-2">
    <div class="rounded-xl border border-sky-100 bg-sky-50/70 p-3">
      <div class="text-sm font-medium text-sky-900">{$_('task_routing.config_object_title')}</div>
      <div class="mt-1 text-xs text-sky-800">
        {$_('task_routing.config_object_desc')}
      </div>
      <div class="mt-2 flex flex-wrap gap-2 text-xs">
        <button class="rounded-full bg-white px-2.5 py-1 text-sky-800 border border-sky-200" onclick={() => (ruleForm.slot_key = 'plain_text')}>{$_('task_routing.config_object_plain_text')}</button>
        <button class="rounded-full bg-white px-2.5 py-1 text-sky-800 border border-sky-200" onclick={() => (ruleForm.slot_key = 'rich_html')}>{$_('task_routing.config_object_rich_html')}</button>
        <button class="rounded-full bg-white px-2.5 py-1 text-sky-800 border border-sky-200" onclick={() => (ruleForm.slot_key = 'json_structured')}>{$_('task_routing.config_object_json')}</button>
        <button class="rounded-full bg-white px-2.5 py-1 text-sky-800 border border-sky-200" onclick={() => (ruleForm.slot_key = 'serialized_php')}>{$_('task_routing.config_object_serialized')}</button>
      </div>
    </div>
    <div class="rounded-xl border border-amber-100 bg-amber-50/70 p-3">
      <div class="text-sm font-medium text-amber-900">{$_('task_routing.msg_template_title')}</div>
      <div class="mt-1 text-xs text-amber-800">
        {$_('task_routing.msg_template_desc')}
      </div>
      <div class="mt-2 flex flex-wrap gap-2 text-xs">
        <button class="rounded-full bg-white px-2.5 py-1 text-amber-800 border border-amber-200" onclick={() => (ruleForm.slot_key = 'plain_text')}>{$_('task_routing.msg_template_plain_text')}</button>
        <button class="rounded-full bg-white px-2.5 py-1 text-amber-800 border border-amber-200" onclick={() => (ruleForm.slot_key = 'rich_html')}>{$_('task_routing.msg_template_rich_html')}</button>
        <button class="rounded-full bg-white px-2.5 py-1 text-amber-800 border border-amber-200" onclick={() => (ruleForm.slot_key = 'code')}>{$_('task_routing.msg_template_code')}</button>
      </div>
    </div>
  </div>
  <div class="px-4 pt-4">
    <div class="rounded-xl border border-gray-200 bg-white">
      <div class="flex items-center justify-between gap-3 px-3 py-3 border-b border-gray-100">
        <div>
          <div class="text-sm font-medium text-gray-900">{$_('task_routing.discovered_rules')}</div>
          <div class="mt-1 text-xs text-gray-500">
            {$_('task_routing.discovered_rules_desc')}
          </div>
        </div>
        <div class="flex items-center gap-2">
          <div class="text-xs text-gray-400">{$_('task_routing.rules_count', { values: { count: discoveredRulesCount } })}</div>
          <button
            class="rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50"
            onclick={loadRuleDiscovery}
            disabled={ruleDiscoveryLoading}>
            {ruleDiscoveryLoading ? $_('task_routing.refreshing') : $_('task_routing.refresh_rules')}
          </button>
        </div>
      </div>

      {#if ruleDiscoveryIssues.length > 0}
        <div class="px-3 py-3 border-b border-amber-100 bg-amber-50/70">
          <div class="text-xs font-medium text-amber-900">{$_('task_routing.partial_load_failed')}</div>
          <div class="mt-2 space-y-1">
            {#each ruleDiscoveryIssues as issue}
              <div class="text-[11px] text-amber-800 break-all">
                {issue.api_base_url}
                {#if issue.relation_id}
                  · {$_('task_routing.relation_label')} {issue.relation_id}
                {/if}
                · {issue.stage} · {issue.error}
              </div>
            {/each}
          </div>
        </div>
      {/if}

      <div class="max-h-[28rem] overflow-auto">
        {#if ruleDiscoveryLoading && ruleDiscoveryItems.length === 0}
          <div class="px-4 py-6 text-sm text-gray-400">{$_('task_routing.loading_rules')}</div>
        {:else if ruleDiscoveryItems.length === 0}
          <div class="px-4 py-6 text-sm text-gray-400">{$_('task_routing.no_rules')}</div>
        {:else}
          <div class="divide-y divide-gray-100">
            {#each ruleDiscoveryItems as item}
              <div class="px-3 py-3">
                <div class="flex flex-wrap items-start justify-between gap-3">
                  <div class="min-w-0">
                    <div class="text-sm font-medium text-gray-900">
                      {item.rule_name || $_('task_routing.rule_fallback', { values: { id: item.rule_id } })} · {$_('task_routing.relation_label')} {item.relation_id}
                    </div>
                    <div class="mt-1 text-xs text-gray-500 break-all">{item.api_base_url}</div>
                    <div class="mt-2 flex flex-wrap gap-2 text-[11px]">
                      <span class="rounded-full bg-slate-100 px-2 py-1 text-slate-700">{formatBusinessLineLabel(item.business_line)}</span>
                      <span class="rounded-full bg-sky-50 px-2 py-1 text-sky-700">{formatSourceGroupLabel(item.source_group)}</span>
                      <span class="rounded-full bg-amber-50 px-2 py-1 text-amber-700">{formatRoutingProfileLabel(item.routing_profile)}</span>
                      <span class="rounded-full bg-emerald-50 px-2 py-1 text-emerald-700">{formatDeliveryTargetLabel(item.delivery_target)}</span>
                    </div>
                    <div class="mt-2 text-xs text-gray-500">
                      {$_('task_routing.object')}: <span class="font-medium text-gray-700">{item.object_name}</span>
                      <span class="ml-2">{$_('task_routing.type')}: {item.data_type}</span>
                      {#if item.plugin_slug}
                        <span class="ml-2">{$_('task_routing.plugin')}: {item.plugin_slug}</span>
                      {/if}
                      <span class="ml-2">{item.source_lang} → {item.target_lang}</span>
                    </div>
                  </div>
                  <div class="flex flex-wrap gap-2">
                    <button
                      class="rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50"
                      onclick={() => {
                        ruleForm.scope = 'rule';
                        ruleForm.scope_key = String(item.rule_id);
                      }}>
                      {$_('task_routing.fill_rule_scope')}
                    </button>
                    {#if item.plugin_slug}
                      <button
                        class="rounded-lg border border-gray-200 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50"
                        onclick={() => {
                          ruleForm.scope = 'plugin';
                          ruleForm.scope_key = item.plugin_slug;
                        }}>
                        {$_('task_routing.fill_plugin_scope')}
                      </button>
                    {/if}
                  </div>
                </div>

                {#if item.required_component_slots.length > 0 || item.required_content_formats.length > 0}
                  <div class="mt-3 flex flex-wrap gap-2 text-[11px]">
                    {#each item.required_component_slots as slot}
                      <span class="rounded-full bg-blue-50 px-2 py-1 text-blue-700">
                        {$_('task_routing.recommended_slot')}: {formatRuleSlotLabel(slot)}
                      </span>
                    {/each}
                    {#each item.required_content_formats as format}
                      <span class="rounded-full bg-gray-100 px-2 py-1 text-gray-600">
                        {$_('task_routing.format_label')}: {formatCapabilityLabel(format)}
                      </span>
                    {/each}
                  </div>
                {/if}

                <div class="mt-3 grid gap-2">
                  {#each item.fields as field}
                    <div class="rounded-lg border border-gray-200 bg-gray-50 px-3 py-2">
                      <div class="flex flex-wrap items-start justify-between gap-3">
                        <div class="min-w-0">
                          <div class="text-xs font-mono text-gray-700 break-all">{field.field_name}</div>
                          <div class="mt-1 flex flex-wrap gap-2 text-[11px]">
                            <span class="text-gray-500">{fieldActionLabel(field.action)}</span>
                            <span class="text-gray-500">{formatCapabilityLabel(field.content_format)}</span>
                            <span class="text-gray-400">{$_('task_routing.field_role')}: {formatSourceRoleLabel(field.source_role)}</span>
                            <span class="text-gray-400">{$_('task_routing.field_storage')}: {field.storage}</span>
                            <span class="text-gray-400">{$_('task_routing.field_task')}: {field.suggested_task_type}</span>
                          </div>
                        </div>
                        <button
                          class="rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700"
                          onclick={() => {
                            ruleForm.scope = 'rule';
                            ruleForm.scope_key = String(item.rule_id);
                            ruleForm.slot_key = field.required_slot_key;
                          }}>
                          {$_('task_routing.use_field')}
                        </button>
                      </div>
                      <div class="mt-2 text-[11px] text-gray-500">
                        {$_('task_routing.slot_label')}: <span class="font-medium text-gray-700">{formatRuleSlotLabel(field.required_slot_key)}</span>
                        <span class="ml-1 font-mono text-gray-400">{field.required_slot_key}</span>
                      </div>
                    </div>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  </div>
  <div class="px-4 pt-3">
    <div class="rounded-xl border border-gray-200 bg-gray-50 px-3 py-3">
      <div class="text-xs font-medium text-gray-700">{$_('task_routing.quick_select')}</div>
      <div class="mt-2 flex flex-wrap gap-2">
        {#each SLOT_QUICK_PICKS as item}
          <button
            class="rounded-full border px-2.5 py-1 text-xs transition-colors {ruleForm.slot_key === item.key ? 'border-blue-300 bg-blue-50 text-blue-700' : 'border-gray-200 bg-white text-gray-600 hover:bg-gray-100'}"
            onclick={() => (ruleForm.slot_key = item.key)}>
            {$_(item.title)} · {item.key}
          </button>
        {/each}
      </div>
      <div class="mt-2 text-xs text-gray-500">
        {$_('task_routing.current_slot')}：<span class="font-medium text-gray-700">{formatRuleSlotLabel(ruleForm.slot_key)}</span>
        <span class="font-mono text-[11px] text-gray-400 ml-1">{ruleForm.slot_key}</span>
        <span class="ml-2">{$_(selectedSlotHint)}</span>
      </div>
    </div>
  </div>
  <div class="p-4 flex flex-wrap gap-3 items-end">
    <div>
      <label for={ruleBindFieldId('scope')} class="block text-xs text-gray-500 mb-1">{$_('task_routing.scope')}</label>
      <select id={ruleBindFieldId('scope')} data-testid="rule-bind-scope" bind:value={ruleForm.scope} class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-32">
        <option value="global">global</option>
        <option value="plugin">plugin</option>
        <option value="relation">relation</option>
        <option value="rule">rule</option>
      </select>
    </div>
    <div>
      <label for={ruleBindFieldId('scope-key')} class="block text-xs text-gray-500 mb-1">scope_key</label>
      <input
        id={ruleBindFieldId('scope-key')}
        data-testid="rule-bind-scope-key"
        bind:value={ruleForm.scope_key}
        disabled={ruleForm.scope === 'global'}
        placeholder={ruleScopePlaceholder}
        class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-56 disabled:bg-gray-50 disabled:text-gray-400" />
      <div class="mt-1 text-[11px] text-gray-400">{$_(RULE_SCOPE_EXAMPLES[ruleForm.scope])}</div>
    </div>
    <div>
      <label for={ruleBindFieldId('slot-key')} class="block text-xs text-gray-500 mb-1">{$_('task_routing.slot_key_label')}</label>
      <select id={ruleBindFieldId('slot-key')} data-testid="rule-bind-slot-key" bind:value={ruleForm.slot_key} class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-56">
        {#each RULE_SLOT_OPTIONS as opt}
          <option value={opt.key}>{opt.label} · {$_(opt.hint)}</option>
        {/each}
      </select>
    </div>
    <div>
      <label for={ruleBindFieldId('component-id')} class="block text-xs text-gray-500 mb-1">{$_('task_routing.component_id')}</label>
      <input
        id={ruleBindFieldId('component-id')}
        data-testid="rule-bind-component-id"
        bind:value={ruleForm.component_id}
        placeholder={$_('task_routing.placeholder_component_id')}
        class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-56" />
    </div>
    <button onclick={saveRuleBind} data-testid="rule-bind-save" class="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">
      {$_('task_routing.save_binding')}
    </button>
  </div>
  <div class="px-4 pb-4">
    <div class="text-xs text-gray-500 mb-1">{$_('task_routing.recommended_comps')}</div>
    <div class="flex flex-wrap gap-2">
      {#if ruleCandidateComponents.length === 0}
        <span class="text-xs text-gray-400">{$_('task_routing.no_matching_comps')}</span>
      {:else}
        {#each ruleCandidateComponents as cid}
          <button
            onclick={() => (ruleForm.component_id = cid)}
            class="text-xs font-mono px-2 py-1 bg-gray-50 border border-gray-200 rounded hover:bg-gray-100">
            {cid}
          </button>
        {/each}
      {/if}
    </div>
  </div>
</div>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden mb-4">
  <div class="px-5 py-3 border-b border-gray-100">
    <h3 class="font-medium text-gray-900 text-sm">{$_('task_routing.task_type_binding_title')}</h3>
    <p class="text-xs text-gray-500 mt-1">
      {$_('task_routing.task_type_binding_desc')}
    </p>
  </div>
  <div class="p-4 flex flex-wrap gap-3 items-end">
    <div>
      <label for="task-type-bind-business-line" class="block text-xs text-gray-500 mb-1">{$_('task_routing.business_line')}</label>
      <select
        id="task-type-bind-business-line"
        data-testid="task-type-bind-business-line"
        bind:value={taskTypeForm.business_line}
        class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-52">
        {#each BUSINESS_LINE_OPTIONS as line}
          <option value={line}>{formatBusinessLineLabel(line)} · {line}</option>
        {/each}
      </select>
    </div>
    <div>
      <label for="task-type-bind-task-type" class="block text-xs text-gray-500 mb-1">{$_('task_routing.task_type')}</label>
      <select
        id="task-type-bind-task-type"
        data-testid="task-type-bind-task-type"
        bind:value={taskTypeForm.task_type}
        class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-32">
        {#each TASK_TYPE_OPTIONS as tt}
          <option value={tt}>{tt}</option>
        {/each}
      </select>
    </div>
    <div>
      <label for="task-type-bind-component-id" class="block text-xs text-gray-500 mb-1">{$_('task_routing.task_type_component_id')}</label>
      <input
        id="task-type-bind-component-id"
        data-testid="task-type-bind-component-id"
        bind:value={taskTypeForm.component_id}
        placeholder={$_('task_routing.placeholder_component_id')}
        class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-56" />
    </div>
    <button
      onclick={saveTaskTypeBind}
      data-testid="task-type-bind-save"
      class="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">
      {$_('task_routing.save_task_type_binding')}
    </button>
  </div>
  <div class="px-4 pb-4">
    <table class="w-full text-sm">
      <thead>
        <tr class="text-xs text-gray-500 bg-gray-50">
          <th class="px-4 py-2.5 text-left font-medium">business_line</th>
          <th class="px-4 py-2.5 text-left font-medium">task_type</th>
          <th class="px-4 py-2.5 text-left font-medium">component_id</th>
          <th class="px-4 py-2.5 text-right font-medium">{$_('task_routing.th_actions')}</th>
        </tr>
      </thead>
      <tbody>
        {#if taskTypeBindings.length === 0}
          <tr>
            <td colspan="4" class="px-4 py-8 text-center text-gray-400">{$_('task_routing.no_task_type_bindings')}</td>
          </tr>
        {:else}
          {#each taskTypeBindings as b}
            <tr class="border-t border-gray-50 hover:bg-gray-50/50">
              <td class="px-4 py-3 text-xs">
                {#if b.business_line}
                  <span class="font-mono text-blue-600">{b.business_line}</span>
                  <span class="ml-1 text-gray-500">{formatBusinessLineLabel(b.business_line)}</span>
                {:else}
                  <span class="text-gray-400">{$_('task_routing.global_scope')}</span>
                {/if}
              </td>
              <td class="px-4 py-3 text-xs font-mono">{b.task_type}</td>
              <td class="px-4 py-3 text-xs font-mono text-gray-600">{b.component_id}</td>
              <td class="px-4 py-3 text-right">
                <button
                  onclick={() => {
                    taskTypeForm.business_line = b.business_line || 'config_i18n';
                    taskTypeForm.task_type = b.task_type;
                    taskTypeForm.component_id = b.component_id;
                  }}
                  class="text-xs text-gray-500 hover:text-gray-700 mr-2">
                  {$_('task_routing.fill')}
                </button>
                <button
                  onclick={() => deleteTaskTypeBind(b)}
                  data-testid="task-type-bind-delete"
                  class="text-xs text-red-500 hover:text-red-600">
                  {$_('common.delete')}
                </button>
              </td>
            </tr>
          {/each}
        {/if}
      </tbody>
    </table>
  </div>
</div>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden mb-4">
  <table class="w-full text-sm">
    <thead>
      <tr class="text-xs text-gray-500 bg-gray-50">
        <th class="px-4 py-2.5 text-left font-medium">scope</th>
        <th class="px-4 py-2.5 text-left font-medium">scope_key</th>
        <th class="px-4 py-2.5 text-left font-medium">slot_key</th>
        <th class="px-4 py-2.5 text-left font-medium">component_id</th>
        <th class="px-4 py-2.5 text-right font-medium">{$_('task_routing.th_actions')}</th>
      </tr>
    </thead>
    <tbody>
      {#if ruleBindings.length === 0}
        <tr>
          <td colspan="5" class="px-4 py-8 text-center text-gray-400">{$_('task_routing.no_bindings')}</td>
        </tr>
      {:else}
        {#each ruleBindings as b}
          <tr class="border-t border-gray-50 hover:bg-gray-50/50">
            <td class="px-4 py-3 text-xs font-mono">{b.scope}</td>
            <td class="px-4 py-3 text-xs font-mono text-gray-500">{b.scope_key || $_('task_routing.global_scope')}</td>
            <td class="px-4 py-3 text-xs font-mono text-blue-600">{b.slot_key}</td>
            <td class="px-4 py-3 text-xs font-mono text-gray-600">{b.component_id}</td>
            <td class="px-4 py-3 text-right">
              <button
                onclick={() => {
                  ruleForm.scope = b.scope as RuleBindingScope;
                  ruleForm.scope_key = b.scope_key || '';
                  ruleForm.slot_key = b.slot_key;
                  ruleForm.component_id = b.component_id;
                }}
                class="text-xs text-gray-500 hover:text-gray-700 mr-2">
                {$_('task_routing.fill')}
              </button>
              <button onclick={() => deleteRuleBind(b)} class="text-xs text-red-500 hover:text-red-600">
                {$_('common.delete')}
              </button>
            </td>
          </tr>
        {/each}
      {/if}
    </tbody>
  </table>
</div>
