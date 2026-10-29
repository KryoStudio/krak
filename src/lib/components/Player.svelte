<!-- Watching a shard, over the content area. ←/→ go through the shards on screen. -->
<script lang="ts">
  import { fade, fly, scale } from 'svelte/transition';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import GameIcon from './GameIcon.svelte';
  import RenameInput from './RenameInput.svelte';
  import Editor from './Editor.svelte';
  import { mediaSrc } from '$lib/api';
  import { gameTint } from '$lib/games';
  import { formatAge, formatSize, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import * as actions from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { ui } from '$lib/ui.svelte';
  import { view } from '$lib/view.svelte';
  import { appWindow } from '$lib/app-window.svelte';

  const shard = $derived(ui.playing ? library.find(ui.playing) : undefined);
  const index = $derived(shard ? view.shards.indexOf(shard) : -1);
  const tint = $derived(gameTint(shard?.game ?? null));
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);

  let video = $state<HTMLVideoElement>();

  // Krak minimized or sent to the tray: the clip (and the editor) stop playing.
  $effect(() => {
    if (!appWindow.shown) video?.pause();
  });

  // The shard was deleted or filtered away: close.
  $effect(() => {
    if (ui.playing && !shard) ui.playing = null;
    if (!ui.playing) ui.editing = false;
  });

  function go(step: number) {
    const next = view.shards[index + step];
    if (next) ui.playing = next.id;
  }

  function close() {
    ui.playing = null;
  }

  function onkeydown(e: KeyboardEvent) {
    if (!shard || ui.renaming || e.defaultPrevented) return;
    const tag = (e.target as HTMLElement).tagName;
    if (tag === 'INPUT') return;
    if (e.key === 'Escape') {
      if (ui.editing) ui.editing = false;
      else close();
    } else if (ui.editing && [' ', 'ArrowLeft', 'ArrowRight'].includes(e.key)) return; // the editor's keys
    else if (e.key === 'ArrowLeft' && tag !== 'VIDEO') go(-1);
    else if (e.key === 'ArrowRight' && tag !== 'VIDEO') go(1);
    else if (e.key === ' ' && video && tag !== 'VIDEO' && tag !== 'BUTTON') video.paused ? video.play() : video.pause();
    else if (!actions.handleShardKey(e, shard)) return;
    e.preventDefault();
  }
</script>

<svelte:window {onkeydown} />

{#if shard}
  <div class="player" transition:fade={{ duration: 160 * motion }}>
    <header>
      <button class="back" onclick={close} use:tooltip={{ text: `${t('common.close')} · ${t('key.escape')}`, side: 'bottom' }}>
        <Icon name="arrowLeft" size={16} />{t('player.back')}
      </button>
      <GameIcon game={shard.game} size={30} />
      <div class="heading">
        {#if ui.renaming === shard.id}
          <RenameInput {shard} size={15} />
        {:else}
          <h2>{actions.displayTitle(shard)}</h2>
        {/if}
        <p>{actions.gameName(shard)} · {formatAge(shard.createdAt)} · {formatSize(shard.size)}</p>
      </div>
      <div class="tools">
        <button class="pill" class:on={ui.editing} onclick={() => (ui.editing = !ui.editing)} aria-pressed={ui.editing}>
          <Icon name={ui.editing ? 'check' : 'scissors'} size={15} />
          {t(ui.editing ? 'editor.done' : 'editor.edit')}
        </button>
        <!-- The main actions say what they do; the rest are icons with tooltips. -->
        <button class="pill" onclick={() => actions.copy(shard)} use:tooltip={{ text: t('toast.copiedBody'), side: 'bottom' }}>
          <Icon name="copy" size={15} />{t('shard.copy')}
        </button>
        <span class="divider"></span>
        <button
          class="icon-btn"
          class:fav={shard.favorite}
          onclick={() => actions.toggleFavorite(shard)}
          aria-pressed={shard.favorite}
          aria-label={t(shard.favorite ? 'shard.unfavorite' : 'shard.favorite')}
          use:tooltip={{ text: t(shard.favorite ? 'shard.unfavorite' : 'shard.favorite'), side: 'bottom' }}
        >
          <Icon name="star" size={17} fill={shard.favorite} />
        </button>
        <button class="icon-btn" onclick={() => actions.rename(shard)} aria-label={t('shard.rename')} use:tooltip={{ text: t('shard.rename'), side: 'bottom' }}>
          <Icon name="pencil" size={16} />
        </button>
        <button class="icon-btn" onclick={() => actions.reveal(shard)} aria-label={t('shard.reveal')} use:tooltip={{ text: t('shard.reveal'), side: 'bottom' }}>
          <Icon name="folder" size={16} />
        </button>
        <span class="divider"></span>
        <button class="icon-btn danger" onclick={() => actions.remove(shard)} aria-label={t('shard.delete')} use:tooltip={{ text: t('shard.delete'), side: 'bottom' }}>
          <Icon name="trash" size={16} />
        </button>
      </div>
    </header>

    <div class="stage" class:editing={ui.editing}>
      <button class="nav prev" disabled={index <= 0} onclick={() => go(-1)} aria-label={t('player.prev')}>
        <Icon name="chevronLeft" size={22} />
      </button>

      {#key shard.id}
        <div class="screen" in:scale={{ start: 0.985, duration: 240 * motion, opacity: 0 }}>
          {#if shard.path}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video bind:this={video} src={mediaSrc(shard.path)} controls={!ui.editing} autoplay></video>
          {:else}
            <div class="placeholder" style:--a={tint[0]} style:--b={tint[1]}>
              <span>{t('player.demo')}</span>
            </div>
          {/if}
        </div>
      {/key}

      <button class="nav next" disabled={index < 0 || index >= view.shards.length - 1} onclick={() => go(1)} aria-label={t('player.next')}>
        <Icon name="chevronRight" size={22} />
      </button>
    </div>

    {#if ui.editing}
      <div class="tools-panel" transition:fly={{ y: 16, duration: 260 * motion }}>
        <Editor {shard} {video} />
      </div>
    {/if}
  </div>
{/if}

<style>
  .player {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: color-mix(in srgb, var(--bg) 96%, transparent);
    backdrop-filter: blur(12px);
  }

  header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 4px 20px 12px 16px;
  }
  .heading {
    flex: 1;
    min-width: 0;
    max-width: 560px;
    margin-right: auto;
  }
  h2 {
    font-size: 15px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .heading p {
    font-size: 12.5px;
    color: var(--text-3);
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .divider {
    width: 1px;
    height: 18px;
    margin: 0 6px;
    background: var(--border-strong);
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    color: var(--text-2);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .icon-btn:hover {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  .icon-btn.fav {
    color: #f5c84c;
  }
  .icon-btn.danger:hover {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }

  .stage {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 12px 24px;
  }
  .screen {
    flex: 1;
    min-width: 0;
    height: 100%;
    display: grid;
    place-items: center;
  }
  video,
  .placeholder {
    max-width: 100%;
    max-height: 100%;
    aspect-ratio: 16 / 9;
    border-radius: var(--radius-lg);
    background: #000;
    box-shadow: 0 30px 80px -30px rgba(0, 0, 0, 0.9);
  }
  video {
    width: 100%;
    outline: none;
  }
  .placeholder {
    width: 100%;
    display: grid;
    place-items: center;
    background:
      radial-gradient(110% 90% at 88% 8%, color-mix(in srgb, var(--b) 50%, transparent), transparent 60%),
      radial-gradient(100% 100% at 8% 100%, color-mix(in srgb, var(--a) 55%, transparent), transparent 65%),
      var(--surface);
  }
  .placeholder span {
    padding: 8px 14px;
    border-radius: 999px;
    font-size: 13px;
    color: var(--text-2);
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(8px);
  }

  .stage.editing .nav {
    display: none;
  }
  .stage.editing {
    padding: 0 24px 12px;
  }
  .tools-panel {
    padding: 0 24px 16px;
  }

  /* Back to the page: a labeled button, so nobody has to hunt for a lone arrow. */
  .back {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 14px 0 10px;
    margin-right: 4px;
    border-radius: 9px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      background var(--fast),
      transform var(--fast) var(--ease-out);
  }
  .back:hover {
    background: var(--surface-3);
  }
  .back:active {
    transform: scale(0.97);
  }

  .pill {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 12px;
    margin-right: 4px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .pill:hover {
    background: var(--surface-3);
  }
  .pill.on {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }

  .nav {
    display: grid;
    place-items: center;
    width: 40px;
    height: 64px;
    flex: none;
    border-radius: var(--radius);
    color: var(--text-2);
    transition:
      background var(--fast),
      color var(--fast),
      opacity var(--fast);
  }
  .nav:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  .nav:disabled {
    opacity: 0.25;
  }
</style>
