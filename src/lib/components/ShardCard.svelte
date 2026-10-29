<script lang="ts">
  import Icon from './Icon.svelte';
  import GameIcon from './GameIcon.svelte';
  import RenameInput from './RenameInput.svelte';
  import { mediaSrc, type Shard } from '$lib/api';
  import { gameTint } from '$lib/games';
  import { formatAge, formatDuration, formatSize, t } from '$lib/i18n.svelte';
  import * as actions from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { openMenu, ui } from '$lib/ui.svelte';

  let {
    shard,
    fresh = false,
    selected = false,
    selecting = false,
    onselect,
  }: {
    shard: Shard;
    fresh?: boolean;
    selected?: boolean;
    /** Some shard is selected: a click selects instead of playing. */
    selecting?: boolean;
    /** Given, the card can be selected (shift = extend the range). */
    onselect?: (shard: Shard, shift: boolean) => void;
  } = $props();

  function onThumbClick(e: MouseEvent) {
    if (onselect && (selecting || e.ctrlKey || e.shiftKey)) onselect(shard, e.shiftKey);
    else actions.play(shard);
  }

  const tint = $derived(gameTint(shard.game));
  const title = $derived(actions.displayTitle(shard));
  // While the player is open, it hosts the title editor.
  const renaming = $derived(ui.renaming === shard.id && !ui.playing);

  // Thumbnail: the video itself, loaded once the card scrolls into view. Hovering plays a preview.
  let card: HTMLElement;
  let video = $state<HTMLVideoElement>();
  let inView = $state(false);
  let ready = $state(false);
  let previewing = $state(false);
  let hoverTimer: ReturnType<typeof setTimeout> | undefined;
  const src = $derived(shard.path && inView ? mediaSrc(shard.path) : null);
  // Some files (fragmented MP4) don't state their duration in the header: the video knows it.
  let mediaDuration = $state(0);
  const duration = $derived(shard.duration || mediaDuration);
  const posterTime = $derived(Math.min(1, (duration || 2) / 2));

  $effect(() => {
    const io = new IntersectionObserver(([entry]) => {
      if (entry.isIntersecting) {
        inView = true;
        io.disconnect();
      }
    }, { rootMargin: '200px' });
    io.observe(card);
    return () => io.disconnect();
  });

  function startPreview() {
    if (!video || !ready) return;
    hoverTimer = setTimeout(() => {
      if (!video) return;
      video.currentTime = 0;
      video.play().then(() => (previewing = true), () => {});
    }, 350);
  }
  function stopPreview() {
    clearTimeout(hoverTimer);
    if (!video || !previewing) return;
    video.pause();
    video.currentTime = posterTime;
    previewing = false;
  }

  function onkeydown(e: KeyboardEvent) {
    if (renaming) return;
    if (e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey)) {
      e.preventDefault();
      const r = card.getBoundingClientRect();
      openMenu(r.left + 24, r.top + 24, actions.shardMenu(shard));
      return;
    }
    // In selection mode, Delete applies to the whole selection (handled by the Glacier).
    if (selecting && e.key === 'Delete') return;
    actions.handleShardKey(e, shard);
  }

  function oncontextmenu(e: MouseEvent) {
    e.preventDefault();
    openMenu(e.clientX, e.clientY, actions.shardMenu(shard));
  }

  function openMore(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.right - 4, r.bottom + 6, actions.shardMenu(shard));
  }

</script>

<!-- Shortcuts and right-click from the focused thumbnail or buttons bubble up to the card. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<article bind:this={card} class="card" class:renaming class:previewing class:fresh class:selected {onkeydown} {oncontextmenu}>
  <button
    class="thumb"
    style:--a={tint[0]}
    style:--b={tint[1]}
    onclick={onThumbClick}
    onmouseenter={startPreview}
    onmouseleave={stopPreview}
    aria-label="{t('shard.play')} · {title}"
  >
    <span class="art"></span>
    {#if src}
      <video
        bind:this={video}
        class:ready
        src="{src}#t={posterTime}"
        muted
        preload="metadata"
        playsinline
        onloadedmetadata={(e) => {
          const d = e.currentTarget.duration;
          if (Number.isFinite(d)) mediaDuration = d;
        }}
        onloadeddata={() => (ready = true)}
        onended={stopPreview}
      ></video>
    {/if}
    <span class="shade"></span>
    <span class="play"><Icon name="play" size={18} fill /></span>
    {#if duration > 0}<span class="duration">{formatDuration(duration)}</span>{/if}
  </button>

  {#if onselect}
    <button
      class="check"
      class:on={selected}
      class:visible={selecting}
      onclick={(e) => onselect(shard, e.shiftKey)}
      role="checkbox"
      aria-checked={selected}
      aria-label={t('selection.select')}
    >
      <Icon name="check" size={13} stroke={3} />
    </button>
  {/if}

  <div class="actions" class:hidden={selecting}>
    <button class="cut" onclick={() => actions.edit(shard)} aria-label={t('editor.edit')} use:tooltip={{ text: `${t('editor.edit')} · E`, side: 'bottom' }}>
      <Icon name="scissors" size={15} />
    </button>
    <button class="more" onclick={openMore} aria-label={t('shard.more')} aria-haspopup="menu">
      <Icon name="more" size={16} stroke={2.4} />
    </button>
    <button
      class="fav"
      class:on={shard.favorite}
      onclick={() => actions.toggleFavorite(shard)}
      aria-pressed={shard.favorite}
      aria-label={t(shard.favorite ? 'shard.unfavorite' : 'shard.favorite')}
      use:tooltip={{ text: t(shard.favorite ? 'shard.unfavorite' : 'shard.favorite'), side: 'bottom' }}
    >
      <Icon name="star" size={15} fill={shard.favorite} />
    </button>
  </div>

  <div class="info">
    {#if renaming}
      <div class="title-row">
        <RenameInput {shard} ondone={() => card.querySelector<HTMLElement>('.thumb')?.focus()} />
      </div>
    {:else}
      <div class="title-row">
        <span class="title" class:untitled={!shard.title} ondblclick={() => actions.rename(shard)} role="presentation">{title}</span>
        <button class="edit" onclick={() => actions.rename(shard)} aria-label={t('shard.rename')} use:tooltip={{ text: `${t('shard.rename')} · F2`, side: 'top' }}>
          <Icon name="pencil" size={13} />
        </button>
      </div>
    {/if}
    <div class="meta">
      <GameIcon game={shard.game} size={15} />
      {#if shard.title}<span class="game">{actions.gameName(shard)}</span><span class="sep">·</span>{/if}
      <span>{formatAge(shard.createdAt)}</span>
      <span class="sep">·</span>
      <span>{formatSize(shard.size)}</span>
    </div>
  </div>
</article>

<style>
  .card {
    position: relative;
    display: flex;
    flex-direction: column;
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
    transition:
      background var(--normal),
      border-color var(--normal),
      transform var(--slow) var(--ease-out),
      box-shadow var(--slow) var(--ease-out);
  }
  .card:hover {
    background: var(--surface-2);
    border-color: var(--border-strong);
    transform: translateY(-2px);
    box-shadow: 0 14px 30px -18px rgba(0, 0, 0, 0.9);
  }
  /* Just captured: a ring that fades, so the eye finds it. */
  .card.fresh {
    animation: fresh 2.4s var(--ease-out);
  }
  @keyframes fresh {
    0%,
    40% {
      border-color: var(--accent);
      box-shadow:
        0 0 0 3px var(--accent-soft),
        0 14px 40px -16px var(--accent);
    }
  }
  .card:has(.thumb:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .thumb {
    position: relative;
    display: block;
    aspect-ratio: 16 / 9;
    margin: 5px 5px 0;
    border-radius: calc(var(--radius-lg) - 5px);
    overflow: hidden;
    background: var(--bg);
    isolation: isolate;
  }
  .thumb:focus-visible {
    outline: none;
  }

  /* Placeholder art (demo data, or before the video loads): the game's tint through ice facets. */
  .art,
  video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    transition: transform 600ms var(--ease-out);
  }
  .art {
    background:
      linear-gradient(115deg, transparent 47%, rgba(255, 255, 255, 0.06) 47.2%, transparent 62%),
      linear-gradient(62deg, transparent 64%, rgba(255, 255, 255, 0.04) 64.2%, transparent 80%),
      radial-gradient(110% 90% at 88% 8%, color-mix(in srgb, var(--b) 55%, transparent), transparent 60%),
      radial-gradient(100% 100% at 8% 100%, color-mix(in srgb, var(--a) 60%, transparent), transparent 65%),
      linear-gradient(135deg, var(--surface-2), var(--bg));
  }
  video {
    object-fit: cover;
    opacity: 0;
    transition:
      opacity var(--slow),
      transform 600ms var(--ease-out);
  }
  video.ready {
    opacity: 1;
  }
  .card:hover :is(.art, video) {
    transform: scale(1.035);
  }

  .shade {
    position: absolute;
    inset: 0;
    background: linear-gradient(to top, rgba(0, 0, 0, 0.45), transparent 40%);
    pointer-events: none;
  }

  .play {
    position: absolute;
    left: 50%;
    top: 50%;
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    padding-left: 3px;
    border-radius: 50%;
    color: #0a0b0d;
    background: rgba(255, 255, 255, 0.92);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
    opacity: 0;
    transform: translate(-50%, -50%) scale(0.85);
    transition:
      opacity var(--normal),
      transform var(--slow) var(--ease-out);
  }
  .card:hover .play,
  .thumb:focus-visible .play {
    opacity: 1;
    transform: translate(-50%, -50%) scale(1);
  }
  .previewing .play {
    opacity: 0 !important;
  }

  .duration {
    position: absolute;
    right: 8px;
    bottom: 8px;
    padding: 2px 6px;
    border-radius: 5px;
    font-size: 11.5px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: #fff;
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(6px);
  }

  /* Top-right: the star keeps its corner; "edit" and "more" fade in to its left on hover. */
  .actions {
    position: absolute;
    top: 13px;
    right: 13px;
    display: flex;
    gap: 4px;
    transition: opacity var(--normal);
  }
  .actions.hidden {
    opacity: 0;
    pointer-events: none;
  }
  .actions button {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 7px;
    color: rgba(255, 255, 255, 0.9);
    background: rgba(10, 11, 13, 0.55);
    backdrop-filter: blur(8px);
    opacity: 0;
    transition:
      opacity var(--normal) var(--ease-out),
      transform var(--slow) var(--ease-out),
      background var(--fast),
      color var(--fast);
  }
  .actions .more,
  .actions .cut {
    transform: translateX(6px) scale(0.9);
  }
  .actions .fav {
    transform: scale(0.85);
  }
  .actions button:hover {
    background: rgba(10, 11, 13, 0.8);
    color: #fff;
  }
  .card:hover .actions button,
  .card:focus-within .actions button,
  .fav.on {
    opacity: 1;
    transform: none;
  }
  .fav.on {
    color: #f5c84c;
  }
  .fav:active :global(svg) {
    transform: scale(0.8);
  }
  .fav :global(svg) {
    transition: transform var(--fast) var(--ease-out);
  }

  /* Top-left: the selection box, shown on hover and whenever something is selected. */
  .check {
    position: absolute;
    top: 13px;
    left: 13px;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 7px;
    color: transparent;
    background: rgba(10, 11, 13, 0.5);
    box-shadow: inset 0 0 0 1.5px rgba(255, 255, 255, 0.75);
    backdrop-filter: blur(8px);
    opacity: 0;
    transform: scale(0.85);
    transition:
      opacity var(--normal) var(--ease-out),
      transform var(--slow) var(--ease-out),
      background var(--fast),
      color var(--fast),
      box-shadow var(--fast);
  }
  .card:hover .check,
  .check.visible,
  .check:focus-visible {
    opacity: 1;
    transform: none;
  }
  .check:hover {
    background: rgba(10, 11, 13, 0.75);
  }
  .check.on {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }

  .card.selected {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .card.selected .thumb {
    transform: scale(0.94);
  }
  .thumb {
    transition: transform var(--slow) var(--ease-out);
  }

  .info {
    padding: 10px 12px 12px;
    min-width: 0;
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 24px;
  }
  .title {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: text;
  }
  .title.untitled {
    color: var(--text-2);
  }
  .edit {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    flex: none;
    border-radius: var(--radius-sm);
    color: var(--text-3);
    opacity: 0;
    transition:
      opacity var(--fast),
      background var(--fast),
      color var(--fast);
  }
  .card:hover .edit,
  .edit:focus-visible {
    opacity: 1;
  }
  .edit:hover {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }

  .meta {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 4px;
    font-size: 12.5px;
    color: var(--text-3);
    white-space: nowrap;
    overflow: hidden;
  }
  .meta .game {
    color: var(--text-2);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sep {
    opacity: 0.6;
  }
</style>
