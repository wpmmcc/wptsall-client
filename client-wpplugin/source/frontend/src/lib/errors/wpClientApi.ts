import type { ApiErr } from '../api/types';

export type WpClientApiErrorContext =
  | 'site_test'
  | 'worker_run'
  | 'item_approve'
  | 'item_resubmit'
  | 'item_retranslate'
  | 'batch_approve';

interface ToastCopy {
  message: string;
  detail?: string;
}

type ErrorShape = ApiErr['error'] | { code?: string; message?: string; status?: number } | undefined;

interface BatchApproveFailureLike {
  code?: string;
  message?: string;
  error?: string;
  status?: string;
  reason?: string;
}

function normalizeMessage(value: string | undefined): string {
  return (value ?? '').trim();
}

function matchesRuntimeLicenseDeny(code: string, message: string): boolean {
  return (
    code === 'wptsall_pro_required' ||
    message.includes('Client API disabled') ||
    message.includes('requires a Pro license')
  );
}

function matchesRevokedWpToken(code: string, message: string): boolean {
  return code === 'client_unauthorized' || message.includes('Client authentication failed');
}

function contextTitle(context: WpClientApiErrorContext, tokenRevoked: boolean): string {
  if (tokenRevoked) {
    switch (context) {
      case 'site_test':
        return 'WP Client Token 已失效';
      case 'worker_run':
        return 'Worker 无法继续访问 WP';
      case 'item_approve':
        return 'WP Token 已失效，无法确认同步';
      case 'item_resubmit':
        return 'WP Token 已失效，无法重新提交';
      case 'item_retranslate':
        return 'WP Token 已失效，无法重新翻译';
      case 'batch_approve':
        return '部分条目因 WP Token 失效被拒绝';
    }
  }

  switch (context) {
    case 'site_test':
      return '站点已拒绝当前连通测试';
    case 'worker_run':
      return 'Worker 被 WP 站点拒绝';
    case 'item_approve':
      return 'WP 已拒绝当前确认同步';
    case 'item_resubmit':
      return 'WP 已拒绝当前重新提交';
    case 'item_retranslate':
      return 'WP 已拒绝当前重新翻译';
    case 'batch_approve':
      return '部分条目被 WP 站点拒绝';
  }
}

function rawSuffix(error: ErrorShape): string {
  if (!error) return '';
  const parts = [error.code, error.status ? `HTTP ${error.status}` : '', error.message]
    .filter(Boolean)
    .join(' / ');
  return parts ? ` 原始错误：${parts}` : '';
}

export function getWpClientApiToastCopy(
  fallbackMessage: string,
  error: ErrorShape,
  context: WpClientApiErrorContext
): ToastCopy {
  const code = error?.code ?? '';
  const message = normalizeMessage(error?.message);

  if (matchesRuntimeLicenseDeny(code, message)) {
    return {
      message: contextTitle(context, false),
      detail:
        '该站点的插件授权已失效、当前套餐不再支持，或插件端已关闭 Client API。请先在官网确认授权状态，再到 WP 插件授权页检查 Client API 设置。' +
        rawSuffix(error),
    };
  }

  if (matchesRevokedWpToken(code, message)) {
    return {
      message: contextTitle(context, true),
      detail:
        '该站点保存的 WP Client Token 无效或已被吊销。若刚发生授权失效，这是预期行为；请在 WP 插件重新生成 Token 并更新本地绑定后重试。' +
        rawSuffix(error),
    };
  }

  return {
    message: fallbackMessage,
    detail: message || undefined,
  };
}

export function summarizeBatchApproveFailures(failed: BatchApproveFailureLike[]): string | undefined {
  if (failed.length === 0) return undefined;

  const first = failed.find((item) => item.code || item.message || item.error) ?? failed[0];
  if (!first) return undefined;

  const toast = getWpClientApiToastCopy(
    '批量审批失败',
    {
      code: first.code,
      message: first.message ?? first.error,
    },
    'batch_approve'
  );

  return `${failed.length} 条失败。${toast.message}${toast.detail ? `：${toast.detail}` : ''}`;
}

export function summarizeBatchApproveSkipped(skipped: BatchApproveFailureLike[]): string | undefined {
  if (skipped.length === 0) return undefined;
  const first = skipped.find((item) => item.reason || item.message) ?? skipped[0];
  const reason = normalizeMessage(first.reason ?? first.message);
  return reason || `跳过 ${skipped.length} 条`;
}
