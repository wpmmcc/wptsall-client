import { addMessages, init, getLocaleFromNavigator, locale } from 'svelte-i18n';
import en from './locales/en.json';
import zhCN from './locales/zh-CN.json';
import sharedEn from '../../../locales/en.json';
import sharedZhCN from '../../../locales/zh-CN.json';

/** Structural match for svelte-i18n's LocaleDictionary (not exported by the package). */
interface LocaleDict {
  [key: string]: string | LocaleDict | (string | LocaleDict)[] | null;
}

function unflatten(input: Record<string, string>): LocaleDict {
  const out: LocaleDict = {};
  for (const [key, value] of Object.entries(input)) {
    const parts = key.split('.');
    let cur: LocaleDict = out;
    for (let i = 0; i < parts.length - 1; i++) {
      const p = parts[i];
      if (typeof cur[p] !== 'object' || cur[p] === null) cur[p] = {};
      cur = cur[p] as LocaleDict;
    }
    cur[parts[parts.length - 1]] = value;
  }
  return out;
}

function isPlainDict(value: unknown): value is LocaleDict {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

/**
 * Deep-merge page dictionaries over the shared dictionary. A shallow spread
 * would replace whole top-level sections (e.g. `components`), wiping
 * shared-only keys inside them and leaving raw i18n keys in the UI.
 */
function deepMerge(base: LocaleDict, override: LocaleDict): LocaleDict {
  const out: LocaleDict = { ...base };
  for (const [key, value] of Object.entries(override)) {
    const baseValue = out[key];
    if (isPlainDict(baseValue) && isPlainDict(value)) {
      out[key] = deepMerge(baseValue, value);
    } else {
      out[key] = value;
    }
  }
  return out;
}

const enDict = deepMerge(unflatten(sharedEn as Record<string, string>), en as unknown as LocaleDict);
const zhCNDict = deepMerge(unflatten(sharedZhCN as Record<string, string>), zhCN as unknown as LocaleDict);

// Debug: store the dicts so we can inspect them from playwright
if (typeof window !== 'undefined') {
  (window as unknown as { __i18n: { zhCN: unknown; en: unknown } }).__i18n = { zhCN: zhCNDict, en: enDict };
}

addMessages('en', enDict);
addMessages('zh-CN', zhCNDict);

// P0-LF-05: versioned browser-local storage key. Locale persistence never
// touches the server — there is no account-preference route, no session
// concept, and zero network traffic for locale reads/writes.
export const LOCALE_STORAGE_KEY = 'wptsall_locale.v1';

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
  const savedLocale = normalizeLocale(localStorage.getItem(LOCALE_STORAGE_KEY));
  if (savedLocale) return savedLocale;
  return normalizeLocale(getLocaleFromNavigator(), { lenient: true }) ?? 'en';
}

const initialLocale = detectInitialLocale();

init({
  fallbackLocale: 'en',
  initialLocale,
});

document.documentElement.lang = initialLocale;

/**
 * Re-apply the locally persisted locale. Browser-local only: reads the
 * versioned localStorage key, falls back to the navigator language, then to
 * English, and persists the outcome. Makes zero network calls.
 */
export function initializeI18n(): void {
  const savedLocale = detectInitialLocale();
  locale.set(savedLocale);
  document.documentElement.lang = savedLocale;
  localStorage.setItem(LOCALE_STORAGE_KEY, savedLocale);
}

export function setLocale(newLocale: string): void {
  const next = normalizeLocale(newLocale) ?? 'en';
  localStorage.setItem(LOCALE_STORAGE_KEY, next);
  document.documentElement.lang = next;
  locale.set(next);
}

export const availableLocales = ['en', 'zh-CN'] as const;
export type LocaleCode = typeof availableLocales[number];
