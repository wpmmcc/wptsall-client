<script lang="ts">
  import { onMount } from 'svelte';
  import { apiFetch, hasData } from '../lib/api/client';
  import { showToast } from '../lib/stores/toast';
  import { _ } from 'svelte-i18n';

  interface Product {
    id: string;
    name: string;
    description: string;
    price_cents: number;
    currency: string;
    is_active: boolean;
  }

  interface Entitlement {
    product_id: string;
    product_name: string;
    tier: string;
    capabilities: Record<string, unknown>;
    granted_at: string | null;
    is_active: boolean;
  }

  let products = $state<Product[]>([]);
  let entitlements = $state<Entitlement[]>([]);
  let loading = $state(true);

  onMount(async () => {
    await Promise.all([loadProducts(), loadEntitlements()]);
    loading = false;
  });

  async function loadProducts() {
    const res = await apiFetch<Product[]>('/api/v1/platform/products');
    if (hasData(res)) {
      products = res.data ?? [];
    } else {
      showToast('error', $_('products.load_products_failed'));
    }
  }

  async function loadEntitlements() {
    const res = await apiFetch<Entitlement[]>('/api/v1/platform/entitlements');
    if (hasData(res)) {
      entitlements = res.data ?? [];
    } else {
      showToast('error', $_('products.load_entitlements_failed'));
    }
  }

  function getEntitlementForProduct(productId: string): Entitlement | undefined {
    return entitlements.find(e => e.product_id === productId && e.is_active);
  }

  function formatPrice(cents: number, currency: string): string {
    return new Intl.NumberFormat('en-US', { style: 'currency', currency }).format(cents / 100);
  }

  function formatDate(iso: string): string {
    return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
  }
</script>

<!-- P0-LF-04: this page is legacy-only (Sidebar renders its entry solely under
     runtime_mode === 'legacy_server_control_plane') and must be visibly titled
     as the legacy server control plane, never as a default purchase surface. -->
<h2 class="text-lg font-semibold text-gray-900 mb-6">{$_('products.legacy_title')}</h2>

{#if loading}
  <div class="flex items-center justify-center py-20">
    <p class="text-sm text-gray-400">{$_('products.loading')}</p>
  </div>
{:else if products.length === 0}
  <div class="flex items-center justify-center py-20">
    <p class="text-sm text-gray-400">{$_('products.no_products')}</p>
  </div>
{:else}
  <div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-5">
    {#each products as product (product.id)}
      {@const ent = getEntitlementForProduct(product.id)}
      <div class="bg-white rounded-xl border border-gray-200 p-5 flex flex-col gap-3">
        <div class="flex items-start justify-between gap-2">
          <h3 class="text-base font-semibold text-gray-900">{product.name}</h3>
          {#if ent}
            <span class="shrink-0 inline-flex items-center rounded-full bg-green-50 px-2.5 py-0.5 text-xs font-medium text-green-700">
              {$_('products.active')} · {ent.tier}
            </span>
          {:else}
            <span class="shrink-0 inline-flex items-center rounded-full bg-gray-100 px-2.5 py-0.5 text-xs font-medium text-gray-500">
              {$_('products.not_subscribed')}
            </span>
          {/if}
        </div>

        <p class="text-sm text-gray-500 leading-relaxed">{product.description}</p>

        <div class="mt-auto pt-2 flex items-center justify-between text-sm">
          <span class="font-medium text-gray-900">{formatPrice(product.price_cents, product.currency)}</span>
          {#if ent?.granted_at}
            <span class="text-xs text-gray-400">{$_('products.granted_at', { values: { date: formatDate(ent.granted_at) } })}</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}
