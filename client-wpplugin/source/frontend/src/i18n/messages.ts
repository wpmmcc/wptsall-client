/**
 * Pure locale-dictionary builders shared by the runtime (index.ts) and the
 * vitest global setup (test-setup.ts).
 *
 * The runtime dictionary for each locale is the page-level dictionary
 * deep-merged OVER the shared flat dictionary. Before this module existed,
 * test-setup.ts loaded only the shared dictionaries, so the merge path the
 * real app exercises was never under test and component tests asserted
 * shared-only values (see tasks/client2/27 §UI-27-04).
 */
import sharedEn from '../../../locales/en.json';
import sharedZhCN from '../../../locales/zh-CN.json';
import pageEn from './locales/en.json';
import pageZhCN from './locales/zh-CN.json';

/** Structural match for svelte-i18n's LocaleDictionary (not exported by the package). */
export interface LocaleDict {
  [key: string]: string | LocaleDict | (string | LocaleDict)[] | null;
}

export function unflatten(input: Record<string, string>): LocaleDict {
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
export function deepMerge(base: LocaleDict, override: LocaleDict): LocaleDict {
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

/** Runtime-identical merged dictionaries for both supported locales. */
export function buildLocaleMessages(): Record<'en' | 'zh-CN', LocaleDict> {
  const enDict = deepMerge(
    unflatten(sharedEn as Record<string, string>),
    pageEn as unknown as LocaleDict,
  );
  const zhCNDict = deepMerge(
    unflatten(sharedZhCN as Record<string, string>),
    pageZhCN as unknown as LocaleDict,
  );
  return { en: enDict, 'zh-CN': zhCNDict };
}
