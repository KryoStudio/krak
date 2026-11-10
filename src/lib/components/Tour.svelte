<!-- A guided tour: the first step is a welcome card in the middle ("Follow the guide" or "Skip");
     each next step lights up one element of the page (`data-tour="<target>"`) with a bubble
     beside it. Next / Back / Skip, ←/→, Esc. Everything else is dimmed and inert meanwhile. -->
<script lang="ts" module>
  export interface TourStep {
    /** `data-tour` of the element to light up; none: a card in the middle. */
    target?: string;
    title: string;
    body: string;
  }
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import { fade, fly, scale } from 'svelte/transition';
  import { cubicOut } from 'svelte/easing';
  import { prefersReducedMotion } from 'svelte/motion';
  import Icon from './Icon.svelte';
  import { t } from '$lib/i18n.svelte';
  import { portal } from '$lib/portal';

  let { steps, onclose }: { steps: TourStep[]; onclose: () => void } = $props();

  const motion = $derived(prefersReducedMotion.current ? 0 : 1);
  let index = $state(0);
  let rect = $state<{ x: number; y: number; w: number; h: number } | null>(null);
  let viewport = $state({ w: innerWidth, h: innerHeight });
  let bubble = $state<HTMLDivElement>();
  let bubbleSize = $state({ w: 340, h: 180 });
  const step = $derived(steps[index]);
  const last = $derived(index === steps.length - 1);
  const PAD = 8;

  // Follow the target (layout changes, scrolling, resizing) every frame while open.
  onMount(() => {
    let frame = requestAnimationFrame(function measure() {
      viewport = { w: innerWidth, h: innerHeight };
      const target = step?.target ? document.querySelector<HTMLElement>(`[data-tour="${step.target}"]`) : null;
      if (target) {
        const r = target.getBoundingClientRect();
        const next = { x: r.x - PAD, y: r.y - PAD, w: r.width + 2 * PAD, h: r.height + 2 * PAD };
        if (!rect || Math.abs(rect.x - next.x) + Math.abs(rect.y - next.y) + Math.abs(rect.w - next.w) + Math.abs(rect.h - next.h) > 0.5) rect = next;
      } else rect = null;
      if (bubble) bubbleSize = { w: bubble.offsetWidth, h: bubble.offsetHeight };
      frame = requestAnimationFrame(measure);
    });
    return () => cancelAnimationFrame(frame);
  });

  $effect(() => {
    const target = step?.target ? document.querySelector<HTMLElement>(`[data-tour="${step.target}"]`) : null;
    target?.scrollIntoView({ block: 'nearest', behavior: motion ? 'smooth' : 'auto' });
  });

  /** The bubble beside the lit element: below, above, right or left, whichever fits. */
  const place = $derived.by(() => {
    if (!rect) return null;
    const gap = 14;
    const { w, h } = bubbleSize;
    const clampX = (x: number) => Math.max(12, Math.min(viewport.w - w - 12, x));
    const clampY = (y: number) => Math.max(12, Math.min(viewport.h - h - 12, y));
    if (rect.y + rect.h + gap + h < viewport.h) return { x: clampX(rect.x + rect.w / 2 - w / 2), y: rect.y + rect.h + gap };
    if (rect.y - gap - h > 0) return { x: clampX(rect.x + rect.w / 2 - w / 2), y: rect.y - gap - h };
    if (rect.x + rect.w + gap + w < viewport.w) return { x: rect.x + rect.w + gap, y: clampY(rect.y + rect.h / 2 - h / 2) };
    return { x: Math.max(12, rect.x - gap - w), y: clampY(rect.y + rect.h / 2 - h / 2) };
  });

  function next() {
    if (last) onclose();
    else index++;
  }

  function onkeydown(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') onclose();
    else if (e.key === 'ArrowRight' || e.key === 'Enter') next();
    else if (e.key === 'ArrowLeft' && index > 0) index--;
    else return;
    e.preventDefault();
  }
</script>

<svelte:window {onkeydown} />

<div class="tour" use:portal={'body'} transition:fade={{ duration: 180 * motion }}>
  <svg class="shade" width={viewport.w} height={viewport.h} aria-hidden="true">
    <defs>
      <mask id="tour-hole">
        <rect width="100%" height="100%" fill="white" />
        {#if rect}
          <rect x={rect.x} y={rect.y} width={rect.w} height={rect.h} rx="14" fill="black" />
        {/if}
      </mask>
    </defs>
    <rect width="100%" height="100%" fill="rgba(4, 5, 7, 0.72)" mask="url(#tour-hole)" />
  </svg>
  {#if rect}
    <div class="ring" style:left="{rect.x}px" style:top="{rect.y}px" style:width="{rect.w}px" style:height="{rect.h}px"></div>
  {/if}

  {#key index}
    <div
      bind:this={bubble}
      class="bubble"
      class:center={!rect}
      style:left={place ? `${place.x}px` : undefined}
      style:top={place ? `${place.y}px` : undefined}
      role="dialog"
      aria-modal="true"
      aria-label={step.title}
      in:fly={{ y: 8, duration: 220 * motion, easing: cubicOut }}
    >
      {#if !rect && index === 0}
        <span class="spark" in:scale={{ start: 0.6, duration: 320 * motion }}><Icon name="sparkles" size={22} /></span>
      {/if}
      <span class="count">{index + 1} / {steps.length}</span>
      <h3>{step.title}</h3>
      <p>{step.body}</p>
      <div class="buttons">
        {#if index === 0}
          <button class="link" onclick={onclose}>{t('tour.skip')}</button>
          <button class="primary" onclick={next}>{t(steps.length > 1 ? 'tour.follow' : 'tour.done')}<Icon name="chevronRight" size={14} /></button>
        {:else}
          <button class="link" onclick={onclose}>{t('tour.skip')}</button>
          <button class="secondary" onclick={() => index--}>{t('tour.back')}</button>
          <button class="primary" onclick={next}>{t(last ? 'tour.done' : 'tour.next')}{#if !last}<Icon name="chevronRight" size={14} />{/if}</button>
        {/if}
      </div>
    </div>
  {/key}
</div>

<style>
  .tour {
    position: fixed;
    inset: 0;
    z-index: 200;
  }
  .shade {
    position: absolute;
    inset: 0;
  }
  .ring {
    position: absolute;
    border-radius: 14px;
    box-shadow:
      0 0 0 2px var(--accent),
      0 0 0 6px color-mix(in srgb, var(--accent) 25%, transparent);
    pointer-events: none;
    transition:
      left 260ms var(--ease-out),
      top 260ms var(--ease-out),
      width 260ms var(--ease-out),
      height 260ms var(--ease-out);
  }
  .bubble {
    position: absolute;
    width: 340px;
    padding: 18px 18px 14px;
    border-radius: 14px;
    background: var(--surface);
    box-shadow:
      0 0 0 1px var(--border-strong),
      0 18px 48px rgba(0, 0, 0, 0.55);
    transition:
      left 260ms var(--ease-out),
      top 260ms var(--ease-out);
  }
  .bubble.center {
    left: 50%;
    top: 50%;
    width: 420px;
    padding: 26px 26px 18px;
    text-align: center;
    transform: translate(-50%, -50%);
  }
  .spark {
    display: inline-grid;
    place-items: center;
    width: 52px;
    height: 52px;
    margin-bottom: 12px;
    border-radius: 16px;
    color: var(--accent-fg);
    background: var(--accent);
    box-shadow: 0 10px 30px color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .count {
    display: block;
    margin-bottom: 6px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.04em;
    color: var(--accent);
  }
  h3 {
    margin: 0 0 6px;
    font-size: 16px;
    font-weight: 700;
  }
  .center h3 {
    font-size: 20px;
  }
  p {
    margin: 0;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text-2);
  }
  .buttons {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .center .buttons {
    justify-content: center;
  }
  .buttons button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 32px;
    padding: 0 14px;
    border-radius: 8px;
    font-size: 13px;
    font-weight: 600;
  }
  .link {
    margin-right: auto;
    padding: 0 6px !important;
    color: var(--text-3);
  }
  .center .link {
    margin-right: 0;
  }
  .link:hover {
    color: var(--text);
  }
  .secondary {
    background: var(--surface-3);
  }
  .secondary:hover {
    background: var(--surface-4);
  }
  .primary {
    color: var(--accent-fg);
    background: var(--accent);
  }
  .primary:hover {
    filter: brightness(1.08);
  }
</style>
