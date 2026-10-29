<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import { fly } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import { getVersion } from '@tauri-apps/api/app';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import { page } from '$app/state';
  import Icon, { type IconName } from '$lib/components/Icon.svelte';
  import Logo from '$lib/components/Logo.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import Segmented from '$lib/components/Segmented.svelte';
  import Select from '$lib/components/Select.svelte';
  import HotkeyInput from '$lib/components/HotkeyInput.svelte';
  import QualityPicker from '$lib/components/QualityPicker.svelte';
  import { api, native, type Microphone, type ResolvedVideo, type Settings, type StorageInfo } from '$lib/api';
  import { capture, codecName, listMicrophones, memoryFor, resolveVideo, storageInfo, videoLabel } from '$lib/capture.svelte';
  import { GB, formatSize, hasKey, t } from '$lib/i18n.svelte';
  import { languages } from '$lib/locales';
  import { library } from '$lib/library.svelte';
  import { settings } from '$lib/settings.svelte';
  import { showError } from '$lib/shard-actions';
  import { tooltip } from '$lib/tooltip';
  import { ui, type SettingsSection } from '$lib/ui.svelte';
  import { updates } from '$lib/updates.svelte';

  const s = $derived(settings.value);
  const status = $derived(capture.status);
  const encoder = $derived(capture.encoder);
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const REPO = 'https://github.com/KryoStudio/krak';

  const sections: { id: SettingsSection; icon: IconName }[] = [
    { id: 'general', icon: 'settings' },
    { id: 'capture', icon: 'record' },
    { id: 'video', icon: 'film' },
    { id: 'audio', icon: 'volume' },
    { id: 'storage', icon: 'drive' },
    { id: 'appearance', icon: 'palette' },
    { id: 'about', icon: 'info' },
  ];

  let version = $state('');
  let microphones = $state<Microphone[]>([]);
  let storage = $state<StorageInfo | null>(null);
  let output = $state<ResolvedVideo | null>(null);
  let hotkeyError = $state('');

  onMount(() => {
    const wanted = page.url.searchParams.get('section');
    if (wanted && sections.some((x) => x.id === wanted)) ui.settingsSection = wanted as SettingsSection;
    if (native) getVersion().then((v) => (version = v));
    else version = 'dev';
    capture.loadEncoder();
  });

  // Data that only some sections need, refreshed when they open.
  $effect(() => {
    if (ui.settingsSection === 'audio') listMicrophones().then((list) => (microphones = list));
    if (ui.settingsSection === 'storage') {
      void library.shards.length;
      storageInfo().then((info) => (storage = info));
    }
  });

  // What the current settings record, for the "Result" line and the memory estimate.
  $effect(() => {
    void [s.quality, s.fps, s.resolution, s.codec, s.bitrate, s.tenBit, encoder];
    let alive = true;
    resolveVideo().then((r) => alive && (output = r));
    return () => {
      alive = false;
    };
  });

  const memory = $derived(output ? memoryFor(output.bitrate, s.bufferSeconds, s.microphone) : 0);

  let recordError = $state('');
  let screenshotError = $state('');
  let presence = $state<{ enabled: boolean; connected: boolean } | null>(null);

  async function setScreenshotHotkey(screenshotHotkey: string) {
    screenshotError = '';
    try {
      await settings.update({ screenshotHotkey });
    } catch (code) {
      screenshotError = shortcutError(code);
    }
  }

  // Is Discord there to show the presence? Asked when the General section opens.
  $effect(() => {
    if (ui.settingsSection !== 'general' || !native) return;
    void s.discordPresence;
    api.getPresence().then(
      (p) => (presence = p),
      () => (presence = null),
    );
  });

  const today = $derived(library.shards.filter((x) => x.createdAt > new Date().setHours(0, 0, 0, 0)).length);
  const presenceGame = $derived(library.shards.find((x) => x.game)?.game ?? null);

  async function setRecordHotkey(recordHotkey: string) {
    recordError = '';
    try {
      await settings.update({ recordHotkey });
    } catch (code) {
      recordError = shortcutError(code);
    }
  }

  /** Taken by another app, by another Krak action, or not a valid shortcut. */
  function shortcutError(code: unknown): string {
    const key = `error.${code}`;
    return t(typeof code === 'string' && code.startsWith('hotkey_') && hasKey(key) ? key : 'error.hotkey_invalid');
  }

  async function setHotkey(hotkey: string) {
    hotkeyError = '';
    try {
      await settings.update({ hotkey });
    } catch (code) {
      hotkeyError = shortcutError(code);
    }
  }

  function set(patch: Partial<Settings>) {
    settings.update(patch).catch(showError);
  }

  const engineLine = $derived.by(() => {
    if (status.state === 'starting') return t('capture.starting');
    if (status.state === 'paused') return t('capture.paused');
    if (status.state === 'error') {
      const key = `capture.error.${status.error}`;
      return t(hasKey(key) ? key : 'capture.error.capture_failed');
    }
    const parts = [
      status.encoder ? t(`encoder.${status.encoder}`) : '',
      status.codec ? `${codecName(status.codec)}${status.tenBit ? ` ${t('settings.tenBitShort')}` : ''}` : '',
      status.width ? `${status.width}×${status.height}` : '',
      status.fps ? `${status.fps} ${t('settings.fpsUnit')}` : '',
    ];
    return parts.filter(Boolean).join(' · ');
  });

  // "System" then every language Krak speaks, each in its own name.
  const languageOptions = $derived([
    { value: null as string | null, label: t('settings.language.system') },
    ...Object.entries(languages).map(([code, l]) => ({ value: code as string | null, label: l.name })),
  ]);

  const buffers = [15, 30, 60, 120, 300].map((v) => ({ value: v, label: v < 60 ? `${v} s` : `${v / 60} min` }));
  const fpsChoices = $derived(
    [30, 60, 120, 144]
      .filter((f) => !encoder?.refreshRate || f <= encoder.refreshRate + 1 || f === 30 || f === 60)
      .map((v) => ({ value: v, label: `${v} ${t('settings.fpsUnit')}` })),
  );
  const resolutions = $derived(
    (['native', '1440', '1080', '720'] as const).map((v) => ({ value: v, label: v === 'native' ? t('settings.resolution.native') : `${v}p` })),
  );
  const codecs = $derived(
    (['auto', 'h264', 'hevc', 'av1'] as const)
      .filter((c) => c === 'auto' || !encoder || encoder.codecs[c])
      .map((c) => ({
        value: c,
        label: c === 'auto' ? t('settings.codec.auto') : codecName(c),
        detail: c === 'auto' ? undefined : t(`settings.codecTag.${c}`),
        tag: c === 'auto' ? t('settings.recommended') : undefined,
      })),
  );
  const audioRates = [128, 160, 192, 256, 320].map((v) => ({ value: v, label: `${v}` }));
  const limits = $derived([
    { value: null as number | null, label: t('settings.limit.none') },
    ...[25, 50, 100, 250, 500].map((gb) => ({ value: gb as number | null, label: formatSize(gb * GB) })),
  ]);
  const accents = [
    { id: 'glacier', color: '#7fc6db' },
    { id: 'aurora', color: '#a89cff' },
    { id: 'jade', color: '#5fd3a4' },
    { id: 'ember', color: '#ff9152' },
    { id: 'rose', color: '#ff7aa2' },
    { id: 'frost', color: '#e8f1f2' },
  ] as const;

  /** A warning when the output format needs a Windows extension to be played. */
  const codecNote = $derived.by(() => {
    if (!output || output.codec === 'h264' || !encoder) return '';
    return encoder.decoders[output.codec] ? '' : t('settings.codecNoDecoder', { codec: codecName(output.codec) });
  });
  const usage = $derived(
    storage && s.storageLimitGb ? Math.min(1, storage.usedBytes / (s.storageLimitGb * GB)) : null,
  );
</script>

{#snippet row(label: string, hint: string, control: Snippet, tone: '' | 'error' | 'warn' = '')}
  <div class="row">
    <div class="text">
      <span class="label">{label}</span>
      {#if hint}<span class="hint {tone}">{hint}</span>{/if}
    </div>
    <div class="control">{@render control()}</div>
  </div>
{/snippet}

<div class="settings">
  <nav aria-label={t('nav.settings')}>
    <h1>{t('nav.settings')}</h1>
    {#each sections as section (section.id)}
      <button class:on={ui.settingsSection === section.id} onclick={() => (ui.settingsSection = section.id)} aria-current={ui.settingsSection === section.id ? 'page' : undefined}>
        <Icon name={section.icon} size={16} />
        {t(`settings.section.${section.id}`)}
      </button>
    {/each}
  </nav>

  <div class="panel">
    {#key ui.settingsSection}
      <div class="section" in:fly={{ y: 8, duration: 240 * motion, easing: cubicOut }}>
        <h2>{t(`settings.section.${ui.settingsSection}`)}</h2>

        {#if ui.settingsSection === 'general'}
          <div class="card">
            {#snippet language()}
              {#if languageOptions.length <= 3}
                <Segmented label={t('settings.language')} options={languageOptions} value={s.language} onchange={(v) => set({ language: v })} />
              {:else}
                <Select label={t('settings.language')} options={languageOptions} value={s.language} onchange={(v) => set({ language: v })} width={200} />
              {/if}
            {/snippet}
            {@render row(t('settings.language'), '', language)}
            {#snippet startup()}
              <Toggle label={t('settings.startup')} checked={s.launchAtStartup} onchange={(v) => set({ launchAtStartup: v })} />
            {/snippet}
            {@render row(t('settings.startup'), t('settings.startupHint'), startup)}
            {#snippet tray()}
              <Toggle label={t('settings.tray')} checked={s.closeToTray} onchange={(v) => set({ closeToTray: v })} />
            {/snippet}
            {@render row(t('settings.tray'), t('settings.trayHint'), tray)}
          </div>

          <h3>Discord</h3>
          <div class="card">
            {#snippet discord()}
              <Toggle label={t('presence.title')} checked={s.discordPresence} onchange={(v) => set({ discordPresence: v })} />
            {/snippet}
            {@render row(t('presence.title'), t('presence.hint'), discord)}
            {#if s.discordPresence}
              <div class="presence">
                <span class="presence-label">
                  {t('presence.preview')}
                  {#if presence}
                    <span class="presence-state" class:on={presence.connected}>
                      <span></span>{t(presence.connected ? 'presence.connected' : 'presence.disconnected')}
                    </span>
                  {/if}
                </span>
                <div class="activity">
                  <span class="activity-head">{t('presence.playingLabel')}</span>
                  <div class="activity-row">
                    <div class="activity-art">
                      <Logo size={38} />
                      <!-- Like Discord: recording is the small image's tooltip, the game stays the main line. -->
                      <span
                        class="activity-dot"
                        class:rec={!!status.recording}
                        use:tooltip={{ text: t('presence.recording'), side: 'top', enabled: !!status.recording }}
                      ></span>
                    </div>
                    <div class="activity-text">
                      <strong>Krak</strong>
                      <span>{presenceGame ? t('presence.playing', { game: presenceGame }) : t('presence.desktop')}</span>
                      <span class="dim">{t('presence.today', { count: today })}</span>
                    </div>
                  </div>
                  <span class="activity-button">{t('presence.button')}</span>
                </div>
              </div>
            {/if}
          </div>
        {:else if ui.settingsSection === 'capture'}
          {#if status.audioError}
            <p class="notice warn"><Icon name="alert" size={15} />{t('capture.audioError')}</p>
          {/if}
          <div class="card">
            {#snippet engine()}
              {#if status.state !== 'starting'}
                <button class="button" onclick={() => capture.setPaused(status.state === 'ready').catch(showError)}>
                  <Icon name={status.state === 'ready' ? 'pause' : 'play'} size={13} />
                  {t(status.state === 'ready' ? 'capture.pause' : 'capture.resume')}
                </button>
              {/if}
            {/snippet}
            {@render row(t('settings.engine'), engineLine, engine, status.state === 'error' ? 'error' : '')}
            {#snippet hotkey()}
              <HotkeyInput value={s.hotkey} onchange={setHotkey} />
            {/snippet}
            {@render row(t('settings.hotkey'), hotkeyError || t('settings.hotkeyHint'), hotkey, hotkeyError ? 'error' : '')}
            {#snippet buffer()}
              <Segmented label={t('settings.buffer')} options={buffers} value={s.bufferSeconds} onchange={(v) => set({ bufferSeconds: v })} />
            {/snippet}
            {@render row(
              t('settings.buffer'),
              memory > 1.5e9 ? t('settings.memoryWarning') : t('settings.memory', { size: formatSize(memory) }),
              buffer,
              memory > 1.5e9 ? 'warn' : '',
            )}
          </div>

          <h3>{t('settings.recording')}</h3>
          <div class="card">
            {#snippet recordKey()}
              <HotkeyInput value={s.recordHotkey} onchange={setRecordHotkey} />
            {/snippet}
            {@render row(t('record.hotkey'), recordError || t('record.hint'), recordKey, recordError ? 'error' : '')}
          </div>

          <h3>{t('settings.screenshots')}</h3>
          <div class="card">
            {#snippet shotKey()}
              <HotkeyInput value={s.screenshotHotkey} onchange={setScreenshotHotkey} />
            {/snippet}
            {@render row(t('screens.hotkey'), screenshotError || t('screens.hotkeyHint'), shotKey, screenshotError ? 'error' : '')}
          </div>

          <h3>{t('settings.notifications')}</h3>
          <div class="card">
            {#snippet overlay()}
              <Toggle label={t('settings.overlay')} checked={s.overlay} onchange={(v) => set({ overlay: v })} />
            {/snippet}
            {@render row(t('settings.overlay'), t('settings.overlayHint'), overlay)}
            {#snippet sound()}
              <Toggle label={t('settings.sound')} checked={s.captureSound} onchange={(v) => set({ captureSound: v })} />
            {/snippet}
            {@render row(t('settings.sound'), t('settings.soundHint'), sound)}
          </div>
        {:else if ui.settingsSection === 'video'}
          {#if encoder}
            <div class="gpu">
              <span class="gpu-icon"><Icon name="cpu" size={16} /></span>
              <div class="text">
                <span class="label">{encoder.gpu}</span>
                <span class="hint">{t('settings.gpu')}{encoder.nvenc ? ' · NVIDIA NVENC' : ''}</span>
              </div>
              <div class="chips">
                {#each ['h264', 'hevc', 'av1'] as const as c (c)}
                  <span class="chip" class:yes={encoder.codecs[c]}>
                    <Icon name={encoder.codecs[c] ? 'check' : 'x'} size={11} stroke={2.6} />{codecName(c)}
                  </span>
                {/each}
              </div>
            </div>
          {/if}

          <QualityPicker value={s.quality} onchange={(q) => set({ quality: q })} />

          <div class="card spaced">
            {#snippet fps()}
              <Segmented label={t('settings.fps')} options={fpsChoices} value={s.fps} onchange={(v) => set({ fps: v })} />
            {/snippet}
            {@render row(
              t('settings.fps'),
              encoder?.refreshRate ? t('settings.fpsCapped', { fps: encoder.refreshRate }) : t('settings.fpsHint'),
              fps,
            )}

            {#if s.quality === 'custom'}
              {#snippet codec()}
                <Select label={t('settings.codec')} options={codecs} value={s.codec} onchange={(v) => set({ codec: v })} width={170} />
              {/snippet}
              {@render row(t('settings.codec'), t(`settings.codecHint.${s.codec}`), codec)}
              {#snippet resolution()}
                <Segmented label={t('settings.resolution')} options={resolutions} value={s.resolution} onchange={(v) => set({ resolution: v })} />
              {/snippet}
              {@render row(t('settings.resolution'), t('settings.resolutionHint'), resolution)}
              {#snippet bitrate()}
                <label class="range">
                  <input
                    type="range"
                    min="2"
                    max="150"
                    step="1"
                    value={s.bitrate}
                    style:--fill="{((s.bitrate - 2) / 148) * 100}%"
                    onchange={(e) => set({ bitrate: Number(e.currentTarget.value) })}
                    oninput={(e) => (settings.value.bitrate = Number(e.currentTarget.value))}
                    aria-label={t('settings.bitrate')}
                  />
                  <span>{s.bitrate} Mbit/s</span>
                </label>
              {/snippet}
              {@render row(t('settings.bitrate'), t('settings.bitrateHint'), bitrate)}
              {#if output && output.codec !== 'h264'}
                {#snippet tenBit()}
                  <Toggle label={t('settings.tenBit')} checked={s.tenBit} onchange={(v) => set({ tenBit: v })} />
                {/snippet}
                {@render row(t('settings.tenBit'), t('settings.tenBitHint'), tenBit)}
              {/if}
            {/if}

            {#snippet result()}
              <span class="result">{output ? t('settings.perMinute', { size: formatSize((output.bitrate / 8) * 60) }) : ''}</span>
            {/snippet}
            {@render row(
              t('settings.output'),
              codecNote || (output ? videoLabel(output, t('settings.tenBitShort'), t('settings.fpsUnit')) : ''),
              result,
              codecNote ? 'warn' : '',
            )}
          </div>
        {:else if ui.settingsSection === 'audio'}
          <div class="card">
            {#snippet mic()}
              <Toggle label={t('settings.microphone')} checked={s.microphone} onchange={(v) => set({ microphone: v })} />
            {/snippet}
            {@render row(t('settings.microphone'), t('settings.microphoneHint'), mic)}
            {#if s.microphone}
              {#snippet device()}
                <Select
                  label={t('settings.micDevice')}
                  options={[
                    { value: null as string | null, label: t('settings.micDefault') },
                    ...microphones.map((m) => ({ value: m.id as string | null, label: m.name })),
                  ]}
                  value={s.microphoneDevice}
                  onchange={(v) => set({ microphoneDevice: v })}
                  width={260}
                />
              {/snippet}
              {@render row(t('settings.micDevice'), '', device)}
            {/if}
            {#snippet quality()}
              <Segmented label={t('settings.audioQuality')} options={audioRates} value={s.audioBitrate} onchange={(v) => set({ audioBitrate: v })} />
            {/snippet}
            {@render row(t('settings.audioQuality'), `${t('settings.audioQualityHint')} kbit/s`, quality)}
          </div>
        {:else if ui.settingsSection === 'storage'}
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
            {#snippet used()}
              <span class="result">{storage ? formatSize(storage.usedBytes) : '…'}</span>
            {/snippet}
            {@render row(
              t('settings.usage'),
              storage ? t('settings.usageValue', { count: storage.clipCount, free: formatSize(storage.freeBytes) }) : '',
              used,
            )}
            {#if usage !== null}
              <div class="meter"><span style:transform="scaleX({usage})" class:full={usage > 0.9}></span></div>
            {/if}
            {#snippet limit()}
              <Select label={t('settings.limit')} options={limits} value={s.storageLimitGb} onchange={(v) => set({ storageLimitGb: v })} width={140} />
            {/snippet}
            {@render row(t('settings.limit'), t('settings.limitHint'), limit)}
          </div>
        {:else if ui.settingsSection === 'appearance'}
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
            {#snippet compact()}
              <Toggle label={t('settings.compactSidebar')} checked={s.sidebarCollapsed} onchange={(v) => set({ sidebarCollapsed: v })} />
            {/snippet}
            {@render row(t('settings.compactSidebar'), t('settings.compactSidebarHint'), compact)}
          </div>
        {:else}
          <div class="card about">
            <Logo size={44} />
            <div class="text">
              <span class="label">Krak <span class="version">{version}</span></span>
              <span class="hint">{t('settings.license')}</span>
            </div>
            <div class="buttons">
              {#if native}
                {#if updates.installing !== null}
                  <span class="button">{t('update.installing', { percent: Math.round(updates.installing * 100) })}</span>
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

          {#if import.meta.env.DEV && native}
            <h3>{t('settings.developer')}</h3>
            <div class="card">
              {#snippet demo()}
                <Toggle label={t('settings.demo')} checked={library.demo} onchange={(v) => library.setDemo(v)} />
              {/snippet}
              {@render row(t('settings.demo'), t('settings.demoHint'), demo)}
            </div>
          {/if}
        {/if}
      </div>
    {/key}
  </div>
</div>

<style>
  .settings {
    display: flex;
    gap: 28px;
    padding: 8px 32px 56px;
    max-width: 1100px;
  }

  nav {
    position: sticky;
    top: 8px;
    align-self: flex-start;
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 196px;
    flex: none;
  }
  h1 {
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.015em;
    margin: 0 0 14px 4px;
  }
  nav button {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 10px;
    border-radius: var(--radius-sm);
    font-weight: 500;
    color: var(--text-2);
    text-align: left;
    transition:
      background var(--fast),
      color var(--fast);
  }
  nav button :global(svg) {
    color: var(--text-3);
    transition: color var(--fast);
  }
  nav button:hover {
    background: rgba(255, 255, 255, 0.035);
    color: var(--text);
  }
  nav button.on {
    background: rgba(255, 255, 255, 0.06);
    color: var(--text);
  }
  nav button.on :global(svg) {
    color: var(--accent);
  }

  .panel {
    flex: 1;
    min-width: 0;
    max-width: 760px;
  }
  h2 {
    font-size: 18px;
    font-weight: 600;
    margin: 6px 0 16px;
  }
  h3 {
    margin: 26px 0 10px 2px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-2);
  }

  .notice {
    display: flex;
    gap: 10px;
    margin-bottom: 12px;
    padding: 10px 12px;
    border-radius: var(--radius);
    font-size: 12.5px;
    line-height: 1.5;
    color: var(--text-2);
    background: color-mix(in srgb, #f5a524 10%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, #f5a524 24%, transparent);
  }
  .notice :global(svg) {
    flex: none;
    margin-top: 2px;
    color: #f5a524;
  }

  .card {
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .card.spaced {
    margin-top: 14px;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    min-height: 64px;
    padding: 12px 16px;
  }
  .row + .row,
  .meter + .row {
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
  }
  .hint.error {
    color: var(--danger);
  }
  .hint.warn {
    color: #f5a524;
  }
  .control {
    flex: none;
  }
  .result {
    font-size: 13px;
    font-weight: 500;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }

  .gpu {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
    padding: 12px 16px;
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .gpu .text {
    flex: 1;
  }
  .gpu-icon {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 9px;
    color: var(--accent);
    background: var(--accent-soft);
  }
  .chips {
    display: flex;
    gap: 6px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 8px;
    border-radius: 999px;
    font-size: 11.5px;
    font-weight: 600;
    color: var(--text-3);
    background: var(--surface-2);
  }
  .chip.yes {
    color: var(--success);
    background: color-mix(in srgb, var(--success) 12%, transparent);
  }

  .range {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .range span {
    width: 74px;
    text-align: right;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
    color: var(--text-2);
  }
  input[type='range'] {
    appearance: none;
    width: 200px;
    height: 4px;
    border-radius: 4px;
    background: linear-gradient(to right, var(--accent) var(--fill), var(--surface-3) var(--fill));
    cursor: pointer;
  }
  input[type='range']::-webkit-slider-thumb {
    appearance: none;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
  }

  .meter {
    height: 6px;
    margin: 0 16px 14px;
    border-radius: 6px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .meter span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transition: transform var(--slow) var(--ease-out);
  }
  .meter span.full {
    background: #f5a524;
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
    background: var(--surface-4);
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

  .presence {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 4px 16px 16px;
  }
  .presence-label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    font-size: 12px;
    font-weight: 600;
    color: var(--text-3);
  }
  .presence-state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 500;
  }
  .presence-state span {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-3);
  }
  .presence-state.on {
    color: var(--success);
  }
  .presence-state.on span {
    background: var(--success);
  }
  .activity {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px;
    border-radius: 12px;
    background: #111214;
  }
  .activity-head {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: #a7afb8;
  }
  .activity-row {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .activity-art {
    position: relative;
    display: grid;
    place-items: center;
    width: 64px;
    height: 64px;
    flex: none;
    border-radius: 12px;
    background: var(--nuit);
  }
  .activity-dot {
    position: absolute;
    right: -4px;
    bottom: -4px;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: 4px solid #111214;
    background: var(--accent);
  }
  .activity-dot.rec {
    background: var(--rec);
  }
  .activity-text {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
    font-size: 13px;
    color: #c9d1d9;
  }
  .activity-text strong {
    font-size: 15px;
    color: var(--text);
  }
  .activity-text .dim {
    color: #a7afb8;
  }
  .activity-button {
    display: grid;
    place-items: center;
    padding: 9px;
    border-radius: 6px;
    font-size: 14px;
    font-weight: 500;
    color: var(--text);
    background: #2a2d33;
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
