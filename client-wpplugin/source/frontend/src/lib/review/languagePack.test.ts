import { describe, expect, it } from 'vitest';
import type { ItemContentData } from '../api/items';
import { readLanguagePackReview } from './languagePack';

function pack(): ItemContentData {
  return {
    item: {
      id: 8, job_id: 1, domain: 'https://owned.invalid', relation_id: 7,
      object_type: 'language_pack', business_line: 'plugin_i18n',
      wp_object_id: 81, wp_object_subtype: 'plugin', task_type: 'text',
      source_lang: 'en', target_lang: 'zh', component_id: 'owned',
      raw_path: '/owned/raw', translated_path: '/owned/translated',
      status: 'pending_review', client_task_id: 'owned-pack',
      upload_id: null, wp_attachment_id: null, error_message: null,
      retry_count: 0, max_retries: 3, fetched_at: null, translated_at: 1,
      synced_at: null, created_at: 1, updated_at: 1,
    },
    raw: { relation_id: 7, business_line: 'plugin_i18n', entries: [
      { entry_id: 101, source: { object_id: 81, msgid: 'Owned %s', msgctxt: 'First', plural_index: 0 } },
      { entry_id: 102, source: { object_id: 82, msgid: 'Owned %s', msgctxt: 'Second', msgid_plural: 'Owned %s items', plural_index: 1 } },
    ] },
    translated: { payload: { relation_id: 7, business_line: 'plugin_i18n', entries: [
      { entry_id: 102, msgstr: 'Second %s' }, { entry_id: 101, msgstr: 'First %s' },
    ] } },
  };
}

describe('language-pack review identity', () => {
  it('joins shuffled translations by ID, not identical source text or array order', () => {
    const result = readLanguagePackReview(pack());
    expect(result.kind).toBe('pack');
    expect(result.entries.map((entry) => [entry.entryId, entry.msgstr, entry.context])).toEqual([
      [101, 'First %s', 'First'], [102, 'Second %s', 'Second'],
    ]);
    expect(result.entries[1].msgidPlural).toBe('Owned %s items');
    expect(result.entries[1].pluralIndex).toBe(1);
  });

  it.each([
    (data: any) => { data.raw.entries[1].entry_id = 101; },
    (data: any) => { data.translated.payload.entries[1].entry_id = 102; },
    (data: any) => { data.raw.entries.pop(); },
    (data: any) => { data.translated.payload.entries[0].entry_id = 999; },
    (data: any) => { data.raw.entries[0].entry_id = Number.MAX_SAFE_INTEGER + 1; },
    (data: any) => { data.raw.entries[0].source.object_id = -1; },
    (data: any) => { data.raw.entries[0].source.msgid = 7; },
    (data: any) => { data.raw.entries[0].source.msgctxt = {}; },
    (data: any) => { data.raw.entries[0].source.plural_index = 0x100000000; },
    (data: any) => { data.raw.business_line = 'widget_strings'; },
    (data: any) => { data.translated.payload.relation_id = 9; },
    (data: any) => { data.translated.payload.entries[0].msgstr = ''; },
  ])('retains malformed or conflicting pack scope without producing editable fields', (mutate) => {
    const data = pack();
    mutate(data);
    expect(readLanguagePackReview(data)).toEqual({ kind: 'invalid', entries: [] });
  });

  it('keeps config_i18n content-shaped fields on the ordinary review route', () => {
    const data = pack();
    data.item.object_type = 'post_type';
    data.item.business_line = 'config_i18n';
    data.raw = { post_title: 'Owned source', entries: ['raw object metadata'] };
    data.translated = { payload: { translated_fields: { post_title: 'Owned translation' } } };
    expect(readLanguagePackReview(data)).toEqual({ kind: 'not-pack', entries: [] });
  });
});
