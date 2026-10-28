<script lang="ts">
  import { fade, scale } from 'svelte/transition';
  import { prefersReducedMotion } from 'svelte/motion';
  import { t } from '$lib/i18n.svelte';
  import { dialog } from '$lib/ui.svelte';

  let confirmButton = $state<HTMLButtonElement>();
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);

  $effect(() => {
    if (dialog.open) confirmButton?.focus();
  });

  function answer(ok: boolean) {
    dialog.open = false;
    dialog.resolve(ok);
    dialog.resolve = () => {};
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') answer(false);
  }
</script>

{#if dialog.open && dialog.options}
  <div class="backdrop" transition:fade={{ duration: 160 * motion }} onpointerdown={() => answer(false)} role="presentation"></div>
  <div class="center">
    <div
      class="dialog"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="dialog-title"
      tabindex="-1"
      {onkeydown}
      in:scale={{ start: 0.96, duration: 200 * motion }}
      out:scale={{ start: 0.98, duration: 120 * motion }}
    >
      <h2 id="dialog-title">{dialog.options.title}</h2>
      <p>{dialog.options.body}</p>
      <div class="buttons">
        <button class="secondary" onclick={() => answer(false)}>{t('common.cancel')}</button>
        <button bind:this={confirmButton} class="primary" class:danger={dialog.options.danger} onclick={() => answer(true)}>
          {dialog.options.confirm}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    background: rgba(0, 0, 0, 0.5);
    backdrop-filter: blur(2px);
  }
  .center {
    position: fixed;
    inset: 0;
    z-index: 51;
    display: grid;
    place-items: center;
    pointer-events: none;
  }
  .dialog {
    width: 400px;
    padding: 22px 22px 18px;
    border-radius: var(--radius-lg);
    background: var(--surface-2);
    box-shadow: var(--shadow-pop);
    pointer-events: auto;
    outline: none;
  }
  h2 {
    font-size: 16px;
    font-weight: 600;
  }
  p {
    margin-top: 8px;
    color: var(--text-2);
    overflow-wrap: anywhere;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 22px;
  }
  button {
    height: 34px;
    padding: 0 16px;
    border-radius: 8px;
    font-weight: 600;
    font-size: 13px;
    transition:
      background var(--fast),
      filter var(--fast);
  }
  .secondary {
    background: rgba(255, 255, 255, 0.06);
  }
  .secondary:hover {
    background: rgba(255, 255, 255, 0.1);
  }
  .primary {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .primary.danger {
    background: var(--danger);
    color: #fff;
  }
  .primary:hover {
    filter: brightness(1.1);
  }
</style>
