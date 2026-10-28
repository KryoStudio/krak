<!-- Click, then press the new shortcut. Escape cancels. -->
<script lang="ts">
  import { formatHotkey, t } from '$lib/i18n.svelte';

  let { value, onchange }: { value: string; onchange: (accelerator: string) => Promise<void> } = $props();

  let recording = $state(false);
  let hint = $state('');

  // Keys that are fine on their own; anything else (letters, digits, space…) would fire while typing.
  const standalone = /^(F\d{1,2}|Pause|ScrollLock|PrintScreen|Insert|Home|End|PageUp|PageDown|Numpad.*)$/;
  const modifierKeys = ['Control', 'Alt', 'Shift', 'Meta'];

  /** KeyboardEvent.code → accelerator key ("KeyA" → "A"). Every code is understood by the backend. */
  function keyName(code: string): string {
    return code.replace(/^Key|^Digit/, '').replace(/^Arrow/, '');
  }

  async function onkeydown(e: KeyboardEvent) {
    if (!recording) return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === 'Escape') return stop();
    if (modifierKeys.includes(e.key)) return;

    const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Super'].filter(Boolean);
    const key = keyName(e.code);
    if (!mods.length && !standalone.test(e.code)) {
      hint = t('settings.hotkeyModifier');
      return;
    }
    stop();
    await onchange([...mods, key].join('+'));
  }

  function start() {
    recording = true;
    hint = '';
  }

  function stop() {
    recording = false;
    hint = '';
  }
</script>

<div class="wrap">
  <button
    class:recording
    onclick={() => (recording ? stop() : start())}
    {onkeydown}
    onblur={stop}
    aria-label={t('settings.hotkey')}
  >
    {#if recording}
      <span class="listening">{t('settings.hotkeyRecord')}</span>
    {:else}
      {#each formatHotkey(value) as k (k)}<kbd>{k}</kbd>{/each}
    {/if}
  </button>
  {#if hint}<span class="hint">{hint}</span>{/if}
</div>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
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
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition: box-shadow var(--normal);
  }
  button:hover {
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.18);
  }
  button.recording {
    box-shadow:
      inset 0 0 0 1px var(--accent-line),
      0 0 0 3px var(--accent-soft);
  }
  kbd {
    color: var(--text);
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
