// catalog: WEBUI-UI-Settings
// oracle: L2
// 状态矩阵：worker 配置保存（自适应延迟归一 + 审核模式）/ 媒体步骤禁用 / 绑定模式变更重启指引。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import Settings from './Settings.svelte';
import * as settingsApi from '../lib/api/settings';
import * as workerApi from '../lib/api/worker';
import * as clientApi from '../lib/api/client';
import * as statusModule from '../lib/stores/status';
import * as toastModule from '../lib/stores/toast';
import { withEnglishLocale } from '../lib/test-en-locale';

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
  getHealthVersion: vi.fn(),
}));

// Partial mock: keep the real result guards (isOk/hasData) and intercept only
// apiFetch — Settings.svelte calls it directly for /api/update-check.
vi.mock('../lib/api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../lib/api/client')>();
  return { ...actual, apiFetch: vi.fn() };
});

vi.mock('../lib/api/worker', () => ({
  getWorkerConfig: vi.fn(),
  saveWorkerConfig: vi.fn(),
}));

vi.mock('../lib/api/storage', () => ({
  getStorageCapacity: vi.fn().mockResolvedValue({
    success: true,
    data: {
      policy: { format: 'wptsall-storage-v1', max_physical_bytes: 34359738368 },
      revision: null,
      physical_retained_bytes: 0,
      physical_retained_files: 0,
      root_booked_bytes: 0,
      max_physical_bytes: 34359738368,
    },
  }),
  saveStorageCapacity: vi.fn(),
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
        current_port: 9099,
      },
    } as never);
    vi.mocked(settingsApi.getHealthVersion).mockResolvedValue({
      success: true,
      data: {
        status: 'ok',
        version: '3.1.0-test',
        ui_version: '3.1.0-ui-test',
        uptime_seconds: 5,
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

  it.each([
    { value: '0', expected: 0 },
    { value: '', expected: 2 },
    { value: '-1', expected: 0 },
    { value: '11', expected: 10 },
  ])('saves both retry limits as $expected for input "$value"', async ({ value, expected }) => {
    render(Settings);
    await fireEvent.click(screen.getByText('Worker 配置'));
    await screen.findByLabelText('开启审核模式');
    for (const field of ['callback-retry-max', 'fetch-retry-max']) {
      const input = document.getElementById(`settings-${field}`) as HTMLInputElement;
      await fireEvent.input(input, { target: { value } });
    }
    await fireEvent.click(screen.getByText('保存 Worker 配置'));
    await waitFor(() => {
      expect(workerApi.saveWorkerConfig).toHaveBeenCalledWith(
        expect.objectContaining({ callback_retry_max: expected, fetch_retry_max: expected }),
      );
    });
  });

  it('reads the effective saved config before showing success', async () => {
    render(Settings);
    await fireEvent.click(screen.getByText('Worker 配置'));
    await screen.findByLabelText('开启审核模式');
    vi.mocked(workerApi.getWorkerConfig).mockResolvedValueOnce({
      success: true,
      data: { callback_retry_max: 0, fetch_retry_max: 0, poll_seconds: 20 },
    });
    await fireEvent.click(screen.getByText('保存 Worker 配置'));
    await waitFor(() => {
      expect((document.getElementById('settings-callback-retry-max') as HTMLInputElement).value).toBe('0');
      expect((document.getElementById('settings-fetch-retry-max') as HTMLInputElement).value).toBe('0');
    });
    expect(toastModule.showToast).toHaveBeenCalledWith('success', 'Worker 配置已保存');
  });

  it('does not claim a save succeeded when its readback fails', async () => {
    render(Settings);
    await fireEvent.click(screen.getByText('Worker 配置'));
    await screen.findByLabelText('开启审核模式');
    vi.mocked(workerApi.getWorkerConfig).mockResolvedValueOnce({
      success: false,
      error: { code: 'TEST_READBACK_FAILED', message: 'readback failed' },
    });
    await fireEvent.click(screen.getByText('保存 Worker 配置'));
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith('error', '保存失败', 'readback failed');
    });
    expect(vi.mocked(toastModule.showToast).mock.calls.some(([kind]) => kind === 'success')).toBe(false);
  });

  it('shows a rejected save and does not attempt a readback', async () => {
    render(Settings);
    await fireEvent.click(screen.getByText('Worker 配置'));
    await screen.findByLabelText('开启审核模式');
    vi.mocked(workerApi.saveWorkerConfig).mockResolvedValueOnce({
      success: false,
      error: { code: 'INVALID_WORKFLOW_POLICY', message: 'Invalid workflow policy.' },
    });
    vi.mocked(workerApi.getWorkerConfig).mockClear();
    await fireEvent.click(screen.getByText('保存 Worker 配置'));
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith('error', '保存失败', 'Invalid workflow policy.');
    });
    expect(workerApi.getWorkerConfig).not.toHaveBeenCalled();
    expect(statusModule.fetchStatus).not.toHaveBeenCalled();
    expect(vi.mocked(toastModule.showToast).mock.calls.some(([kind]) => kind === 'success')).toBe(false);
  });

  it.each(['save', 'readback'])('shows a %s exception without claiming success', async (stage) => {
    render(Settings);
    await fireEvent.click(screen.getByText('Worker 配置'));
    await screen.findByLabelText('开启审核模式');
    if (stage === 'save') {
      vi.mocked(workerApi.saveWorkerConfig).mockRejectedValueOnce(new Error('save transport failed'));
    } else {
      vi.mocked(workerApi.getWorkerConfig).mockRejectedValueOnce(new Error('readback transport failed'));
    }
    await fireEvent.click(screen.getByText('保存 Worker 配置'));
    await waitFor(() => {
      expect(toastModule.showToast).toHaveBeenCalledWith('error', '保存失败', `${stage} transport failed`);
    });
    expect(vi.mocked(toastModule.showToast).mock.calls.some(([kind]) => kind === 'success')).toBe(false);
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

  it.each([
    { name: 'media-only', ids: ['translate', 'media', 'sync'], expected: ['translate', 'media', 'human_review', 'sync'] },
    { name: 'no optional steps', ids: ['translate', 'sync'], expected: ['translate', 'human_review', 'sync'] },
    { name: 'review-first', ids: ['translate', 'human_review', 'media', 'sync'], expected: ['translate', 'human_review', 'media', 'sync'] },
    { name: 'repeated optional steps', ids: ['translate', 'human_review', 'media', 'human_review', 'sync'], expected: ['translate', 'human_review', 'media', 'sync'] },
  ])('restores review after loading a saved $name pipeline', async ({ ids, expected }) => {
    vi.mocked(workerApi.getWorkerConfig).mockResolvedValueOnce({
      success: true,
      data: {
        review_mode: false,
        workflow_policy: { schema_version: 'workflow-policy-v1', default_mode: 'auto' },
        workflow_dsl: {
          schema_version: 'workflow-dsl-v1',
          steps: ids.map(id => ({ id, type: id === 'media' ? 'media' : 'builtin' })),
        },
      },
    });
    render(Settings);
    await fireEvent.click(screen.getByTestId('settings-tab-worker'));
    await screen.findByLabelText('开启审核模式');
    await fireEvent.click(screen.getByTestId('settings-review-toggle'));
    await fireEvent.click(screen.getByTestId('settings-save-worker'));
    await waitFor(() => expect(workerApi.saveWorkerConfig).toHaveBeenCalledTimes(1));
    const saved = vi.mocked(workerApi.saveWorkerConfig).mock.calls[0][0];
    expect(saved.review_mode).toBe(true);
    expect(saved.workflow_policy?.default_mode).toBe('review');
    const dsl = saved.workflow_dsl as { steps: Array<{ id: string; type: string; when?: string }> };
    expect(dsl.steps.map(step => step.id)).toEqual(expected);
    expect(dsl.steps.filter(step => step.id === 'human_review')).toEqual([
      { id: 'human_review', type: 'human_review', when: 'policy' },
    ]);
  });

  it('re-enables media after loading a review-only pipeline without duplicating or reordering review', async () => {
    vi.mocked(workerApi.getWorkerConfig).mockResolvedValueOnce({
      success: true,
      data: {
        review_mode: true,
        workflow_policy: { schema_version: 'workflow-policy-v1', default_mode: 'review' },
        workflow_dsl: {
          schema_version: 'workflow-dsl-v1',
          steps: [
            { id: 'translate', type: 'builtin' },
            { id: 'human_review', type: 'human_review', when: 'policy' },
            { id: 'sync', type: 'builtin' },
          ],
        },
      },
    });
    render(Settings);
    await fireEvent.click(screen.getByTestId('settings-tab-worker'));
    await screen.findByLabelText('禁用审阅');
    const mediaUp = await screen.findByLabelText('Move media step up');
    const media = mediaUp.closest('li')!.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    expect(media.checked).toBe(false);
    await fireEvent.click(media);
    await fireEvent.click(screen.getByTestId('settings-save-worker'));
    await waitFor(() => expect(workerApi.saveWorkerConfig).toHaveBeenCalledTimes(1));
    const saved = vi.mocked(workerApi.saveWorkerConfig).mock.calls[0][0];
    const dsl = saved.workflow_dsl as { steps: Array<{ id: string }> };
    expect(dsl.steps.map(step => step.id)).toEqual(['translate', 'human_review', 'media', 'sync']);
  });

  it('shows restart guidance when access-control bind mode changes', async () => {
    render(Settings);

    await fireEvent.click(screen.getByText('访问控制'));
    // The bind display must render the backend-reported port, never a
    // hard-coded default (UI-27-02 regression guard).
    expect(await screen.findByText('127.0.0.1:9099')).toBeTruthy();
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

  it('saves worker config with multi-dimensional workflow policy (format, domain, rule rules)', async () => {
    vi.mocked(workerApi.getWorkerConfig).mockResolvedValueOnce({
      success: true,
      data: {
        poll_seconds: 20,
        review_mode: false,
        workflow_policy: {
          schema_version: 'workflow-policy-v1',
          default_mode: 'auto',
          by_domain: { 'shop.example.com': 'review' },
          by_content_format: { html: 'review' },
          by_rule: { post_content: 'review' },
        },
      },
    } as never);

    render(Settings);

    await fireEvent.click(screen.getByText('Worker 配置'));
    expect(await screen.findByText('多维同步策略')).toBeTruthy();

    await fireEvent.click(screen.getByText('保存 Worker 配置'));

    await waitFor(() => {
      expect(workerApi.saveWorkerConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          workflow_policy: expect.objectContaining({
            schema_version: 'workflow-policy-v1',
            by_domain: expect.objectContaining({ 'shop.example.com': 'review' }),
            by_content_format: expect.objectContaining({ html: 'review' }),
            by_rule: expect.objectContaining({ post_content: 'review' }),
          }),
        }),
      );
    });
  });

  it('About tab pre-displays local versions and shows a friendly error on failed update check', async () => {
    const warnSpy = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      // Failure payload mirrors the Rust handler: success=false with raw
      // transport error + local versions in data.
      vi.mocked(clientApi.apiFetch).mockResolvedValueOnce({
        success: false,
        error: {
          code: 'UPDATE_CHECK_FAILED',
          message: 'error sending request for url (http://127.0.0.1:1/api/v1/client/releases)',
        },
        data: {
          current_version: '3.1.0-test',
          ui_current_version: '3.1.0-ui-test',
        },
      } as never);

      render(Settings);

      // Versions are visible BEFORE any update check (local /health read).
      await fireEvent.click(screen.getByText('关于'));
      const versionBlock = await screen.findByTestId('about-local-versions');
      expect(within(versionBlock).getByText(/Binary: v3\.1\.0-test/)).toBeTruthy();
      expect(within(versionBlock).getByText(/UI: v3\.1\.0-ui-test/)).toBeTruthy();

      // Run the (failing) check: friendly localized message, no raw error leak.
      await fireEvent.click(screen.getByTestId('about-check-update'));

      const errBanner = await screen.findByTestId('about-update-error');
      expect(errBanner.textContent).toContain('更新检查失败');
      expect(errBanner.textContent).not.toContain('error sending request');
      expect(errBanner.textContent).not.toContain('127.0.0.1:1');
      // Raw detail goes to the console, not the UI.
      expect(warnSpy).toHaveBeenCalledWith(
        '[Settings] update check failed:',
        'UPDATE_CHECK_FAILED',
        'error sending request for url (http://127.0.0.1:1/api/v1/client/releases)',
      );
      // Versions stay visible after the failed check.
      expect(screen.getByTestId('about-local-versions')).toBeTruthy();
    } finally {
      warnSpy.mockRestore();
    }
  });
  it('renders its heading in the EN locale (UI-27-04 EN smoke)', async () => {
    await withEnglishLocale(async () => {
      render(Settings);
      expect(
        await screen.findByRole('heading', { level: 2, name: 'Settings' }),
      ).toBeTruthy();
    });
  });

  it('saves retained capacity limits and reloads their independent readback', async () => {
    await withEnglishLocale(async () => {
      vi.mocked(workerApi.getWorkerConfig).mockResolvedValue({
        success: true,
        data: { storage_max_retained_units: 17, storage_max_reserved_bytes: 8589934592 },
      } as never);
      const first = render(Settings);
      await fireEvent.click(screen.getByText('Worker'));
      const units = await screen.findByLabelText('Retained provider units') as HTMLInputElement;
      const bytes = screen.getByLabelText('Reserved provider storage (bytes)') as HTMLInputElement;
      await waitFor(() => expect(units.value).toBe('17'));
      expect(bytes.value).toBe('8589934592');
      await fireEvent.input(units, { target: { value: '19' } });
      await fireEvent.input(bytes, { target: { value: '10737418240' } });
      vi.mocked(workerApi.getWorkerConfig).mockResolvedValue({
        success: true,
        data: { storage_max_retained_units: 19, storage_max_reserved_bytes: 10737418240 },
      } as never);
      await fireEvent.click(screen.getByText('Save Worker'));
      await waitFor(() => expect(workerApi.saveWorkerConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          storage_max_retained_units: 19,
          storage_max_reserved_bytes: 10737418240,
        }),
      ));
      await waitFor(() => expect(workerApi.getWorkerConfig).toHaveBeenCalledTimes(2));
      first.unmount();
      render(Settings);
      await fireEvent.click(screen.getByText('Worker'));
      await waitFor(() => expect((screen.getByLabelText('Retained provider units') as HTMLInputElement).value).toBe('19'));
      expect((screen.getByLabelText('Reserved provider storage (bytes)') as HTMLInputElement).value).toBe('10737418240');
    });
  });

  it.each(['0', '1.5', '9007199254740992'])('refuses unsafe retained capacity input %s before saving', async (value) => {
    await withEnglishLocale(async () => {
      render(Settings);
      await fireEvent.click(screen.getByText('Worker'));
      const input = await screen.findByLabelText('Retained provider units');
      await fireEvent.input(input, { target: { value } });
      await fireEvent.click(screen.getByText('Save Worker'));
      expect(workerApi.saveWorkerConfig).not.toHaveBeenCalled();
      expect(toastModule.showToast).toHaveBeenCalledWith(
        'error', 'Retained capacity limits must be positive safe integers.',
      );
    });
  });

});

