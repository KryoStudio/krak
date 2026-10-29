<!-- Crops a picture: drag the corners or edges, drag inside to move, drag outside to draw a new
     frame. `crop` is in the picture's own pixels; `aspect` (width / height) locks the shape.
     Fills its (positioned) parent, the picture as large as it fits. -->
<script lang="ts">
  import type { Crop } from '$lib/api';

  let {
    src,
    width,
    height,
    aspect = null,
    crop = $bindable(),
  }: { src: string | null; width: number; height: number; aspect?: number | null; crop: Crop } = $props();

  /** Smallest frame kept, in pixels; a smaller drag (a click) puts the previous one back. */
  const MIN = 16;

  type Handle = 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w';
  type Drag =
    | { mode: 'move'; fromX: number; fromY: number; start: Crop }
    | { mode: 'size'; ax: number; ay: number; fx: boolean; fy: boolean; start: Crop };

  let area: HTMLDivElement;
  let box = $state<HTMLDivElement>();
  let fit = $state({ w: 0, h: 0 });
  let drag = $state<Drag | null>(null);

  // The picture as large as the area allows, keeping its shape.
  $effect(() => {
    const ro = new ResizeObserver(() => {
      const scale = Math.min(area.clientWidth / width, area.clientHeight / height);
      fit = { w: Math.max(0, width * scale), h: Math.max(0, height * scale) };
    });
    ro.observe(area);
    return () => ro.disconnect();
  });

  const pct = (v: number, of: number) => `${(v / of) * 100}%`;
  const handles = $derived<Handle[]>(aspect ? ['nw', 'ne', 'se', 'sw'] : ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w']);

  /** The pointer in picture pixels (kept inside the picture unless `free`). */
  function at(e: PointerEvent, free = false) {
    const r = box!.getBoundingClientRect();
    const x = ((e.clientX - r.left) / r.width) * width;
    const y = ((e.clientY - r.top) / r.height) * height;
    return free ? { x, y } : { x: Math.min(width, Math.max(0, x)), y: Math.min(height, Math.max(0, y)) };
  }

  function begin(e: PointerEvent, next: Drag) {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    box!.setPointerCapture(e.pointerId);
    drag = next;
  }

  /** Outside the frame: a new one from here. */
  function draw(e: PointerEvent) {
    const p = at(e);
    begin(e, { mode: 'size', ax: p.x, ay: p.y, fx: true, fy: true, start: { ...crop } });
  }

  function move(e: PointerEvent) {
    const p = at(e, true);
    begin(e, { mode: 'move', fromX: p.x, fromY: p.y, start: { ...crop } });
  }

  /** A handle: the opposite corner or edge stays where it is. */
  function resize(e: PointerEvent, handle: Handle) {
    const { x, y, width: w, height: h } = crop;
    begin(e, {
      mode: 'size',
      ax: handle.includes('w') ? x + w : x,
      ay: handle.includes('n') ? y + h : y,
      fx: handle.includes('w') || handle.includes('e'),
      fy: handle.includes('n') || handle.includes('s'),
      start: { ...crop },
    });
  }

  function onpointermove(e: PointerEvent) {
    if (!drag) return;
    if (drag.mode === 'move') {
      const p = at(e, true);
      const { start } = drag;
      crop = {
        ...start,
        x: Math.min(width - start.width, Math.max(0, start.x + p.x - drag.fromX)),
        y: Math.min(height - start.height, Math.max(0, start.y + p.y - drag.fromY)),
      };
      return;
    }
    const p = at(e);
    const { ax, ay, fx, fy, start } = drag;
    let left = fx ? Math.min(ax, p.x) : start.x;
    let right = fx ? Math.max(ax, p.x) : start.x + start.width;
    let top = fy ? Math.min(ay, p.y) : start.y;
    let bottom = fy ? Math.max(ay, p.y) : start.y + start.height;
    if (aspect && fx && fy) {
      // Grow to reach the pointer, then shrink to stay inside the picture, keeping the shape.
      const toRight = p.x >= ax;
      const toBottom = p.y >= ay;
      let w = Math.max(right - left, (bottom - top) * aspect);
      let h = w / aspect;
      const maxW = toRight ? width - ax : ax;
      const maxH = toBottom ? height - ay : ay;
      if (w > maxW) [w, h] = [maxW, maxW / aspect];
      if (h > maxH) [w, h] = [maxH * aspect, maxH];
      left = toRight ? ax : ax - w;
      top = toBottom ? ay : ay - h;
      right = left + w;
      bottom = top + h;
    }
    crop = { x: left, y: top, width: right - left, height: bottom - top };
  }

  function onpointerup() {
    if (drag && (crop.width < MIN || crop.height < MIN)) crop = drag.start;
    drag = null;
  }
</script>

<div class="area" bind:this={area}>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    bind:this={box}
    class="box"
    class:dragging={!!drag}
    style:width="{fit.w}px"
    style:height="{fit.h}px"
    onpointerdown={draw}
    {onpointermove}
    {onpointerup}
    onpointercancel={onpointerup}
  >
    {#if src}<img {src} alt="" draggable="false" />{/if}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="frame"
      style:left={pct(crop.x, width)}
      style:top={pct(crop.y, height)}
      style:width={pct(crop.width, width)}
      style:height={pct(crop.height, height)}
      onpointerdown={move}
    >
      <span class="third v1"></span><span class="third v2"></span>
      <span class="third h1"></span><span class="third h2"></span>
      {#each handles as handle (handle)}
        <span class="handle {handle}" onpointerdown={(e) => resize(e, handle)}></span>
      {/each}
    </div>
  </div>
</div>

<style>
  .area {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
  }
  .box {
    position: relative;
    overflow: hidden;
    border-radius: 6px;
    cursor: crosshair;
    touch-action: none;
    user-select: none;
    background: #000;
  }
  img {
    display: block;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
  /* Everything outside the frame is dimmed by the frame's own shadow. */
  .frame {
    position: absolute;
    box-shadow:
      0 0 0 1px rgba(255, 255, 255, 0.9),
      0 0 0 100vmax rgba(4, 6, 9, 0.62);
    cursor: move;
  }
  .third {
    position: absolute;
    background: rgba(255, 255, 255, 0.35);
    opacity: 0;
    transition: opacity var(--normal);
    pointer-events: none;
  }
  .dragging .third {
    opacity: 1;
  }
  .v1,
  .v2 {
    top: 0;
    bottom: 0;
    width: 1px;
  }
  .v1 {
    left: 33.33%;
  }
  .v2 {
    left: 66.66%;
  }
  .h1,
  .h2 {
    left: 0;
    right: 0;
    height: 1px;
  }
  .h1 {
    top: 33.33%;
  }
  .h2 {
    top: 66.66%;
  }

  /* Corners are white brackets; edges a short bar, both drawn just inside the frame (still seen
     when it touches the picture's edge). Each has a bigger invisible grip. */
  .handle {
    position: absolute;
    width: 26px;
    height: 26px;
  }
  .handle::before {
    content: '';
    position: absolute;
    border: 0 solid #fff;
    filter: drop-shadow(0 1px 2px rgba(0, 0, 0, 0.6));
  }
  .nw,
  .ne,
  .se,
  .sw {
    width: 30px;
    height: 30px;
  }
  .nw::before,
  .ne::before,
  .se::before,
  .sw::before {
    width: 18px;
    height: 18px;
  }
  .nw {
    left: -9px;
    top: -9px;
    cursor: nwse-resize;
  }
  .nw::before {
    left: 9px;
    top: 9px;
    border-width: 3px 0 0 3px;
  }
  .ne {
    right: -9px;
    top: -9px;
    cursor: nesw-resize;
  }
  .ne::before {
    right: 9px;
    top: 9px;
    border-width: 3px 3px 0 0;
  }
  .se {
    right: -9px;
    bottom: -9px;
    cursor: nwse-resize;
  }
  .se::before {
    right: 9px;
    bottom: 9px;
    border-width: 0 3px 3px 0;
  }
  .sw {
    left: -9px;
    bottom: -9px;
    cursor: nesw-resize;
  }
  .sw::before {
    left: 9px;
    bottom: 9px;
    border-width: 0 0 3px 3px;
  }
  .n,
  .s {
    left: calc(50% - 16px);
    width: 32px;
    height: 20px;
    cursor: ns-resize;
  }
  .n {
    top: -10px;
  }
  .s {
    bottom: -10px;
  }
  .n::before,
  .s::before {
    left: 6px;
    right: 6px;
    top: 10px;
    height: 3px;
    border-radius: 2px;
    background: #fff;
  }
  .e,
  .w {
    top: calc(50% - 16px);
    width: 20px;
    height: 32px;
    cursor: ew-resize;
  }
  .e {
    right: -10px;
  }
  .w {
    left: -10px;
  }
  .e::before,
  .w::before {
    top: 6px;
    bottom: 6px;
    left: 10px;
    width: 3px;
    border-radius: 2px;
    background: #fff;
  }
  .s::before {
    top: 7px;
  }
  .e::before {
    left: 7px;
  }
</style>
