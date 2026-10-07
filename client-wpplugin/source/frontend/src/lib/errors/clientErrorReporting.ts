// Global client-side error reporting: window.onerror + unhandledrejection
// events are forwarded to the client backend (`POST /api/logs/client`) so
// frontend JS crashes surface in the same JSONL log stream the engine and
// routes write to. Without this, render crashes are invisible to operators
// (the Logs page only shows server-side events).
import { apiFetch } from '../api/client';

export type ClientErrorKind = 'js_error' | 'unhandled_rejection';

export interface ClientErrorReport {
  kind: ClientErrorKind;
  message: string;
  source?: string;
  lineno?: number;
  colno?: number;
  stack?: string;
}

/** Fire-and-forget: reporting must never break the running app. */
export function reportClientError(payload: ClientErrorReport): void {
  apiFetch<{ recorded: boolean }>('/api/logs/client', {
    method: 'POST',
    body: payload,
  })
    .then(() => undefined)
    .catch(() => {
      // Server down / offline desktop / test env without a backend — swallow.
    });
}

/** Extracted for testability; wired to `window` by installGlobalErrorReporting. */
export function handleJsErrorEvent(event: {
  message?: string;
  filename?: string;
  lineno?: number;
  colno?: number;
  error?: unknown;
}): void {
  reportClientError({
    kind: 'js_error',
    message: event.message || 'unknown error',
    source: event.filename,
    lineno: event.lineno,
    colno: event.colno,
    stack: event.error instanceof Error ? event.error.stack : undefined,
  });
}

/** Extracted for testability; wired to `window` by installGlobalErrorReporting. */
export function handleUnhandledRejection(reason: unknown): void {
  reportClientError({
    kind: 'unhandled_rejection',
    message: reason instanceof Error ? reason.message : String(reason),
    stack: reason instanceof Error ? reason.stack : undefined,
  });
}

let installed = false;

/**
 * Register the global listeners once per page load (idempotent — safe to
 * call from both the WebUI and the desktop shell entry points).
 */
export function installGlobalErrorReporting(): void {
  if (installed || typeof window === 'undefined') return;
  installed = true;
  window.addEventListener('error', (event) => {
    handleJsErrorEvent(event as unknown as {
      message?: string;
      filename?: string;
      lineno?: number;
      colno?: number;
      error?: unknown;
    });
  });
  window.addEventListener('unhandledrejection', (event) => {
    handleUnhandledRejection((event as PromiseRejectionEvent).reason);
  });
}
