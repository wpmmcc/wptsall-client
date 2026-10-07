/**
 * Svelte action: dialog semantics + Escape close for fixed-overlay modals.
 *
 * Background (tasks/client2/27 §UI-27-07): every modal in the WebUI was a
 * plain positioned <div> — no role="dialog", no aria-modal, and Escape only
 * worked on the rare backdrop that happened to hold focus. Attach this
 * action to the modal's root overlay element:
 *
 *   <div class="fixed inset-0 ..." use:modalA11y={{ onClose: closeModal }}>
 *
 * The action:
 *   - sets role="dialog" + aria-modal="true" (+ aria-label when provided);
 *   - focuses the dialog so keyboard users land inside it;
 *   - closes on Escape via a WINDOW-level listener (works regardless of
 *     focus) — with a mount stack so only the top-most dialog reacts when
 *     dialogs are nested.
 */
import type { Action } from 'svelte/action';

export interface ModalA11yOptions {
  /** Called when Escape is pressed (same semantics as backdrop click). */
  onClose: () => void;
  /** Optional accessible name for the dialog. */
  label?: string;
}

/** Mount stack: only the top-most dialog handles Escape. */
const stack: HTMLElement[] = [];

function isTop(node: HTMLElement): boolean {
  return stack[stack.length - 1] === node;
}

export const modalA11y: Action<HTMLElement, ModalA11yOptions> = (node, params) => {
  let onClose: (() => void) | null = params?.onClose ?? null;

  node.setAttribute('role', 'dialog');
  node.setAttribute('aria-modal', 'true');
  if (params?.label) node.setAttribute('aria-label', params.label);
  // Make the dialog focusable and land focus in it (keyboard a11y).
  node.setAttribute('tabindex', '-1');
  node.focus({ preventScroll: true });

  const handleKey = (event: KeyboardEvent) => {
    if (event.key !== 'Escape') return;
    // An inner handler (e.g. a backdrop's own keydown) already acted on this
    // Escape and called preventDefault() — respect it and stay single-close.
    if (event.defaultPrevented) return;
    if (!isTop(node)) return;
    event.preventDefault();
    onClose?.();
  };
  window.addEventListener('keydown', handleKey);
  stack.push(node);

  return {
    update(next: ModalA11yOptions) {
      onClose = next?.onClose ?? null;
      if (next?.label) node.setAttribute('aria-label', next.label);
    },
    destroy() {
      window.removeEventListener('keydown', handleKey);
      const idx = stack.indexOf(node);
      if (idx !== -1) stack.splice(idx, 1);
    },
  };
};
