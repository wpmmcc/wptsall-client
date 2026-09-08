export interface VendorKeyItem {
  id: string;
  vendor_id: string;
  label: string;
  auth_keys: string[];
  max_concurrent: number;
  requests_per_second: number;
  weight: number;
  enabled: boolean;
  max_input_chars?: number;
  max_file_size_mb?: number;
}

export interface OAuthItem {
  id: string;
  vendor_id: string;
  label: string;
  grant_type: string;
  auth_url?: string;
  token_url?: string;
  client_id: string;
  scopes: string;
  auth_extra_params?: Record<string, string>;
  extra_params?: Record<string, string>;
  max_concurrent?: number;
  weight?: number;
  max_input_chars?: number;
  max_file_size_mb?: number;
  token_field?: string;
  has_token: boolean;
  has_refresh_token?: boolean;
  token_expires_at?: number;
}

export interface KeyFormState {
  id: string;
  vendor_id: string;
  label: string;
  auth_json: string;
  max_concurrent: string;
  requests_per_second: string;
  weight: string;
  enabled: boolean;
  max_input_chars: string;
  max_file_size_mb: string;
}

export interface ExtraParam {
  key: string;
  value: string;
}

export interface OAuthFormState {
  id: string;
  vendor_id: string;
  label: string;
  grant_type: string;
  auth_url: string;
  token_url: string;
  client_id: string;
  client_secret: string;
  scopes: string;
  auth_extra_params: ExtraParam[];
  max_concurrent: string;
  weight: string;
  max_input_chars: string;
  max_file_size_mb: string;
  token_field: string;
}

export interface AuthExtraPreset {
  label: string;
  params: Record<string, string>;
}
