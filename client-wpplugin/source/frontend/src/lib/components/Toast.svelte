<script lang="ts">
  import { toasts, dismissToast } from '../stores/toast';
  import { X, CheckCircle, AlertCircle, Info } from 'lucide-svelte';
</script>

<div class="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm w-full">
  {#each $toasts as toast (toast.id)}
    <div class="flex items-start gap-3 rounded-lg px-4 py-3 shadow-lg text-sm
      {toast.type === 'success' ? 'bg-green-50 border border-green-200 text-green-800' :
       toast.type === 'error'   ? 'bg-red-50 border border-red-200 text-red-800' :
                                   'bg-blue-50 border border-blue-200 text-blue-800'}">
      <span class="mt-0.5 shrink-0">
        {#if toast.type === 'success'}<CheckCircle size={16} />
        {:else if toast.type === 'error'}<AlertCircle size={16} />
        {:else}<Info size={16} />{/if}
      </span>
      <div class="flex-1 min-w-0">
        <div class="font-medium">{toast.message}</div>
        {#if toast.detail}<div class="text-xs mt-0.5 opacity-75 break-all">{toast.detail}</div>{/if}
      </div>
      <button onclick={() => dismissToast(toast.id)} class="shrink-0 opacity-60 hover:opacity-100">
        <X size={14} />
      </button>
    </div>
  {/each}
</div>
