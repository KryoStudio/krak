<script lang="ts">
  import type { Snippet } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import Logo from './Logo.svelte';
  import Icon from './Icon.svelte';
  import GameIcon from './GameIcon.svelte';
  import { formatSize, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { screens } from '$lib/screenshots.svelte';
  import { settings } from '$lib/settings.svelte';
  import { capture, formatClock } from '$lib/capture.svelte';
  import { showError } from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { filterFor, ui, type Filter } from '$lib/ui.svelte';

  const collapsed = $derived(settings.value.sidebarCollapsed);
  const path = $derived(page.url.pathname);
  const active = $derived(
    path.startsWith('/settings')
      ? 'settings'
      : path.startsWith('/screenshots')
        ? 'screenshots'
        : path.startsWith('/glacier')
          ? ui.filter
          : 'home',
  );

  function show(filter: Filter) {
    ui.filter = filter;
    ui.playing = null;
    if (!path.startsWith('/glacier')) goto('/glacier');
  }

  // One highlight slides between the items of the list instead of each item lighting up.
  let list: HTMLElement;
  const items: Record<string, HTMLElement> = $state({});
  let highlight = $state({ y: 0, h: 0, shown: false, animate: false });

  $effect(() => {
    const el = items[active];
    if (!el || !list) {
      highlight.shown = false;
      return;
    }
    const measure = () => {
      highlight.y = el.offsetTop;
      highlight.h = el.offsetHeight;
      highlight.shown = true;
    };
    measure();
    // Slide only once the first position is known, so it doesn't fly in from the top.
    requestAnimationFrame(() => (highlight.animate = true));
    const ro = new ResizeObserver(measure);
    ro.observe(list);
    return () => ro.disconnect();
  });

  const recording = $derived(capture.status.recording);
  const engineState = $derived(recording ? 'recording' : capture.status.state);
  const seconds = $derived(settings.value.bufferSeconds);
  const status = $derived(
    recording
      ? { label: `${t('record.live')} · ${formatClock(recording.seconds)}`, hint: formatSize(recording.bytes), action: t('record.stop') }
      : {
          starting: { label: t('capture.starting'), hint: t('capture.hintStarting'), action: '' },
          ready: { label: t('capture.ready'), hint: t('capture.hint', { seconds }), action: t('capture.pause') },
          paused: { label: t('capture.paused'), hint: t('capture.hintPaused'), action: t('capture.resume') },
          error: { label: t('capture.error'), hint: t('capture.hintError'), action: t('nav.settings') },
        }[capture.status.state],
  );
  // The buffer fills up after (re)starting: show it until it holds the full length.
  const fill = $derived(engineState === 'ready' ? Math.min(1, capture.status.bufferedSeconds / seconds) : 0);

  function onStatus() {
    if (recording) capture.toggleRecording();
    else if (engineState === 'ready') capture.setPaused(true).catch(showError);
    else if (engineState === 'paused') capture.setPaused(false).catch(showError);
    else if (engineState === 'error') goto('/settings');
  }
</script>

{#snippet item(key: string, label: string, count: number | null, icon: Snippet, onclick: () => void)}
  <button
    bind:this={items[key]}
    class="item"
    class:active={active === key}
    aria-current={active === key ? 'page' : undefined}
    {onclick}
    use:tooltip={{ text: label, enabled: collapsed }}
  >
    <span class="icon">{@render icon()}</span>
    <span class="label">{label}</span>
    {#if count !== null}<span class="count">{count}</span>{/if}
  </button>
{/snippet}

{#snippet imageIcon()}<Icon name="image" size={16} />{/snippet}
{#snippet homeIcon()}<Icon name="home" size={16} />{/snippet}
{#snippet gridIcon()}<Icon name="grid" size={16} />{/snippet}
{#snippet starIcon()}<Icon name="star" size={16} />{/snippet}
{#snippet gearIcon()}<Icon name="settings" size={16} />{/snippet}

<aside class:collapsed>
  <div class="brand" data-tauri-drag-region>
    <Logo size={26} />
    <span class="wordmark" data-tauri-drag-region>krak</span>
  </div>

  <button
    class="status"
    data-state={engineState}
    onclick={onStatus}
    disabled={engineState === 'starting'}
    aria-label={status.action || status.label}
    use:tooltip={{ text: collapsed ? `${status.label} · ${status.hint}` : status.action, enabled: collapsed || !!status.action }}
  >
    <span class="dot-wrap"><span class="dot"></span></span>
    <span class="status-text">
      <strong>{status.label}</strong>
      <span>{status.hint}</span>
    </span>
    <kbd>{settings.value.hotkey}</kbd>
    {#if fill > 0 && fill < 1}<span class="fill" style:transform="scaleX({fill})"></span>{/if}
  </button>

  <nav>
    <div class="list" bind:this={list}>
      <span
        class="highlight"
        class:shown={highlight.shown}
        class:animate={highlight.animate}
        style:transform="translateY({highlight.y}px)"
        style:height="{highlight.h}px"
      ></span>

      {@render item('home', t('nav.home'), null, homeIcon, () => ((ui.playing = null), goto('/')))}
      {@render item('all', t('nav.glacier'), library.shards.length, gridIcon, () => show('all'))}
      {@render item('favorites', t('nav.favorites'), library.favorites, starIcon, () => show('favorites'))}
      {@render item('screenshots', t('nav.screenshots'), screens.items.length, imageIcon, () => ((ui.playing = null), goto('/screenshots')))}

      {#if library.games.length}
        <div class="section">
          <span>{t('nav.games')}</span>
        </div>
        {#each library.games as { game, count } (game)}
          {@const name = game ?? t('game.desktop')}
          {#snippet tile()}<GameIcon {game} size={22} />{/snippet}
          {@render item(filterFor(game), name, count, tile, () => show(filterFor(game)))}
        {/each}
      {/if}
    </div>
  </nav>

  <div class="foot">
    {@render item('settings', t('nav.settings'), null, gearIcon, () => goto('/settings'))}
  </div>
</aside>

<style>
  aside {
    --pad: 12px;
    display: flex;
    flex-direction: column;
    width: var(--sidebar);
    height: 100vh;
    padding: 0 var(--pad) var(--pad);
    background: var(--bg-raised);
    border-right: 1px solid var(--border);
    overflow: hidden;
    transition: width var(--slow) var(--ease-out);
  }
  aside.collapsed {
    width: var(--sidebar-collapsed);
  }

  /* Everything that only makes sense with room: fades out first, back in last. */
  .wordmark,
  .status-text,
  .status kbd,
  .label,
  .count,
  .section span {
    white-space: nowrap;
    transition: opacity var(--normal) var(--ease-out) 60ms;
  }
  .collapsed :is(.wordmark, .status-text, .status kbd, .label, .count, .section span) {
    opacity: 0;
    pointer-events: none;
    transition-delay: 0ms;
    transition-duration: var(--fast);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 11px;
    height: var(--titlebar);
    padding-left: 7px;
    flex: none;
  }
  .brand :global(svg) {
    flex: none;
  }
  .wordmark {
    font-weight: 700;
    font-size: 21px;
    line-height: 1;
    letter-spacing: -0.05em;
    padding-bottom: 2px;
  }

  .status {
    position: relative;
    display: flex;
    align-items: center;
    text-align: left;
    height: 50px;
    margin: 6px 0 14px;
    padding-right: 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface);
    flex: none;
    overflow: hidden;
    transition:
      background var(--normal),
      border-color var(--normal);
  }
  .status:hover:not(:disabled) {
    background: var(--surface-2);
    border-color: var(--border-strong);
  }
  .status:disabled {
    cursor: default;
  }
  .fill {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 2px;
    background: var(--accent);
    opacity: 0.7;
    transform-origin: left;
    transition: transform 1s linear;
  }
  .dot-wrap {
    display: grid;
    place-items: center;
    width: 38px;
    flex: none;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-3);
  }
  [data-state='ready'] .dot {
    background: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 20%, transparent);
    animation: breathe 2.4s var(--ease-in-out) infinite;
  }
  [data-state='paused'] .dot {
    background: #f5a524;
  }
  [data-state='error'] .dot {
    background: var(--danger);
  }
  [data-state='recording'] .dot {
    background: var(--rec);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--rec) 22%, transparent);
    animation: blink 1.2s var(--ease-in-out) infinite;
  }
  [data-state='recording'] {
    border-color: color-mix(in srgb, var(--rec) 35%, transparent);
  }
  [data-state='recording'] strong {
    font-variant-numeric: tabular-nums;
  }
  [data-state='starting'] .dot {
    animation: blink 1s var(--ease-in-out) infinite;
  }
  .status-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
    margin-right: 8px;
    line-height: 1.3;
  }
  .status-text strong {
    font-size: 13px;
    font-weight: 600;
  }
  [data-state='ready'] .status-text strong {
    color: var(--accent);
  }
  .status-text span {
    font-size: 11.5px;
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  nav {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    scrollbar-width: none;
  }
  .list {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .highlight {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    border-radius: var(--radius-sm);
    background: rgba(255, 255, 255, 0.06);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.04);
    opacity: 0;
    pointer-events: none;
  }
  .highlight.shown {
    opacity: 1;
  }
  .highlight.animate {
    transition:
      transform 380ms var(--ease-out),
      height 380ms var(--ease-out),
      opacity var(--normal);
  }

  .item {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 8px 0 6px;
    border-radius: var(--radius-sm);
    color: var(--text-2);
    font-weight: 500;
    text-align: left;
    transition: color var(--fast);
  }
  .item::before {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: rgba(255, 255, 255, 0.035);
    opacity: 0;
    transition: opacity var(--fast);
  }
  .item:hover::before {
    opacity: 1;
  }
  .item.active::before {
    opacity: 0;
  }
  .item:hover,
  .item.active {
    color: var(--text);
  }
  .item:focus-visible {
    outline-width: 1.5px;
    outline-offset: -1.5px;
  }

  .icon {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: 7px;
    color: var(--text-3);
    transition:
      background var(--normal),
      color var(--normal),
      box-shadow var(--normal);
  }
  .item:hover .icon {
    color: var(--text-2);
  }
  .item.active .icon {
    color: var(--accent);
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 22%, transparent);
  }
  /* Game tiles keep their colors; the active one gets a ring. */
  .icon:has(:global(.tile)) {
    border-radius: 9px;
  }
  .item.active .icon:has(:global(.tile)) {
    background: none;
    box-shadow:
      0 0 0 1.5px var(--bg-raised),
      0 0 0 3px var(--accent);
  }

  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .count {
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    color: var(--text-3);
  }
  .item.active .count {
    color: var(--text-2);
  }

  .section {
    position: relative;
    height: 36px;
    display: flex;
    align-items: flex-end;
    padding: 0 0 7px 8px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-3);
  }
  /* collapsed: the title gives way to a separator */
  .section::after {
    content: '';
    position: absolute;
    left: 8px;
    right: 8px;
    top: 50%;
    height: 1px;
    background: var(--border-strong);
    opacity: 0;
    transition: opacity var(--fast);
  }
  .collapsed .section::after {
    opacity: 1;
    transition-delay: 120ms;
  }

  .foot {
    display: flex;
    flex-direction: column;
    padding-top: 10px;
    margin-top: 8px;
    border-top: 1px solid var(--border);
    flex: none;
  }
  .foot .item.active {
    background: rgba(255, 255, 255, 0.06);
  }

  @keyframes blink {
    50% {
      opacity: 0.3;
    }
  }
  @keyframes breathe {
    50% {
      box-shadow: 0 0 0 5px color-mix(in srgb, var(--accent) 8%, transparent);
    }
  }
</style>
