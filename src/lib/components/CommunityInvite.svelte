<!-- The one-time invitation to the Krak Discord, where the update offer shows (never both). -->
<script lang="ts">
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import { t } from '$lib/i18n.svelte';
  import { invite } from '$lib/invite.svelte';

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
</script>

<div class="invite" role="status" in:fly={{ y: -12, duration: 320 * motion, easing: cubicOut }} out:fly={{ y: -8, duration: 160 * motion }}>
  <span class="badge"><Icon name="users" size={18} /></span>
  <div class="text">
    <strong>{t('community.inviteTitle')}</strong>
    <span>{t('community.hint')}</span>
    <div class="actions">
      <button class="never" onclick={() => invite.never()}>{t('community.never')}</button>
      <div class="answers">
        <button class="later" onclick={() => invite.notNow()}>{t('community.notNow')}</button>
        <button class="join" onclick={() => invite.join()}>{t('community.joinShort')}<Icon name="external" size={12} /></button>
      </div>
    </div>
  </div>
  <button class="close" onclick={() => invite.notNow()} aria-label={t('community.notNow')}><Icon name="x" size={14} /></button>
</div>

<style>
  .invite {
    position: fixed;
    top: calc(var(--titlebar) + 10px);
    right: 20px;
    z-index: 55;
    display: flex;
    align-items: flex-start;
    gap: 12px;
    width: min(380px, calc(100vw - 40px));
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
    color: var(--accent);
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
  /* One row when it fits ("Don't ask again" on the left). Otherwise the answers keep the first
     row and "Don't ask again" goes under them (wrap-reverse): the buttons never run past the card,
     whatever the language. */
  .actions {
    display: flex;
    flex-wrap: wrap-reverse;
    align-items: center;
    gap: 6px;
    margin-top: 10px;
  }
  .answers {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 6px;
    margin-left: auto;
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
    white-space: nowrap;
    transition:
      background var(--fast),
      color var(--fast),
      filter var(--fast);
  }
  .actions .never {
    padding: 0 4px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-3);
  }
  .actions .never:hover {
    color: var(--text-2);
  }
  .later {
    color: var(--text-2);
    background: rgba(255, 255, 255, 0.06);
  }
  .later:hover {
    background: rgba(255, 255, 255, 0.1);
  }
  .join {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .join:hover {
    filter: brightness(1.08);
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
