<!-- First launch: welcome, shortcut, quality, sound, done. Everything can be changed later. -->
<script lang="ts">
  import { onMount } from 'svelte';
  import { fade, fly, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import Logotype from './Logotype.svelte';
  import Toggle from './Toggle.svelte';
  import Segmented from './Segmented.svelte';
  import Select from './Select.svelte';
  import HotkeyInput from './HotkeyInput.svelte';
  import QualityPicker from './QualityPicker.svelte';
  import type { Microphone, Settings } from '$lib/api';
  import { listMicrophones } from '$lib/capture.svelte';
  import { formatHotkey, t } from '$lib/i18n.svelte';
  import { settings } from '$lib/settings.svelte';
  import { showError } from '$lib/shard-actions';

  const STEPS = 5;
  let step = $state(0);
  let direction = $state(1);
  let hotkeyError = $state('');
  let microphones = $state<Microphone[]>([]);
  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  const s = $derived(settings.value);

  onMount(() => {
    listMicrophones().then((list) => (microphones = list));
  });

  function go(next: number) {
    direction = next > step ? 1 : -1;
    step = Math.max(0, Math.min(STEPS - 1, next));
  }

  function finish() {
    settings.update({ onboarded: true }).catch(showError);
  }

  function set(patch: Partial<Settings>) {
    settings.update(patch).catch(showError);
  }

  async function setHotkey(hotkey: string) {
    hotkeyError = '';
    try {
      await settings.update({ hotkey });
    } catch (code) {
      hotkeyError = t(code === 'hotkey_taken' ? 'error.hotkey_taken' : 'error.hotkey_invalid');
    }
  }

  function onkeydown(e: KeyboardEvent) {
    // Enter moves on, unless the shortcut recorder is listening.
    if (e.key === 'Enter' && (e.target as HTMLElement).tagName !== 'BUTTON') {
      e.preventDefault();
      if (step === STEPS - 1) finish();
      else go(step + 1);
    }
  }

  const buffers = [15, 30, 60, 120].map((v) => ({ value: v, label: v < 60 ? `${v} s` : `${v / 60} min` }));
</script>

<svelte:window {onkeydown} />

<div class="backdrop" transition:fade={{ duration: 200 * motion }}></div>
<div class="center">
  <div class="dialog" role="dialog" aria-modal="true" aria-label={t('onboarding.welcomeTitle')} in:scale={{ start: 0.96, duration: 320 * motion, easing: cubicOut }}>
    <div class="glow"></div>

    {#if step > 0}
      <div class="progress" transition:fade={{ duration: 150 * motion }}>
        <span>{t('onboarding.step', { n: step, total: STEPS - 1 })}</span>
        <div class="bar"><span style:transform="scaleX({step / (STEPS - 1)})"></span></div>
      </div>
    {/if}

    <div class="body">
      {#key step}
        <div class="step" in:fly={{ x: 28 * direction * motion, duration: 320 * motion, easing: cubicOut }}>
          {#if step === 0}
            <div class="hero">
              <div class="logotype"><Logotype width={300} /></div>
              <span class="studio">BY KRYO STUDIO</span>
              <h1>{t('onboarding.welcomeTitle')}</h1>
              <p>{t('onboarding.welcomeBody')}</p>
            </div>
          {:else if step === 1}
            <h2>{t('onboarding.hotkeyTitle')}</h2>
            <p class="lead">{t('onboarding.hotkeyBody')}</p>
            <div class="hotkey">
              <HotkeyInput value={s.hotkey} onchange={setHotkey} />
              {#if hotkeyError}<span class="error">{hotkeyError}</span>{/if}
            </div>
            <div class="field">
              <span>{t('settings.buffer')}</span>
              <Segmented label={t('settings.buffer')} options={buffers} value={s.bufferSeconds} onchange={(v) => set({ bufferSeconds: v })} />
            </div>
          {:else if step === 2}
            <h2>{t('onboarding.qualityTitle')}</h2>
            <p class="lead">{t('onboarding.qualityBody')}</p>
            <QualityPicker value={s.quality === 'custom' ? 'high' : s.quality} onchange={(q) => set({ quality: q })} custom={false} />
          {:else if step === 3}
            <h2>{t('onboarding.soundTitle')}</h2>
            <p class="lead">{t('onboarding.soundBody')}</p>
            <div class="options">
              <div class="option">
                <span class="option-icon"><Icon name="mic" size={16} /></span>
                <div class="option-text">
                  <strong>{t('settings.microphone')}</strong>
                  <span>{t('settings.microphoneHint')}</span>
                </div>
                <Toggle label={t('settings.microphone')} checked={s.microphone} onchange={(v) => set({ microphone: v })} />
              </div>
              {#if s.microphone && microphones.length}
                <div class="option sub" transition:fly={{ y: -6, duration: 200 * motion }}>
                  <Select
                    label={t('settings.micDevice')}
                    options={[
                      { value: null as string | null, label: t('settings.micDefault') },
                      ...microphones.map((m) => ({ value: m.id as string | null, label: m.name })),
                    ]}
                    value={s.microphoneDevice}
                    onchange={(v) => set({ microphoneDevice: v })}
                    width={320}
                  />
                </div>
              {/if}
              <div class="option">
                <span class="option-icon"><Icon name="bell" size={16} /></span>
                <div class="option-text">
                  <strong>{t('settings.sound')}</strong>
                  <span>{t('settings.soundHint')}</span>
                </div>
                <Toggle label={t('settings.sound')} checked={s.captureSound} onchange={(v) => set({ captureSound: v })} />
              </div>
            </div>
          {:else}
            <div class="hero">
              <div class="logo done"><Icon name="check" size={34} stroke={2.4} /></div>
              <h1>{t('onboarding.doneTitle')}</h1>
              <p>{t('onboarding.doneBody')}</p>
              <p class="keys">
                {#each formatHotkey(s.hotkey) as key, i (i)}{#if i > 0}<span class="plus">+</span>{/if}<kbd>{key}</kbd>{/each}
              </p>
            </div>
          {/if}
        </div>
      {/key}
    </div>

    <div class="actions">
      {#if step < STEPS - 1}
        <button class="ghost" onclick={finish}>{t('onboarding.skip')}</button>
      {/if}
      <span class="spacer"></span>
      {#if step > 0 && step < STEPS - 1}
        <button class="secondary" onclick={() => go(step - 1)}>{t('onboarding.back')}</button>
      {/if}
      {#if step === 0}
        <button class="primary" onclick={() => go(1)}>{t('onboarding.start')}<Icon name="arrowRight" size={15} /></button>
      {:else if step < STEPS - 1}
        <button class="primary" onclick={() => go(step + 1)}>{t('onboarding.next')}<Icon name="arrowRight" size={15} /></button>
      {:else}
        <button class="primary" onclick={finish}>{t('onboarding.finish')}</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 70;
    background: rgba(5, 6, 8, 0.72);
    backdrop-filter: blur(6px);
  }
  .center {
    position: fixed;
    inset: 0;
    z-index: 71;
    display: grid;
    place-items: center;
    padding: 24px;
  }
  .dialog {
    position: relative;
    display: flex;
    flex-direction: column;
    width: min(720px, 100%);
    min-height: 470px;
    padding: 28px 30px 22px;
    border-radius: 20px;
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--border-strong),
      0 40px 100px -30px rgba(0, 0, 0, 0.9);
    overflow: hidden;
    isolation: isolate;
  }
  .glow {
    position: absolute;
    inset: 0;
    z-index: -1;
    background:
      radial-gradient(50% 60% at 50% 0%, color-mix(in srgb, var(--face) 16%, transparent), transparent 70%),
      radial-gradient(40% 50% at 100% 100%, color-mix(in srgb, var(--accent) 8%, transparent), transparent 70%);
  }

  .progress {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 22px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-3);
  }
  .bar {
    flex: 1;
    height: 4px;
    border-radius: 4px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transition: transform var(--slow) var(--ease-out);
  }

  .body {
    flex: 1;
    display: grid;
  }
  .step {
    grid-area: 1 / 1;
  }

  .hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding-top: 30px;
  }
  .logo {
    display: grid;
    place-items: center;
    width: 104px;
    height: 104px;
    border-radius: 30px;
    background: var(--bg);
    border: 1px solid var(--border-strong);
    box-shadow: 0 0 80px -10px color-mix(in srgb, var(--face) 45%, transparent);
    animation: rise 700ms var(--ease-out);
  }
  .logotype {
    margin-top: 6px;
    animation: rise 700ms var(--ease-out);
  }
  .studio {
    margin-top: 14px;
    font-size: 11px;
    font-weight: 500;
    letter-spacing: 0.3em;
    color: var(--text-3);
  }
  .logo.done {
    color: var(--success);
    box-shadow: 0 0 80px -10px color-mix(in srgb, var(--success) 40%, transparent);
  }
  .hero h1 {
    margin-top: 24px;
    font-size: 26px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .hero p {
    max-width: 460px;
    margin-top: 10px;
    line-height: 1.6;
    color: var(--text-2);
  }
  .keys {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .keys kbd {
    height: 32px;
    padding: 0 12px;
    font-size: 15px;
    color: var(--text);
  }
  .plus {
    color: var(--text-3);
  }

  h2 {
    font-size: 20px;
    font-weight: 650;
    letter-spacing: -0.015em;
  }
  .lead {
    margin: 6px 0 22px;
    color: var(--text-2);
  }
  .hotkey {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 22px;
    border-radius: 14px;
    background: var(--bg);
    border: 1px solid var(--border);
  }
  .hotkey :global(.wrap button) {
    min-width: 220px;
    height: 44px;
  }
  .error {
    font-size: 12.5px;
    color: var(--danger);
  }
  .field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 16px;
    color: var(--text-2);
    font-weight: 500;
  }

  .options {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .option {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 14px 16px;
    border-radius: 14px;
    background: var(--bg);
    border: 1px solid var(--border);
  }
  .option.sub {
    padding: 10px 16px 10px 64px;
  }
  .option-icon {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: 9px;
    color: var(--accent);
    background: var(--accent-soft);
  }
  .option-text {
    display: flex;
    flex-direction: column;
    flex: 1;
  }
  .option-text span {
    font-size: 12.5px;
    color: var(--text-3);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 26px;
  }
  .spacer {
    flex: 1;
  }
  .primary,
  .secondary,
  .ghost {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    height: 40px;
    padding: 0 18px;
    border-radius: 10px;
    font-weight: 600;
    transition:
      filter var(--fast),
      background var(--fast),
      color var(--fast),
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
  .ghost {
    padding: 0 6px;
    font-weight: 500;
    color: var(--text-3);
  }
  .ghost:hover {
    color: var(--text);
  }

  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(10px) scale(0.92);
    }
  }
</style>
