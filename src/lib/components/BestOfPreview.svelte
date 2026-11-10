<!-- Plays the best-of as it will be exported: the title card, then each clip as edited (cut
     passages skipped, slow zones slowed, muted ranges silent), the dips to black or white between
     them, and the song from where it starts. One video element goes from clip to clip; the bar
     below shows each part, a click goes there. Captions, blurs and the music's ducking are only in
     the export. -->
<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import GameIcon from './GameIcon.svelte';
  import { mediaSrc, native, type Shard } from '$lib/api';
  import { TITLE_SECONDS, clipOf, type Music, type Transition } from '$lib/bestof.svelte';
  import { cutAt, muteAt, playedAt, playedLength, rateAt, sourceAt } from '$lib/fx.svelte';
  import { gameTint } from '$lib/games';
  import { t } from '$lib/i18n.svelte';
  import { displayTitle } from '$lib/shard-actions';

  let {
    shards,
    title,
    transition,
    music,
    vertical,
  }: { shards: Shard[]; title: string | null; transition: Transition; music: Music | null; vertical: boolean } = $props();

  type Segment =
    | { kind: 'title'; at: number; length: number }
    | { kind: 'clip'; at: number; length: number; shard: Shard; clip: ReturnType<typeof clipOf> };

  const segments = $derived.by(() => {
    const list: Segment[] = [];
    let at = 0;
    if (title) {
      list.push({ kind: 'title', at, length: TITLE_SECONDS });
      at += TITLE_SECONDS;
    }
    for (const shard of shards) {
      const clip = clipOf(shard);
      const length = playedLength(clip.start, clip.end, clip.fx);
      if (length <= 0.05) continue;
      list.push({ kind: 'clip', at, length, shard, clip });
      at += length;
    }
    return list;
  });
  const total = $derived(segments.reduce((sum, s) => sum + s.length, 0));

  let video = $state<HTMLVideoElement>();
  let audio = $state<HTMLAudioElement>();
  let playing = $state(false);
  let index = $state(0);
  let time = $state(0);
  const segment = $derived(segments[index] as Segment | undefined);
  const real = $derived(native && segment?.kind === 'clip' && !!segment.shard.path);

  /** The dip over the picture at `time`: around each join, and out at the end for a fade. */
  const dip = $derived.by(() => {
    if (transition === 'cut') return 0;
    const [out, back] = transition === 'fade' ? [0.3, 0.3] : [0.08, 0.3];
    let opacity = 0;
    for (const s of segments.slice(1)) {
      if (time >= s.at - out && time < s.at) opacity = Math.max(opacity, (time - (s.at - out)) / out);
      else if (time >= s.at && time < s.at + back) opacity = Math.max(opacity, 1 - (time - s.at) / back);
    }
    if (transition === 'fade' && time >= total - out) opacity = Math.max(opacity, Math.min(1, (time - (total - out)) / out));
    return Math.min(1, opacity);
  });

  /** Shows segment `i`, `offset` seconds into it. */
  function goTo(i: number, offset = 0) {
    if (i >= segments.length) {
      stop();
      return;
    }
    index = i;
    const s = segments[i];
    time = s.at + offset;
    if (s.kind === 'clip' && video && native && s.shard.path) {
      const src = mediaSrc(s.shard.path);
      const at = sourceAt(s.clip.start, s.clip.end, s.clip.fx, offset);
      const start = () => {
        if (!video) return;
        video.currentTime = at;
        if (playing) video.play().catch(() => {});
      };
      if (video.getAttribute('src') !== src) {
        video.setAttribute('src', src);
        video.addEventListener('loadedmetadata', start, { once: true });
        video.load();
      } else start();
    } else video?.pause();
  }

  function stop() {
    playing = false;
    video?.pause();
    audio?.pause();
    index = 0;
    time = 0;
    if (segments.length) goTo(0);
  }

  function toggle() {
    if (!segments.length) return;
    playing = !playing;
    if (playing) {
      if (time >= total - 0.05) goTo(0);
      else goTo(index, time - (segment?.at ?? 0));
      syncMusic();
    } else {
      video?.pause();
      audio?.pause();
    }
  }

  function syncMusic() {
    if (!audio || !music || !native) return;
    audio.volume = Math.min(1, music.volume);
    audio.currentTime = music.offset + time;
    if (playing) audio.play().catch(() => {});
  }

  function seekTo(at: number) {
    const target = Math.max(0, Math.min(total - 0.01, at));
    const i = Math.max(0, segments.findLastIndex((s) => s.at <= target));
    goTo(i, target - segments[i].at);
    syncMusic();
  }

  let bar: HTMLDivElement;
  function seekPointer(e: PointerEvent) {
    const r = bar.getBoundingClientRect();
    seekTo(((e.clientX - r.left) / r.width) * total);
  }

  // The clock: the title card and demo clips by the wall clock, real clips by their video.
  onMount(() => {
    let last = performance.now();
    let frame = requestAnimationFrame(function tick(now) {
      const dt = (now - last) / 1000;
      last = now;
      const s = segments[index];
      if (playing && s) {
        if (s.kind === 'title' || !real || !video) {
          time += dt;
          if (time >= s.at + s.length) goTo(index + 1);
        } else if (video.readyState >= 1) {
          const { start, end, fx, gains } = s.clip;
          let at = video.currentTime;
          const cut = cutAt(fx, at);
          if (cut) video.currentTime = at = Math.min(end, cut.end);
          const rate = rateAt(fx, at);
          if (video.playbackRate !== rate) {
            video.preservesPitch = false;
            video.playbackRate = rate;
          }
          const muted = muteAt(fx, at);
          video.volume = Math.max(0, Math.min(1, Math.max(gains.game * muted.game, gains.mic * muted.mic)));
          time = s.at + playedAt(start, end, fx, at);
          if (at >= end - 0.03 || video.ended) goTo(index + 1);
        }
      }
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });

  // A change of the best-of (order, title...) starts the preview over.
  $effect(() => {
    void segments;
    untrack(stop);
  });

  onDestroy(() => {
    video?.pause();
    audio?.pause();
  });

  const clock = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
  const tint = $derived(segment?.kind === 'clip' ? gameTint(segment.shard.game) : (['#1d2b3a', '#0b1220'] as [string, string]));
</script>

<div class="preview" class:vertical>
  <div class="screen" style:--a={tint[0]} style:--b={tint[1]}>
    <!-- svelte-ignore a11y_media_has_caption -->
    <video
      bind:this={video}
      class:shown={real}
      playsinline
      style:object-position={segment?.kind === 'clip' ? `${segment.clip.cropX * 100}% 50%` : undefined}
    ></video>
    {#if !segments.length}
      <div class="empty">
        <Icon name="film" size={26} />
        <span>{t('bestof.previewEmpty')}</span>
      </div>
    {:else if segment?.kind === 'title'}
      <div class="title-card"><span>{title}</span></div>
    {:else if segment?.kind === 'clip' && !real}
      <div class="placeholder">
        <GameIcon game={segment.shard.game} size={44} />
        <strong>{displayTitle(segment.shard)}</strong>
      </div>
    {/if}
    <div class="dip" style:background={transition === 'flash' ? '#fff' : '#000'} style:opacity={dip}></div>
  </div>

  <div class="controls">
    <button class="play" onclick={toggle} disabled={!segments.length} aria-label={t(playing ? 'bestof.pause' : 'bestof.play')}>
      <Icon name={playing ? 'pause' : 'play'} size={15} fill />
    </button>
    <span class="clock">{clock(time)} <span>/ {clock(Math.round(total))}</span></span>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="bar" bind:this={bar} onpointerdown={seekPointer}>
      {#each segments as s, i (i)}
        <span class="part" class:title={s.kind === 'title'} class:current={i === index} style:flex-grow={s.length}></span>
      {/each}
      <span class="head" style:left="{total ? (time / total) * 100 : 0}%"></span>
    </div>
  </div>
  {#if music && native}
    <audio bind:this={audio} src={mediaSrc(music.path)} preload="auto"></audio>
  {/if}
</div>

<style>
  .preview {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
    height: 100%;
  }
  .screen {
    position: relative;
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    border-radius: var(--radius-lg);
    overflow: hidden;
    background:
      radial-gradient(90% 120% at 85% 0%, color-mix(in srgb, var(--b) 40%, transparent), transparent 60%),
      radial-gradient(90% 120% at 10% 100%, color-mix(in srgb, var(--a) 45%, transparent), transparent 65%),
      #07080a;
    aspect-ratio: 16 / 9;
    max-width: 100%;
    margin: 0 auto;
  }
  .vertical .screen {
    aspect-ratio: 9 / 16;
  }
  video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    opacity: 0;
  }
  .vertical video {
    object-fit: cover;
  }
  video.shown {
    opacity: 1;
  }
  .title-card {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    padding: 0 10%;
    background: #000;
    font-family: 'Segoe UI', system-ui, sans-serif;
    font-weight: 700;
    font-size: clamp(18px, 4.2cqh, 46px);
    text-align: center;
    color: #fff;
    text-shadow: 0 0.06em 0.1em rgba(0, 0, 0, 0.5);
  }
  .placeholder,
  .empty {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 0 12%;
    text-align: center;
    color: var(--text-2);
    font-size: 13px;
  }
  .placeholder strong {
    font-size: 15px;
    color: var(--text);
  }
  .dip {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .play {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    flex: none;
    border-radius: 50%;
    color: #0a0b0d;
    background: #fff;
  }
  .play:disabled {
    opacity: 0.4;
  }
  .clock {
    width: 84px;
    flex: none;
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
  }
  .clock span {
    color: var(--text-3);
  }
  .bar {
    position: relative;
    display: flex;
    gap: 3px;
    flex: 1;
    height: 10px;
    cursor: pointer;
  }
  .part {
    flex-basis: 0;
    min-width: 4px;
    border-radius: 3px;
    background: var(--surface-3);
    transition: background var(--fast);
  }
  .part.title {
    background: color-mix(in srgb, #ffc75a 45%, var(--surface-3));
  }
  .part.current {
    background: color-mix(in srgb, var(--accent) 55%, var(--surface-3));
  }
  .head {
    position: absolute;
    top: -4px;
    bottom: -4px;
    width: 3px;
    margin-left: -1.5px;
    border-radius: 2px;
    background: #fff;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4);
    pointer-events: none;
  }
</style>
