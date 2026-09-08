import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  approveItem,
  batchApproveItems,
  getItemContent,
  resubmitItem,
  saveItemOverride,
  saveTranslated,
} from './items';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/items', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('fetches item content', async () => {
    await getItemContent(12);
    expect(apiFetchMock).toHaveBeenCalledWith('/api/items/12/content');
  });

  it('saves translated payload', async () => {
    await saveTranslated(5, { title: 'hi' });
    expect(apiFetchMock).toHaveBeenCalledWith('/api/items/5/translated', {
      method: 'PUT',
      body: { content: { title: 'hi' } },
    });
  });

  it('preserves explicit nulls in override payload', async () => {
    await saveItemOverride(5, {
      component_id: null,
      source_lang: null,
      target_lang: 'zh_CN',
      editable_overrides: {},
    });
    expect(apiFetchMock).toHaveBeenCalledWith('/api/items/5/override', {
      method: 'PUT',
      body: {
        component_id: null,
        source_lang: null,
        target_lang: 'zh_CN',
        editable_overrides: {},
      },
    });
  });

  it('submits approve/resubmit and batch approve', async () => {
    await resubmitItem(8);
    await approveItem(8);
    await batchApproveItems([1, 2]);

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/items/8/resubmit', { method: 'POST' });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/items/8/approve', { method: 'POST' });
    expect(apiFetchMock).toHaveBeenNthCalledWith(3, '/api/items/batch-approve', {
      method: 'POST',
      body: { ids: [1, 2] },
    });
  });
});
