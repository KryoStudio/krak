<!-- A dropdown that opens the app's own menu, so it looks like the rest of the interface. -->
<script lang="ts" generics="T">
  import Icon from './Icon.svelte';
  import { openMenu } from '$lib/ui.svelte';

  let {
    options,
    value,
    label,
    onchange,
    width = 240,
  }: { options: { value: T; label: string }[]; value: T; label: string; onchange: (value: T) => void; width?: number } = $props();

  let button: HTMLButtonElement;
  const current = $derived(options.find((o) => o.value === value)?.label ?? '');

  function open() {
    const r = button.getBoundingClientRect();
    openMenu(
      r.left,
      r.bottom + 6,
      options.map((o) => ({ label: o.label, icon: o.value === value ? 'check' : undefined, run: () => onchange(o.value) })),
    );
  }
</script>

<button bind:this={button} class="select" style:width="{width}px" onclick={open} aria-label={label} aria-haspopup="menu">
  <span>{current}</span>
  <Icon name="chevronDown" size={14} />
</button>

<style>
  .select {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: 32px;
    padding: 0 10px 0 12px;
    border-radius: 8px;
    font-size: 13px;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    color: var(--text);
    transition: box-shadow var(--fast);
  }
  .select:hover {
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.18);
  }
  span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .select :global(svg) {
    flex: none;
    color: var(--text-3);
  }
</style>
