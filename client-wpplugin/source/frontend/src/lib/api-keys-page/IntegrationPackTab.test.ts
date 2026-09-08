import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import IntegrationPackTab from './IntegrationPackTab.svelte';
import * as keysApi from '../api/keys';

vi.mock('../api/keys', () => ({
  exportIntegrationPack: vi.fn(),
  previewIntegrationPack: vi.fn(),
  importIntegrationPack: vi.fn(),
}));

describe('api-keys-page/IntegrationPackTab', () => {
  beforeEach(() => {
    vi.resetAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('exports a public redacted pack without a passphrase', async () => {
    vi.mocked(keysApi.exportIntegrationPack).mockResolvedValue({
      success: true,
      data: {
        export_mode: 'public',
        encrypted: false,
        pack: { schema: 'wptsall-integration-pack.v1', redacted: true },
      },
    } as never);

    render(IntegrationPackTab);
    await fireEvent.click(screen.getByText('导出 Pack'));

    await waitFor(() => {
      expect(keysApi.exportIntegrationPack).toHaveBeenCalledWith({ mode: 'public' });
    });
    expect(screen.getByText('Integration Pack 已导出')).toBeTruthy();
    expect((screen.getByDisplayValue(/wptsall-integration-pack/) as HTMLTextAreaElement).value).toContain('redacted');
  });

  it('requires confirmation and a passphrase for private export', async () => {
    vi.mocked(keysApi.exportIntegrationPack).mockResolvedValue({
      success: true,
      data: { export_mode: 'private_encrypted', encrypted: true, payload_base64: 'ciphertext' },
    } as never);

    render(IntegrationPackTab);
    await fireEvent.change(screen.getByRole('combobox'), { target: { value: 'private' } });
    await fireEvent.click(screen.getByText('导出 Pack'));
    expect(screen.getByText('请先确认私有备份提示')).toBeTruthy();

    const passwords = screen.getAllByPlaceholderText('至少 8 个字符');
    await fireEvent.input(passwords[0], { target: { value: 'portable-passphrase' } });
    await fireEvent.click(screen.getByText('我了解此备份包含凭据，必须安全保存。'));
    await fireEvent.click(screen.getByText('导出 Pack'));

    await waitFor(() => {
      expect(keysApi.exportIntegrationPack).toHaveBeenCalledWith({
        mode: 'private',
        confirm: true,
        passphrase: 'portable-passphrase',
      });
    });
    expect(screen.getByText('Integration Pack 已导出')).toBeTruthy();
  });

  it('previews and imports only a safe pack', async () => {
    vi.mocked(keysApi.previewIntegrationPack).mockResolvedValue({
      success: true,
      data: {
        safe_to_import: true,
        new_components: ['local-a'],
        overwrite_components: [],
        component_binding_conflicts: [],
        task_binding_conflicts: [],
        rule_binding_conflicts: [],
        blocked_provider_url_count: 0,
        missing_component_refs: [],
        workflow_valid: true,
      },
    } as never);
    vi.mocked(keysApi.importIntegrationPack).mockResolvedValue({
      success: true,
      data: { imported_components: ['local-a'] },
    } as never);

    render(IntegrationPackTab);
    await fireEvent.input(screen.getByPlaceholderText('请粘贴 Public 或加密 Integration Pack JSON'), {
      target: { value: '{"schema":"wptsall-integration-pack.v1"}' },
    });
    await fireEvent.click(screen.getByText('预览'));
    expect(await screen.findByText('可以安全导入')).toBeTruthy();
    await fireEvent.click(screen.getByText('导入 Pack'));

    await waitFor(() => {
      expect(keysApi.importIntegrationPack).toHaveBeenCalledWith(
        { schema: 'wptsall-integration-pack.v1' },
        false,
      );
    });
    expect(screen.getByText('Integration Pack 已导入')).toBeTruthy();
  });
});
