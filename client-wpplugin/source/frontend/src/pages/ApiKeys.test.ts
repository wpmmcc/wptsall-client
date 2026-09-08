import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import ApiKeys from './ApiKeys.svelte';
import * as keysApi from '../lib/api/keys';
import * as toastModule from '../lib/stores/toast';
import { status } from '../lib/stores/status';

vi.mock('../lib/api/keys', () => ({
  listProviderCatalog: vi.fn(),
  refreshProviderCatalog: vi.fn(),
  installCatalogTemplate: vi.fn(),
  exportIntegrationPack: vi.fn(),
  previewIntegrationPack: vi.fn(),
  importIntegrationPack: vi.fn(),
  listVendors: vi.fn(),
  listWpTranslationProviders: vi.fn(),
  listCloudApiTypes: vi.fn(),
  listVendorKeys: vi.fn(),
  listOAuthConfigs: vi.fn(),
  createVendorKey: vi.fn(),
  updateVendorKey: vi.fn(),
  deleteVendorKey: vi.fn(),
  createOAuthConfig: vi.fn(),
  updateOAuthConfig: vi.fn(),
  deleteOAuthConfig: vi.fn(),
  authorizeOAuth: vi.fn(),
}));

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('pages/ApiKeys', () => {
  beforeEach(() => {
    vi.mocked(keysApi.listProviderCatalog).mockResolvedValue({
      success: true,
      data: {
        schema: 'wptsall-provider-catalog-manifest.v1',
        catalog_version: 'builtin-1',
        offline: true,
        items: [
          {
            entry_id: 'openai-compatible',
            template_id: 'openai-compatible-chat-completions-v1',
            name: 'OpenAI-compatible Chat Completions',
            vendor_id: 'openai_compatible',
            family: 'openai_compatible',
            kind: 'text',
            supported_content_formats: ['plain_text'],
            source: 'builtin',
            verified: true,
            requires_local_credentials: true,
            template: {},
          },
        ],
      },
    } as never);
    vi.mocked(keysApi.refreshProviderCatalog).mockResolvedValue({
      success: true,
      data: {
        catalog_version: 'builtin-1',
        template_count: 1,
        source: 'builtin',
        verified: true,
        offline: true,
      },
    } as never);
    vi.mocked(keysApi.listVendorKeys).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);
    vi.mocked(keysApi.listOAuthConfigs).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);
    vi.mocked(toastModule.showToast).mockClear();
  });

  afterEach(() => {
    status.set(null);
    cleanup();
    vi.clearAllMocks();
  });

  it('defaults to vendor catalog and switches to key/oauth tabs', async () => {
    render(ApiKeys);

    expect(await screen.findByText('OpenAI-compatible Chat Completions')).toBeTruthy();
    expect(keysApi.listProviderCatalog).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByText('Vendor Keys'));
    await waitFor(() => {
      expect(screen.getByText('API Key 池')).toBeTruthy();
    });
    expect(keysApi.listVendorKeys).toHaveBeenCalledTimes(1);

    await fireEvent.click(screen.getByText('OAuth 配置'));
    await waitFor(() => {
      expect(screen.getByText('暂无 OAuth 配置')).toBeTruthy();
    });
    expect(keysApi.listOAuthConfigs).toHaveBeenCalledTimes(1);
  });

  it('local mode hides the official website inventory tabs', async () => {
    // status store is null → runtime_mode undefined → default local UI.
    render(ApiKeys);

    expect(await screen.findByText('OpenAI-compatible Chat Completions')).toBeTruthy();

    // P0-LF-04: /api/wp-translation-providers and /api/cloud-api-types are
    // website inventories; their tabs must not exist in the local default.
    expect(screen.queryByText('WP 翻译商 (L1)')).toBeNull();
    expect(screen.queryByText('Cloud API 类型 (L2)')).toBeNull();

    // The valid local surfaces stay reachable.
    expect(screen.getByText('Vendor 目录')).toBeTruthy();
    expect(screen.getByText('Vendor Keys')).toBeTruthy();
  });

  it('legacy mode exposes the website inventory tabs', async () => {
    status.set({ runtime_mode: 'legacy_server_control_plane' } as never);

    render(ApiKeys);

    expect(await screen.findByText('OpenAI-compatible Chat Completions')).toBeTruthy();
    expect(screen.getByText('WP 翻译商 (L1)')).toBeTruthy();
    expect(screen.getByText('Cloud API 类型 (L2)')).toBeTruthy();
  });
});
