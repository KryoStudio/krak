<!-- The Kryo Shard ice shard. `small` is the simplified mark for 32 px and below. -->
<script lang="ts">
  let { size = 32, small = false }: { size?: number; small?: boolean } = $props();
  const id = $props.id();
</script>

<svg viewBox="4 1 200 200" width={size} height={size} aria-hidden="true">
  <defs>
    <mask id={id} maskUnits="userSpaceOnUse" x="0" y="0" width="210" height="210">
      <rect width="210" height="210" fill="#fff" />
      {#if small}
        <polygon points="76,62 150,104 76,146" fill="#000" />
      {:else}
        <polygon points="84,70 142,104 84,138" fill="#000" />
        <polygon points="142,104 199.2,153.2 186,165.4" fill="#000" />
        <polygon points="84,70 29.2,39.8 22.8,56.6" fill="#000" />
      {/if}
    </mask>
  </defs>
  {#if small}
    <polygon mask="url(#{id})" points="78,4 158,44 184,150 108,198 24,136 36,52" fill="var(--face)" />
  {:else}
    <g mask="url(#{id})">
      <polygon points="36,52 78,4 158,44 184,150 142,104 84,70" fill="var(--glace)" />
      <polygon points="36,52 84,70 84,138 142,104 184,150 108,198 24,136" fill="var(--face)" />
    </g>
  {/if}
</svg>
