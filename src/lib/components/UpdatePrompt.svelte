<!-- The update offer: top right, under the title bar, out of the way of what you're doing. Shown
     when the main window is on screen (started in the tray, it waits for the window). -->
<script lang="ts">
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import Logo from './Logo.svelte';
  import { t } from '$lib/i18n.svelte';
  import { updates } from '$lib/updates.svelte';

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const version = $derived(updates.available?.version ?? '');
  const installing = $derived(updates.installing);
</script>

<div class="offer" role="status" in:fly={{ y: -12, duration: 320 * motion, easing: cubicOut }} out:fly={{ y: -8, duration: 160 * motion }}>
  <span class="badge"><Logo size={22} /></span>
  <div class="text">
    <strong>{t('update.offerTitle', { version })}</strong>
    <span>{updates.summary || t('update.offerBody')}</span>
    {#if installing !== null}
      <div class="bar" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(installing * 100)}>
        <span style:transform="scaleX({installing})"></span>
      </div>
      <span class="progress">{t('update.installing', { percent: Math.round(installing * 100) })}</span>
    {:else}
      <div class="actions">
        <button class="later" onclick={() => updates.later()}>{t('update.later')}</button>
        <button class="now" onclick={() => updates.install()}><Icon name="zap" size={13} />{t('update.now')}</button>
      </div>
    {/if}
  </div>
  {#if installing === null}
    <button class="close" onclick={() => updates.later()} aria-label={t('update.later')}><Icon name="x" size={14} /></button>
  {/if}
</div>

<style>
  .offer {
    position: fixed;
    top: calc(var(--titlebar) + 10px);
    right: 20px;
    z-index: 55;
    display: flex;
    align-items: flex-start;
    gap: 12px;
    width: min(360px, calc(100vw - 40px));
    padding: 14px 10px 14px 14px;
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--surface-2) 96%, transparent);
    backdrop-filter: blur(16px);
    box-shadow: var(--shadow-pop);
  }
  .badge {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    flex: none;
    border-radius: 10px;
    background: var(--accent-soft);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    flex: 1;
    min-width: 0;
  }
  strong {
    font-size: 14px;
    font-weight: 650;
  }
  .text > span {
    font-size: 12.5px;
    line-height: 1.45;
    color: var(--text-2);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 6px;
    margin-top: 10px;
  }
  .actions button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 30px;
    padding: 0 12px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
    transition:
      background var(--fast),
      filter var(--fast);
  }
  .later {
    color: var(--text-2);
    background: rgba(255, 255, 255, 0.06);
  }
  .later:hover {
    background: rgba(255, 255, 255, 0.1);
  }
  .now {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .now:hover {
    filter: brightness(1.08);
  }
  .bar {
    height: 4px;
    margin-top: 10px;
    border-radius: 2px;
    background: var(--surface-4);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transition: transform 200ms linear;
  }
  .text > .progress {
    font-size: 12px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .close {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex: none;
    border-radius: 7px;
    color: var(--text-3);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .close:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.06);
  }
</style>
