import type { ApiResult } from './types';

export type FetchHandler = <T>(
  path: string,
  options?: { method?: string; body?: unknown }
) => Promise<ApiResult<T>>;

let customFetchHandler: FetchHandler | null = null;

export function setApiFetchHandler(handler: FetchHandler | null) {
  customFetchHandler = handler;
}

const WEBUI_TOKEN_STORAGE_KEY = 'wptsall_webui_token';

/** Read the persisted WebUI access token (S2 external-mode gate). */
export function readWebUiToken(): string | null {
  try {
    return localStorage.getItem(WEBUI_TOKEN_STORAGE_KEY);
  } catch {
    return null;
  }
}

/** Persist the WebUI access token for subsequent requests. */
export function storeWebUiToken(token: string) {
  try {
    localStorage.setItem(WEBUI_TOKEN_STORAGE_KEY, token);
  } catch {
    /* storage unavailable — requests will keep 401ing */
  }
}

/** Ask the operator for the token when the server demands one (remote
 *  browser in external mode). Returns null on cancel or when prompting is
 *  unavailable (tests, non-browser hosts); never blocks retry loops. */
function promptForWebUiToken(): string | null {
  if (typeof window === 'undefined' || typeof window.prompt !== 'function') {
    return null;
  }
  const token = window.prompt(
    'Enter the Web UI access token (shown in Settings → Access Control on the server machine)'
  );
  return token && token.trim() ? token.trim() : null;
}

export async function apiFetch<T>(
  path: string,
  options: { method?: string; body?: unknown } = {}
): Promise<ApiResult<T>> {
  if (customFetchHandler) {
    return customFetchHandler<T>(path, options);
  }
  // Single async frame with a retry loop (not a recursive helper): the
  // original microtask cadence is preserved for every non-token path, so
  // component tests that assert right after `waitFor(fetch called)` keep
  // their timing.
  for (let attempt = 0; ; attempt++) {
    try {
      const token = readWebUiToken();
      const res = await fetch(path, {
        method: options.method ?? 'GET',
        headers: {
          'Content-Type': 'application/json',
          ...(token ? { 'X-WPTSALL-WebUI-Token': token } : {}),
        },
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
          const error = parsed.error as Record<string, unknown>;
          // S2 token gate: prompt once for the token, store it, retry once.
          if (error.code === 'WEBUI_TOKEN_REQUIRED' && attempt === 0) {
            const entered = promptForWebUiToken();
            if (entered) {
              storeWebUiToken(entered);
              continue;
            }
          }
          return {
            ...(parsed as Record<string, unknown>),
            error: {
              ...error,
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
}

export function isOk<T>(r: ApiResult<T>): r is { success: true; data: T } {
  return r.success === true;
}

export function hasData<T>(r: ApiResult<T>): r is { success: true; data: T } {
  return r.success === true && Object.prototype.hasOwnProperty.call(r, 'data') && (r as { data?: T }).data !== undefined;
}
