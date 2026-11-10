<!-- Editing tools under the player: a multi-track timeline (video, game sound, microphone) with
     trim handles and per-track volume, heard live (`LiveMix`), then save a copy or export for
     Discord. A zone selected on the timeline's ruler can be cut out, muted or bleeped, slowed
     down; captions and blurred zones go on the picture (moved on the player). All of it is
     previewed live: cut passages are skipped, mutes and bleeps heard, slow motion played.
     Keys: Space play/pause, I/O set the start/end at the playhead, ←/→ seek (Shift: 5 s),
     X cut the zone, Delete remove the selected effect, Escape let go of the zone. -->
<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import Timeline, { type Lane } from './Timeline.svelte';
  import ExportDialog from './ExportDialog.svelte';
  import Segmented from './Segmented.svelte';
  import Toggle from './Toggle.svelte';
  import { api, mediaSrc, native, type Shard, type ShardInfo, type Waveform } from '$lib/api';
  import { getEdit, saveEdit, type Edit } from '$lib/edits';
  import {
    CAPTION_SIZES,
    add,
    cutAt,
    fx,
    hasEffects,
    loadEffects,
    muteAt,
    playedLength,
    rateAt,
    remove,
    snapshot,
    type CaptionStyle,
    type MuteTrack,
    type Range,
  } from '$lib/fx.svelte';
  import { setBeep } from '$lib/live-mix.svelte';
  import { openMenu } from '$lib/ui.svelte';
  import { canFrame, framing } from '$lib/framing.svelte';
  import { gameTint } from '$lib/games';
  import { t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import type { LiveMix } from '$lib/live-mix.svelte';
  import { tooltip } from '$lib/tooltip';

  let {
    shard,
    video,
    mix,
    onedit,
  }: {
    shard: Shard;
    video?: HTMLVideoElement;
    /** Plays the tracks at the volumes set here. */
    mix?: LiveMix;
    /** Every change of the trim or volumes, as it happens. */
    onedit?: (edit: Edit) => void;
  } = $props();

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
  /** The zone selected on the ruler, for the tools. */
  let zone = $state<Range | null>(null);

  const duration = $derived(info?.duration ?? 0);
  const hasMic = $derived(!!info?.audioTracks.some((track) => track.kind === 'mic'));
  const selection = $derived(Math.max(0, end - start));
  const real = $derived(native && !library.demo);
  const trimmed = $derived(start > 0.05 || end < duration - 0.05);
  /** A vertical window can be cut out of this clip (it is wider than 9:16). */
  const frameable = $derived(!!info && canFrame(info.width, info.height));

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
    framing.vertical = false;
    framing.x = 0.5;
    loadEffects(null);
    zone = null;
    const apply = (next: ShardInfo) => {
      if (shard.id !== id) return;
      info = { ...next, duration: next.duration || fallback.duration };
      // Start from the last edit of this clip, if any (clamped: the file may have changed).
      const saved = getEdit(id);
      const d = info.duration;
      start = saved ? Math.min(Math.max(0, saved.start), Math.max(0, d - 0.5)) : 0;
      end = saved ? Math.min(d, Math.max(start + 0.5, saved.end)) : d;
      gains = saved ? { ...saved.gains } : { game: 1, mic: 1 };
      framing.vertical = !!saved?.vertical;
      framing.x = typeof saved?.cropX === 'number' ? Math.min(1, Math.max(0, saved.cropX)) : 0.5;
      loadEffects(saved?.fx);
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

  // Remember the trim and volumes of this clip (shortly after the last change, or when the
  // editor closes before that).
  let unsaved: (() => void) | null = null;
  $effect(() => {
    const edit: Edit = { start, end, gains: { ...gains }, vertical: framing.vertical, cropX: framing.x, fx: snapshot() };
    const id = loadedId;
    const d = duration;
    if (!id || id !== shard.id || !d) return;
    untrack(() => onedit?.(edit));
    const save = () => {
      saveEdit(id, edit, d);
      unsaved = null;
    };
    unsaved = save;
    const timer = setTimeout(save, 300);
    return () => clearTimeout(timer);
  });
  onDestroy(() => {
    unsaved?.();
    setBeep(0);
    if (video) video.playbackRate = 1;
    fx.selected = null;
  });

  // Playhead follows the video smoothly; playback loops inside the kept range, skips the cut
  // passages, plays slow zones slowly (and lower, like the export), and the mutes and bleeps are
  // heard where they are.
  let heard = '';
  onMount(() => {
    let frame = requestAnimationFrame(function tick() {
      if (video) {
        current = video.currentTime;
        fx.current = current;
        playing = !video.paused;
        if (playing && end > 0 && current >= end - 0.02) video.currentTime = start;
        else if (playing && current < start - 0.05) video.currentTime = start;
        else if (playing) {
          const cut = cutAt(fx, current);
          if (cut) video.currentTime = Math.min(end, cut.end);
        }
        const rate = rateAt(fx, current);
        if (video.playbackRate !== rate) {
          video.preservesPitch = false;
          video.playbackRate = rate;
        }
        const muted = muteAt(fx, current);
        const levels = { game: gains.game * muted.game, mic: gains.mic * muted.mic };
        const key = `${levels.game}|${levels.mic}|${playing ? muted.beep : 0}`;
        if (key !== heard) {
          heard = key;
          mix?.setGains(levels);
          if (!live) video.volume = Math.min(1, hasMic ? Math.min(levels.game, levels.mic) : levels.game);
          setBeep(playing ? muted.beep : 0);
        }
      }
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  });

  // The volumes are heard live. A clip the live mix cannot play (over five minutes) only
  // previews a single track's volume, up to 100 %; separate tracks then apply on export.
  const live = $derived(mix?.state === 'loading' || mix?.state === 'active');
  $effect(() => {
    const target = mix;
    untrack(() => target?.load());
  });
  // A change of volume is applied by the next frame (with the mutes).
  $effect(() => {
    void [gains.game, gains.mic, mix, live];
    heard = '';
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

  // Tools ------------------------------------------------------------------------------

  const clampZone = (z: Range): Range => ({ start: Math.max(0, z.start), end: Math.min(duration, z.end) });

  function cutZone() {
    if (!zone) return;
    add('cut', clampZone(zone));
    zone = null;
  }

  function muteZone(track: MuteTrack, beep: boolean) {
    if (!zone) return;
    add('mute', { ...clampZone(zone), track, beep });
    zone = null;
  }

  function slowZone(rate: number) {
    if (!zone) return;
    add('slow', { ...clampZone(zone), rate });
    zone = null;
  }

  function addText() {
    const range = zone ? clampZone(zone) : { start: current, end: Math.min(end, current + 3) };
    if (range.end - range.start < 0.3) range.end = Math.min(duration, range.start + 3);
    add('text', { ...range, text: t('editor.textDefault'), x: 0.5, y: 0.82, size: CAPTION_SIZES.m, style: 'shadow' });
    zone = null;
  }

  function addBlur() {
    add('blur', { x: 0.36, y: 0.36, w: 0.28, h: 0.28, ...(zone ? clampZone(zone) : {}) });
    zone = null;
  }

  function menuAt(e: MouseEvent, items: { label: string; run: () => void }[]) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu(r.left, r.bottom + 6, items);
  }

  const tracks = $derived(
    (hasMic ? (['mic', 'game', 'all'] as const) : (['game'] as const)).map((value) => ({ value: value as MuteTrack, label: t(`editor.track.${value}`) })),
  );
  const rates = [0.5, 0.25].map((value) => ({ value, label: `×${value}` }));
  const sizes = $derived(
    (Object.entries(CAPTION_SIZES) as [keyof typeof CAPTION_SIZES, number][]).map(([key, value]) => ({ value, label: t(`editor.textSize.${key}`) })),
  );
  const styles = $derived((['shadow', 'outline', 'box'] as CaptionStyle[]).map((value) => ({ value, label: t(`editor.textStyle.${value}`) })));

  const selected = $derived(fx.selected);
  const zoneLength = $derived(zone ? zone.end - zone.start : 0);
  /** What the export lasts: the kept range, cut and slowed. */
  const played = $derived(playedLength(start, end, fx));

  function onkeydown(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement).tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA' || e.ctrlKey || e.altKey) return;
    const key = e.key.toLowerCase();
    if (key === 'x' && zone) cutZone();
    else if ((key === 'delete' || key === 'backspace') && fx.selected) remove(fx.selected.kind, fx.selected.index);
    else if (key === 'escape' && (zone || fx.selected)) {
      zone = null;
      fx.selected = null;
    } else if (key === ' ' && tag !== 'BUTTON') toggle();
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
      <span class="selection" class:trimmed={trimmed || hasEffects(fx)}>{t('editor.selection', { duration: `${(hasEffects(fx) ? played : selection).toFixed(1)} s` })}</span>
    </div>

    {#if frameable}
      <span class="divider"></span>
      <button
        class="chip format"
        class:on={framing.vertical}
        aria-pressed={framing.vertical}
        onclick={() => (framing.vertical = !framing.vertical)}
        use:tooltip={{ text: t('editor.verticalHint'), side: 'top' }}
      >
        <Icon name="vertical" size={14} />{t('editor.vertical')}
      </button>
    {/if}

    <div class="actions">
      <button class="button primary" onclick={() => (exporting = true)} disabled={!info}>
        <Icon name="share" size={14} />{t('export.title')}
      </button>
    </div>
  </div>

  <div class="tools">
    {#if selected}
      {@const kind = selected.kind}
      {@const i = selected.index}
      <span class="tools-label strong">{t(`editor.kind.${kind}`)}</span>
      {#if kind === 'cut'}
        <span class="tools-label">{t('editor.cutHint')}</span>
        <button class="tool" onclick={() => remove('cut', i)}><Icon name="undo" size={13} />{t('editor.restore')}</button>
      {:else if kind === 'slow' && fx.slow[i]}
        <Segmented label={t('editor.slow')} options={rates} value={fx.slow[i].rate} onchange={(v) => (fx.slow[i].rate = v)} />
      {:else if kind === 'mute' && fx.mutes[i]}
        <Segmented label={t('editor.muteZone')} options={tracks} value={fx.mutes[i].track} onchange={(v) => (fx.mutes[i].track = v)} />
        <label class="inline-toggle">
          <Toggle label={t('editor.beep')} checked={fx.mutes[i].beep} onchange={(v) => (fx.mutes[i].beep = v)} />
          {t('editor.beep')}
        </label>
      {:else if kind === 'blur' && fx.blurs[i]}
        <Segmented
          label={t('editor.blur')}
          options={[
            { value: 'all', label: t('editor.blurWhole') },
            { value: 'zone', label: t('editor.blurZone') },
          ]}
          value={fx.blurs[i].start === undefined ? 'all' : 'zone'}
          onchange={(v) => {
            const blur = fx.blurs[i];
            if (v === 'all') {
              delete blur.start;
              delete blur.end;
            } else {
              blur.start = Math.max(start, current - 1.5);
              blur.end = Math.min(end, blur.start + 3);
            }
          }}
        />
        <span class="tools-label">{t('editor.blurHint')}</span>
      {:else if kind === 'text' && fx.texts[i]}
        <input class="text-input" type="text" maxlength="80" bind:value={fx.texts[i].text} placeholder={t('editor.textDefault')} aria-label={t('editor.text')} />
        <Segmented label={t('editor.textSize.label')} options={sizes} value={fx.texts[i].size} onchange={(v) => (fx.texts[i].size = v)} />
        <Segmented label={t('editor.textStyle.label')} options={styles} value={fx.texts[i].style} onchange={(v) => (fx.texts[i].style = v)} />
      {/if}
      <div class="tools-end">
        {#if kind !== 'cut'}
          <button class="tool danger" onclick={() => remove(kind, i)} use:tooltip={{ text: `${t('editor.remove')} · Suppr`, side: 'top' }}>
            <Icon name="trash" size={13} />{t('editor.remove')}
          </button>
        {/if}
        <button class="tool" onclick={() => (fx.selected = null)}><Icon name="check" size={13} />{t('editor.done')}</button>
      </div>
    {:else}
      <span class="tools-label" class:strong={!!zone}>
        {zone ? t('editor.zone', { duration: `${zoneLength.toFixed(1)} s` }) : t('editor.toolsHint')}
      </span>
      <button class="tool" disabled={!zone} onclick={cutZone} use:tooltip={{ text: `${t('editor.cutTip')} · X`, side: 'top' }}>
        <Icon name="scissors" size={13} />{t('editor.cut')}
      </button>
      <button
        class="tool"
        disabled={!zone}
        onclick={(e) => menuAt(e, tracks.map((tr) => ({ label: tr.label, run: () => muteZone(tr.value, false) })))}
        use:tooltip={{ text: t('editor.muteTip'), side: 'top' }}
      >
        <Icon name="mute" size={13} />{t('editor.muteZone')}<Icon name="chevronDown" size={11} />
      </button>
      <button
        class="tool"
        disabled={!zone}
        onclick={(e) => menuAt(e, tracks.map((tr) => ({ label: tr.label, run: () => muteZone(tr.value, true) })))}
        use:tooltip={{ text: t('editor.beepTip'), side: 'top' }}
      >
        <Icon name="zap" size={13} />{t('editor.beep')}<Icon name="chevronDown" size={11} />
      </button>
      <button
        class="tool"
        disabled={!zone}
        onclick={(e) => menuAt(e, rates.map((r) => ({ label: r.label, run: () => slowZone(r.value) })))}
        use:tooltip={{ text: t('editor.slowTip'), side: 'top' }}
      >
        <Icon name="clock" size={13} />{t('editor.slow')}<Icon name="chevronDown" size={11} />
      </button>
      <button class="tool" onclick={addText} use:tooltip={{ text: t('editor.textTip'), side: 'top' }}><b class="t">T</b>{t('editor.text')}</button>
      <button class="tool" onclick={addBlur} use:tooltip={{ text: t('editor.blurTip'), side: 'top' }}><Icon name="grid" size={13} />{t('editor.blur')}</button>
      {#if zone}
        <button class="tool ghost" onclick={() => (zone = null)} aria-label={t('editor.clearZone')} use:tooltip={{ text: `${t('editor.clearZone')} · Échap`, side: 'top' }}>
          <Icon name="x" size={13} />
        </button>
      {/if}
    {/if}
  </div>

  {#if info}
    <Timeline
      {duration}
      bind:start
      bind:end
      bind:zone
      {current}
      src={shard.path ? mediaSrc(shard.path) : null}
      tint={gameTint(shard.game)}
      {lanes}
      onseek={seek}
      ongain={(key, gain) => (gains[key] = gain)}
    />
  {/if}

  {#if !live && hasMic && (gains.game !== 1 || gains.mic !== 1)}
    <p class="note"><Icon name="info" size={13} />{t('editor.volumesOnExport')}</p>
  {/if}
</div>

{#if exporting}
  <ExportDialog
    {shard}
    {start}
    {end}
    gains={{ ...gains }}
    effects={hasEffects(fx) ? snapshot() : null}
    size={info ? [info.width, info.height] : null}
    onclose={() => (exporting = false)}
  />
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
    flex-wrap: wrap;
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
  .chip.format {
    display: inline-flex;
    gap: 6px;
    font-size: 12.5px;
    font-weight: 600;
    white-space: nowrap;
  }
  .chip.format.on {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
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

  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    min-height: 34px;
    padding: 4px 6px 4px 12px;
    border-radius: 10px;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .tools-label {
    margin-right: 6px;
    font-size: 12.5px;
    color: var(--text-3);
  }
  .tools-label.strong {
    font-weight: 600;
    color: var(--text);
  }
  .tool {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
    background: var(--surface-3);
    white-space: nowrap;
    transition:
      background var(--fast),
      color var(--fast),
      opacity var(--fast);
  }
  .tool:hover:not(:disabled) {
    color: var(--text);
    background: var(--surface-4);
  }
  .tool:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .tool.ghost {
    padding: 0 8px;
    background: none;
  }
  .tool.danger:hover {
    color: #ff6b6b;
  }
  .tool .t {
    font-weight: 800;
    font-size: 13px;
  }
  .tools-end {
    display: flex;
    gap: 6px;
    margin-left: auto;
  }
  .inline-toggle {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin-left: 6px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .text-input {
    width: 220px;
    height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    font-size: 13px;
    color: var(--text);
    background: var(--surface-3);
    border: none;
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .text-input:focus {
    outline: none;
    box-shadow: inset 0 0 0 1.5px var(--accent);
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
    background: var(--surface-4);
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
