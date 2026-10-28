<!-- Editing tools under the player: trim, per-track volume, save a copy, export for Discord.
     Keys: Space play/pause, I/O set the start/end at the playhead, ←/→ seek. -->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import Timeline from './Timeline.svelte';
  import { api, errorCode, mediaSrc, native, on, type Shard, type ShardInfo } from '$lib/api';
  import { gameTint } from '$lib/games';
  import { formatSize, hasKey, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { showError } from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { openMenu, toast } from '$lib/ui.svelte';

  let { shard, video }: { shard: Shard; video?: HTMLVideoElement } = $props();

  let info = $state<ShardInfo | null>(null);
  let start = $state(0);
  let end = $state(0);
  let current = $state(0);
  let playing = $state(false);
  let gains = $state({ game: 1, mic: 1 });
  let progress = $state<number | null>(null);

  const duration = $derived(info?.duration ?? 0);
  const hasMic = $derived(!!info?.audioTracks.some((track) => track.kind === 'mic'));
  const selection = $derived(Math.max(0, end - start));
  const real = $derived(native && !library.demo);

  // Discord upload limits (MB) and the one used last.
  const limits = [10, 50, 500] as const;
  let limit = $state<(typeof limits)[number]>(readLimit());
  function readLimit(): (typeof limits)[number] {
    try {
      const saved = Number(localStorage.getItem('kryo-shard.discord-limit'));
      return limits.find((l) => l === saved) ?? 10;
    } catch {
      return 10;
    }
  }

  // What the clip contains. Demo shards and older backends get a plausible stand-in.
  // Only a change of clip re-runs this.
  $effect(() => {
    const id = shard.id;
    untrack(() => load(id));
  });

  function load(id: string) {
    const fallback: ShardInfo = {
      duration: shard.duration || video?.duration || 30,
      width: 1920,
      height: 1080,
      fps: 60,
      audioTracks: real ? [{ name: 'Audio', kind: 'mix' }] : [
        { name: 'Mix', kind: 'mix' },
        { name: 'Game', kind: 'game' },
        { name: 'Microphone', kind: 'mic' },
      ],
    };
    const apply = (next: ShardInfo) => {
      if (shard.id !== id) return;
      info = { ...next, duration: next.duration || fallback.duration };
      start = 0;
      end = info.duration;
      gains = { game: 1, mic: 1 };
    };
    if (real) api.probeShard(id).then(apply, () => apply(fallback));
    else apply(fallback);
  }

  // Playhead follows the video smoothly; playback loops inside the kept range.
  onMount(() => {
    let frame = requestAnimationFrame(function tick() {
      if (video) {
        current = video.currentTime;
        playing = !video.paused;
        if (playing && end > 0 && current >= end - 0.02) video.currentTime = start;
        else if (playing && current < start - 0.05) video.currentTime = start;
      }
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });

  // Single audio track: the volume can be previewed live (up to 100 %).
  $effect(() => {
    if (video && !hasMic) video.volume = Math.min(1, gains.game);
  });

  function seek(time: number) {
    if (video) video.currentTime = time;
    current = time;
  }

  function toggle() {
    if (!video) return;
    if (video.paused) {
      if (current >= end - 0.05 || current < start) video.currentTime = start;
      video.play();
    } else video.pause();
  }

  function onkeydown(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement).tagName;
    if (tag === 'INPUT' || e.ctrlKey || e.altKey) return;
    const key = e.key.toLowerCase();
    if (key === ' ' && tag !== 'BUTTON') toggle();
    else if (key === 'i') start = Math.min(current, end - 0.5);
    else if (key === 'o') end = Math.max(current, start + 0.5);
    else if (key === 'arrowleft') seek(Math.max(start, current - (e.shiftKey ? 5 : 1)));
    else if (key === 'arrowright') seek(Math.min(end, current + (e.shiftKey ? 5 : 1)));
    else return;
    e.preventDefault();
  }

  const clock = (time: number) => {
    const m = Math.floor(time / 60);
    const s = (time % 60).toFixed(1).padStart(4, '0');
    return `${m}:${s}`;
  };

  async function run(target: 'copy' | 'discord') {
    if (progress !== null || !info) return;
    progress = 0;
    if (!real) {
      // Browser preview / demo data: show the flow.
      for (let p = 0; p <= 1; p += 0.1) {
        progress = p;
        await new Promise((r) => setTimeout(r, 90));
      }
      progress = null;
      toast({ kind: 'info', title: t('toast.demo') });
      return;
    }
    const stop = on('export-progress', (p) => {
      if (p.id === shard.id && progress !== null) progress = p.progress;
    });
    try {
      const maxBytes = target === 'discord' ? limit * 1_000_000 : null;
      const result = await api.exportShard({ id: shard.id, start, end, gains: $state.snapshot(gains), target, maxBytes });
      if (target === 'copy') {
        if (result.shard) library.upsert(result.shard);
        toast({ kind: 'success', title: t('editor.saved') });
      } else {
        toast({ kind: 'success', title: t('editor.copied'), body: t('editor.copiedBody', { size: formatSize(result.size) }) });
      }
    } catch (e) {
      const code = errorCode(e);
      if (code === 'cancelled') toast({ kind: 'info', title: t('error.cancelled') });
      else showError(hasKey(`error.${code}`) ? code : 'export_failed');
    } finally {
      stop.then((unlisten) => unlisten());
      progress = null;
    }
  }

  function pickLimit(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(
      r.right,
      r.top - 8,
      limits.map((l) => ({
        label: `${formatSize(l * 1_000_000)} · ${t(`editor.limit.${l}`)}`,
        icon: l === limit ? 'check' : undefined,
        run: () => {
          limit = l;
          try {
            localStorage.setItem('kryo-shard.discord-limit', String(l));
          } catch {
            // no storage
          }
          run('discord');
        },
      })),
    );
  }
</script>

<svelte:window {onkeydown} />

{#snippet slider(label: string, icon: 'gamepad' | 'mic' | 'volume', key: 'game' | 'mic')}
  <label class="slider">
    <span class="slider-label" use:tooltip={{ text: label, side: 'top' }}>
      <Icon name={gains[key] === 0 ? 'mute' : icon} size={15} />
    </span>
    <input type="range" min="0" max="2" step="0.05" bind:value={gains[key]} aria-label={label} style:--fill="{(gains[key] / 2) * 100}%" />
    <span class="percent">{Math.round(gains[key] * 100)} %</span>
  </label>
{/snippet}

<div class="editor" class:busy={progress !== null}>
  <div class="top">
    <button class="play" onclick={toggle} aria-label={t('shard.play')}>
      <Icon name={playing ? 'pause' : 'play'} size={15} fill />
    </button>
    <span class="clock">{clock(current)} <span>/ {clock(duration)}</span></span>
    <div class="grow">
      {#if info}
        <Timeline
          {duration}
          bind:start
          bind:end
          {current}
          src={shard.path ? mediaSrc(shard.path) : null}
          tint={gameTint(shard.game)}
          onseek={seek}
        />
      {/if}
    </div>
  </div>

  <div class="bottom">
    <div class="trim">
      <button class="chip" onclick={() => (start = Math.min(current, end - 0.5))} aria-label={t('editor.setIn')} use:tooltip={{ text: `${t('editor.setIn')} · I`, side: 'top' }}>
        <span class="bracket">[</span>
      </button>
      <button class="chip" onclick={() => (end = Math.max(current, start + 0.5))} aria-label={t('editor.setOut')} use:tooltip={{ text: `${t('editor.setOut')} · O`, side: 'top' }}>
        <span class="bracket">]</span>
      </button>
      {#if start > 0 || end < duration}
        <button class="chip" onclick={() => ((start = 0), (end = duration))} aria-label={t('editor.reset')} use:tooltip={{ text: t('editor.reset'), side: 'top' }}>
          <Icon name="undo" size={13} />
        </button>
      {/if}
      <span class="selection">{t('editor.selection', { duration: `${selection.toFixed(1)} s` })}</span>
    </div>

    <div class="audio">
      {#if hasMic}
        {@render slider(t('editor.game'), 'gamepad', 'game')}
        {@render slider(t('editor.mic'), 'mic', 'mic')}
      {:else}
        {@render slider(t('editor.volume'), 'volume', 'game')}
      {/if}
    </div>

    <div class="actions">
      {#if progress !== null}
        <div class="progress">
          <span class="bar" style:transform="scaleX({progress})"></span>
          <span class="label">{t('editor.exporting', { percent: Math.round(progress * 100) })}</span>
          {#if real}
            <button class="cancel" onclick={() => api.cancelExport()} aria-label={t('common.cancel')}><Icon name="x" size={13} /></button>
          {/if}
        </div>
      {:else}
        <button class="button" onclick={() => run('copy')}>{t('editor.saveCopy')}</button>
        <div class="split" use:tooltip={{ text: t('editor.discordHint'), side: 'top' }}>
          <button class="button primary" onclick={() => run('discord')}>
            <Icon name="share" size={14} />{t('editor.discord')} · {formatSize(limit * 1_000_000)}
          </button>
          <button class="button primary more" onclick={pickLimit} aria-label={t('shard.more')} aria-haspopup="menu">
            <Icon name="chevronDown" size={14} />
          </button>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 14px 16px 16px;
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .play {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    flex: none;
    border-radius: 50%;
    color: #0a0b0d;
    background: #fff;
    transition: transform var(--fast) var(--ease-out);
  }
  .play:active {
    transform: scale(0.92);
  }
  .clock {
    width: 92px;
    flex: none;
    font-size: 12.5px;
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }
  .clock span {
    color: var(--text-3);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }

  .bottom {
    display: flex;
    align-items: center;
    gap: 10px 22px;
    flex-wrap: wrap;
  }
  .trim,
  .audio,
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .audio {
    gap: 14px;
  }
  .actions {
    margin-left: auto;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 12.5px;
    font-weight: 500;
    color: var(--text-2);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .chip:hover {
    color: var(--text);
    background: var(--surface-3);
  }
  .chip {
    justify-content: center;
    min-width: 30px;
  }
  .bracket {
    font-weight: 700;
    font-size: 14px;
    line-height: 1;
  }
  .selection {
    margin-left: 6px;
    font-size: 12.5px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }

  .slider {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .slider-label {
    display: grid;
    place-items: center;
    color: var(--text-2);
  }
  .percent {
    width: 40px;
    font-size: 12px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  input[type='range'] {
    appearance: none;
    width: 84px;
    height: 4px;
    border-radius: 4px;
    background: linear-gradient(to right, var(--accent) var(--fill), var(--surface-3) var(--fill));
    cursor: pointer;
  }
  input[type='range']::-webkit-slider-thumb {
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
    transition: transform var(--fast) var(--ease-out);
  }
  input[type='range']:active::-webkit-slider-thumb {
    transform: scale(1.2);
  }

  .button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 14px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      background var(--fast),
      filter var(--fast);
  }
  .button:hover {
    background: #2a2b31;
  }
  .split {
    display: flex;
  }
  .button.primary {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }
  .button.primary:hover {
    filter: brightness(1.08);
  }
  .split .button.primary:first-child {
    border-radius: 8px 0 0 8px;
  }
  .button.more {
    width: 30px;
    padding: 0;
    justify-content: center;
    border-radius: 0 8px 8px 0;
    border-left: 1px solid color-mix(in srgb, var(--accent-fg) 25%, transparent);
  }

  .progress {
    position: relative;
    display: flex;
    align-items: center;
    width: 280px;
    height: 34px;
    padding: 0 6px 0 14px;
    border-radius: 8px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar {
    position: absolute;
    inset: 0;
    background: var(--accent-soft);
    transform-origin: left;
    transition: transform 200ms linear;
  }
  .progress .label {
    position: relative;
    flex: 1;
    font-size: 13px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .cancel {
    position: relative;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 6px;
    color: var(--text-2);
  }
  .cancel:hover {
    background: rgba(255, 255, 255, 0.08);
    color: var(--text);
  }
</style>
