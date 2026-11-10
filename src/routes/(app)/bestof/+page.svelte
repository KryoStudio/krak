<!-- Best-of: put shards together into one video. The shards on the left (search, game, quick
     fills), the montage in the middle (preview, then the clips in order: drag to reorder, edit a
     clip in the editor, remove it), the settings on the right (format, transitions, title,
     music). Each clip plays as edited. A guide walks through it the first time (and on "?"). -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { flip } from 'svelte/animate';
  import { fly } from 'svelte/transition';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from '$lib/components/Icon.svelte';
  import GameIcon from '$lib/components/GameIcon.svelte';
  import Segmented from '$lib/components/Segmented.svelte';
  import Select from '$lib/components/Select.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import BestOfPreview from '$lib/components/BestOfPreview.svelte';
  import BestOfExport from '$lib/components/BestOfExport.svelte';
  import Tour, { type TourStep } from '$lib/components/Tour.svelte';
  import { api, errorCode, native } from '$lib/api';
  import { TITLE_SECONDS, add, bestof, clipLength, composition, move, persist, quickPick, removeAt } from '$lib/bestof.svelte';
  import { formatDuration, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import { displayTitle, showError } from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { toast, ui } from '$lib/ui.svelte';

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const GUIDE_KEY = 'krak.guide.bestof';

  let query = $state('');
  /** undefined: every game; null: the desktop. */
  let game = $state<string | null | undefined>(undefined);
  let exporting = $state(false);
  let guiding = $state(false);
  let dragging = $state<number | null>(null);
  let over = $state<number | null>(null);

  // Clips that left the library (deleted, renamed) leave the best-of too.
  const shards = $derived(bestof.items.map((id) => library.find(id)).filter((s) => !!s));
  $effect(() => {
    if (library.shards.length && shards.length !== bestof.items.length) bestof.items = shards.map((s) => s.id);
  });
  // Every change is kept for next time.
  $effect(() => {
    void [bestof.items.length, ...bestof.items, bestof.transition, bestof.titleOn, bestof.title, bestof.vertical, JSON.stringify(bestof.music)];
    persist();
  });

  const games = $derived([...new Set(library.shards.map((s) => s.game))].sort((a, b) => (a ?? '').localeCompare(b ?? '')));
  const gameOptions = $derived([
    { value: undefined as string | null | undefined, label: t('bestof.allGames') },
    ...games.map((g) => ({ value: g as string | null | undefined, label: g ?? t('game.desktop') })),
  ]);
  const available = $derived(
    library.shards
      .filter((s) => game === undefined || s.game === game)
      .filter((s) => !query.trim() || displayTitle(s).toLowerCase().includes(query.trim().toLowerCase()))
      .sort((a, b) => b.createdAt - a.createdAt),
  );
  const lengths = $derived(shards.map((s) => clipLength(s)));
  const total = $derived(lengths.reduce((sum, l) => sum + l, 0) + (bestof.titleOn && shards.length ? TITLE_SECONDS : 0));
  const defaultTitle = $derived(t('bestof.defaultTitle'));
  // The title card opens a best-of that has clips.
  const title = $derived(bestof.titleOn && shards.length ? bestof.title.trim() || defaultTitle : null);

  onMount(() => {
    let seen = true;
    try {
      seen = localStorage.getItem(GUIDE_KEY) === '1';
    } catch {
      // no storage: no guide
    }
    if (!seen) setTimeout(() => (guiding = true), 450);
  });

  function closeGuide() {
    guiding = false;
    try {
      localStorage.setItem(GUIDE_KEY, '1');
    } catch {
      // no storage
    }
  }

  const steps = $derived<TourStep[]>([
    { title: t('bestof.guide.welcomeTitle'), body: t('bestof.guide.welcome') },
    { target: 'library', title: t('bestof.guide.libraryTitle'), body: t('bestof.guide.library') },
    { target: 'quick', title: t('bestof.guide.quickTitle'), body: t('bestof.guide.quick') },
    { target: 'strip', title: t('bestof.guide.stripTitle'), body: t('bestof.guide.strip') },
    { target: 'preview', title: t('bestof.guide.previewTitle'), body: t('bestof.guide.preview') },
    { target: 'format', title: t('bestof.guide.formatTitle'), body: t('bestof.guide.format') },
    { target: 'music', title: t('bestof.guide.musicTitle'), body: t('bestof.guide.music') },
    { target: 'create', title: t('bestof.guide.createTitle'), body: t('bestof.guide.create') },
  ]);

  function quick(kind: Parameters<typeof quickPick>[1]) {
    const ids = quickPick(library.shards, kind);
    if (!ids.length) {
      toast({ kind: 'info', title: t('bestof.quickNone') });
      return;
    }
    add(ids);
  }

  function edit(id: string) {
    ui.playing = id;
    ui.editing = true;
  }

  async function pickMusic() {
    if (!native) {
      toast({ kind: 'info', title: t('toast.demo') });
      return;
    }
    const path = await api.chooseMusic().catch((e) => {
      showError(errorCode(e));
      return null;
    });
    if (!path) return;
    const name = path.split(/[\\/]/).pop() ?? path;
    bestof.music = { path, name, volume: bestof.music?.volume ?? 0.6, offset: 0, duck: bestof.music?.duck ?? true };
  }

  // Drag and drop to reorder the montage.
  function dragstart(i: number) {
    return (e: DragEvent) => {
      dragging = i;
      e.dataTransfer?.setData('text/plain', String(i));
      if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
    };
  }
  function dragover(i: number) {
    return (e: DragEvent) => {
      if (dragging === null) return;
      e.preventDefault();
      over = i;
    };
  }
  function drop(i: number) {
    return (e: DragEvent) => {
      e.preventDefault();
      if (dragging !== null) move(dragging, i);
      dragging = over = null;
    };
  }

  const formats = $derived((['wide', 'vertical'] as const).map((value) => ({ value, label: t(`bestof.format.${value}`) })));
  const transitions = $derived((['cut', 'fade', 'flash'] as const).map((value) => ({ value, label: t(`bestof.transition.${value}`) })));
</script>

<div class="bestof">
  <header>
    <div class="heading">
      <h1>{t('bestof.title')}</h1>
      <p>{t('bestof.subtitle')}</p>
    </div>
    <button class="icon-button" onclick={() => (guiding = true)} aria-label={t('bestof.guide.open')} use:tooltip={{ text: t('bestof.guide.open'), side: 'bottom' }}>
      <Icon name="info" size={16} />
    </button>
    <button class="button primary" data-tour="create" disabled={!shards.length} onclick={() => (exporting = true)}>
      <Icon name="sparkles" size={15} />{t('bestof.create')}
    </button>
  </header>

  <div class="layout">
    <aside class="panel library" data-tour="library">
      <div class="search">
        <Icon name="search" size={14} />
        <input type="search" bind:value={query} placeholder={t('bestof.search')} aria-label={t('bestof.search')} />
      </div>
      <Select label={t('bestof.allGames')} options={gameOptions} value={game} onchange={(v) => (game = v)} width={252} />
      <div class="quick" data-tour="quick">
        <span class="label">{t('bestof.quick')}</span>
        <div class="quick-buttons">
          <button class="chip" onclick={() => quick('favorites')}><Icon name="star" size={12} />{t('bestof.quickFavorites')}</button>
          <button class="chip" onclick={() => quick('week')}><Icon name="clock" size={12} />{t('bestof.quickWeek')}</button>
          {#if game !== undefined}
            <button class="chip" onclick={() => quick({ game: game ?? null })}><Icon name="gamepad" size={12} />{game ?? t('game.desktop')}</button>
          {/if}
        </div>
      </div>
      <ul class="list">
        {#each available as shard (shard.id)}
          {@const inside = bestof.items.includes(shard.id)}
          <li>
            <button class="row" class:inside onclick={() => (inside ? removeAt(bestof.items.indexOf(shard.id)) : add([shard.id]))}>
              <GameIcon game={shard.game} size={30} />
              <span class="row-text">
                <strong>{displayTitle(shard)}</strong>
                <span>{shard.game ?? t('game.desktop')} · {formatDuration(clipLength(shard))}</span>
              </span>
              {#if shard.favorite}<Icon name="star" size={12} fill />{/if}
              <span class="add" aria-label={t(inside ? 'bestof.remove' : 'bestof.add')}><Icon name={inside ? 'check' : 'plus'} size={14} stroke={2.4} /></span>
            </button>
          </li>
        {:else}
          <li class="none">{t('bestof.noShards')}</li>
        {/each}
      </ul>
    </aside>

    <section class="stage">
      <div class="preview-box" data-tour="preview">
        <BestOfPreview {shards} {title} transition={bestof.transition} music={bestof.music} vertical={bestof.vertical} />
      </div>
      <div class="strip-head">
        <strong>{t('bestof.montage')}</strong>
        <span>{t('bestof.clips', { count: shards.length })} · {formatDuration(total)}</span>
        {#if shards.length}
          <button class="link" onclick={() => (bestof.items = [])}>{t('bestof.clear')}</button>
        {/if}
      </div>
      <div class="strip" data-tour="strip">
        {#each shards as shard, i (shard.id)}
          <div
            class="card"
            class:dragging={dragging === i}
            class:over={over === i && dragging !== i}
            draggable="true"
            ondragstart={dragstart(i)}
            ondragover={dragover(i)}
            ondrop={drop(i)}
            ondragend={() => (dragging = over = null)}
            animate:flip={{ duration: 220 * motion }}
            role="listitem"
          >
            <span class="order">{i + 1}</span>
            <GameIcon game={shard.game} size={34} />
            <strong>{displayTitle(shard)}</strong>
            <span class="length">{formatDuration(lengths[i] ?? 0)}</span>
            <div class="card-actions">
              <button onclick={() => edit(shard.id)} aria-label={t('bestof.edit')} use:tooltip={{ text: t('bestof.edit'), side: 'top' }}><Icon name="pencil" size={13} /></button>
              <button onclick={() => removeAt(i)} aria-label={t('bestof.remove')} use:tooltip={{ text: t('bestof.remove'), side: 'top' }}><Icon name="x" size={13} /></button>
            </div>
          </div>
        {:else}
          <div class="strip-empty" in:fly={{ y: 6, duration: 200 * motion }}>
            <Icon name="film" size={18} />
            <span>{t('bestof.empty')}</span>
          </div>
        {/each}
      </div>
    </section>

    <aside class="panel settings">
      <div class="group" data-tour="format">
        <span class="label">{t('bestof.format')}</span>
        <Segmented label={t('bestof.format')} options={formats} value={bestof.vertical ? 'vertical' : 'wide'} onchange={(v) => (bestof.vertical = v === 'vertical')} />
        <span class="label">{t('bestof.transition')}</span>
        <Segmented label={t('bestof.transition')} options={transitions} value={bestof.transition} onchange={(v) => (bestof.transition = v)} />
      </div>
      <div class="group">
        <div class="line">
          <span class="label">{t('bestof.titleCard')}</span>
          <Toggle label={t('bestof.titleCard')} checked={bestof.titleOn} onchange={(v) => (bestof.titleOn = v)} />
        </div>
        {#if bestof.titleOn}
          <input class="text" type="text" maxlength="60" bind:value={bestof.title} placeholder={defaultTitle} aria-label={t('bestof.titleCard')} />
        {/if}
      </div>
      <div class="group" data-tour="music">
        <span class="label">{t('bestof.music')}</span>
        {#if bestof.music}
          <div class="song">
            <Icon name="volume" size={14} />
            <span class="song-name" title={bestof.music.path}>{bestof.music.name}</span>
            <button onclick={() => (bestof.music = null)} aria-label={t('bestof.musicRemove')} use:tooltip={{ text: t('bestof.musicRemove'), side: 'top' }}><Icon name="x" size={13} /></button>
          </div>
          <label class="slider">
            <span>{t('bestof.musicVolume')}</span>
            <input type="range" min="0" max="1" step="0.05" bind:value={bestof.music.volume} style:--fill="{bestof.music.volume * 100}%" />
            <span class="value">{Math.round(bestof.music.volume * 100)} %</span>
          </label>
          <label class="slider">
            <span>{t('bestof.musicStart')}</span>
            <input type="range" min="0" max="240" step="1" bind:value={bestof.music.offset} style:--fill="{(bestof.music.offset / 240) * 100}%" />
            <span class="value">{formatDuration(bestof.music.offset)}</span>
          </label>
          <div class="line">
            <span class="hint">{t('bestof.musicDuck')}</span>
            <Toggle label={t('bestof.musicDuck')} checked={bestof.music.duck} onchange={(v) => bestof.music && (bestof.music.duck = v)} />
          </div>
          <button class="chip wide" onclick={pickMusic}><Icon name="refresh" size={12} />{t('bestof.musicChange')}</button>
        {:else}
          <button class="chip wide" onclick={pickMusic}><Icon name="plus" size={12} />{t('bestof.musicPick')}</button>
          <span class="hint">{t('bestof.musicHint')}</span>
        {/if}
      </div>
      <div class="group">
        <div class="line">
          <span class="label">{t('export.watermark')}</span>
          <Toggle label={t('export.watermark')} checked={settings.value.watermark} onchange={(v) => settings.update({ watermark: v }).catch(() => {})} />
        </div>
      </div>
    </aside>
  </div>
</div>

{#if exporting}
  <BestOfExport composition={composition(shards, defaultTitle)} duration={total} onclose={() => (exporting = false)} />
{/if}
{#if guiding}
  <Tour {steps} onclose={closeGuide} />
{/if}

<style>
  .bestof {
    display: flex;
    flex-direction: column;
    gap: 14px;
    height: 100%;
    padding: 6px 20px 20px;
    container-type: inline-size;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .heading {
    flex: 1;
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 700;
  }
  .heading p {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--text-3);
  }
  .layout {
    display: grid;
    grid-template-columns: 280px minmax(0, 1fr) 270px;
    gap: 14px;
    flex: 1;
    min-height: 0;
  }
  @container (max-width: 1080px) {
    .layout {
      grid-template-columns: 250px minmax(0, 1fr);
    }
    .settings {
      grid-column: 1 / -1;
      flex-direction: row !important;
      flex-wrap: wrap;
    }
    .settings .group {
      flex: 1 1 220px;
    }
  }
  .panel {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
    padding: 14px;
    border-radius: var(--radius-lg);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .search {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 34px;
    padding: 0 10px;
    border-radius: 8px;
    color: var(--text-3);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: none;
    background: none;
    color: var(--text);
    font-size: 13px;
    outline: none;
  }
  .label {
    font-size: 11.5px;
    font-weight: 700;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--text-3);
  }
  .quick {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .quick-buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .chip:hover {
    color: var(--text);
    background: var(--surface-4);
  }
  .chip.wide {
    justify-content: center;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-height: 0;
    margin: 0 -6px;
    padding: 0 6px;
    overflow-y: auto;
    list-style: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 7px 8px;
    border-radius: 9px;
    text-align: left;
    color: #ffc75a;
    transition: background var(--fast);
  }
  .row:hover {
    background: var(--surface-2);
  }
  .row-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    flex: 1;
    min-width: 0;
    color: var(--text);
  }
  .row-text strong {
    overflow: hidden;
    font-size: 12.5px;
    font-weight: 600;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .row-text span {
    font-size: 11.5px;
    color: var(--text-3);
  }
  .add {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex: none;
    border-radius: 7px;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .row.inside .add {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .none {
    padding: 20px 8px;
    font-size: 12.5px;
    text-align: center;
    color: var(--text-3);
  }
  .stage {
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-width: 0;
    min-height: 0;
  }
  .preview-box {
    flex: 1;
    min-height: 220px;
    padding: 12px;
    border-radius: var(--radius-lg);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .strip-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
    font-size: 12.5px;
    color: var(--text-3);
  }
  .strip-head strong {
    font-size: 13.5px;
    color: var(--text);
  }
  .link {
    margin-left: auto;
    font-size: 12px;
    color: var(--text-3);
  }
  .link:hover {
    color: var(--text);
  }
  .strip {
    display: flex;
    gap: 10px;
    min-height: 118px;
    padding: 10px;
    overflow-x: auto;
    border-radius: var(--radius-lg);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 148px;
    flex: none;
    padding: 10px;
    border-radius: 11px;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    cursor: grab;
    transition:
      box-shadow var(--fast),
      opacity var(--fast);
  }
  .card.dragging {
    opacity: 0.4;
  }
  .card.over {
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .card strong {
    overflow: hidden;
    font-size: 12.5px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .order {
    position: absolute;
    top: 8px;
    right: 10px;
    font-size: 11px;
    font-weight: 700;
    color: var(--text-3);
  }
  .length {
    font-size: 11.5px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .card-actions {
    position: absolute;
    right: 8px;
    bottom: 8px;
    display: flex;
    gap: 4px;
    opacity: 0;
    transition: opacity var(--fast);
  }
  .card:hover .card-actions,
  .card:focus-within .card-actions {
    opacity: 1;
  }
  .card-actions button {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .card-actions button:hover {
    color: var(--text);
    background: var(--surface-4);
  }
  .strip-empty {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
    flex: 1;
    font-size: 13px;
    color: var(--text-3);
  }
  .settings {
    overflow-y: auto;
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
  }
  .group:last-child {
    border-bottom: none;
  }
  .line {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }
  .hint {
    font-size: 12px;
    line-height: 1.4;
    color: var(--text-3);
  }
  .text {
    height: 32px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: 13px;
    color: var(--text);
    background: var(--surface-2);
    border: none;
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .text:focus {
    outline: none;
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .song {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-radius: 8px;
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, var(--surface-2));
  }
  .song-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    font-size: 12.5px;
    font-weight: 600;
    white-space: nowrap;
    text-overflow: ellipsis;
    color: var(--text);
  }
  .song button {
    color: var(--text-3);
  }
  .slider {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 4px 8px;
    font-size: 12px;
    color: var(--text-2);
  }
  .slider input {
    grid-column: 1 / -1;
    appearance: none;
    height: 3px;
    border-radius: 3px;
    background: linear-gradient(to right, var(--accent) var(--fill), var(--surface-3) var(--fill));
  }
  .slider input::-webkit-slider-thumb {
    appearance: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
  }
  .slider .value {
    grid-row: 1;
    grid-column: 2;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .icon-button {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 9px;
    color: var(--text-2);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .icon-button:hover {
    color: var(--text);
  }
  .button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 36px;
    padding: 0 16px;
    border-radius: 9px;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
  }
  .button.primary {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .button.primary:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .button:disabled {
    opacity: 0.45;
    cursor: default;
  }
</style>
