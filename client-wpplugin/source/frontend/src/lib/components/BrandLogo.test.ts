// catalog: WEBUI-UI-BrandLogo
// oracle: L2
// 渲染契约：默认尺寸 / 自定义尺寸与类名 / a11y 标签。
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render } from '@testing-library/svelte';
import BrandLogo from './BrandLogo.svelte';

describe('components/BrandLogo', () => {
  afterEach(() => {
    cleanup();
  });

  it('renders the brand svg with the default size and label', () => {
    const { container } = render(BrandLogo);
    const svg = container.querySelector('svg');
    expect(svg?.getAttribute('width')).toBe('32');
    expect(svg?.getAttribute('height')).toBe('32');
    expect(svg?.getAttribute('aria-label')).toBe('WPTSALL Brand Logo');
  });

  it('applies custom size and className', () => {
    const { container } = render(BrandLogo, { size: 64, className: 'site-logo' });
    const svg = container.querySelector('svg');
    expect(svg?.getAttribute('width')).toBe('64');
    expect(svg?.getAttribute('class')).toContain('shrink-0');
    expect(svg?.getAttribute('class')).toContain('site-logo');
  });
});
