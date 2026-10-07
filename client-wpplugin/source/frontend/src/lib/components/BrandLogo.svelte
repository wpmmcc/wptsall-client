<script lang="ts">
  let { size = 32, className = '' }: { size?: number; className?: string } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 512 512"
  class="shrink-0 {className}"
  fill="none"
  xmlns="http://www.w3.org/2000/svg"
  aria-label="WPTSALL Brand Logo"
>
  <defs>
    <linearGradient id="logoBgGrad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#090D16" />
      <stop offset="45%" stop-color="#0F172A" />
      <stop offset="100%" stop-color="#1E1B4B" />
    </linearGradient>
    <linearGradient id="logoBorderGrad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#38BDF8" stop-opacity="0.8" />
      <stop offset="50%" stop-color="#818CF8" stop-opacity="0.5" />
      <stop offset="100%" stop-color="#C084FC" stop-opacity="0.85" />
    </linearGradient>
    <linearGradient id="logoCyanBlue" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#38BDF8" />
      <stop offset="50%" stop-color="#0284C7" />
      <stop offset="100%" stop-color="#2563EB" />
    </linearGradient>
    <linearGradient id="logoVioletPurple" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#A855F7" />
      <stop offset="50%" stop-color="#7C3AED" />
      <stop offset="100%" stop-color="#4F46E5" />
    </linearGradient>
    <radialGradient id="logoCoreGlow" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#FFFFFF" />
      <stop offset="40%" stop-color="#E0F2FE" />
      <stop offset="70%" stop-color="#38BDF8" stop-opacity="0.8" />
      <stop offset="100%" stop-color="#6366F1" stop-opacity="0.8" />
    </radialGradient>
  </defs>

  <!-- Squircle Base -->
  <rect x="36" y="36" width="440" height="440" rx="100" ry="100" fill="url(#logoBgGrad)" />
  <rect x="36" y="36" width="440" height="440" rx="100" ry="100" fill="none" stroke="url(#logoBorderGrad)" stroke-width="8" />

  <!-- Flow Paths -->
  <path d="M 330 200 C 375 200, 405 225, 405 265 C 405 310, 365 345, 310 345 C 240 345, 195 265, 155 265 C 125 265, 107 282, 107 302 C 107 325, 125 345, 160 345 L 175 345"
        fill="none" stroke="url(#logoVioletPurple)" stroke-width="32" stroke-linecap="round" stroke-linejoin="round" />
  <path d="M 182 312 C 137 312, 107 287, 107 247 C 107 202, 147 167, 202 167 C 272 167, 317 247, 357 247 C 387 247, 405 230, 405 210 C 405 187, 387 167, 352 167 L 337 167"
        fill="none" stroke="url(#logoCyanBlue)" stroke-width="32" stroke-linecap="round" stroke-linejoin="round" />

  <!-- Arrow Heads -->
  <path d="M 342 146 L 378 167 L 342 188 Z" fill="#38BDF8" />
  <path d="M 170 324 L 134 345 L 170 366 Z" fill="#A855F7" />

  <!-- Orbs & Core -->
  <circle cx="165" cy="240" r="22" fill="url(#logoCyanBlue)" />
  <circle cx="165" cy="240" r="11" fill="#FFFFFF" opacity="0.9" />
  <circle cx="347" cy="272" r="22" fill="url(#logoVioletPurple)" />
  <circle cx="347" cy="272" r="11" fill="#FFFFFF" opacity="0.9" />

  <circle cx="256" cy="256" r="36" fill="url(#logoCoreGlow)" />
  <circle cx="256" cy="256" r="14" fill="#FFFFFF" />
  <path d="M 256 228 L 270 256 L 256 284 L 242 256 Z" fill="#38BDF8" opacity="0.85" />
</svg>
