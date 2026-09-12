// catalog: WEBUI-UI-Components
// oracle: L2
// 状态矩阵：默认 tab + 组件治理 tab 切换；local 模式隐藏 server 模板 tab。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import Components from './Components.svelte';
import * as componentsApi from '../lib/api/components';
import * as keysApi from '../lib/api/keys';
import * as statusModule from '../lib/stores/status';
import * as toastModule from '../lib/stores/toast';

vi.mock('../lib/api/components', () => ({
  createComponentVersion: vi.fn(),
  createLocalComponent: vi.fn(),
  deleteBinding: vi.fn(),
  deleteComponentVersion: vi.fn(),
  deleteLocalComponent: vi.fn(),
  deleteRuleBinding: vi.fn(),
  deleteTaskTypeBinding: vi.fn(),
  getComponentCapabilities: vi.fn(),
  getRuleBindingDiscovery: vi.fn(),
  getLocalComponent: vi.fn(),
  listAllLocalComponents: vi.fn(),
  listLocalComponents: vi.fn(),
  loadComponentTemplate: vi.fn(),
  quickTestLocalComponent: vi.fn(),
  refreshLocalComponentSnapshot: vi.fn(),
  searchServerComponents: vi.fn(),
  testComponentVersion: vi.fn(),
  testLocalComponentFile: vi.fn(),
  updateComponentVersion: vi.fn(),
  updateLocalComponent: vi.fn(),
  upsertBinding: vi.fn(),
  upsertRuleBinding: vi.fn(),
  upsertTaskTypeBinding: vi.fn(),
}));

vi.mock('../lib/api/keys', () => ({
  listVendorKeys: vi.fn(),
  listOAuthConfigs: vi.fn(),
}));

vi.mock('../lib/stores/status', async () => {
  const { writable } = await import('svelte/store');
  return {
    status: writable<any>({
      // P0-LF-04: legacy mode so the Server 模板 tab remains reachable in the
      // legacy-governance test; local mode is covered by a dedicated test.
      runtime_mode: 'legacy_server_control_plane',
      components: [
        {
          id: 'server-text',
          name: 'Server Text',
          kind: 'text',
          supported_business_lines: ['post_content'],
          supported_content_formats: ['plain_text'],
          template_group: 'official',
          size_class: 'S',
        },
      ],
      rule_component_bindings: [],
    }),
    fetchStatus: vi.fn().mockResolvedValue(undefined),
  };
});

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('pages/Components', () => {
  beforeEach(() => {
    vi.mocked(componentsApi.listLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [],
        page: 1,
        per_page: 50,
        total: 0,
        total_pages: 1,
        kinds: ['text'],
      },
    } as never);
    vi.mocked(componentsApi.listAllLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [{ id: 'local-text', kind: 'text', name: 'Local Text', enabled: true }],
        total: 1,
      },
    } as never);
    vi.mocked(componentsApi.getComponentCapabilities).mockResolvedValue({
      success: true,
      data: {
        components: [
          {
            id: 'local-text',
            name: 'Local Text',
            kind: 'text',
            supported_business_lines: ['post_content'],
            supported_content_formats: ['plain_text'],
            supported_formats: ['plain_text'],
            client_contract: {
              schema_version: 'component-client-contract-v1',
              task_kind: 'text',
              input_mode: 'text',
              workflow_mode: 'sync',
              output_mode: 'translated_text',
              stages: ['request'],
            },
            constraints: {},
          },
        ],
        format_component_map: {
          plain_text: ['local-text'],
        },
      },
    } as never);
    vi.mocked(componentsApi.getRuleBindingDiscovery).mockResolvedValue({
      success: true,
      data: {
        summary: {
          domains_checked: 0,
          relations_checked: 0,
          rules_checked: 0,
          fields_checked: 0,
          issues: 0,
        },
        items: [],
        issues: [],
      },
    } as never);
    vi.mocked(componentsApi.searchServerComponents).mockResolvedValue({
      success: true,
      data: {
        items: [],
        page: 1,
        per_page: 20,
        total: 0,
        total_pages: 1,
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
    vi.mocked(statusModule.fetchStatus).mockClear();
    vi.mocked(toastModule.showToast).mockClear();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('renders default tab and switches across active component governance tabs', async () => {
    render(Components);

    expect(await screen.findByText(/本地组件实例/)).toBeTruthy();

    await fireEvent.click(screen.getByText('能力矩阵'));
    await waitFor(() => {
      expect(screen.getByText('组件能力矩阵')).toBeTruthy();
    });
    expect(screen.getByText('格式 → 组件映射')).toBeTruthy();

    await fireEvent.click(screen.getByText('规则绑定'));
    await waitFor(() => {
      expect(screen.getByText('按翻译规则槽位绑定组件')).toBeTruthy();
    });

    await fireEvent.click(screen.getByText('Server 模板'));
    await waitFor(() => {
      expect(screen.getByPlaceholderText('搜索模板名称、ID、Vendor...')).toBeTruthy();
    });
  });

  it('hides the legacy Server 模板 tab in local runtime mode', async () => {
    (statusModule.status as any).set({
      runtime_mode: 'local',
      components: [],
      rule_component_bindings: [],
    });

    render(Components);

    expect(await screen.findByText(/本地组件实例/)).toBeTruthy();
    // P0-LF-04: the server-template affordance is a legacy-only surface.
    expect(screen.queryByText('Server 模板')).toBeNull();
  });
});
