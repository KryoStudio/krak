<!-- Floating bar for the checked shards: count, select all, favorite, delete, clear. -->
<script lang="ts">
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import { t } from '$lib/i18n.svelte';
  import { portal } from '$lib/portal';
  import { tooltip } from '$lib/tooltip';

  let {
    count,
    total,
    allFavorite = false,
    onall,
    onfavorite,
    ondelete,
    onclear,
  }: {
    count: number;
    total: number;
    allFavorite?: boolean;
    onall: () => void;
    /** Leave out for things that can't be favorites. */
    onfavorite?: () => void;
    ondelete: () => void;
    onclear: () => void;
  } = $props();

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
</script>

<div class="bar" use:portal={'.content'} transition:fly={{ y: 24, duration: 260 * motion, easing: cubicOut, opacity: 0 }} role="toolbar" aria-label={t('selection.count', { count })}>
  <button class="icon" onclick={onclear} aria-label={t('selection.clear')} use:tooltip={{ text: `${t('selection.clear')} · Échap`, side: 'top' }}>
    <Icon name="x" size={15} />
  </button>
  {#key count}
    <strong in:fly={{ y: 6, duration: 180 * motion }}>{t('selection.count', { count })}</strong>
  {/key}
  {#if count < total}
    <button class="text" onclick={onall} use:tooltip={{ text: 'Ctrl+A', side: 'top' }}>{t('selection.all')}</button>
  {/if}
  <span class="divider"></span>
  {#if onfavorite}
    <button class="action" class:on={allFavorite} onclick={onfavorite}>
      <Icon name="star" size={15} fill={allFavorite} />
      {t(allFavorite ? 'selection.unfavorite' : 'selection.favorite')}
    </button>
  {/if}
  <button class="action danger" onclick={ondelete} use:tooltip={{ text: t('key.delete'), side: 'top' }}>
    <Icon name="trash" size={15} />{t('selection.delete')}
  </button>
</div>

<style>
  .bar {
    position: absolute;
    left: 50%;
    bottom: 22px;
    z-index: 15;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 8px 7px 7px;
    border-radius: 14px;
    background: color-mix(in srgb, var(--surface-3) 94%, transparent);
    backdrop-filter: blur(16px);
    box-shadow:
      0 0 0 1px var(--border-strong),
      0 18px 50px -14px rgba(0, 0, 0, 0.85);
    translate: -50% 0;
    white-space: nowrap;
  }
  strong {
    min-width: 108px;
    font-size: 13.5px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 9px;
    color: var(--text-2);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .icon:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.08);
  }
  .text {
    padding: 0 8px;
    height: 32px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 500;
    color: var(--accent);
    transition: background var(--fast);
  }
  .text:hover {
    background: var(--accent-soft);
  }
  .divider {
    width: 1px;
    height: 22px;
    margin: 0 2px;
    background: var(--border-strong);
  }
  .action {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 13px;
    border-radius: 9px;
    font-size: 13px;
    font-weight: 600;
    background: rgba(255, 255, 255, 0.06);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .action:hover {
    background: rgba(255, 255, 255, 0.11);
  }
  .action.on :global(svg) {
    color: #f5c84c;
  }
  .action.danger {
    color: #fff;
    background: color-mix(in srgb, var(--danger) 85%, transparent);
  }
  .action.danger:hover {
    background: var(--danger);
  }
</style>
