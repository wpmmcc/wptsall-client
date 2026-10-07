/**
 * Locale persistence helpers (P0-LF-05).
 *
 * Browser-local only: reads/writes the versioned localStorage key, falls back
 * to the navigator language, then to English. Makes zero network calls —
 * there is no account-preference route, no session concept.
 */
import { getLocaleFromNavigator } from 'svelte-i18n';

/** Versioned browser-local storage key. */
export const LOCALE_STORAGE_KEY = 'wptsall_locale.v1';

/**
 * Strict normalization for stored values: returns the canonical code only on
 * an exact (case/underscore-insensitive) match, otherwise null so the caller
 * can fall back deterministically.
 */
export function normalizeLocale(value: string | null | undefined, opts: { lenient?: boolean } = {}): string | null {
  if (!value) return null;
  const lower = String(value).trim().toLowerCase().replace(/_/g, '-');
  if (!lower) return null;
  if (lower === 'en') return 'en';
  if (lower === 'zh-cn' || lower === 'zh-hans') return 'zh-CN';
  if (opts.lenient) {
    // navigator.language is a BCP 47 tag (zh-TW, en-GB, fr-FR, ...). Map any
    // Chinese tag to the only supported Chinese variant, any English tag to en.
    if (lower.startsWith('zh')) return 'zh-CN';
    if (lower.startsWith('en')) return 'en';
  }
  return null;
}

export function detectInitialLocale(): string {
  const savedLocale = normalizeLocale(localStorage.getItem(LOCALE_STORAGE_KEY));
  if (savedLocale) return savedLocale;
  return normalizeLocale(getLocaleFromNavigator(), { lenient: true }) ?? 'en';
}
