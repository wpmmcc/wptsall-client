/**
 * Test helper: run a callback with the svelte-i18n locale switched to EN,
 * restoring the previous locale afterwards.
 *
 * The vitest global setup defaults to zh-CN (historical baseline); EN render
 * smoke tests use this helper so English rendering finally has CI coverage
 * (tasks/client2/27 §UI-27-04 — before this, no test ever rendered EN).
 */
import { get } from 'svelte/store';
import { locale } from 'svelte-i18n';

export async function withEnglishLocale<T>(body: () => T | Promise<T>): Promise<T> {
  const previous = get(locale) ?? 'zh-CN';
  locale.set('en');
  try {
    return await body();
  } finally {
    locale.set(previous as string);
  }
}
