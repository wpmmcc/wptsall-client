/**
 * Global test setup for vitest.
 * - Initialises svelte-i18n so every test file can use $t() without calling
 *   addMessages / init individually.
 * - EN loads the RUNTIME-IDENTICAL merged dictionary (shared flat dict +
 *   page dict deep-merge via i18n/messages.ts — the exact code path the real
 *   app uses), so EN render tests assert what English users actually see
 *   (tasks/client2/27 §UI-27-04).
 * - 批 P (zh 双轨收敛): the flat dict is now the 24-key Rust domain
 *   (CLI/OAuth/tray only), so the shared-flat-only zh registration is
 *   retired — zh-CN registers the merged dictionary like EN. The historical
 *   divergence debt ("page-zh diverges from shared-zh on ~476 keys") is
 *   resolved: page-zh is the single UI source of truth, and the shared-zh
 *   values on those keys were dead in production (page always won the
 *   deep-merge at runtime).
 * - Default locale is zh-CN (existing suites assert Chinese strings). EN
 *   smoke tests flip the locale per-test via the svelte-i18n store; set
 *   VITEST_LOCALE=en to run a whole suite in English.
 * - Ensures localStorage is available in the jsdom environment (Sidebar and
 *   any component that imports i18n.ts at module-load time).
 */
import { addMessages, init } from 'svelte-i18n';
import { buildLocaleMessages } from './i18n/messages';

// Provide a minimal localStorage stub in case jsdom's impl is missing
// (observed when a component module calls localStorage.getItem() at the
// top-level during module evaluation before the jsdom environment is ready).
if (typeof globalThis.localStorage === 'undefined' || typeof globalThis.localStorage.getItem !== 'function') {
  const store: Record<string, string> = {};
  Object.defineProperty(globalThis, 'localStorage', {
    value: {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, value: string) => { store[key] = value; },
      removeItem: (key: string) => { delete store[key]; },
      clear: () => { Object.keys(store).forEach((k) => delete store[k]); },
      get length() { return Object.keys(store).length; },
      key: (i: number) => Object.keys(store)[i] ?? null,
    },
    writable: false,
  });
}

const messages = buildLocaleMessages();
addMessages('en', messages.en);
addMessages('zh-CN', messages['zh-CN']);

const envLocale = (globalThis as Record<string, any>).process?.env?.VITEST_LOCALE;
const testLocale = envLocale === 'en' ? 'en' : 'zh-CN';
globalThis.localStorage.setItem('wptsall_locale.v1', testLocale);
init({
  fallbackLocale: 'en',
  initialLocale: testLocale,
});
