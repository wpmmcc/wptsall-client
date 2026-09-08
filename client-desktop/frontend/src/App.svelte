<script lang="ts">
  /**
   * Desktop App shell backed by Tauri commands.
   *
   * The commands proxy into the embedded shared WebUI runtime, so this UI uses
   * the same Protocol v2 worker, SQLite persistence, domain token bindings, and
   * component registry as the standalone WebUI client.
   */
  import { onMount } from 'svelte';
  import { apiFetch, isOk } from './lib/api/client';
  import type { UpdateCheckResult } from './lib/api/types';

  type SiteInfo = {
    id: string;
    domain: string;
    wp_url: string;
    connected: boolean;
    languages: string[];
  };

  // P1-A-10: `logged_in`/`email` are legacy server-control-plane fields kept
  // for type compatibility; the default local-first Desktop UI never renders
  // or requests them. `device_id`/`domains` remain in use locally.
  type AuthStatus = {
    logged_in: boolean;
    email?: string | null;
    device_id?: string | null;
    domains: Array<{ domain: string }>;
  };

  type ImportedSiteConnection = {
    api_base_url: string;
    token_prefix: string;
    token_len: number;
    route_secret_set?: boolean;
    pairing_claimed?: boolean;
  };

  type TaskSummary = {
    id: string;
    site_domain: string;
    status: string;
    progress: number;
    created_at: string;
  };

  type ComponentInfo = {
    id: string;
    name: string;
    provider_type: string;
    configured: boolean;
  };

  type ProviderCatalogItem = {
    entry_id: string;
    template_id: string;
    name: string;
    vendor_id: string;
    family: string;
    kind: string;
    supported_content_formats: string[];
    source: string;
    verified: boolean;
    evidence_tier?: string;
    requires_local_credentials: boolean;
    auth_fields?: string[];
    template?: {
      request?: { url?: string };
      response?: { translated_text_path?: string };
      [key: string]: unknown;
    };
  };

  type ProviderCatalogData = {
    schema: string;
    catalog_version: string | null;
    offline: boolean;
    items: ProviderCatalogItem[];
  };

  type WorkerStatus = {
    running: boolean;
    active_tasks: number;
    completed_total: number;
    uptime_seconds: number;
  };

  type WorkerRunSummary = {
    tasks_processed?: number;
    tasks_succeeded?: number;
    tasks_failed?: number;
    total_items?: number;
    break_reason?: string;
    [key: string]: unknown;
  };

  const importPackPlaceholder = '{"schema":"wptsall-site-connection.v1",...}';

  const pages = [
    { id: 'overview', label: 'Overview' },
    { id: 'sites', label: 'Sites' },
    { id: 'credentials', label: 'Credentials' },
    { id: 'tasks', label: 'Tasks' },
    { id: 'components', label: 'Components' },
    { id: 'providers', label: 'Providers' },
    { id: 'integration_pack', label: 'Integration pack' },
    { id: 'history', label: 'History' },
    { id: 'logs', label: 'Logs' },
    { id: 'settings', label: 'Settings' },
  ];

  let currentPage = $state('overview');
  let loading = $state(false);
  let globalError = $state('');
  let globalMessage = $state('');

  let statusInfo = $state<AuthStatus | null>(null);
  let worker = $state<WorkerStatus | null>(null);
  let sites = $state<SiteInfo[]>([]);
  let tasks = $state<TaskSummary[]>([]);
  let components = $state<ComponentInfo[]>([]);
  let providerCatalog = $state<ProviderCatalogData | null>(null);
  let providerCatalogBusy = $state(false);
  let providerCatalogRefreshing = $state(false);
  let providerCatalogInstalling = $state<string | null>(null);
  let wizardItem = $state<ProviderCatalogItem | null>(null);
  let wizardStep = $state<'install' | 'key' | 'test' | 'enable' | 'route' | 'done'>('install');
  let wizardBusy = $state(false);
  let wizardHint = $state('');
  let wizardLocalId = $state('');
  let wizardKeyId = $state('');
  let wizardAuthFields = $state<string[]>(['api_key']);
  let wizardAuthValues = $state<Record<string, string>>({});
  let wizardUrl = $state('');
  let wizardResponsePath = $state('');
  let wizardModel = $state('gpt-4o-mini');
  let wizardSlot = $state('plain_text');
  let wizardTestText = $state('Hello world');
  let wizardTestOut = $state('');

  function wizardPrimarySecret(): string {
    for (const name of wizardAuthFields) {
      const v = wizardAuthValues[name];
      if (v && v.trim()) return v.trim();
    }
    return '';
  }

  let integrationExportMode = $state<'public' | 'private'>('public');
  let integrationExportPassphrase = $state('');
  let integrationConfirmPrivate = $state(false);
  let integrationExportBusy = $state(false);
  let integrationExportJson = $state('');
  let integrationImportJson = $state('');
  let integrationImportPassphrase = $state('');
  let integrationOverwrite = $state(false);
  let integrationPreview = $state<Record<string, unknown> | null>(null);
  let integrationPreviewBusy = $state(false);
  let integrationImportBusy = $state(false);
  let integrationPreviewInput = $state('');
  let integrationPreviewPassphrase = $state('');

  let addWpUrl = $state('');
  let addToken = $state('');
  let addRouteSecret = $state('');
  let importPackJson = $state('');
  let importPairingCode = $state('');
  let importDeviceLabel = $state('');
  let importBusy = $state(false);
  let siteBusy = $state(false);
  let workerBusy = $state(false);
  let discoverBusy = $state(false);
  let runOnceBusy = $state(false);
  let workerRunSummary = $state<WorkerRunSummary | null>(null);
  let focusBusy = $state(false);
  let focusRelationId = $state('');
  let focusIncludeResync = $state(true);

  let componentBusy = $state(false);
  let quickTestBusy = $state<string | null>(null);
  let mockComponentId = $state('desktop-local-openai');
  let mockApiBase = $state('http://127.0.0.1:9090');
  let mockModel = $state('mock-openai-v1');
  let mockApiKey = $state('mock-translate-dev-key-2026');

  let vendorKeys = $state<Array<Record<string, unknown>>>([]);
  let vendorKeysBusy = $state(false);
  let vendorKeyVendorId = $state('openai');
  let vendorKeyLabel = $state('Desktop key');
  let vendorKeySecret = $state('');
  let vendorKeySaving = $state(false);

  let vendorOauthConfigs = $state<Array<Record<string, unknown>>>([]);
  let vendorOauthBusy = $state(false);
  let oauthId = $state('desktop-oauth-1');
  let oauthVendorId = $state('deepl');
  let oauthClientId = $state('');
  let oauthClientSecret = $state('');
  let oauthSaving = $state(false);

  let proxyProfiles = $state<Array<Record<string, unknown>>>([]);
  let proxyBusy = $state(false);
  let proxyId = $state('desktop-proxy-1');
  let proxyHost = $state('127.0.0.1');
  let proxyPort = $state('7890');
  let proxyName = $state('Desktop proxy');
  let proxySaving = $state(false);

  let selectedJobId = $state('');
  let jobItems = $state<Array<Record<string, unknown>>>([]);
  let jobItemsBusy = $state(false);
  let selectedItemId = $state('');
  let itemContent = $state<Record<string, unknown> | null>(null);
  let itemBusy = $state(false);
  let translatedDraft = $state('');

  let logLines = $state<string[]>([]);
  let logsBusy = $state(false);

  type HistoryRecord = {
    id: number;
    created_at?: number;
    domain?: string;
    object_type?: string;
    object_id?: number;
    source_lang?: string;
    target_lang?: string;
    status?: string;
    fields_count?: number;
    execution_ms?: number;
    error_message?: string;
    component_ids?: string[];
    failed_fields_count?: number;
  };
  let historyRecords = $state<HistoryRecord[]>([]);
  let historyTotal = $state(0);
  let historyBusy = $state(false);
  let historyDomain = $state('');
  let historyStatus = $state('');
  let historySearch = $state('');
  let historyPage = $state(1);
  let historySelected = $state<Set<number>>(new Set());
  let historyBatchBusy = $state(false);

  let updateChecking = $state(false);
  let updating = $state(false);
  let updateInfo = $state<UpdateCheckResult | null>(null);
  let updateError = $state('');
  let updateMessage = $state('');

  function setError(message: string) {
    globalError = message;
    globalMessage = '';
  }

  function setMessage(message: string) {
    globalMessage = message;
    globalError = '';
  }

  async function loadStatus() {
    const r = await apiFetch<AuthStatus>('/api/status');
    if (isOk(r)) {
      statusInfo = r.data;
    } else {
      setError(r.error?.message || 'Status request failed');
    }
  }

  async function loadWorkerStatus() {
    const r = await apiFetch<WorkerStatus>('/api/worker/status');
    if (isOk(r)) {
      worker = r.data;
    } else {
      setError(r.error?.message || 'Worker status request failed');
    }
  }

  async function loadSites() {
    const r = await apiFetch<SiteInfo[]>('/api/sites');
    if (isOk(r)) {
      sites = r.data;
    } else {
      setError(r.error?.message || 'Site list request failed');
    }
  }

  async function loadTasks() {
    const r = await apiFetch<TaskSummary[]>('/api/tasks');
    if (isOk(r)) {
      tasks = r.data;
    } else {
      setError(r.error?.message || 'Task list request failed');
    }
  }

  async function loadComponents() {
    const r = await apiFetch<ComponentInfo[]>('/api/components');
    if (isOk(r)) {
      components = r.data;
    } else {
      setError(r.error?.message || 'Component list request failed');
    }
  }

  async function loadVendorKeys() {
    vendorKeysBusy = true;
    const r = await apiFetch<{ items?: Array<Record<string, unknown>> } | Array<Record<string, unknown>>>(
      '/api/vendor-keys'
    );
    if (isOk(r)) {
      const data = r.data;
      vendorKeys = Array.isArray(data)
        ? data
        : Array.isArray(data?.items)
          ? data.items
          : [];
    } else {
      setError(r.error?.message || 'Vendor key list failed');
    }
    vendorKeysBusy = false;
  }

  async function saveVendorKey(event?: Event) {
    event?.preventDefault();
    if (!vendorKeySecret.trim()) {
      setError('API key secret is required');
      return;
    }
    vendorKeySaving = true;
    const r = await apiFetch<Record<string, unknown>>('/api/vendor-keys', {
      method: 'POST',
      body: {
        vendor_id: vendorKeyVendorId.trim(),
        label: vendorKeyLabel.trim() || 'Desktop key',
        api_key: vendorKeySecret.trim(),
      },
    });
    if (isOk(r)) {
      setMessage(`Vendor key saved for ${vendorKeyVendorId}`);
      vendorKeySecret = '';
      await loadVendorKeys();
    } else {
      setError(r.error?.message || 'Vendor key save failed');
    }
    vendorKeySaving = false;
  }

  async function deleteVendorKey(keyId: string) {
    vendorKeysBusy = true;
    const r = await apiFetch<Record<string, unknown>>(`/api/vendor-keys/${encodeURIComponent(keyId)}`, {
      method: 'DELETE',
    });
    if (isOk(r)) {
      setMessage(`Vendor key deleted: ${keyId}`);
      await loadVendorKeys();
    } else {
      setError(r.error?.message || 'Vendor key delete failed');
    }
    vendorKeysBusy = false;
  }

  async function loadVendorOauth() {
    vendorOauthBusy = true;
    const r = await apiFetch<{ items?: Array<Record<string, unknown>> } | Array<Record<string, unknown>>>(
      '/api/vendor-oauth'
    );
    if (isOk(r)) {
      const data = r.data;
      vendorOauthConfigs = Array.isArray(data)
        ? data
        : Array.isArray(data?.items)
          ? data.items
          : [];
    } else {
      setError(r.error?.message || 'Vendor OAuth list failed');
    }
    vendorOauthBusy = false;
  }

  async function saveVendorOauth(event?: Event) {
    event?.preventDefault();
    if (!oauthId.trim() || !oauthVendorId.trim()) {
      setError('OAuth id and vendor id are required');
      return;
    }
    oauthSaving = true;
    const r = await apiFetch<Record<string, unknown>>('/api/vendor-oauth', {
      method: 'POST',
      body: {
        id: oauthId.trim(),
        vendor_id: oauthVendorId.trim(),
        client_id: oauthClientId.trim() || undefined,
        client_secret: oauthClientSecret.trim() || undefined,
      },
    });
    if (isOk(r)) {
      setMessage(`Vendor OAuth saved: ${oauthId}`);
      oauthClientSecret = '';
      await loadVendorOauth();
    } else {
      setError(r.error?.message || 'Vendor OAuth save failed');
    }
    oauthSaving = false;
  }

  async function authorizeVendorOauth(id: string) {
    vendorOauthBusy = true;
    const r = await apiFetch<Record<string, unknown>>(
      `/api/vendor-oauth/${encodeURIComponent(id)}/authorize`,
      { method: 'POST', body: {} }
    );
    if (isOk(r)) {
      const url = typeof r.data?.authorize_url === 'string' ? r.data.authorize_url : '';
      setMessage(url ? `Authorize URL ready for ${id}` : `Authorize requested for ${id}`);
      if (url) {
        try {
          window.open(url, '_blank', 'noopener,noreferrer');
        } catch {
          /* ignore popup blockers */
        }
      }
      await loadVendorOauth();
    } else {
      setError(r.error?.message || 'Vendor OAuth authorize failed');
    }
    vendorOauthBusy = false;
  }

  async function deleteVendorOauth(id: string) {
    vendorOauthBusy = true;
    const r = await apiFetch<Record<string, unknown>>(`/api/vendor-oauth/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    });
    if (isOk(r)) {
      setMessage(`Vendor OAuth deleted: ${id}`);
      await loadVendorOauth();
    } else {
      setError(r.error?.message || 'Vendor OAuth delete failed');
    }
    vendorOauthBusy = false;
  }

  async function loadProxyProfiles() {
    proxyBusy = true;
    const r = await apiFetch<{ items?: Array<Record<string, unknown>> } | Array<Record<string, unknown>>>(
      '/api/proxy-profiles'
    );
    if (isOk(r)) {
      const data = r.data;
      proxyProfiles = Array.isArray(data)
        ? data
        : Array.isArray(data?.items)
          ? data.items
          : [];
    } else {
      setError(r.error?.message || 'Proxy profile list failed');
    }
    proxyBusy = false;
  }

  async function saveProxyProfile(event?: Event) {
    event?.preventDefault();
    if (!proxyId.trim() || !proxyHost.trim()) {
      setError('Proxy id and host are required');
      return;
    }
    proxySaving = true;
    const portNum = Number(proxyPort) || 0;
    const r = await apiFetch<Record<string, unknown>>('/api/proxy-profiles', {
      method: 'POST',
      body: {
        id: proxyId.trim(),
        name: proxyName.trim() || proxyId.trim(),
        host: proxyHost.trim(),
        port: portNum > 0 ? portNum : undefined,
        protocol: 'http',
        enabled: true,
      },
    });
    if (isOk(r)) {
      setMessage(`Proxy profile saved: ${proxyId}`);
      await loadProxyProfiles();
    } else {
      setError(r.error?.message || 'Proxy profile save failed');
    }
    proxySaving = false;
  }

  async function testProxyProfile(id: string) {
    proxyBusy = true;
    const r = await apiFetch<Record<string, unknown>>(
      `/api/proxy-profiles/${encodeURIComponent(id)}/test`,
      { method: 'POST', body: {} }
    );
    if (isOk(r)) {
      const reachable = r.data?.reachable === true;
      setMessage(
        reachable
          ? `Proxy ${id} reachable${r.data?.ip ? ` (${String(r.data.ip)})` : ''}`
          : `Proxy ${id} test completed`
      );
    } else {
      setError(r.error?.message || 'Proxy test failed');
    }
    proxyBusy = false;
  }

  async function deleteProxyProfile(id: string) {
    proxyBusy = true;
    const r = await apiFetch<Record<string, unknown>>(`/api/proxy-profiles/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    });
    if (isOk(r)) {
      setMessage(`Proxy profile deleted: ${id}`);
      await loadProxyProfiles();
    } else {
      setError(r.error?.message || 'Proxy profile delete failed');
    }
    proxyBusy = false;
  }

  async function loadJobItems(jobId: string) {
    selectedJobId = jobId;
    selectedItemId = '';
    itemContent = null;
    translatedDraft = '';
    jobItemsBusy = true;
    const r = await apiFetch<{ items?: Array<Record<string, unknown>> } | Array<Record<string, unknown>>>(
      `/api/jobs/${encodeURIComponent(jobId)}/items`
    );
    if (isOk(r)) {
      const data = r.data;
      jobItems = Array.isArray(data)
        ? data
        : Array.isArray(data?.items)
          ? data.items
          : [];
    } else {
      setError(r.error?.message || 'Job items request failed');
      jobItems = [];
    }
    jobItemsBusy = false;
  }

  async function openItemReview(itemId: string | number) {
    const id = String(itemId);
    selectedItemId = id;
    itemBusy = true;
    const r = await apiFetch<Record<string, unknown>>(`/api/items/${encodeURIComponent(id)}/content`);
    if (isOk(r)) {
      itemContent = r.data;
      const translated = r.data?.translated;
      translatedDraft =
        typeof translated === 'string'
          ? translated
          : translated
            ? JSON.stringify(translated, null, 2)
            : '';
    } else {
      setError(r.error?.message || 'Item content request failed');
      itemContent = null;
    }
    itemBusy = false;
  }

  async function saveItemTranslation() {
    if (!selectedItemId) return;
    itemBusy = true;
    let content: unknown = translatedDraft;
    try {
      content = JSON.parse(translatedDraft);
    } catch {
      content = translatedDraft;
    }
    const r = await apiFetch<Record<string, unknown>>(
      `/api/items/${encodeURIComponent(selectedItemId)}/translated`,
      { method: 'PUT', body: { content } }
    );
    if (isOk(r)) {
      setMessage(`Saved translation for item ${selectedItemId}`);
      await openItemReview(selectedItemId);
    } else {
      setError(r.error?.message || 'Save translation failed');
    }
    itemBusy = false;
  }

  async function approveSelectedItem() {
    if (!selectedItemId) return;
    itemBusy = true;
    const r = await apiFetch<Record<string, unknown>>(
      `/api/items/${encodeURIComponent(selectedItemId)}/approve`,
      { method: 'POST' }
    );
    if (isOk(r)) {
      setMessage(`Approved item ${selectedItemId}`);
      if (selectedJobId) await loadJobItems(selectedJobId);
    } else {
      setError(r.error?.message || 'Approve failed');
    }
    itemBusy = false;
  }

  async function retranslateSelectedItem() {
    if (!selectedItemId) return;
    itemBusy = true;
    const r = await apiFetch<Record<string, unknown>>(
      `/api/items/${encodeURIComponent(selectedItemId)}/retranslate`,
      { method: 'POST' }
    );
    if (isOk(r)) {
      setMessage(`Retranslate queued for item ${selectedItemId}`);
    } else {
      setError(r.error?.message || 'Retranslate failed');
    }
    itemBusy = false;
  }

  async function quickTestComponent(componentId: string) {
    quickTestBusy = componentId;
    const r = await apiFetch<Record<string, unknown>>(
      `/api/components/local/${encodeURIComponent(componentId)}/quick-test`,
      { method: 'POST', body: {} }
    );
    if (isOk(r)) {
      setMessage(`Quick-test OK: ${componentId}`);
    } else {
      setError(r.error?.message || `Quick-test failed: ${componentId}`);
    }
    quickTestBusy = null;
  }

  async function loadLogs() {
    logsBusy = true;
    const r = await apiFetch<{ lines?: string[] } | Array<{ message?: string }>>('/api/logs/recent', {
      method: 'POST',
      body: { limit: 200 },
    });
    if (isOk(r)) {
      const data = r.data;
      if (Array.isArray(data)) {
        logLines = data.map((row) => (typeof row === 'string' ? row : String(row?.message || '')));
      } else if (Array.isArray(data?.lines)) {
        logLines = data.lines.map(String);
      } else {
        logLines = [];
      }
    } else {
      setError(r.error?.message || 'Logs request failed');
    }
    logsBusy = false;
  }

  async function loadHistory() {
    historyBusy = true;
    const qs = new URLSearchParams();
    qs.set('page', String(historyPage));
    qs.set('limit', '20');
    if (historyDomain.trim()) qs.set('domain', historyDomain.trim());
    if (historyStatus.trim()) qs.set('status', historyStatus.trim());
    if (historySearch.trim()) qs.set('search', historySearch.trim());
    const r = await apiFetch<{
      records?: HistoryRecord[];
      total?: number;
      page?: number;
      limit?: number;
    }>(`/api/translations?${qs.toString()}`);
    if (isOk(r)) {
      historyRecords = Array.isArray(r.data?.records) ? r.data.records : [];
      historyTotal = Number(r.data?.total || 0);
      historySelected = new Set();
    } else {
      setError(r.error?.message || 'History request failed');
    }
    historyBusy = false;
  }

  function toggleHistorySelect(id: number) {
    const next = new Set(historySelected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    historySelected = next;
  }

  function toggleHistorySelectAll() {
    if (historySelected.size === historyRecords.length) {
      historySelected = new Set();
      return;
    }
    historySelected = new Set(historyRecords.map((row) => row.id));
  }

  async function batchRetryHistory() {
    const ids = [...historySelected];
    if (ids.length === 0) return;
    historyBatchBusy = true;
    const r = await apiFetch<{ queued?: number }>('/api/translations/batch-retry', {
      method: 'POST',
      body: { ids },
    });
    if (isOk(r)) {
      setMessage(`Queued ${r.data?.queued ?? ids.length} translation(s) for retry`);
      await loadHistory();
    } else {
      setError(r.error?.message || 'Batch retry failed');
    }
    historyBatchBusy = false;
  }

  async function batchDeleteHistory() {
    const ids = [...historySelected];
    if (ids.length === 0) return;
    if (!confirm(`Delete ${ids.length} translation record(s)?`)) return;
    historyBatchBusy = true;
    const r = await apiFetch<Record<string, unknown>>('/api/translations/batch-delete', {
      method: 'POST',
      body: { ids },
    });
    if (isOk(r)) {
      setMessage(`Deleted ${ids.length} translation record(s)`);
      await loadHistory();
    } else {
      setError(r.error?.message || 'Batch delete failed');
    }
    historyBatchBusy = false;
  }

  async function retryHistoryOne(id: number) {
    historyBatchBusy = true;
    const r = await apiFetch<Record<string, unknown>>(`/api/translations/${id}/retry`, {
      method: 'POST',
    });
    if (isOk(r)) {
      setMessage(`Queued retry for #${id}`);
      await loadHistory();
    } else {
      setError(r.error?.message || `Retry failed for #${id}`);
    }
    historyBatchBusy = false;
  }

  function formatHistoryTime(ts?: number): string {
    if (!ts) return '—';
    try {
      return new Date(ts * 1000).toLocaleString();
    } catch {
      return String(ts);
    }
  }

  async function loadProviderCatalog() {
    providerCatalogBusy = true;
    const r = await apiFetch<ProviderCatalogData>('/api/provider-catalog');
    if (isOk(r)) {
      providerCatalog = r.data;
    } else {
      setError(r.error?.message || 'Provider catalog request failed');
    }
    providerCatalogBusy = false;
  }

  async function refreshProviderCatalog() {
    providerCatalogRefreshing = true;
    const r = await apiFetch<Record<string, unknown>>('/api/provider-catalog/refresh', {
      method: 'POST',
      body: {},
    });
    if (isOk(r)) {
      setMessage(`Provider catalog verified: ${String(r.data.catalog_version || 'local')}`);
      await loadProviderCatalog();
    } else {
      setError(r.error?.message || 'Provider catalog verification failed');
    }
    providerCatalogRefreshing = false;
  }

  function catalogLocalId(item: ProviderCatalogItem) {
    return `${item.vendor_id || 'provider'}-${item.template_id}`
      .toLowerCase()
      .replace(/[^a-z0-9_-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 120);
  }

  async function installProviderTemplate(item: ProviderCatalogItem) {
    providerCatalogInstalling = item.entry_id;
    const r = await apiFetch<Record<string, unknown>>('/api/components/local/install-from-catalog', {
      method: 'POST',
      body: { entry_id: item.entry_id, local_id: catalogLocalId(item) },
    });
    if (isOk(r)) {
      setMessage(`Provider template installed: ${String(r.data.id || item.template_id)}`);
      await loadComponents();
    } else {
      setError(r.error?.message || 'Provider template installation failed');
    }
    providerCatalogInstalling = null;
  }

  function openProviderWizard(item: ProviderCatalogItem) {
    wizardItem = item;
    wizardStep = 'install';
    wizardHint = '';
    wizardLocalId = catalogLocalId(item);
    wizardKeyId = `${wizardLocalId}-key`.slice(0, 120);
    const auth = (item.template as { auth?: { fields?: Array<{ name?: string }> } } | undefined)?.auth;
    const fromTemplate = Array.isArray(auth?.fields)
      ? auth.fields
          .map((f) => (typeof f?.name === 'string' ? f.name.trim() : ''))
          .filter(Boolean)
      : [];
    const fields =
      fromTemplate.length > 0
        ? fromTemplate
        : Array.isArray(item.auth_fields) && item.auth_fields.length > 0
          ? item.auth_fields
          : ['api_key'];
    wizardAuthFields = fields;
    wizardAuthValues = Object.fromEntries(fields.map((f) => [f, '']));
    // Official catalog URL by default; Lab UI e2e overwrites from mock lab-provider-cases.
    const reqUrl = item.template?.request?.url;
    wizardUrl = typeof reqUrl === 'string' ? reqUrl : '';
    wizardResponsePath =
      typeof item.template?.response?.translated_text_path === 'string'
        ? item.template.response.translated_text_path
        : '';
    wizardModel = item.family === 'openai_compatible' ? 'mock-openai-v1' : 'gpt-4o-mini';
    wizardSlot = item.supported_content_formats?.[0] || 'plain_text';
    wizardTestText = 'Hello world';
    wizardTestOut = '';
  }

  async function wizardInstall() {
    if (!wizardItem) return;
    wizardBusy = true;
    wizardHint = '';
    const r = await apiFetch<Record<string, unknown>>('/api/components/local/install-from-catalog', {
      method: 'POST',
      body: { entry_id: wizardItem.entry_id, local_id: wizardLocalId, name: wizardItem.name },
    });
    if (isOk(r)) {
      wizardLocalId = String(r.data.id || wizardLocalId);
      wizardStep = 'key';
      setMessage(`Installed ${wizardLocalId}`);
    } else {
      setError(r.error?.message || 'Install failed');
    }
    wizardBusy = false;
  }

  async function wizardBindKey() {
    if (!wizardItem || !wizardPrimarySecret()) {
      setError('Credentials are required');
      return;
    }
    wizardBusy = true;
    wizardHint = '';
    const auth_values: Record<string, string> = {};
    for (const name of wizardAuthFields) {
      const v = (wizardAuthValues[name] || '').trim();
      if (v) auth_values[name] = v;
    }
    const keyRes = await apiFetch<Record<string, unknown>>('/api/vendor-keys', {
      method: 'POST',
      body: {
        id: wizardKeyId,
        vendor_id: wizardItem.vendor_id || 'custom',
        label: `${wizardItem.name || wizardItem.template_id} key`,
        auth_values,
        enabled: true,
      },
    });
    if (!isOk(keyRes)) {
      setError(keyRes.error?.message || 'Save key failed');
      wizardBusy = false;
      return;
    }
    await apiFetch(`/api/components/local/${encodeURIComponent(wizardLocalId)}/versions`, {
      method: 'POST',
      body: {
        version: 'v1',
        key_ids: [wizardKeyId],
        auth_type: 'key',
        remarks: 'desktop provider wizard',
      },
    });
    wizardStep = 'test';
    wizardBusy = false;
  }

  async function wizardQuickTest() {
    wizardBusy = true;
    wizardHint = '';
    wizardTestOut = '';
    const auth_values: Record<string, string> = {};
    for (const name of wizardAuthFields) {
      const v = (wizardAuthValues[name] || '').trim();
      if (v) auth_values[name] = v;
    }
    const config_overrides: Record<string, string> = {};
    if (wizardUrl.trim()) config_overrides['request.url'] = wizardUrl.trim();
    if (wizardResponsePath.trim()) {
      config_overrides['response.translated_text_path'] = wizardResponsePath.trim();
    }
    if (wizardModel.trim() && wizardItem?.family === 'openai_compatible') {
      config_overrides['request.body.model'] = wizardModel.trim();
    }
    const r = await apiFetch<{
      translated_text?: string;
      hint?: string;
      error?: string;
    }>(`/api/components/local/${encodeURIComponent(wizardLocalId)}/quick-test`, {
      method: 'POST',
      body: {
        api_key: wizardPrimarySecret(),
        auth_values,
        config_overrides,
        text: wizardTestText || 'Hello world',
        source_lang: 'en_US',
        target_lang: 'zh_CN',
      },
    });
    if (!isOk(r)) {
      setError(r.error?.message || 'Quick-test failed');
      wizardHint = (r.error as { hint?: string } | undefined)?.hint || '';
      wizardBusy = false;
      return;
    }
    if (r.data?.error) {
      setError(r.data.error);
      wizardHint = r.data.hint || '';
      wizardBusy = false;
      return;
    }
    wizardTestOut = r.data?.translated_text || 'ok';
    wizardHint = r.data?.hint || '';
    wizardStep = 'enable';
    wizardBusy = false;
  }

  async function wizardEnable() {
    wizardBusy = true;
    const r = await apiFetch(`/api/components/local/${encodeURIComponent(wizardLocalId)}`, {
      method: 'PUT',
      body: { enabled: true },
    });
    if (isOk(r)) {
      wizardStep = 'route';
    } else {
      setError(r.error?.message || 'Enable failed');
    }
    wizardBusy = false;
  }

  async function wizardRoute() {
    wizardBusy = true;
    const r = await apiFetch('/api/rule-component-bindings/upsert', {
      method: 'POST',
      body: {
        scope: 'global',
        slot_key: wizardSlot.trim() || 'plain_text',
        component_id: wizardLocalId,
      },
    });
    if (isOk(r)) {
      wizardStep = 'done';
      setMessage(
        `Wizard done: ${wizardLocalId} bound to ${wizardSlot}. review_mode controls auto vs manual WP callback.`,
      );
      await loadComponents();
    } else {
      setError(r.error?.message || 'Route binding failed');
    }
    wizardBusy = false;
  }

  function parseIntegrationPack(): Record<string, unknown> | null {
    if (!integrationImportJson.trim()) {
      setError('Paste an integration pack JSON first');
      return null;
    }
    try {
      const value: unknown = JSON.parse(integrationImportJson);
      if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error('Integration pack must be a JSON object');
      }
      return value as Record<string, unknown>;
    } catch (error) {
      setError(`Invalid integration pack JSON: ${String(error)}`);
      return null;
    }
  }

  function integrationPackWithPassphrase(pack: Record<string, unknown>) {
    const passphrase = integrationImportPassphrase.trim();
    return passphrase ? { ...pack, passphrase } : pack;
  }

  async function exportIntegrationPackDesktop() {
    integrationExportBusy = true;
    try {
      if (integrationExportMode === 'private') {
        if (!integrationConfirmPrivate) {
          setError('Confirm the private backup warning first');
          return;
        }
        if (integrationExportPassphrase.trim().length < 8) {
          setError('A passphrase of at least 8 characters is required');
          return;
        }
      }
      const r = await apiFetch<Record<string, unknown>>('/api/integrations/pack/export', {
        method: 'POST',
        body: {
          mode: integrationExportMode,
          ...(integrationExportMode === 'private'
            ? { confirm: true, passphrase: integrationExportPassphrase }
            : {}),
        },
      });
      if (isOk(r)) {
        integrationExportJson = JSON.stringify(r.data, null, 2);
        integrationExportPassphrase = '';
        setMessage('Integration pack exported');
      } else {
        setError(r.error?.message || 'Integration pack export failed');
      }
    } finally {
      integrationExportBusy = false;
    }
  }

  function downloadIntegrationPack() {
    if (!integrationExportJson) return;
    const blob = new Blob([integrationExportJson], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `wptsall-integration-pack-${integrationExportMode}.json`;
    link.click();
    URL.revokeObjectURL(url);
  }

  async function previewIntegrationPackDesktop(): Promise<boolean> {
    const pack = parseIntegrationPack();
    if (!pack) return false;
    integrationPreviewBusy = true;
    try {
      const r = await apiFetch<Record<string, unknown>>('/api/integrations/pack/preview', {
        method: 'POST',
        body: integrationPackWithPassphrase(pack),
      });
      if (isOk(r)) {
        integrationPreview = r.data;
        integrationPreviewInput = integrationImportJson;
        integrationPreviewPassphrase = integrationImportPassphrase;
        setMessage('Integration pack preview ready');
        return true;
      }
      setError(r.error?.message || 'Integration pack preview failed');
    } finally {
      integrationPreviewBusy = false;
    }
    return false;
  }

  async function importIntegrationPackDesktop() {
    if (!(await previewIntegrationPackDesktop())) return;
    if (integrationPreview?.safe_to_import !== true) {
      setError('Integration pack is not safe to import');
      return;
    }
    integrationImportBusy = true;
    try {
      const pack = parseIntegrationPack();
      if (!pack) return;
      const r = await apiFetch<Record<string, unknown>>('/api/integrations/pack/import', {
        method: 'POST',
        body: { ...integrationPackWithPassphrase(pack), overwrite: integrationOverwrite },
      });
      if (isOk(r)) {
        setMessage('Integration pack imported');
        integrationImportJson = '';
        integrationImportPassphrase = '';
        integrationPreview = null;
        integrationPreviewInput = '';
        integrationPreviewPassphrase = '';
        await loadAll();
      } else {
        setError(r.error?.message || 'Integration pack import failed');
      }
    } finally {
      integrationImportBusy = false;
    }
  }

  async function loadAll() {
    loading = true;
    globalError = '';
    try {
      await loadStatus();
      await loadWorkerStatus();
      await loadSites();
      await loadTasks();
      await loadComponents();
    } finally {
      loading = false;
    }
  }

  async function navigateTo(pageId: string) {
    currentPage = pageId;
    if (pageId === 'credentials') {
      await loadVendorKeys();
      await loadVendorOauth();
      await loadProxyProfiles();
    }
    if (pageId === 'logs') await loadLogs();
    if (pageId === 'history') await loadHistory();
    if (pageId === 'tasks' && selectedJobId) await loadJobItems(selectedJobId);
  }

  async function refreshDomains() {
    await loadAll();
    setMessage(`Local data refreshed: ${sites.length} sites`);
  }

  async function addSite(event: Event) {
    event.preventDefault();
    siteBusy = true;
    const r = await apiFetch<SiteInfo>('/api/sites/add', {
      method: 'POST',
      body: {
        request: {
          wp_url: addWpUrl,
          token: addToken,
          route_secret: addRouteSecret || null,
        },
      },
    });
    if (isOk(r)) {
      setMessage(`Site saved: ${r.data.domain || r.data.wp_url}`);
      addWpUrl = '';
      addToken = '';
      addRouteSecret = '';
      await loadAll();
    } else {
      setError(r.error?.message || 'Add site failed');
    }
    siteBusy = false;
  }

  async function importSiteConnection(event: Event) {
    event.preventDefault();
    if (!importPackJson.trim()) {
      setError('Paste a site connection pack JSON first');
      return;
    }
    let pack: unknown;
    try {
      pack = JSON.parse(importPackJson);
    } catch (err) {
      setError(`Invalid connection pack JSON: ${String(err)}`);
      return;
    }
    importBusy = true;
    const r = await apiFetch<ImportedSiteConnection>('/api/site-connections/import', {
      method: 'POST',
      body: {
        request: {
          site_connection_pack: pack,
          pairing_code: importPairingCode || undefined,
          device_label: importDeviceLabel || undefined,
        },
      },
    });
    if (isOk(r)) {
      setMessage(`Connection pack imported: ${r.data.api_base_url}`);
      importPackJson = '';
      importPairingCode = '';
      importDeviceLabel = '';
      await loadAll();
    } else {
      setError(r.error?.message || 'Connection pack import failed');
    }
    importBusy = false;
  }

  async function testSite(siteId: string) {
    siteBusy = true;
    const r = await apiFetch<boolean>('/api/sites/test', {
      method: 'POST',
      body: { siteId },
    });
    if (isOk(r)) {
      setMessage(r.data ? `Connection OK: ${siteId}` : `Connection failed: ${siteId}`);
    } else {
      setError(r.error?.message || `Connection failed: ${siteId}`);
    }
    siteBusy = false;
  }

  async function removeSite(siteId: string) {
    siteBusy = true;
    const r = await apiFetch<void>('/api/sites/remove', {
      method: 'POST',
      body: { siteId },
    });
    if (isOk(r)) {
      setMessage(`Site removed: ${siteId}`);
      await loadAll();
    } else {
      setError(r.error?.message || `Remove failed: ${siteId}`);
    }
    siteBusy = false;
  }

  async function startWorker() {
    workerBusy = true;
    const r = await apiFetch<void>('/api/worker/start', { method: 'POST' });
    if (isOk(r)) {
      setMessage('Worker started');
      await loadWorkerStatus();
    } else {
      setError(r.error?.message || 'Worker start failed');
    }
    workerBusy = false;
  }

  async function stopWorker() {
    workerBusy = true;
    const r = await apiFetch<void>('/api/worker/stop', { method: 'POST' });
    if (isOk(r)) {
      setMessage('Worker stopped');
      await loadWorkerStatus();
    } else {
      setError(r.error?.message || 'Worker stop failed');
    }
    workerBusy = false;
  }

  async function triggerDiscover(siteId = '') {
    discoverBusy = true;
    const r = await apiFetch<string>('/api/tasks/discover', {
      method: 'POST',
      body: { siteId },
    });
    if (isOk(r)) {
      setMessage(`Discovery queued: ${r.data}`);
      await loadTasks();
    } else {
      setError(r.error?.message || 'Discovery failed');
    }
    discoverBusy = false;
  }

  async function configureMockComponent(event?: Event) {
    event?.preventDefault();
    componentBusy = true;
    const r = await apiFetch<ComponentInfo>('/api/components/configure-mock', {
      method: 'POST',
      body: {
        request: {
          id: mockComponentId,
          api_base: mockApiBase,
          model: mockModel,
          api_key: mockApiKey,
        },
      },
    });
    if (isOk(r)) {
      setMessage(`Component configured: ${r.data.id}`);
      await loadComponents();
    } else {
      setError(r.error?.message || 'Component configuration failed');
    }
    componentBusy = false;
  }

  async function focusRelation(event?: Event) {
    event?.preventDefault();
    const relationId = Number.parseInt(focusRelationId, 10);
    if (!Number.isFinite(relationId) || relationId <= 0) {
      setError('Relation ID must be a positive integer');
      return;
    }
    focusBusy = true;
    const r = await apiFetch<{ relation_id: number; updated: number; enabled: number }>('/api/tasks/focus-relation', {
      method: 'POST',
      body: {
        request: {
          relation_id: relationId,
          include_resync: focusIncludeResync,
          selected_component_id: mockComponentId || null,
        },
      },
    });
    if (isOk(r)) {
      setMessage(`Focused relation ${r.data.relation_id}: enabled=${r.data.enabled}, updated=${r.data.updated}`);
    } else {
      setError(r.error?.message || 'Focus relation failed');
    }
    focusBusy = false;
  }

  async function runWorkerOnce() {
    runOnceBusy = true;
    workerRunSummary = null;
    const config = await apiFetch<WorkerRunSummary>('/api/worker/config', {
      method: 'POST',
      body: {
        request: {
          poll_seconds: 20,
          callback_concurrency: 4,
          callback_retry_max: 4,
          fetch_timeout_secs: 45,
          fetch_retry_max: 6,
          relation_max_pending_callbacks: 10000,
          global_callback_concurrency: 4,
        },
      },
    });
    if (!isOk(config)) {
      setError(config.error?.message || 'Worker config failed');
      runOnceBusy = false;
      return;
    }

    const r = await apiFetch<WorkerRunSummary>('/api/worker/run-once', {
      method: 'POST',
      body: {
        request: {
          max_iterations: 4,
          max_items_per_run: 64,
          max_elapsed_secs: 180,
        },
      },
    });
    if (isOk(r)) {
      workerRunSummary = r.data;
      setMessage(`Run once complete: processed=${Number(r.data.tasks_processed ?? r.data.total_items ?? 0)}, succeeded=${Number(r.data.tasks_succeeded ?? 0)}, failed=${Number(r.data.tasks_failed ?? 0)}`);
      await loadAll();
    } else {
      setError(r.error?.message || 'Worker run-once failed');
    }
    runOnceBusy = false;
  }

  async function checkForUpdate() {
    updateChecking = true;
    updateError = '';
    updateMessage = '';
    updateInfo = null;
    const r = await apiFetch<UpdateCheckResult>('/api/update-check');
    if (isOk(r)) {
      updateInfo = r.data;
    } else {
      updateError = r.error?.message || 'Check failed';
    }
    updateChecking = false;
  }

  async function performUpdate() {
    updating = true;
    updateError = '';
    updateMessage = '';
    const r = await apiFetch<{
      message: string;
      update_kind: string;
      current_version: string;
      target_version: string;
    }>('/api/perform-update', { method: 'POST' });
    if (isOk(r)) {
      updateMessage = r.data.message;
      if (r.data.update_kind === 'ui') {
        updating = false;
        await checkForUpdate();
      }
      // binary: app restarts — leave updating true
    } else {
      updateError = r.error?.message || 'Update failed';
      updating = false;
    }
  }

  onMount(() => {
    void loadAll();
    void loadProviderCatalog();
  });
</script>

<div class="flex h-screen">
  <nav class="w-60 bg-slate-900 text-white flex flex-col">
    <div class="p-4 border-b border-slate-700">
      <h1 class="text-lg font-bold">WPTSALL</h1>
      <p class="text-xs text-slate-400">Translation Client · Desktop</p>
    </div>
    <ul class="flex-1 py-2">
      {#each pages as page}
        <li>
          <button
            class="w-full px-4 py-2 text-left text-sm hover:bg-slate-700 transition-colors"
            class:bg-slate-700={currentPage === page.id}
            onclick={() => void navigateTo(page.id)}
          >
            {page.label}
          </button>
        </li>
      {/each}
    </ul>
    <div class="p-3 border-t border-slate-700 text-xs text-slate-400">
      v2.1.0 · embedded runtime
    </div>
  </nav>

  <main class="flex-1 overflow-auto p-6 space-y-5">
    <header class="flex items-center justify-between gap-4">
      <div>
        <h2 class="text-xl font-semibold capitalize">{currentPage}</h2>
        <p class="text-sm text-slate-500">Shared WebUI runtime through Tauri commands.</p>
      </div>
      <button
        class="text-xs px-3 py-2 border border-slate-200 rounded-lg hover:bg-slate-50 disabled:opacity-50"
        disabled={loading}
        onclick={loadAll}
      >
        {loading ? 'Refreshing…' : 'Refresh'}
      </button>
    </header>

    {#if globalError}
      <p class="text-sm text-red-700 bg-red-50 border border-red-100 rounded p-3">{globalError}</p>
    {/if}
    {#if globalMessage}
      <p class="text-sm text-green-700 bg-green-50 border border-green-100 rounded p-3">{globalMessage}</p>
    {/if}

    {#if currentPage === 'overview'}
      <section class="grid grid-cols-1 md:grid-cols-3 gap-4">
        <div class="card">
          <p class="label">Sites</p>
          <p class="value">{sites.length}</p>
          <p class="hint">Device-token bindings stored in local SQLite.</p>
        </div>
        <div class="card">
          <p class="label">Worker</p>
          <p class="value">{worker?.running ? 'Running' : 'Idle'}</p>
          <p class="hint">Completed total: {worker?.completed_total ?? 0}</p>
        </div>
        <div class="card">
          <p class="label">Device ID</p>
          <p class="value text-sm break-all">{statusInfo?.device_id || 'unknown'}</p>
          <p class="hint break-all">Use this ID in WordPress to issue a site connection pack.</p>
        </div>
      </section>

      <section class="card space-y-3">
        <h3 class="section-title">Quick actions</h3>
        <div class="flex flex-wrap gap-2">
          <button class="btn" disabled={loading} onclick={refreshDomains}>Refresh local data</button>
          <button class="btn-primary" disabled={workerBusy || worker?.running} onclick={startWorker}>Start worker</button>
          <button class="btn" disabled={workerBusy || !worker?.running} onclick={stopWorker}>Stop worker</button>
          <button class="btn" disabled={discoverBusy} onclick={() => triggerDiscover()}>Bootstrap discovery tasks</button>
          <button class="btn-primary" data-testid="overview-run-once" disabled={runOnceBusy} onclick={runWorkerOnce}>
            {runOnceBusy ? 'Running once…' : 'Run worker once'}
          </button>
        </div>
        {#if workerRunSummary}
          <pre class="summary-json">{JSON.stringify(workerRunSummary, null, 2)}</pre>
        {/if}
      </section>
    {:else if currentPage === 'sites'}
      <section class="card space-y-4">
        <h3 class="section-title">Import site connection pack</h3>
        <p class="hint">Paste the JSON produced by WordPress: wp wptsall security issue-pairing-pack --device-id=&lt;this client device id&gt;</p>
        <form class="grid grid-cols-1 lg:grid-cols-3 gap-3" onsubmit={importSiteConnection}>
          <textarea class="input lg:col-span-3 min-h-28 font-mono text-xs" bind:value={importPackJson} placeholder={importPackPlaceholder}></textarea>
          <input class="input font-mono" bind:value={importPairingCode} placeholder="Pairing code (optional if pack includes it)" />
          <input class="input" bind:value={importDeviceLabel} placeholder="Device label (optional)" />
          <button class="btn-primary" disabled={importBusy || !importPackJson} type="submit">
            {importBusy ? 'Importing…' : 'Import and claim token'}
          </button>
        </form>
      </section>

      <section class="card space-y-4">
        <h3 class="section-title">Add WordPress site manually</h3>
        <form class="grid grid-cols-1 lg:grid-cols-4 gap-3" onsubmit={addSite}>
          <input
            class="input lg:col-span-2"
            data-testid="sites-modal-url"
            bind:value={addWpUrl}
            placeholder="https://site.test or full /wp-json/wptsall/v2/<secret>/client URL"
          />
          <input
            class="input"
            data-testid="sites-modal-token"
            bind:value={addToken}
            placeholder="Device token"
            type="password"
          />
          <input
            class="input"
            data-testid="sites-modal-route-secret"
            bind:value={addRouteSecret}
            placeholder="Route secret (optional if URL includes it)"
          />
          <button
            class="btn-primary lg:col-span-4"
            data-testid="sites-modal-save"
            disabled={siteBusy || !addWpUrl || !addToken}
            type="submit"
          >
            {siteBusy ? 'Saving…' : 'Save site binding'}
          </button>
        </form>
      </section>

      <section class="card">
        <h3 class="section-title">Connected sites</h3>
        {#if sites.length === 0}
          <p class="hint">No sites configured yet.</p>
        {:else}
          <div class="divide-y divide-slate-100">
            {#each sites as site}
              <div class="py-3 flex items-center justify-between gap-3">
                <div>
                  <p class="font-medium">{site.domain || site.wp_url}</p>
                  <p class="hint">{site.connected ? 'connected' : 'not verified'} · {site.languages.join(', ') || 'languages unknown'}</p>
                </div>
                <div class="flex gap-2">
                  <button class="btn" data-testid="sites-test-connection" disabled={siteBusy} onclick={() => testSite(site.id)}>Test</button>
                  <button class="btn-danger" disabled={siteBusy} onclick={() => removeSite(site.id)}>Remove</button>
                  <button class="btn" disabled={discoverBusy} onclick={() => triggerDiscover(site.id)}>Discover</button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {:else if currentPage === 'credentials'}
      <section class="card space-y-4">
        <div>
          <h3 class="section-title">Vendor API keys</h3>
          <p class="hint">Store provider credentials locally through the embedded WebUI runtime (same as WebUI API Keys).</p>
        </div>
        <form class="grid grid-cols-1 md:grid-cols-4 gap-3" onsubmit={saveVendorKey}>
          <input class="input" bind:value={vendorKeyVendorId} placeholder="Vendor ID (e.g. openai)" />
          <input class="input" bind:value={vendorKeyLabel} placeholder="Label" />
          <input class="input" type="password" bind:value={vendorKeySecret} placeholder="API key secret" />
          <button class="btn-primary" disabled={vendorKeySaving || !vendorKeyVendorId.trim() || !vendorKeySecret.trim()} type="submit">
            {vendorKeySaving ? 'Saving…' : 'Save key'}
          </button>
        </form>
        {#if vendorKeysBusy}
          <p class="hint">Loading keys…</p>
        {:else if vendorKeys.length === 0}
          <p class="hint">No vendor keys yet.</p>
        {:else}
          <div class="divide-y divide-slate-100">
            {#each vendorKeys as key (String(key.id || key.label))}
              <div class="py-3 flex items-center justify-between gap-3">
                <div>
                  <p class="font-medium">{String(key.label || key.id || 'key')}</p>
                  <p class="hint">{String(key.vendor_id || '-')} · {String(key.masked_key || key.key_prefix || '••••')}</p>
                </div>
                <button class="btn" disabled={vendorKeysBusy || !key.id} onclick={() => deleteVendorKey(String(key.id))}>
                  Delete
                </button>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="card space-y-4">
        <div>
          <h3 class="section-title">Vendor OAuth (translation providers)</h3>
          <p class="hint">Local provider OAuth configs only — not website account login. Authorize opens the provider URL when returned.</p>
        </div>
        <form class="grid grid-cols-1 md:grid-cols-5 gap-3" onsubmit={saveVendorOauth}>
          <input class="input" bind:value={oauthId} placeholder="Config id" />
          <input class="input" bind:value={oauthVendorId} placeholder="Vendor ID" />
          <input class="input" bind:value={oauthClientId} placeholder="Client id (optional)" />
          <input class="input" type="password" bind:value={oauthClientSecret} placeholder="Client secret (optional)" />
          <button class="btn-primary" disabled={oauthSaving || !oauthId.trim() || !oauthVendorId.trim()} type="submit">
            {oauthSaving ? 'Saving…' : 'Save OAuth'}
          </button>
        </form>
        {#if vendorOauthBusy}
          <p class="hint">Loading OAuth configs…</p>
        {:else if vendorOauthConfigs.length === 0}
          <p class="hint">No vendor OAuth configs yet.</p>
        {:else}
          <div class="divide-y divide-slate-100">
            {#each vendorOauthConfigs as cfg (String(cfg.id))}
              <div class="py-3 flex items-center justify-between gap-3">
                <div>
                  <p class="font-medium">{String(cfg.id)}</p>
                  <p class="hint">{String(cfg.vendor_id || '-')} · {String(cfg.status || cfg.token_preview || 'configured')}</p>
                </div>
                <div class="flex gap-2">
                  <button class="btn" disabled={vendorOauthBusy} onclick={() => authorizeVendorOauth(String(cfg.id))}>
                    Authorize
                  </button>
                  <button class="btn" disabled={vendorOauthBusy} onclick={() => deleteVendorOauth(String(cfg.id))}>
                    Delete
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="card space-y-4">
        <div>
          <h3 class="section-title">Proxy profiles</h3>
          <p class="hint">Optional local HTTP(S) proxies for provider calls (embedded WebUI runtime).</p>
        </div>
        <form class="grid grid-cols-1 md:grid-cols-5 gap-3" onsubmit={saveProxyProfile}>
          <input class="input" bind:value={proxyId} placeholder="Profile id" />
          <input class="input" bind:value={proxyName} placeholder="Name" />
          <input class="input" bind:value={proxyHost} placeholder="Host" />
          <input class="input" bind:value={proxyPort} placeholder="Port" />
          <button class="btn-primary" disabled={proxySaving || !proxyId.trim() || !proxyHost.trim()} type="submit">
            {proxySaving ? 'Saving…' : 'Save proxy'}
          </button>
        </form>
        {#if proxyBusy}
          <p class="hint">Loading proxies…</p>
        {:else if proxyProfiles.length === 0}
          <p class="hint">No proxy profiles yet.</p>
        {:else}
          <div class="divide-y divide-slate-100">
            {#each proxyProfiles as profile (String(profile.id))}
              <div class="py-3 flex items-center justify-between gap-3">
                <div>
                  <p class="font-medium">{String(profile.name || profile.id)}</p>
                  <p class="hint">{String(profile.host || '-')}:{String(profile.port || '-')}</p>
                </div>
                <div class="flex gap-2">
                  <button class="btn" disabled={proxyBusy} onclick={() => testProxyProfile(String(profile.id))}>
                    Test
                  </button>
                  <button class="btn" disabled={proxyBusy} onclick={() => deleteProxyProfile(String(profile.id))}>
                    Delete
                  </button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {:else if currentPage === 'tasks'}
      <section class="card">
        <div class="flex items-center justify-between gap-3 mb-3">
          <h3 class="section-title">Translation jobs</h3>
          <div class="flex gap-2">
            <button class="btn" disabled={discoverBusy} onclick={() => triggerDiscover()}>Bootstrap discovery</button>
            <button class="btn-primary" data-testid="overview-run-once" disabled={runOnceBusy} onclick={runWorkerOnce}>
              {runOnceBusy ? 'Running once…' : 'Run worker once'}
            </button>
          </div>
        </div>
        <form class="grid grid-cols-1 md:grid-cols-4 gap-3 mb-3" onsubmit={focusRelation}>
          <input class="input md:col-span-2" bind:value={focusRelationId} placeholder="Focus relation ID (optional)" />
          <label class="checkbox-row">
            <input type="checkbox" bind:checked={focusIncludeResync} />
            include resync
          </label>
          <button class="btn" disabled={focusBusy || !focusRelationId} type="submit">
            {focusBusy ? 'Focusing…' : 'Focus relation'}
          </button>
        </form>
        {#if workerRunSummary}
          <pre class="summary-json mb-3">{JSON.stringify(workerRunSummary, null, 2)}</pre>
        {/if}
        {#if tasks.length === 0}
          <p class="hint">No jobs yet.</p>
        {:else}
          <div class="space-y-3">
            {#each tasks as task}
              <div class="border border-slate-100 rounded-lg p-3">
                <div class="flex items-center justify-between gap-3">
                  <div>
                    <p class="font-medium">Job #{task.id || 'unknown'} · {task.status}</p>
                    <p class="hint">{task.site_domain || 'domain unknown'} · {task.created_at || 'time unknown'}</p>
                  </div>
                  <div class="flex items-center gap-2">
                    <span class="text-xs text-slate-500">{Math.round(task.progress)}%</span>
                    <button class="btn" disabled={jobItemsBusy} onclick={() => loadJobItems(String(task.id))}>
                      {selectedJobId === String(task.id) ? 'Items' : 'Open items'}
                    </button>
                  </div>
                </div>
                <div class="mt-2 h-2 bg-slate-100 rounded-full overflow-hidden">
                  <div class="h-full bg-blue-600" style={`width: ${Math.max(0, Math.min(100, task.progress))}%`}></div>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      {#if selectedJobId}
        <section class="card space-y-3">
          <h3 class="section-title">Job #{selectedJobId} items</h3>
          {#if jobItemsBusy}
            <p class="hint">Loading items…</p>
          {:else if jobItems.length === 0}
            <p class="hint">No items for this job.</p>
          {:else}
            <div class="divide-y divide-slate-100">
              {#each jobItems as item (String(item.id))}
                <div class="py-2 flex items-center justify-between gap-3">
                  <div>
                    <p class="font-medium">Item #{String(item.id)} · {String(item.status || 'unknown')}</p>
                    <p class="hint">{String(item.source_lang || '?')} → {String(item.target_lang || '?')}</p>
                  </div>
                  <button class="btn" disabled={itemBusy} onclick={() => openItemReview(String(item.id))}>Review</button>
                </div>
              {/each}
            </div>
          {/if}
        </section>
      {/if}

      {#if selectedItemId}
        <section class="card space-y-3">
          <h3 class="section-title">Review item #{selectedItemId}</h3>
          {#if itemBusy && !itemContent}
            <p class="hint">Loading content…</p>
          {:else}
            <textarea class="input min-h-40 font-mono text-xs" bind:value={translatedDraft}></textarea>
            <div class="flex flex-wrap gap-2">
              <button class="btn-primary" disabled={itemBusy} onclick={saveItemTranslation}>Save translation</button>
              <button class="btn" disabled={itemBusy} onclick={approveSelectedItem}>Approve</button>
              <button class="btn" disabled={itemBusy} onclick={retranslateSelectedItem}>Retranslate</button>
            </div>
          {/if}
        </section>
      {/if}
    {:else if currentPage === 'components'}
      <section class="card space-y-4">
        <h3 class="section-title">Configure local mock component</h3>
        <form class="grid grid-cols-1 lg:grid-cols-4 gap-3" onsubmit={configureMockComponent}>
          <input class="input" bind:value={mockComponentId} placeholder="Component ID" />
          <input class="input" bind:value={mockApiBase} placeholder="Mock API base" />
          <input class="input" bind:value={mockModel} placeholder="Model" />
          <input class="input" bind:value={mockApiKey} placeholder="API key" type="password" />
          <button class="btn-primary lg:col-span-4" disabled={componentBusy || !mockComponentId || !mockApiBase || !mockModel || !mockApiKey} type="submit">
            {componentBusy ? 'Configuring…' : 'Configure mock component'}
          </button>
        </form>
      </section>

      <section class="card">
        <h3 class="section-title">Local runtime components</h3>
        {#if components.length === 0}
          <p class="hint">No local components configured yet.</p>
        {:else}
          <div class="divide-y divide-slate-100">
            {#each components as component}
              <div class="py-3 flex items-center justify-between gap-3">
                <div>
                  <p class="font-medium">{component.name || component.id}</p>
                  <p class="hint">{component.provider_type} · {component.configured ? 'enabled' : 'disabled'}</p>
                </div>
                <div class="flex items-center gap-2">
                  <button
                    class="btn"
                    disabled={quickTestBusy === component.id || !component.configured}
                    onclick={() => quickTestComponent(component.id)}
                  >
                    {quickTestBusy === component.id ? 'Testing…' : 'Quick-test'}
                  </button>
                  <span class:badge-ok={component.configured} class:badge-muted={!component.configured}>
                    {component.configured ? 'Configured' : 'Disabled'}
                  </span>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {:else if currentPage === 'providers'}
      <section class="card space-y-4">
        <div class="flex items-center justify-between gap-3">
          <div>
            <h3 class="section-title">Local provider catalog</h3>
            <p class="hint">Postman-style templates: install → key → quick-test → enable → route. Prefer mock-verified + mock-api (:9090).</p>
          </div>
          <div class="flex gap-2">
            <button class="btn" disabled={providerCatalogBusy} onclick={loadProviderCatalog}>
              {providerCatalogBusy ? 'Loading…' : 'Reload'}
            </button>
            <button class="btn-primary" disabled={providerCatalogRefreshing} onclick={refreshProviderCatalog}>
              {providerCatalogRefreshing ? 'Verifying…' : 'Verify local catalog'}
            </button>
          </div>
        </div>
        {#if wizardItem}
          <div class="border border-blue-100 bg-blue-50/40 rounded-lg p-4 space-y-3" data-testid="provider-setup-wizard">
            <div class="flex justify-between gap-2">
              <div>
                <h4 class="font-medium text-sm">Setup wizard · {wizardItem.name}</h4>
                <p class="hint">{wizardItem.family} · {wizardItem.evidence_tier || 'schema-only'} · step {wizardStep}</p>
              </div>
              <button class="btn" data-testid="wizard-close" disabled={wizardBusy} onclick={() => (wizardItem = null)}>Close</button>
            </div>
            {#if wizardHint}<p class="text-xs text-amber-800" data-testid="wizard-hint">{wizardHint}</p>{/if}
            {#if wizardStep === 'install'}
              <input class="input font-mono text-xs" data-testid="wizard-local-id" bind:value={wizardLocalId} />
              <button class="btn-primary" data-testid="wizard-install-next" disabled={wizardBusy} onclick={wizardInstall}>Install and continue</button>
            {:else if wizardStep === 'key'}
              <input class="input font-mono text-xs" data-testid="wizard-key-id" bind:value={wizardKeyId} placeholder="key id" />
              {#each wizardAuthFields as fieldName, idx}
                <input
                  class="input font-mono text-xs"
                  data-testid={idx === 0 ? 'wizard-api-key' : `wizard-auth-${fieldName}`}
                  type="password"
                  bind:value={wizardAuthValues[fieldName]}
                  placeholder={fieldName}
                />
              {/each}
              <input class="input font-mono text-xs" data-testid="wizard-request-url" bind:value={wizardUrl} placeholder="request.url (any http(s))" />
              <input class="input font-mono text-xs" data-testid="wizard-response-path" bind:value={wizardResponsePath} placeholder="response.translated_text_path" />
              {#if wizardItem.family === 'openai_compatible'}
                <input class="input font-mono text-xs" data-testid="wizard-model" bind:value={wizardModel} placeholder="model" />
              {/if}
              <button class="btn-primary" data-testid="wizard-key-next" disabled={wizardBusy} onclick={wizardBindKey}>Save key and continue</button>
            {:else if wizardStep === 'test'}
              <input class="input text-xs" data-testid="wizard-test-text" bind:value={wizardTestText} />
              {#if wizardTestOut}<pre class="summary-json" data-testid="wizard-test-out">{wizardTestOut}</pre>{/if}
              <button class="btn-primary" data-testid="wizard-test-next" disabled={wizardBusy} onclick={wizardQuickTest}>Run quick-test</button>
            {:else if wizardStep === 'enable'}
              <button class="btn-primary" data-testid="wizard-enable-next" disabled={wizardBusy} onclick={wizardEnable}>Enable component</button>
            {:else if wizardStep === 'route'}
              <input class="input font-mono text-xs" data-testid="wizard-route-slot" bind:value={wizardSlot} placeholder="content_format slot" />
              <button class="btn-primary" data-testid="wizard-route-next" disabled={wizardBusy} onclick={wizardRoute}>Bind global route</button>
            {:else}
              <p class="text-sm text-green-700" data-testid="wizard-done">Done. Use Tasks review when review_mode is on; otherwise worker callbacks WP directly.</p>
            {/if}
          </div>
        {/if}
        {#if providerCatalog}
          <p class="hint">{providerCatalog.catalog_version || 'local'} · {providerCatalog.items.length} templates · offline={providerCatalog.offline ? 'yes' : 'no'}</p>
          {#if providerCatalog.items.length === 0}
            <p class="hint">No provider templates found.</p>
          {:else}
            <div class="overflow-x-auto">
              <table class="w-full text-sm">
                <thead>
                  <tr class="text-left text-xs text-slate-500 border-b border-slate-100">
                    <th class="py-2 pr-3">Name</th>
                    <th class="py-2 pr-3">Vendor</th>
                    <th class="py-2 pr-3">Evidence</th>
                    <th class="py-2 pr-3">Source</th>
                    <th class="py-2 text-right">Action</th>
                  </tr>
                </thead>
                <tbody>
                  {#each providerCatalog.items as item (item.entry_id + item.template_id)}
                    <tr class="border-b border-slate-50 align-top">
                      <td class="py-3 pr-3">
                        <p class="font-medium">{item.name || item.template_id}</p>
                        <p class="hint">{item.template_id} · {item.family || item.kind}</p>
                      </td>
                      <td class="py-3 pr-3 font-mono text-xs">{item.vendor_id || '-'}</td>
                      <td class="py-3 pr-3 text-xs">{item.evidence_tier || 'schema-only'}</td>
                      <td class="py-3 pr-3 text-xs">{item.verified ? 'verified' : 'unverified'} · {item.source}</td>
                      <td class="py-3 text-right space-x-2 whitespace-nowrap">
                        <button
                          class="btn-primary"
                          data-testid="open-provider-wizard"
                          data-entry-id={item.entry_id}
                          onclick={() => openProviderWizard(item)}
                        >Setup wizard</button>
                        <button
                          class="btn"
                          disabled={providerCatalogInstalling === item.entry_id}
                          onclick={() => installProviderTemplate(item)}
                        >
                          {providerCatalogInstalling === item.entry_id ? 'Installing…' : 'Install'}
                        </button>
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
        {:else if providerCatalogBusy}
          <p class="hint">Loading provider catalog…</p>
        {/if}
      </section>
    {:else if currentPage === 'integration_pack'}
      <section class="card space-y-4">
        <div>
          <h3 class="section-title">Integration pack</h3>
          <p class="hint">Move local sites, components, bindings, workflow, and provider configuration between clients.</p>
        </div>
        <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <div class="border border-slate-100 rounded-lg p-4 space-y-3">
            <h4 class="font-medium">Export integration pack</h4>
            <label class="block text-sm">
              Export mode
              <select class="input mt-1" bind:value={integrationExportMode}>
                <option value="public">Public redacted export</option>
                <option value="private">Private encrypted backup</option>
              </select>
            </label>
            {#if integrationExportMode === 'private'}
              <input class="input" type="password" autocomplete="new-password" bind:value={integrationExportPassphrase} placeholder="Backup passphrase (8+ characters)" />
              <label class="checkbox-row">
                <input type="checkbox" bind:checked={integrationConfirmPrivate} />
                I understand this backup contains credentials.
              </label>
              <p class="text-xs text-amber-700">Private backups use AES-256-GCM; keep the passphrase separate.</p>
            {:else}
              <p class="hint">Public exports redact credentials and keep imported components disabled until configured.</p>
            {/if}
            <div class="flex gap-2">
              <button class="btn-primary" disabled={integrationExportBusy} onclick={exportIntegrationPackDesktop}>
                {integrationExportBusy ? 'Exporting…' : 'Export pack'}
              </button>
              <button class="btn" disabled={!integrationExportJson} onclick={downloadIntegrationPack}>Download JSON</button>
            </div>
            {#if integrationExportJson}
              <textarea class="input min-h-40 font-mono text-xs" readonly value={integrationExportJson}></textarea>
            {/if}
          </div>
          <div class="border border-slate-100 rounded-lg p-4 space-y-3">
            <h4 class="font-medium">Preview and import</h4>
            <p class="hint">Preview checks component references, provider URLs, workflow data, and conflicts before writing local state.</p>
            <textarea class="input min-h-40 font-mono text-xs" bind:value={integrationImportJson} placeholder="Paste public or encrypted integration pack JSON"></textarea>
            <input class="input" type="password" autocomplete="off" bind:value={integrationImportPassphrase} placeholder="Private pack passphrase (optional)" />
            <label class="checkbox-row">
              <input type="checkbox" bind:checked={integrationOverwrite} />
              Overwrite existing components and bindings
            </label>
            <div class="flex gap-2">
              <button class="btn" disabled={integrationPreviewBusy || !integrationImportJson.trim()} onclick={previewIntegrationPackDesktop}>
                {integrationPreviewBusy ? 'Checking…' : 'Preview'}
              </button>
              <button class="btn-primary" disabled={integrationImportBusy || integrationPreviewBusy || !integrationImportJson.trim()} onclick={importIntegrationPackDesktop}>
                {integrationImportBusy ? 'Importing…' : 'Import pack'}
              </button>
            </div>
            {#if integrationPreview}
              <div class="border border-slate-100 rounded p-3 text-xs space-y-1">
                <p class={integrationPreview.safe_to_import === true ? 'text-green-700' : 'text-red-700'}>
                  {integrationPreview.safe_to_import === true ? 'Safe to import' : 'Import blocked'}
                </p>
                <p>New components: {Array.isArray(integrationPreview.new_components) ? integrationPreview.new_components.length : 0}</p>
                <p>Existing components: {Array.isArray(integrationPreview.overwrite_components) ? integrationPreview.overwrite_components.length : 0}</p>
                <p>Blocked provider URLs: {Number(integrationPreview.blocked_provider_url_count || 0)}</p>
                <p>Missing references: {Array.isArray(integrationPreview.missing_component_refs) ? integrationPreview.missing_component_refs.length : 0}</p>
                {#if integrationPreview.workflow_valid === false}<p class="text-red-700">Workflow data is invalid</p>{/if}
              </div>
            {/if}
          </div>
        </div>
      </section>
    {:else if currentPage === 'history'}
      <section class="card space-y-3">
        <div class="flex items-center justify-between gap-3 flex-wrap">
          <div>
            <h3 class="section-title">Translation history</h3>
            <p class="hint">Local SQLite records via `GET /api/translations` (no website control plane).</p>
          </div>
          <button class="btn" disabled={historyBusy} onclick={loadHistory}>
            {historyBusy ? 'Loading…' : 'Reload'}
          </button>
        </div>
        <div class="flex gap-2 flex-wrap items-end">
          <label class="min-w-[10rem] grow">
            <span class="label">Domain</span>
            <input class="input mt-1" bind:value={historyDomain} placeholder="example.test" />
          </label>
          <label class="min-w-[8rem]">
            <span class="label">Status</span>
            <select class="input mt-1" bind:value={historyStatus}>
              <option value="">Any</option>
              <option value="success">success</option>
              <option value="failed">failed</option>
              <option value="pending_callback">pending_callback</option>
            </select>
          </label>
          <label class="min-w-[10rem] grow">
            <span class="label">Search</span>
            <input class="input mt-1" bind:value={historySearch} placeholder="object / error text" />
          </label>
          <button
            class="btn"
            disabled={historyBusy}
            onclick={() => {
              historyPage = 1;
              loadHistory();
            }}
          >
            Apply
          </button>
        </div>
        {#if historySelected.size > 0}
          <div class="flex gap-2 items-center flex-wrap">
            <span class="hint">{historySelected.size} selected</span>
            <button class="btn" disabled={historyBatchBusy} onclick={batchRetryHistory}>Batch retry</button>
            <button class="btn" disabled={historyBatchBusy} onclick={batchDeleteHistory}>Batch delete</button>
          </div>
        {/if}
        <p class="hint">Total: {historyTotal} · page {historyPage}</p>
        {#if historyRecords.length === 0}
          <p class="hint">No translation records.</p>
        {:else}
          <div class="overflow-auto">
            <table class="w-full text-sm">
              <thead>
                <tr>
                  <th>
                    <input
                      type="checkbox"
                      checked={historySelected.size > 0 && historySelected.size === historyRecords.length}
                      onchange={toggleHistorySelectAll}
                    />
                  </th>
                  <th>Time</th>
                  <th>Domain</th>
                  <th>Object</th>
                  <th>Lang</th>
                  <th>Status</th>
                  <th>Fields</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {#each historyRecords as row}
                  <tr>
                    <td>
                      <input
                        type="checkbox"
                        checked={historySelected.has(row.id)}
                        onchange={() => toggleHistorySelect(row.id)}
                      />
                    </td>
                    <td class="text-xs">{formatHistoryTime(row.created_at)}</td>
                    <td class="text-xs">{row.domain || '—'}</td>
                    <td class="text-xs">{row.object_type || '—'} #{row.object_id ?? '—'}</td>
                    <td class="text-xs">{row.source_lang || '?'}→{row.target_lang || '?'}</td>
                    <td class="text-xs">{row.status || '—'}</td>
                    <td class="text-xs">{row.fields_count ?? 0}{#if row.failed_fields_count} / fail {row.failed_fields_count}{/if}</td>
                    <td>
                      {#if row.status === 'failed'}
                        <button class="btn" disabled={historyBatchBusy} onclick={() => retryHistoryOne(row.id)}>
                          Retry
                        </button>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
          <div class="flex gap-2">
            <button
              class="btn"
              disabled={historyBusy || historyPage <= 1}
              onclick={() => {
                historyPage = Math.max(1, historyPage - 1);
                loadHistory();
              }}
            >
              Prev
            </button>
            <button
              class="btn"
              disabled={historyBusy || historyPage * 20 >= historyTotal}
              onclick={() => {
                historyPage += 1;
                loadHistory();
              }}
            >
              Next
            </button>
          </div>
        {/if}
      </section>
    {:else if currentPage === 'logs'}
      <section class="card space-y-3">
        <div class="flex items-center justify-between gap-3">
          <div>
            <h3 class="section-title">Recent logs</h3>
            <p class="hint">Proxied from embedded WebUI `POST /api/logs/recent`.</p>
          </div>
          <button class="btn" disabled={logsBusy} onclick={loadLogs}>
            {logsBusy ? 'Loading…' : 'Reload'}
          </button>
        </div>
        {#if logLines.length === 0}
          <p class="hint">No log lines.</p>
        {:else}
          <pre class="summary-json max-h-96 overflow-auto">{logLines.join('\n')}</pre>
        {/if}
      </section>
    {:else if currentPage === 'settings'}
      <section class="card max-w-lg space-y-3">
        <div class="flex items-center justify-between">
          <h3 class="section-title">Version update</h3>
          <button class="btn" disabled={updateChecking || updating} onclick={checkForUpdate}>
            {updateChecking ? 'Checking…' : 'Check for updates'}
          </button>
        </div>

        {#if updateError}
          <p class="text-sm text-red-600 bg-red-50 rounded p-2">{updateError}</p>
        {/if}
        {#if updateMessage}
          <p class="text-sm text-green-700 bg-green-50 rounded p-2">{updateMessage}</p>
        {/if}

        {#if updateInfo}
          <div class="text-xs text-slate-500 space-y-1">
            <div>Binary: v{updateInfo.current_version}
              {#if updateInfo.latest_version !== updateInfo.current_version}
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
            <button class="btn-primary" disabled={updating} onclick={performUpdate}>
              {updating ? 'Updating…' : 'Update now'}
            </button>
          {:else}
            <p class="text-sm text-slate-500">You are up to date.</p>
          {/if}
        {/if}
      </section>
    {/if}
  </main>
</div>
