import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  approveItem,
  batchApproveItems,
  batchRejectItems,
  getItemContent,
  listPendingReviewItems,
  resubmitItem,
  retranslateItem,
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

  it('lists pending review and batch rejects', async () => {
    await listPendingReviewItems({ limit: 50, countOnly: true });
    await batchRejectItems([3, 4], 'nope');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/items/pending-review?limit=50&count_only=1');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/items/batch-reject', {
      method: 'POST',
      body: { ids: [3, 4], reason: 'nope' },
    });
  });
});

describe('manual request fee identity', () => {
  beforeEach(() => { localStorage.clear(); vi.clearAllMocks(); });
  afterEach(() => { localStorage.clear(); vi.restoreAllMocks(); });

  it('retains one request across failure and module reload, then creates a new explicit request', async () => {
    apiFetchMock.mockResolvedValueOnce({ success: false, error: { code: 'NETWORK_ERROR', message: 'owned lost reply' } });
    await retranslateItem(8);
    const first = apiFetchMock.mock.calls[0][1]?.body as { request_id: string };
    expect(first.request_id).toMatch(/^[0-9a-f-]{36}$/);
    vi.resetModules();
    const reloaded = await import('./items');
    apiFetchMock.mockResolvedValueOnce({ success: true, data: { item_id: 8, status: 'pending_review', request_id: first.request_id } });
    await reloaded.retranslateItem(8);
    expect(apiFetchMock.mock.calls[1][1]?.body).toEqual({ ...first, resume_only: true });
    apiFetchMock.mockResolvedValueOnce({ success: false, error: { code: 'owned', message: 'owned' } });
    await reloaded.retranslateItem(8);
    expect(apiFetchMock.mock.calls[2][1]?.body).not.toEqual(first);
  });

  it('uses the unfinished request returned by an independent server read', async () => {
    const request = 'c1b05b00-4af3-4a5f-9d88-13b6dc674f11';
    apiFetchMock.mockResolvedValueOnce({ success: false, error: { code: 'owned', message: 'owned' } });
    await retranslateItem(8, request);
    expect(apiFetchMock).toHaveBeenCalledWith('/api/items/8/retranslate', { method: 'POST', body: { request_id: request, resume_only: true } });
  });

  it('refuses before POST when request storage is unavailable or damaged', async () => {
    localStorage.setItem('wptsall:manual-request:8', '{damaged');
    expect((await retranslateItem(8)).success).toBe(false);
    expect(apiFetchMock).not.toHaveBeenCalled();
    localStorage.clear();
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('owned quota'); });
    expect((await retranslateItem(8)).success).toBe(false);
    expect(apiFetchMock).not.toHaveBeenCalled();
  });

  it('does not acknowledge a mismatched request receipt', async () => {
    apiFetchMock.mockResolvedValueOnce({ success: true, data: { request_id: 'ec30126b-97fa-4a98-8f3b-032e284f7996' } });
    expect((await retranslateItem(8)).success).toBe(false);
    expect(localStorage.getItem('wptsall:manual-request:8')).not.toBeNull();
  });

  it('does not erase a newer request when an older response arrives late', async () => {
    const original = 'c1b05b00-4af3-4a5f-9d88-13b6dc674f11';
    const newer = 'ec30126b-97fa-4a98-8f3b-032e284f7996';
    localStorage.setItem('wptsall:manual-request:8', original);
    apiFetchMock.mockImplementationOnce(async () => {
      localStorage.setItem('wptsall:manual-request:8', newer);
      return { success: true, data: { item_id: 8, status: 'pending_review', request_id: original } } as never;
    });
    expect((await retranslateItem(8)).success).toBe(true);
    expect(localStorage.getItem('wptsall:manual-request:8')).toBe(newer);
  });

  it('marks a browser or server retry as resume-only, not a new fee request', async () => {
    const request = 'c1b05b00-4af3-4a5f-9d88-13b6dc674f11';
    apiFetchMock.mockResolvedValue({ success: false, error: { code: 'owned', message: 'owned' } });
    localStorage.setItem('wptsall:manual-request:8', request);
    await retranslateItem(8);
    expect(apiFetchMock.mock.calls[0][1]?.body).toEqual({ request_id: request, resume_only: true });
    localStorage.clear();
    await retranslateItem(8, request);
    expect(apiFetchMock.mock.calls[1][1]?.body).toEqual({ request_id: request, resume_only: true });
  });

  it('refuses a nil UUID before POST', async () => {
    apiFetchMock.mockResolvedValue({ success: false, error: { code: 'INVALID_REQUEST_ID', message: 'owned nil UUID' } });
    localStorage.setItem('wptsall:manual-request:8', '00000000-0000-0000-0000-000000000000');
    expect((await retranslateItem(8)).success).toBe(false);
    expect(apiFetchMock).not.toHaveBeenCalled();
  });

  it('retains the request when a matching UUID receipt belongs to the wrong item', async () => {
    const request = 'c1b05b00-4af3-4a5f-9d88-13b6dc674f11';
    localStorage.setItem('wptsall:manual-request:8', request);
    apiFetchMock.mockResolvedValue({ success: true, data: { item_id: 9, status: 'pending_review', request_id: request } });
    expect((await retranslateItem(8)).success).toBe(false);
    expect(localStorage.getItem('wptsall:manual-request:8')).toBe(request);
  });
});
