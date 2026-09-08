<script lang="ts">
  import type {
    AuthStrategy,
    LocalComp,
    OAuthItem,
    VendorKeyItem,
  } from '../types';
  import { _ } from 'svelte-i18n';

  export let open = false;
  export let component: LocalComp | null = null;
  export let authModalSupportedModes: string[] = [];
  export let authFields: { key: string; value: string }[] = [];
  export let editableParamPaths: string[] = [];
  export let overrideRequestUrl = '';
  export let overrideRequestHeadersJson = '';
  export let overrideRequestBodyJson = '';
  export let overrideMaxInputChars = '';
  export let overrideRateLimitQps = '';
  export let overrideMaxConcurrentRequests = '';
  export let overrideMaxFileSizeMb = '';
  export let authPoolKeyIds: string[] = [];
  export let authPoolOAuthIds: string[] = [];
  export let authPoolStrategy: AuthStrategy = 'RoundRobin';
  export let authPoolExpanded = false;
  export let availableKeys: VendorKeyItem[] = [];
  export let availableOAuthConfigs: OAuthItem[] = [];
  export let poolResourcesLoading = false;
  export let keyDropdownOpen = false;
  export let oauthDropdownOpen = false;
  export let authModeEnabled: (mode: 'key' | 'oauth' | 'none') => boolean;
  export let toggleKeyId: (id: string) => void;
  export let toggleOAuthId: (id: string) => void;
  export let isEditableParam: (path: string) => boolean;
  export let authModalFieldId: (compId: string, field: string) => string;
  export let handleBackdropKeydown: (
    event: KeyboardEvent,
    close: () => void,
    allowSpace?: boolean
  ) => void;
  export let onLoadPoolResources: () => void | Promise<void>;
  export let onSave: () => void | Promise<void>;
  export let onClear: () => void | Promise<void>;
  export let onClose: () => void;
</script>

{#if open && component}
  <div
    class="fixed inset-0 bg-black/40 z-50 flex items-center justify-center p-4"
    role="button"
    tabindex="0"
    aria-label={$_('auth_modal.close_modal')}
    onclick={(e) => {
      if (e.target === e.currentTarget) onClose();
    }}
    onkeydown={(e) => handleBackdropKeydown(e, onClose)}>
    <div class="bg-white rounded-xl shadow-xl p-6 w-full max-w-lg max-h-[90vh] flex flex-col overflow-y-auto">
      <h4 class="font-semibold text-gray-900 mb-4">{$_('auth_modal.title')} — {component.name}</h4>

      <div class="space-y-2 mb-3">
        {#each authFields as field, i}
          <div class="flex gap-2 items-center">
            <input
              bind:value={field.key}
              placeholder={$_('auth_modal.field_name')}
              class="flex-1 border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
            <input
              bind:value={field.value}
              placeholder={$_('auth_modal.field_value')}
              class="flex-1 border border-gray-200 rounded-lg px-3 py-2 text-sm" />
            <button
              onclick={() => (authFields = authFields.filter((_, idx) => idx !== i))}
              class="text-red-400 hover:text-red-600 text-sm px-1">
              ×
            </button>
          </div>
        {/each}
      </div>
      <button
        onclick={() => (authFields = [...authFields, { key: '', value: '' }])}
        class="text-xs text-blue-600 hover:text-blue-700 mb-4 self-start">
        {$_('auth_modal.add_field')}
      </button>

      <div class="border border-gray-200 rounded-lg overflow-hidden mb-4">
        <button
          class="w-full flex items-center justify-between px-4 py-2.5 bg-gray-50 text-sm font-medium text-gray-700 hover:bg-gray-100 transition-colors"
          onclick={() => {
            authPoolExpanded = !authPoolExpanded;
            if (
              authPoolExpanded &&
              availableKeys.length === 0 &&
              availableOAuthConfigs.length === 0
            )
              onLoadPoolResources();
          }}>
          <span>{$_('auth_modal.pool_config')}</span>
          <div class="flex items-center gap-2">
            {#if authPoolKeyIds.length > 0}
              <span class="text-xs px-1.5 py-0.5 bg-blue-100 text-blue-700 rounded font-mono">
                Key×{authPoolKeyIds.length}
              </span>
            {/if}
            {#if authPoolOAuthIds.length > 0}
              <span class="text-xs px-1.5 py-0.5 bg-purple-100 text-purple-700 rounded font-mono">
                OAuth×{authPoolOAuthIds.length}
              </span>
            {/if}
            <span class="text-gray-400 text-xs">{authPoolExpanded ? '▲' : '▼'}</span>
          </div>
        </button>

        {#if authPoolExpanded}
          <div class="px-4 py-3 space-y-4">
            {#if component.vendor_id?.trim()}
              <div class="text-[11px] text-gray-500">
                {$_('auth_modal.current_vendor')}: <span class="font-mono text-gray-700">{component.vendor_id}</span>
              </div>
            {:else}
              <div class="text-[11px] text-amber-600">
                {$_('auth_modal.no_vendor_warning')}
              </div>
            {/if}
            <div class="flex items-center gap-2 text-xs text-gray-500">
              <span>{$_('auth_modal.template_auth_modes')}:</span>
              {#if authModalSupportedModes.length === 0}
                <span class="px-1.5 py-0.5 rounded bg-gray-100 text-gray-600">key (default)</span>
              {:else}
                {#each authModalSupportedModes as mode}
                  <span
                    class="px-1.5 py-0.5 rounded {mode === 'key'
                      ? 'bg-blue-100 text-blue-700'
                      : mode === 'oauth'
                        ? 'bg-purple-100 text-purple-700'
                        : 'bg-emerald-100 text-emerald-700'}">
                    {mode}
                  </span>
                {/each}
              {/if}
            </div>
            {#if poolResourcesLoading}
              <p class="text-xs text-gray-400">{$_('common.loading')}</p>
            {/if}

            <div class:opacity-50={!authModeEnabled('key')}>
              <div class="text-xs font-medium text-gray-600 mb-2">{$_('auth_modal.api_key_pool')}</div>
              {#if !authModeEnabled('key')}
                <p class="text-xs text-amber-600 mb-2">{$_('auth_modal.key_not_supported')}</p>
              {/if}
              {#if authPoolKeyIds.length > 0}
                <div class="flex flex-wrap gap-1.5 mb-2">
                  {#each authPoolKeyIds as kid}
                    {@const keyObj = availableKeys.find((k) => k.id === kid)}
                    <span class="inline-flex items-center gap-1 text-xs px-2 py-1 bg-blue-50 text-blue-700 border border-blue-200 rounded-full">
                      {keyObj ? `${keyObj.label} (${kid})` : kid}
                      <button
                        class="text-blue-400 hover:text-blue-600 leading-none ml-0.5"
                        onclick={() => authModeEnabled('key') && toggleKeyId(kid)}>
                        ×
                      </button>
                    </span>
                  {/each}
                </div>
              {/if}
              <div class="relative">
                <button
                  class="w-full text-left border border-gray-200 rounded-lg px-3 py-2 text-sm text-gray-500 hover:border-gray-300 transition-colors flex items-center justify-between disabled:opacity-60 disabled:cursor-not-allowed"
                  disabled={!authModeEnabled('key')}
                  onclick={() => {
                    if (!authModeEnabled('key')) return;
                    keyDropdownOpen = !keyDropdownOpen;
                    oauthDropdownOpen = false;
                  }}>
                  <span>
                    {availableKeys.length === 0 && !poolResourcesLoading
                      ? $_('auth_modal.no_keys')
                      : $_('auth_modal.select_key')}
                  </span>
                  <span class="text-gray-400 text-xs">{keyDropdownOpen ? '▲' : '▼'}</span>
                </button>
                {#if keyDropdownOpen && availableKeys.length > 0}
                  <div class="absolute z-10 w-full mt-1 bg-white border border-gray-200 rounded-lg shadow-lg max-h-48 overflow-y-auto">
                    {#each availableKeys as k}
                      <button
                        class="w-full text-left px-3 py-2 text-sm hover:bg-gray-50 flex items-center gap-2 transition-colors"
                        onclick={() => authModeEnabled('key') && toggleKeyId(k.id)}>
                        <span
                          class="w-4 h-4 rounded border flex items-center justify-center shrink-0 {authPoolKeyIds.includes(
                            k.id
                          )
                            ? 'bg-blue-600 border-blue-600 text-white'
                            : 'border-gray-300'}">
                          {#if authPoolKeyIds.includes(k.id)}
                            <span class="text-xs leading-none">✓</span>
                          {/if}
                        </span>
                        <span class="flex-1 min-w-0">
                          <span class="font-medium text-gray-800">{k.label || k.id}</span>
                          <span class="text-gray-400 font-mono text-xs ml-1">({k.id})</span>
                        </span>
                        {#if k.vendor_id}
                          <span class="text-xs text-gray-400 shrink-0">{k.vendor_id}</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>

            <div class:opacity-50={!authModeEnabled('oauth')}>
              <div class="text-xs font-medium text-gray-600 mb-2">{$_('auth_modal.oauth_pool')}</div>
              {#if !authModeEnabled('oauth')}
                <p class="text-xs text-amber-600 mb-2">{$_('auth_modal.oauth_not_supported')}</p>
              {/if}
              {#if authPoolOAuthIds.length > 0}
                <div class="flex flex-wrap gap-1.5 mb-2">
                  {#each authPoolOAuthIds as oid}
                    {@const oauthObj = availableOAuthConfigs.find((o) => o.id === oid)}
                    <span class="inline-flex items-center gap-1 text-xs px-2 py-1 bg-purple-50 text-purple-700 border border-purple-200 rounded-full">
                      {oauthObj ? `${oauthObj.label} (${oid})` : oid}
                      <button
                        class="text-purple-400 hover:text-purple-600 leading-none ml-0.5"
                        onclick={() => authModeEnabled('oauth') && toggleOAuthId(oid)}>
                        ×
                      </button>
                    </span>
                  {/each}
                </div>
              {/if}
              <div class="relative">
                <button
                  class="w-full text-left border border-gray-200 rounded-lg px-3 py-2 text-sm text-gray-500 hover:border-gray-300 transition-colors flex items-center justify-between disabled:opacity-60 disabled:cursor-not-allowed"
                  disabled={!authModeEnabled('oauth')}
                  onclick={() => {
                    if (!authModeEnabled('oauth')) return;
                    oauthDropdownOpen = !oauthDropdownOpen;
                    keyDropdownOpen = false;
                  }}>
                  <span>
                    {availableOAuthConfigs.length === 0 && !poolResourcesLoading
                      ? $_('auth_modal.no_oauth')
                      : $_('auth_modal.select_oauth')}
                  </span>
                  <span class="text-gray-400 text-xs">{oauthDropdownOpen ? '▲' : '▼'}</span>
                </button>
                {#if oauthDropdownOpen && availableOAuthConfigs.length > 0}
                  <div class="absolute z-10 w-full mt-1 bg-white border border-gray-200 rounded-lg shadow-lg max-h-48 overflow-y-auto">
                    {#each availableOAuthConfigs as o}
                      <button
                        class="w-full text-left px-3 py-2 text-sm hover:bg-gray-50 flex items-center gap-2 transition-colors"
                        onclick={() => authModeEnabled('oauth') && toggleOAuthId(o.id)}>
                        <span
                          class="w-4 h-4 rounded border flex items-center justify-center shrink-0 {authPoolOAuthIds.includes(
                            o.id
                          )
                            ? 'bg-purple-600 border-purple-600 text-white'
                            : 'border-gray-300'}">
                          {#if authPoolOAuthIds.includes(o.id)}
                            <span class="text-xs leading-none">✓</span>
                          {/if}
                        </span>
                        <span class="flex-1 min-w-0">
                          <span class="font-medium text-gray-800">{o.label || o.id}</span>
                          <span class="text-gray-400 font-mono text-xs ml-1">({o.id})</span>
                        </span>
                        {#if o.vendor_id}
                          <span class="text-xs text-gray-400 shrink-0">{o.vendor_id}</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>

            <div>
              <div class="text-xs font-medium text-gray-600 mb-2">{$_('auth_modal.strategy')}</div>
              <div class="flex gap-3 flex-wrap">
                <label class="flex items-center gap-1.5 cursor-pointer text-sm text-gray-700">
                  <input type="radio" bind:group={authPoolStrategy} value="RoundRobin" class="accent-blue-600" />
                  {$_('auth_modal.round_robin')} (RoundRobin)
                </label>
                <label class="flex items-center gap-1.5 cursor-pointer text-sm text-gray-700">
                  <input type="radio" bind:group={authPoolStrategy} value="Random" class="accent-blue-600" />
                  {$_('auth_modal.random')} (Random)
                </label>
                <label class="flex items-center gap-1.5 cursor-pointer text-sm text-gray-700">
                  <input type="radio" bind:group={authPoolStrategy} value="Weighted" class="accent-blue-600" />
                  {$_('auth_modal.weighted')} (Weighted)
                </label>
              </div>
            </div>
          </div>
        {/if}
      </div>

      {#if editableParamPaths.length > 0}
        <div class="border border-gray-200 rounded-lg p-3 mb-4 space-y-3">
          <div class="text-sm font-medium text-gray-700">{$_('auth_modal.overridable_params')}</div>
          {#if isEditableParam('request.url')}
            <div>
              <label for={authModalFieldId(component.id, 'request-url')} class="block text-xs text-gray-500 mb-1">
                request.url
              </label>
              <input
                id={authModalFieldId(component.id, 'request-url')}
                bind:value={overrideRequestUrl}
                placeholder="https://api.vendor.com/translate"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono" />
            </div>
          {/if}
          {#if isEditableParam('request.headers')}
            <div>
              <label for={authModalFieldId(component.id, 'request-headers')} class="block text-xs text-gray-500 mb-1">
                request.headers (JSON)
              </label>
              <textarea
                id={authModalFieldId(component.id, 'request-headers')}
                bind:value={overrideRequestHeadersJson}
                rows="4"
                placeholder="Authorization header JSON"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono"></textarea>
            </div>
          {/if}
          {#if isEditableParam('request.body')}
            <div>
              <label for={authModalFieldId(component.id, 'request-body')} class="block text-xs text-gray-500 mb-1">
                request.body (JSON)
              </label>
              <textarea
                id={authModalFieldId(component.id, 'request-body')}
                bind:value={overrideRequestBodyJson}
                rows="5"
                placeholder="Request body JSON"
                class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm font-mono"></textarea>
            </div>
          {/if}
          <div class="grid grid-cols-2 gap-2">
            {#if isEditableParam('constraints.max_input_chars')}
              <div>
                <label for={authModalFieldId(component.id, 'max-input-chars')} class="block text-xs text-gray-500 mb-1">
                  constraints.max_input_chars
                </label>
                <input
                  id={authModalFieldId(component.id, 'max-input-chars')}
                  bind:value={overrideMaxInputChars}
                  type="number"
                  min="0"
                  class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              </div>
            {/if}
            {#if isEditableParam('constraints.rate_limit_qps')}
              <div>
                <label for={authModalFieldId(component.id, 'rate-limit-qps')} class="block text-xs text-gray-500 mb-1">
                  constraints.rate_limit_qps
                </label>
                <input
                  id={authModalFieldId(component.id, 'rate-limit-qps')}
                  bind:value={overrideRateLimitQps}
                  type="number"
                  min="0"
                  class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              </div>
            {/if}
            {#if isEditableParam('constraints.max_concurrent_requests')}
              <div>
                <label for={authModalFieldId(component.id, 'max-concurrent-requests')} class="block text-xs text-gray-500 mb-1">
                  constraints.max_concurrent_requests
                </label>
                <input
                  id={authModalFieldId(component.id, 'max-concurrent-requests')}
                  bind:value={overrideMaxConcurrentRequests}
                  type="number"
                  min="0"
                  class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              </div>
            {/if}
            {#if isEditableParam('constraints.max_file_size_mb')}
              <div>
                <label for={authModalFieldId(component.id, 'max-file-size-mb')} class="block text-xs text-gray-500 mb-1">
                  constraints.max_file_size_mb
                </label>
                <input
                  id={authModalFieldId(component.id, 'max-file-size-mb')}
                  bind:value={overrideMaxFileSizeMb}
                  type="number"
                  min="0"
                  step="0.1"
                  class="w-full border border-gray-200 rounded-lg px-3 py-2 text-sm" />
              </div>
            {/if}
          </div>
          <p class="text-xs text-gray-400">{$_('auth_modal.editable_params_hint')}</p>
        </div>
      {/if}

      <div class="flex gap-2 mt-2">
        <button
          onclick={onSave}
          class="flex-1 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700">
          {$_('common.save')}
        </button>
        <button
          onclick={onClear}
          class="py-2 px-4 border border-red-200 text-red-500 text-sm rounded-lg hover:bg-red-50">
          {$_('auth_modal.clear_binding')}
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
