import { writable } from 'svelte/store';

export type ToastType = 'success' | 'error' | 'info';

export interface Toast {
  id: number;
  type: ToastType;
  message: string;
  detail?: string;
}

let nextId = 0;
export const toasts = writable<Toast[]>([]);

export function showToast(type: ToastType, message: string, detail?: string) {
  const id = nextId++;
  toasts.update(list => [...list, { id, type, message, detail }]);
  if (type !== 'error') {
    setTimeout(() => dismissToast(id), 3000);
  }
}

export function dismissToast(id: number) {
  toasts.update(list => list.filter(t => t.id !== id));
}
