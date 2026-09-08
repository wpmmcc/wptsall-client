export function handleBackdropKeydown(event: KeyboardEvent, close: () => void) {
  if (event.key === 'Escape' || event.key === 'Enter' || event.key === ' ') {
    event.preventDefault();
    close();
  }
}

export function keyModalFieldId(field: string): string {
  return `vendor-key-${field}`;
}

export function oauthModalFieldId(field: string): string {
  return `vendor-oauth-${field}`;
}

export function formatExpiry(ts: number): string {
  if (!ts) return '-';
  const diff = ts * 1000 - Date.now();
  if (diff < 0) return '已过期';
  const h = Math.floor(diff / 3600000);
  const m = Math.floor((diff % 3600000) / 60000);
  if (h > 24) return `${Math.floor(h / 24)}天后`;
  if (h > 0) return `${h}h${m}m 后`;
  return `${m}m 后`;
}
