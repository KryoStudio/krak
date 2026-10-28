<!-- Trim timeline: a filmstrip, two handles for the kept range and the playhead.
     Drag the handles or the playhead, click the strip to seek. -->
<script lang="ts">
  import { formatDuration, t } from '$lib/i18n.svelte';

  let {
    duration,
    start = $bindable(),
    end = $bindable(),
    current,
    src,
    tint,
    onseek,
  }: {
    duration: number;
    start: number;
    end: number;
    current: number;
    /** Video URL for the filmstrip; null shows the placeholder. */
    src: string | null;
    tint: [string, string];
    onseek: (time: number) => void;
  } = $props();

  const MIN = 0.5;
  const FRAMES = 12;

  let track: HTMLDivElement;
  let canvases: HTMLCanvasElement[] = $state([]);
  /** Frames painted so far: each one fades in as soon as it is ready. */
  let painted = $state(0);

  const pct = (time: number) => `${(time / (duration || 1)) * 100}%`;

  // Filmstrip: seek a hidden copy of the video and paint each frame. Painting a video into a
  // visible canvas needs no CORS, nothing is read back.
  $effect(() => {
    if (!src || !duration) return;
    let cancelled = false;
    painted = 0;
    const video = document.createElement('video');
    video.muted = true;
    video.preload = 'auto';
    video.src = src;
    (async () => {
      await new Promise((r) => video.addEventListener('loadeddata', r, { once: true }));
      for (let i = 0; i < FRAMES && !cancelled; i++) {
        video.currentTime = ((i + 0.5) / FRAMES) * duration;
        await new Promise((r) => video.addEventListener('seeked', r, { once: true }));
        const canvas = canvases[i];
        if (!canvas || cancelled) break;
        canvas.getContext('2d')?.drawImage(video, 0, 0, canvas.width, canvas.height);
        painted = i + 1;
      }
    })();
    return () => {
      cancelled = true;
      video.removeAttribute('src');
      video.load();
    };
  });

  function timeAt(clientX: number): number {
    const r = track.getBoundingClientRect();
    return Math.min(duration, Math.max(0, ((clientX - r.left) / r.width) * duration));
  }

  function drag(what: 'start' | 'end' | 'head') {
    return (e: PointerEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const el = e.currentTarget as HTMLElement;
      el.setPointerCapture(e.pointerId);
      const move = (ev: PointerEvent) => {
        const time = timeAt(ev.clientX);
        if (what === 'start') {
          start = Math.min(time, end - MIN);
          onseek(start);
        } else if (what === 'end') {
          end = Math.max(time, start + MIN);
          onseek(end);
        } else onseek(Math.min(end, Math.max(start, time)));
      };
      move(e);
      el.addEventListener('pointermove', move);
      el.addEventListener('pointerup', () => el.removeEventListener('pointermove', move), { once: true });
    };
  }

  function nudge(what: 'start' | 'end') {
    return (e: KeyboardEvent) => {
      const step = (e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0) * (e.shiftKey ? 1 : 0.1);
      if (!step) return;
      e.preventDefault();
      e.stopPropagation();
      if (what === 'start') start = Math.min(end - MIN, Math.max(0, start + step));
      else end = Math.max(start + MIN, Math.min(duration, end + step));
      onseek(what === 'start' ? start : end);
    };
  }
</script>

<div class="timeline">
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div class="track" bind:this={track} onpointerdown={drag('head')} role="presentation" style:--a={tint[0]} style:--b={tint[1]}>
    <div class="strip">
      {#each { length: FRAMES } as _, i (i)}
        <canvas bind:this={canvases[i]} class:on={i < painted} width="160" height="90"></canvas>
      {/each}
    </div>

    <div class="dim" style:left="0" style:width={pct(start)}></div>
    <div class="dim" style:left={pct(end)} style:right="0"></div>
    <div class="range" style:left={pct(start)} style:width={pct(end - start)}></div>

    <button
      class="handle start"
      style:left={pct(start)}
      onpointerdown={drag('start')}
      onkeydown={nudge('start')}
      aria-label="{t('editor.start')} · {formatDuration(start)}"
    ></button>
    <button
      class="handle end"
      style:left={pct(end)}
      onpointerdown={drag('end')}
      onkeydown={nudge('end')}
      aria-label="{t('editor.end')} · {formatDuration(end)}"
    ></button>

    <div class="head" style:left={pct(current)}></div>
  </div>
</div>

<style>
  .timeline {
    padding: 0 9px;
  }
  .track {
    position: relative;
    height: 54px;
    border-radius: 8px;
    cursor: pointer;
    touch-action: none;
  }

  .strip {
    position: absolute;
    inset: 0;
    display: flex;
    border-radius: inherit;
    overflow: hidden;
    background:
      radial-gradient(90% 120% at 85% 0%, color-mix(in srgb, var(--b) 45%, transparent), transparent 60%),
      radial-gradient(90% 120% at 10% 100%, color-mix(in srgb, var(--a) 50%, transparent), transparent 65%),
      var(--surface-2);
  }
  canvas {
    flex: 1;
    min-width: 0;
    height: 100%;
    object-fit: cover;
    opacity: 0;
    transition: opacity var(--slow);
  }
  canvas.on {
    opacity: 1;
  }

  .dim {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgba(10, 11, 13, 0.72);
    pointer-events: none;
  }
  .dim:first-of-type {
    border-radius: 8px 0 0 8px;
  }
  .range {
    position: absolute;
    top: -2px;
    bottom: -2px;
    border: 2px solid var(--accent);
    border-left-width: 0;
    border-right-width: 0;
    pointer-events: none;
  }

  .handle {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 12px;
    background: var(--accent);
    cursor: ew-resize;
    transition: filter var(--fast);
  }
  .handle::after {
    content: '';
    position: absolute;
    left: 50%;
    top: 50%;
    width: 2px;
    height: 16px;
    border-radius: 2px;
    background: var(--accent-fg);
    opacity: 0.55;
    transform: translate(-50%, -50%);
  }
  .handle:hover {
    filter: brightness(1.12);
  }
  .handle.start {
    transform: translateX(-100%);
    border-radius: 6px 0 0 6px;
  }
  .handle.end {
    border-radius: 0 6px 6px 0;
  }

  .head {
    position: absolute;
    top: -6px;
    bottom: -6px;
    width: 2px;
    margin-left: -1px;
    border-radius: 2px;
    background: #fff;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4);
    pointer-events: none;
  }
  .head::before {
    content: '';
    position: absolute;
    top: 0;
    left: 50%;
    width: 10px;
    height: 10px;
    border-radius: 3px;
    background: #fff;
    transform: translate(-50%, -2px) rotate(45deg);
  }
</style>
