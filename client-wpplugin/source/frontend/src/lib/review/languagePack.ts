import type { ItemContentData } from '../api/items';

export interface LanguagePackReviewEntry {
  entryId: number;
  msgstr: string;
  savedMsgstr: string;
  objectId: number;
  textDomain: string;
  context: string;
  msgid: string;
  msgidPlural: string;
  pluralIndex: number;
}

type PackReview =
  | { kind: 'not-pack'; entries: [] }
  | { kind: 'invalid'; entries: [] }
  | { kind: 'pack'; entries: LanguagePackReviewEntry[] };

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function positiveId(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0;
}

function optionalText(value: unknown): value is string | undefined {
  return value === undefined || typeof value === 'string';
}

export function readLanguagePackReview(data: ItemContentData): PackReview {
  const raw = record(data.raw);
  const translated = record(data.translated);
  const payload = record(translated?.payload) ?? translated;
  const type = data.item.object_type.trim().toLowerCase();
  // *_i18n content objects also use ordinary field maps. Names alone must
  // not route those objects into a language-pack editor.
  if (type !== 'language_pack' && type !== 'site_string' && !payload?.entries) {
    return { kind: 'not-pack', entries: [] };
  }
  const invalid: PackReview = { kind: 'invalid', entries: [] };
  if (!raw || !payload || !Array.isArray(raw.entries) || !Array.isArray(payload.entries)
      || raw.entries.length === 0 || raw.entries.length !== payload.entries.length
      || raw.relation_id !== data.item.relation_id || payload.relation_id !== data.item.relation_id
      || raw.business_line !== data.item.business_line || payload.business_line !== data.item.business_line) {
    return invalid;
  }
  const translations = new Map<number, string>();
  for (const value of payload.entries) {
    const entry = record(value);
    if (!entry || !positiveId(entry.entry_id) || translations.has(entry.entry_id)
        || typeof entry.msgstr !== 'string' || !entry.msgstr.trim()) return invalid;
    translations.set(entry.entry_id, entry.msgstr);
  }
  const entries: LanguagePackReviewEntry[] = [];
  const ids = new Set<number>();
  for (const value of raw.entries) {
    const entry = record(value);
    const source = record(entry?.source);
    if (!entry || !source || !positiveId(entry.entry_id) || ids.has(entry.entry_id)
        || !positiveId(source.object_id) || !translations.has(entry.entry_id)
        || typeof source.msgid !== 'string'
        || !optionalText(source.msgid_plural) || !optionalText(source.msgctxt)
        || !optionalText(source.text_domain)) return invalid;
    const pluralIndex = source.plural_index ?? 0;
    if (typeof pluralIndex !== 'number' || !Number.isInteger(pluralIndex)
        || pluralIndex < 0 || pluralIndex > 0xffffffff
        || (!source.msgid.trim() && !(source.msgid_plural as string | undefined)?.trim())) return invalid;
    ids.add(entry.entry_id);
    const msgstr = translations.get(entry.entry_id)!;
    entries.push({
      entryId: entry.entry_id, msgstr, savedMsgstr: msgstr,
      objectId: source.object_id, textDomain: source.text_domain ?? '',
      context: source.msgctxt ?? '', msgid: source.msgid,
      msgidPlural: source.msgid_plural ?? '', pluralIndex,
    });
  }
  return { kind: 'pack', entries };
}
