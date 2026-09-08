<script lang="ts">
  import { _ } from 'svelte-i18n';
  import type {
    AuthExtraPreset,
    OAuthFormState,
    OAuthItem,
  } from '../types';

  export let open = false;
  export let editingOAuth: OAuthItem | null = null;
  export let oauthForm: OAuthFormState;
  export let authExtraPresets: AuthExtraPreset[] = [];
  export let callbackUrl = '';
  export let oauthModalFieldId: (field: string) => string;
  export let handleBackdropKeydown: (event: KeyboardEvent, close: () => void) => void;
  export let onApplyPreset: (preset: Record<string, string>) => void;
  export let onAddExtraParam: () => void;
  export let onRemoveExtraParam: (index: number) => void;
  export let onCopyCallbackUrl: () => void | Promise<void>;
  export let onSave: () => void | Promise<void>;
  export let onClose: () => void;
</script>

{#if open}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('oauth_modal.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    onkeydown={(e) => handleBackdropKeydown(e, onClose)}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-lg max-h-[90vh] overflow-y-auto">
      <h3 class="font-semibold text-gray-900 mb-4">
        {editingOAuth ? $_('oauth_modal.title_edit') : $_('oauth_modal.title_create')}
      </h3>
      <div class="space-y-3">
        {#if !editingOAuth}
          <input
            bind:value={oauthForm.id}
            placeholder={$_('oauth_modal.placeholder_config_id')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
        {/if}
        <input
          bind:value={oauthForm.vendor_id}
          placeholder={$_('oauth_modal.placeholder_vendor_id')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        <div>
          <label for={oauthModalFieldId('label')} class="block text-xs font-medium text-gray-700 mb-1">
            {$_('oauth_modal.label_name')}
          </label>
          <input
            id={oauthModalFieldId('label')}
            bind:value={oauthForm.label}
            placeholder={$_('oauth_modal.placeholder_name')}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        </div>

        <div>
          <label for={oauthModalFieldId('grant-type')} class="block text-xs text-gray-500 mb-1">
            {$_('oauth_modal.grant_type')}
          </label>
          <select
            id={oauthModalFieldId('grant-type')}
            bind:value={oauthForm.grant_type}
            class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm">
            <option value="client_credentials">{$_('oauth_modal.grant_client_credentials')}</option>
            <option value="authorization_code">{$_('oauth_modal.grant_authorization_code')}</option>
            <option value="jwt_bearer">{$_('oauth_modal.grant_jwt_bearer')}</option>
          </select>
        </div>

        {#if oauthForm.grant_type === 'authorization_code'}
          <div class="bg-blue-50 border border-blue-200 rounded-lg px-3 py-2.5">
            <p class="text-xs text-blue-700 font-medium mb-1">{$_('oauth_modal.callback_step1')}</p>
            <div class="flex items-center gap-2">
              <code class="text-xs font-mono text-blue-800 flex-1 break-all">{callbackUrl}</code>
              <button
                onclick={onCopyCallbackUrl}
                class="text-xs bg-blue-100 hover:bg-blue-200 text-blue-700 px-2 py-1 rounded shrink-0">
                {$_('oauth_modal.copy')}
              </button>
            </div>
          </div>

          <div>
            <label for={oauthModalFieldId('auth-url')} class="block text-xs text-gray-500 mb-1">
              {$_('oauth_modal.auth_url_required')}<span class="text-gray-400 ml-1">{$_('oauth_modal.auth_url_hint')}</span>
            </label>
            <input
              id={oauthModalFieldId('auth-url')}
              bind:value={oauthForm.auth_url}
              placeholder="https://example.com/oauth/authorize"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
          <div>
            <label for={oauthModalFieldId('token-url')} class="block text-xs text-gray-500 mb-1">
              {$_('oauth_modal.token_url_required')}<span class="text-gray-400 ml-1">{$_('oauth_modal.token_url_hint')}</span>
            </label>
            <input
              id={oauthModalFieldId('token-url')}
              bind:value={oauthForm.token_url}
              placeholder="https://example.com/oauth/token"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>

          <div class="border border-gray-200 rounded-lg p-3">
            <div class="flex items-center justify-between mb-1">
              <div>
                <p class="text-xs font-medium text-gray-700">{$_('oauth_modal.extra_params_title')}</p>
                <p class="text-xs text-gray-400">{$_('oauth_modal.extra_params_hint')}</p>
              </div>
              <button
                onclick={onAddExtraParam}
                class="text-xs text-blue-600 hover:text-blue-700 border border-blue-200 px-2 py-1 rounded">
                {$_('common.add')}
              </button>
            </div>
            <div class="flex flex-wrap gap-1.5 mb-2">
              {#each authExtraPresets as preset}
                <button
                  onclick={() => onApplyPreset(preset.params)}
                  class="text-xs px-2 py-0.5 rounded-full border border-gray-300 bg-gray-50 hover:bg-gray-100 text-gray-600">
                  {preset.label}
                </button>
              {/each}
            </div>
            {#if oauthForm.auth_extra_params.length === 0}
              <p class="text-xs text-gray-400 italic">{$_('oauth_modal.no_extra_params')}</p>
            {:else}
              <div class="space-y-1.5">
                {#each oauthForm.auth_extra_params as pair, i}
                  <div class="flex gap-1.5 items-center">
                    <input
                      bind:value={pair.key}
                      placeholder={$_('oauth_modal.param_name')}
                      class="flex-1 border border-gray-200 rounded px-2 py-1 text-xs font-mono" />
                    <span class="text-gray-400 text-xs">=</span>
                    <input
                      bind:value={pair.value}
                      placeholder={$_('oauth_modal.param_value')}
                      class="flex-1 border border-gray-200 rounded px-2 py-1 text-xs font-mono" />
                    <button
                      onclick={() => onRemoveExtraParam(i)}
                      class="text-red-400 hover:text-red-600 text-xs px-1"
                      title={$_('common.delete')}>
                      ✕
                    </button>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {:else}
          <div>
            <label for={oauthModalFieldId('token-url')} class="block text-xs text-gray-500 mb-1">
              {$_('oauth_modal.token_url_required')}
            </label>
            <input
              id={oauthModalFieldId('token-url')}
              bind:value={oauthForm.token_url}
              placeholder="https://example.com/oauth/token"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
          </div>
        {/if}

        <input
          bind:value={oauthForm.client_id}
          placeholder={$_('oauth_modal.placeholder_client_id')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        <input
          bind:value={oauthForm.client_secret}
          type="password"
          placeholder={$_('oauth_modal.placeholder_client_secret')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
        <input
          bind:value={oauthForm.scopes}
          placeholder={$_('oauth_modal.placeholder_scopes')}
          class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />

        <div class="border-t border-gray-100 pt-3">
          <p class="text-xs text-gray-500 mb-2 font-medium">{$_('oauth_modal.advanced')}</p>
          <div class="flex gap-2 mb-2">
            <div class="flex-1">
              <label for={oauthModalFieldId('max-concurrent')} class="block text-xs text-gray-500 mb-1">
                {$_('oauth_modal.max_concurrent')}
              </label>
              <input
                id={oauthModalFieldId('max-concurrent')}
                bind:value={oauthForm.max_concurrent}
                type="number"
                min="0"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.no_limit_hint')}</p>
            </div>
            <div class="flex-1">
              <label for={oauthModalFieldId('weight')} class="block text-xs text-gray-500 mb-1">
                {$_('oauth.th_weight')}
              </label>
              <input
                id={oauthModalFieldId('weight')}
                bind:value={oauthForm.weight}
                type="number"
                min="1"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.weight_hint')}</p>
            </div>
          </div>
          <div class="flex gap-2 mb-2">
            <div class="flex-1">
              <label for={oauthModalFieldId('max-input-chars')} class="block text-xs text-gray-500 mb-1">
                {$_('key_modal.max_input_chars')}
              </label>
              <input
                id={oauthModalFieldId('max-input-chars')}
                bind:value={oauthForm.max_input_chars}
                type="number"
                min="0"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.no_limit_hint')}</p>
            </div>
            <div class="flex-1">
              <label for={oauthModalFieldId('max-file-size-mb')} class="block text-xs text-gray-500 mb-1">
                {$_('key_modal.max_file_size')}
              </label>
              <input
                id={oauthModalFieldId('max-file-size-mb')}
                bind:value={oauthForm.max_file_size_mb}
                type="number"
                min="0"
                step="0.1"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.no_limit_hint')}</p>
            </div>
          </div>
          <div>
            <label for={oauthModalFieldId('token-field')} class="block text-xs text-gray-500 mb-1">
              {$_('oauth_modal.token_inject_field')}
            </label>
            <input
              id={oauthModalFieldId('token-field')}
              bind:value={oauthForm.token_field}
              placeholder="access_token"
              class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
            <p class="text-xs text-gray-400 mt-0.5">{$_('oauth_modal.token_inject_hint')}</p>
          </div>
        </div>

        {#if oauthForm.grant_type === 'authorization_code'}
          <p class="text-xs text-gray-400">
            {$_('oauth_modal.auth_code_save_hint')}
          </p>
        {/if}
      </div>
      <div class="flex gap-2 mt-5">
        <button
          onclick={onSave}
          class="flex-1 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">
          {$_('common.save')}
        </button>
        <button
          onclick={onClose}
          class="flex-1 py-2 border border-gray-200 text-gray-700 text-sm rounded-lg hover:bg-gray-50">
          {$_('common.cancel')}
        </button>
      </div>
    </div>
  </div>
{/if}
