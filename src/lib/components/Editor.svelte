<!-- Editing tools under the player: a multi-track timeline (video, game sound, microphone) with
     trim handles and per-track volume, then save a copy or export for Discord.
     Keys: Space play/pause, I/O set the start/end at the playhead, ←/→ seek (Shift: 5 s). -->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import Timeline, { type Lane } from './Timeline.svelte';
  import ExportDialog from './ExportDialog.svelte';
  import { api, mediaSrc, native, type Shard, type ShardInfo, type Waveform } from '$lib/api';
  import { getEdit, saveEdit } from '$lib/edits';
  import { gameTint } from '$lib/games';
  import { t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { tooltip } from '$lib/tooltip';

  let { shard, video }: { shard: Shard; video?: HTMLVideoElement } = $props();

  const BUCKETS = 800;

  let info = $state<ShardInfo | null>(null);
  let waveforms = $state<Waveform[] | null>(null);
  let start = $state(0);
  let end = $state(0);
  let current = $state(0);
  let playing = $state(false);
  let gains = $state({ game: 1, mic: 1 });
  let exporting = $state(false);
  /** The clip whose saved edit is applied: edits are only saved once it is. */
  let loadedId = $state<string | null>(null);

  const duration = $derived(info?.duration ?? 0);
  const hasMic = $derived(!!info?.audioTracks.some((track) => track.kind === 'mic'));
  const selection = $derived(Math.max(0, end - start));
  const real = $derived(native && !library.demo);
  const trimmed = $derived(start > 0.05 || end < duration - 0.05);

  // One lane per thing the user can turn up or down: game + mic, or the only track.
  const lanes = $derived.by((): Lane[] => {
    if (!info?.audioTracks.length) return [];
    const peaksOf = (kind: Waveform['kind']) => waveforms?.find((w) => w.kind === kind)?.peaks ?? null;
    if (hasMic) {
      return [
        { key: 'game', label: t('editor.game'), icon: 'gamepad', peaks: peaksOf('game'), gain: gains.game },
        { key: 'mic', label: t('editor.mic'), icon: 'mic', peaks: peaksOf('mic'), gain: gains.mic },
      ];
    }
    const only = info.audioTracks[0];
    return [
      {
        key: 'game',
        label: only.kind === 'game' ? t('editor.game') : t('editor.audio'),
        icon: only.kind === 'game' ? 'gamepad' : 'volume',
        peaks: waveforms?.[0]?.peaks ?? null,
        gain: gains.game,
      },
    ];
  });

  // What the clip contains and its waveforms. Only a change of clip re-runs this.
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
      audioTracks: real
        ? [{ name: 'Audio', kind: 'mix' }]
        : [
            { name: 'Mix', kind: 'mix' },
            { name: 'Game', kind: 'game' },
            { name: 'Microphone', kind: 'mic' },
          ],
    };
    waveforms = null;
    loadedId = null;
    const apply = (next: ShardInfo) => {
      if (shard.id !== id) return;
      info = { ...next, duration: next.duration || fallback.duration };
      // Start from the last edit of this clip, if any (clamped: the file may have changed).
      const saved = getEdit(id);
      const d = info.duration;
      start = saved ? Math.min(Math.max(0, saved.start), Math.max(0, d - 0.5)) : 0;
      end = saved ? Math.min(d, Math.max(start + 0.5, saved.end)) : d;
      gains = saved ? { ...saved.gains } : { game: 1, mic: 1 };
      if (saved) seek(start);
      loadedId = id;
    };
    if (real) {
      api.probeShard(id).then(apply, () => apply(fallback));
      api.getWaveforms(id, BUCKETS).then(
        (list) => shard.id === id && (waveforms = list),
        () => {},
      );
    } else {
      apply(fallback);
      waveforms = demoWaveforms(id);
    }
  }

  /** Plausible waveforms for demo shards: game sound with bursts, a voice in short phrases. */
  function demoWaveforms(seed: string): Waveform[] {
    let h = 0;
    for (const c of seed) h = (h * 31 + c.charCodeAt(0)) | 0;
    const rand = () => ((h = (h * 1103515245 + 12345) | 0), ((h >>> 16) & 0x7fff) / 0x7fff);
    const game = Array.from({ length: BUCKETS }, (_, i) => {
      const base = 0.28 + 0.12 * Math.sin(i / 37) + 0.08 * Math.sin(i / 9);
      return Math.min(1, base + (rand() < 0.04 ? 0.45 * rand() : 0.12 * rand()));
    });
    let talking = false;
    const mic = Array.from({ length: BUCKETS }, () => {
      if (rand() < 0.02) talking = !talking;
      return talking ? 0.2 + 0.35 * rand() : 0.02 * rand();
    });
    return [
      { name: 'Mix', kind: 'mix', peaks: game.map((g, i) => Math.min(1, g + mic[i] * 0.6)) },
      { name: 'Game', kind: 'game', peaks: game },
      { name: 'Microphone', kind: 'mic', peaks: mic },
    ];
  }

  // Remember the trim and volumes of this clip (shortly after the last change).
  $effect(() => {
    const edit = { start, end, gains: { ...gains } };
    const id = loadedId;
    const d = duration;
    if (!id || id !== shard.id || !d) return;
    const timer = setTimeout(() => saveEdit(id, edit, d), 300);
    return () => clearTimeout(timer);
  });

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

  // A single audio track can be previewed live (up to 100 %); separate tracks apply on export.
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
</script>

<svelte:window {onkeydown} />

<div class="editor">
  <div class="toolbar">
    <button class="play" onclick={toggle} aria-label={t('shard.play')} use:tooltip={{ text: `${t('shard.play')} · Espace`, side: 'top' }}>
      <Icon name={playing ? 'pause' : 'play'} size={15} fill />
    </button>
    <span class="clock">{clock(current)} <span>/ {clock(duration)}</span></span>

    <span class="divider"></span>

    <div class="trim">
      <button class="chip" onclick={() => (start = Math.min(current, end - 0.5))} aria-label={t('editor.setIn')} use:tooltip={{ text: `${t('editor.setIn')} · I`, side: 'top' }}>
        <span class="bracket">[</span>
      </button>
      <button class="chip" onclick={() => (end = Math.max(current, start + 0.5))} aria-label={t('editor.setOut')} use:tooltip={{ text: `${t('editor.setOut')} · O`, side: 'top' }}>
        <span class="bracket">]</span>
      </button>
      {#if trimmed}
        <button class="chip" onclick={() => ((start = 0), (end = duration))} aria-label={t('editor.reset')} use:tooltip={{ text: t('editor.reset'), side: 'top' }}>
          <Icon name="undo" size={13} />
        </button>
      {/if}
      <span class="selection" class:trimmed>{t('editor.selection', { duration: `${selection.toFixed(1)} s` })}</span>
    </div>

    <div class="actions">
      <button class="button primary" onclick={() => (exporting = true)} disabled={!info}>
        <Icon name="share" size={14} />{t('export.title')}
      </button>
    </div>
  </div>

  {#if info}
    <Timeline
      {duration}
      bind:start
      bind:end
      {current}
      src={shard.path ? mediaSrc(shard.path) : null}
      tint={gameTint(shard.game)}
      {lanes}
      onseek={seek}
      ongain={(key, gain) => (gains[key] = gain)}
    />
  {/if}

  {#if hasMic && (gains.game !== 1 || gains.mic !== 1)}
    <p class="note"><Icon name="info" size={13} />{t('editor.volumesOnExport')}</p>
  {/if}
</div>

{#if exporting}
  <ExportDialog {shard} {start} {end} gains={{ ...gains }} onclose={() => (exporting = false)} />
{/if}

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 12px 16px 16px;
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
  }

  .toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 36px;
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
    width: 96px;
    flex: none;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }
  .clock span {
    color: var(--text-3);
  }
  .divider {
    width: 1px;
    height: 20px;
    background: var(--border-strong);
  }
  .trim {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .chip {
    display: inline-grid;
    place-items: center;
    min-width: 30px;
    height: 28px;
    padding: 0 8px;
    border-radius: 7px;
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
  .bracket {
    font-weight: 700;
    font-size: 14px;
    line-height: 1;
  }
  .selection {
    white-space: nowrap;
    margin-left: 6px;
    font-size: 12.5px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .selection.trimmed {
    color: var(--accent);
  }
  .note {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: -4px 0 0 212px;
    font-size: 12px;
    color: var(--text-3);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
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
    white-space: nowrap;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      background var(--fast),
      filter var(--fast);
  }
  .button:hover {
    background: #2a2b31;
  }
  .button.primary {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }
  .button.primary:hover {
    filter: brightness(1.08);
  }

</style>
