import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import Settings from './Settings.svelte';
import * as settingsApi from '../lib/api/settings';
import * as workerApi from '../lib/api/worker';
import * as statusModule from '../lib/stores/status';
import * as toastModule from '../lib/stores/toast';

vi.mock('../lib/api/settings', () => ({
  listProxyProfiles: vi.fn(),
  createProxyProfile: vi.fn(),
  updateProxyProfile: vi.fn(),
  deleteProxyProfile: vi.fn(),
  testProxyProfile: vi.fn(),
  loadLogSettings: vi.fn(),
  updateLogSettings: vi.fn(),
  getAccessControl: vi.fn(),
  updateAccessControl: vi.fn(),
}));

vi.mock('../lib/api/worker', () => ({
  getWorkerConfig: vi.fn(),
  saveWorkerConfig: vi.fn(),
}));

vi.mock('../lib/stores/status', async () => {
  const { writable } = await import('svelte/store');
  return {
    status: writable<any>({ worker_loop_poll_seconds: 20 }),
    fetchStatus: vi.fn().mockResolvedValue(undefined),
  };
});

vi.mock('../lib/stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('pages/Settings', () => {
  beforeEach(() => {
    vi.mocked(settingsApi.listProxyProfiles).mockResolvedValue({
      success: true,
      data: { items: [] },
    } as never);
    vi.mocked(settingsApi.loadLogSettings).mockResolvedValue({
      success: true,
      data: { enabled: true, level: 'info' },
    } as never);
    vi.mocked(settingsApi.getAccessControl).mockResolvedValue({
      success: true,
      data: {
        external_access: false,
        allowed_ips: [],
        current_bind: '127.0.0.1',
      },
    } as never);
    vi.mocked(settingsApi.updateAccessControl).mockResolvedValue({
      success: true,
      data: {
        external_access: true,
        allowed_ips: ['10.0.0.8'],
        restart_required: true,
      },
    } as never);
    vi.mocked(workerApi.getWorkerConfig).mockResolvedValue({
      success: true,
      data: {
        poll_seconds: 20,
        auto_start_worker: false,
        domain_concurrency: 3,
        relation_concurrency: 1,
        global_translation_concurrency: 30,
        global_callback_concurrency: 12,
        relation_max_pending_callbacks: 200,
        adaptive_rate_control: true,
        adaptive_max_delay_ms: 5000,
        callback_concurrency: 4,
        callback_timeout_secs: 30,
        callback_retry_max: 2,
        fetch_timeout_secs: 20,
        fetch_retry_max: 2,
        review_mode: false,
      },
    } as never);
    vi.mocked(workerApi.saveWorkerConfig).mockResolvedValue({
      success: true,
      data: {},
    } as never);
    vi.mocked(statusModule.fetchStatus).mockClear();
    vi.mocked(toastModule.showToast).mockClear();
    (statusModule.status as any).set({ worker_loop_poll_seconds: 20 });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('saves worker config with normalized adaptive delay and review mode', async () => {
    render(Settings);

    await fireEvent.click(screen.getByText('Worker 配置'));
    const adaptiveDelayInput = (await screen.findByLabelText(
      '最大退避延迟（毫秒）',
    )) as HTMLInputElement;

    adaptiveDelayInput.value = '1';
    await fireEvent.input(adaptiveDelayInput);
    await fireEvent.click(screen.getByLabelText('开启审核模式'));
    await fireEvent.click(screen.getByText('保存 Worker 配置'));

    await waitFor(() => {
      expect(workerApi.saveWorkerConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          adaptive_max_delay_ms: 200,
          review_mode: true,
        }),
      );
    });

    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Worker 配置已保存');
    expect(statusModule.fetchStatus).toHaveBeenCalledTimes(1);
  });

  it('saves worker config with workflow media step disabled', async () => {
    render(Settings);

    await fireEvent.click(screen.getByText('Worker 配置'));
    const mediaUpButton = await screen.findByLabelText('Move media step up');
    const mediaToggle = mediaUpButton.closest('li')?.querySelector('input[type="checkbox"]');
    expect(mediaToggle).toBeTruthy();
    if (mediaToggle) {
      await fireEvent.click(mediaToggle);
    }
    await fireEvent.click(screen.getByText('保存 Worker 配置'));

    await waitFor(() => {
      expect(workerApi.saveWorkerConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          workflow_dsl: expect.objectContaining({
            steps: expect.arrayContaining([
              expect.objectContaining({ id: 'translate' }),
              expect.objectContaining({ id: 'sync' }),
            ]),
          }),
        }),
      );
    });
    const saveCalls = vi.mocked(workerApi.saveWorkerConfig).mock.calls;
    const saveCall = saveCalls[saveCalls.length - 1]?.[0] as {
      workflow_dsl?: { steps?: Array<{ id?: string }> };
    };
    const stepIds = saveCall?.workflow_dsl?.steps?.map((s) => s.id) ?? [];
    expect(stepIds).not.toContain('media');
  });

  it('shows restart guidance when access-control bind mode changes', async () => {
    render(Settings);

    await fireEvent.click(screen.getByText('访问控制'));
    await fireEvent.click(await screen.findByLabelText('开启外网访问'));
    await fireEvent.input(await screen.findByLabelText('IP 白名单'), {
      target: { value: '10.0.0.8' },
    });
    await fireEvent.click(screen.getByText('保存访问控制'));

    await waitFor(() => {
      expect(settingsApi.updateAccessControl).toHaveBeenCalledWith(true, ['10.0.0.8']);
    });

    expect(toastModule.showToast).toHaveBeenCalledWith(
      'success',
      '访问控制已保存',
      '绑定地址变更需要重启客户端才能生效',
    );
  });
});
