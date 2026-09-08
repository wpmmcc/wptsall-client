/**
 * Shared API response types — identical to WebUI client.
 * This file can be symlinked from the shared location.
 */
export type ApiResult<T> =
  | { success: true; data: T }
  | { success: false; error: ApiError };

export interface ApiError {
  code: string;
  message: string;
  status?: number;
}

export interface UpdateCheckResult {
  current_version: string;
  latest_version: string;
  min_supported_version: string;
  update_available: boolean;
  mandatory: boolean;
  bump_kind: string;
  release_notes_url: string;
  download_url_template: string;
  signature_url_template: string;
  update_kind?: string;
  ui_current_version?: string;
  ui_latest_version?: string;
  ui_update_available?: boolean;
  ui_download_url_template?: string;
  ui_signature_url_template?: string;
  ui_release_notes_url?: string;
  ui_subdir?: string;
  product_id?: string;
}
