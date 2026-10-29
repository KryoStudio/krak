<!-- Export a clip (with its current trim and volumes): Discord with a guided size limit, save the
     file anywhere, or a copy in the Glacier. Then progress, then what to do next. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { fade, fly, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon, { type IconName } from './Icon.svelte';
  import { api, errorCode, native, on, type ExportRequest, type Shard } from '$lib/api';
  import { MB, formatDuration, formatSize, hasKey, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import Logo from './Logo.svelte';
  import Toggle from './Toggle.svelte';
  import { displayTitle, showError } from '$lib/shard-actions';
  import { toast } from '$lib/ui.svelte';

  let {
    shard,
    start,
    end,
    gains,
    onclose,
  }: {
    shard: Shard;
    start: number;
    end: number;
    gains: { game: number; mic: number };
    onclose: () => void;
  } = $props();

  type Target = ExportRequest['target'];
  const limits = [10, 50, 500] as const;
  type Limit = (typeof limits)[number];

  let target = $state<Target>('discord');
  let limit = $state<Limit>(readLimit());
  let stage = $state<'choose' | 'working' | 'done'>('choose');
  let progress = $state(0);
  let result = $state<{ path: string; size: number; reencoded: boolean } | null>(null);
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  let dialog: HTMLDivElement;
  // Keys go to the dialog, not to the editor and player behind it.
  onMount(() => dialog.focus());
  const real = $derived(native && !library.demo);

  const selection = $derived(Math.max(0, end - start));
  /** The trimmed clip at its current quality, roughly. */
  const estimate = $derived(shard.size * (shard.duration ? selection / shard.duration : 1));

  function readLimit(): Limit {
    try {
      const saved = Number(localStorage.getItem('krak.discord-limit') ?? localStorage.getItem('kryo-shard.discord-limit'));
      return limits.find((l) => l === saved) ?? 10;
    } catch {
      return 10;
    }
  }

  function pickLimit(l: Limit) {
    limit = l;
    try {
      localStorage.setItem('krak.discord-limit', String(l));
    } catch {
      // no storage
    }
  }

  const options: { id: Target; icon: IconName }[] = [
    { id: 'discord', icon: 'share' },
    { id: 'file', icon: 'drive' },
    { id: 'copy', icon: 'copy' },
  ];

  async function go() {
    let path: string | null = null;
    if (target === 'file') {
      if (!real) {
        toast({ kind: 'info', title: t('toast.demo') });
        return;
      }
      // An untitled clip keeps its file name, which has the date ("Valorant 2026-10-29 21-04-37").
      const name = shard.title ?? shard.path.split(/[\\/]/).pop()?.replace(/\.mp4$/i, '') ?? displayTitle(shard);
      path = await api.chooseExportPath(`${name}.mp4`).catch((e) => {
        showError(errorCode(e));
        return null;
      });
      if (!path) return;
    }

    stage = 'working';
    progress = 0;
    if (!real) {
      for (let p = 0; p <= 1.001; p += 0.05) {
        progress = p;
        await new Promise((r) => setTimeout(r, 60));
      }
      result = { path: '', size: Math.min(estimate, limit * 0.95e6), reencoded: estimate > limit * 0.95e6 };
      stage = 'done';
      return;
    }

    const stop = on('export-progress', (p) => {
      if (p.id === shard.id) progress = p.progress;
    });
    try {
      const done = await api.exportShard({
        id: shard.id,
        start,
        end,
        gains: { ...gains },
        target,
        maxBytes: target === 'discord' ? limit * 1_000_000 : null,
        path,
        watermark: settings.value.watermark,
      });
      if (done.shard) library.upsert(done.shard);
      result = done;
      stage = 'done';
    } catch (e) {
      const code = errorCode(e);
      stage = 'choose';
      if (code === 'cancelled') toast({ kind: 'info', title: t('error.cancelled') });
      else showError(hasKey(`error.${code}`) ? code : 'export_failed');
    } finally {
      stop.then((unlisten) => unlisten());
    }
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') {
      if (stage === 'working' && real) api.cancelExport();
      else onclose();
    } else if (e.key === 'Enter' && stage === 'choose') go();
    else if (e.key === 'Enter' && stage === 'done') onclose();
  }

  const doneTitle = $derived(
    target === 'discord' ? t('export.doneDiscord') : target === 'file' ? t('export.doneFile') : t('export.doneCopy'),
  );
</script>

<div class="backdrop" transition:fade={{ duration: 180 * motion }} onpointerdown={() => stage !== 'working' && onclose()} role="presentation"></div>
<div class="center">
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div bind:this={dialog} class="dialog" role="dialog" aria-modal="true" aria-label={t('export.title')} tabindex="-1" {onkeydown} in:scale={{ start: 0.96, duration: 260 * motion, easing: cubicOut }} out:scale={{ start: 0.98, duration: 140 * motion }}>
    <header>
      <div>
        <h2>{t('export.title')}</h2>
        <p>{displayTitle(shard)} · {formatDuration(selection)}</p>
      </div>
      <button class="close" onclick={onclose} aria-label={t('common.close')} disabled={stage === 'working'}><Icon name="x" size={16} /></button>
    </header>

    {#if stage === 'choose'}
      <div class="body" in:fly={{ y: 8, duration: 220 * motion }}>
        <div class="targets" role="radiogroup" aria-label={t('export.title')}>
          {#each options as option (option.id)}
            <button class="target" class:on={target === option.id} role="radio" aria-checked={target === option.id} onclick={() => (target = option.id)}>
              <span class="target-icon"><Icon name={option.icon} size={17} /></span>
              <span class="target-text">
                <strong>{t(`export.${option.id}`)}</strong>
                <span>{t(`export.${option.id}Hint`)}</span>
              </span>
              <span class="radio"></span>
            </button>
          {/each}
        </div>

        {#if target === 'discord'}
          <div class="discord" transition:fly={{ y: -6, duration: 200 * motion }}>
            <span class="question">{t('export.discordChoose')}</span>
            <div class="limits">
              {#each limits as l (l)}
                {@const fits = estimate <= l * 0.95e6}
                <button class="limit" class:on={limit === l} onclick={() => pickLimit(l)}>
                  <span class="limit-name">{t(`export.nitro.${l}`)}</span>
                  <strong>{formatSize(l * MB)}</strong>
                  <span class="limit-note" class:fits>{fits ? t('export.fits') : t('export.reduced')}</span>
                </button>
              {/each}
            </div>
          </div>
        {/if}
      </div>

      <div class="badge-row">
        <span class="badge-preview" class:on={settings.value.watermark}>
          <Logo size={13} /><span>by</span><b>krak</b>
        </span>
        <div class="badge-text">
          <strong>{t('export.watermark')}</strong>
          <span>{t('export.watermarkHint')}</span>
        </div>
        <Toggle label={t('export.watermark')} checked={settings.value.watermark} onchange={(v) => settings.update({ watermark: v }).catch(() => {})} />
      </div>

      <footer>
        <span class="estimate">≈ {formatSize(target === 'discord' ? Math.min(estimate, limit * 0.95e6) : estimate)}</span>
        <button class="primary" onclick={go}><Icon name={options.find((o) => o.id === target)?.icon ?? 'share'} size={15} />{t('export.go')}</button>
      </footer>
    {:else if stage === 'working'}
      <div class="status" in:fade={{ duration: 160 * motion }}>
        <div class="ring" style:--p={progress}>
          <span>{Math.round(progress * 100)} %</span>
        </div>
        <p>{t('export.working', { percent: Math.round(progress * 100) })}</p>
        {#if real}
          <button class="secondary" onclick={() => api.cancelExport()}>{t('common.cancel')}</button>
        {/if}
      </div>
    {:else}
      <div class="status" in:scale={{ start: 0.96, duration: 260 * motion, easing: cubicOut }}>
        <div class="done-icon"><Icon name="check" size={30} stroke={2.4} /></div>
        <h3>{doneTitle}</h3>
        <p>
          {#if target === 'discord'}{t('export.pasteHint')}{/if}
          {#if result}<span class="size">{formatSize(result.size)}{result.reencoded ? ` · ${t('export.reduced')}` : ''}</span>{/if}
        </p>
        <div class="done-actions">
          {#if real && result?.path && target !== 'discord'}
            <button class="secondary" onclick={() => result && api.revealExport(result.path).catch(() => {})}>
              <Icon name="folder" size={14} />{t('export.reveal')}
            </button>
          {/if}
          <button class="primary" onclick={onclose}>{t('export.done')}</button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    background: rgba(5, 6, 8, 0.6);
    backdrop-filter: blur(4px);
  }
  .center {
    position: fixed;
    inset: 0;
    z-index: 51;
    display: grid;
    place-items: center;
    padding: 24px;
    pointer-events: none;
  }
  .dialog {
    display: flex;
    flex-direction: column;
    width: min(560px, 100%);
    padding: 20px 22px;
    border-radius: 18px;
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--border-strong),
      0 40px 100px -30px rgba(0, 0, 0, 0.9);
    pointer-events: auto;
    outline: none;
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 18px;
  }
  h2 {
    font-size: 18px;
    font-weight: 650;
  }
  header p {
    margin-top: 2px;
    font-size: 12.5px;
    color: var(--text-3);
  }
  .close {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    color: var(--text-3);
    transition:
      background var(--fast),
      color var(--fast);
  }
  .close:hover:not(:disabled) {
    color: var(--text);
    background: rgba(255, 255, 255, 0.06);
  }

  .targets {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .target {
    display: flex;
    align-items: center;
    gap: 13px;
    padding: 12px 14px;
    border-radius: 12px;
    text-align: left;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      box-shadow var(--normal),
      background var(--normal);
  }
  .target:hover {
    background: var(--surface-2);
  }
  .target.on {
    background: color-mix(in srgb, var(--accent) 7%, var(--bg));
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .target-icon {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    flex: none;
    border-radius: 10px;
    color: var(--text-2);
    background: var(--surface-3);
    transition:
      color var(--normal),
      background var(--normal);
  }
  .on .target-icon {
    color: var(--accent);
    background: var(--accent-soft);
  }
  .target-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .target-text strong {
    font-size: 14px;
    font-weight: 600;
  }
  .target-text span {
    font-size: 12.5px;
    color: var(--text-3);
  }
  .radio {
    width: 18px;
    height: 18px;
    flex: none;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
    transition: box-shadow var(--normal);
  }
  .on .radio {
    box-shadow:
      inset 0 0 0 5px var(--accent),
      inset 0 0 0 9px var(--accent-fg);
  }

  .discord {
    margin-top: 16px;
  }
  .question {
    display: block;
    margin: 0 0 8px 2px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--text-2);
  }
  .limits {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }
  .limit {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px;
    border-radius: 12px;
    text-align: left;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      box-shadow var(--normal),
      background var(--normal);
  }
  .limit:hover {
    background: var(--surface-2);
  }
  .limit.on {
    background: color-mix(in srgb, var(--accent) 7%, var(--bg));
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .limit-name {
    font-size: 12px;
    color: var(--text-3);
  }
  .limit strong {
    font-size: 17px;
    font-weight: 650;
  }
  .limit-note {
    margin-top: 4px;
    font-size: 11.5px;
    font-weight: 600;
    color: #f5a524;
  }
  .limit-note.fits {
    color: var(--success);
  }

  .badge-row {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 16px;
    padding: 12px 14px;
    border-radius: 12px;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .badge-preview {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 10px 0 8px;
    flex: none;
    border-radius: 13px;
    font-size: 11px;
    color: rgba(232, 241, 242, 0.7);
    background: rgba(10, 14, 20, 0.62);
    box-shadow: inset 0 0 0 1px rgba(232, 241, 242, 0.14);
    opacity: 0.45;
    transition: opacity var(--normal);
  }
  .badge-preview.on {
    opacity: 1;
  }
  .badge-preview b {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: -0.05em;
    color: var(--text);
  }
  .badge-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .badge-text strong {
    font-size: 13px;
    font-weight: 600;
  }
  .badge-text span {
    font-size: 12px;
    color: var(--text-3);
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 20px;
  }
  .estimate {
    font-size: 13px;
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .primary,
  .secondary {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 38px;
    padding: 0 18px;
    border-radius: 10px;
    font-weight: 600;
    transition:
      filter var(--fast),
      background var(--fast),
      transform var(--fast) var(--ease-out);
  }
  .primary {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: 0 10px 26px -12px var(--accent);
  }
  .primary:hover {
    filter: brightness(1.08);
  }
  .primary:active,
  .secondary:active {
    transform: scale(0.97);
  }
  .secondary {
    background: rgba(255, 255, 255, 0.06);
  }
  .secondary:hover {
    background: rgba(255, 255, 255, 0.1);
  }

  .status {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 18px 0 8px;
    text-align: center;
  }
  .status p {
    color: var(--text-2);
  }
  .size {
    display: block;
    margin-top: 4px;
    font-size: 12.5px;
    color: var(--text-3);
  }
  .ring {
    display: grid;
    place-items: center;
    width: 96px;
    height: 96px;
    border-radius: 50%;
    background: conic-gradient(var(--accent) calc(var(--p) * 1turn), var(--surface-3) 0);
    transition: background 200ms linear;
  }
  .ring span {
    display: grid;
    place-items: center;
    width: 80px;
    height: 80px;
    border-radius: 50%;
    background: var(--surface);
    font-size: 16px;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }
  .done-icon {
    display: grid;
    place-items: center;
    width: 72px;
    height: 72px;
    border-radius: 50%;
    color: var(--success);
    background: color-mix(in srgb, var(--success) 14%, transparent);
    animation: pop 500ms var(--ease-out);
  }
  h3 {
    font-size: 17px;
    font-weight: 650;
  }
  .done-actions {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }

  @keyframes pop {
    from {
      transform: scale(0.5);
      opacity: 0;
    }
  }
</style>
