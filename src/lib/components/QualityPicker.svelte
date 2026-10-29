<!-- Quality profiles as cards, each showing what the engine would really record with it. -->
<script lang="ts">
  import Icon from './Icon.svelte';
  import type { Quality, ResolvedVideo } from '$lib/api';
  import { capture, resolveVideo, videoLabel } from '$lib/capture.svelte';
  import { formatSize, t, type Key } from '$lib/i18n.svelte';
  import { settings } from '$lib/settings.svelte';

  let {
    value,
    onchange,
    custom = true,
  }: { value: Quality; onchange: (quality: Quality) => void; custom?: boolean } = $props();

  const presets = $derived<Quality[]>(custom ? ['standard', 'high', 'premium', 'custom'] : ['standard', 'high', 'premium']);
  const icons = { standard: 'film', high: 'sparkles', premium: 'zap', custom: 'sliders' } as const;
  /** What each profile is good for, at a glance; High is the one to pick. */
  const tags: Partial<Record<Quality, Key>> = { standard: 'settings.tag.light', high: 'settings.recommended', premium: 'settings.tag.best' };

  let resolved = $state<Partial<Record<Quality, ResolvedVideo | null>>>({});

  // Re-resolve when anything that changes the result does.
  $effect(() => {
    const s = settings.value;
    void [s.fps, s.resolution, s.codec, s.bitrate, s.tenBit, capture.encoder, presets];
    let alive = true;
    Promise.all(presets.map((q) => resolveVideo({ quality: q }))).then((list) => {
      if (!alive) return;
      const next: Partial<Record<Quality, ResolvedVideo | null>> = {};
      presets.forEach((q, i) => (next[q] = list[i]));
      resolved = next;
    });
    return () => {
      alive = false;
    };
  });
</script>

<div class="presets" class:four={custom} role="radiogroup" aria-label={t('settings.preset')}>
  {#each presets as q (q)}
    {@const r = resolved[q]}
    <button class="preset" class:on={value === q} role="radio" aria-checked={value === q} onclick={() => onchange(q)}>
      <span class="top">
        <span class="icon"><Icon name={icons[q]} size={16} /></span>
        {#if tags[q]}<span class="badge" class:neutral={q !== 'high'}>{t(tags[q])}</span>{/if}
        <span class="check"><Icon name="check" size={12} stroke={3} /></span>
      </span>
      <strong>{t(`settings.quality.${q}`)}</strong>
      <span class="hint">{t(`settings.quality.${q}Hint`)}</span>
      <span class="spec">
        {#if r}
          <span>{videoLabel(r, t('settings.tenBitShort'), t('settings.fpsUnit'))}</span>
          <span class="size">{t('settings.perMinute', { size: formatSize((r.bitrate / 8) * 60) })}</span>
        {:else}
          <span class="placeholder"></span>
        {/if}
      </span>
    </button>
  {/each}
</div>

<style>
  .presets {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 10px;
  }
  .presets.four {
    grid-template-columns: repeat(4, 1fr);
  }

  .preset {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    padding: 14px;
    border-radius: var(--radius);
    text-align: left;
    background: var(--bg);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      box-shadow var(--normal),
      background var(--normal),
      transform var(--normal) var(--ease-out);
  }
  .preset:hover {
    background: var(--surface-2);
  }
  .preset:active {
    transform: scale(0.985);
  }
  .preset.on {
    background: color-mix(in srgb, var(--accent) 7%, var(--bg));
    box-shadow:
      inset 0 0 0 1.5px var(--accent),
      0 8px 24px -14px var(--accent);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 8px;
    color: var(--text-2);
    background: var(--surface-3);
    transition:
      color var(--normal),
      background var(--normal);
  }
  .on .icon {
    color: var(--accent);
    background: var(--accent-soft);
  }
  .badge {
    min-width: 0;
    padding: 2px 7px;
    border-radius: 999px;
    font-size: 10.5px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--accent);
    background: var(--accent-soft);
  }
  .badge.neutral {
    color: var(--text-2);
    background: rgba(255, 255, 255, 0.06);
  }
  .check {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    margin-left: auto;
    border-radius: 50%;
    color: var(--accent-fg);
    background: var(--accent);
    opacity: 0;
    transform: scale(0.6);
    transition:
      opacity var(--fast),
      transform var(--normal) var(--ease-out);
  }
  .on .check {
    opacity: 1;
    transform: none;
  }

  strong {
    font-size: 14px;
    font-weight: 600;
  }
  .hint {
    font-size: 12px;
    line-height: 1.4;
    color: var(--text-3);
    min-height: 2.8em;
  }
  .spec {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-top: 6px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
    font-size: 11.5px;
    color: var(--text-2);
    font-variant-numeric: tabular-nums;
  }
  .size {
    color: var(--text-3);
  }
  .placeholder {
    height: 30px;
  }
</style>
