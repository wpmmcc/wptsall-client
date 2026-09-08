import type { SaveItemOverridePayload } from '../api/items';

export const EDITABLE_OVERRIDES_MUST_BE_OBJECT = 'editable_overrides_must_be_object';

function nullableTrimmed(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function parseEditableOverridesJson(editableOverridesJson: string): Record<string, unknown> {
  const parsed = editableOverridesJson.trim() ? JSON.parse(editableOverridesJson) : {};
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(EDITABLE_OVERRIDES_MUST_BE_OBJECT);
  }
  return parsed as Record<string, unknown>;
}

export function buildItemOverridePayload(input: {
  componentId: string;
  sourceLang: string;
  targetLang: string;
  editableOverridesJson: string;
}): SaveItemOverridePayload {
  return {
    component_id: nullableTrimmed(input.componentId),
    source_lang: nullableTrimmed(input.sourceLang),
    target_lang: nullableTrimmed(input.targetLang),
    editable_overrides: parseEditableOverridesJson(input.editableOverridesJson),
  };
}
