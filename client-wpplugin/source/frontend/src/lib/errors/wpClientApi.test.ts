import { describe, expect, it } from 'vitest';
import { getWpClientApiToastCopy, summarizeBatchApproveFailures } from './wpClientApi';

describe('wpClientApi error copy helpers', () => {
  it('maps runtime license deny to clearer operator copy', () => {
    const toast = getWpClientApiToastCopy(
      '连通测试失败',
      { code: 'wptsall_pro_required', message: 'Client API disabled', status: 403 },
      'site_test'
    );

    expect(toast.message).toBe('站点已拒绝当前连通测试');
    expect(toast.detail).toContain('官网确认授权状态');
    expect(toast.detail).toContain('wptsall_pro_required');
  });

  it('maps revoked wp token errors to regeneration guidance', () => {
    const toast = getWpClientApiToastCopy(
      '执行失败',
      { code: 'client_unauthorized', message: 'Client authentication failed', status: 401 },
      'worker_run'
    );

    expect(toast.message).toBe('Worker 无法继续访问 WP');
    expect(toast.detail).toContain('重新生成 Token');
    expect(toast.detail).toContain('HTTP 401');
  });

  it('summarizes batch approve failures using structured wp error details', () => {
    const detail = summarizeBatchApproveFailures([
      { code: 'wptsall_pro_required', message: 'Client API disabled', status: '403 Forbidden' },
    ]);

    expect(detail).toContain('1 条失败');
    expect(detail).toContain('部分条目被 WP 站点拒绝');
    expect(detail).toContain('官网确认授权状态');
  });
});
