<!-- Pick one of a few options; the selection slides between them. Arrow keys move it. -->
<script lang="ts" generics="T extends string | number | null">
  let {
    options,
    value,
    label,
    onchange,
  }: { options: { value: T; label: string }[]; value: T; label: string; onchange: (value: T) => void } = $props();

  const buttons: HTMLButtonElement[] = $state([]);
  let thumb = $state({ x: 0, w: 0, shown: false, animate: false });
  const selected = $derived(options.findIndex((o) => o.value === value));

  $effect(() => {
    const el = buttons[selected];
    if (!el) {
      thumb.shown = false;
      return;
    }
    const measure = () => {
      thumb.x = el.offsetLeft;
      thumb.w = el.offsetWidth;
      thumb.shown = true;
    };
    measure();
    requestAnimationFrame(() => (thumb.animate = true));
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  });

  function onkeydown(e: KeyboardEvent) {
    const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    if (!step) return;
    e.preventDefault();
    const next = Math.min(options.length - 1, Math.max(0, (selected < 0 ? 0 : selected) + step));
    onchange(options[next].value);
    buttons[next]?.focus();
  }
</script>

<!-- Arrow keys from the focused radio bubble up here. -->
<!-- svelte-ignore a11y_interactive_supports_focus -->
<div class="seg" role="radiogroup" aria-label={label} {onkeydown}>
  <span
    class="thumb"
    class:shown={thumb.shown}
    class:animate={thumb.animate}
    style:transform="translateX({thumb.x}px)"
    style:width="{thumb.w}px"
  ></span>
  {#each options as option, i (option.value)}
    <button
      bind:this={buttons[i]}
      role="radio"
      aria-checked={i === selected}
      tabindex={i === selected || (selected < 0 && i === 0) ? 0 : -1}
      class:on={i === selected}
      onclick={() => onchange(option.value)}
    >
      {option.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    position: relative;
    display: inline-flex;
    padding: 3px;
    border-radius: 9px;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .thumb {
    position: absolute;
    top: 3px;
    bottom: 3px;
    left: 0;
    border-radius: 6px;
    background: var(--surface-3);
    box-shadow:
      inset 0 0 0 1px var(--border-strong),
      0 1px 3px rgba(0, 0, 0, 0.3);
    opacity: 0;
  }
  .thumb.shown {
    opacity: 1;
  }
  .thumb.animate {
    transition:
      transform var(--slow) var(--ease-out),
      width var(--slow) var(--ease-out),
      opacity var(--normal);
  }
  button {
    position: relative;
    height: 28px;
    padding: 0 12px;
    border-radius: 6px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-3);
    white-space: nowrap;
    transition: color var(--normal);
  }
  button:hover {
    color: var(--text-2);
  }
  button.on {
    color: var(--text);
  }
</style>
