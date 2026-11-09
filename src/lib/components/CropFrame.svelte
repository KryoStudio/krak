<!-- The vertical (9:16) window over the player while editing: what a vertical export keeps.
     Drag it (or click beside it, or use ←/→ once focused) to choose what stays; the rest is
     dimmed. It covers the same box as the video, and finds the picture inside it. -->
<script lang="ts">
  import { framing, windowShare } from '$lib/framing.svelte';
  import { t } from '$lib/i18n.svelte';

  let { width, height }: { width: number; height: number } = $props();

  /** The picture inside the 16:9 player box (the video is letterboxed or pillarboxed). */
  const picture = $derived.by(() => {
    const aspect = width / height;
    const box = 16 / 9;
    return aspect >= box
      ? { left: 0, top: (1 - box / aspect) / 2, width: 1, height: box / aspect }
      : { left: (1 - aspect / box) / 2, top: 0, width: aspect / box, height: 1 };
  });
  const share = $derived(windowShare(width, height));
  const left = $derived(framing.x * (1 - share));

  let area: HTMLDivElement;
  let drag = $state<{ pointer: number; from: number; x: number } | null>(null);

  const clamp = (v: number) => Math.min(1, Math.max(0, v));
  /** A position from a pointer at `clientX`, the window centred on it. */
  function centreOn(clientX: number) {
    const rect = area.getBoundingClientRect();
    const at = (clientX - rect.left) / rect.width;
    framing.x = clamp((at - share / 2) / Math.max(1e-6, 1 - share));
  }

  function grab(e: PointerEvent) {
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    (e.currentTarget as HTMLElement).focus();
    drag = { pointer: e.pointerId, from: e.clientX, x: framing.x };
  }

  function move(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.pointer) return;
    const span = area.getBoundingClientRect().width * (1 - share);
    framing.x = clamp(drag.x + (e.clientX - drag.from) / Math.max(1, span));
  }

  function release(e: PointerEvent) {
    if (drag?.pointer === e.pointerId) drag = null;
  }

  function onkeydown(e: KeyboardEvent) {
    const step = e.shiftKey ? 0.1 : 0.02;
    if (e.key === 'ArrowLeft') framing.x = clamp(framing.x - step);
    else if (e.key === 'ArrowRight') framing.x = clamp(framing.x + step);
    else if (e.key === 'Home') framing.x = 0;
    else if (e.key === 'End') framing.x = 1;
    else return;
    // Not the editor's seek keys.
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<div class="frame-box" aria-hidden="false">
  <div
    class="picture"
    bind:this={area}
    style:left="{picture.left * 100}%"
    style:top="{picture.top * 100}%"
    style:width="{picture.width * 100}%"
    style:height="{picture.height * 100}%"
  >
    <!-- Beside the window: a click brings it there. -->
    <div class="shade" style:left="0" style:width="{left * 100}%" onpointerdown={(e) => centreOn(e.clientX)} role="presentation"></div>
    <div class="shade" style:left="{(left + share) * 100}%" style:right="0" onpointerdown={(e) => centreOn(e.clientX)} role="presentation"></div>
    <div
      class="window"
      class:dragging={!!drag}
      style:left="{left * 100}%"
      style:width="{share * 100}%"
      role="slider"
      tabindex="0"
      aria-label={t('editor.cropFrame')}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(framing.x * 100)}
      onpointerdown={grab}
      onpointermove={move}
      onpointerup={release}
      onpointercancel={release}
      {onkeydown}
    >
      <span class="tag">9:16</span>
      <span class="grip"></span>
    </div>
  </div>
</div>

<style>
  .frame-box {
    position: relative;
    width: min(100cqw, 100cqh * 16 / 9);
    aspect-ratio: 16 / 9;
    pointer-events: none;
  }
  .picture {
    position: absolute;
  }
  .shade {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgba(5, 6, 8, 0.62);
    pointer-events: auto;
    cursor: pointer;
  }
  .window {
    position: absolute;
    top: 0;
    bottom: 0;
    border-radius: 6px;
    box-shadow:
      inset 0 0 0 2px var(--accent),
      0 0 0 1px rgba(0, 0, 0, 0.5);
    pointer-events: auto;
    cursor: grab;
    touch-action: none;
    outline: none;
    transition: box-shadow var(--fast);
  }
  .window:focus-visible {
    box-shadow:
      inset 0 0 0 3px var(--accent),
      0 0 0 3px color-mix(in srgb, var(--accent) 35%, transparent);
  }
  .window.dragging {
    cursor: grabbing;
  }
  .tag {
    position: absolute;
    top: 8px;
    left: 50%;
    transform: translateX(-50%);
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 700;
    color: var(--accent-fg);
    background: var(--accent);
  }
  /* A handle in the middle, to say it moves sideways. */
  .grip {
    position: absolute;
    top: 50%;
    left: 50%;
    width: 6px;
    height: 34px;
    transform: translate(-50%, -50%);
    border-radius: 3px;
    background: var(--accent);
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.4);
  }
</style>
