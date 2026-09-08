import type { ApiResult } from './types';

export async function apiFetch<T>(
  path: string,
  options: { method?: string; body?: unknown } = {}
): Promise<ApiResult<T>> {
  try {
    const res = await fetch(path, {
      method: options.method ?? 'GET',
      headers: { 'Content-Type': 'application/json' },
      body: options.body !== undefined ? JSON.stringify(options.body) : undefined,
    });
    const text = await res.text();
    try {
      const parsed = JSON.parse(text);
      if (
        parsed &&
        typeof parsed === 'object' &&
        'success' in parsed &&
        (parsed as { success?: unknown }).success === false &&
        'error' in parsed &&
        parsed.error &&
        typeof parsed.error === 'object'
      ) {
        return {
          ...(parsed as Record<string, unknown>),
          error: {
            ...(parsed.error as Record<string, unknown>),
            status: typeof res.status === 'number' ? res.status : undefined,
          },
        } as ApiResult<T>;
      }
      return parsed;
    }
    catch {
      return {
        success: false,
        error: {
          code: 'PARSE_ERROR',
          message: text,
          status: typeof res.status === 'number' ? res.status : undefined,
        },
      };
    }
  } catch (e) {
    return { success: false, error: { code: 'NETWORK_ERROR', message: String(e) } };
  }
}

export function isOk<T>(r: ApiResult<T>): r is { success: true; data: T } {
  return r.success === true;
}

export function hasData<T>(r: ApiResult<T>): r is { success: true; data: T } {
  return r.success === true && Object.prototype.hasOwnProperty.call(r, 'data') && (r as { data?: T }).data !== undefined;
}
