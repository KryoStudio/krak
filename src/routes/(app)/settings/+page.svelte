<script lang="ts">
  import type { Snippet } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import Icon from '$lib/components/Icon.svelte';
  import Logo from '$lib/components/Logo.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import Segmented from '$lib/components/Segmented.svelte';
  import HotkeyInput from '$lib/components/HotkeyInput.svelte';
  import { api, native, type Quality, type Settings } from '$lib/api';
  import { capture, estimateMemory } from '$lib/capture.svelte';
  import { formatSize, hasKey, t, type Lang } from '$lib/i18n.svelte';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import { showError } from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { updates } from '$lib/updates.svelte';

  const s = $derived(settings.value);
  const REPO = 'https://github.com/KryoStudio/kryo-shard';

  let version = $state('');
  $effect(() => {
    if (native) getVersion().then((v) => (version = v));
    else version = 'dev';
  });

  let hotkeyError = $state('');

  async function setHotkey(hotkey: string) {
    hotkeyError = '';
    try {
      await settings.update({ hotkey });
    } catch (code) {
      hotkeyError = t(code === 'hotkey_taken' ? 'error.hotkey_taken' : 'error.hotkey_invalid');
    }
  }

  function set(patch: Partial<Settings>) {
    settings.update(patch).catch(showError);
  }

  const status = $derived(capture.status);
  const engineLine = $derived.by(() => {
    if (status.state === 'starting') return t('capture.starting');
    if (status.state === 'paused') return t('capture.paused');
    if (status.state === 'error') {
      const key = `capture.error.${status.error}`;
      return t(hasKey(key) ? key : 'capture.error.capture_failed');
    }
    const parts = [status.encoder ? t(`encoder.${status.encoder}`) : '', status.width ? `${status.width}×${status.height}` : '', status.fps ? `${status.fps} ${t('settings.fpsUnit')}` : ''];
    return parts.filter(Boolean).join(' · ');
  });
  const memory = $derived(t('settings.memory', { size: formatSize(estimateMemory(s.bufferSeconds, s.fps, s.quality, s.microphone)) }));

  const buffers = [15, 30, 60, 120, 300].map((v) => ({ value: v, label: v < 60 ? `${v} s` : `${v / 60} min` }));
  const qualities: Quality[] = ['balanced', 'high', 'ultra'];
  const accents = [
    { id: 'glacier', color: '#5aa9ff' },
    { id: 'aurora', color: '#9a8cff' },
    { id: 'jade', color: '#3fd49a' },
    { id: 'ember', color: '#ff9152' },
    { id: 'rose', color: '#ff7aa2' },
    { id: 'frost', color: '#e6e8ec' },
  ] as const;
</script>

{#snippet row(label: string, hint: string, control: Snippet, error = false)}
  <div class="row">
    <div class="text">
      <span class="label">{label}</span>
      {#if hint}<span class="hint" class:error>{hint}</span>{/if}
    </div>
    <div class="control">{@render control()}</div>
  </div>
{/snippet}

<div class="page">
  <h1>{t('nav.settings')}</h1>

  <!-- Capture -->
  <section>
    <h2>{t('settings.capture')}</h2>
    {#if status.audioError}
      <p class="notice"><Icon name="alert" size={15} />{t('capture.audioError')}</p>
    {/if}
    <div class="card">
      {#snippet engine()}
        {#if status.state === 'ready' || status.state === 'paused' || status.state === 'error'}
          <button class="button" onclick={() => capture.setPaused(status.state === 'ready').catch(showError)}>
            <Icon name={status.state === 'ready' ? 'pause' : 'play'} size={13} />
            {t(status.state === 'ready' ? 'capture.pause' : 'capture.resume')}
          </button>
        {/if}
      {/snippet}
      <div class="engine" data-state={status.state}>
        {@render row(t('settings.engine'), engineLine, engine, status.state === 'error')}
      </div>

      {#snippet hotkey()}
        <HotkeyInput value={s.hotkey} onchange={setHotkey} />
      {/snippet}
      {@render row(t('settings.hotkey'), hotkeyError || t('settings.hotkeyHint'), hotkey, !!hotkeyError)}

      {#snippet buffer()}
        <Segmented label={t('settings.buffer')} options={buffers} value={s.bufferSeconds} onchange={(v) => set({ bufferSeconds: v })} />
      {/snippet}
      {@render row(t('settings.buffer'), memory, buffer)}

      {#snippet fps()}
        <Segmented
          label={t('settings.fps')}
          options={[30, 60].map((v) => ({ value: v, label: `${v} ${t('settings.fpsUnit')}` }))}
          value={s.fps}
          onchange={(v) => set({ fps: v })}
        />
      {/snippet}
      {@render row(t('settings.fps'), t('settings.fpsHint'), fps)}

      {#snippet quality()}
        <Segmented
          label={t('settings.quality')}
          options={qualities.map((q) => ({ value: q, label: t(`settings.quality.${q}`) }))}
          value={s.quality}
          onchange={(v) => set({ quality: v })}
        />
      {/snippet}
      {@render row(t('settings.quality'), t('settings.qualityHint'), quality)}

      {#snippet mic()}
        <Toggle label={t('settings.microphone')} checked={s.microphone} onchange={(v) => set({ microphone: v })} />
      {/snippet}
      {@render row(t('settings.microphone'), t('settings.microphoneHint'), mic)}

      {#snippet sound()}
        <Toggle label={t('settings.sound')} checked={s.captureSound} onchange={(v) => set({ captureSound: v })} />
      {/snippet}
      {@render row(t('settings.sound'), t('settings.soundHint'), sound)}

      {#snippet overlay()}
        <Toggle label={t('settings.overlay')} checked={s.overlay} onchange={(v) => set({ overlay: v })} />
      {/snippet}
      {@render row(t('settings.overlay'), t('settings.overlayHint'), overlay)}
    </div>
  </section>

  <!-- Storage -->
  <section>
    <h2>{t('settings.storage')}</h2>
    <div class="card">
      {#snippet folder()}
        <div class="buttons">
          <button class="button" onclick={() => settings.chooseFolder().catch(showError)} disabled={!native}>{t('settings.change')}</button>
          <button class="button square" onclick={() => api.openClipsDir()} disabled={!native} aria-label={t('settings.open')} use:tooltip={{ text: t('settings.open'), side: 'top' }}>
            <Icon name="external" size={15} />
          </button>
        </div>
      {/snippet}
      {@render row(t('settings.folder'), s.clipsDir, folder)}
    </div>
  </section>

  <!-- General -->
  <section>
    <h2>{t('settings.general')}</h2>
    <div class="card">
      {#snippet startup()}
        <Toggle label={t('settings.startup')} checked={s.launchAtStartup} onchange={(v) => set({ launchAtStartup: v })} />
      {/snippet}
      {@render row(t('settings.startup'), t('settings.startupHint'), startup)}

      {#snippet tray()}
        <Toggle label={t('settings.tray')} checked={s.closeToTray} onchange={(v) => set({ closeToTray: v })} />
      {/snippet}
      {@render row(t('settings.tray'), t('settings.trayHint'), tray)}

      {#snippet language()}
        <Segmented
          label={t('settings.language')}
          options={[
            { value: null as Lang | null, label: t('settings.language.system') },
            { value: 'fr', label: 'Français' },
            { value: 'en', label: 'English' },
          ]}
          value={s.language}
          onchange={(v) => set({ language: v })}
        />
      {/snippet}
      {@render row(t('settings.language'), '', language)}
    </div>
  </section>

  <!-- Appearance -->
  <section>
    <h2>{t('settings.appearance')}</h2>
    <div class="card">
      {#snippet accent()}
        <div class="swatches" role="radiogroup" aria-label={t('settings.accent')}>
          {#each accents as a (a.id)}
            <button
              role="radio"
              aria-checked={s.accent === a.id}
              aria-label={t(`accent.${a.id}`)}
              class:on={s.accent === a.id}
              style:--c={a.color}
              onclick={() => set({ accent: a.id })}
              use:tooltip={{ text: t(`accent.${a.id}`), side: 'top' }}
            >
              <Icon name="check" size={13} stroke={3} />
            </button>
          {/each}
        </div>
      {/snippet}
      {@render row(t('settings.accent'), t('settings.accentHint'), accent)}
    </div>
  </section>

  {#if import.meta.env.DEV && native}
    <section>
      <h2>{t('settings.developer')}</h2>
      <div class="card">
        {#snippet demo()}
          <Toggle label={t('settings.demo')} checked={library.demo} onchange={(v) => library.setDemo(v)} />
        {/snippet}
        {@render row(t('settings.demo'), t('settings.demoHint'), demo)}
      </div>
    </section>
  {/if}

  <!-- About -->
  <section>
    <h2>{t('settings.about')}</h2>
    <div class="card about">
      <Logo size={40} />
      <div class="text">
        <span class="label">Kryo Shard <span class="version">{version}</span></span>
        <span class="hint">{t('settings.license')}</span>
      </div>
      <div class="buttons">
        {#if native}
          {#if updates.installing !== null}
            <span class="button" aria-live="polite">{t('update.installing', { percent: Math.round(updates.installing * 100) })}</span>
          {:else if updates.available}
            <button class="button accent" onclick={() => updates.install()}>
              {t('update.available', { version: updates.available.version })} · {t('update.install')}
            </button>
          {:else}
            <button class="button" onclick={() => updates.check(false)} disabled={updates.checking}>
              <Icon name={updates.checking ? 'spinner' : 'undo'} size={14} />
              {t(updates.checking ? 'update.checking' : 'update.check')}
            </button>
          {/if}
        {/if}
        <button class="button" onclick={() => (native ? openUrl(REPO) : open(REPO))}>
          <Icon name="external" size={14} />{t('settings.source')}
        </button>
      </div>
    </div>
  </section>
</div>

<style>
  .page {
    max-width: 760px;
    padding: 8px 32px 56px;
  }
  h1 {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.015em;
    margin-bottom: 8px;
  }

  section {
    margin-top: 26px;
  }
  h2 {
    margin: 0 0 10px 2px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-2);
  }
  .notice {
    display: flex;
    gap: 10px;
    margin-bottom: 10px;
    padding: 10px 12px;
    border-radius: var(--radius);
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--text-2);
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .notice :global(svg) {
    flex: none;
    margin-top: 2px;
    color: var(--accent);
  }

  .card {
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    min-height: 64px;
    padding: 12px 16px;
  }
  .row + .row {
    border-top: 1px solid var(--border);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .label {
    font-weight: 500;
  }
  .hint {
    font-size: 12.5px;
    color: var(--text-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hint.error {
    color: var(--danger);
    white-space: normal;
  }
  .engine + :global(.row),
  .engine ~ :global(.row) {
    border-top: 1px solid var(--border);
  }
  .control {
    flex: none;
  }

  .buttons {
    display: flex;
    gap: 6px;
  }
  .button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 12px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 500;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition: background var(--fast);
  }
  .button:hover:not(:disabled) {
    background: #2a2b31;
  }
  .button:disabled {
    opacity: 0.45;
  }
  .button.accent {
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: none;
  }
  .button.square {
    width: 32px;
    padding: 0;
    justify-content: center;
  }

  .swatches {
    display: flex;
    gap: 8px;
  }
  .swatches button {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--c);
    color: rgba(0, 0, 0, 0.75);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.15);
    transition:
      box-shadow var(--normal),
      transform var(--normal) var(--ease-out);
  }
  .swatches button:hover {
    transform: scale(1.08);
  }
  .swatches button :global(svg) {
    opacity: 0;
    transform: scale(0.5);
    transition:
      opacity var(--fast),
      transform var(--normal) var(--ease-out);
  }
  .swatches button.on {
    box-shadow:
      0 0 0 2px var(--surface),
      0 0 0 4px var(--c);
  }
  .swatches button.on :global(svg) {
    opacity: 1;
    transform: none;
  }

  .about {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 16px;
  }
  .about .text {
    flex: 1;
  }
  .version {
    margin-left: 6px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-3);
  }
</style>
