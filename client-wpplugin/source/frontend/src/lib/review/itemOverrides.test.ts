import { describe, expect, it } from 'vitest';
import {
  buildItemOverridePayload,
  EDITABLE_OVERRIDES_MUST_BE_OBJECT,
} from './itemOverrides';

describe('review/itemOverrides', () => {
  it('converts blank text inputs into explicit null clears', () => {
    expect(
      buildItemOverridePayload({
        componentId: '   ',
        sourceLang: '',
        targetLang: ' zh_CN ',
        editableOverridesJson: '{}',
      })
    ).toEqual({
      component_id: null,
      source_lang: null,
      target_lang: 'zh_CN',
      editable_overrides: {},
    });
  });

  it('rejects non-object editable overrides payloads', () => {
    expect(() =>
      buildItemOverridePayload({
        componentId: 'comp-a',
        sourceLang: 'en_US',
        targetLang: 'zh_CN',
        editableOverridesJson: '[]',
      })
    ).toThrow(EDITABLE_OVERRIDES_MUST_BE_OBJECT);
  });
});
