import { beforeEach, describe, expect, it, vi } from 'vitest';
import { listDiscoveryTasks, updateDiscoveryTask } from './tasks';
import { apiFetch } from './client';

vi.mock('./client', () => ({
  apiFetch: vi.fn(),
}));

const apiFetchMock = vi.mocked(apiFetch);

describe('api/tasks', () => {
  beforeEach(() => {
    apiFetchMock.mockReset();
    apiFetchMock.mockResolvedValue({ success: true, data: {} } as never);
  });

  it('lists discovery tasks', async () => {
    await listDiscoveryTasks();
    expect(apiFetchMock).toHaveBeenCalledWith('/api/discovery-tasks');
  });

  it('updates discovery task params', async () => {
    await updateDiscoveryTask(7, { enabled: false, retry_max: 5 });
    expect(apiFetchMock).toHaveBeenCalledWith('/api/discovery-tasks/7', {
      method: 'PUT',
      body: { enabled: false, retry_max: 5 },
    });
  });

  it('updates discovery task execution overrides', async () => {
    await updateDiscoveryTask(9, {
      selected_component_id: 'comp-text',
      effective_source_lang: 'zh_CN',
      effective_target_lang: 'en_US',
      editable_overrides: {
        'default_values.system_prompt': 'Translate product copy',
      },
    });
    expect(apiFetchMock).toHaveBeenCalledWith('/api/discovery-tasks/9', {
      method: 'PUT',
      body: {
        selected_component_id: 'comp-text',
        effective_source_lang: 'zh_CN',
        effective_target_lang: 'en_US',
        editable_overrides: {
          'default_values.system_prompt': 'Translate product copy',
        },
      },
    });
  });
});
