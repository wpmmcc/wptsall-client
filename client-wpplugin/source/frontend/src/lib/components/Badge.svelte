<script lang="ts">
  let { variant = 'default', children }: { variant?: 'success'|'warning'|'error'|'default', children?: any } = $props();
</script>
<span class="inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium
  {variant === 'success' ? 'bg-green-100 text-green-700' :
   variant === 'warning' ? 'bg-yellow-100 text-yellow-700' :
   variant === 'error'   ? 'bg-red-100 text-red-700' :
                           'bg-gray-100 text-gray-600'}">
  {@render children?.()}
</span>
