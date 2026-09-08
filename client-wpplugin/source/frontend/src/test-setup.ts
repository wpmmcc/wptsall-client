/**
 * Global test setup for vitest.
 * - Initialises svelte-i18n so every test file can use $t() without calling
 *   addMessages / init individually.
 * - Ensures localStorage is available in the jsdom environment (Sidebar and
 *   any component that imports i18n.ts at module-load time).
 */
import { addMessages, init } from 'svelte-i18n';
import en from '../../locales/en.json';
import zhCN from '../../locales/zh-CN.json';

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

addMessages('en', en);
addMessages('zh-CN', zhCN);
globalThis.localStorage.setItem('wptsall_locale.v1', 'zh-CN');
init({
  fallbackLocale: 'en',
  initialLocale: 'zh-CN',
});
