<script lang="ts">
  import Icon from './Icon.svelte';
  import { games, type Shard } from '$lib/demo';
  import { t, formatAge, formatDuration, formatSize } from '$lib/i18n.svelte';

  let { shard }: { shard: Shard } = $props();

  const game = $derived(games[shard.game]);
  const gameName = $derived(shard.game === 'desktop' ? t('game.desktop') : game.name);
</script>

<button class="card">
  <div class="thumb" style:--a={game.tint[0]} style:--b={game.tint[1]}>
    <span class="game">{gameName}</span>
    {#if shard.favorite}
      <span class="fav"><Icon name="star" size={14} fill /></span>
    {/if}
    <span class="play"><Icon name="play" size={22} fill /></span>
    <span class="duration">{formatDuration(shard.duration)}</span>
  </div>
  <div class="info">
    <div class="title">{shard.title ?? gameName}</div>
    <div class="sub">{formatAge(shard.ageMinutes)} · {formatSize(shard.sizeMb)}</div>
  </div>
</button>

<style>
  .card {
    display: flex;
    flex-direction: column;
    text-align: left;
    border-radius: var(--radius);
    background: var(--nuit-2);
    border: 1px solid var(--line);
    overflow: hidden;
    transition:
      transform 0.25s var(--ease),
      border-color 0.25s,
      box-shadow 0.25s;
  }
  .card:hover {
    transform: translateY(-3px);
    border-color: rgba(79, 195, 247, 0.5);
    box-shadow: 0 12px 40px -12px rgba(79, 195, 247, 0.35);
  }

  .thumb {
    position: relative;
    aspect-ratio: 16 / 9;
    background:
      radial-gradient(120% 90% at 85% 10%, color-mix(in srgb, var(--b) 70%, transparent), transparent 60%),
      radial-gradient(100% 100% at 10% 100%, color-mix(in srgb, var(--a) 75%, transparent), transparent 65%),
      linear-gradient(135deg, #10172b, #0b1020);
    overflow: hidden;
  }
  /* ice facets */
  .thumb::before {
    content: '';
    position: absolute;
    inset: 0;
    background:
      linear-gradient(115deg, transparent 48%, rgba(255, 255, 255, 0.07) 48.2%, transparent 62%),
      linear-gradient(60deg, transparent 65%, rgba(255, 255, 255, 0.05) 65.2%, transparent 80%);
  }
  .thumb::after {
    content: '';
    position: absolute;
    inset: 0;
    background: linear-gradient(to top, rgba(11, 16, 32, 0.75), transparent 45%);
  }

  .game,
  .duration {
    position: absolute;
    z-index: 1;
    font-size: 12px;
    font-weight: 600;
    padding: 3px 8px;
    border-radius: 7px;
    background: rgba(11, 16, 32, 0.6);
    backdrop-filter: blur(6px);
  }
  .game {
    top: 10px;
    left: 10px;
  }
  .duration {
    bottom: 10px;
    right: 10px;
  }
  .fav {
    position: absolute;
    z-index: 1;
    top: 10px;
    right: 10px;
    color: var(--glace);
    filter: drop-shadow(0 0 6px rgba(184, 236, 251, 0.7));
  }

  .play {
    position: absolute;
    z-index: 1;
    left: 50%;
    top: 50%;
    width: 52px;
    height: 52px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    color: var(--nuit);
    background: var(--givre);
    box-shadow: 0 0 30px rgba(79, 195, 247, 0.6);
    opacity: 0;
    transform: translate(-50%, -50%) scale(0.8);
    transition:
      opacity 0.2s,
      transform 0.25s var(--ease);
  }
  .card:hover .play {
    opacity: 1;
    transform: translate(-50%, -50%) scale(1);
  }

  .info {
    padding: 12px 14px 14px;
  }
  .title {
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub {
    margin-top: 4px;
    font-size: 13px;
    color: var(--gris);
  }
</style>
