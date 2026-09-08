export type RuleBindingScope = 'global' | 'plugin' | 'relation' | 'rule';

export function getRuleScopePlaceholder(scope: RuleBindingScope): string {
  if (scope === 'plugin') return '插件 slug，例如 woocommerce';
  if (scope === 'relation') return '站点关系 ID，例如 12';
  if (scope === 'rule') return '规则 ID，例如 128';
  return 'global 作用域无需填写';
}

export function normalizeRuleScopeKey(
  scope: RuleBindingScope,
  scopeKey: string
): string | undefined {
  if (scope === 'global') return undefined;
  const trimmed = scopeKey.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

export function validateRuleScopeKey(
  scope: RuleBindingScope,
  scopeKey: string
): string | null {
  const trimmed = scopeKey.trim();
  if (scope !== 'global' && !trimmed) {
    return '当前作用域必须填写 scope_key';
  }
  if ((scope === 'relation' || scope === 'rule') && !/^[1-9]\d*$/.test(trimmed)) {
    return 'relation/rule 作用域的 scope_key 必须是正整数 ID';
  }
  return null;
}

export function slotSupportsKind(slotKey: string, kind: string): boolean {
  const rawKind = (kind || '').toLowerCase();
  const normalizedKind = (() => {
    if (['text_translation', 'field', 'fields'].includes(rawKind)) return 'text';
    if (['image_translation', 'images'].includes(rawKind)) return 'image';
    if (['video_translation', 'videos'].includes(rawKind)) return 'video';
    if (['audio_translation', 'audios'].includes(rawKind)) return 'audio';
    if (['document_translation', 'documents', 'file', 'files', 'doc'].includes(rawKind)) return 'document';
    if (['openai-compatible', 'openai', 'llm'].includes(rawKind)) return 'openai_compatible';
    return rawKind;
  })();
  if (normalizedKind === 'mixed') return true;
  if (slotKey.startsWith('media_ref:')) {
    const mediaType = slotKey.split(':')[1] || '';
    return mediaType === normalizedKind;
  }
  if (slotKey === 'media_ref') {
    return ['image', 'video', 'audio', 'document'].includes(normalizedKind);
  }
  return normalizedKind === 'text' || normalizedKind === 'openai_compatible';
}
