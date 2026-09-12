// catalog: WEBUI-UI-Products
// oracle: L2
// 状态矩阵：产品渲染 + 激活权益徽章 / 空态。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import Products from './Products.svelte';
import * as toastModule from '../lib/stores/toast';

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

function createFetchMock(products: unknown[] = [], entitlements: unknown[] = []) {
  return vi.fn(async (url: string) => {
    if (url === '/api/v1/platform/products') {
      return {
        text: async () =>
          JSON.stringify({ success: true, data: products }),
      };
    }
    if (url === '/api/v1/platform/entitlements') {
      return {
        text: async () =>
          JSON.stringify({ success: true, data: entitlements }),
      };
    }
    return { text: async () => JSON.stringify({ success: true }) };
  });
}

describe('pages/Products', () => {
  beforeEach(() => {
    vi.mocked(toastModule.showToast).mockClear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    cleanup();
  });

  it('renders products and shows active entitlement badge', async () => {
    vi.stubGlobal(
      'fetch',
      createFetchMock(
        [
          {
            id: 'p1',
            name: 'Pro Plan',
            description: 'Advanced features',
            price_cents: 1999,
            currency: 'USD',
            is_active: true,
          },
          {
            id: 'p2',
            name: 'Basic Plan',
            description: 'Essential features',
            price_cents: 999,
            currency: 'USD',
            is_active: true,
          },
        ],
        [
          {
            product_id: 'p1',
            product_name: 'Pro Plan',
            tier: 'premium',
            capabilities: {},
            granted_at: '2024-06-01T00:00:00Z',
            is_active: true,
          },
        ],
      ),
    );

    render(Products);

    await waitFor(() => {
      // P0-LF-04: the page is legacy-only and visibly titled as such.
      expect(screen.getByText('旧版服务器控制面')).toBeTruthy();
      expect(screen.getByText('Pro Plan')).toBeTruthy();
      expect(screen.getByText('Basic Plan')).toBeTruthy();
      expect(screen.getByText('有效 · premium')).toBeTruthy();
      expect(screen.getByText('未订阅')).toBeTruthy();
    });
  });

  it('shows empty state when no products exist', async () => {
    vi.stubGlobal('fetch', createFetchMock([], []));

    render(Products);

    await waitFor(() => {
      expect(screen.getByText('暂无可用产品')).toBeTruthy();
    });
  });
});
