<!-- Creating the best-of: a new shard in the Glacier (named), a file anywhere, or a file under a
     Discord limit (copied, ready to paste). Then progress, cancel, and what to do next. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { fade, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon, { type IconName } from './Icon.svelte';
  import { api, errorCode, native, on, type Composition, type Shard } from '$lib/api';
  import { MB, formatDuration, formatSize, hasKey, t } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { portal } from '$lib/portal';
  import { showError } from '$lib/shard-actions';
  import { toast, ui } from '$lib/ui.svelte';

  let { composition, duration, onclose }: { composition: Composition; duration: number; onclose: () => void } = $props();

  type Target = 'glacier' | 'file' | 'discord';
  const limits = [20, 50, 500] as const;
  const options: { id: Target; icon: IconName }[] = [
    { id: 'glacier', icon: 'grid' },
    { id: 'file', icon: 'drive' },
    { id: 'discord', icon: 'share' },
  ];
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const real = $derived(native && !library.demo);

  let target = $state<Target>('glacier');
  let limit = $state<(typeof limits)[number]>(20);
  let name = $state(t('bestof.defaultName', { date: new Date().toLocaleDateString() }).replace(/[\\/:*?"<>|]/g, '-'));
  let stage = $state<'choose' | 'working' | 'done'>('choose');
  let progress = $state(0);
  let result = $state<{ path: string; size: number; shard: Shard | null } | null>(null);
  let dialog: HTMLDivElement;
  onMount(() => dialog.focus());

  async function go() {
    let path: string | null = null;
    if (target === 'file') {
      if (!real) {
        toast({ kind: 'info', title: t('toast.demo') });
        return;
      }
      path = await api.chooseExportPath(`${name.trim() || 'Best-of'}.mp4`).catch((e) => {
        showError(errorCode(e));
        return null;
      });
      if (!path) return;
    }
    stage = 'working';
    progress = 0;
    if (!real) {
      for (let p = 0; p <= 1.001; p += 0.04) {
        progress = p;
        await new Promise((r) => setTimeout(r, 50));
      }
      result = { path: '', size: duration * 2 * MB, shard: null };
      stage = 'done';
      return;
    }
    const stop = on('export-progress', (p) => {
      if (p.id === 'composition') progress = p.progress;
    });
    try {
      const done = await api.exportComposition({
        ...composition,
        target,
        maxBytes: target === 'discord' ? limit * 1_000_000 : null,
        path,
        name: target === 'glacier' ? name.trim() || null : null,
      });
      if (done.shard) library.upsert(done.shard);
      result = { path: done.path, size: done.size, shard: done.shard };
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

  function open() {
    if (result?.shard) ui.playing = result.shard.id;
    onclose();
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') {
      if (stage === 'working' && real) api.cancelExport();
      else if (stage !== 'working') onclose();
    } else if (e.key === 'Enter' && stage === 'choose' && (e.target as HTMLElement).tagName !== 'BUTTON') go();
  }
</script>

<div class="backdrop" use:portal={'body'} transition:fade={{ duration: 180 * motion }} onpointerdown={() => stage !== 'working' && onclose()} role="presentation"></div>
<div class="center" use:portal={'body'}>
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div bind:this={dialog} class="dialog" role="dialog" aria-modal="true" aria-label={t('bestof.create')} tabindex="-1" {onkeydown} in:scale={{ start: 0.96, duration: 240 * motion, easing: cubicOut }}>
    <header>
      <div>
        <h2>{t('bestof.create')}</h2>
        <p>{t('bestof.clips', { count: composition.clips.length })} · {formatDuration(duration)}{composition.vertical ? ' · 9:16' : ''}</p>
      </div>
      <button class="close" onclick={onclose} aria-label={t('common.close')} disabled={stage === 'working'}><Icon name="x" size={16} /></button>
    </header>

    {#if stage === 'choose'}
      <div class="targets" role="radiogroup" aria-label={t('bestof.create')}>
        {#each options as option (option.id)}
          <button class="target" class:on={target === option.id} role="radio" aria-checked={target === option.id} onclick={() => (target = option.id)}>
            <span class="target-icon"><Icon name={option.icon} size={17} /></span>
            <span class="target-text">
              <strong>{t(`bestof.target.${option.id}`)}</strong>
              <span>{t(`bestof.target.${option.id}Hint`)}</span>
            </span>
            <span class="radio"></span>
          </button>
        {/each}
      </div>
      {#if target === 'glacier' || target === 'file'}
        <label class="name">
          <span>{t('bestof.name')}</span>
          <input type="text" bind:value={name} maxlength="80" />
        </label>
      {:else}
        <div class="limits">
          {#each limits as l (l)}
            <button class="limit" class:on={limit === l} onclick={() => (limit = l)}>
              <span>{t(`export.nitro.${l}`)}</span>
              <strong>{formatSize(l * MB)}</strong>
            </button>
          {/each}
        </div>
      {/if}
      <footer>
        <span class="note">{t('bestof.reencoded')}</span>
        <button class="button primary" onclick={go}><Icon name="sparkles" size={14} />{t('bestof.create')}</button>
      </footer>
    {:else if stage === 'working'}
      <div class="working">
        <div class="meter"><span style:transform="scaleX({progress})"></span></div>
        <span>{t('bestof.working', { percent: Math.round(progress * 100) })}</span>
        <button class="button" onclick={() => real && api.cancelExport()}>{t('common.cancel')}</button>
      </div>
    {:else if result}
      <div class="done">
        <span class="done-icon"><Icon name="check" size={22} stroke={2.6} /></span>
        <strong>{t(target === 'discord' ? 'bestof.doneDiscord' : 'bestof.done')}</strong>
        <span>{formatSize(result.size)}</span>
        <div class="buttons">
          {#if result.shard}
            <button class="button primary" onclick={open}><Icon name="play" size={13} fill />{t('bestof.watch')}</button>
          {:else if target === 'file' && result.path}
            <button class="button" onclick={() => api.revealExport(result!.path)}><Icon name="folder" size={14} />{t('bestof.reveal')}</button>
          {/if}
          <button class="button" onclick={onclose}>{t('common.close')}</button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
    background: rgba(4, 5, 7, 0.6);
    backdrop-filter: blur(6px);
  }
  .center {
    position: fixed;
    inset: 0;
    z-index: 101;
    display: grid;
    place-items: center;
    pointer-events: none;
  }
  .dialog {
    width: min(520px, calc(100vw - 32px));
    padding: 20px;
    border-radius: 16px;
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--border-strong),
      0 24px 60px rgba(0, 0, 0, 0.55);
    pointer-events: auto;
    outline: none;
  }
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 16px;
  }
  h2 {
    margin: 0;
    font-size: 17px;
  }
  header p {
    margin: 4px 0 0;
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
  }
  .close:hover {
    color: var(--text);
    background: var(--surface-3);
  }
  .targets {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .target {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px;
    border-radius: 12px;
    text-align: left;
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .target.on {
    box-shadow: inset 0 0 0 1.5px var(--accent);
    background: color-mix(in srgb, var(--accent) 7%, var(--surface-2));
  }
  .target-icon {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    flex: none;
    border-radius: 9px;
    background: var(--surface-3);
  }
  .target-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
  }
  .target-text strong {
    font-size: 13.5px;
  }
  .target-text span {
    font-size: 12px;
    color: var(--text-3);
  }
  .radio {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1.5px var(--border-strong);
  }
  .target.on .radio {
    box-shadow: inset 0 0 0 5px var(--accent);
  }
  .name {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 14px;
    font-size: 12.5px;
    color: var(--text-2);
  }
  .name input {
    height: 34px;
    padding: 0 12px;
    border-radius: 8px;
    font-size: 13.5px;
    color: var(--text);
    background: var(--surface-2);
    border: none;
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .name input:focus {
    outline: none;
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  .limits {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
    margin-top: 14px;
  }
  .limit {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 10px 12px;
    border-radius: 10px;
    text-align: left;
    font-size: 11.5px;
    color: var(--text-3);
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--border);
  }
  .limit strong {
    font-size: 16px;
    color: var(--text);
  }
  .limit.on {
    box-shadow: inset 0 0 0 1.5px var(--accent);
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-top: 18px;
  }
  .note {
    font-size: 12px;
    color: var(--text-3);
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
  }
  .button:hover {
    background: var(--surface-4);
  }
  .button.primary {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .working {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 14px;
    padding: 18px 0 6px;
    font-size: 13px;
    color: var(--text-2);
  }
  .meter {
    width: 100%;
    height: 8px;
    border-radius: 4px;
    overflow: hidden;
    background: var(--surface-3);
  }
  .meter span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transition: transform 120ms linear;
  }
  .done {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 10px 0 4px;
    font-size: 13px;
    color: var(--text-3);
  }
  .done strong {
    font-size: 15px;
    color: var(--text);
  }
  .done-icon {
    display: grid;
    place-items: center;
    width: 46px;
    height: 46px;
    border-radius: 50%;
    color: var(--accent-fg);
    background: var(--accent);
  }
  .buttons {
    display: flex;
    gap: 8px;
    margin-top: 10px;
  }
</style>
