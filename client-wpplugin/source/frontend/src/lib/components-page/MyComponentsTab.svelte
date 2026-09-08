<script lang="ts">
  import { onMount } from 'svelte';
  import {
    createComponentVersion,
    createLocalComponent,
    deleteBinding,
    deleteComponentVersion,
    deleteLocalComponent,
    exportLocalComponent,
    getLocalComponent,
    importLocalComponent,
    listLocalComponents,
    loadComponentTemplate,
    quickTestLocalComponent,
    refreshLocalComponentSnapshot,
    testComponentVersion,
    testLocalComponentFile,
    updateComponentVersion,
    updateLocalComponent,
    upsertBinding,
  } from '../api/components';
  import { listOAuthConfigs, listVendorKeys } from '../api/keys';
  import { status, fetchStatus } from '../stores/status';
  import { showToast } from '../stores/toast';
  import {
    authModalFieldId,
    componentModalFieldId,
    extractAuthModes,
    extractOpenAiCompatibleConfig,
    formatTsLabel,
    handleBackdropKeydown,
    isNonTextKind,
    localCapability,
    localCapabilityBadgeClass,
    parsePositiveFloat,
    parsePositiveInt,
    testFileFieldId,
    versionModalFieldId,
  } from './helpers';
  import type {
    AuthStrategy,
    ComponentFormState,
    ComponentVersion,
    FileTestModalState,
    LocalComp,
    LocalCompsPage,
    OAuthItem,
    OpenAiPreset,
    QuickTestState,
    VendorKeyItem,
    VersionFormState,
  } from './types';
  import AuthBindingModal from './modals/AuthBindingModal.svelte';
  import ComponentModal from './modals/ComponentModal.svelte';
  import FileTestModal from './modals/FileTestModal.svelte';
  import VersionModal from './modals/VersionModal.svelte';
  import { _ } from 'svelte-i18n';

  const OAI_PRESETS: OpenAiPreset[] = [
    { label: 'OpenAI', api_base: 'https://api.openai.com', model: 'gpt-4o-mini' },
    { label: 'DeepSeek', api_base: 'https://api.deepseek.com', model: 'deepseek-chat' },
    { label: 'Moonshot', api_base: 'https://api.moonshot.cn', model: 'moonshot-v1-8k' },
    { label: 'Qwen', api_base: 'https://dashscope.aliyuncs.com/compatible-mode', model: 'qwen-turbo' },
    { label: 'Groq', api_base: 'https://api.groq.com/openai', model: 'llama-3.1-70b-versatile' },
    { label: 'Together AI', api_base: 'https://api.together.xyz', model: 'meta-llama/Llama-3-70b-chat-hf' },
    { label: 'Ollama', api_base: 'http://localhost:11434', model: 'llama3' },
  ];

  function emptyCompForm(): ComponentFormState {
    return {
      id: '',
      name: '',
      template_id: '',
      vendor_id: '',
      vendor_name: '',
      kind: 'text',
      remarks: '',
      enabled: true,
      api_base: '',
      model: '',
      system_prompt: '',
      temperature: '0.1',
      max_tokens: '',
      response_path: 'choices.0.message.content',
    };
  }

  function emptyVersionForm(): VersionFormState {
    return {
      version: '1.0.0',
      key_ids: '',
      key_selection_strategy: 'round_robin',
      auth_type: 'key',
      proxy_profile_id: '',
      remarks: '',
    };
  }

  function emptyFileTestModal(): FileTestModalState {
    return {
      open: false,
      componentId: '',
      componentName: '',
      fileUrl: 'http://127.0.0.1:9090/api/v1/test-file/1024',
      sourceLang: 'en',
      targetLang: 'zh-CN',
      loading: false,
      result: null,
      error: null,
    };
  }

  function emptyQuickTestState(): QuickTestState {
    return {
      compId: '',
      apiKey: '',
      text: 'Hello World',
      targetLang: 'zh_CN',
      loading: false,
      result: null,
      error: null,
    };
  }

  function fallbackErrorMessage(message: unknown, fallback: string): string {
    if (typeof message !== 'string') return fallback;
    const trimmed = message.trim();
    return trimmed.length > 0 ? trimmed : fallback;
  }

  let localComps = $state<LocalComp[]>([]);
  let localCompsLoading = $state(false);
  let localSearchQ = $state('');
  let localKindFilter = $state('');
  let localPage = $state(1);
  let localTotalPages = $state(1);
  let localTotal = $state(0);
  let localAvailableKinds = $state<string[]>([]);
  let showCompModal = $state(false);
  let editingComp = $state<LocalComp | null>(null);
  let compForm = $state<ComponentFormState>(emptyCompForm());
  let expandedCompId = $state<string | null>(null);

  let showVersionModal = $state(false);
  let versionForm = $state<VersionFormState>(emptyVersionForm());
  let editingVersion = $state<{ compId: string; ver: string } | null>(null);
  let versionModalCompId = $state('');
  let currentVersionModalComp = $derived(
    localComps.find((comp) => comp.id === versionModalCompId) ?? null
  );

  let showAuthModal = $state(false);
  let authModalComp = $state<LocalComp | null>(null);
  let authModalSupportedModes = $state<string[]>([]);
  let authFields = $state<{ key: string; value: string }[]>([]);
  let editableParamPaths = $state<string[]>([]);
  let overrideRequestUrl = $state('');
  let overrideRequestHeadersJson = $state('');
  let overrideRequestBodyJson = $state('');
  let overrideMaxInputChars = $state('');
  let overrideRateLimitQps = $state('');
  let overrideMaxConcurrentRequests = $state('');
  let overrideMaxFileSizeMb = $state('');
  let authPoolKeyIds = $state<string[]>([]);
  let authPoolOAuthIds = $state<string[]>([]);
  let authPoolStrategy = $state<AuthStrategy>('RoundRobin');
  let authPoolExpanded = $state(false);
  let availableKeys = $state<VendorKeyItem[]>([]);
  let availableOAuthConfigs = $state<OAuthItem[]>([]);
  let poolResourcesLoading = $state(false);
  let keyDropdownOpen = $state(false);
  let oauthDropdownOpen = $state(false);

  let testingVer = $state<string | null>(null);
  let testFileModal = $state<FileTestModalState>(emptyFileTestModal());
  let quickTestState = $state<QuickTestState>(emptyQuickTestState());

  function authModeEnabled(mode: 'key' | 'oauth' | 'none'): boolean {
    if (authModalSupportedModes.length === 0) return mode === 'key';
    return authModalSupportedModes.includes(mode);
  }

  async function loadPoolResources() {
    poolResourcesLoading = true;
    try {
      const vendorQuery = authModalComp?.vendor_id?.trim()
        ? authModalComp.vendor_id.trim()
        : undefined;
      const [keysRes, oauthRes] = await Promise.all([
        listVendorKeys(vendorQuery),
        listOAuthConfigs(vendorQuery),
      ]);
      if (keysRes.success) availableKeys = keysRes.data.items as unknown as VendorKeyItem[];
      if (oauthRes.success) availableOAuthConfigs = oauthRes.data.items as unknown as OAuthItem[];
    } finally {
      poolResourcesLoading = false;
    }
  }

  function toggleKeyId(id: string) {
    if (authPoolKeyIds.includes(id)) {
      authPoolKeyIds = authPoolKeyIds.filter((k) => k !== id);
    } else {
      authPoolKeyIds = [...authPoolKeyIds, id];
    }
  }

  function toggleOAuthId(id: string) {
    if (authPoolOAuthIds.includes(id)) {
      authPoolOAuthIds = authPoolOAuthIds.filter((k) => k !== id);
    } else {
      authPoolOAuthIds = [...authPoolOAuthIds, id];
    }
  }

  function isEditableParam(path: string): boolean {
    const normalized = path.trim().toLowerCase();
    return editableParamPaths.some((p) => {
      const base = p.trim().toLowerCase();
      if (base === normalized) return true;
      if (normalized === 'request.headers' && base.startsWith('request.headers.')) return true;
      if (normalized === 'request.body' && base.startsWith('request.body.')) return true;
      if (normalized === 'default_values' && base.startsWith('default_values.')) return true;
      if (base === 'request.body' && normalized.startsWith('request.body.')) return true;
      if (base === 'request.headers' && normalized.startsWith('request.headers.')) return true;
      if (base === 'default_values' && normalized.startsWith('default_values.')) return true;
      return false;
    });
  }

  function extractTemplateAuthFieldNames(templateJson: unknown): string[] {
    const obj =
      templateJson && typeof templateJson === 'object'
        ? (templateJson as Record<string, unknown>)
        : null;
    const authObj =
      obj?.auth && typeof obj.auth === 'object'
        ? (obj.auth as Record<string, unknown>)
        : null;
    const fields = Array.isArray(authObj?.fields) ? authObj.fields : [];
    const names: string[] = [];
    for (const item of fields) {
      if (!item || typeof item !== 'object') continue;
      const name = typeof (item as Record<string, unknown>).name === 'string'
        ? ((item as Record<string, unknown>).name as string).trim()
        : '';
      if (name && !names.includes(name)) names.push(name);
    }
    return names;
  }

  function extractEditableParams(templateJson: unknown): string[] {
    const obj =
      templateJson && typeof templateJson === 'object'
        ? (templateJson as Record<string, unknown>)
        : null;
    const editableParams = Array.isArray(obj?.editable_params) ? obj.editable_params : [];
    return editableParams
      .map((item) => {
        if (!item || typeof item !== 'object') return '';
        const path = (item as Record<string, unknown>).path;
        return typeof path === 'string' ? path.trim() : '';
      })
      .filter(Boolean);
  }

  async function openAuthModal(comp: LocalComp) {
    authModalComp = comp;
    const existing = ($status as any)?.component_bindings?.components?.[comp.id] ?? {};
    const existingAuth = existing.auth ?? {};
    const existingKeys = Object.keys(existingAuth).filter((k) => k.trim().length > 0);
    let mergedKeys = [...existingKeys];

    editableParamPaths = [];
    overrideRequestUrl = existing?.request_overrides?.url ?? '';
    overrideRequestHeadersJson = existing?.request_overrides?.headers
      ? JSON.stringify(existing.request_overrides.headers, null, 2)
      : '';
    overrideRequestBodyJson = existing?.request_overrides?.body
      ? JSON.stringify(existing.request_overrides.body, null, 2)
      : '';
    overrideMaxInputChars =
      existing?.constraints_override?.max_input_chars != null
        ? String(existing.constraints_override.max_input_chars)
        : '';
    overrideRateLimitQps =
      existing?.constraints_override?.rate_limit_qps != null
        ? String(existing.constraints_override.rate_limit_qps)
        : '';
    overrideMaxConcurrentRequests =
      existing?.constraints_override?.max_concurrent_requests != null
        ? String(existing.constraints_override.max_concurrent_requests)
        : '';
    overrideMaxFileSizeMb =
      existing?.constraints_override?.max_file_size_mb != null
        ? String(existing.constraints_override.max_file_size_mb)
        : '';
    if (comp.kind === 'openai_compatible') {
      try {
        const detail = await getLocalComponent(comp.id);
        const templateJson = detail.data?.template_json;
        const templateKeys = extractTemplateAuthFieldNames(templateJson);
        if (templateKeys.length > 0) {
          mergedKeys = Array.from(new Set([...templateKeys, ...existingKeys]));
        }
        const extractedModes = extractAuthModes(templateJson);
        authModalSupportedModes = extractedModes.length > 0 ? extractedModes : ['key'];
        editableParamPaths = extractEditableParams(templateJson);
      } catch {
        authModalSupportedModes = ['key'];
      }
    } else {
      // Local-first: prefer the component's inline template_json snapshot.
      // Legacy server download is only a fallback when no local snapshot exists.
      let loadedFromLocal = false;
      try {
        const detail = await getLocalComponent(comp.id);
        const templateJson = detail.data?.template_json;
        if (templateJson) {
          const templateKeys = extractTemplateAuthFieldNames(templateJson);
          if (templateKeys.length > 0) {
            mergedKeys = Array.from(new Set([...templateKeys, ...existingKeys]));
          }
          const extractedModes = extractAuthModes(templateJson);
          authModalSupportedModes = extractedModes.length > 0 ? extractedModes : ['key'];
          editableParamPaths = extractEditableParams(templateJson);
          loadedFromLocal = true;
        }
      } catch {
        // fall through to server template
      }
      const templateSourceId = (comp.template_id || '').trim() || comp.id;
      if (!loadedFromLocal && templateSourceId) {
        try {
          const tr = await loadComponentTemplate(templateSourceId);
          if (tr.success && tr.data?.auth_fields) {
            const templateKeys = tr.data.auth_fields
              .map((f: { name?: string } | null | undefined) => (f?.name ?? '').trim())
              .filter(Boolean);
            mergedKeys = Array.from(new Set([...templateKeys, ...existingKeys]));
          }
          const extractedModes = extractAuthModes(tr.data?.template_json, tr.data?.auth_modes);
          authModalSupportedModes = extractedModes.length > 0 ? extractedModes : ['key'];
          const templateJson =
            tr.data?.template_json &&
            typeof tr.data.template_json === 'object' &&
            !Array.isArray(tr.data.template_json)
              ? (tr.data.template_json as Record<string, unknown>)
              : {};
          const editableParams = templateJson.editable_params;
          if (Array.isArray(editableParams)) {
            editableParamPaths = editableParams
              .map((item) => (item?.path ?? '').trim())
              .filter(Boolean);
          }
        } catch {
          authModalSupportedModes = ['key'];
        }
      } else if (!loadedFromLocal) {
        authModalSupportedModes = ['key'];
      }
    }
    authFields =
      mergedKeys.length > 0
        ? mergedKeys.map((k) => ({ key: k, value: '' }))
        : [{ key: '', value: '' }];
    authPoolKeyIds = Array.isArray(existing.key_ids) ? [...existing.key_ids] : [];
    authPoolOAuthIds = Array.isArray(existing.oauth_ids) ? [...existing.oauth_ids] : [];
    authPoolStrategy = (existing.auth_strategy as AuthStrategy) ?? 'RoundRobin';
    if (!authModeEnabled('key')) authPoolKeyIds = [];
    if (!authModeEnabled('oauth')) authPoolOAuthIds = [];
    authPoolExpanded = authPoolKeyIds.length > 0 || authPoolOAuthIds.length > 0;
    keyDropdownOpen = false;
    oauthDropdownOpen = false;
    showAuthModal = true;
    loadPoolResources();
  }

  async function saveAuthBinding() {
    if (!authModalComp) return;
    if (!authModeEnabled('key') && authPoolKeyIds.length > 0) {
      showToast('error', $_('my_components.key_not_supported'));
      return;
    }
    if (!authModeEnabled('oauth') && authPoolOAuthIds.length > 0) {
      showToast('error', $_('my_components.oauth_not_supported'));
      return;
    }
    if (
      (authPoolKeyIds.length > 0 || authPoolOAuthIds.length > 0) &&
      !authModalComp.vendor_id.trim()
    ) {
      showToast('error', $_('my_components.no_vendor_id'));
      return;
    }
    const auth: Record<string, string> = {};
    for (const f of authFields) {
      if (f.key.trim()) auth[f.key.trim()] = f.value;
    }
    const constraints_override: Record<string, number> = {};
    if (isEditableParam('constraints.max_input_chars')) {
      const parsed = parsePositiveInt(overrideMaxInputChars);
      if (parsed !== undefined) constraints_override.max_input_chars = parsed;
    }
    if (isEditableParam('constraints.rate_limit_qps')) {
      const parsed = parsePositiveInt(overrideRateLimitQps);
      if (parsed !== undefined) constraints_override.rate_limit_qps = parsed;
    }
    if (isEditableParam('constraints.max_concurrent_requests')) {
      const parsed = parsePositiveInt(overrideMaxConcurrentRequests);
      if (parsed !== undefined) constraints_override.max_concurrent_requests = parsed;
    }
    if (isEditableParam('constraints.max_file_size_mb')) {
      const parsed = parsePositiveFloat(overrideMaxFileSizeMb);
      if (parsed !== undefined) constraints_override.max_file_size_mb = parsed;
    }
    const request_overrides: Record<string, unknown> = {};
    if (isEditableParam('request.url') && overrideRequestUrl.trim()) {
      request_overrides.url = overrideRequestUrl.trim();
    }
    if (isEditableParam('request.headers') && overrideRequestHeadersJson.trim()) {
      try {
        const parsed = JSON.parse(overrideRequestHeadersJson);
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
          showToast('error', $_('my_components.headers_must_json'));
          return;
        }
        request_overrides.headers = parsed;
      } catch {
        showToast('error', $_('my_components.headers_json_error'));
        return;
      }
    }
    if (isEditableParam('request.body') && overrideRequestBodyJson.trim()) {
      try {
        request_overrides.body = JSON.parse(overrideRequestBodyJson);
      } catch {
        showToast('error', $_('my_components.body_json_error'));
        return;
      }
    }

    const r = await upsertBinding({
      component_id: authModalComp.id,
      auth,
      key_ids: authPoolKeyIds,
      oauth_ids: authPoolOAuthIds,
      auth_strategy: authPoolStrategy,
      constraints_override:
        Object.keys(constraints_override).length > 0 ? constraints_override : undefined,
      request_overrides:
        Object.keys(request_overrides).length > 0 ? request_overrides : undefined,
    });
    if (r.success) {
      showToast('success', $_('my_components.auth_saved'));
      closeAuthModal();
      await fetchStatus();
    } else {
      showToast('error', $_('common.save_failed'), (r as any).error?.message);
    }
  }

  async function refreshCompSnapshot(comp: LocalComp) {
    const r = await refreshLocalComponentSnapshot(comp.id);
    if (r.success) {
      showToast('success', $_('my_components.snapshot_refreshed'));
      await loadLocalComps();
    } else {
      showToast('error', $_('my_components.snapshot_refresh_failed'), (r as any).error?.message);
    }
  }

  async function clearAuthBinding() {
    if (!authModalComp) return;
    if (!confirm($_('my_components.confirm_clear_auth', { values: { name: authModalComp.name } }))) return;
    const r = await deleteBinding(authModalComp.id);
    if (r.success) {
      showToast('success', $_('my_components.binding_cleared'));
      closeAuthModal();
      await fetchStatus();
    } else {
      showToast('error', $_('my_components.clear_failed'), (r as any).error?.message);
    }
  }

  async function loadLocalComps() {
    localCompsLoading = true;
    try {
      const r = await listLocalComponents(localPage, localSearchQ.trim(), undefined, localKindFilter || undefined);
      if (r.success) {
        localComps = r.data.items as unknown as LocalComp[];
        localTotalPages = r.data.total_pages ?? 1;
        localTotal = r.data.total ?? localComps.length;
        if ((r.data as LocalCompsPage & { kinds?: string[] }).kinds) localAvailableKinds = (r.data as LocalCompsPage & { kinds?: string[] }).kinds ?? [];
      }
    } finally {
      localCompsLoading = false;
    }
  }

  let searchDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  function onSearchInput() {
    localPage = 1;
    if (searchDebounceTimer) clearTimeout(searchDebounceTimer);
    searchDebounceTimer = setTimeout(() => loadLocalComps(), 300);
  }

  function onKindFilterChange() {
    localPage = 1;
    loadLocalComps();
  }

  function openCreateComp() {
    editingComp = null;
    compForm = emptyCompForm();
    showCompModal = true;
  }

  async function openEditComp(c: LocalComp) {
    editingComp = c;
    const nextForm: ComponentFormState = {
      id: c.id,
      name: c.name,
      template_id: c.template_id,
      vendor_id: c.vendor_id,
      vendor_name: c.vendor_name,
      kind: c.kind,
      remarks: c.remarks,
      enabled: c.enabled,
      api_base: '',
      model: '',
      system_prompt: '',
      temperature: '0.1',
      max_tokens: '',
      response_path: 'choices.0.message.content',
    };
    if (c.kind === 'openai_compatible') {
      try {
        const detail = await getLocalComponent(c.id);
        const config = extractOpenAiCompatibleConfig(detail.data?.template_json);
        if (config) {
          nextForm.api_base = config.api_base;
          nextForm.model = config.model;
          nextForm.system_prompt = config.system_prompt;
          nextForm.temperature = config.temperature;
          nextForm.max_tokens = config.max_tokens;
          nextForm.response_path = config.response_path;
        }
      } catch (e: any) {
        showToast('error', $_('my_components.read_config_failed'), e?.message);
      }
    }
    compForm = nextForm;
    showCompModal = true;
  }

  async function saveComp() {
    if (!compForm.name.trim()) {
      showToast('error', $_('my_components.fill_name'));
      return;
    }
    const compKind = editingComp ? editingComp.kind : compForm.kind;
    const isOpenAiCompatible = compKind === 'openai_compatible';
    if (!editingComp && !compForm.id.trim()) {
      showToast('error', $_('my_components.fill_id'));
      return;
    }
    if (!isOpenAiCompatible && !compForm.template_id.trim()) {
      showToast('error', $_('my_components.fill_template_id'));
      return;
    }
    if (isOpenAiCompatible) {
      if (!compForm.api_base.trim()) {
        showToast('error', $_('my_components.fill_api_base'));
        return;
      }
      if (!compForm.model.trim()) {
        showToast('error', $_('my_components.fill_model'));
        return;
      }
    }
    let r;
    if (editingComp) {
      const body: Record<string, unknown> = {
        name: compForm.name,
        remarks: compForm.remarks,
        enabled: compForm.enabled,
        vendor_id: compForm.vendor_id,
        vendor_name: compForm.vendor_name,
      };
      if (editingComp.kind === 'openai_compatible') {
        body.api_base = compForm.api_base.trim();
        body.model = compForm.model.trim();
        body.system_prompt = compForm.system_prompt || undefined;
        body.temperature = parseFloat(compForm.temperature) || undefined;
        body.max_tokens = parseInt(compForm.max_tokens) || undefined;
        body.response_path = compForm.response_path || undefined;
      } else {
        body.template_id = compForm.template_id || undefined;
      }
      r = await updateLocalComponent(editingComp.id, body);
    } else {
      if (compForm.kind === 'openai_compatible') {
        if (!compForm.api_base.trim()) {
          showToast('error', $_('my_components.fill_api_base'));
          return;
        }
        if (!compForm.model.trim()) {
          showToast('error', $_('my_components.fill_model'));
          return;
        }
      }
      const body: Parameters<typeof createLocalComponent>[0] = {
        id: compForm.id.trim(),
        name: compForm.name.trim(),
        enabled: compForm.enabled,
        vendor_id: compForm.vendor_id || undefined,
        vendor_name: compForm.vendor_name || undefined,
        kind: compForm.kind || 'text',
        remarks: compForm.remarks || undefined,
      };
      if (compForm.kind === 'openai_compatible') {
        body.api_base = compForm.api_base.trim();
        body.model = compForm.model.trim();
        body.system_prompt = compForm.system_prompt || undefined;
        body.temperature = parseFloat(compForm.temperature) || undefined;
        body.max_tokens = parseInt(compForm.max_tokens) || undefined;
        body.response_path = compForm.response_path || undefined;
      } else {
        body.template_id = compForm.template_id || undefined;
      }
      r = await createLocalComponent(body);
    }
    if (r.success) {
      showToast('success', editingComp ? $_('my_components.comp_updated') : $_('my_components.comp_created'));
      closeComponentModal();
      await loadLocalComps();
    } else {
      showToast('error', $_('common.operation_failed'), (r as any).error?.message);
    }
  }

  async function deleteComp(id: string) {
    const r = await deleteLocalComponent(id);
    if (r.success) {
      showToast('success', $_('my_components.comp_deleted'));
      await loadLocalComps();
    } else {
      showToast('error', $_('common.delete_failed'), (r as any).error?.message);
    }
  }

  async function exportComp(id: string) {
    try {
      const r = await exportLocalComponent(id);
      if (r.success && r.data) {
        const blob = new Blob([JSON.stringify(r.data, null, 2)], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = `component-${id}.json`;
        a.click();
        URL.revokeObjectURL(url);
        showToast('success', $_('my_components.export_ok'));
      } else {
        showToast('error', $_('my_components.export_failed'), (r as any).error?.message);
      }
    } catch {
      showToast('error', $_('my_components.export_failed'));
    }
  }

  async function importCompFromFile() {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = '.json';
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      try {
        const text = await file.text();
        const data = JSON.parse(text);
        const r = await importLocalComponent(data);
        if (r.success) {
          showToast('success', $_('my_components.import_ok', { values: { id: r.data.id } }) + (r.data.overwrite ? ' - ' + $_('my_components.import_overwritten') : ''));
          await loadLocalComps();
        } else {
          showToast('error', $_('my_components.import_failed'), (r as any).error?.message);
        }
      } catch (e) {
        showToast('error', $_('my_components.import_failed'), $_('my_components.file_parse_error'));
      }
    };
    input.click();
  }

  async function setCompEnabled(comp: LocalComp, enabled: boolean) {
    const r = await updateLocalComponent(comp.id, { enabled });
    if (r.success) {
      showToast('success', enabled ? $_('my_components.comp_enabled') : $_('my_components.comp_disabled'));
      await loadLocalComps();
    } else {
      showToast('error', enabled ? $_('my_components.enable_failed') : $_('my_components.disable_failed'), (r as any).error?.message);
    }
  }

  function openAddVersion(comp: LocalComp) {
    editingVersion = null;
    versionForm = emptyVersionForm();
    versionModalCompId = comp.id;
    showVersionModal = true;
  }

  function openEditVersion(comp: LocalComp, ver: string, v: ComponentVersion) {
    editingVersion = { compId: comp.id, ver };
    versionForm = {
      version: ver,
      key_ids: v.key_ids.join(', '),
      key_selection_strategy: v.key_selection_strategy,
      auth_type: v.auth_type,
      proxy_profile_id: v.proxy_profile_id ?? '',
      remarks: v.remarks ?? '',
    };
    versionModalCompId = comp.id;
    showVersionModal = true;
  }

  async function saveVersion(compId: string) {
    const keyIds = versionForm.key_ids
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean);
    if (editingVersion) {
      const r = await updateComponentVersion(editingVersion.compId, editingVersion.ver, {
        key_ids: keyIds,
        key_selection_strategy: versionForm.key_selection_strategy,
        auth_type: versionForm.auth_type,
        proxy_profile_id: versionForm.proxy_profile_id || undefined,
        remarks: versionForm.remarks || undefined,
      });
      if (r.success) {
        showToast('success', $_('my_components.version_updated'));
        closeVersionModal();
        await loadLocalComps();
      } else {
        showToast('error', $_('my_components.version_update_failed'), (r as any).error?.message);
      }
    } else {
      const r = await createComponentVersion(compId, {
        version: versionForm.version,
        key_ids: keyIds,
        key_selection_strategy: versionForm.key_selection_strategy,
        auth_type: versionForm.auth_type,
        proxy_profile_id: versionForm.proxy_profile_id || undefined,
        remarks: versionForm.remarks || undefined,
      });
      if (r.success) {
        showToast('success', $_('my_components.version_added'));
        closeVersionModal();
        await loadLocalComps();
      } else {
        showToast('error', $_('my_components.version_add_failed'), (r as any).error?.message);
      }
    }
  }

  async function deleteVersion(compId: string, ver: string) {
    const r = await deleteComponentVersion(compId, ver);
    if (r.success) {
      showToast('success', $_('my_components.version_deleted'));
      await loadLocalComps();
    }
  }

  async function testVersion(compId: string, ver: string) {
    testingVer = `${compId}:${ver}`;
    try {
      const r = await testComponentVersion(compId, ver, 'Hello World', 'en_US', 'zh_CN');
      if (r.success && r.data)
        showToast('success', $_('my_components.test_result', { values: { text: r.data.translated_text, ms: r.data.elapsed_ms } }));
      else showToast('error', $_('my_components.test_failed'), (r as any).error?.message);
    } finally {
      testingVer = null;
    }
  }

  function openTestFileModal(comp: LocalComp) {
    testFileModal = {
      open: true,
      componentId: comp.id,
      componentName: comp.name,
      fileUrl: 'http://127.0.0.1:9090/api/v1/test-file/1024',
      sourceLang: 'en',
      targetLang: 'zh-CN',
      loading: false,
      result: null,
      error: null,
    };
  }

  async function runFileTest() {
    testFileModal.loading = true;
    testFileModal.result = null;
    testFileModal.error = null;
    try {
      const r = await testLocalComponentFile({
        component_id: testFileModal.componentId,
        file_url: testFileModal.fileUrl,
        source_lang: testFileModal.sourceLang,
        target_lang: testFileModal.targetLang,
      });
      if (r.success && r.data) {
        testFileModal.result = r.data;
      } else {
        testFileModal.error = fallbackErrorMessage((r as any).error?.message, $_('my_components.test_failed'));
      }
    } catch (e: any) {
      testFileModal.error = fallbackErrorMessage(e?.message, $_('my_components.request_failed'));
    } finally {
      testFileModal.loading = false;
    }
  }

  function getBindingPoolSummary(compId: string): { keys: number; oauth: number } {
    const binding = ($status as any)?.component_bindings?.components?.[compId];
    return {
      keys: Array.isArray(binding?.key_ids) ? binding.key_ids.length : 0,
      oauth: Array.isArray(binding?.oauth_ids) ? binding.oauth_ids.length : 0,
    };
  }

  async function runQuickTest(compId: string) {
    if (!quickTestState.apiKey.trim()) {
      showToast('error', $_('my_components.fill_api_key'));
      return;
    }
    quickTestState.loading = true;
    quickTestState.result = null;
    quickTestState.error = null;
    quickTestState.compId = compId;
    try {
      const r = await quickTestLocalComponent(compId, {
        api_key: quickTestState.apiKey,
        text: quickTestState.text,
        source_lang: 'en_US',
        target_lang: quickTestState.targetLang,
      });
      if (r.success && r.data) {
        quickTestState.result = r.data;
      } else {
        quickTestState.error = fallbackErrorMessage((r as any).error?.message, $_('my_components.test_failed'));
      }
    } catch (e: any) {
      quickTestState.error = fallbackErrorMessage(e?.message, $_('my_components.request_failed'));
    } finally {
      quickTestState.loading = false;
    }
  }

  function applyPreset(preset: OpenAiPreset) {
    compForm.api_base = preset.api_base;
    compForm.model = preset.model;
    if (!compForm.name.trim()) compForm.name = preset.label;
    if (!compForm.id.trim()) {
      compForm.id = preset.label.toLowerCase().replace(/[^a-z0-9]/g, '-');
    }
  }

  function closeComponentModal() {
    showCompModal = false;
  }

  function closeVersionModal() {
    showVersionModal = false;
    editingVersion = null;
    versionModalCompId = '';
  }

  function closeAuthModal() {
    showAuthModal = false;
    keyDropdownOpen = false;
    oauthDropdownOpen = false;
  }

  function closeFileTestModal() {
    testFileModal.open = false;
  }

  onMount(() => {
    loadLocalComps();
  });
</script>

<div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
  <div class="px-5 py-3 border-b border-gray-100 space-y-2">
    <div class="flex items-center justify-between">
      <h3 class="font-medium text-gray-900 text-sm">{$_('my_components.local_instances', { values: { count: localTotal } })}</h3>
      <div class="flex gap-2">
        <button onclick={importCompFromFile} class="px-3 py-1.5 bg-gray-100 text-gray-700 text-xs rounded-lg hover:bg-gray-200 transition-colors">
          {$_('my_components.import')}
        </button>
        <button onclick={openCreateComp} class="px-3 py-1.5 bg-blue-600 text-white text-xs rounded-lg hover:bg-blue-700 transition-colors">
          {$_('my_components.create')}
        </button>
      </div>
    </div>
    <div class="flex gap-2 items-center">
      <input bind:value={localSearchQ} oninput={onSearchInput} placeholder={$_('my_components.search_placeholder')} class="flex-1 border border-gray-200 rounded-lg px-3 py-1.5 text-sm" />
      <select bind:value={localKindFilter} onchange={onKindFilterChange} class="border border-gray-200 rounded-lg px-3 py-1.5 text-sm text-gray-600">
        <option value="">{$_('my_components.filter_all_kinds')}</option>
        {#each localAvailableKinds as k}
          <option value={k}>{k === 'openai_compatible' ? 'LLM' : k}</option>
        {/each}
      </select>
    </div>
  </div>
  {#if localCompsLoading}
    <div class="py-8 text-center text-gray-400 text-sm">{$_('common.loading')}</div>
  {:else if localComps.length === 0}
    <div class="py-8 text-center text-gray-400 text-sm">{$_('my_components.no_comps')}</div>
  {:else}
    {#each localComps as comp}
      {@const poolSummary = getBindingPoolSummary(comp.id)}
      <div class="border-t border-gray-50">
        <div class="flex items-center px-4 py-3 hover:bg-gray-50/50">
          <button onclick={() => (expandedCompId = expandedCompId === comp.id ? null : comp.id)} class="flex-1 flex items-center gap-3 text-left min-w-0">
            <span class="text-gray-400 text-xs">{expandedCompId === comp.id ? '▼' : '▶'}</span>
            <div class="min-w-0">
              <div class="text-sm font-medium text-gray-900">{comp.name}</div>
              <div class="text-xs text-gray-400 font-mono">{comp.id}
                {#if comp.vendor_name}
                  <span class="ml-2 text-gray-300">·</span>
                  <span class="text-gray-400">{comp.vendor_name}</span>
                {/if}
              </div>
              <div class="text-[11px] text-gray-400 mt-0.5">
                {#if comp.source_template_id}
                  <span>{$_('my_components.source_template')}<span class="font-mono">{comp.source_template_id}</span></span>
                {:else}
                  <span>{$_('my_components.source_local')}</span>
                {/if}
                {#if comp.source_template_api_version}
                  <span class="ml-2 text-gray-300">·</span>
                  <span>API: <span class="font-mono">{comp.source_template_api_version}</span></span>
                {/if}
                {#if comp.updated_at}
                  <span class="ml-2 text-gray-300">·</span>
                  <span>{$_('my_components.updated_at')}{formatTsLabel(comp.updated_at)}</span>
                {/if}
              </div>
            </div>
            <span class="text-xs px-2 py-0.5 rounded-full shrink-0 {localCapabilityBadgeClass(localCapability(comp))}">
              {localCapability(comp)}
            </span>
            {#if comp.kind === 'openai_compatible'}
              <span class="text-xs px-2 py-0.5 rounded-full shrink-0 bg-emerald-50 text-emerald-600">LLM Builder</span>
            {/if}
            <span class="text-xs px-2 py-0.5 rounded-full shrink-0 {comp.enabled ? 'bg-emerald-50 text-emerald-700' : 'bg-rose-50 text-rose-700'}">{comp.enabled ? $_('common.enabled') : $_('common.disabled')}</span>
            <span class="text-xs text-gray-400 shrink-0">{Object.keys(comp.versions).length} {$_('my_components.versions_count')}</span>
            {#if poolSummary.keys > 0}
              <span class="text-xs px-2 py-0.5 bg-blue-50 text-blue-600 rounded-full shrink-0 font-mono">Key×{poolSummary.keys}</span>
            {/if}
            {#if poolSummary.oauth > 0}
              <span class="text-xs px-2 py-0.5 bg-purple-50 text-purple-600 rounded-full shrink-0 font-mono">OAuth×{poolSummary.oauth}</span>
            {/if}
          </button>
          <div class="flex gap-2 ml-3">
            {#if comp.enabled && isNonTextKind(comp.kind)}
              <button onclick={() => openTestFileModal(comp)} class="text-xs text-emerald-600 hover:text-emerald-700">{$_('my_components.test_file')}</button>
            {/if}
            {#if comp.kind !== 'openai_compatible'}
              <button onclick={() => refreshCompSnapshot(comp)} class="text-xs text-sky-600 hover:text-sky-700">{$_('my_components.refresh_snapshot')}</button>
            {/if}
            <button onclick={() => setCompEnabled(comp, !comp.enabled)} class="text-xs {comp.enabled ? 'text-amber-600 hover:text-amber-700' : 'text-emerald-600 hover:text-emerald-700'}">
              {comp.enabled ? $_('my_components.disable') : $_('my_components.enable')}
            </button>
            <button onclick={() => openEditComp(comp)} class="text-xs text-gray-500 hover:text-gray-700">{$_('common.edit')}</button>
            <button onclick={() => exportComp(comp.id)} class="text-xs text-indigo-500 hover:text-indigo-700">{$_('my_components.export')}</button>
            <button onclick={() => deleteComp(comp.id)} class="text-xs text-red-500 hover:text-red-600">{$_('common.delete')}</button>
          </div>
        </div>
        {#if expandedCompId === comp.id}
          <div class="px-10 pb-3 bg-gray-50/50">
            <div class="flex items-center gap-2 mb-2">
              <span class="text-xs font-medium text-gray-600">{$_('my_components.version_list')}</span>
              <button onclick={() => openAddVersion(comp)} class="text-xs text-blue-600 hover:text-blue-700">{$_('my_components.add_version')}</button>
            </div>
            {#if Object.keys(comp.versions).length === 0}
              <p class="text-xs text-gray-400">{$_('my_components.no_versions')}</p>
            {:else}
              <div class="space-y-1">
                {#each Object.entries(comp.versions) as [ver, v]}
                  <div class="flex items-center gap-3 text-xs text-gray-600 bg-white rounded-lg px-3 py-2 border border-gray-100">
                    <span class="font-mono font-medium">{ver}</span>
                    <span class="text-gray-400">{v.auth_type}</span>
                    {#if v.key_ids.length > 0}
                      <span class="text-gray-400">{v.key_ids.length} keys</span>
                    {/if}
                    <div class="ml-auto flex gap-2">
                      <button onclick={() => testVersion(comp.id, ver)} disabled={testingVer === `${comp.id}:${ver}`} class="text-blue-600 hover:text-blue-700 disabled:opacity-50">
                        {testingVer === `${comp.id}:${ver}` ? $_('my_components.testing') : $_('common.test')}
                      </button>
                      <button onclick={() => openEditVersion(comp, ver, v)} class="text-gray-500 hover:text-gray-700">{$_('common.edit')}</button>
                      <button onclick={() => deleteVersion(comp.id, ver)} class="text-red-500 hover:text-red-600">{$_('common.delete')}</button>
                    </div>
                  </div>
                {/each}
              </div>
            {/if}

            <div class="mt-4">
              <div class="flex items-center gap-2 mb-2">
                <span class="text-xs font-medium text-gray-600">{$_('my_components.auth_creds')}</span>
                <button onclick={() => openAuthModal(comp)} class="text-xs text-blue-600 hover:text-blue-700">{$_('my_components.edit_auth')}</button>
              </div>
              {#if ($status as any)?.component_bindings?.components?.[comp.id]?.auth && Object.keys(($status as any).component_bindings.components[comp.id].auth).length > 0}
                <div class="space-y-1">
                  {#each Object.keys(($status as any).component_bindings.components[comp.id].auth) as k}
                    <div class="flex items-center gap-2 text-xs bg-white rounded-lg px-3 py-1.5 border border-gray-100">
                      <span class="font-mono text-gray-500">{k}</span>
                      <span class="text-gray-300">:</span>
                      <span class="font-mono text-gray-400">***</span>
                    </div>
                  {/each}
                </div>
              {:else}
                <p class="text-xs text-gray-400">{$_('my_components.no_auth')}</p>
              {/if}
            </div>

            {#if comp.kind === 'openai_compatible'}
              <div class="mt-4">
                <div class="flex items-center gap-2 mb-2">
                  <span class="text-xs font-medium text-gray-600">{$_('my_components.quick_test')}</span>
                  <span class="text-xs text-gray-400">{$_('my_components.no_key_needed')}</span>
                </div>
                <div class="space-y-2">
                  <input bind:value={quickTestState.apiKey} type="password" placeholder="API Key (sk-...)" class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
                  <div class="flex gap-2">
                    <input bind:value={quickTestState.text} placeholder={$_('my_components.test_text')} class="flex-1 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
                    <input bind:value={quickTestState.targetLang} placeholder={$_('my_components.target_lang')} class="w-24 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
                    <button
                      onclick={() => runQuickTest(comp.id)}
                      disabled={quickTestState.loading && quickTestState.compId === comp.id}
                      class="px-3 py-2 bg-emerald-600 text-white text-xs rounded-lg hover:bg-emerald-700 disabled:opacity-50 shrink-0">
                      {quickTestState.loading && quickTestState.compId === comp.id ? $_('my_components.testing') + '...' : $_('common.test')}
                    </button>
                  </div>
                  {#if quickTestState.result && quickTestState.compId === comp.id}
                    <div class="p-2 bg-green-50 border border-green-200 rounded-lg text-xs">
                      <span class="text-green-700 font-medium">{$_('my_components.test_result_label', { values: { ms: quickTestState.result.elapsed_ms } })}</span>
                      <span class="text-green-600">{quickTestState.result.translated_text}</span>
                    </div>
                  {/if}
                  {#if quickTestState.error && quickTestState.compId === comp.id}
                    <div class="p-2 bg-red-50 border border-red-200 rounded-lg text-xs text-red-600">{quickTestState.error}</div>
                  {/if}
                </div>
              </div>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
  {/if}
</div>

{#if localTotalPages > 1}
  <div class="flex items-center justify-between px-4 py-3 mt-2">
    <span class="text-xs text-gray-500">{$_('my_components.pagination', { values: { total: localTotal, page: localPage, pages: localTotalPages } })}</span>
    <div class="flex gap-1">
      <button onclick={() => { localPage = Math.max(1, localPage - 1); loadLocalComps(); }} disabled={localPage <= 1} class="px-3 py-1 text-xs border border-gray-200 rounded-lg hover:bg-gray-50 disabled:opacity-30 disabled:cursor-not-allowed">{$_('common.prev_page')}</button>
      <button onclick={() => { localPage = Math.min(localTotalPages, localPage + 1); loadLocalComps(); }} disabled={localPage >= localTotalPages} class="px-3 py-1 text-xs border border-gray-200 rounded-lg hover:bg-gray-50 disabled:opacity-30 disabled:cursor-not-allowed">{$_('common.next_page')}</button>
    </div>
  </div>
{/if}

<ComponentModal
  open={showCompModal}
  {editingComp}
  bind:compForm
  presets={OAI_PRESETS}
  {componentModalFieldId}
  {handleBackdropKeydown}
  onApplyPreset={applyPreset}
  onSave={saveComp}
  onClose={closeComponentModal}
/>

<VersionModal
  open={showVersionModal}
  component={currentVersionModalComp}
  bind:editingVersion
  bind:versionForm
  {versionModalFieldId}
  {handleBackdropKeydown}
  onSave={saveVersion}
  onClose={closeVersionModal}
/>

<AuthBindingModal
  open={showAuthModal}
  component={authModalComp}
  {authModalSupportedModes}
  bind:authFields
  {editableParamPaths}
  bind:overrideRequestUrl
  bind:overrideRequestHeadersJson
  bind:overrideRequestBodyJson
  bind:overrideMaxInputChars
  bind:overrideRateLimitQps
  bind:overrideMaxConcurrentRequests
  bind:overrideMaxFileSizeMb
  bind:authPoolKeyIds
  bind:authPoolOAuthIds
  bind:authPoolStrategy
  bind:authPoolExpanded
  {availableKeys}
  {availableOAuthConfigs}
  {poolResourcesLoading}
  bind:keyDropdownOpen
  bind:oauthDropdownOpen
  {authModeEnabled}
  {toggleKeyId}
  {toggleOAuthId}
  {isEditableParam}
  {authModalFieldId}
  {handleBackdropKeydown}
  onLoadPoolResources={loadPoolResources}
  onSave={saveAuthBinding}
  onClear={clearAuthBinding}
  onClose={closeAuthModal}
/>

<FileTestModal
  bind:testFileModal
  {testFileFieldId}
  {handleBackdropKeydown}
  onRun={runFileTest}
  onClose={closeFileTestModal}
/>
