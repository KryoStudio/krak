<script lang="ts">
  import { prefersReducedMotion } from 'svelte/motion';
  import { cubicOut } from 'svelte/easing';
  import Icon from './Icon.svelte';
  import { menu, type MenuItem } from '$lib/ui.svelte';

  let el = $state<HTMLElement>();
  let pos = $state({ x: 0, y: 0, originX: 'left', originY: 'top' });
  let focused = $state(-1);

  // Keep the menu inside the window, opening upwards/leftwards when there is no room.
  $effect(() => {
    if (!menu.open || !el) return;
    const { width, height } = el.getBoundingClientRect();
    const flipX = menu.x + width > innerWidth - 8;
    const flipY = menu.y + height > innerHeight - 8;
    pos = {
      x: flipX ? Math.max(8, menu.x - width) : menu.x,
      y: flipY ? Math.max(8, menu.y - height) : menu.y,
      originX: flipX ? 'right' : 'left',
      originY: flipY ? 'bottom' : 'top',
    };
    focused = -1;
    el.focus();
  });

  const actionable = $derived(menu.items.map((item, i) => (item === 'separator' ? -1 : i)).filter((i) => i >= 0));

  function close() {
    menu.open = false;
  }

  function run(item: MenuItem) {
    if (item === 'separator') return;
    close();
    item.run();
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape' || e.key === 'Tab') {
      e.preventDefault();
      close();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const at = actionable.indexOf(focused);
      const step = e.key === 'ArrowDown' ? 1 : -1;
      focused = actionable[(at + step + actionable.length) % actionable.length] ?? -1;
      if (at < 0 && step < 0) focused = actionable[actionable.length - 1];
    } else if (e.key === 'Enter' && focused >= 0) {
      e.preventDefault();
      run(menu.items[focused]);
    }
  }

  function pop(_: Element, { out = false } = {}) {
    const d = prefersReducedMotion.current ? 0 : out ? 90 : 160;
    return {
      duration: d,
      easing: cubicOut,
      css: (t: number) => (out ? `opacity: ${t}` : `opacity: ${t}; transform: scale(${0.96 + 0.04 * t})`),
    };
  }
</script>

<svelte:window onblur={close} onresize={close} />

{#if menu.open}
  <div class="catcher" onpointerdown={close} oncontextmenu={(e) => (e.preventDefault(), close())} role="presentation"></div>
  <div
    bind:this={el}
    class="menu"
    role="menu"
    tabindex="-1"
    style:left="{pos.x}px"
    style:top="{pos.y}px"
    style:transform-origin="{pos.originY} {pos.originX}"
    {onkeydown}
    in:pop
    out:pop={{ out: true }}
  >
    {#each menu.items as item, i (i)}
      {#if item === 'separator'}
        <div class="sep" role="separator"></div>
      {:else}
        <button
          role="menuitem"
          class:danger={item.danger}
          class:focused={focused === i}
          onclick={() => run(item)}
          onpointermove={() => (focused = i)}
          onpointerleave={() => (focused = -1)}
        >
          <span class="icon">{#if item.icon}<Icon name={item.icon} size={15} />{/if}</span>
          <span class="label">{item.label}</span>
          {#if item.tag}<span class="tag">{item.tag}</span>{/if}
          {#if item.shortcut}<span class="shortcut">{item.shortcut}</span>{/if}
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  /* Above every layer, dialogs and the first-launch setup included: their dropdowns open here. */
  .catcher {
    position: fixed;
    inset: 0;
    z-index: 80;
  }
  .menu {
    position: fixed;
    z-index: 81;
    min-width: 220px;
    max-height: calc(100vh - 16px);
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 5px;
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--surface-2) 92%, transparent);
    backdrop-filter: blur(16px);
    box-shadow: var(--shadow-pop);
    outline: none;
  }
  button {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    height: 32px;
    padding: 0 10px 0 8px;
    border-radius: var(--radius-sm);
    font-size: 13px;
    color: var(--text);
    text-align: left;
  }
  button.focused {
    background: rgba(255, 255, 255, 0.07);
  }
  .icon {
    display: grid;
    place-items: center;
    width: 16px;
    color: var(--text-2);
  }
  .label {
    flex: 1;
  }
  .shortcut {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .tag {
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 10.5px;
    font-weight: 600;
    color: var(--accent);
    background: var(--accent-soft);
  }
  .danger,
  .danger .icon {
    color: var(--danger);
  }
  .danger.focused {
    background: color-mix(in srgb, var(--danger) 14%, transparent);
  }
  .sep {
    height: 1px;
    margin: 5px 6px;
    background: var(--border-strong);
  }
</style>
