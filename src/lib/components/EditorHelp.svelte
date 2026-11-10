<!-- The editor's help, opened from its toolbar (or with "?"), and by itself the first time: how
     to edit a clip in three steps, and every shortcut. Opens above its button (or below, when a
     small window leaves more room there), scrolling if it has to. Closes on Escape, a click
     elsewhere or its button. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import { locale, t } from '$lib/i18n.svelte';

  let { onclose }: { onclose: () => void } = $props();

  let panel: HTMLDivElement;
  let above = $state(true);
  /** The height it has room for (px). */
  let room = $state<number | null>(null);
  /** Kept clear at the top for the title bar, and at the bottom (px). */
  const TOP = 56;
  const BOTTOM = 12;

  onMount(() => {
    const place = () => {
      const anchor = panel.parentElement?.getBoundingClientRect();
      if (!anchor) return;
      const up = anchor.top - 10 - TOP;
      const down = innerHeight - anchor.bottom - 10 - BOTTOM;
      above = up >= panel.scrollHeight || up >= down;
      room = Math.max(160, above ? up : down);
    };
    place();
    addEventListener('resize', place);
    return () => removeEventListener('resize', place);
  });
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);

  /** The tools, as a list in the language ("Cut out, Mute, … and Blur"). */
  const tools = $derived(
    new Intl.ListFormat(locale(), { type: 'conjunction' }).format(
      (['editor.cut', 'editor.muteZone', 'editor.beep', 'editor.slow', 'editor.text', 'editor.blur'] as const).map((key) => t(key)),
    ),
  );

  const steps = $derived([
    { title: t('editor.help.place'), text: t('editor.help.placeText') },
    { title: t('editor.help.pick'), text: t('editor.help.pickText') },
    { title: t('editor.help.apply'), text: t('editor.help.applyText', { tools }) },
  ]);

  const shortcuts = $derived([
    { keys: [t('key.space')], label: t('editor.keys.play') },
    { keys: ['←', '→'], label: t('editor.keys.seek') },
    { keys: ['S'], label: t('editor.keys.zone') },
    { keys: ['X'], label: t('editor.keys.cut') },
    { keys: ['M'], label: t('editor.keys.mute') },
    { keys: ['B'], label: t('editor.keys.beep') },
    { keys: ['R'], label: t('editor.keys.slow') },
    { keys: ['I', 'O'], label: t('editor.keys.trim') },
    { keys: [t('key.delete')], label: t('editor.keys.remove') },
    { keys: [t('key.escape')], label: t('editor.keys.escape') },
    { keys: ['Ctrl', 'Z'], label: t('editor.undo'), joined: true },
    { keys: ['Ctrl', 'Y'], label: t('editor.redo'), joined: true },
    { keys: ['?'], label: t('editor.keys.help') },
  ]);

  // A click outside the panel (and its toolbar button, which toggles it) closes it.
  function onpointerdown(e: PointerEvent) {
    const target = e.target as Element;
    if (!panel?.contains(target) && !target.closest?.('[data-editor-help]')) onclose();
  }
</script>

<svelte:window {onpointerdown} />

<div
  bind:this={panel}
  class="help"
  class:below={!above}
  style:max-height={room === null ? null : `${room}px`}
  role="dialog"
  aria-label={t('editor.help.title')}
  transition:fly={{ y: 8, duration: 180 * motion, easing: cubicOut }}
>
  <div class="top">
    <h2>{t('editor.help.title')}</h2>
    <button class="close" onclick={onclose} aria-label={t('common.close')}><Icon name="x" size={14} /></button>
  </div>
  <div class="columns">
    <ol class="steps">
      {#each steps as step, i (i)}
        <li>
          <span class="number">{i + 1}</span>
          <p><strong>{step.title}</strong> {step.text}</p>
        </li>
      {/each}
      <li class="undo">
        <span class="number"><Icon name="undo" size={12} /></span>
        <p>{t('editor.help.undoText')}</p>
      </li>
    </ol>
    <dl class="keys">
      {#each shortcuts as shortcut (shortcut.label)}
        <dt>
          {#each shortcut.keys as key, i (i)}
            {#if i > 0}<span class="sep">{shortcut.joined ? '+' : '/'}</span>{/if}<kbd>{key}</kbd>
          {/each}
        </dt>
        <dd>{shortcut.label}</dd>
      {/each}
    </dl>
  </div>
</div>

<style>
  .help {
    position: absolute;
    right: 0;
    bottom: calc(100% + 10px);
    z-index: 30;
    width: min(680px, calc(100vw - 340px));
    overflow: auto;
    padding: 16px 18px 18px;
    border-radius: var(--radius-lg);
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    box-shadow: 0 18px 50px rgba(0, 0, 0, 0.55);
  }
  .help.below {
    top: calc(100% + 10px);
    bottom: auto;
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }
  h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 700;
  }
  .close {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 7px;
    color: var(--text-3);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .close:hover {
    color: var(--text);
    background: var(--surface-3);
  }
  .columns {
    display: grid;
    grid-template-columns: minmax(0, 1.1fr) minmax(0, 1fr);
    gap: 22px;
  }
  .steps {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .steps li {
    display: flex;
    gap: 10px;
  }
  .number {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    flex: none;
    border-radius: 50%;
    font-size: 12px;
    font-weight: 700;
    color: var(--accent-fg);
    background: var(--accent);
  }
  .undo .number {
    color: var(--text-2);
    background: var(--surface-3);
  }
  .steps p {
    margin: 2px 0 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text-2);
  }
  .steps strong {
    color: var(--text);
  }
  .keys {
    display: grid;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 7px 12px;
    margin: 0;
    padding-left: 20px;
    border-left: 1px solid var(--border);
  }
  dt {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 3px;
    white-space: nowrap;
  }
  dd {
    margin: 0;
    font-size: 12.5px;
    color: var(--text-2);
  }
  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 22px;
    height: 22px;
    padding: 0 6px;
    border-radius: 5px;
    font-family: inherit;
    font-size: 11.5px;
    font-weight: 700;
    color: var(--text);
    background: var(--surface-3);
    box-shadow:
      inset 0 0 0 1px var(--border-strong),
      0 1px 0 var(--border-strong);
  }
  .sep {
    font-size: 11px;
    color: var(--text-3);
  }
</style>
