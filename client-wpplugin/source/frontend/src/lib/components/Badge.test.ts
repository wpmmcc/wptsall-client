// catalog: WEBUI-UI-Badge
// oracle: L2
// 状态矩阵：变体样式矩阵：默认 / success / error。
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import Badge from './Badge.svelte';

describe('components/Badge', () => {
  afterEach(() => {
    cleanup();
  });

  it('uses default variant styles', () => {
    const { container } = render(Badge);
    const el = container.querySelector('span');
    expect(el?.className).toContain('bg-gray-100');
    expect(el?.className).toContain('text-gray-600');
  });

  it('uses success variant styles', () => {
    const { container } = render(Badge, { variant: 'success' });
    const el = container.querySelector('span');
    expect(el?.className).toContain('bg-green-100');
    expect(el?.className).toContain('text-green-700');
  });

  it('uses error variant styles', () => {
    const { container } = render(Badge, { variant: 'error' });
    const el = container.querySelector('span');
    expect(el?.className).toContain('bg-red-100');
    expect(el?.className).toContain('text-red-700');
  });
});
