<!-- Home: is the replay ready, how to capture, the latest shard, a few numbers, recent shards. -->
<script lang="ts">
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import { goto } from '$app/navigation';
  import Icon from '$lib/components/Icon.svelte';
  import GameIcon from '$lib/components/GameIcon.svelte';
  import Logo from '$lib/components/Logo.svelte';
  import ShardCard from '$lib/components/ShardCard.svelte';
  import { mediaSrc } from '$lib/api';
  import { capture, codecName, formatClock, saveReplay } from '$lib/capture.svelte';
  import { gameTint } from '$lib/games';
  import { GB, formatAge, formatDuration, formatHotkey, formatSize, hasKey, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import * as actions from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { filterFor, ui } from '$lib/ui.svelte';

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const status = $derived(capture.status);

  const hour = new Date().getHours();
  const greeting = $derived(t(hour >= 18 || hour < 5 ? 'home.evening' : 'home.morning'));

  const title = $derived(
    status.state === 'ready'
      ? t('home.ready', { seconds: Math.round(Math.min(status.bufferedSeconds, settings.value.bufferSeconds)) || settings.value.bufferSeconds })
      : status.state === 'paused'
        ? t('home.paused')
        : status.state === 'starting'
          ? t('home.starting')
          : t('home.error'),
  );
  const errorLine = $derived.by(() => {
    if (status.state !== 'error') return '';
    const key = `capture.error.${status.error}`;
    return t(hasKey(key) ? key : 'capture.error.capture_failed');
  });
  const fill = $derived(status.state === 'ready' ? Math.min(1, status.bufferedSeconds / settings.value.bufferSeconds) : 0);
  // The format being recorded. Paused or restarting, the last one stays (dimmed), so the card
  // keeps its height and nothing below it jumps.
  let format = $state<Pick<typeof status, 'encoder' | 'codec' | 'tenBit' | 'height' | 'fps'> | null>(null);
  $effect(() => {
    if (status.state === 'ready') format = { encoder: status.encoder, codec: status.codec, tenBit: status.tenBit, height: status.height, fps: status.fps };
  });
  const chips = $derived(
    format
      ? [
          format.encoder ? t(`encoder.${format.encoder}`) : '',
          format.codec ? `${codecName(format.codec)}${format.tenBit ? ` ${t('settings.tenBitShort')}` : ''}` : '',
          format.height ? `${format.height}p` : '',
          format.fps ? `${format.fps} ${t('settings.fpsUnit')}` : '',
        ].filter(Boolean)
      : [],
  );

  // The latest shard loops on the home page, but not under the player or the first-launch
  // setup, nor while the window is hidden (Krak in the tray while you play).
  let preview = $state<HTMLVideoElement>();
  let visible = $state(true);
  $effect(() => {
    const update = () => (visible = document.visibilityState === 'visible');
    update();
    document.addEventListener('visibilitychange', update);
    return () => document.removeEventListener('visibilitychange', update);
  });
  $effect(() => {
    if (!preview) return;
    if (visible && settings.value.onboarded && !ui.playing) preview.play().catch(() => {});
    else preview.pause();
  });

  const latest = $derived(library.shards.reduce<(typeof library.shards)[number] | undefined>((a, b) => (!a || b.createdAt > a.createdAt ? b : a), undefined));
  const recent = $derived([...library.shards].sort((a, b) => b.createdAt - a.createdAt).slice(1, 5));

  const week = $derived(library.shards.filter((s) => s.createdAt > Date.now() - 7 * 86_400_000).length);
  const used = $derived(library.shards.reduce((sum, s) => sum + s.size, 0));
  const topGame = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const s of library.shards) if (s.game) counts.set(s.game, (counts.get(s.game) ?? 0) + 1);
    return [...counts].sort((a, b) => b[1] - a[1])[0]?.[0] ?? null;
  });

  function openGlacier(game?: string | null) {
    ui.filter = game === undefined ? 'all' : filterFor(game);
    goto('/glacier');
  }
</script>

<div class="home">
  <header in:fly={{ y: 6, duration: 260 * motion, easing: cubicOut }}>
    <h1>{greeting}</h1>
    <p>{t('home.subtitle')}</p>
  </header>

  <section class="hero" data-state={status.state}>
    <div class="glow"></div>
    <div class="hero-main">
      <div class="state">
        <span class="dot"></span>
        <strong>{title}</strong>
      </div>
      {#if errorLine}
        <p class="press error">{errorLine}</p>
      {:else}
        <p class="press">
          {t('home.press')}
          {#each formatHotkey(settings.value.hotkey) as key, i (i)}{#if i > 0}<span class="plus">+</span>{/if}<kbd>{key}</kbd>{/each}
          {t('home.toCapture')}
        </p>
      {/if}
      <button
        class="chips"
        class:dim={status.state !== 'ready'}
        onclick={() => ((ui.settingsSection = 'video'), goto('/settings'))}
        use:tooltip={{ text: t('settings.section.video'), side: 'bottom' }}
      >
        {#each chips as chip (chip)}<span>{chip}</span>{:else}<span class="placeholder">1080p</span>{/each}
      </button>
    </div>
    <div class="hero-side">
      <button class="capture" onclick={() => saveReplay()} disabled={!capture.live}>
        <Icon name="record" size={16} />{t('capture.now')}
      </button>
      <div class="side-row">
        <button
          class="ghost rec"
          class:on={!!status.recording}
          onclick={() => capture.toggleRecording()}
          disabled={!status.recording && !capture.live}
          use:tooltip={{ text: `${t(status.recording ? 'record.stop' : 'record.start')} · ${settings.value.recordHotkey}`, side: 'bottom' }}
        >
          <span class="rec-dot"></span>
          {#if status.recording}
            {t('record.stop')} · {formatClock(status.recording.seconds)}
          {:else}
            {t('record.start')}
          {/if}
        </button>
        {#if status.state !== 'error'}
          <!-- Stays in place while the engine restarts (disabled, so it can't be spammed). -->
          <button
            class="ghost square"
            onclick={() => capture.setPaused(status.state === 'ready').catch(actions.showError)}
            disabled={status.state === 'starting'}
            aria-label={t(status.state === 'paused' ? 'capture.resume' : 'capture.pause')}
            use:tooltip={{ text: t(status.state === 'paused' ? 'capture.resume' : 'capture.pause'), side: 'bottom' }}
          >
            <Icon name={status.state === 'paused' ? 'play' : 'pause'} size={14} />
          </button>
        {:else}
          <button class="ghost square" onclick={() => ((ui.settingsSection = 'capture'), goto('/settings'))} aria-label={t('nav.settings')}>
            <Icon name="settings" size={14} />
          </button>
        {/if}
      </div>
    </div>
    {#if fill > 0 && fill < 1}<span class="fill" style:transform="scaleX({fill})"></span>{/if}
  </section>

  {#if latest}
    <section class="grid">
      <article class="latest">
        <button class="thumb" style:--a={gameTint(latest.game)[0]} style:--b={gameTint(latest.game)[1]} onclick={() => actions.play(latest)} aria-label={t('shard.play')}>
          <span class="art"></span>
          {#if latest.path}
            {#key latest.id}
              <video bind:this={preview} src={mediaSrc(latest.path)} muted loop playsinline></video>
            {/key}
          {/if}
          <span class="shade"></span>
          <span class="label"><Icon name="sparkles" size={13} />{t('home.latest')}</span>
          <span class="play"><Icon name="play" size={20} fill /></span>
        </button>
        <div class="latest-info">
          <div class="latest-text">
            <strong>{actions.displayTitle(latest)}</strong>
            <span>
              {actions.gameName(latest)} · {formatAge(latest.createdAt)}{latest.duration ? ` · ${formatDuration(latest.duration)}` : ''} · {formatSize(latest.size)}
            </span>
          </div>
          <div class="latest-actions">
            <button class="button" onclick={() => actions.edit(latest)}><Icon name="scissors" size={14} />{t('home.edit')}</button>
            <button class="button" onclick={() => actions.copy(latest)} use:tooltip={{ text: t('toast.copiedBody'), side: 'top' }}>
              <Icon name="copy" size={14} />{t('shard.copy')}
            </button>
          </div>
        </div>
      </article>

      <div class="stats">
        <button class="stat" onclick={() => openGlacier()}>
          <span class="stat-label">{t('home.week')}</span>
          <strong>{t('home.weekValue', { count: week })}</strong>
        </button>
        <button class="stat" onclick={() => ((ui.settingsSection = 'storage'), goto('/settings'))}>
          <span class="stat-label">{t('home.storage')}</span>
          <strong>
            {settings.value.storageLimitGb
              ? t('home.storageOf', { used: formatSize(used), limit: formatSize(settings.value.storageLimitGb * GB) })
              : formatSize(used)}
          </strong>
          {#if settings.value.storageLimitGb}
            <span class="meter"><span style:transform="scaleX({Math.min(1, used / (settings.value.storageLimitGb * GB))})"></span></span>
          {/if}
        </button>
        <button class="stat" onclick={() => topGame && openGlacier(topGame)} disabled={!topGame}>
          <span class="stat-label">{t('home.topGame')}</span>
          {#if topGame}
            <strong class="game"><GameIcon game={topGame} size={22} />{topGame}</strong>
          {:else}
            <strong class="muted">{t('home.none')}</strong>
          {/if}
        </button>
      </div>
    </section>

    {#if recent.length}
      <section class="recent">
        <div class="section-head">
          <h2>{t('home.recent')}</h2>
          <button class="link" onclick={() => openGlacier()}>{t('home.seeAll')}<Icon name="arrowRight" size={14} /></button>
        </div>
        <div class="cards">
          {#each recent as shard, i (library.key(shard.id))}
            <div in:fly={{ y: 10, duration: 280 * motion, delay: i * 40 * motion, easing: cubicOut }}>
              <ShardCard {shard} fresh={ui.fresh === shard.id} />
            </div>
          {/each}
        </div>
      </section>
    {/if}
  {:else if !library.loading}
    <section class="empty">
      <div class="mark"><Logo size={40} /></div>
      <h2>{t('home.emptyTitle')}</h2>
      <p>{t('home.emptyBody')}</p>
    </section>
  {/if}
</div>

<style>
  .home {
    display: flex;
    flex-direction: column;
    gap: 22px;
    max-width: 1240px;
    padding: 8px 32px 48px;
  }

  header h1 {
    font-size: 24px;
    font-weight: 650;
    letter-spacing: -0.02em;
  }
  header p {
    margin-top: 2px;
    color: var(--text-3);
  }

  /* Replay status ---------------------------------------------------------------- */
  .hero {
    position: relative;
    display: flex;
    align-items: center;
    gap: 24px;
    padding: 24px 26px;
    border-radius: 18px;
    background: var(--surface);
    border: 1px solid var(--border);
    overflow: hidden;
    isolation: isolate;
  }
  .glow {
    position: absolute;
    inset: 0;
    z-index: -1;
    background:
      radial-gradient(60% 140% at 0% 0%, color-mix(in srgb, var(--accent) 14%, transparent), transparent 70%),
      radial-gradient(40% 120% at 100% 100%, color-mix(in srgb, var(--face) 10%, transparent), transparent 70%);
    opacity: 0.9;
  }
  [data-state='paused'] .glow,
  [data-state='error'] .glow {
    opacity: 0.25;
  }
  .hero-main {
    flex: 1;
    min-width: 0;
  }
  .state {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .state strong {
    font-size: 19px;
    font-weight: 650;
    letter-spacing: -0.01em;
  }
  .dot {
    width: 10px;
    height: 10px;
    flex: none;
    border-radius: 50%;
    background: var(--text-3);
  }
  [data-state='ready'] .dot {
    background: var(--accent);
    box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 20%, transparent);
    animation: breathe 2.4s var(--ease-in-out) infinite;
  }
  [data-state='paused'] .dot {
    background: #f5a524;
  }
  [data-state='error'] .dot {
    background: var(--danger);
  }
  .press {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 12px;
    color: var(--text-2);
  }
  .press kbd {
    height: 26px;
    padding: 0 9px;
    font-size: 13px;
    color: var(--text);
  }
  .plus {
    color: var(--text-3);
  }
  /* Same height as the shortcut line it replaces. */
  .press.error {
    min-height: 26px;
    color: var(--danger);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 14px;
  }
  .chips span {
    padding: 3px 9px;
    border-radius: 999px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-2);
    background: rgba(255, 255, 255, 0.06);
    transition: background var(--fast);
  }
  .chips:hover span {
    background: rgba(255, 255, 255, 0.09);
  }
  .chips {
    transition: opacity var(--normal);
  }
  .chips.dim {
    opacity: 0.45;
  }
  .chips .placeholder {
    visibility: hidden;
  }

  /* A fixed width: the record button's timer or a longer word never squeezes the text. */
  .hero-side {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: none;
    width: 234px;
  }
  .capture,
  .ghost {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-width: 190px;
    height: 40px;
    padding: 0 18px;
    border-radius: 10px;
    font-weight: 600;
    transition:
      filter var(--fast),
      transform var(--fast) var(--ease-out),
      background var(--fast),
      opacity var(--normal);
  }
  .capture {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: 0 10px 26px -12px var(--accent);
  }
  .capture:hover:not(:disabled) {
    filter: brightness(1.08);
  }
  .capture:active:not(:disabled),
  .ghost:active {
    transform: scale(0.97);
  }
  .capture:disabled {
    opacity: 0.4;
    box-shadow: none;
  }
  .ghost {
    height: 36px;
    font-size: 13px;
    color: var(--text-2);
    background: rgba(255, 255, 255, 0.05);
  }
  .ghost:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.08);
  }
  .side-row {
    display: flex;
    gap: 8px;
  }
  .side-row .ghost.rec {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
  }
  .ghost.square {
    min-width: 36px;
    width: 36px;
    padding: 0;
  }
  .ghost:disabled {
    opacity: 0.4;
  }
  .rec-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--rec);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--rec) 20%, transparent);
  }
  .ghost.rec.on {
    color: #fff;
    background: color-mix(in srgb, var(--rec) 22%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--rec) 45%, transparent);
    font-variant-numeric: tabular-nums;
  }
  .ghost.rec.on .rec-dot {
    border-radius: 2px;
    animation: recblink 1.2s var(--ease-in-out) infinite;
  }
  @keyframes recblink {
    50% {
      opacity: 0.35;
    }
  }
  .fill {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 2px;
    background: var(--accent);
    opacity: 0.7;
    transform-origin: left;
    transition: transform 1s linear;
  }

  /* Latest + stats --------------------------------------------------------------------- */
  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 280px;
    gap: 18px;
  }
  .latest {
    display: flex;
    flex-direction: column;
    border-radius: 18px;
    background: var(--surface);
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .thumb {
    position: relative;
    display: block;
    aspect-ratio: 16 / 7;
    overflow: hidden;
    isolation: isolate;
    background: var(--bg);
  }
  .art,
  video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    transition: transform 900ms var(--ease-out);
  }
  .art {
    background:
      radial-gradient(110% 90% at 88% 8%, color-mix(in srgb, var(--b) 55%, transparent), transparent 60%),
      radial-gradient(100% 100% at 8% 100%, color-mix(in srgb, var(--a) 60%, transparent), transparent 65%),
      linear-gradient(135deg, var(--surface-2), var(--bg));
  }
  .thumb:hover :is(.art, video) {
    transform: scale(1.025);
  }
  .shade {
    position: absolute;
    inset: 0;
    background: linear-gradient(to top, rgba(0, 0, 0, 0.5), transparent 45%);
  }
  .label {
    position: absolute;
    left: 14px;
    top: 14px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 600;
    color: #fff;
    background: rgba(0, 0, 0, 0.5);
    backdrop-filter: blur(8px);
  }
  .play {
    position: absolute;
    left: 50%;
    top: 50%;
    display: grid;
    place-items: center;
    width: 56px;
    height: 56px;
    padding-left: 3px;
    border-radius: 50%;
    color: #0a0b0d;
    background: rgba(255, 255, 255, 0.92);
    transform: translate(-50%, -50%) scale(0.9);
    opacity: 0;
    transition:
      opacity var(--normal),
      transform var(--slow) var(--ease-out);
  }
  .thumb:hover .play,
  .thumb:focus-visible .play {
    opacity: 1;
    transform: translate(-50%, -50%);
  }
  .latest-info {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 14px 16px;
  }
  .latest-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .latest-text strong {
    font-size: 15px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .latest-text span {
    font-size: 12.5px;
    color: var(--text-3);
  }
  .latest-actions {
    display: flex;
    gap: 6px;
    flex: none;
  }
  .button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 34px;
    padding: 0 13px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition: background var(--fast);
  }
  .button:hover {
    background: #2a2b31;
  }

  .stats {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: 1;
    padding: 16px;
    border-radius: 14px;
    text-align: left;
    background: var(--surface);
    border: 1px solid var(--border);
    transition:
      background var(--fast),
      border-color var(--fast);
  }
  .stat:hover:not(:disabled) {
    background: var(--surface-2);
    border-color: var(--border-strong);
  }
  .stat:disabled {
    cursor: default;
  }
  .stat-label {
    font-size: 12px;
    font-weight: 500;
    color: var(--text-3);
  }
  .stat strong {
    font-size: 18px;
    font-weight: 650;
    letter-spacing: -0.01em;
    font-variant-numeric: tabular-nums;
  }
  .stat strong.game {
    display: flex;
    align-items: center;
    gap: 9px;
    font-size: 15px;
  }
  .stat strong.muted {
    font-size: 14px;
    font-weight: 500;
    color: var(--text-3);
  }
  .meter {
    height: 5px;
    border-radius: 5px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .meter span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left;
  }

  /* Recent ------------------------------------------------------------------------- */
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }
  .section-head h2 {
    font-size: 15px;
    font-weight: 600;
  }
  .link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-2);
    transition: color var(--fast);
  }
  .link:hover {
    color: var(--accent);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 16px;
  }

  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 56px 24px;
    border-radius: 18px;
    border: 1px dashed var(--border-strong);
    text-align: center;
  }
  .mark {
    display: grid;
    place-items: center;
    width: 76px;
    height: 76px;
    border-radius: 22px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: 0 0 60px -10px color-mix(in srgb, var(--face) 35%, transparent);
  }
  .empty h2 {
    margin-top: 18px;
    font-size: 17px;
    font-weight: 600;
  }
  .empty p {
    max-width: 380px;
    margin-top: 6px;
    color: var(--text-2);
    line-height: 1.6;
  }

  @media (max-width: 1100px) {
    .cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @keyframes breathe {
    50% {
      box-shadow: 0 0 0 7px color-mix(in srgb, var(--accent) 6%, transparent);
    }
  }
</style>
