// catalog: WEBUI-UI-PageStateMatrix
// oracle: L2
// 批 N2 / U-6 三态矩阵：页面 loading/error/empty 状态体系化——此前覆盖散点
// 在各页自己的 spec 里（CapabilitiesTab error+recover、ServerTemplatesTab
// empty+error、Products/SyncPairs/TaskRouting empty），没有一个统一矩阵。
// 本矩阵枚举核心数据面的空态/载入态契约，断言每面渲染确定性状态节点。
//
// 缺口清单（诚实记账，非断言）：
// - ~~ApiKeys / Logs 的空列表为结构性渲染（无专门空态 marker）~~——批 O4 销：
//   vendors/keys/oauth 三 tab 与 Logs 均加 data-testid 空态 marker（i18n 文案
//   原已存在），矩阵补 apikeys [empty]（三 tab 巡）+ logs [empty] 两行。
// - error 态由各组件 spec 就地覆盖（见文件头引用），矩阵不重复断言。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/svelte';

import Overview from './Overview.svelte';
import Sites from './Sites.svelte';
import History from './History.svelte';
import ApiKeys from './ApiKeys.svelte';
import Logs from './Logs.svelte';
import * as translationsApi from '../lib/api/translations';

vi.mock('../lib/stores/status', async () => {
  const { writable } = await import('svelte/store');
  return {
    status: writable<any>(null),
    fetchStatus: vi.fn().mockResolvedValue(undefined),
  };
});

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

vi.mock('../lib/api/translations', () => ({
  getTranslations: vi.fn(),
  retryTranslation: vi.fn(),
  batchRetryTranslations: vi.fn(),
  batchDeleteTranslations: vi.fn(),
}));

// English markers regardless of the fixture locale.
import { withEnglishLocale } from '../lib/test-en-locale';

describe('pages/page-state-matrix (批 N2 / U-6: three-state contract)', () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    cleanup();
  });

  it('overview [empty]: pending-review inbox, recent runs and domain detail all render their empty nodes', async () => {
    const fetchMock = vi.fn(async (url: string) => {
      if (url === '/api/stats/overview') {
        return {
          text: async () =>
            JSON.stringify({
              success: true,
              data: { by_domain: [], by_status: [], daily: [] },
            }),
        };
      }
      if (url === '/api/worker/start-check') {
        return {
          text: async () =>
            JSON.stringify({
              success: true,
              data: {
                can_start: true,
                requires_confirmation: false,
                summary: {
                  domains_checked: 0,
                  relations_checked: 0,
                  rules_checked: 0,
                  fields_checked: 0,
                  language_pack_lanes_checked: 0,
                  blocking_missing_components: 0,
                  confirm_missing_components: 0,
                  auto_skip_missing_components: 0,
                },
                missing_components: [],
              },
            }),
        };
      }
      return { text: async () => JSON.stringify({ success: true }) };
    });
    vi.stubGlobal('fetch', fetchMock);

    await withEnglishLocale(async () => {
      render(Overview);
      await waitFor(() => {
        expect(screen.getByText('No items waiting for human review')).toBeTruthy();
      });
      expect(screen.getByText('No Runs')).toBeTruthy();
      expect(screen.getByText('No Data')).toBeTruthy();
    });
  });

  it('sites [empty]: no bound sites renders the empty-list marker', async () => {
    await import('../lib/stores/status');
    vi.stubGlobal('fetch', vi.fn(async () => ({ text: async () => JSON.stringify({ success: true }) })));

    await withEnglishLocale(async () => {
      render(Sites);
      await waitFor(() => {
        expect(screen.getByText('No sites bound yet')).toBeTruthy();
      });
    });
  });

  it('history [empty]: zero records renders No Records', async () => {
    vi.mocked(translationsApi.getTranslations).mockResolvedValue({
      success: true,
      data: {
        records: [],
        total: 0,
        page: 1,
        limit: 20,
      } as never,
    });

    await withEnglishLocale(async () => {
      render(History);
      await waitFor(() => {
        expect(screen.getByText('No Records')).toBeTruthy();
      });
    });
  });

  it('history [loading]: a pending fetch renders the loading node (never a blank surface)', async () => {
    vi.mocked(translationsApi.getTranslations).mockReturnValue(new Promise(() => {}) as never);

    await withEnglishLocale(async () => {
      render(History);
      await waitFor(() => {
        // common.loading renders as 'Loading...' — match the prefix.
        expect(screen.getByText(/Loading/)).toBeTruthy();
      });
    });
  });

  it('apikeys [empty]: vendors/keys/oauth tabs each render their empty-list marker (批 O4)', async () => {
    const fetchMock = vi.fn(async (url: string) => {
      const emptyItems = () => ({ text: async () => JSON.stringify({ success: true, data: { items: [] } }) });
      if (url.startsWith('/api/provider-catalog')) {
        return {
          text: async () =>
            JSON.stringify({ success: true, data: { items: [], catalog_version: 'test-0' } }),
        };
      }
      if (url.startsWith('/api/vendor-keys') || url.startsWith('/api/vendor-oauth')) {
        return emptyItems();
      }
      return { text: async () => JSON.stringify({ success: true, data: {} }) };
    });
    vi.stubGlobal('fetch', fetchMock);

    await withEnglishLocale(async () => {
      render(ApiKeys);
      // Default tab: vendors.
      await waitFor(() => {
        expect(screen.getByText('No provider templates')).toBeTruthy();
      });
      expect(document.querySelector('[data-testid="apikeys-vendors-empty"]')).toBeTruthy();
      // Keys tab.
      await screen.getByTestId('apikeys-tab-keys').click();
      await waitFor(() => {
        expect(screen.getByText('No Keys')).toBeTruthy();
      });
      expect(document.querySelector('[data-testid="apikeys-keys-empty"]')).toBeTruthy();
      // OAuth tab.
      await screen.getByTestId('apikeys-tab-oauth').click();
      await waitFor(() => {
        expect(screen.getByText('No Configs')).toBeTruthy();
      });
      expect(document.querySelector('[data-testid="apikeys-oauth-empty"]')).toBeTruthy();
    });
  });

  it('logs [empty]: initial no-lines state renders the empty marker, never a blank surface (批 O4)', async () => {
    // Logs loads on demand (click "Load Logs") — the initial state is the
    // empty surface; no fetch happens on mount.
    vi.stubGlobal('fetch', vi.fn(async () => ({ text: async () => JSON.stringify({ success: true, data: {} }) })));

    await withEnglishLocale(async () => {
      render(Logs);
      await waitFor(() => {
        expect(screen.getByText(/Click "Load Logs"/)).toBeTruthy();
      });
      expect(document.querySelector('[data-testid="logs-empty"]')).toBeTruthy();
    });
  });
});
