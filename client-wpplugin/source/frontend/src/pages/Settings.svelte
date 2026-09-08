<script lang="ts">
  import { onMount } from 'svelte';
  import { apiFetch, hasData, isOk } from '../lib/api/client';
  import type { UpdateCheckResult } from '../lib/api/types';
  import { status, fetchStatus } from '../lib/stores/status';
  import { showToast } from '../lib/stores/toast';
  import { RefreshCw, ArrowUpCircle, CheckCircle, ExternalLink } from 'lucide-svelte';
  import {
    createProxyProfile,
    deleteProxyProfile,
    getAccessControl,
    listProxyProfiles,
    loadLogSettings,
    testProxyProfile,
    updateAccessControl,
    updateLogSettings,
    updateProxyProfile,
  } from '../lib/api/settings';
  import {
    getWorkerConfig,
    saveWorkerConfig as persistWorkerConfig,
    type WorkerConfigData,
  } from '../lib/api/worker';
  import { _ } from 'svelte-i18n';

  type Tab = 'proxy' | 'worker' | 'log' | 'access' | 'about';
  let activeTab = $state<Tab>('proxy');

  function handleBackdropKeydown(event: KeyboardEvent, close: () => void) {
    if (event.key === 'Escape' || event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      close();
    }
  }

  function settingsFieldId(field: string): string {
    return `settings-${field}`;
  }

  interface ProxyItem {
    id: string; name: string; protocol: string;
    host: string; port: number; has_auth: boolean; enabled: boolean;
  }

  // --- 代理配置 ---
  let proxies = $state<ProxyItem[]>([]);
  let proxiesLoading = $state(false);
  let showProxyModal = $state(false);
  let editingProxy = $state<ProxyItem | null>(null);
  let proxyForm = $state({ id: '', name: '', protocol: 'http', host: '', port: '8080', username: '', password: '', enabled: true });
  let testingProxyId = $state<string | null>(null);

  async function loadProxies() {
    proxiesLoading = true;
    try {
      const r = await listProxyProfiles();
      if (hasData(r)) proxies = r.data.items;
    } finally { proxiesLoading = false; }
  }

  function openCreateProxy() {
    editingProxy = null;
    proxyForm = { id: '', name: '', protocol: 'http', host: '', port: '8080', username: '', password: '', enabled: true };
    showProxyModal = true;
  }
  function openEditProxy(p: ProxyItem) {
    editingProxy = p;
    proxyForm = { id: p.id, name: p.name, protocol: p.protocol, host: p.host, port: String(p.port), username: '', password: '', enabled: p.enabled };
    showProxyModal = true;
  }

  async function saveProxy() {
    if (!proxyForm.host.trim()) { showToast('error', $_('settings.host_required')); return; }
    let r;
    if (editingProxy) {
      r = await updateProxyProfile(editingProxy.id, {
        name: proxyForm.name || undefined,
        protocol: proxyForm.protocol,
        host: proxyForm.host.trim(),
        port: parseInt(proxyForm.port) || 8080,
        username: proxyForm.username || undefined,
        password: proxyForm.password || undefined,
        enabled: proxyForm.enabled,
      });
    } else {
      if (!proxyForm.id.trim()) { showToast('error', $_('settings.profile_id_required')); return; }
      r = await createProxyProfile({
        id: proxyForm.id.trim(), name: proxyForm.name || proxyForm.id,
        protocol: proxyForm.protocol, host: proxyForm.host.trim(),
        port: parseInt(proxyForm.port) || 8080, username: proxyForm.username || undefined,
        password: proxyForm.password || undefined, enabled: proxyForm.enabled,
      });
    }
    if (isOk(r)) { showToast('success', editingProxy ? $_('settings.proxy_updated') : $_('settings.proxy_created')); showProxyModal = false; await loadProxies(); }
    else showToast('error', $_('common.operation_failed'), r.error?.message);
  }

  async function deleteProxy(id: string) {
    const r = await deleteProxyProfile(id);
    if (isOk(r)) { showToast('success', $_('settings.proxy_deleted')); await loadProxies(); }
    else showToast('error', $_('settings.delete_failed'), r.error?.message);
  }

  async function testProxy(id: string) {
    testingProxyId = id;
    try {
      const r = await testProxyProfile(id);
      if (isOk(r) && r.data?.reachable) showToast('success', $_('settings.proxy_reachable', { values: { ip: r.data.ip ?? $_('settings.unknown') } }));
      else showToast('error', $_('settings.proxy_unreachable'), isOk(r) ? undefined : r.error?.message);
    } finally { testingProxyId = null; }
  }

  // --- Worker 配置 ---
  let pollSeconds = $state('20');
  let autoStartWorker = $state(false);
  let domainConcurrency = $state('3');
  let relationConcurrency = $state('1');
  let globalTranslationConcurrency = $state('30');
  let globalCallbackConcurrency = $state('12');
  let relationMaxPendingCallbacks = $state('200');
  let adaptiveRateControl = $state(true);
  let adaptiveMaxDelayMs = $state('5000');
  let callbackConcurrency = $state('4');
  let callbackTimeoutSecs = $state('30');
  let callbackRetryMax = $state('2');
  let fetchTimeoutSecs = $state('20');
  let fetchRetryMax = $state('2');
  let reviewMode = $state(false);
  let workflowDefaultMode = $state<'auto' | 'review'>('auto');
  let workflowStepMedia = $state(true);
  let workflowStepReview = $state(true);
  let workflowOptionalStepOrder = $state<Array<'media' | 'review'>>(['media', 'review']);
  let workflowByDomainText = $state('');
  let workflowByFormatText = $state('');
  let workflowAdvancedOpen = $state(false);
  let workflowPolicyJson = $state('');
  let workflowDslJson = $state('');
  let workerConfigLoading = $state(false);

  type WorkflowStepUi = { id: string; type: string; when?: string | null };

  function mapToLines(map: Record<string, unknown> | null | undefined): string {
    if (!map || typeof map !== 'object') return '';
    return Object.entries(map)
      .map(([k, v]) => `${k}=${String(v)}`)
      .join('\n');
  }

  function linesToMap(text: string): Record<string, string> {
    const out: Record<string, string> = {};
    for (const line of text.split('\n')) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith('#')) continue;
      const eq = trimmed.indexOf('=');
      if (eq <= 0) continue;
      const key = trimmed.slice(0, eq).trim();
      const val = trimmed.slice(eq + 1).trim().toLowerCase();
      if (!key || (val !== 'auto' && val !== 'review')) continue;
      out[key] = val;
    }
    return out;
  }

  function defaultWorkflowSteps(): WorkflowStepUi[] {
    const steps: WorkflowStepUi[] = [
      { id: 'translate', type: 'builtin' },
    ];
    for (const stepId of workflowOptionalStepOrder) {
      if (stepId === 'media' && workflowStepMedia) {
        steps.push({ id: 'media', type: 'media' });
      }
      if (stepId === 'review' && workflowStepReview) {
        steps.push({ id: 'human_review', type: 'human_review', when: 'policy' });
      }
    }
    steps.push({ id: 'sync', type: 'builtin' });
    return steps;
  }

  function moveOptionalWorkflowStep(stepId: 'media' | 'review', direction: -1 | 1) {
    const order = [...workflowOptionalStepOrder];
    const index = order.indexOf(stepId);
    if (index < 0) return;
    const target = index + direction;
    if (target < 0 || target >= order.length) return;
    [order[index], order[target]] = [order[target], order[index]];
    workflowOptionalStepOrder = order;
  }

  function buildWorkflowDslFromUi(): Record<string, unknown> {
    return {
      schema_version: 'workflow-dsl-v1',
      steps: defaultWorkflowSteps(),
    };
  }

  function buildWorkflowPolicyFromUi(): Record<string, unknown> {
    return {
      schema_version: 'workflow-policy-v1',
      default_mode: workflowDefaultMode,
      by_domain: linesToMap(workflowByDomainText),
      by_content_format: linesToMap(workflowByFormatText),
      by_rule: {},
    };
  }

  function applyWorkflowFromConfig(d: WorkerConfigData) {
    reviewMode = !!d.review_mode;
    const policy = d.workflow_policy as Record<string, unknown> | null | undefined;
    const dsl = d.workflow_dsl as { steps?: Array<{ id?: string; type?: string }> } | null | undefined;
    if (policy && typeof policy.default_mode === 'string') {
      workflowDefaultMode = policy.default_mode === 'review' ? 'review' : 'auto';
    } else {
      workflowDefaultMode = reviewMode ? 'review' : 'auto';
    }
    workflowByDomainText = mapToLines(policy?.by_domain as Record<string, unknown> | undefined);
    workflowByFormatText = mapToLines(policy?.by_content_format as Record<string, unknown> | undefined);
    if (dsl?.steps && Array.isArray(dsl.steps) && dsl.steps.length > 0) {
      workflowStepMedia = dsl.steps.some((s) => s.id === 'media' || s.type === 'media');
      workflowStepReview = dsl.steps.some(
        (s) => s.id === 'human_review' || s.id === 'review' || s.type === 'human_review'
      );
      const optionalOrder: Array<'media' | 'review'> = [];
      for (const step of dsl.steps) {
        if (step.id === 'media' || step.type === 'media') optionalOrder.push('media');
        if (
          step.id === 'human_review' ||
          step.id === 'review' ||
          step.type === 'human_review'
        ) {
          optionalOrder.push('review');
        }
      }
      if (optionalOrder.length > 0) workflowOptionalStepOrder = optionalOrder;
    } else {
      workflowStepMedia = true;
      workflowStepReview = true;
    }
    workflowPolicyJson = JSON.stringify(policy ?? buildWorkflowPolicyFromUi(), null, 2);
    workflowDslJson = JSON.stringify(dsl ?? buildWorkflowDslFromUi(), null, 2);
  }

  $effect(() => { if ($status?.worker_loop_poll_seconds) pollSeconds = String($status.worker_loop_poll_seconds); });

  onMount(async () => {
    await loadWorkerConfig();
  });

  async function loadWorkerConfig() {
    workerConfigLoading = true;
    try {
      const r = await getWorkerConfig();
      if (isOk(r)) {
        const d = r.data as WorkerConfigData;
        if (d.poll_seconds != null) pollSeconds = String(d.poll_seconds);
        autoStartWorker = !!d.auto_start_worker;
        if (d.domain_concurrency != null) domainConcurrency = String(d.domain_concurrency);
        if (d.relation_concurrency != null) relationConcurrency = String(d.relation_concurrency);
        if (d.global_translation_concurrency != null) globalTranslationConcurrency = String(d.global_translation_concurrency);
        if (d.global_callback_concurrency != null) globalCallbackConcurrency = String(d.global_callback_concurrency);
        if (d.relation_max_pending_callbacks != null) relationMaxPendingCallbacks = String(d.relation_max_pending_callbacks);
        if (d.adaptive_rate_control != null) adaptiveRateControl = !!d.adaptive_rate_control;
        if (d.adaptive_max_delay_ms != null) adaptiveMaxDelayMs = String(d.adaptive_max_delay_ms);
        if (d.callback_concurrency != null) callbackConcurrency = String(d.callback_concurrency);
        if (d.callback_timeout_secs != null) callbackTimeoutSecs = String(d.callback_timeout_secs);
        if (d.callback_retry_max != null) callbackRetryMax = String(d.callback_retry_max);
        if (d.fetch_timeout_secs != null) fetchTimeoutSecs = String(d.fetch_timeout_secs);
        if (d.fetch_retry_max != null) fetchRetryMax = String(d.fetch_retry_max);
        applyWorkflowFromConfig(d);
      }
    } finally { workerConfigLoading = false; }
  }

  async function saveWorkerConfig() {
    const secs = Math.max(1, Math.min(3600, parseInt(pollSeconds) || 20));
    let workflow_policy: Record<string, unknown> = buildWorkflowPolicyFromUi();
    let workflow_dsl: Record<string, unknown> = buildWorkflowDslFromUi();
    if (workflowAdvancedOpen) {
      if (workflowPolicyJson.trim()) {
        try {
          workflow_policy = JSON.parse(workflowPolicyJson) as Record<string, unknown>;
        } catch {
          showToast('error', 'Workflow policy JSON is invalid');
          return;
        }
      }
      if (workflowDslJson.trim()) {
        try {
          workflow_dsl = JSON.parse(workflowDslJson) as Record<string, unknown>;
        } catch {
          showToast('error', 'Workflow DSL JSON is invalid');
          return;
        }
      }
    } else {
      workflowPolicyJson = JSON.stringify(workflow_policy, null, 2);
      workflowDslJson = JSON.stringify(workflow_dsl, null, 2);
    }
    reviewMode = workflowDefaultMode === 'review' || workflowStepReview;
    const r = await persistWorkerConfig({
      poll_seconds: secs,
      auto_start_worker: autoStartWorker,
      domain_concurrency: Math.max(1, parseInt(domainConcurrency) || 3),
      relation_concurrency: Math.max(1, parseInt(relationConcurrency) || 1),
      global_translation_concurrency: Math.max(1, parseInt(globalTranslationConcurrency) || 30),
      global_callback_concurrency: Math.max(1, parseInt(globalCallbackConcurrency) || 12),
      relation_max_pending_callbacks: Math.max(1, parseInt(relationMaxPendingCallbacks) || 200),
      adaptive_rate_control: adaptiveRateControl,
      adaptive_max_delay_ms: Math.max(200, parseInt(adaptiveMaxDelayMs) || 5000),
      callback_concurrency: Math.max(1, parseInt(callbackConcurrency) || 4),
      callback_timeout_secs: Math.max(1, parseInt(callbackTimeoutSecs) || 30),
      callback_retry_max: Math.max(0, parseInt(callbackRetryMax) || 2),
      fetch_timeout_secs: Math.max(1, parseInt(fetchTimeoutSecs) || 20),
      fetch_retry_max: Math.max(0, parseInt(fetchRetryMax) || 2),
      review_mode: reviewMode,
      workflow_policy,
      workflow_dsl,
    });
    if (isOk(r)) { showToast('success', $_('settings.worker_saved')); await fetchStatus(); }
    else showToast('error', $_('common.save_failed'), r.error?.message);
  }

  // --- 日志设置 ---
  let logEnabled = $state(true);
  let logLevel = $state('info');
  let logSaving = $state(false);

  async function loadLogConfig() {
    const r = await loadLogSettings();
    if (r.success && r.data) {
      logEnabled = r.data.enabled;
      logLevel = r.data.level;
    }
  }

  async function saveLogConfig() {
    logSaving = true;
    try {
      const r = await updateLogSettings(logEnabled, logLevel);
      if (r.success) showToast('success', $_('settings.log_saved'));
      else showToast('error', $_('common.save_failed'), (r as any).error?.message);
    } finally { logSaving = false; }
  }

  // --- 访问控制 ---
  let acExternalAccess = $state(false);
  let acAllowedIps = $state('');
  let acCurrentBind = $state('127.0.0.1');
  let acLoading = $state(false);
  let acSaving = $state(false);

  async function loadAccessControl() {
    acLoading = true;
    try {
      const r = await getAccessControl();
      if (r.success && r.data) {
        acExternalAccess = r.data.external_access;
        acAllowedIps = r.data.allowed_ips.join('\n');
        acCurrentBind = r.data.current_bind;
      }
    } finally { acLoading = false; }
  }

  async function saveAccessControl() {
    acSaving = true;
    try {
      const ips = acAllowedIps.split('\n').map(s => s.trim()).filter(s => s.length > 0);
      const r = await updateAccessControl(acExternalAccess, ips);
      if (r.success && r.data) {
        if (r.data.restart_required) {
          showToast('success', $_('settings.access_saved'), $_('settings.restart_required'));
        } else {
          showToast('success', $_('settings.access_saved'));
        }
        await loadAccessControl();
      } else {
        showToast('error', $_('common.save_failed'), (r as any).error?.message);
      }
    } finally { acSaving = false; }
  }

  onMount(() => { loadProxies(); loadLogConfig(); loadAccessControl(); });

  // --- 版本更新 ---
  let updateChecking = $state(false);
  let updateInfo: UpdateCheckResult | null = $state(null);
  let updateError = $state('');
  let updating = $state(false);

  async function checkForUpdate() {
    updateChecking = true;
    updateError = '';
    updateInfo = null;
    const r = await apiFetch<UpdateCheckResult>('/api/update-check');
    if (isOk(r)) {
      updateInfo = r.data;
    } else {
      updateError = r.error?.message || '';
    }
    updateChecking = false;
  }

  async function performUpdate() {
    updating = true;
    updateError = '';
    const r = await apiFetch('/api/perform-update', { method: 'POST' });
    if (isOk(r)) {
      const kind = (r.data as { update_kind?: string } | undefined)?.update_kind;
      if (kind === 'ui') {
        showToast('success', $_('settings.update_started'));
        updating = false;
        await checkForUpdate();
      } else {
        showToast('success', $_('settings.update_started'));
        // Binary path: service restarts — page disconnects
      }
    } else {
      updateError = r.error?.message || $_('settings.update_failed');
      updating = false;
    }
  }
</script>

<div class="mb-6">
  <h2 class="text-xl font-semibold text-gray-900">{$_('settings.title')}</h2>
  <p class="text-sm text-gray-500 mt-1">{$_('settings.subtitle')}</p>
</div>

<!-- Tabs -->
<div class="flex border-b border-gray-200 mb-4">
  {#each [['proxy', $_('settings.tab_proxy')],['worker', $_('settings.tab_worker')],['log', $_('settings.tab_log')],['access', $_('settings.tab_access')],['about', $_('settings.tab_about')]] as [id, label]}
    <button
      data-testid={`settings-tab-${id}`}
      onclick={() => activeTab = id as Tab}
      class="px-4 py-2.5 text-sm font-medium border-b-2 transition-colors mr-1
        {activeTab === id ? 'border-blue-600 text-blue-600' : 'border-transparent text-gray-500 hover:text-gray-700'}">
      {label}
    </button>
  {/each}
</div>

<!-- 代理配置 Tab -->
{#if activeTab === 'proxy'}
  <div class="bg-white border border-gray-200 rounded-xl overflow-hidden">
    <div class="px-5 py-3 border-b border-gray-100 flex items-center justify-between">
      <h3 class="font-medium text-gray-900 text-sm">{$_('settings.proxy_title')}</h3>
      <div class="flex gap-2">
        <button onclick={loadProxies} class="text-xs border border-gray-200 px-3 py-1.5 rounded-lg hover:bg-gray-50 text-gray-600">{$_('common.refresh')}</button>
        <button onclick={openCreateProxy} class="px-3 py-1.5 bg-blue-600 text-white text-xs rounded-lg hover:bg-blue-700">{$_('settings.add_proxy')}</button>
      </div>
    </div>
    <table class="w-full text-sm">
      <thead>
        <tr class="text-xs text-gray-500 bg-gray-50">
          <th class="px-4 py-2.5 text-left font-medium">ID</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('settings.th_name')}</th>
          <th class="px-4 py-2.5 text-left font-medium">{$_('settings.th_address')}</th>
          <th class="px-4 py-2.5 text-center font-medium">{$_('settings.th_status')}</th>
          <th class="px-4 py-2.5 text-right font-medium">{$_('settings.th_actions')}</th>
        </tr>
      </thead>
      <tbody>
        {#if proxiesLoading}
          <tr><td colspan="5" class="px-4 py-8 text-center text-gray-400">{$_('common.loading')}</td></tr>
        {:else if proxies.length === 0}
          <tr><td colspan="5" class="px-4 py-8 text-center text-gray-400">{$_('settings.no_proxies')}</td></tr>
        {:else}
          {#each proxies as p}
            <tr class="border-t border-gray-50 hover:bg-gray-50/50">
              <td class="px-4 py-3 font-mono text-xs text-gray-600">{p.id}</td>
              <td class="px-4 py-3 text-sm">{p.name}</td>
              <td class="px-4 py-3 text-xs font-mono text-gray-500">{p.protocol}://{p.host}:{p.port}</td>
              <td class="px-4 py-3 text-center">
                <span class="text-xs px-2 py-0.5 rounded-full {p.enabled ? 'bg-green-100 text-green-700' : 'bg-gray-100 text-gray-500'}">
                  {p.enabled ? $_('settings.enabled') : $_('settings.disabled')}
                </span>
              </td>
              <td class="px-4 py-3 text-right">
                <button onclick={() => testProxy(p.id)} disabled={testingProxyId === p.id}
                  class="text-xs text-blue-600 hover:text-blue-700 mr-2 disabled:opacity-50">
                  {testingProxyId === p.id ? $_('settings.testing') : $_('settings.test')}
                </button>
                <button onclick={() => openEditProxy(p)} class="text-xs text-gray-500 hover:text-gray-700 mr-2">{$_('settings.edit')}</button>
                <button onclick={() => deleteProxy(p.id)} class="text-xs text-red-500 hover:text-red-600">{$_('settings.delete')}</button>
              </td>
            </tr>
          {/each}
        {/if}
      </tbody>
    </table>
  </div>

  {#if showProxyModal}
    <div class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
      role="button"
      tabindex="0"
      aria-label={$_('settings.close_proxy_modal')}
      onclick={(e) => { if (e.target === e.currentTarget) showProxyModal = false; }}
      onkeydown={(e) => handleBackdropKeydown(e, () => { showProxyModal = false; })}>
      <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-md">
        <h3 class="font-semibold text-gray-900 mb-4">{editingProxy ? $_('settings.edit_proxy') : $_('settings.add_proxy_title')}</h3>
        <div class="space-y-3">
          {#if !editingProxy}
            <input bind:value={proxyForm.id} placeholder={$_('settings.profile_id_placeholder')} class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
          {/if}
          <input bind:value={proxyForm.name} placeholder={$_('settings.name_placeholder')} class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          <div class="flex gap-2">
            <select bind:value={proxyForm.protocol} class="border border-gray-200 rounded-lg px-3 py-2 text-sm w-28">
              {#each ['http','https','socks5','socks5h'] as p}
                <option value={p}>{p}</option>
              {/each}
            </select>
            <input bind:value={proxyForm.host} placeholder={$_('settings.host_placeholder')} class="flex-1 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            <input bind:value={proxyForm.port} placeholder={$_('settings.port_placeholder')} type="number" class="w-20 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div class="flex gap-2">
            <input bind:value={proxyForm.username} placeholder={$_('settings.username_placeholder')} class="flex-1 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            <input bind:value={proxyForm.password} type="password" placeholder={$_('settings.password_placeholder')} class="flex-1 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <label class="flex items-center gap-2 text-sm">
            <input type="checkbox" bind:checked={proxyForm.enabled} /> {$_('settings.enabled')}
          </label>
        </div>
        <div class="flex gap-2 mt-5">
          <button onclick={saveProxy} class="flex-1 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">{$_('common.save')}</button>
          <button onclick={() => showProxyModal = false} class="flex-1 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50">{$_('common.cancel')}</button>
        </div>
      </div>
    </div>
  {/if}

<!-- Worker 配置 Tab -->
{:else if activeTab === 'worker'}
  <div class="bg-white border border-gray-200 rounded-xl p-5 max-w-lg">
    <h3 class="font-medium text-gray-900 mb-4">{$_('settings.worker_params')}</h3>
    {#if workerConfigLoading}
      <p class="text-sm text-gray-400">{$_('common.loading')}</p>
    {:else}
    <div class="space-y-5">

      <!-- 自动启动 -->
      <div class="flex items-center justify-between">
        <div>
          <div class="text-sm font-medium text-gray-700">{$_('settings.auto_start_worker')}</div>
          <div class="text-xs text-gray-400 mt-0.5">{$_('settings.auto_start_desc')}</div>
        </div>
        <button onclick={() => autoStartWorker = !autoStartWorker}
          aria-label={autoStartWorker ? $_('settings.disable_auto_start') : $_('settings.enable_auto_start')}
          class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none
            {autoStartWorker ? 'bg-blue-600' : 'bg-gray-200'}">
          <span class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform
            {autoStartWorker ? 'translate-x-6' : 'translate-x-1'}"></span>
        </button>
      </div>

      <!-- 翻译审核模式（兼容旧开关；保存时与 workflow 对齐） -->
      <div class="flex items-center justify-between">
        <div>
          <div class="text-sm font-medium text-gray-700">{$_('settings.review_mode')}</div>
          <div class="text-xs text-gray-400 mt-0.5">{$_('settings.review_mode_desc')}</div>
        </div>
        <button
          data-testid="settings-review-toggle"
          onclick={() => {
            reviewMode = !reviewMode;
            workflowDefaultMode = reviewMode ? 'review' : 'auto';
            workflowStepReview = reviewMode;
          }}
          aria-label={reviewMode ? $_('settings.disable_review') : $_('settings.enable_review')}
          class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none
            {reviewMode ? 'bg-amber-500' : 'bg-gray-200'}">
          <span class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform
            {reviewMode ? 'translate-x-6' : 'translate-x-1'}"></span>
        </button>
      </div>

      <!-- Visual workflow pipeline -->
      <div class="pt-3 border-t border-gray-100">
        <p class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-3">Workflow pipeline</p>
        <div class="mb-3">
          <label for={settingsFieldId('workflow-default-mode')} class="block text-xs font-medium text-gray-600 mb-1">Default mode</label>
          <select
            id={settingsFieldId('workflow-default-mode')}
            bind:value={workflowDefaultMode}
            onchange={() => {
              reviewMode = workflowDefaultMode === 'review';
              if (workflowDefaultMode === 'review') workflowStepReview = true;
            }}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm"
          >
            <option value="auto">Auto (translate → sync)</option>
            <option value="review">Review (hold for approval)</option>
          </select>
        </div>
        <div class="mb-3 grid gap-3 sm:grid-cols-2">
          <div>
            <label for={settingsFieldId('workflow-by-domain')} class="block text-xs font-medium text-gray-600 mb-1">
              By domain (one per line: domain=auto|review)
            </label>
            <textarea
              id={settingsFieldId('workflow-by-domain')}
              bind:value={workflowByDomainText}
              rows="3"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-xs font-mono"
              placeholder="shop.example.com=review"
            ></textarea>
          </div>
          <div>
            <label for={settingsFieldId('workflow-by-format')} class="block text-xs font-medium text-gray-600 mb-1">
              By content_format (one per line: format=auto|review)
            </label>
            <textarea
              id={settingsFieldId('workflow-by-format')}
              bind:value={workflowByFormatText}
              rows="3"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-xs font-mono"
              placeholder="json_structured=review"
            ></textarea>
          </div>
        </div>
        <ol class="space-y-2">
          <li class="flex items-center gap-3 rounded-lg border border-gray-100 bg-gray-50 px-3 py-2 text-sm">
            <span class="w-5 text-xs text-gray-400">1</span>
            <span class="font-medium text-gray-800">Translate</span>
            <span class="ml-auto text-xs text-gray-400">required</span>
          </li>
          <li class="flex items-center gap-3 rounded-lg border border-gray-200 px-3 py-2 text-sm">
            <span class="w-5 text-xs text-gray-400">2</span>
            <label class="flex flex-1 items-center gap-2 cursor-pointer">
              <input type="checkbox" bind:checked={workflowStepMedia} class="rounded border-gray-300" />
              <span class="font-medium text-gray-800">Media</span>
            </label>
            <div class="flex items-center gap-1">
              <button
                type="button"
                class="rounded border border-gray-200 px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-50 disabled:opacity-40"
                disabled={workflowOptionalStepOrder.indexOf('media') === 0}
                onclick={() => moveOptionalWorkflowStep('media', -1)}
                aria-label="Move media step up"
              >↑</button>
              <button
                type="button"
                class="rounded border border-gray-200 px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-50 disabled:opacity-40"
                disabled={workflowOptionalStepOrder.indexOf('media') === workflowOptionalStepOrder.length - 1}
                onclick={() => moveOptionalWorkflowStep('media', 1)}
                aria-label="Move media step down"
              >↓</button>
            </div>
            <span class="text-xs text-gray-400">upload/prepare</span>
          </li>
          <li class="flex items-center gap-3 rounded-lg border border-gray-200 px-3 py-2 text-sm">
            <span class="w-5 text-xs text-gray-400">3</span>
            <label class="flex flex-1 items-center gap-2 cursor-pointer">
              <input type="checkbox" bind:checked={workflowStepReview} class="rounded border-gray-300"
                onchange={() => { if (workflowStepReview) reviewMode = true; }} />
              <span class="font-medium text-gray-800">Human review</span>
            </label>
            <div class="flex items-center gap-1">
              <button
                type="button"
                class="rounded border border-gray-200 px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-50 disabled:opacity-40"
                disabled={workflowOptionalStepOrder.indexOf('review') === 0}
                onclick={() => moveOptionalWorkflowStep('review', -1)}
                aria-label="Move review step up"
              >↑</button>
              <button
                type="button"
                class="rounded border border-gray-200 px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-gray-50 disabled:opacity-40"
                disabled={workflowOptionalStepOrder.indexOf('review') === workflowOptionalStepOrder.length - 1}
                onclick={() => moveOptionalWorkflowStep('review', 1)}
                aria-label="Move review step down"
              >↓</button>
            </div>
            <span class="text-xs text-gray-400">when policy</span>
          </li>
          <li class="flex items-center gap-3 rounded-lg border border-gray-100 bg-gray-50 px-3 py-2 text-sm">
            <span class="w-5 text-xs text-gray-400">4</span>
            <span class="font-medium text-gray-800">Sync</span>
            <span class="ml-auto text-xs text-gray-400">required</span>
          </li>
        </ol>
        <button
          type="button"
          class="mt-3 text-xs text-blue-600 hover:underline"
          onclick={() => {
            workflowAdvancedOpen = !workflowAdvancedOpen;
            if (workflowAdvancedOpen) {
              workflowPolicyJson = JSON.stringify(buildWorkflowPolicyFromUi(), null, 2);
              workflowDslJson = JSON.stringify(buildWorkflowDslFromUi(), null, 2);
            }
          }}
        >
          {workflowAdvancedOpen ? 'Hide advanced JSON' : 'Show advanced JSON'}
        </button>
        {#if workflowAdvancedOpen}
          <div class="mt-3 space-y-3">
            <div>
              <label for={settingsFieldId('workflow-policy')} class="block text-xs font-medium text-gray-600 mb-1">
                Workflow policy (JSON)
              </label>
              <textarea
                id={settingsFieldId('workflow-policy')}
                bind:value={workflowPolicyJson}
                rows="4"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-xs font-mono"
              ></textarea>
            </div>
            <div>
              <label for={settingsFieldId('workflow-dsl')} class="block text-xs font-medium text-gray-600 mb-1">
                Workflow DSL (JSON)
              </label>
              <textarea
                id={settingsFieldId('workflow-dsl')}
                bind:value={workflowDslJson}
                rows="5"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-xs font-mono"
              ></textarea>
            </div>
          </div>
        {/if}
      </div>

      <!-- 轮询间隔 -->
      <div>
        <label for={settingsFieldId('poll-seconds')} class="block text-sm font-medium text-gray-700 mb-1">{$_('settings.poll_interval')}</label>
        <div class="flex gap-2 items-center">
          <input id={settingsFieldId('poll-seconds')} bind:value={pollSeconds} type="number" min="1" max="3600"
            class="w-24 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          <span class="text-sm text-gray-500">{$_('settings.seconds_range')}</span>
        </div>
      </div>

      <!-- 调度预算 -->
      <div class="pt-3 border-t border-gray-100">
        <p class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-3">{$_('settings.scheduler_budget')}</p>
        <div class="grid grid-cols-2 gap-3">
          <div>
            <label for={settingsFieldId('domain-concurrency')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.domain_concurrency')}</label>
            <input id={settingsFieldId('domain-concurrency')} bind:value={domainConcurrency} type="number" min="1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={settingsFieldId('relation-concurrency')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.relation_concurrency')}</label>
            <input id={settingsFieldId('relation-concurrency')} bind:value={relationConcurrency} type="number" min="1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={settingsFieldId('global-translation-concurrency')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.global_translation_concurrency')}</label>
            <input id={settingsFieldId('global-translation-concurrency')} bind:value={globalTranslationConcurrency} type="number" min="1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={settingsFieldId('global-callback-concurrency')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.global_callback_concurrency')}</label>
            <input id={settingsFieldId('global-callback-concurrency')} bind:value={globalCallbackConcurrency} type="number" min="1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div class="col-span-2">
            <label for={settingsFieldId('relation-max-pending-callbacks')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.relation_max_pending')}</label>
            <input id={settingsFieldId('relation-max-pending-callbacks')} bind:value={relationMaxPendingCallbacks} type="number" min="1"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            <p class="text-xs text-gray-400 mt-1">{$_('settings.pending_threshold_desc')}</p>
          </div>
          <div class="col-span-2 p-3 rounded-lg border border-gray-200 bg-gray-50/60">
            <div class="flex items-center justify-between">
              <div>
                <div class="text-xs font-medium text-gray-700">{$_('settings.adaptive_rate')}</div>
                <div class="text-xs text-gray-500 mt-0.5">{$_('settings.adaptive_rate_desc')}</div>
              </div>
              <button onclick={() => adaptiveRateControl = !adaptiveRateControl}
                aria-label={adaptiveRateControl ? $_('settings.disable_adaptive') : $_('settings.enable_adaptive')}
                class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none
                  {adaptiveRateControl ? 'bg-blue-600' : 'bg-gray-200'}">
                <span class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform
                  {adaptiveRateControl ? 'translate-x-6' : 'translate-x-1'}"></span>
              </button>
            </div>
            <div class="mt-3">
              <label for={settingsFieldId('adaptive-max-delay-ms')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.max_backoff_delay')}</label>
              <input id={settingsFieldId('adaptive-max-delay-ms')} bind:value={adaptiveMaxDelayMs} type="number" min="200"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm bg-white" />
            </div>
          </div>
        </div>
      </div>

      <!-- 拉取阶段 -->
      <div class="pt-3 border-t border-gray-100">
        <p class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-3">{$_('settings.fetch_phase')}</p>
        <div class="grid grid-cols-2 gap-3">
          <div>
            <label for={settingsFieldId('fetch-timeout-secs')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.timeout_secs')}</label>
            <input id={settingsFieldId('fetch-timeout-secs')} bind:value={fetchTimeoutSecs} type="number" min="1" max="300"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={settingsFieldId('fetch-retry-max')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.retry_count')}</label>
            <input id={settingsFieldId('fetch-retry-max')} bind:value={fetchRetryMax} type="number" min="0" max="10"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
        </div>
      </div>

      <!-- 回调阶段 -->
      <div class="pt-3 border-t border-gray-100">
        <p class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-3">{$_('settings.callback_phase')}</p>
        <div class="grid grid-cols-3 gap-3">
          <div>
            <label for={settingsFieldId('callback-concurrency')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.concurrency')}</label>
            <input id={settingsFieldId('callback-concurrency')} bind:value={callbackConcurrency} type="number" min="1" max="50"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={settingsFieldId('callback-timeout-secs')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.timeout_secs')}</label>
            <input id={settingsFieldId('callback-timeout-secs')} bind:value={callbackTimeoutSecs} type="number" min="1" max="300"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={settingsFieldId('callback-retry-max')} class="block text-xs font-medium text-gray-600 mb-1">{$_('settings.retry_count')}</label>
            <input id={settingsFieldId('callback-retry-max')} bind:value={callbackRetryMax} type="number" min="0" max="10"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
        </div>
      </div>

      <!-- 环境变量说明 -->
      <div class="pt-2 border-t border-gray-100">
        <p class="text-xs text-gray-400 mb-2">{$_('settings.env_vars_desc')}</p>
        <div class="space-y-1.5 text-xs font-mono">
          {#each [
            ['WPTSALL_DOMAIN_CONCURRENCY', $_('settings.env_domain_concurrency')],
            ['WPTSALL_GLOBAL_TRANSLATION_CONCURRENCY', $_('settings.env_global_translation')],
            ['WPTSALL_GLOBAL_CALLBACK_CONCURRENCY', $_('settings.env_global_callback')],
            ['WPTSALL_RELATION_CONCURRENCY', $_('settings.env_relation_concurrency')],
            ['WPTSALL_RELATION_MAX_PENDING_CALLBACKS', $_('settings.env_relation_pending')],
            ['WPTSALL_ADAPTIVE_RATE_CONTROL', $_('settings.env_adaptive_rate')],
            ['WPTSALL_ADAPTIVE_MAX_DELAY_MS', $_('settings.env_adaptive_delay')],
            ['WPTSALL_DISCOVERY_MODE', $_('settings.env_discovery_mode')],
            ['WPTSALL_DEFAULT_MAX_INPUT_CHARS', $_('settings.env_max_input_chars')],
          ] as [env, desc]}
            <div class="flex gap-3 items-start">
              <span class="text-blue-600 shrink-0">{env}</span>
              <span class="text-gray-400">{desc}</span>
            </div>
          {/each}
        </div>
      </div>

      <button
        data-testid="settings-save-worker"
        onclick={saveWorkerConfig}
        class="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 transition-colors">
        {$_('settings.save_worker')}
      </button>
    </div>
    {/if}
  </div>

<!-- 日志设置 Tab -->
{:else if activeTab === 'log'}
  <div class="bg-white border border-gray-200 rounded-xl p-5 max-w-lg">
    <h3 class="font-medium text-gray-900 mb-4">{$_('settings.log_file_settings')}</h3>
    <div class="space-y-5">
      <div class="flex items-center justify-between">
        <div>
          <div class="text-sm font-medium text-gray-700">{$_('settings.enable_log_write')}</div>
          <div class="text-xs text-gray-400 mt-0.5">{$_('settings.log_write_desc')}</div>
        </div>
        <button
          onclick={() => logEnabled = !logEnabled}
          aria-label={logEnabled ? $_('settings.disable_log') : $_('settings.enable_log')}
          class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none
            {logEnabled ? 'bg-blue-600' : 'bg-gray-200'}">
          <span class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform
            {logEnabled ? 'translate-x-6' : 'translate-x-1'}"></span>
        </button>
      </div>

      <div>
        <label for={settingsFieldId('log-level')} class="block text-sm font-medium text-gray-700 mb-1">{$_('settings.min_log_level')}</label>
        <select id={settingsFieldId('log-level')} bind:value={logLevel}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm bg-white">
          {#each [['debug',$_('settings.log_debug')],['info',$_('settings.log_info')],['warn',$_('settings.log_warn')],['error',$_('settings.log_error')]] as [val, label]}
            <option value={val}>{label}</option>
          {/each}
        </select>
        <p class="text-xs text-gray-400 mt-1">{$_('settings.log_level_desc')}</p>
      </div>

      <div class="pt-2 border-t border-gray-100">
        <p class="text-xs text-gray-400">{$_('settings.log_path')}<span class="font-mono">./logs/wptsall-client.log</span></p>
      </div>

      <button onclick={saveLogConfig} disabled={logSaving}
        class="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors">
        {logSaving ? $_('settings.saving') : $_('settings.save_settings')}
      </button>
    </div>
  </div>

<!-- 访问控制 Tab -->
{:else if activeTab === 'access'}
  <div class="bg-white border border-gray-200 rounded-xl p-5 max-w-lg">
    <h3 class="font-medium text-gray-900 mb-4">{$_('settings.access_control')}</h3>
    {#if acLoading}
      <p class="text-sm text-gray-400">{$_('common.loading')}</p>
    {:else}
    <div class="space-y-5">

      <!-- 当前绑定地址 -->
      <div>
        <div class="text-sm font-medium text-gray-700 mb-1">{$_('settings.current_bind')}</div>
        <div class="text-sm font-mono text-gray-500 bg-gray-50 px-3 py-2 rounded-lg">{acCurrentBind}:8977</div>
      </div>

      <!-- 外网访问开关 -->
      <div class="flex items-center justify-between">
        <div>
          <div class="text-sm font-medium text-gray-700">{$_('settings.enable_external')}</div>
          <div class="text-xs text-gray-400 mt-0.5">{$_('settings.external_desc')}</div>
        </div>
        <button onclick={() => acExternalAccess = !acExternalAccess}
          aria-label={acExternalAccess ? $_('settings.disable_external') : $_('settings.enable_external')}
          class="relative inline-flex h-6 w-11 items-center rounded-full transition-colors focus:outline-none
            {acExternalAccess ? 'bg-red-500' : 'bg-gray-200'}">
          <span class="inline-block h-4 w-4 transform rounded-full bg-white shadow transition-transform
            {acExternalAccess ? 'translate-x-6' : 'translate-x-1'}"></span>
        </button>
      </div>

      {#if acExternalAccess}
        <div class="bg-amber-50 border border-amber-200 rounded-lg p-3">
          <p class="text-xs text-amber-700">
            {$_('settings.external_warning')}
          </p>
        </div>
      {/if}

      <!-- IP 白名单 -->
      <div>
        <label for={settingsFieldId('ip-whitelist')} class="block text-sm font-medium text-gray-700 mb-1">{$_('settings.ip_whitelist')}</label>
        <textarea id={settingsFieldId('ip-whitelist')} bind:value={acAllowedIps}
          placeholder={$_('settings.ip_placeholder')}
          rows="4"
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono resize-y"></textarea>
        <p class="text-xs text-gray-400 mt-1">{$_('settings.localhost_always_allowed')}</p>
      </div>

      <button onclick={saveAccessControl} disabled={acSaving}
        class="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-60 transition-colors">
        {acSaving ? $_('settings.saving') : $_('settings.save_access')}
      </button>
    </div>
    {/if}
  </div>

{:else if activeTab === 'about'}
  <!-- 版本更新 -->
  <div class="bg-white border border-gray-200 rounded-xl p-5 max-w-lg">
    <div class="flex items-center justify-between mb-4">
      <h3 class="font-medium text-gray-900 flex items-center gap-2 text-sm">
        <RefreshCw class="w-4 h-4 text-gray-400" />
        {$_('settings.version_update')}
      </h3>
      <button
        onclick={checkForUpdate}
        disabled={updateChecking || updating}
        class="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium rounded-lg border border-gray-200 text-gray-700 hover:bg-gray-50 transition-colors disabled:opacity-50 disabled:cursor-not-allowed">
        <RefreshCw size={12} class={updateChecking ? 'animate-spin' : ''} />
        {updateChecking ? $_('settings.checking') : $_('settings.check_for_updates')}
      </button>
    </div>

    {#if updateError}
      <div class="text-sm text-red-600 bg-red-50 rounded-lg p-3">{updateError}</div>
    {:else if updateInfo}
      <div class="text-xs text-gray-500 mb-3 space-y-1">
        <div>Binary: v{updateInfo.current_version}
          {#if updateInfo.latest_version && updateInfo.latest_version !== updateInfo.current_version}
            → v{updateInfo.latest_version}
          {/if}
        </div>
        {#if updateInfo.ui_current_version}
          <div>UI: v{updateInfo.ui_current_version}
            {#if updateInfo.ui_latest_version && updateInfo.ui_latest_version !== updateInfo.ui_current_version}
              → v{updateInfo.ui_latest_version}
            {/if}
          </div>
        {/if}
        {#if updateInfo.update_kind}
          <div>Kind: {updateInfo.update_kind}</div>
        {/if}
      </div>
      {#if updateInfo.update_available}
        <div class="space-y-3">
          <div class="flex items-center gap-2 text-sm">
            <ArrowUpCircle size={16} class="text-amber-500 flex-shrink-0" />
            <span class="text-gray-900">
              {$_('settings.new_version_available')}
              <span class="font-semibold text-amber-600">
                {#if updateInfo.update_kind === 'ui'}
                  UI v{updateInfo.ui_latest_version}
                {:else}
                  v{updateInfo.latest_version}
                {/if}
              </span>
            </span>
            {#if updateInfo.release_notes_url}
              <a href={updateInfo.release_notes_url} target="_blank" rel="noopener" class="text-blue-500 hover:text-blue-600">
                <ExternalLink size={12} />
              </a>
            {/if}
          </div>
          <div class="flex items-center gap-3">
            <button
              onclick={performUpdate}
              disabled={updating}
              class="flex items-center gap-1.5 px-4 py-2 text-xs font-medium rounded-lg bg-blue-600 hover:bg-blue-700 text-white transition-colors disabled:opacity-50">
              {#if updating}
                <RefreshCw size={12} class="animate-spin" />
                {$_('settings.updating')}
              {:else}
                <ArrowUpCircle size={12} />
                {$_('settings.update_now')}
              {/if}
            </button>
            {#if updating}
              <span class="text-xs text-gray-400">{$_('settings.update_restart_hint')}</span>
            {/if}
          </div>
        </div>
      {:else}
        <div class="flex items-center gap-2 text-sm text-emerald-600">
          <CheckCircle size={16} />
          <span>{$_('settings.already_up_to_date')}</span>
        </div>
      {/if}
    {:else if updateChecking}
      <div class="text-sm text-gray-400">{$_('settings.checking_update_hint')}</div>
    {/if}
  </div>
{/if}
