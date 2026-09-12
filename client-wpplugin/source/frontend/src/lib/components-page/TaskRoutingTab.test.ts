// catalog: WEBUI-UI-TaskRoutingTab
// oracle: L2
// 状态矩阵：槽位推荐过滤 + 规则绑定保存 / scope_key 校验 / 删除绑定 + 刷新 / config·message 规则指引 + 快捷选择 / 规则语义发现 + 快填。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import TaskRoutingTab from './TaskRoutingTab.svelte';
import * as componentsApi from '../api/components';
import * as toastModule from '../stores/toast';

const hoisted = vi.hoisted(() => ({
  statusStore: (() => {
    let value: any = { components: [], rule_component_bindings: [] };
    const subscribers = new Set<(next: any) => void>();
    return {
      subscribe(run: (next: any) => void) {
        subscribers.add(run);
        run(value);
        return () => subscribers.delete(run);
      },
      set(next: any) {
        value = next;
        subscribers.forEach((run) => run(value));
      },
    };
  })(),
  fetchStatusMock: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('../api/components', () => ({
  deleteRuleBinding: vi.fn(),
  getRuleBindingDiscovery: vi.fn(),
  listAllLocalComponents: vi.fn(),
  upsertRuleBinding: vi.fn(),
}));

vi.mock('../stores/status', () => ({
  status: hoisted.statusStore,
  fetchStatus: hoisted.fetchStatusMock,
}));

vi.mock('../stores/toast', () => ({
  showToast: vi.fn(),
}));

describe('components-page/TaskRoutingTab', () => {
  beforeEach(() => {
    hoisted.statusStore.set({
      components: [
        { id: 'server-text', kind: 'text' },
        { id: 'server-image', kind: 'image' },
      ],
      rule_component_bindings: [],
    });
    vi.mocked(componentsApi.listAllLocalComponents).mockResolvedValue({
      success: true,
      data: {
        items: [
          { id: 'local-text', kind: 'text', name: 'Local Text', enabled: true },
          { id: 'local-image', kind: 'image', name: 'Local Image', enabled: true },
        ],
        total: 2,
      },
    } as never);
    vi.mocked(componentsApi.getRuleBindingDiscovery).mockResolvedValue({
      success: true,
      data: {
        summary: {
          domains_checked: 1,
          relations_checked: 1,
          rules_checked: 1,
          fields_checked: 2,
          issues: 0,
        },
        items: [
          {
            api_base_url: 'https://blog.wpmm.cc',
            relation_id: 12,
            source_lang: 'en_US',
            target_lang: 'zh_CN',
            plugin_slug: 'woocommerce',
            plugin_name: 'WooCommerce',
            rule_id: 128,
            rule_name: '邮件模板',
            data_type: 'post',
            object_name: 'shop_order',
            business_line: 'custom_model',
            source_group: 'message_template',
            routing_profile: 'notification_email',
            delivery_target: 'message_template_writeback',
            required_component_slots: ['plain_text', 'rich_html'],
            required_content_formats: ['plain_text', 'rich_html'],
            fields: [
              {
                field_name: 'subject',
                content_format: 'plain_text',
                source_role: 'message_subject',
                storage: 'post_title',
                required_slot_key: 'plain_text',
                suggested_task_type: 'text',
              },
              {
                field_name: 'body_html',
                content_format: 'rich_html',
                source_role: 'message_body_html',
                storage: 'post_content',
                required_slot_key: 'rich_html',
                suggested_task_type: 'text',
              },
            ],
          },
        ],
        issues: [],
      },
    } as never);
    vi.mocked(componentsApi.upsertRuleBinding).mockResolvedValue({
      success: true,
      data: {
        scope: 'global',
        slot_key: 'plain_text',
        component_id: 'local-text',
      },
    } as never);
    vi.mocked(componentsApi.deleteRuleBinding).mockResolvedValue({
      success: true,
      data: { deleted: true },
    } as never);
    hoisted.fetchStatusMock.mockClear();
    vi.mocked(toastModule.showToast).mockClear();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it('filters recommended components by slot kind and saves a rule binding', async () => {
    render(TaskRoutingTab);

    expect(await screen.findByText('按翻译规则槽位绑定组件')).toBeTruthy();
    expect(await screen.findByText('当前站点发现的规则语义')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'local-text' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: 'local-image' })).toBeNull();

    await fireEvent.change(screen.getByLabelText('槽位 (slot_key)'), {
      target: { value: 'media_ref:image' },
    });

    await waitFor(() => {
      expect(screen.getByRole('button', { name: 'local-image' })).toBeTruthy();
    });
    expect(screen.queryByRole('button', { name: 'local-text' })).toBeNull();

    await fireEvent.click(screen.getByRole('button', { name: 'local-image' }));
    await fireEvent.click(screen.getByText('保存绑定'));

    await waitFor(() => {
      expect(componentsApi.upsertRuleBinding).toHaveBeenCalledWith(
        'global',
        'media_ref:image',
        'local-image',
        undefined
      );
    });
    expect(hoisted.fetchStatusMock).toHaveBeenCalled();
    expect(toastModule.showToast).toHaveBeenCalledWith('success', '规则绑定已保存');
  });

  it('validates scope_key for relation scoped bindings', async () => {
    render(TaskRoutingTab);

    expect(await screen.findByText('按翻译规则槽位绑定组件')).toBeTruthy();

    await fireEvent.change(screen.getByLabelText('作用域'), {
      target: { value: 'relation' },
    });
    await fireEvent.click(screen.getByRole('button', { name: 'local-text' }));
    await fireEvent.click(screen.getByText('保存绑定'));

    expect(toastModule.showToast).toHaveBeenLastCalledWith(
      'error',
      '当前作用域必须填写 scope_key'
    );
    expect(componentsApi.upsertRuleBinding).not.toHaveBeenCalled();

    vi.mocked(toastModule.showToast).mockClear();
    await fireEvent.input(screen.getByLabelText('scope_key'), {
      target: { value: 'abc' },
    });
    await fireEvent.click(screen.getByText('保存绑定'));

    expect(toastModule.showToast).toHaveBeenLastCalledWith(
      'error',
      'relation/rule 作用域的 scope_key 必须是正整数 ID'
    );
    expect(componentsApi.upsertRuleBinding).not.toHaveBeenCalled();
  });

  it('deletes existing rule bindings and refreshes status', async () => {
    hoisted.statusStore.set({
      components: [{ id: 'server-text', kind: 'text' }],
      rule_component_bindings: [
        {
          scope: 'plugin',
          scope_key: 'woocommerce',
          slot_key: 'plain_text',
          component_id: 'local-text',
        },
      ],
    });

    render(TaskRoutingTab);

    expect(await screen.findByText('woocommerce')).toBeTruthy();
    await fireEvent.click(screen.getByText('删除'));

    await waitFor(() => {
      expect(componentsApi.deleteRuleBinding).toHaveBeenCalledWith(
        'plugin',
        'plain_text',
        'woocommerce'
      );
    });
    expect(hoisted.fetchStatusMock).toHaveBeenCalled();
    expect(toastModule.showToast).toHaveBeenCalledWith('success', '规则绑定已删除');
  });

  it('shows guidance for config and message rules and supports quick slot picks', async () => {
    render(TaskRoutingTab);

    expect(await screen.findByText('`config_object` 绑定建议')).toBeTruthy();
    expect(screen.getByText('`message_template` 绑定建议')).toBeTruthy();
    expect(screen.getByText(/走 `config_i18n` 语义 lane/)).toBeTruthy();
    expect(screen.getByText(/subject \/ heading → plain_text/)).toBeTruthy();

    const slotSelect = screen.getByLabelText('槽位 (slot_key)') as HTMLSelectElement;
    expect(slotSelect.value).toBe('plain_text');

    await fireEvent.click(screen.getByRole('button', { name: 'HTML · rich_html' }));
    expect(slotSelect.value).toBe('rich_html');
    expect(screen.getAllByText('HTML 富文本').length).toBeGreaterThan(0);

    await fireEvent.change(screen.getByLabelText('作用域'), {
      target: { value: 'rule' },
    });
    expect(screen.getByText('rule 作用域填规则 ID，例如 128')).toBeTruthy();
  });

  it('renders discovered rule semantics and can quick-fill rule scope plus slot', async () => {
    render(TaskRoutingTab);

    expect(await screen.findByText('邮件模板 · relation 12')).toBeTruthy();
    expect(screen.getByText('消息模板')).toBeTruthy();
    expect(screen.getByText('通知邮件路由')).toBeTruthy();
    expect(screen.getByText('模板写回')).toBeTruthy();
    expect(screen.getByText('subject')).toBeTruthy();
    expect(screen.getByText('body_html')).toBeTruthy();

    await fireEvent.click(screen.getAllByRole('button', { name: '用此字段填充' })[0]);

    expect((screen.getByLabelText('作用域') as HTMLSelectElement).value).toBe('rule');
    expect((screen.getByLabelText('scope_key') as HTMLInputElement).value).toBe('128');
    expect((screen.getByLabelText('槽位 (slot_key)') as HTMLSelectElement).value).toBe(
      'plain_text'
    );
  });
});
