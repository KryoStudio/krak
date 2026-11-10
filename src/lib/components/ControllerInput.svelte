<!-- Click, then hold the controller buttons together and let go. Click again to cancel; × removes
     the shortcut. -->
<script lang="ts">
  import Icon from './Icon.svelte';
  import { api, errorCode, native } from '$lib/api';
  import { hasKey, t } from '$lib/i18n.svelte';
  import { tooltip } from '$lib/tooltip';

  let { value, onchange }: { value: string; onchange: (combo: string) => Promise<void> } = $props();

  let listening = $state(false);
  let hint = $state('');
  /** Bumped by every start and cancel: an answer for an older attempt is dropped. */
  let attempt = 0;

  async function start() {
    const mine = ++attempt;
    listening = true;
    hint = '';
    try {
      // The browser preview has no controller: pretend one was pressed.
      const combo = native ? await api.captureControllerCombo() : 'LB+RB';
      if (mine === attempt) await onchange(combo);
    } catch (e) {
      const key = `error.${errorCode(e)}`;
      if (mine === attempt) hint = t(hasKey(key) ? key : 'error.unknown');
    } finally {
      if (mine === attempt) listening = false;
    }
  }

  function cancel() {
    attempt++;
    listening = false;
    hint = '';
  }
</script>

<div class="wrap">
  <div class="field">
    <button class:listening onclick={() => (listening ? cancel() : start())} aria-label={t('settings.controller')}>
      {#if listening}
        <span class="listening">{t('settings.controllerListen')}</span>
      {:else if value}
        <Icon name="gamepad" size={15} />
        {#each value.split('+') as button (button)}<kbd>{button}</kbd>{/each}
      {:else}
        <span class="none">{t('settings.controllerNone')}</span>
      {/if}
    </button>
    {#if value && !listening}
      <button
        class="clear"
        onclick={() => onchange('')}
        aria-label={t('settings.controllerClear')}
        use:tooltip={{ text: t('settings.controllerClear'), side: 'top' }}
      >
        <Icon name="x" size={14} />
      </button>
    {/if}
  </div>
  {#if hint}<span class="hint">{hint}</span>{/if}
</div>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 6px;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    min-width: 132px;
    height: 34px;
    padding: 0 10px;
    border-radius: 8px;
    color: var(--text-2);
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition: box-shadow var(--normal);
  }
  button:hover {
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.18);
  }
  button.listening {
    box-shadow:
      inset 0 0 0 1px var(--accent-line),
      0 0 0 3px var(--accent-soft);
  }
  .clear {
    min-width: 34px;
    width: 34px;
    padding: 0;
    color: var(--text-3);
  }
  .clear:hover {
    color: var(--text);
  }
  kbd {
    color: var(--text);
  }
  .none {
    font-size: 12.5px;
    color: var(--text-2);
  }
  .listening {
    font-size: 12.5px;
    color: var(--accent);
    animation: blink 1.2s var(--ease-in-out) infinite;
  }
  .hint {
    font-size: 12px;
    color: #f5a524;
  }
  @keyframes blink {
    50% {
      opacity: 0.5;
    }
  }
</style>
