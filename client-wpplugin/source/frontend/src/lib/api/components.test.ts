import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
  createComponentVersion,
  deleteBinding,
  deleteComponentVersion,
  deleteRuleBinding,
  deleteTaskTypeBinding,
  listAllLocalComponents,
  listLocalComponents,
  quickTestLocalComponent,
  testComponentVersion,
  updateLocalComponent,
  upsertBinding,
  upsertRuleBinding,
  upsertTaskTypeBinding,
} from './components';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/components', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('builds listLocalComponents query params', async () => {
    await listLocalComponents(2, 'hello world');

    expect(apiFetchMock).toHaveBeenCalledWith('/api/components/local?page=2&per_page=50&q=hello%20world');
  });

  it('walks all local component pages', async () => {
    apiFetchMock
      .mockResolvedValueOnce({
        success: true,
        data: {
          items: [{ id: 'comp-1' }],
          page: 1,
          per_page: 50,
          total: 2,
          total_pages: 2,
        },
      } as never)
      .mockResolvedValueOnce({
        success: true,
        data: {
          items: [{ id: 'comp-2' }],
          page: 2,
          per_page: 50,
          total: 2,
          total_pages: 2,
        },
      } as never);

    const res = await listAllLocalComponents();

    expect(res).toEqual({
      success: true,
      data: {
        items: [{ id: 'comp-1' }, { id: 'comp-2' }],
        total: 2,
      },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/components/local?page=1&per_page=50');
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/components/local?page=2&per_page=50');
  });

  it('encodes component id in update/delete version endpoints', async () => {
    await updateLocalComponent('comp/1', { name: 'New' });
    await createComponentVersion('comp/1', { version: 'v1' });
    await deleteComponentVersion('comp/1', 'v/1');

    expect(apiFetchMock).toHaveBeenNthCalledWith(
      1,
      '/api/components/local/comp%2F1',
      { method: 'PUT', body: { name: 'New' } },
    );
    expect(apiFetchMock).toHaveBeenNthCalledWith(
      2,
      '/api/components/local/comp%2F1/versions',
      { method: 'POST', body: { version: 'v1' } },
    );
    expect(apiFetchMock).toHaveBeenNthCalledWith(
      3,
      '/api/components/local/comp%2F1/versions/v%2F1',
      { method: 'DELETE' },
    );
  });

  it('uses defaults for testComponentVersion', async () => {
    await testComponentVersion('comp-a', 'v1');

    expect(apiFetchMock).toHaveBeenCalledWith(
      '/api/components/local/comp-a/versions/v1/test',
      {
        method: 'POST',
        body: {
          text: 'Hello World',
          source_lang: 'en_US',
          target_lang: 'zh_CN',
        },
      },
    );
  });

  it('sends explicit rule binding payloads', async () => {
    await upsertRuleBinding('plugin', 'image.alt', 'comp-a', 'wordpress-seo');
    await deleteRuleBinding('relation', 'text.title', '12');

    expect(apiFetchMock).toHaveBeenNthCalledWith(
      1,
      '/api/rule-component-bindings/upsert',
      {
        method: 'POST',
        body: {
          scope: 'plugin',
          scope_key: 'wordpress-seo',
          slot_key: 'image.alt',
          component_id: 'comp-a',
        },
      },
    );
    expect(apiFetchMock).toHaveBeenNthCalledWith(
      2,
      '/api/rule-component-bindings/delete',
      {
        method: 'POST',
        body: {
          scope: 'relation',
          scope_key: '12',
          slot_key: 'text.title',
        },
      },
    );
  });


  it('passes through component auth binding payloads', async () => {
    await upsertBinding({
      component_id: 'comp-a',
      auth: { api_key: 'sk-1' },
      key_ids: ['key-1'],
      oauth_ids: ['oauth-1'],
      auth_strategy: 'round_robin',
      constraints_override: { max_file_size_mb: 10 },
      request_overrides: { headers: { 'X-Test': '1' } },
    });
    await deleteBinding('comp-a');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/components/bindings/upsert', {
      method: 'POST',
      body: {
        component_id: 'comp-a',
        auth: { api_key: 'sk-1' },
        key_ids: ['key-1'],
        oauth_ids: ['oauth-1'],
        auth_strategy: 'round_robin',
        constraints_override: { max_file_size_mb: 10 },
        request_overrides: { headers: { 'X-Test': '1' } },
      },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/components/bindings/delete', {
      method: 'POST',
      body: { component_id: 'comp-a' },
    });
  });

  it('sends task type binding payloads', async () => {
    await upsertTaskTypeBinding('text_translation', 'comp-a', 'post_content');
    await deleteTaskTypeBinding('text_translation', 'post_content');

    expect(apiFetchMock).toHaveBeenNthCalledWith(1, '/api/task-type-components/upsert', {
      method: 'POST',
      body: {
        task_type: 'text_translation',
        component_id: 'comp-a',
        business_line: 'post_content',
      },
    });
    expect(apiFetchMock).toHaveBeenNthCalledWith(2, '/api/task-type-components/delete', {
      method: 'POST',
      body: {
        task_type: 'text_translation',
        business_line: 'post_content',
      },
    });
  });

  it('hits quick-test endpoint', async () => {
    await quickTestLocalComponent('comp/x', { api_key: 'sk-1', text: 't' });

    expect(apiFetchMock).toHaveBeenCalledWith(
      '/api/components/local/comp%2Fx/quick-test',
      {
        method: 'POST',
        body: { api_key: 'sk-1', text: 't' },
      },
    );
  });
});
