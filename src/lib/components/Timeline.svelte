<!-- Multi-track timeline: ruler, video filmstrip and one lane per audio source (waveform, volume,
     mute), then the effects (captions, blurs). The kept range and the playhead span every lane:
     drag the handles to trim, drag anywhere else to scrub. Dragging on the ruler (or anywhere
     with Shift) selects a zone for the editor's tools; the effects show where they apply (cuts
     across every lane, slow motion on the video, mutes on their track) and are picked with a
     click, their edges dragged to resize them. -->
<script lang="ts" module>
  import type { IconName } from './Icon.svelte';

  export interface Lane {
    key: 'game' | 'mic';
    label: string;
    icon: IconName;
    /** null while loading or when the waveform isn't available. */
    peaks: number[] | null;
    gain: number;
  }
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { fx, type Kind, type Range } from '$lib/fx.svelte';
  import { formatDuration, t } from '$lib/i18n.svelte';
  import { tooltip } from '$lib/tooltip';

  let {
    duration,
    start = $bindable(),
    end = $bindable(),
    current,
    src,
    tint,
    lanes,
    zone = $bindable(null),
    onseek,
    ongain,
  }: {
    duration: number;
    start: number;
    end: number;
    current: number;
    src: string | null;
    tint: [string, string];
    lanes: Lane[];
    /** The zone selected for the tools, null when none. */
    zone?: Range | null;
    onseek: (time: number) => void;
    ongain: (key: Lane['key'], gain: number) => void;
  } = $props();

  const MIN = 0.5;
  const FRAMES = 14;

  let area: HTMLDivElement;
  let canvases: HTMLCanvasElement[] = $state([]);
  let painted = $state(0);
  /** Gain before muting, to restore it. */
  const beforeMute: Partial<Record<Lane['key'], number>> = {};

  const pct = (time: number) => `${(time / (duration || 1)) * 100}%`;

  // Ruler: a readable step for the clip length.
  const ticks = $derived.by(() => {
    const step = duration <= 12 ? 1 : duration <= 40 ? 5 : duration <= 150 ? 10 : 30;
    const list: { time: number; label: boolean }[] = [];
    const minor = step === 1 ? 0.5 : step / 5;
    for (let time = 0; time <= duration + 1e-6; time += minor) {
      const major = Math.abs(time / step - Math.round(time / step)) < 1e-6;
      list.push({ time, label: major });
    }
    return list;
  });

  // Filmstrip: seek a hidden copy of the video and paint each frame as soon as it is ready.
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

  /** Range drawn by a waveform: from silence at -48 dB up to full scale. */
  const FLOOR_DB = 48;
  /** Height (0..1) of a level (0..1 of full scale) on a decibel scale, like audio editors: a
   *  normal game mix (peaks around -12 dB) fills most of the lane instead of a quarter of it. */
  const loudness = (level: number) => (level > 0 ? Math.max(0, 1 + (20 * Math.log10(level)) / FLOOR_DB) : 0);

  /** Draws a mirrored waveform scaled by the lane's gain; what would clip is drawn in orange. */
  function wave(canvas: HTMLCanvasElement, params: { peaks: number[] | null; gain: number }) {
    let current = params;
    const draw = () => {
      const dpr = devicePixelRatio || 1;
      const w = Math.max(1, Math.round(canvas.clientWidth * dpr));
      const h = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== w || canvas.height !== h) {
        canvas.width = w;
        canvas.height = h;
      }
      const ctx = canvas.getContext('2d');
      if (!ctx) return;
      ctx.clearRect(0, 0, w, h);
      const mid = h / 2;
      const color = getComputedStyle(canvas).color;
      const { peaks, gain } = current;
      if (!peaks?.length) {
        ctx.fillStyle = color;
        ctx.globalAlpha = 0.35;
        ctx.fillRect(0, mid - dpr / 2, w, dpr);
        return;
      }
      const bar = 2 * dpr;
      const gap = 1 * dpr;
      const bars = Math.floor(w / (bar + gap));
      for (let i = 0; i < bars; i++) {
        // The loudest peak of the buckets this bar covers.
        const from = Math.floor((i / bars) * peaks.length);
        const to = Math.max(from + 1, Math.floor(((i + 1) / bars) * peaks.length));
        let peak = 0;
        for (let j = from; j < to; j++) peak = Math.max(peak, peaks[j]);
        const level = peak * gain;
        const x = i * (bar + gap);
        const half = Math.max(dpr / 2, Math.min(1, loudness(level)) * (mid - dpr));
        ctx.globalAlpha = 0.9;
        ctx.fillStyle = color;
        ctx.fillRect(x, mid - half, bar, half * 2);
        if (level > 1) {
          // Would clip: tint the tips.
          ctx.fillStyle = '#f5a524';
          ctx.fillRect(x, mid - half, bar, 2 * dpr);
          ctx.fillRect(x, mid + half - 2 * dpr, bar, 2 * dpr);
        }
      }
    };
    const ro = new ResizeObserver(draw);
    ro.observe(canvas);
    draw();
    return {
      update(next: { peaks: number[] | null; gain: number }) {
        current = next;
        draw();
      },
      destroy: () => ro.disconnect(),
    };
  }

  function timeAt(clientX: number): number {
    const r = area.getBoundingClientRect();
    return Math.min(duration, Math.max(0, ((clientX - r.left) / r.width) * duration));
  }

  function drag(what: 'start' | 'end' | 'head') {
    return (e: PointerEvent) => {
      if (e.button !== 0) return;
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

  /** Drag on the ruler (or Shift-drag anywhere): select a zone. A click without dragging seeks. */
  function selectZone(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const from = timeAt(e.clientX);
    fx.selected = null;
    let moved = false;
    const move = (ev: PointerEvent) => {
      const to = timeAt(ev.clientX);
      if (!moved && Math.abs(ev.clientX - e.clientX) < 4) return;
      moved = true;
      zone = { start: Math.min(from, to), end: Math.max(from, to) };
    };
    const up = () => {
      el.removeEventListener('pointermove', move);
      if (!moved || (zone && zone.end - zone.start < 0.05)) {
        zone = null;
        onseek(from);
      }
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up, { once: true });
  }

  function areaDown(e: PointerEvent) {
    if (e.shiftKey) selectZone(e);
    else {
      fx.selected = null;
      drag('head')(e);
    }
  }

  /** Picks an effect (and moves the playhead into it). */
  function pick(kind: Kind, index: number, at: number) {
    return (e: PointerEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      fx.selected = { kind, index };
      zone = null;
      onseek(Math.max(at, timeAt(e.clientX)));
    };
  }

  /** Drags an edge of the selected effect's range. */
  function resize(item: { start?: number; end?: number }, edge: 'start' | 'end') {
    return (e: PointerEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      const el = e.currentTarget as HTMLElement;
      el.setPointerCapture(e.pointerId);
      const move = (ev: PointerEvent) => {
        const time = timeAt(ev.clientX);
        const startAt = item.start ?? start;
        const endAt = item.end ?? end;
        if (edge === 'start') item.start = Math.min(time, endAt - 0.1);
        else item.end = Math.max(time, startAt + 0.1);
        onseek(edge === 'start' ? (item.start ?? start) : (item.end ?? end));
      };
      el.addEventListener('pointermove', move);
      el.addEventListener('pointerup', () => el.removeEventListener('pointermove', move), { once: true });
    };
  }

  const isSelected = (kind: Kind, index: number) => fx.selected?.kind === kind && fx.selected.index === index;
  const mutesOf = (key: Lane['key']) =>
    fx.mutes.map((m, index) => ({ m, index })).filter(({ m }) => m.track === 'all' || m.track === key);
  const effectsLane = $derived(fx.texts.length > 0 || fx.blurs.length > 0);

  function toggleMute(lane: Lane) {
    if (lane.gain > 0) {
      beforeMute[lane.key] = lane.gain;
      ongain(lane.key, 0);
    } else ongain(lane.key, beforeMute[lane.key] || 1);
  }
</script>

<div class="timeline" style:--a={tint[0]} style:--b={tint[1]}>
  <div class="heads">
    <div class="head ruler-head"></div>
    <div class="head video-head">
      <span class="badge"><Icon name="film" size={14} /></span>
      <span class="name">{t('editor.video')}</span>
    </div>
    {#each lanes as lane (lane.key)}
      <div class="head audio-head" class:mic={lane.key === 'mic'} class:muted={lane.gain === 0}>
        <button
          class="badge mute"
          onclick={() => toggleMute(lane)}
          aria-pressed={lane.gain === 0}
          aria-label={t(lane.gain === 0 ? 'editor.unmute' : 'editor.mute')}
          use:tooltip={{ text: t(lane.gain === 0 ? 'editor.unmute' : 'editor.mute'), side: 'top' }}
        >
          <Icon name={lane.gain === 0 ? 'mute' : lane.icon} size={14} />
        </button>
        <div class="head-body">
          <span class="name">{lane.label}</span>
          <label class="volume" use:tooltip={{ text: t('editor.resetVolume'), side: 'top' }}>
            <input
              type="range"
              min="0"
              max="2"
              step="0.05"
              value={lane.gain}
              style:--fill="{(lane.gain / 2) * 100}%"
              oninput={(e) => ongain(lane.key, Number(e.currentTarget.value))}
              ondblclick={() => ongain(lane.key, 1)}
              aria-label="{lane.label} · {t('editor.volume')}"
            />
            <span class:loud={lane.gain > 1}>{Math.round(lane.gain * 100)} %</span>
          </label>
        </div>
      </div>
    {/each}
    {#if effectsLane}
      <div class="head fx-head">
        <span class="badge"><Icon name="sparkles" size={14} /></span>
        <span class="name">{t('editor.effects')}</span>
      </div>
    {/if}
  </div>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="area" bind:this={area} onpointerdown={areaDown}>
    <div class="ruler" onpointerdown={selectZone} use:tooltip={{ text: t('editor.zoneHint'), side: 'top' }}>
      {#each ticks as tick (tick.time)}
        <span class="tick" class:major={tick.label} style:left={pct(tick.time)}>
          {#if tick.label}<span class="tick-label">{formatDuration(tick.time)}</span>{/if}
        </span>
      {/each}
    </div>

    {#snippet edges(item: { start?: number; end?: number }, on: boolean)}
      {#if on}
        <span class="edge left" onpointerdown={resize(item, 'start')} role="presentation"></span>
        <span class="edge right" onpointerdown={resize(item, 'end')} role="presentation"></span>
      {/if}
    {/snippet}

    <div class="lane video-lane">
      <div class="strip">
        {#each { length: FRAMES } as _, i (i)}
          <canvas bind:this={canvases[i]} class:on={i < painted} width="160" height="90"></canvas>
        {/each}
      </div>
      {#each fx.slow as zone, i (i)}
        <button
          class="seg slow"
          class:on={isSelected('slow', i)}
          style:left={pct(zone.start)}
          style:width={pct(zone.end - zone.start)}
          onpointerdown={pick('slow', i, zone.start)}
          aria-label="{t('editor.slow')} ×{zone.rate}"
        >
          <span>×{zone.rate}</span>
          {@render edges(zone, isSelected('slow', i))}
        </button>
      {/each}
    </div>

    {#each lanes as lane (lane.key)}
      <div class="lane audio-lane" class:mic={lane.key === 'mic'} class:muted={lane.gain === 0}>
        <canvas class="wave" use:wave={{ peaks: lane.peaks, gain: lane.gain }}></canvas>
        {#each mutesOf(lane.key) as { m, index } (index)}
          <button
            class="seg mute"
            class:beep={m.beep}
            class:on={isSelected('mute', index)}
            style:left={pct(m.start)}
            style:width={pct(m.end - m.start)}
            onpointerdown={pick('mute', index, m.start)}
            aria-label={t(m.beep ? 'editor.beep' : 'editor.muted')}
          >
            <span><Icon name={m.beep ? 'zap' : 'mute'} size={11} />{t(m.beep ? 'editor.beep' : 'editor.muted')}</span>
            {@render edges(m, isSelected('mute', index))}
          </button>
        {/each}
      </div>
    {/each}

    {#if effectsLane}
      <div class="lane fx-lane">
        {#each fx.blurs as blur, i (i)}
          {@const from = blur.start ?? start}
          {@const to = blur.end ?? end}
          <button
            class="seg chip blur"
            class:on={isSelected('blur', i)}
            style:left={pct(from)}
            style:width={pct(to - from)}
            onpointerdown={pick('blur', i, from)}
            aria-label={t('editor.blur')}
          >
            <span><Icon name="grid" size={11} />{t('editor.blur')}</span>
            {@render edges(blur, isSelected('blur', i))}
          </button>
        {/each}
        {#each fx.texts as caption, i (i)}
          <button
            class="seg chip text"
            class:on={isSelected('text', i)}
            style:left={pct(caption.start)}
            style:width={pct(caption.end - caption.start)}
            onpointerdown={pick('text', i, caption.start)}
            aria-label="{t('editor.text')} · {caption.text}"
          >
            <span><b>T</b>{caption.text}</span>
            {@render edges(caption, isSelected('text', i))}
          </button>
        {/each}
      </div>
    {/if}

    {#each fx.cuts as cut, i (i)}
      <button
        class="cut"
        class:on={isSelected('cut', i)}
        style:left={pct(cut.start)}
        style:width={pct(cut.end - cut.start)}
        onpointerdown={pick('cut', i, cut.start)}
        aria-label={t('editor.cutLabel')}
        use:tooltip={{ text: t('editor.cutLabel'), side: 'top' }}
      >
        <span class="cut-icon"><Icon name="scissors" size={13} /></span>
        {@render edges(cut, isSelected('cut', i))}
      </button>
    {/each}

    {#if zone}
      <div class="zone" style:left={pct(zone.start)} style:width={pct(zone.end - zone.start)}>
        <span class="zone-length">{(zone.end - zone.start).toFixed(1)} s</span>
      </div>
    {/if}

    <div class="dim" style:left="0" style:width={pct(start)}></div>
    <div class="dim" style:left={pct(end)} style:right="0"></div>
    <div class="range" style:left={pct(start)} style:width={pct(end - start)}></div>

    <button
      class="handle start"
      style:left={pct(start)}
      onpointerdown={drag('start')}
      onkeydown={nudge('start')}
      aria-label="{t('editor.start')} · {formatDuration(start)}"
    ><span></span></button>
    <button
      class="handle end"
      style:left={pct(end)}
      onpointerdown={drag('end')}
      onkeydown={nudge('end')}
      aria-label="{t('editor.end')} · {formatDuration(end)}"
    ><span></span></button>

    <div class="playhead" style:left={pct(current)}></div>
  </div>
</div>

<style>
  .timeline {
    --ruler: 24px;
    --video: 58px;
    --audio: 52px;
    --gap: 6px;
    display: flex;
    gap: 16px;
    user-select: none;
  }

  /* Lane headers ------------------------------------------------------------------- */
  .heads {
    display: flex;
    flex-direction: column;
    gap: var(--gap);
    width: 196px;
    flex: none;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 10px;
    border-radius: 9px;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .ruler-head {
    height: var(--ruler);
    background: none;
    box-shadow: none;
  }
  .video-head {
    height: var(--video);
  }
  .audio-head {
    --c: var(--accent);
    height: var(--audio);
  }
  .audio-head.mic {
    --c: #3fd49a;
  }
  .badge {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: 7px;
    color: var(--text-2);
    background: var(--surface-3);
  }
  .audio-head .badge {
    color: var(--c);
    background: color-mix(in srgb, var(--c) 14%, transparent);
    transition:
      color var(--fast),
      background var(--fast),
      transform var(--fast) var(--ease-out);
  }
  .audio-head .badge:hover {
    background: color-mix(in srgb, var(--c) 22%, transparent);
  }
  .audio-head .badge:active {
    transform: scale(0.92);
  }
  .audio-head.muted .badge {
    color: var(--text-3);
    background: var(--surface-3);
  }
  .name {
    font-size: 12.5px;
    font-weight: 600;
  }
  .head-body {
    display: flex;
    flex-direction: column;
    gap: 4px;
    flex: 1;
    min-width: 0;
  }
  .volume {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .volume span {
    width: 38px;
    font-size: 11px;
    text-align: right;
    font-variant-numeric: tabular-nums;
    color: var(--text-3);
  }
  .volume span.loud {
    color: #f5a524;
  }
  input[type='range'] {
    appearance: none;
    flex: 1;
    min-width: 0;
    height: 3px;
    border-radius: 3px;
    background: linear-gradient(to right, var(--c) var(--fill), var(--surface-3) var(--fill));
    cursor: pointer;
  }
  input[type='range']::-webkit-slider-thumb {
    appearance: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.5);
    transition: transform var(--fast) var(--ease-out);
  }
  input[type='range']:active::-webkit-slider-thumb {
    transform: scale(1.25);
  }

  /* Lanes ------------------------------------------------------------------------ */
  .area {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--gap);
    flex: 1;
    min-width: 0;
    cursor: text;
    touch-action: none;
  }
  .ruler {
    position: relative;
    height: var(--ruler);
  }
  .tick {
    position: absolute;
    bottom: 0;
    width: 1px;
    height: 5px;
    background: var(--border-strong);
  }
  .tick.major {
    height: 9px;
    background: rgba(255, 255, 255, 0.22);
  }
  .tick-label {
    position: absolute;
    bottom: 11px;
    left: 0;
    transform: translateX(-50%);
    font-size: 10.5px;
    font-variant-numeric: tabular-nums;
    color: var(--text-3);
    white-space: nowrap;
  }
  .lane {
    position: relative;
    border-radius: 9px;
    overflow: hidden;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .video-lane {
    height: var(--video);
  }
  .strip {
    position: absolute;
    inset: 0;
    display: flex;
    background:
      radial-gradient(90% 120% at 85% 0%, color-mix(in srgb, var(--b) 45%, transparent), transparent 60%),
      radial-gradient(90% 120% at 10% 100%, color-mix(in srgb, var(--a) 50%, transparent), transparent 65%),
      var(--surface-2);
  }
  .strip canvas {
    flex: 1;
    min-width: 0;
    height: 100%;
    object-fit: cover;
    opacity: 0;
    transition: opacity var(--slow);
  }
  .strip canvas.on {
    opacity: 1;
  }
  .audio-lane {
    --c: var(--accent);
    height: var(--audio);
    background: color-mix(in srgb, var(--c) 6%, var(--surface-2));
  }
  .audio-lane.mic {
    --c: #3fd49a;
  }
  .wave {
    position: absolute;
    inset: 4px 0;
    width: 100%;
    height: calc(100% - 8px);
    color: var(--c);
    transition: opacity var(--normal);
  }
  .audio-lane.muted .wave {
    opacity: 0.3;
  }

  /* Effects ---------------------------------------------------------------------------- */
  .fx-head {
    height: 30px;
  }
  .fx-head .badge {
    width: 22px;
    height: 22px;
    color: #ffc75a;
    background: color-mix(in srgb, #ffc75a 14%, transparent);
  }
  .fx-lane {
    height: 30px;
  }
  .ruler {
    cursor: crosshair;
  }
  .seg {
    position: absolute;
    top: 3px;
    bottom: 3px;
    display: flex;
    align-items: center;
    min-width: 6px;
    padding: 0 6px;
    border-radius: 6px;
    overflow: hidden;
    font-size: 10.5px;
    font-weight: 700;
    white-space: nowrap;
    z-index: 2;
    transition:
      box-shadow var(--fast),
      filter var(--fast);
  }
  .seg span {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .seg:hover {
    filter: brightness(1.12);
  }
  .seg.on {
    box-shadow:
      0 0 0 2px #fff,
      0 2px 10px rgba(0, 0, 0, 0.5);
    overflow: visible;
  }
  .seg.slow {
    top: auto;
    bottom: 4px;
    height: 18px;
    color: #0b1220;
    background: #7fc6db;
  }
  .seg.mute {
    color: #fff;
    background: repeating-linear-gradient(-45deg, rgba(10, 11, 13, 0.82) 0 6px, rgba(10, 11, 13, 0.62) 6px 12px);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.18);
  }
  .seg.mute.beep {
    color: #1a1200;
    background: repeating-linear-gradient(-45deg, #ffc75a 0 6px, #f5b23a 6px 12px);
  }
  .seg.chip.blur {
    color: #fff;
    background: color-mix(in srgb, #a89cff 55%, var(--surface-3));
  }
  .seg.chip.text {
    color: #1a1200;
    background: #ffc75a;
  }
  .seg.chip.text b {
    font-weight: 800;
  }
  .edge {
    position: absolute;
    top: -3px;
    bottom: -3px;
    width: 8px;
    border-radius: 3px;
    background: #fff;
    cursor: ew-resize;
    z-index: 3;
  }
  .edge.left {
    left: -4px;
  }
  .edge.right {
    right: -4px;
  }
  .cut {
    position: absolute;
    top: calc(var(--ruler) + var(--gap));
    bottom: 0;
    display: grid;
    place-items: start center;
    padding-top: 6px;
    border-radius: 6px;
    background: repeating-linear-gradient(45deg, rgba(8, 9, 11, 0.86) 0 7px, rgba(255, 85, 85, 0.2) 7px 14px);
    box-shadow: inset 0 0 0 1px rgba(255, 110, 110, 0.55);
    z-index: 4;
  }
  .cut.on {
    box-shadow:
      inset 0 0 0 2px #fff,
      0 2px 12px rgba(0, 0, 0, 0.5);
  }
  .cut-icon {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 6px;
    color: #fff;
    background: #e5484d;
  }
  .zone {
    position: absolute;
    top: 0;
    bottom: -2px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    box-shadow: inset 0 0 0 1.5px var(--accent);
    pointer-events: none;
    z-index: 5;
  }
  .zone-length {
    position: absolute;
    top: 3px;
    left: 50%;
    transform: translateX(-50%);
    padding: 1px 6px;
    border-radius: 5px;
    font-size: 10.5px;
    font-weight: 700;
    color: var(--accent-fg);
    background: var(--accent);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  /* Range, handles, playhead -------------------------------------------------------------- */
  .dim {
    position: absolute;
    top: calc(var(--ruler) + var(--gap));
    bottom: 0;
    background: rgba(8, 9, 11, 0.66);
    pointer-events: none;
  }
  .range {
    position: absolute;
    top: calc(var(--ruler) + var(--gap) - 2px);
    bottom: -2px;
    border-top: 2px solid var(--accent);
    border-bottom: 2px solid var(--accent);
    pointer-events: none;
  }
  .handle {
    z-index: 7;
    position: absolute;
    top: calc(var(--ruler) + var(--gap) - 2px);
    bottom: -2px;
    width: 12px;
    background: var(--accent);
    cursor: ew-resize;
    transition: filter var(--fast);
  }
  .handle span {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 2px;
    height: 22px;
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
    border-radius: 7px 0 0 7px;
  }
  .handle.end {
    border-radius: 0 7px 7px 0;
  }
  .playhead {
    z-index: 6;
    position: absolute;
    top: 4px;
    bottom: -4px;
    width: 2px;
    margin-left: -1px;
    border-radius: 2px;
    background: #fff;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.45);
    pointer-events: none;
  }
  .playhead::before {
    content: '';
    position: absolute;
    top: -2px;
    left: 50%;
    width: 11px;
    height: 11px;
    border-radius: 3px;
    background: #fff;
    transform: translateX(-50%) rotate(45deg);
  }
</style>
