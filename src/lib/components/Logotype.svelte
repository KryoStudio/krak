<!-- The cracked KRAK logotype: the name splits like ice, the bottom half shifted in Glacier. -->
<script lang="ts">
  let { width = 320, top = 'var(--face)', bottom = '#7fc6db' }: { width?: number; top?: string; bottom?: string } = $props();
  const id = $props.id();
</script>

<svg viewBox="0 0 640 220" {width} height={Math.round((width * 220) / 640)} role="img" aria-label="Krak">
  <defs>
    <clipPath id="{id}-top"><polygon points="0,0 640,0 640,112 520,98 430,120 330,90 240,116 130,94 0,108" /></clipPath>
    <clipPath id="{id}-bottom"><polygon points="0,108 130,94 240,116 330,90 430,120 520,98 640,112 640,220 0,220" /></clipPath>
  </defs>
  <g clip-path="url(#{id}-top)">
    <text x="320" y="180" text-anchor="middle" font-family="Space Grotesk Variable, sans-serif" font-weight="700" font-size="200" letter-spacing="-8" fill={top}>KRAK</text>
  </g>
  <g clip-path="url(#{id}-bottom)" transform="translate(7 6)">
    <text x="320" y="180" text-anchor="middle" font-family="Space Grotesk Variable, sans-serif" font-weight="700" font-size="200" letter-spacing="-8" fill={bottom}>KRAK</text>
  </g>
</svg>
