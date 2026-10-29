<!-- The Krak mark: an ice shard with a play cut-out and a crack. Below 32 px the simplified
     mark (bigger play, no crack) stays readable. The cut-outs are real holes (mask). -->
<script lang="ts">
  let { size = 32, color = 'var(--face)' }: { size?: number; color?: string } = $props();
  const id = $props.id();
  const small = $derived(size < 32);
</script>

<svg viewBox="4 1 200 200" width={size} height={size} aria-hidden="true">
  <defs>
    <mask id={id} maskUnits="userSpaceOnUse" x="0" y="0" width="210" height="210">
      <rect width="210" height="210" fill="#fff" />
      {#if small}
        <polygon points="80,66 146,104 80,142" fill="#000" />
      {:else}
        <polygon points="84,70 142,104 84,138" fill="#000" />
        <polygon points="143.5,102.7 185.5,148.7 182.5,151.3 140.5,105.3" fill="#000" />
      {/if}
    </mask>
  </defs>
  <polygon mask="url(#{id})" points="78,4 158,44 184,150 108,198 24,136 36,52" fill={color} />
</svg>
