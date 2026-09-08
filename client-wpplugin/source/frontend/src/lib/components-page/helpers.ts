import type {
  FileSizeClass,
  LocalComp,
  OpenAiCompatibleConfig,
  TemplateTranslationMode,
} from './types';

const CONTENT_FORMAT_LABELS: Record<string, string> = {
  plain_text: '纯文本',
  rich_html: 'HTML 富文本',
  json_structured: 'JSON 结构',
  serialized_php: 'PHP 序列化',
  media_ref: '媒体引用',
  slug: 'Slug',
  code: '代码片段',
};

const CONTENT_FORMAT_ORDER = [
  'plain_text',
  'rich_html',
  'json_structured',
  'serialized_php',
  'media_ref',
  'slug',
  'code',
];

const ARTIFACT_KIND_LABELS: Record<string, string> = {
  image_file: '图片文件',
  audio_file: '音频文件',
  video_file: '视频文件',
  video_asset_id: '视频资产 ID',
  document_file: '文档文件',
  subtitle_content: '字幕正文',
  i18n_bundle: '语言包 / 资源包',
  translated_image_file: '译后图片',
  translated_audio_file: '译后音频',
  dubbed_audio_file: '配音音频',
  translated_video_file: '译后视频',
  dubbed_video_file: '配音视频',
  translated_document_file: '译后文档',
  translated_subtitle_content: '译后字幕正文',
  translated_i18n_bundle: '译后语言包',
  translated_text_content: '提取后译文文本',
};

const BUSINESS_LINE_LABELS: Record<string, string> = {
  post_content: '文章内容',
  taxonomy_content: '分类术语内容',
  custom_model: '自定义内容模型',
  config_i18n: '配置国际化',
  plugin_i18n: '插件语言包',
  theme_i18n: '主题语言包',
};

const SOURCE_GROUP_LABELS: Record<string, string> = {
  content_object: '内容对象',
  config_object: '配置对象',
  message_template: '消息模板',
  plugin_i18n: '插件语言包',
  theme_i18n: '主题语言包',
};

const ROUTING_PROFILE_LABELS: Record<string, string> = {
  post_content_default: '文章内容默认路由',
  taxonomy_default: '分类术语默认路由',
  config_i18n: '配置国际化路由',
  notification_email: '通知邮件路由',
  notification_message: '消息通知路由',
  plugin_i18n_default: '插件语言包路由',
  theme_i18n_default: '主题语言包路由',
};

const DELIVERY_TARGET_LABELS: Record<string, string> = {
  object_writeback: '对象写回',
  option_writeback: '配置写回',
  message_template_writeback: '模板写回',
  i18n_bundle_writeback: '语言包写回',
};

const SOURCE_ROLE_LABELS: Record<string, string> = {
  title: '标题',
  excerpt: '摘要',
  body_html: '正文 HTML',
  body_text: '正文文本',
  message_subject: '消息主题',
  message_heading: '消息标题',
  message_body_html: '消息正文 HTML',
  message_body_text: '消息正文文本',
  placeholder_map: '占位符映射',
  config_label: '配置标签',
  config_help: '配置说明',
  config_value_text: '配置文本值',
  config_value_html: '配置 HTML 值',
  config_value_json: '配置 JSON 值',
  config_value_serialized: '配置序列化值',
  media_file: '媒体文件',
  media_alt: '媒体 alt 文本',
  media_caption: '媒体说明',
  media_title: '媒体标题',
  slug: 'Slug',
  code_fragment: '代码片段',
};

const PREFLIGHT_POLICY_LABELS: Record<string, string> = {
  block: '阻断启动',
  warn: '告警',
  auto_skip: '自动跳过',
};

const MISSING_COMPONENT_BEHAVIOR_LABELS: Record<string, string> = {
  stop_task: '停止任务',
  confirm_continue: '确认后继续',
  skip_field: '跳过当前字段',
};

const SEVERITY_LABELS: Record<string, string> = {
  blocking: '阻断',
  confirm: '需确认',
  auto_skip: '自动跳过',
};

function normalizedContentFormats(formats: string[] | undefined): string[] {
  if (!formats || formats.length === 0) return [];
  const normalized = Array.from(
    new Set(
      formats
        .filter((item): item is string => typeof item === 'string')
        .map((item) => item.trim())
        .filter(Boolean)
    )
  );
  return normalized.sort((a, b) => {
    const ai = CONTENT_FORMAT_ORDER.indexOf(a);
    const bi = CONTENT_FORMAT_ORDER.indexOf(b);
    if (ai < 0 && bi < 0) return a.localeCompare(b);
    if (ai < 0) return 1;
    if (bi < 0) return -1;
    return ai - bi;
  });
}

export function formatCapabilityLabel(format: string): string {
  return CONTENT_FORMAT_LABELS[format] ?? format;
}

export function formatArtifactKindLabel(kind: string): string {
  return ARTIFACT_KIND_LABELS[kind] ?? kind;
}

export function formatBusinessLineLabel(line: string): string {
  return BUSINESS_LINE_LABELS[line] ?? line;
}

export function formatSourceGroupLabel(group: string): string {
  return SOURCE_GROUP_LABELS[group] ?? group;
}

export function formatRoutingProfileLabel(profile: string): string {
  return ROUTING_PROFILE_LABELS[profile] ?? profile;
}

export function formatDeliveryTargetLabel(target: string): string {
  return DELIVERY_TARGET_LABELS[target] ?? target;
}

export function formatSourceRoleLabel(role: string): string {
  return SOURCE_ROLE_LABELS[role] ?? role;
}

export function formatPreflightPolicyLabel(policy: string): string {
  return PREFLIGHT_POLICY_LABELS[policy] ?? policy;
}

export function formatMissingComponentBehaviorLabel(behavior: string): string {
  return MISSING_COMPONENT_BEHAVIOR_LABELS[behavior] ?? behavior;
}

export function formatSeverityLabel(severity: string): string {
  return SEVERITY_LABELS[severity] ?? severity;
}

export function formatRuleSlotLabel(slot: string): string {
  if (slot === 'media_ref:image') return '图片媒体';
  if (slot === 'media_ref:video') return '视频媒体';
  if (slot === 'media_ref:audio') return '音频媒体';
  if (slot === 'media_ref:document') return '文档媒体';
  return CONTENT_FORMAT_LABELS[slot] ?? slot;
}

export function renderContentCapabilitySummary(
  formats: string[] | undefined,
  kind?: string
): string {
  const normalized = normalizedContentFormats(formats);
  if (normalized.length === 0) return '未声明';
  const labels = normalized.map(formatCapabilityLabel);
  if (normalized.length === 1 && normalized[0] === 'media_ref') {
    if (kind === 'image' || kind === 'image_translation') return '媒体引用 / 图片文件';
    if (kind === 'video' || kind === 'video_translation') return '媒体引用 / 视频文件';
    if (kind === 'audio' || kind === 'audio_translation') return '媒体引用 / 音频文件';
    if (kind === 'document' || kind === 'document_translation') return '媒体引用 / 文档文件';
  }
  return labels.join(' / ');
}

export function renderHtmlSupportSummary(formats: string[] | undefined): string {
  const normalized = normalizedContentFormats(formats);
  const hasText = normalized.some((fmt) => fmt !== 'media_ref');
  if (!hasText) return '非文本组件';
  if (normalized.includes('rich_html')) return '支持 HTML';
  return '仅非 HTML 文本';
}

export function deriveWpSourceHints(
  formats: string[] | undefined,
  kind?: string
): string[] {
  const normalized = normalizedContentFormats(formats);
  const hints: string[] = [];

  for (const format of normalized) {
    if (format === 'plain_text') {
      hints.push('标题/摘要/普通 meta 文本');
    } else if (format === 'rich_html') {
      hints.push('post_content/Classic HTML/Gutenberg 富文本');
    } else if (format === 'json_structured') {
      hints.push('JSON meta/区块属性/Builder 数据');
    } else if (format === 'serialized_php') {
      hints.push('PHP 序列化 meta/options/ACF 数据');
    } else if (format === 'media_ref') {
      if (kind === 'image' || kind === 'image_translation') {
        hints.push('附件 ID/URL -> 图片文件翻译');
        hints.push('图片 alt/caption/title 文本');
      } else if (kind === 'video' || kind === 'video_translation') {
        hints.push('附件 ID/URL -> 视频文件翻译');
        hints.push('视频 caption/描述 文本');
      } else if (kind === 'audio' || kind === 'audio_translation') {
        hints.push('附件 ID/URL -> 音频文件翻译');
        hints.push('音频 caption/描述 文本');
      } else if (kind === 'document' || kind === 'document_translation') {
        hints.push('附件 ID/URL -> 文档文件翻译');
        hints.push('文档标题/描述 文本');
      } else {
        hints.push('媒体附件引用');
      }
    } else if (format === 'slug') {
      hints.push('slug/URL 别名');
    } else if (format === 'code') {
      hints.push('模板代码/短码/配置片段');
    }
  }

  return Array.from(new Set(hints));
}

export function extractArtifactHints(templateJson: unknown): {
  input_artifact_kind?: string;
  output_artifact_kinds: string[];
} {
  const obj =
    templateJson && typeof templateJson === 'object'
      ? (templateJson as Record<string, unknown>)
      : null;
  const constraints =
    obj?.constraints && typeof obj.constraints === 'object'
      ? (obj.constraints as Record<string, unknown>)
      : null;
  const inputArtifactKind =
    typeof constraints?.input_artifact_kind === 'string' &&
    constraints.input_artifact_kind.trim()
      ? constraints.input_artifact_kind.trim()
      : undefined;
  const outputArtifactKinds = Array.isArray(constraints?.output_artifact_kinds)
    ? constraints.output_artifact_kinds
        .filter((item): item is string => typeof item === 'string')
        .map((item) => item.trim())
        .filter(Boolean)
    : [];
  return {
    input_artifact_kind: inputArtifactKind,
    output_artifact_kinds: outputArtifactKinds,
  };
}

export function renderArtifactSummary(
  inputArtifactKind?: string,
  outputArtifactKinds?: string[]
): string {
  const outputs = (outputArtifactKinds ?? []).filter(Boolean);
  if (!inputArtifactKind && outputs.length === 0) return '未声明';
  const inputLabel = inputArtifactKind
    ? formatArtifactKindLabel(inputArtifactKind)
    : '未声明输入';
  const outputLabel =
    outputs.length > 0
      ? outputs.map(formatArtifactKindLabel).join(' / ')
      : '未声明输出';
  return `${inputLabel} -> ${outputLabel}`;
}

export function safeDomId(raw: string): string {
  return raw.replace(/[^a-zA-Z0-9_-]+/g, '-');
}

export function componentModalFieldId(field: string): string {
  return `component-modal-${field}`;
}

export function versionModalFieldId(compId: string, field: string): string {
  return `version-${safeDomId(compId)}-${field}`;
}

export function authModalFieldId(compId: string, field: string): string {
  return `auth-${safeDomId(compId)}-${field}`;
}

export function testFileFieldId(field: string): string {
  return `test-file-${field}`;
}

export function ruleBindFieldId(field: string): string {
  return `rule-bind-${field}`;
}

export function handleBackdropKeydown(
  event: KeyboardEvent,
  close: () => void,
  allowSpace = true
) {
  if (
    event.key === 'Escape' ||
    event.key === 'Enter' ||
    (allowSpace && event.key === ' ')
  ) {
    event.preventDefault();
    close();
  }
}

export function parsePositiveInt(value: string): number | undefined {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const n = Number.parseInt(trimmed, 10);
  return Number.isNaN(n) || n < 0 ? undefined : n;
}

export function parsePositiveFloat(value: string): number | undefined {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  const n = Number.parseFloat(trimmed);
  return Number.isNaN(n) || n < 0 ? undefined : n;
}

export function deriveFileSizeClass(
  maxFileSizeMb: number | null | undefined
): FileSizeClass | null {
  if (maxFileSizeMb === null || maxFileSizeMb === undefined) return null;
  if (maxFileSizeMb <= 0) return 'UNLIMITED';
  if (maxFileSizeMb <= 10) return 'S';
  if (maxFileSizeMb <= 50) return 'M';
  if (maxFileSizeMb <= 200) return 'L';
  return 'XL';
}

export function fileSizeClassBadgeClass(sizeClass: FileSizeClass): string {
  if (sizeClass === 'S') return 'bg-emerald-50 text-emerald-700';
  if (sizeClass === 'M') return 'bg-blue-50 text-blue-700';
  if (sizeClass === 'L') return 'bg-amber-50 text-amber-700';
  if (sizeClass === 'XL') return 'bg-rose-50 text-rose-700';
  return 'bg-slate-100 text-slate-700';
}

export function fileSizeLabel(maxFileSizeMb: number | null | undefined): string {
  if (maxFileSizeMb === null || maxFileSizeMb === undefined) return '';
  if (maxFileSizeMb <= 0) return 'UNLIMITED';
  return `${maxFileSizeMb}MB`;
}

export function formatTsLabel(raw?: string | null): string {
  if (!raw) return '-';
  const n = Number(raw);
  if (!Number.isNaN(n) && raw.trim() !== '') {
    return new Date(n * 1000).toLocaleString();
  }
  const dt = new Date(raw);
  return Number.isNaN(dt.getTime()) ? raw : dt.toLocaleString();
}

export function extractTranslationModes(
  templateJson: unknown
): TemplateTranslationMode[] {
  const obj =
    templateJson && typeof templateJson === 'object'
      ? (templateJson as Record<string, unknown>)
      : null;
  const rawModes = obj?.translation_modes;
  if (!Array.isArray(rawModes)) return [];
  const modes: TemplateTranslationMode[] = [];
  for (const rawMode of rawModes) {
    if (!rawMode || typeof rawMode !== 'object') continue;
    const mode = rawMode as Record<string, unknown>;
    const id = typeof mode.id === 'string' ? mode.id.trim() : '';
    const label = typeof mode.label === 'string' ? mode.label.trim() : '';
    if (!id || !label) continue;
    const supportedContentFormats = Array.isArray(mode.supported_content_formats)
      ? mode.supported_content_formats
          .filter((item): item is string => typeof item === 'string')
          .map((item) => item.trim())
          .filter(Boolean)
      : [];
    const apiDocsUrl =
      typeof mode.api_docs_url === 'string' && mode.api_docs_url.trim()
        ? mode.api_docs_url.trim()
        : undefined;
    modes.push({
      id,
      label,
      supported_content_formats: supportedContentFormats,
      api_docs_url: apiDocsUrl,
    });
  }
  return modes;
}

export function extractAuthModes(
  templateJson: unknown,
  fallbackModes?: unknown
): string[] {
  const fromRaw = (raw: unknown): string[] => {
    if (!Array.isArray(raw)) return [];
    const out: string[] = [];
    for (const item of raw) {
      if (typeof item !== 'string') continue;
      const normalized = item.trim().toLowerCase();
      if (!['key', 'oauth', 'none'].includes(normalized)) continue;
      if (!out.includes(normalized)) out.push(normalized);
    }
    return out;
  };

  const explicitFallback = fromRaw(fallbackModes);
  const obj =
    templateJson && typeof templateJson === 'object'
      ? (templateJson as Record<string, unknown>)
      : null;
  const authObj =
    obj?.auth && typeof obj.auth === 'object'
      ? (obj.auth as Record<string, unknown>)
      : null;
  const modes = fromRaw(authObj?.modes);
  if (modes.length > 0) return modes;

  const mode =
    typeof authObj?.mode === 'string' ? authObj.mode.trim().toLowerCase() : '';
  if (['key', 'oauth', 'none'].includes(mode)) return [mode];

  if (explicitFallback.length > 0) return explicitFallback;
  return [];
}

export function extractOpenAiCompatibleConfig(
  templateJson: unknown
): OpenAiCompatibleConfig | null {
  const obj =
    templateJson && typeof templateJson === 'object'
      ? (templateJson as Record<string, unknown>)
      : null;
  const request =
    obj?.request && typeof obj.request === 'object'
      ? (obj.request as Record<string, unknown>)
      : null;
  const response =
    obj?.response && typeof obj.response === 'object'
      ? (obj.response as Record<string, unknown>)
      : null;
  const body =
    request?.body && typeof request.body === 'object'
      ? (request.body as Record<string, unknown>)
      : null;
  const rawUrl = typeof request?.url === 'string' ? request.url.trim() : '';
  const apiBase = rawUrl.endsWith('/v1/chat/completions')
    ? rawUrl.slice(0, -'/v1/chat/completions'.length)
    : rawUrl;
  const model = typeof body?.model === 'string' ? body.model.trim() : '';
  if (!apiBase || !model) return null;

  let systemPrompt = '';
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  for (const item of messages) {
    if (!item || typeof item !== 'object') continue;
    const message = item as Record<string, unknown>;
    const role = typeof message.role === 'string' ? message.role.trim() : '';
    const content = typeof message.content === 'string' ? message.content : '';
    if (role === 'system') {
      systemPrompt = content;
      break;
    }
  }

  const rawTemperature = body?.temperature;
  const temperature =
    typeof rawTemperature === 'number' && Number.isFinite(rawTemperature)
      ? String(rawTemperature)
      : '0.1';
  const rawMaxTokens = body?.max_tokens;
  const maxTokens =
    typeof rawMaxTokens === 'number' && Number.isFinite(rawMaxTokens)
      ? String(rawMaxTokens)
      : '';
  const responsePath =
    typeof response?.translated_text_path === 'string' && response.translated_text_path.trim()
      ? response.translated_text_path.trim()
      : 'choices.0.message.content';

  return {
    api_base: apiBase,
    model,
    system_prompt: systemPrompt,
    temperature,
    max_tokens: maxTokens,
    response_path: responsePath,
  };
}

export function isNonTextKind(kind: string): boolean {
  return kind !== '' && kind !== 'text' && kind !== 'generic_text' && kind !== 'unknown';
}

export function localCapability(comp: LocalComp): string {
  if (comp.capability && comp.capability.trim()) return comp.capability.trim();
  return comp.kind === 'openai_compatible' ? 'text' : comp.kind;
}

export function localCapabilityBadgeClass(capability: string): string {
  if (capability === 'image') return 'bg-sky-50 text-sky-700';
  if (capability === 'video') return 'bg-rose-50 text-rose-700';
  if (capability === 'audio') return 'bg-amber-50 text-amber-700';
  if (capability === 'document') return 'bg-emerald-50 text-emerald-700';
  return 'bg-slate-100 text-slate-700';
}
