import { addMessages, init, locale } from 'svelte-i18n';
import { buildLocaleMessages } from './messages';
import { LOCALE_STORAGE_KEY, normalizeLocale, detectInitialLocale } from './storage';

const { en: enDict, 'zh-CN': zhCNDict } = buildLocaleMessages();

// Debug: store the dicts so we can inspect them from playwright
if (typeof window !== 'undefined') {
  (window as unknown as { __i18n: { zhCN: unknown; en: unknown } }).__i18n = { zhCN: zhCNDict, en: enDict };
}

addMessages('en', enDict);
addMessages('zh-CN', zhCNDict);

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

// Re-export the storage helpers for existing importers of this module.
export { LOCALE_STORAGE_KEY, normalizeLocale, detectInitialLocale };
