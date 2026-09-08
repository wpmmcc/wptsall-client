import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import Tasks from './Tasks.svelte';
import * as tasksApi from '../lib/api/tasks';
import * as jobsApi from '../lib/api/jobs';
import * as componentsApi from '../lib/api/components';

vi.mock('../lib/api/tasks', () => ({
  listDiscoveryTasks: vi.fn(),
  updateDiscoveryTask: vi.fn(),
}));

vi.mock('../lib/api/jobs', () => ({
  getJob: vi.fn(),
  listJobs: vi.fn(),
  listAllJobs: vi.fn(),
  listJobItems: vi.fn(),
}));

vi.mock('../lib/api/components', () => ({
  listAllLocalComponents: vi.fn(),
}));

describe('pages/Tasks', () => {
  beforeEach(() => {
    vi.mocked(tasksApi.listDiscoveryTasks).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);
    vi.mocked(componentsApi.listAllLocalComponents).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);
    vi.mocked(jobsApi.listJobs).mockResolvedValue({
      success: true,
      data: {
        items: [
          {
            id: 1,
            domain: 'https://blog.wpmm.cc',
            relation_id: 9,
            business_line: 'post_content',
            status: 'running',
            total_items: 1,
            done_items: 0,
            failed_items: 0,
            triggered_by: 'worker',
            started_at: null,
            completed_at: null,
            created_at: 1,
            updated_at: 1,
            progress: {
              total: 1,
              done: 0,
              failed: 0,
              pending_review: 0,
              translating: 1,
            },
          },
        ],
      },
    } as never);
    vi.mocked(jobsApi.getJob).mockResolvedValue({
      success: true,
      data: {
        id: 1,
        domain: 'https://blog.wpmm.cc',
        relation_id: 9,
        business_line: 'post_content',
        status: 'running',
        total_items: 1,
        done_items: 0,
        failed_items: 0,
        triggered_by: 'worker',
        started_at: null,
        completed_at: null,
        created_at: 1,
        updated_at: 1,
        progress: {
          total: 1,
          done: 0,
          failed: 0,
          pending_review: 0,
          translating: 1,
        },
      },
    } as never);
    vi.mocked(jobsApi.listJobItems).mockResolvedValue({
      success: true,
      data: {
        items: [
          {
            id: 101,
            job_id: 1,
            domain: 'https://blog.wpmm.cc',
            relation_id: 9,
            business_line: 'post_content',
            object_type: 'post',
            wp_object_id: 88,
            wp_object_subtype: 'post',
            task_type: 'mixed',
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            component_id: 'comp-text',
            component_ids: ['comp-text', 'comp-image'],
            selected_component_id: 'comp-text-override',
            raw_path: '/tmp/raw.json',
            translated_path: '/tmp/translated.json',
            status: 'translated',
            client_task_id: 'ctask-1',
            upload_id: null,
            wp_attachment_id: null,
            error_message: null,
            retry_count: 0,
            max_retries: 3,
            fetched_at: null,
            translated_at: 100,
            synced_at: null,
            created_at: 1,
            updated_at: 1,
          },
        ],
      },
    } as never);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('shows multi-component execution trace for expanded job items', async () => {
    render(Tasks, { onReviewItem: vi.fn() });

    await waitFor(() => {
      expect(screen.getByText('blog.wpmm.cc')).toBeTruthy();
    });

    await fireEvent.click(screen.getByText('blog.wpmm.cc'));

    await waitFor(() => {
      expect(screen.getByText('实际执行:')).toBeTruthy();
    });

    expect(screen.getByText('comp-text-override')).toBeTruthy();
    expect(screen.getByText('comp-text -> comp-image')).toBeTruthy();
  });
});
