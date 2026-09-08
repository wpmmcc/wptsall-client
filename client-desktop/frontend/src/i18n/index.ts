import { addMessages, init, getLocaleFromNavigator, locale, _ } from 'svelte-i18n';
import { get } from 'svelte/store';
import en from './locales/en.json';
import zhCN from './locales/zh-CN.json';

addMessages('en', en);
addMessages('zh-CN', zhCN);

// P0-LF-05 (mirrors the WebUI client): versioned local storage key. Locale
// persistence stays on this device — no network, no account preference.
export const LOCALE_STORAGE_KEY = 'wptsall_locale.v1';

// The Desktop frontend also runs under vitest (node, no DOM) and vite build
// (no DOM during module init); guard every browser-API touch.
const hasBrowserDom =
  typeof window !== 'undefined' &&
  typeof window.localStorage !== 'undefined' &&
  typeof document !== 'undefined';

/**
 * Strict normalization for stored values: returns the canonical code only on
 * an exact (case/underscore-insensitive) match, otherwise null so the caller
 * can fall back deterministically.
 */
function normalizeLocale(value: string | null | undefined, opts: { lenient?: boolean } = {}): string | null {
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

function detectInitialLocale(): string {
  if (hasBrowserDom) {
    const savedLocale = normalizeLocale(localStorage.getItem(LOCALE_STORAGE_KEY));
    if (savedLocale) return savedLocale;
    return normalizeLocale(getLocaleFromNavigator(), { lenient: true }) ?? 'en';
  }
  return 'en';
}

init({
  fallbackLocale: 'en',
  initialLocale: detectInitialLocale(),
});

if (hasBrowserDom) document.documentElement.lang = detectInitialLocale();

/**
 * Re-apply the locally persisted locale. Device-local only: reads the
 * versioned localStorage key, falls back to the navigator language, then to
 * English, and persists the outcome. Makes zero network calls.
 */
export function initializeI18n(): void {
  const savedLocale = detectInitialLocale();
  locale.set(savedLocale);
  if (hasBrowserDom) {
    document.documentElement.lang = savedLocale;
    localStorage.setItem(LOCALE_STORAGE_KEY, savedLocale);
  }
}

export function setLocale(newLocale: string): void {
  const next = normalizeLocale(newLocale) ?? 'en';
  if (hasBrowserDom) {
    localStorage.setItem(LOCALE_STORAGE_KEY, next);
    document.documentElement.lang = next;
  }
  locale.set(next);
}

/**
 * Script-side translation. Markup uses the auto-subscribed `$_`; event
 * handlers and other imperative code use this helper. svelte-i18n v4 wraps
 * ICU interpolation values inside `options.values` (placeholders `{count}`).
 */
export function t(key: string, params?: Record<string, string | number>): string {
  const translate = get(_);
  const out = translate(key, params ? { values: params } : undefined);
  return out ?? key;
}

export const availableLocales = ['en', 'zh-CN'] as const;
export type LocaleCode = typeof availableLocales[number];
