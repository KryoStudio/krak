<!-- The editor's captions and blurred zones over the player, where the export puts them: blurs
     on the clip's picture, captions on the exported frame (the 9:16 window when vertical, the
     whole 16:9 box for a stretched 4:3 clip). Each shows while the playhead is in its range (a
     selected one always); a click selects it, a drag moves it, the corner handle resizes a blur.
     The preview blurs where the export pixelates, a close enough look. -->
<script lang="ts">
  import { canFrame, framing, windowShare } from '$lib/framing.svelte';
  import { fx, presence, type Blur, type Caption } from '$lib/fx.svelte';
  import { t } from '$lib/i18n.svelte';

  let { width, height }: { width: number; height: number } = $props();

  /** The picture inside the 16:9 player box (letterboxed or pillarboxed), as shares of it. */
  const picture = $derived.by(() => {
    const aspect = width / height;
    const box = 16 / 9;
    return aspect >= box
      ? { left: 0, top: (1 - box / aspect) / 2, width: 1, height: box / aspect }
      : { left: (1 - aspect / box) / 2, top: 0, width: aspect / box, height: 1 };
  });
  /** The exported frame, as shares of the box. */
  const frame = $derived.by(() => {
    if (framing.vertical && canFrame(width, height)) {
      const share = windowShare(width, height);
      return {
        left: picture.left + framing.x * (1 - share) * picture.width,
        top: picture.top,
        width: share * picture.width,
        height: picture.height,
      };
    }
    const narrow = width / height < (16 / 9) * 0.99;
    return narrow ? { left: 0, top: 0, width: 1, height: 1 } : picture;
  });

  let box: HTMLDivElement;
  let boxHeight = $state(0);
  $effect(() => {
    const ro = new ResizeObserver(() => (boxHeight = box.clientHeight));
    ro.observe(box);
    return () => ro.disconnect();
  });

  const pct = (v: number) => `${v * 100}%`;
  const clamp = (v: number, lo = 0, hi = 1) => Math.min(hi, Math.max(lo, v));
  const isSelected = (kind: 'blur' | 'text', index: number) => fx.selected?.kind === kind && fx.selected.index === index;

  /** Drags in shares of `area` (the picture for blurs, the frame for captions). */
  function dragging(area: { width: number; height: number }, apply: (dx: number, dy: number) => void) {
    return (e: PointerEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      const el = e.currentTarget as HTMLElement;
      el.setPointerCapture(e.pointerId);
      const r = box.getBoundingClientRect();
      const [x0, y0] = [e.clientX, e.clientY];
      let last = [0, 0];
      const move = (ev: PointerEvent) => {
        const dx = (ev.clientX - x0) / (r.width * area.width);
        const dy = (ev.clientY - y0) / (r.height * area.height);
        apply(dx - last[0], dy - last[1]);
        last = [dx, dy];
      };
      el.addEventListener('pointermove', move);
      el.addEventListener('pointerup', () => el.removeEventListener('pointermove', move), { once: true });
    };
  }

  function grabBlur(blur: Blur, index: number) {
    return (e: PointerEvent) => {
      fx.selected = { kind: 'blur', index };
      dragging(picture, (dx, dy) => {
        blur.x = clamp(blur.x + dx, 0, 1 - blur.w);
        blur.y = clamp(blur.y + dy, 0, 1 - blur.h);
      })(e);
    };
  }

  function resizeBlur(blur: Blur) {
    return dragging(picture, (dx, dy) => {
      blur.w = clamp(blur.w + dx, 0.03, 1 - blur.x);
      blur.h = clamp(blur.h + dy, 0.03, 1 - blur.y);
    });
  }

  function grabCaption(caption: Caption, index: number) {
    return (e: PointerEvent) => {
      fx.selected = { kind: 'text', index };
      dragging(frame, (dx, dy) => {
        caption.x = clamp(caption.x + dx, 0.02, 0.98);
        caption.y = clamp(caption.y + dy, 0.02, 0.98);
      })(e);
    };
  }

  const blurShown = (blur: Blur, index: number) =>
    isSelected('blur', index) || blur.start === undefined || (fx.current >= (blur.start ?? 0) && fx.current < (blur.end ?? Infinity));
</script>

<div class="fx-box" bind:this={box}>
  {#each fx.blurs as blur, i (i)}
    {#if blurShown(blur, i)}
      <div
        class="blur"
        class:on={isSelected('blur', i)}
        style:left={pct(picture.left + blur.x * picture.width)}
        style:top={pct(picture.top + blur.y * picture.height)}
        style:width={pct(blur.w * picture.width)}
        style:height={pct(blur.h * picture.height)}
        onpointerdown={grabBlur(blur, i)}
        role="button"
        tabindex="-1"
        aria-label={t('editor.blur')}
      >
        {#if isSelected('blur', i)}
          <span class="corner" onpointerdown={resizeBlur(blur)} role="presentation"></span>
        {/if}
      </div>
    {/if}
  {/each}

  {#each fx.texts as caption, i (i)}
    {@const shown = isSelected('text', i) ? 1 : presence(caption.start, caption.end, fx.current)}
    {#if shown > 0}
      <div
        class="caption {caption.style}"
        class:on={isSelected('text', i)}
        style:left={pct(frame.left + caption.x * frame.width)}
        style:top={pct(frame.top + caption.y * frame.height)}
        style:max-width={pct(frame.width * 0.9)}
        style:font-size="{caption.size * frame.height * boxHeight}px"
        style:opacity={shown}
        onpointerdown={grabCaption(caption, i)}
        role="button"
        tabindex="-1"
        aria-label="{t('editor.text')} · {caption.text}"
      >
        {caption.text || t('editor.textDefault')}
      </div>
    {/if}
  {/each}
</div>

<style>
  .fx-box {
    position: relative;
    width: min(100cqw, 100cqh * 16 / 9);
    aspect-ratio: 16 / 9;
    pointer-events: none;
  }
  .blur {
    position: absolute;
    border-radius: 4px;
    backdrop-filter: blur(9px) saturate(0.85);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.25);
    pointer-events: auto;
    cursor: move;
    touch-action: none;
  }
  .blur.on {
    box-shadow:
      inset 0 0 0 2px #a89cff,
      0 0 0 1px rgba(0, 0, 0, 0.5);
  }
  .corner {
    position: absolute;
    right: -6px;
    bottom: -6px;
    width: 14px;
    height: 14px;
    border-radius: 4px;
    background: #a89cff;
    box-shadow: 0 0 0 2px rgba(0, 0, 0, 0.45);
    cursor: nwse-resize;
  }
  .caption {
    position: absolute;
    transform: translate(-50%, -50%);
    padding: 0.18em 0.28em;
    font-family: 'Segoe UI', system-ui, sans-serif;
    font-weight: 700;
    line-height: 1.2;
    text-align: center;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: #fff;
    pointer-events: auto;
    cursor: move;
    touch-action: none;
    user-select: none;
  }
  .caption.shadow {
    text-shadow:
      0.05em 0.05em 0.04em rgba(0, 0, 0, 0.55),
      0.08em 0.08em 0.1em rgba(0, 0, 0, 0.4);
  }
  .caption.outline {
    -webkit-text-stroke: 0.12em rgba(0, 0, 0, 0.62);
    paint-order: stroke fill;
  }
  .caption.box {
    padding: 0.45em 0.45em;
    border-radius: 0.3em;
    background: rgba(0, 0, 0, 0.62);
  }
  .caption.on {
    outline: 2px dashed #ffc75a;
    outline-offset: 4px;
    border-radius: 6px;
  }
</style>
